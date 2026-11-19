//! Set hash table (C9a: `src/objects/containers/map.zig`, set part).
//!
//! Split out of `map.rs` (C9a-split: the 800-line cap). `MpSet` /
//! `set_lookup` are the nano-style open-addressing table operations
//! for the set type (the set TYPE itself is C9c's). The map half
//! (`MpMap` lookup / rehash / cache) stays in `map.rs`; the set half
//! hashes through the same `hash_of` and compares through the same
//! `obj_equal` as the map half.

use crate::memory::gc::CallCtx;
use crate::objects::obj::BuiltinTypes;
use crate::objects::obj_ops::probe_equal;
use crate::objects::slots::CallHook;
use crate::objects::value::Value;
use crate::objects::containers::map::{get_hash_alloc_geq, hash_of_hook, LookupKind, MapError};

// ── `mp_set_*` (set underlying hash table) ──────────────────────────

/// One slot of an `MpSet` table: empty (NULL), tombstone (SENTINEL),
/// or a live element. The Zig stores bare `Obj` words with NULL /
/// SENTINEL markers; Rust needs the third state explicit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetSlot {
    /// NULL — an empty slot (search-terminating).
    Empty,
    /// SENTINEL — a deleted slot (probe steps over it).
    Tombstone,
    /// A live element.
    Live(Value),
}

/// A set's hash table (`mp_set_t`, map.zig: `alloc`, `used`,
/// `table`). Nano-style open addressing in both configs (unlike the
/// compact dict layout). The set TYPE (`Obj::Set`) is C9c's — this
/// is only the table operations.
#[derive(Clone, Debug)]
pub struct MpSet {
    pub alloc: usize,
    pub used: usize,
    pub table: Vec<SetSlot>,
}

impl MpSet {
    /// `mp_set_init(set, n)`.
    pub fn new(n: usize) -> MpSet {
        MpSet {
            alloc: n,
            used: 0,
            table: vec![SetSlot::Empty; n],
        }
    }

    /// `mp_set_clear`: drop the table, reset to empty.
    pub fn clear(&mut self) {
        self.table.clear();
        self.table.shrink_to_fit();
        self.alloc = 0;
        self.used = 0;
    }

    /// `mp_set_remove_first`: the first live element in TABLE order
    /// (not insertion order), tombstoned/removed; `None` when empty.
    pub fn remove_first(&mut self) -> Option<Value> {
        let mut pos = 0;
        while pos < self.alloc {
            if let SetSlot::Live(v) = self.table[pos] {
                self.used -= 1;
                // If the next slot is empty, set this one to NULL
                // (search-terminating); else SENTINEL (keep the chain).
                let next = self.table[(pos + 1) % self.alloc];
                self.table[pos] = if next == SetSlot::Empty {
                    SetSlot::Empty
                } else {
                    SetSlot::Tombstone
                };
                return Some(v);
            }
            pos += 1;
        }
        None
    }
}

/// `setRehash` (map.zig): grow via `getHashAllocGeq(alloc + 1)` and
/// replay the live elements. The replay hashes with a 0 fallback:
/// the Zig's `mp_set_lookup` reads `mp_unary_op` HASH's NULL return
/// as hash 0 (`MP_OBJ_SMALL_INT_VALUE(NULL) == 0`), so an unhashable
/// element that was inserted at hash 0 by a quirk caller (see
/// `set_lookup_hashed`) replays at hash 0 instead of failing the
/// rehash. `eq_blocked` is threaded through from the caller: while
/// a raise is pending the Zig's `mp_obj_equal` returns false for
/// every comparison (`mp_obj_is_true` returns false when pending is
/// set, obj.zig:470), so a replayed element is never "found" — the
/// replay inserts it. A set can hold two equal unhashable elements
/// that way (both inserted while pending was set); the replay must
/// keep both, which the guard gives. G3/H5: the replay hashes
/// through `hash_of_hook` and probes through
/// `set_lookup_hashed_hook` when a hook is passed, so an instance
/// element's FunBc `__hash__` / `__eq__` runs during the replay.
fn set_rehash_inner(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    eq_blocked: bool,
    mut hook: Option<CallHook<'_, '_>>,
) -> Result<(), MapError> {
    let old_table = std::mem::take(&mut set.table);
    let old_alloc = set.alloc;
    set.alloc = get_hash_alloc_geq(set.alloc + 1);
    set.used = 0;
    set.table = vec![SetSlot::Empty; set.alloc];
    for slot in old_table.into_iter().take(old_alloc) {
        if let SetSlot::Live(k) = slot {
            let hash = hash_of_hook(ctx, types, k, hook.as_deref_mut()).unwrap_or(0);
            let _ = set_lookup_hashed_hook(
                ctx,
                types,
                set,
                k,
                LookupKind::Add,
                hash,
                eq_blocked,
                hook.as_deref_mut(),
            )?;
        }
    }
    Ok(())
}

