//! Low-level key/value map + set table (C9a: `src/objects/containers/map.zig`, 630 lines).
//!
//! Port of `py/map.c` as re-implemented in Zig: `mp_map_*` (the dict /
//! locals-dict hash table) and `mp_set_*` (the set hash table). Full
//! config only — the nano open-addressing map path (`mapRehash`, the
//! `hash % alloc` probe in `mp_map_lookup`) is NOT ported; full keeps
//! insertion order with the compact layout, as CPython's dicts do.
//!
//! # Layout (map.zig, compact)
//!
//! One allocation holds `alloc` entries in insertion order, then
//! `fill` (entries appended since the last rehash — live +
//! tombstones), then an open-addressing index (0 = empty, i+1 =
//! entry i) of u8/u16/u32 slots, the smallest that holds `alloc`.
//! Iterating the entries and skipping NULL/SENTINEL keys yields
//! insertion order. A removed entry keeps its key slot as SENTINEL
//! and its index slot keeps pointing at it, so probes step over it
//! like a tombstone.
//!
//! The Rust model (`MapData`, defined in `value.rs`) keeps the same
//! fields: `entries: Vec<MapEntry>` (`len()` is the Zig `fill` for
//! compact maps, `used` for ordered ones — entries beyond `fill`
//! don't exist as a `Vec`), `index: Vec<u32>` (always u32 — the
//! Zig's u8/u16/u32 width choice is memory-only, so Rust uses one
//! width; `index_width` is kept as a documented helper for the
//! `compact_bytes` sizing). `alloc` is the entry capacity.
//!
//! # Borrow design (the arena problem)
//!
//! A dict lives in the `GcArena`, but probing needs the arena twice
//! at once: `&mut` the map (to read/write entries + index) AND
//! `&mut CallCtx` (to hash keys and compare them — `call_unary` /
//! `obj_equal` take `&mut CallCtx`). Holding both borrows at once
//! does not compile, and taking the map out of the arena (swap-out)
//! would hide its entries from the GC while a `__hash__` / `__eq__`
//! slot allocates. So the lookups here take `(ctx, types, map_ref)`
//! and touch the map only through short scoped borrows
//! (`map_of` / `map_of_mut`): each read copies a `Value` out, each
//! write is its own scope, and no map borrow is live across a
//! `hash_of` / `obj_equal` call. The map never leaves the arena, so
//! the GC keeps tracing it through the dict slot (which the caller
//! roots, per D6).
//!
//! # Re-entrant mutation during `__eq__`
//!
//! The Zig holds raw table pointers across `mp_obj_equal` — a
//! `__eq__` that mutates (or rehashes) the same dict dangles them.
//! The Rust probe re-validates after a positive `obj_equal`: if the
//! entry's key changed under it, the probe restarts from the current
//! table state. No user `__eq__` exists yet (C12), so this is
//! defensive today.
//!
//! # Lookup cache
//!
//! `map.zig` keeps a 128-byte global `lookup_cache` ("A plain global
//! rather than MP_STATE_VM, like pyerr.pending_exc: petitpy has no
//! threads"), indexed by `(obj_word >> 2) % 128`, validated by key
//! comparison — a stale entry is only a miss. The Rust port is a
//! `thread_local!` cell of the same shape (the cache is a hint, not
//! state: the word is the REPR_A word from `obj_id`, where the Zig
//! uses the object pointer — both are hints only, and the value is
//! validated the same way).
//!
//! # What is NOT here
//!
//! * The nano map path (`mapRehash`, the `% alloc` probe) — full only.
//! * `MpMap` as a dict payload is `Obj::Dict` / `Obj::Map` (value.rs);
//!   the dict TYPE and methods are `objdict.rs`.
//! * The set TYPE (`Obj::Set` is still a `Vec<Value>` stub) is C9c's;
//!   the `MpSet` table operations live in `map_set.rs` (C9a-split),
//!   re-exported from this file so the public paths are unchanged.

use std::cell::RefCell;

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{call_unary, is_str, obj_get_type, obj_id, BuiltinTypes, ObjError, SlotOutcome};
use crate::objects::obj_ops::obj_equal;
use crate::objects::slots::UnaryOp;
use crate::objects::value::{GcRef, MapData, MapEntry, Obj, Value};
use crate::pyerr::PyError;
use crate::qstr_table::qstr_compute_hash;

/// Full config: the compact insertion-ordered layout
/// (`map.zig: pub const compact = build_options.is_full`).
const COMPACT: bool = true;

/// `mp_map_lookup_kind_t` (`py/obj.h:491-496`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum LookupKind {
    /// `MP_MAP_LOOKUP` (0): find only.
    Lookup = 0,
    /// `MP_MAP_LOOKUP_ADD_IF_NOT_FOUND` (1): find or append.
    Add = 1,
    /// `MP_MAP_LOOKUP_REMOVE_IF_FOUND` (2): find and tombstone/remove.
    Remove = 2,
    /// `MP_MAP_LOOKUP_ADD_IF_NOT_FOUND_OR_REMOVE_IF_FOUND` (3): the
    /// set table's combined kind — bit-tested by `mp_set_lookup`.
    /// `mp_map_lookup` never receives it in the original.
    AddOrRemove = 3,
}

/// What a map lookup found. The Zig returns `?*Elem` (a slot pointer
/// the caller writes `.value` into); Rust cannot hand out that
/// pointer across the arena borrow, so the outcome names the entry
/// index instead — and, unlike the Zig, reports "fresh slot"
/// explicitly as `Added`, because the Zig's marker for it (a live
/// entry whose value is NULL) has no Rust counterpart (`Value` is
/// non-nullable; a `None` value is a real value, not NULL).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LookupOutcome {
    /// Entry `i` already holds the key (LOOKUP/ADD-found). Its value
    /// is readable/writable at `entries[i]` (see `map_value` /
    /// `map_set_value`).
    Found(usize),
    /// A fresh slot was created at index `i` (ADD only). Its value is
    /// the `Value::None` placeholder — the caller MUST store the real
    /// value before any other observation (every caller in
    /// `objdict.rs` does so in the same step).
    Added(usize),
    /// The key was found and removed (REMOVE only): the removed
    /// (key, value). Compact: the entry is tombstoned (key `None`,
    /// value dropped); ordered: the entry left the table
    /// (`Vec::remove`, i.e. the Zig's `orderedRemoveAt` memmove).
    Removed((Value, Value)),
    /// No entry holds the key (LOOKUP/REMOVE miss).
    NotFound,
}

/// Errors the map layer can raise. The dict layer (`objdict.rs`
/// `DictError`) wraps these (plus `KeyError` etc.).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
    /// `TypeError` with a TERSE literal: unhashable key
    /// ("unsupported type for operator"), a fixed-map mutation, …
    TypeError(&'static str),
    /// An allocation failed (`PyError::OutOfMemory` through).
    OutOfMemory,
    /// A `__hash__` / `__eq__` slot raised. The raise data is dropped
    /// at the slot boundary (the accepted C6/C7r convention — the
    /// exception object travels via `VmState.pending_exc` once C13
    /// adds it); the error itself is preserved, not swallowed.
    PyRaise,
}

impl From<PyError> for MapError {
    fn from(e: PyError) -> MapError {
        match e {
            PyError::OutOfMemory => MapError::OutOfMemory,
            PyError::PyRaise => MapError::PyRaise,
        }
    }
}

// ── sizing tables (map.zig) ─────────────────────────────────────────

/// `hash_allocation_sizes[]` (map.c): growth sizes for the set table
/// (and the nano map path). Used by `set_rehash`.
pub(crate) const HASH_ALLOCATION_SIZES: [u16; 34] = [
    0, 2, 4, 6, 8, 10, 12, // +2
    17, 23, 29, 37, 47, 59, 73, // *1.25
    97, 127, 167, 223, 293, 389, 521, 691, 919, 1223, 1627, 2161, // *1.33
    3229, 4831, 7243, 10861, 16273, 24407, 36607, 54907, // *1.5
];

/// `getHashAllocGeq`: the smallest table size `>= x`.
pub fn get_hash_alloc_geq(x: usize) -> usize {
    for s in HASH_ALLOCATION_SIZES {
        if (s as usize) >= x {
            return s as usize;
        }
    }
    // Ran out of primes — return something sensible (odd).
    (x + x / 2) | 1
}