/// Live elements of an `MpSet` in TABLE order (slot 0..alloc) — the
/// order `set`/`frozenset` print and iterate in (map.zig:
/// `npy_set_print` / `npy_set_it_iternext` scan the table, not
/// insertion order).
pub fn set_items(set: &MpSet) -> Vec<Value> {
    set.table
        .iter()
        .filter_map(|s| match s {
            SetSlot::Live(v) => Some(*v),
            _ => None,
        })
        .collect()
}

/// `mp_set_lookup` (map.zig): find (or add / remove) `index` in the
/// set table. Returns the found/added element, or `None` when absent
/// (LOOKUP/REMOVE miss). `lookup_kind` is bit-tested (ADD=1,
/// REMOVE=2 — the combined kind 3 is `AddOrRemove`). Unlike the map
/// lookup there is no cache and no qstr fast path: the hash always
/// goes through `mp_unary_op` HASH (here `hash_of`, same values)
/// and equality is always `mp_obj_equal`.
pub fn set_lookup(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
) -> Result<Option<Value>, MapError> {
    set_lookup_inner(ctx, types, set, index, lookup_kind, None)
}

/// `mp_set_lookup` with a VM hook (G3/H5): the hash goes
/// through `hash_of_hook` and the probe through
/// `set_lookup_hashed_hook`, so an instance element's FunBc
/// `__hash__` / `__eq__` runs (`{A()}` then `A() in s`).
fn set_lookup_inner(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
    mut hook: Option<CallHook<'_, '_>>,
) -> Result<Option<Value>, MapError> {
    // The hash is computed once, before any mutation (it can raise).
    // NOTE: the Zig hashes here with `mp_unary_op` directly (no qstr
    // fast path in `mp_set_lookup`) — `hash_of` applies the same
    // `qstr_hash` for qstrs, so the values agree.
    let hash = hash_of_hook(ctx, types, index, hook.as_deref_mut())?;
    set_lookup_hashed_hook(ctx, types, set, index, lookup_kind, hash, false, hook)
}