/// `indexCap(alloc)`: the open-addressing index size — the next power
/// of two `>= alloc + alloc/2 + 1`.
pub fn index_cap(alloc: usize) -> usize {
    (alloc + alloc / 2 + 1).next_power_of_two()
}

/// `indexWidth(alloc)`: the Zig's index slot width (1/2/4 bytes).
/// Memory-only in Rust (the index is always `Vec<u32>`); kept for
/// the `compact_bytes` sizing and as documentation.
pub fn index_width(alloc: usize) -> usize {
    if alloc < 0xff {
        1
    } else if alloc < 0xffff {
        2
    } else {
        4
    }
}

/// `compactBytes(alloc)`: the Zig's single-allocation size for a
/// compact table (`alloc` entries + `fill` + index).
pub fn compact_bytes(alloc: usize) -> usize {
    // Elem is two words (key + value); `fill` is one word.
    alloc * 16 + 8 + index_cap(alloc) * index_width(alloc)
}

// ── `MapData` constructors / helpers ────────────────────────────────

impl MapData {
    /// `mp_map_init(map, 0)`: an empty compact map.
    pub fn new() -> MapData {
        MapData {
            alloc: 0,
            used: 0,
            all_keys_are_qstrs: true,
            is_fixed: false,
            is_ordered: false,
            entries: Vec::new(),
            index: Vec::new(),
        }
    }

    /// `mp_map_init(map, n)`: an empty compact map with `n` entry
    /// slots pre-allocated (the `mp_obj_new_dict(n)` hint).
    pub fn new_hint(n: usize) -> MapData {
        MapData {
            alloc: n,
            used: 0,
            all_keys_are_qstrs: true,
            is_fixed: false,
            is_ordered: false,
            entries: Vec::with_capacity(n),
            index: vec![0u32; index_cap(n)],
        }
    }

    /// Build an ordered map from (key, value) pairs in order
    /// (insertion order = pair order). Convenience constructor for
    /// the literal dicts other lanes build (defkwargs, kwargs
    /// tables): the ordered layout needs no hashing, so it takes no
    /// context. The layout is internal only — `dict` print/equality
    /// key off the dict's TYPE, not this flag (as in the Zig, where
    /// only the type decides `OrderedDict(...)` / order-sensitive
    /// `==`), so an ordered plain dict behaves exactly like a
    /// compact one.
    pub fn from_pairs(pairs: Vec<(Value, Value)>) -> MapData {
        let all_qstrs = pairs.iter().all(|(k, _)| k.is_qstr());
        let n = pairs.len();
        MapData {
            alloc: n,
            used: n,
            all_keys_are_qstrs: all_qstrs,
            is_fixed: false,
            is_ordered: true,
            entries: pairs
                .into_iter()
                .map(|(k, v)| MapEntry { key: Some(k), value: v })
                .collect(),
            index: Vec::new(),
        }
    }

    /// `mp_map_init_fixed_table(map, n, table)`: a fixed (ROM)
    /// ordered table over the given pairs. The Zig points the map at
    /// the caller's table; Rust copies the pairs (owned `Vec`) —
    /// same contents, no aliasing. `all_keys_are_qstrs` is set
    /// unconditionally, as in the Zig; the caller (kwargs tables)
    /// guarantees qstr keys.
    pub fn new_fixed(pairs: Vec<(Value, Value)>) -> MapData {
        debug_assert!(
            pairs.iter().all(|(k, _)| k.is_qstr()),
            "fixed tables hold qstr keys (kwargs)"
        );
        let n = pairs.len();
        MapData {
            alloc: n,
            used: n,
            all_keys_are_qstrs: true,
            is_fixed: true,
            is_ordered: true,
            entries: pairs
                .into_iter()
                .map(|(k, v)| MapEntry { key: Some(k), value: v })
                .collect(),
            index: Vec::new(),
        }
    }

    /// `mp_map_clear`: drop the table and reset to an empty compact
    /// map (alloc 0, flags reset). The Zig frees the table unless
    /// fixed; the Rust `Vec`s are owned either way, so clearing is
    /// safe for fixed maps too (their `ensure_not_fixed` gate lives
    /// in the dict methods, not here).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.entries.shrink_to_fit();
        self.index.clear();
        self.index.shrink_to_fit();
        self.alloc = 0;
        self.used = 0;
        self.all_keys_are_qstrs = true;
        self.is_fixed = false;
        self.is_ordered = false;
    }

    /// Live entry count (`mp_obj_dict_len`).
    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// The next live entry at or after `*cur` (the `dictIterNext`
    /// scan in `objdict.zig`: entries in order, skipping
    /// NULL/SENTINEL keys). Updates `*cur` to one past the returned
    /// index; `None` on exhaustion. `cur` is a plain cursor — the
    /// caller owns it (the Zig passes `*usize` the same way).
    pub fn next_live(&self, cur: &mut usize) -> Option<usize> {
        while *cur < self.entries.len() {
            let i = *cur;
            *cur += 1;
            if self.entries[i].key.is_some() {
                return Some(i);
            }
        }
        None
    }
}

// ── arena access (short scoped borrows — see module docs) ───────────

/// Read the `MapData` of an arena dict/map object.
pub(crate) fn map_of<'a>(arena: &'a GcArena, r: GcRef) -> &'a MapData {
    match arena.get(r) {
        Obj::Dict(d) => &d.map,
        Obj::Map(m) => m,
        _ => panic!("map_of: not a dict/map object"),
    }
}

/// Mutably borrow the `MapData` of an arena dict/map object.
pub(crate) fn map_of_mut<'a>(arena: &'a mut GcArena, r: GcRef) -> &'a mut MapData {
    match arena.get_mut(r) {
        Obj::Dict(d) => &mut d.map,
        Obj::Map(m) => m,
        _ => panic!("map_of_mut: not a dict/map object"),
    }
}

/// The value of entry `i` of an arena map.
pub fn map_value(arena: &GcArena, map_ref: GcRef, i: usize) -> Value {
    map_of(arena, map_ref).entries[i].value
}

/// Store the value of entry `i` of an arena map.
pub fn map_set_value(arena: &mut GcArena, map_ref: GcRef, i: usize, value: Value) {
    map_of_mut(arena, map_ref).entries[i].value = value;
}

// ── hashing ─────────────────────────────────────────────────────────

/// The REPR_A word of a value, as a `usize` (the `obj_id` small int).
/// Used for the lookup cache (the Zig indexes it by the object
/// pointer) and the identity hash.
fn obj_id_word(value: Value) -> usize {
    match obj_id(value) {
        Value::SmallInt(w) => w as usize,
        _ => unreachable!("obj_id always returns a small int"),
    }
}