/// `mp_set_lookup` with a VM hook (G3/H5) — the public entry
/// for callers that already have a hash or a quirk to preserve
/// (see `set_lookup_quirk_hook` in `objset.rs`).
pub fn set_lookup_hook(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Option<Value>, MapError> {
    set_lookup_inner(ctx, types, set, index, lookup_kind, hook)
}

/// `mp_set_lookup` with a precomputed `hash` (map.zig). The set
/// TYPE (C9c) calls this when the hash already failed: the Zig's
/// `mp_unary_op` HASH bridge returns NULL on failure and the caller
/// reads it as hash 0 (`MP_OBJ_SMALL_INT_VALUE(NULL) == 0`), so an
/// unhashable element is STILL inserted (at slot 0) and the error
/// only surfaces afterwards — verified: `s.add([2])` raises
/// `TypeError` but `s == {[2]}`. Callers that use this variant own
/// the pending-error bookkeeping (first pending wins); this function
/// never hashes and never fails on the hash.
///
/// `eq_blocked`: the Zig's probe compares with `mp_obj_equal`,
/// which is `mp_obj_is_true(equalNotEqualImpl(...))` — and
/// `mp_obj_is_true` returns false whenever a raise is pending
/// (obj.zig:470). The quirk caller passes `true` here when it has
/// already recorded a pending hash error, so the probe never
/// "finds" an element — verified: with `s == {[2]}` (inserted by
/// the quirk), `s.remove([2])` raises `KeyError '[2]'` instead of
/// removing it, and `[2] in s` raises the hash `TypeError`.
/// `set_lookup` (no pending possible) passes `false`.
pub fn set_lookup_hashed(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
    hash: usize,
    eq_blocked: bool,
) -> Result<Option<Value>, MapError> {
    set_lookup_hashed_inner(ctx, types, set, index, lookup_kind, hash, eq_blocked, None)
}

/// `set_lookup_hashed` with a VM hook (G3/H5): the probe
/// comparison runs through `probe_equal`, so an instance
/// element's FunBc `__eq__` runs (a raising `__eq__` reads
/// as not-equal there, per the vendor's `mp_obj_is_true`
/// pending guard — see `probe_equal`).
pub fn set_lookup_hashed_hook(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
    hash: usize,
    eq_blocked: bool,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Option<Value>, MapError> {
    set_lookup_hashed_inner(ctx, types, set, index, lookup_kind, hash, eq_blocked, hook)
}

#[allow(clippy::too_many_arguments)]
fn set_lookup_hashed_inner(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
    hash: usize,
    eq_blocked: bool,
    mut hook: Option<CallHook<'_, '_>>,
) -> Result<Option<Value>, MapError> {
    let kind = lookup_kind as i32;
    let want_add = (kind & LookupKind::Add as i32) != 0;
    let want_remove = (kind & LookupKind::Remove as i32) != 0;
    if set.alloc == 0 {
        if want_add {
            set_rehash_inner(ctx, types, set, eq_blocked, hook.as_deref_mut())?;
        } else {
            return Ok(None);
        }
    }
    let mut pos = hash % set.alloc;
    let mut start_pos = pos;
    let mut avail_slot: Option<usize> = None;
    loop {
        match set.table[pos] {
            SetSlot::Empty => {
                // Found a NULL slot — index is not in the table.
                if want_add {
                    let dest = avail_slot.unwrap_or(pos);
                    set.used += 1;
                    set.table[dest] = SetSlot::Live(index);
                    return Ok(Some(index));
                }
                return Ok(None);
            }
            SetSlot::Tombstone => {
                if avail_slot.is_none() {
                    avail_slot = Some(pos);
                }
            }
            SetSlot::Live(elem) => {
                // A pending raise makes `mp_obj_equal` return false
                // (see `eq_blocked` above). With a hook the probe
                // is `probe_equal`: a FunBc `__eq__` runs, and a
                // raise inside it reads as not-equal (the vendor's
                // `mp_obj_is_true` pending guard, obj.zig:470) —
                // the stashed raise is discarded inside
                // `probe_equal`, never propagated.
                let found = if eq_blocked {
                    false
                } else {
                    probe_equal(ctx, types, elem, index, hook.as_deref_mut())
                        .map_err(MapError::from)?
                };
                if found {
                    if want_remove {
                        set.used -= 1;
                        let next = set.table[(pos + 1) % set.alloc];
                        set.table[pos] = if next == SetSlot::Empty {
                            SetSlot::Empty
                        } else {
                            SetSlot::Tombstone
                        };
                    }
                    return Ok(Some(elem));
                }
            }
        }
        pos = (pos + 1) % set.alloc;
        if pos == start_pos {
            // Search wrapped — index not in the table.
            if want_add {
                if let Some(s) = avail_slot {
                    set.used += 1;
                    set.table[s] = SetSlot::Live(index);
                    return Ok(Some(index));
                }
                // Not enough room — rehash and restart.
                set_rehash_inner(ctx, types, set, eq_blocked, hook.as_deref_mut())?;
                pos = hash % set.alloc;
                start_pos = pos;
                avail_slot = None;
            } else {
                return Ok(None);
            }
        }
    }
}