/// Extract a `usize` hash from a HASH slot's return value. The slot
/// contract (`slots.rs`) says `__hash__` returns a small int; the
/// int slot keeps it (`new_int_from_i64(hash_mpz())` may heap-allocate
/// when the folded hash leaves the 62-bit window, so a heap `Int` is
/// unwrapped too — `as_int_checked` rejects exactly `-2^63`, which
/// then hashes as 0; that corner needs a folded hash of precisely
/// `i64::MIN`). Anything else (a broken slot) hashes as 0 — the
/// Zig's `>> 1` on a non-small-int word is equally meaningless.
fn hash_value_to_usize(arena: &GcArena, v: Value) -> usize {
    match v {
        Value::SmallInt(n) => n as usize,
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => z.as_int_checked().map(|n| n as usize).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// `hashOf` (map.zig:348-353) + the `mp_unary_op` HASH dispatch
/// (`runtime.zig:355-362, 381, 390-391`): the hash of a key, as a
/// `usize`.
///
/// * qstr → `qstr_hash` directly (no slot call);
/// * small int → the int itself (`mp_unary_op` HASH returns `arg`);
/// * heap str/bytes → the `isStrOrBytes` fast path
///   (`strUnaryHash`: the cached `.hash`, else `qstr_compute_hash`
///   over the bytes — the Rust `StrData` has no cached `.hash` field,
///   so it always computes; same value, C8a may add the cache);
/// * anything else → the type's HASH slot (bool delegates to int →
///   1/0; heap int folds via `hash_mpz`; float/tuple go through
///   their slots once C18/C9b register them).
///
/// When the slot is not implemented, the Zig distinguishes two cases
/// (`runtime.zig`): a type WITH a `unary_op` slot that returned NULL
/// for HASH (list/dict/set) raises `TypeError` "unsupported type for
/// operator" (TERSE — verified: `hash([])` on the reference binary);
/// a type with NO `unary_op` slot at all (`NoneType`, instances)
/// falls back to the identity hash (the object word —
/// `hash(None) == 6 == OBJ_NONE`, verified).
pub fn hash_of(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    key: Value,
) -> Result<usize, MapError> {
    match key {
        // The pool's id type is `Qstr = u16` (runtime track); the
        // `Value::Qstr` carrier is u32. Ids are produced by the pool
        // itself (`from_strn` returns `Qstr`), so they fit — the
        // `as u16` only narrows the wider carrier back down.
        Value::Qstr(id) => Ok(ctx.pool.qstr_hash(id as u16) as usize),
        Value::SmallInt(n) => Ok(n as usize),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => Ok(qstr_compute_hash(&s.bytes) as usize),
            Obj::Bytes(b) => Ok(qstr_compute_hash(&b.bytes) as usize),
            _ => hash_via_slot(ctx, types, key),
        },
        _ => hash_via_slot(ctx, types, key),
    }
}

fn hash_via_slot(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    key: Value,
) -> Result<usize, MapError> {
    let t = obj_get_type(ctx.arena, types, key);
    // Does the type HAVE a unary_op slot at all? (`call_unary`
    // collapses "no slot" and "slot returned NULL" into one
    // `NotImplemented`, but the Zig treats them differently.)
    let has_slot = match t {
        Some(tr) => matches!(ctx.arena.get(tr), Obj::Type(d) if d.unary_op.is_some()),
        None => false,
    };
    match call_unary(ctx, t, UnaryOp::Hash, key) {
        Ok(SlotOutcome::Value(v)) => Ok(hash_value_to_usize(ctx.arena, v)),
        Ok(SlotOutcome::NotImplemented) => {
            if has_slot {
                Err(MapError::TypeError("unsupported type for operator"))
            } else {
                Ok(obj_id_word(key))
            }
        }
        Err(ObjError::OutOfMemory) => Err(MapError::OutOfMemory),
        // Unreachable: `call_unary` only errs on `OutOfMemory`.
        Err(_) => Err(MapError::PyRaise),
    }
}

// ── lookup cache ────────────────────────────────────────────────────

thread_local! {
    /// `lookup_cache` (map.zig): the last slot position found for a
    /// key, indexed by the key's word. A plain global in the Zig;
    /// a `thread_local!` cell here (petitpy has no threads).
    static LOOKUP_CACHE: RefCell<[u8; 128]> = const { RefCell::new([0; 128]) };
}

fn cache_get(word: usize) -> u8 {
    LOOKUP_CACHE.with(|c| c.borrow()[(word >> 2) % 128])
}

fn cache_set(word: usize, v: u8) {
    LOOKUP_CACHE.with(|c| c.borrow_mut()[(word >> 2) % 128] = v);
}

// ── `mp_map_lookup` ─────────────────────────────────────────────────

/// `mp_map_lookup` (map.zig:355-...): find (or add / remove) `key`
/// in the arena map `map_ref` (`Obj::Dict` or `Obj::Map`).
/// `compare_only_ptrs` is computed here from `all_keys_are_qstrs`
/// (map.zig:361-373); the compact/ordered branches take it as a
/// parameter. The cache fast path runs first (not for REMOVE).
pub fn map_lookup(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    map_ref: GcRef,
    key: Value,
    kind: LookupKind,
) -> Result<LookupOutcome, MapError> {
    if kind != LookupKind::Remove {
        let alloc = map_of(ctx.arena, map_ref).alloc;
        if alloc != 0 {
            let word = obj_id_word(key);
            let cached = cache_get(word) as usize % alloc;
            let hit = {
                let m = map_of(ctx.arena, map_ref);
                cached < m.entries.len() && m.entries[cached].key == Some(key)
            };
            if hit {
                return Ok(LookupOutcome::Found(cached));
            }
        }
    }
    // Work out if we can compare just pointers.
    let mut compare_only_ptrs = map_of(ctx.arena, map_ref).all_keys_are_qstrs;
    if compare_only_ptrs {
        if key.is_qstr() {
            // Index is a qstr — pointer comparison is OK.
        } else if is_str(ctx.arena, types, key) {
            // Non-interned string — must do full equality.
            compare_only_ptrs = false;
        } else if kind != LookupKind::Add {
            // Not adding and not a qstr/str → will never find it.
            return Ok(LookupOutcome::NotFound);
        }
    }

    if map_of(ctx.arena, map_ref).is_ordered {
        ordered_lookup(ctx, types, map_ref, key, kind, compare_only_ptrs)
    } else {
        // Full config is always compact (`COMPACT`); the nano
        // hash-table path is out of scope for petitpy-rs.
        debug_assert!(COMPACT);
        compact_lookup(ctx, types, map_ref, key, kind, compare_only_ptrs)
    }
}

/// The ordered-array path (fixed maps + OrderedDict, map.zig).
fn ordered_lookup(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    map_ref: GcRef,
    key: Value,
    kind: LookupKind,
    compare_only_ptrs: bool,
) -> Result<LookupOutcome, MapError> {
    let used = map_of(ctx.arena, map_ref).used;
    let mut i = 0;
    while i < used {
        let k = map_of(ctx.arena, map_ref).entries[i].key;
        if let Some(k) = k {
            let found = k == key
                || (!compare_only_ptrs && obj_equal(ctx, types, k, key).map_err(MapError::from)?);
            if found {
                if kind == LookupKind::Remove {
                    // Move the rest of the array down, place the found
                    // element after the end so the caller can read it
                    // (the Zig returns the tail slot; Rust returns the
                    // pair — the entry left the table).
                    let (rk, rv) = {
                        let m = map_of_mut(ctx.arena, map_ref);
                        m.used -= 1;
                        let e = m.entries.remove(i);
                        (e.key.expect("live entry has a key"), e.value)
                    };
                    cache_set(obj_id_word(key), (used - 1) as u8);
                    return Ok(LookupOutcome::Removed((rk, rv)));
                }
                cache_set(obj_id_word(key), i as u8);
                return Ok(LookupOutcome::Found(i));
            }
        }
        i += 1;
    }
    // Not found.
    if kind != LookupKind::Add {
        return Ok(LookupOutcome::NotFound);
    }
    // Add to tail. Grow if needed.
    let (used_now, alloc) = {
        let m = map_of(ctx.arena, map_ref);
        (m.used, m.alloc)
    };
    if used_now == alloc {
        // `orderedGrow(map, 4)`: the Zig reallocs (+ zeroes the tail);
        // the Rust `Vec` grows by capacity instead (no tail slots).
        // Fixed maps never grow here — their callers gate with
        // `ensure_not_fixed` first (the Zig would realloc a ROM
        // pointer).
        debug_assert!(
            !map_of(ctx.arena, map_ref).is_fixed,
            "ordered ADD into a fixed map (callers must gate it)"
        );
        let m = map_of_mut(ctx.arena, map_ref);
        m.entries.reserve(4);
        m.alloc += 4;
    }
    {
        let m = map_of_mut(ctx.arena, map_ref);
        m.used += 1;
        m.entries.push(MapEntry {
            key: Some(key),
            value: Value::None,
        });
        if !key.is_qstr() {
            m.all_keys_are_qstrs = false;
        }
    }
    Ok(LookupOutcome::Added(used_now))
}

/// The compact path (full config, map.zig `compactLookup`).
fn compact_lookup(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    map_ref: GcRef,
    key: Value,
    kind: LookupKind,
    compare_only_ptrs: bool,
) -> Result<LookupOutcome, MapError> {
    if map_of(ctx.arena, map_ref).alloc == 0 {
        if kind != LookupKind::Add {
            return Ok(LookupOutcome::NotFound);
        }
        compact_rehash(ctx, types, map_ref)?;
    }
    // The hash is computed once, before any mutation (it can raise).
    let hash = hash_of(ctx, types, key)?;
    'outer: loop {
        let cap = {
            let m = map_of(ctx.arena, map_ref);
            index_cap(m.alloc)
        };
        let mask = cap - 1;
        let mut pos = hash & mask;
        loop {
            let slot = map_of(ctx.arena, map_ref).index[pos];
            if slot == 0 {
                break; // empty index slot — not found; `pos` is the insertion point
            }
            let i = (slot - 1) as usize;
            let k = map_of(ctx.arena, map_ref).entries[i].key;
            match k {
                // SENTINEL tombstone — step over it.
                None => {
                    pos = (pos + 1) & mask;
                    continue;
                }
                Some(k) => {
                    let mut found = k == key;
                    if !found && !compare_only_ptrs {
                        found = obj_equal(ctx, types, k, key).map_err(MapError::from)?;
                        if found {
                            // Re-validate: a `__eq__` may have mutated
                            // (or rehashed) this map under us — the
                            // Zig would use a dangling pointer here.
                            let k_now = map_of(ctx.arena, map_ref).entries.get(i).and_then(|e| e.key);
                            if k_now != Some(k) {
                                continue 'outer;
                            }
                        }
                    }
                    if found {
                        if kind == LookupKind::Remove {
                            let (rk, rv) = {
                                let m = map_of_mut(ctx.arena, map_ref);
                                m.used -= 1;
                                let e = &mut m.entries[i];
                                let rk = e.key.take().expect("live entry has a key");
                                let rv = std::mem::replace(&mut e.value, Value::None);
                                (rk, rv)
                            };
                            return Ok(LookupOutcome::Removed((rk, rv)));
                        }
                        cache_set(obj_id_word(key), i as u8);
                        return Ok(LookupOutcome::Found(i));
                    }
                    pos = (pos + 1) & mask;
                }
            }
        }
        // Not found; `pos` is the empty index slot that ends the probe.
        if kind != LookupKind::Add {
            return Ok(LookupOutcome::NotFound);
        }
        let (fill, alloc_now) = {
            let m = map_of(ctx.arena, map_ref);
            (m.entries.len(), m.alloc)
        };
        if fill == alloc_now {
            compact_rehash(ctx, types, map_ref)?;
            continue 'outer;
        }
        let i = fill;
        {
            let m = map_of_mut(ctx.arena, map_ref);
            m.index[pos] = (i + 1) as u32;
            m.used += 1;
            m.entries.push(MapEntry {
                key: Some(key),
                value: Value::None,
            });
            if !key.is_qstr() {
                m.all_keys_are_qstrs = false;
            }
        }
        return Ok(LookupOutcome::Added(i));
    }
}

/// `compactRehash` (map.zig): grow (or just compact away removed
/// entries) to 1.5x the live entries, so a dict that deletes and
/// re-adds does not rehash each time. New alloc is
/// `max(used + 1, (3 * used + 1) / 2)`; live entries replay through
/// `compactLookup(ADD, compare_only_ptrs=false)`.
fn compact_rehash(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    map_ref: GcRef,
) -> Result<(), MapError> {
    let used = map_of(ctx.arena, map_ref).used;
    let new_alloc = (used + 1).max((3 * used + 1) / 2);
    let live: Vec<(Value, Value)> = {
        let m = map_of(ctx.arena, map_ref);
        m.entries
            .iter()
            .filter_map(|e| e.key.map(|k| (k, e.value)))
            .collect()
    };
    {
        let m = map_of_mut(ctx.arena, map_ref);
        m.alloc = new_alloc;
        m.entries = Vec::with_capacity(new_alloc);
        m.index = vec![0u32; index_cap(new_alloc)];
        m.used = 0;
        m.all_keys_are_qstrs = true;
    }
    for (k, v) in live {
        match compact_lookup(ctx, types, map_ref, k, LookupKind::Add, false)? {
            LookupOutcome::Added(i) => map_set_value(ctx.arena, map_ref, i, v),
            // Unreachable on a fresh table (no duplicates, room for
            // `used` entries in `new_alloc >= used + 1` slots).
            _ => unreachable!("compact rehash replay must add"),
        }
    }
    Ok(())
}


// The set table operations live in `map_set.rs` (C9a-split); re-exported
// here so the public paths (`map::MpSet`, `map::set_lookup`, …) are unchanged.
pub use super::map_set::{set_items, set_lookup, set_lookup_hashed, MpSet, SetSlot};
