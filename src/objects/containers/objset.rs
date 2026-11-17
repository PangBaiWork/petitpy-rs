//! Set and frozenset (C9c: `src/objects/containers/objset.zig`, 747 lines, 0 test blocks).
//!
//! The table itself is C9a's `MpSet` (`map_set.rs`) — `mp_set_t` is
//! NOT a `MapData` (the brief's "set 的底层就是 C9a 的 MapData" is
//! imprecise; the Zig stores sets in `mp_set_t`, and the Rust
//! equivalent is `MpSet` + `set_lookup` / `set_lookup_hashed`).
//! This file is the TYPE: make_new, unary/binary slots, print,
//! getiter, the type data; the method bodies (add/clear/copy/…)
//! and the shared set-operation internals (diff/intersect/…)
//! live in `objset_methods.rs` (C9a-split, 800-line cap).
//!
//! # The polled-pending model (objset.zig + map.zig + runtime.zig)
//!
//! The Zig has one global `pending_exc`; every raise OVERWRITES it
//! (`raiseMsg` assigns unconditionally, runtime.zig:218). Two
//! pre-flight guards shape the observable behaviour:
//!
//! * `mp_iternext` returns 0 (`== OBJ_STOP_ITERATION`,
//!   mp_mirrors.zig:493) immediately when pending is set, WITHOUT
//!   clearing it (runtime.zig:1359-1362). A make_new / update /
//!   difference loop therefore stops pulling items the moment any
//!   pending is set — items after the first failure are never stored.
//! * `mp_obj_equal` is `mp_obj_is_true(equalNotEqualImpl(...))`,
//!   and `mp_obj_is_true` returns false whenever pending is set
//!   (obj.zig:470). A set lookup performed while a hash error is
//!   pending NEVER finds its element — verified: with `s == {[2]}`
//!   (inserted by the quirk below), `s.remove([2])` raises
//!   `KeyError '[2]'` instead of removing it.
//!
//! Rust has no global pending, so this file threads a local
//! `pending: &mut Option<SetError>` through the store/lookup
//! helpers. The rules, all verified on the reference binary:
//!
//! * Arg-check mismatch is recorded first (the Zig calls
//!   `mp_arg_check_num_sig` as a bare statement — return ignored,
//!   objset.zig:134) but the iterable is STILL fetched:
//!   `set(1,2)` → `TypeError 'object not iterable'`, NOT the
//!   mismatch — `mp_getiter` has no pre-flight and its error
//!   OVERWRITES the mismatch. `set([[]],[2])` → mismatch (getiter
//!   succeeds, then the first `mp_iternext` returns 0 and no item
//!   is ever stored).
//! * A store hash failure records `TypeError "unsupported type for
//!   operator"` and the element is STILL inserted at hash 0 (the
//!   Zig's `mp_unary_op` HASH bridge returns NULL on failure and
//!   the caller reads it as hash 0) — verified: `s.add([2])`
//!   raises but `s == {[2]}`. The loop then breaks (next
//!   `mp_iternext` returns 0), so later items are not stored.
//! * A LATER getiter failure OVERWRITES an earlier hash pending —
//!   verified: `s.update([[]], 1)` → `TypeError 'object not
//!   iterable'` with `s == {[], 1}`; `{1,2}.difference([[]], 1)`
//!   → the same.
//! * `remove`'s lookup miss raises `KeyError(item)`, which
//!   OVERWRITES the hash pending — verified: `s.remove([2])` on
//!   `s == {[2]}` → `KeyError '[2]'`.
//! * `OutOfMemory` propagates immediately, never recorded as pending.
//!
//! # Table order
//!
//! Print and iteration scan the TABLE (slot 0..alloc), not insertion
//! order. `mp_obj_new_set(n, items)` sets `alloc = n` exactly, while
//! make_new builds from an empty set (`alloc = 0`, rehashing to 2
//! then 4) — so `{1,2,3}` prints `{3, 1, 2}` but
//! `frozenset({1,2,3})` prints `{1, 2, 3}`. `set_items` (table
//! order) is the iteration order everywhere.
//!
//! # Frozenset hash seed
//!
//! The Zig seeds with the type object's address
//! (`@intFromPtr(&mp_type_frozenset)`); `nm` on the reference binary
//! puts `mp_type_frozenset` at `0x15c0e50` = 22810192, exactly
//! `hash(frozenset())`. The seed is hardcoded below with that
//! citation. Element hashes wrapping-add onto it
//! (`hash(frozenset({1,2}))` = 22810192+1+2 = 22810195, verified).

use super::map::{hash_of, LookupKind, MapError};
use super::map_set::{set_items, set_lookup_hashed, MpSet, SetSlot};
use super::objdict::ARG_NUM_TYPES_MISMATCH;
use super::objdict::DictError;
use super::objdict_methods::collect_iterable;
use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{BuiltinTypes, ObjError};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{GcRef, IteratorData, Obj, SetData, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::objects::containers::objset_methods::{
    set_diff_impl, set_equal_impl, set_intersect_impl, set_issubset_impl, set_symdiff_impl,
    set_symdiff_update_impl, set_union_impl, set_update_impl,
};

/// `hash(frozenset())` — the address of `mp_type_frozenset` in the
/// reference binary (`nm`: `0x15c0e50` = 22810192), which the Zig
/// uses as the hash seed (`@intFromPtr(&mp_type_frozenset)`,
/// objset.zig `npy_set_unary_op` HASH branch). Element hashes
/// wrapping-add onto it. The Zig's value is address-dependent;
/// the Rust port seeds with the frozenset *type's* arena index
/// instead (C12a — `ctx.types.frozenset.0`), so
/// `hash(frozenset())` equals the frozenset type's id, as in the
/// reference (`hash(frozenset()) == id(frozenset)` for the type).
/// NOTE: the seed is NOT the address of the reference binary's
/// type object, so the absolute hash values differ from the
/// reference binary's (the structure is identical).

/// Errors from set/frozenset operations, mirroring `DictError`
/// (C9a): the exception type name + message travel in the enum;
/// the slot boundary drops them to `PyError` (the accepted C6/C7r
/// convention — C13 constructs the exception object from the rich
/// error instead).
#[derive(Debug, Clone, PartialEq)]
pub enum SetError {
    /// `TypeError(msg)` — arg-count mismatch, unhashable element,
    /// not-iterable, unsupported operator, …
    TypeError(&'static str),
    /// `KeyError(item)` — `remove` on a missing element.
    KeyError(Value),
    /// `KeyError("pop from an empty set")`.
    KeyErrorPopEmpty,
    /// `ValueError(msg)` — (reserved; no set op raises it today).
    ValueError(&'static str),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary).
    PyRaise,
}

impl SetError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            SetError::TypeError(_) => "TypeError",
            SetError::KeyError(_) | SetError::KeyErrorPopEmpty => "KeyError",
            SetError::ValueError(_) => "ValueError",
            SetError::OutOfMemory => "MemoryError",
            SetError::PyRaise => "",
        }
    }

    /// The exception message (`None` for `KeyError(item)`, whose
    /// message is the item's repr — C13 formats it).
    pub fn msg(&self) -> Option<&'static str> {
        match self {
            SetError::TypeError(m) | SetError::ValueError(m) => Some(m),
            SetError::KeyErrorPopEmpty => Some("pop from an empty set"),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — see the
    /// enum docs).
    pub fn to_pyerror(self) -> PyError {
        match self {
            SetError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<SetError> for PyError {
    fn from(e: SetError) -> PyError {
        e.to_pyerror()
    }
}

impl From<MapError> for SetError {
    fn from(e: MapError) -> SetError {
        match e {
            MapError::TypeError(m) => SetError::TypeError(m),
            MapError::OutOfMemory => SetError::OutOfMemory,
            MapError::PyRaise => SetError::PyRaise,
        }
    }
}

impl From<ObjError> for SetError {
    fn from(e: ObjError) -> SetError {
        match e {
            ObjError::OutOfMemory => SetError::OutOfMemory,
            _ => SetError::PyRaise,
        }
    }
}

impl From<PyError> for SetError {
    fn from(e: PyError) -> SetError {
        match e {
            PyError::OutOfMemory => SetError::OutOfMemory,
            PyError::PyRaise => SetError::PyRaise,
        }
    }
}

impl From<DictError> for SetError {
    fn from(e: DictError) -> SetError {
        match e {
            DictError::TypeError(m) => SetError::TypeError(m),
            DictError::ValueError(m) => SetError::ValueError(m),
            DictError::KeyError(v) => SetError::KeyError(v),
            DictError::KeyErrorPopitemEmpty => SetError::KeyErrorPopEmpty,
            DictError::OutOfMemory => SetError::OutOfMemory,
            DictError::PyRaise => SetError::PyRaise,
        }
    }
}

/// Arity check for `BuiltinFn` adapters (C9a's `check_arity`,
/// private to `objdict_methods.rs` — same shape, `SetError`).
pub(crate) fn check_arity(args: &[Value], min: usize, max: usize) -> Result<(), SetError> {
    if (min..=max).contains(&args.len()) {
        Ok(())
    } else {
        Err(SetError::TypeError(ARG_NUM_TYPES_MISMATCH))
    }
}

/// `args[0]` — the `self` of a bound method adapter.
pub(crate) fn first(args: &[Value]) -> Value {
    args[0]
}

// ── type predicates ──────────────────────────────────────────────────

/// The `GcRef` of a set/frozenset value (panics otherwise — the
/// slots and methods are only called on sets; the binary op and
/// the adapters check first).
pub(crate) fn set_ref_of(arena: &GcArena, v: Value) -> GcRef {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Set(_) => r,
            _ => panic!("set operation on a non-set value"),
        },
        _ => panic!("set operation on a non-heap value"),
    }
}

/// `isSetOrFrozenset` (objset.zig:96): header type is `set` or
/// `frozenset`.
pub(crate) fn is_set_or_frozenset(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    match v {
        Value::Heap(r) => matches!(arena.header(r).type_, Some(t) if t == types.set || t == types.frozenset),
        _ => false,
    }
}

/// Exact `set` (not a subclass, not a frozenset) — the in-place
/// operators only mutate exact sets.
pub(crate) fn is_exact_set(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    match v {
        Value::Heap(r) => arena.header(r).type_ == Some(types.set),
        _ => false,
    }
}

/// Exact `frozenset` — the HASH branch and the print prefix.
pub(crate) fn is_frozenset(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    match v {
        Value::Heap(r) => arena.header(r).type_ == Some(types.frozenset),
        _ => false,
    }
}

/// The live element count (`mp_set_t.used`).
pub(crate) fn set_used(arena: &GcArena, v: Value) -> usize {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Set(s) => s.table.used,
            _ => panic!("set_used on a non-set value"),
        },
        _ => panic!("set_used on a non-heap value"),
    }
}

// ── allocation ──────────────────────────────────────────────────────

/// Allocate a fresh set with `alloc = n` (`mp_set_init(set, n)`).
/// `n == 0` is the make_new / empty path; `n == items.len()` is
/// `mp_obj_new_set(n, items)`.
pub(crate) fn alloc_set(ctx: &mut CallCtx, type_: GcRef, n: usize) -> Value {
    Value::Heap(ctx.alloc(Some(type_), Obj::Set(SetData { table: MpSet::new(n) })))
}

/// `setCopyImpl` (objset.zig): a fresh set of the SAME header type,
/// `alloc`/`used`/table copied element-wise (tombstones included).
pub(crate) fn clone_set(ctx: &mut CallCtx, self_v: Value) -> Result<Value, SetError> {
    let (type_, table) = match self_v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Set(s) => (ctx.arena.header(r).type_, s.table.clone()),
            _ => panic!("clone_set on a non-set value"),
        },
        _ => panic!("clone_set on a non-heap value"),
    };
    let Some(t) = type_ else {
        panic!("clone_set on a set without a header type");
    };
    Ok(Value::Heap(ctx.alloc(
        Some(t),
        Obj::Set(SetData { table }),
    )))
}

/// Run `f` with the set's table taken out of the arena. Needed
/// because `set_lookup` takes `&mut MpSet` (caller-owned, C9a)
/// while `f` also needs `&mut CallCtx` — the two borrows cannot
/// coexist on the arena. The table's elements are rooted in
/// `ctx.roots.extra` for the duration, so a `__hash__`/`__eq__`
/// slot that allocates cannot free them (the Zig's conservative GC
/// scan follows every word of the heap object, including the table
/// pointer). The table is put back even when `f` returns an error;
/// a panic inside `f` would lose it (no `f` in this lane panics on
/// a reachable path).
pub(crate) fn with_set_table<R>(
    ctx: &mut CallCtx,
    set_ref: GcRef,
    f: impl FnOnce(&mut CallCtx, &mut MpSet) -> R,
) -> R {
    let mut table = match ctx.arena.get_mut(set_ref) {
        Obj::Set(s) => std::mem::replace(&mut s.table, MpSet::new(0)),
        _ => panic!("with_set_table on a non-set"),
    };
    let mark = ctx.roots.extra.len();
    ctx.roots.extra.extend(set_items(&table).iter().copied());
    let out = f(ctx, &mut table);
    ctx.roots.extra.truncate(mark);
    match ctx.arena.get_mut(set_ref) {
        Obj::Set(s) => s.table = table,
        _ => panic!("with_set_table on a non-set"),
    }
    out
}

/// Replace a set's table wholesale (intersect-update's table swap,
/// objset.zig `setIntersectInt` update branch); returns the old one.
pub(crate) fn replace_set_table(ctx: &mut CallCtx, set_ref: GcRef, new_table: MpSet) -> MpSet {
    match ctx.arena.get_mut(set_ref) {
        Obj::Set(s) => std::mem::replace(&mut s.table, new_table),
        _ => panic!("replace_set_table on a non-set"),
    }
}

// ── lookup with the unhashable-insert quirk ─────────────────────────

/// `mp_set_lookup` with the Zig's unhashable-element behaviour:
/// the hash is tried first; on failure the error is recorded as
/// pending (first pending wins — `pending` already `Some` keeps
/// its error) and the lookup proceeds at hash 0, with the probe's
/// equality comparisons blocked (the `mp_obj_is_true` pending
/// guard, obj.zig:470 — pass `pending.is_some()` as `eq_blocked`
/// to `set_lookup_hashed`). `OutOfMemory` propagates immediately,
/// never recorded. A `__eq__` slot raise inside the probe is
/// recorded as pending and reported as "not found" (the Zig treats
/// it as not-equal and keeps probing; no type in the tree today
/// has a raising `__eq__`, so the continuation is unobservable).
pub(crate) fn set_lookup_quirk(
    ctx: &mut CallCtx,
    set_ref: GcRef,
    item: Value,
    kind: LookupKind,
    pending: &mut Option<SetError>,
) -> Result<Option<Value>, SetError> {
    let hash = match hash_of(ctx, ctx.types, item) {
        Ok(h) => h,
        Err(MapError::OutOfMemory) => return Err(SetError::OutOfMemory),
        Err(e) => {
            if pending.is_none() {
                *pending = Some(SetError::from(e));
            }
            0
        }
    };
    let eq_blocked = pending.is_some();
    let found = with_set_table(ctx, set_ref, |ctx, table| {
        set_lookup_hashed(ctx, ctx.types, table, item, kind, hash, eq_blocked)
    });
    match found {
        Ok(v) => Ok(v),
        Err(e) => {
            if pending.is_none() {
                *pending = Some(SetError::from(e));
            }
            Ok(None)
        }
    }
}

/// `mp_obj_set_store` (objset.zig:510): ADD lookup, quirk included.
/// Only `OutOfMemory` escapes as `Err`; a hash failure is recorded
/// in `pending` and the element is still inserted.
pub(crate) fn set_store(
    ctx: &mut CallCtx,
    set_v: Value,
    item: Value,
    pending: &mut Option<SetError>,
) -> Result<(), SetError> {
    let r = set_ref_of(ctx.arena, set_v);
    let _ = set_lookup_quirk(ctx, r, item, LookupKind::Add, pending)?;
    Ok(())
}

// ── construction ────────────────────────────────────────────────────

/// `mp_obj_new_set(n_args, items)` (objset.zig): `alloc = n`
/// exactly, then store each item (quirk included — an unhashable
/// item is still inserted and the first hash error is returned at
/// the end; the partially-built set is dropped with the error,
/// exactly as the VM discards it when the pending raise fires).
pub fn set_new(ctx: &mut CallCtx, type_: GcRef, items: &[Value]) -> Result<Value, SetError> {
    let set_v = alloc_set(ctx, type_, items.len());
    let r = set_ref_of(ctx.arena, set_v);
    let mut pending: Option<SetError> = None;
    for &item in items {
        set_lookup_quirk(ctx, r, item, LookupKind::Add, &mut pending)?;
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(set_v),
    }
}

/// `npy_set_make_new` (objset.zig:133-151), sig (0,1) — see the
/// module docs for the pending model. `args` is positionals
/// followed by interleaved kw pairs (the `dict_make_new` layout);
/// `n_kw` is the kw-pair count.
pub fn set_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, SetError> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(SetError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    // `mp_arg_check_num_sig` as a bare statement (return ignored):
    // the mismatch is recorded, but construction continues — the
    // getiter below may overwrite it, and the loop never stores
    // while it is set (the `mp_iternext` pre-flight).
    let mut pending: Option<SetError> = if n_kw != 0 || n_pos > 1 {
        Some(SetError::TypeError(ARG_NUM_TYPES_MISMATCH))
    } else {
        None
    };
    if n_pos == 0 {
        return match pending {
            Some(e) => Err(e),
            None => Ok(alloc_set(ctx, type_, 0)),
        };
    }
    // Any n_args != 0 takes the iterable path with args[0].
    let set_v = alloc_set(ctx, type_, 0);
    match collect_iterable(ctx, args[0]) {
        Err(e) => {
            // `mp_getiter` has no pre-flight: its error overwrites
            // whatever pending was set (verified: `set(1,2)` →
            // 'object not iterable', not the mismatch).
            pending = Some(SetError::from(e));
        }
        Ok(items) => {
            if pending.is_none() {
                for item in items {
                    set_store(ctx, set_v, item, &mut pending)?;
                    if pending.is_some() {
                        // Next `mp_iternext` returns 0 (pre-flight):
                        // no more items are pulled or stored.
                        break;
                    }
                }
            }
        }
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(set_v),
    }
}

/// Convert a non-set argument the way the set methods do
/// (`setIssubsetInternal` / `setDiffInt` call `npy_set_make_new`
/// with the plain `set` type — even from a frozenset method).
/// Already-a-set values pass through untouched.
pub(crate) fn to_set(ctx: &mut CallCtx, value: Value) -> Result<Value, SetError> {
    if is_set_or_frozenset(ctx.arena, ctx.types, value) {
        Ok(value)
    } else {
        let type_ = ctx.types.set;
        set_make_new(ctx, type_, &[value], 0)
    }
}

// ── slots ───────────────────────────────────────────────────────────

/// `npy_set_unary_op` (objset.zig): BOOL → `used != 0`, LEN →
/// `used`, HASH → frozensets only (seed + wrapping element-hash
/// sum); a mutable set's HASH and every other op return NULL
/// (`Err(PyError::PyRaise)` = "not implemented", C6 convention).
pub fn set_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_v: Value) -> PyResult<Value> {
    match op {
        UnaryOp::Bool => Ok(Value::Bool(set_used(ctx.arena, self_v) != 0)),
        UnaryOp::Len => Ok(Value::int(
            ctx.arena,
            Some(ctx.types.int),
            ctx.roots,
            set_used(ctx.arena, self_v) as i64,
        )),
        UnaryOp::Hash => {
            if !is_frozenset(ctx.arena, ctx.types, self_v) {
                return Err(PyError::PyRaise);
            }
            // The Zig adds `smallIntValue(mp_unary_op(HASH, elem))`
            // per table-order element; a failing element hash sets
            // pending and contributes 0, but the pending raise wins
            // over the returned value — so returning the error
            // immediately is observably identical.
            // The seed is the frozenset type's identity word
            // (C12a — the Zig uses `@intFromPtr(&mp_type_frozenset)`,
            // 22810192 in the reference binary; `hash(frozenset())`
            // equals the frozenset *type's* id, not an instance's).
            let mut hash: i64 = ctx.types.frozenset.0 as i64;
            let items = match self_v {
                Value::Heap(r) => match ctx.arena.get(r) {
                    Obj::Set(s) => set_items(&s.table),
                    _ => panic!("set_unary_op on a non-set value"),
                },
                _ => panic!("set_unary_op on a non-heap value"),
            };
            for item in items {
                let h = hash_of(ctx, ctx.types, item).map_err(SetError::from)?;
                hash = hash.wrapping_add(h as i64);
            }
            // `newSmallInt(hash)` is a raw shift with no window
            // check — the Zig reads back a corrupted value when the
            // sum leaves the 62-bit window; Rust uses `Value::int`
            // (heap `Int` then), which is deterministic. Documented
            // in the result file.
            Ok(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, hash))
        }
        _ => Err(PyError::PyRaise),
    }
}

/// `npy_set_binary_op` (objset.zig): non-set rhs (except CONTAINS)
/// → NULL; `|`/`^`/`&`/`-` preserve the lhs type via copy (except
/// `&`, which always builds a fresh plain `set`); in-place ops
/// mutate only an exact `set` lhs. Comparisons delegate to the
/// subset/superset internals; EQUAL is size + subset (no type
/// check — `set == frozenset` with equal elements is `True`).
pub fn set_binary_op(ctx: &mut CallCtx, op: BinaryOp, lhs: Value, rhs: Value) -> PyResult<Value> {
    if op != BinaryOp::Contains && !is_set_or_frozenset(ctx.arena, ctx.types, rhs) {
        return Err(PyError::PyRaise);
    }
    let update = is_exact_set(ctx.arena, ctx.types, lhs);
    let out = match op {
        BinaryOp::Or => set_union_impl(ctx, lhs, rhs),
        BinaryOp::Xor => set_symdiff_impl(ctx, lhs, rhs),
        BinaryOp::And => set_intersect_impl(ctx, lhs, rhs, false),
        BinaryOp::Subtract => set_diff_impl(ctx, lhs, &[rhs], false),
        BinaryOp::InplaceOr => {
            if update {
                return set_update_impl(ctx, lhs, &[rhs])
                    .map(|_| lhs)
                    .map_err(SetError::to_pyerror);
            }
            set_union_impl(ctx, lhs, rhs)
        }
        BinaryOp::InplaceXor => {
            if update {
                return set_symdiff_update_impl(ctx, lhs, rhs)
                    .map(|_| lhs)
                    .map_err(SetError::to_pyerror);
            }
            set_symdiff_impl(ctx, lhs, rhs)
        }
        BinaryOp::InplaceAnd => {
            let out = set_intersect_impl(ctx, lhs, rhs, update);
            if update {
                return out.map(|_| lhs).map_err(SetError::to_pyerror);
            }
            out
        }
        BinaryOp::InplaceSubtract => set_diff_impl(ctx, lhs, &[rhs], update),
        BinaryOp::Less => set_issubset_impl(ctx, lhs, rhs, true),
        BinaryOp::More => set_issubset_impl(ctx, rhs, lhs, true),
        BinaryOp::Equal => set_equal_impl(ctx, lhs, rhs),
        BinaryOp::LessEqual => set_issubset_impl(ctx, lhs, rhs, false),
        BinaryOp::MoreEqual => set_issubset_impl(ctx, rhs, lhs, false),
        BinaryOp::Contains => {
            // LOOKUP quirk: a hash failure records pending and the
            // probe never finds the element (eq guard) → `False`,
            // and the pending raise wins (verified: `[2] in {[2]}`
            // → `TypeError 'unsupported type for operator'`).
            let r = set_ref_of(ctx.arena, lhs);
            let mut pending: Option<SetError> = None;
            let found = set_lookup_quirk(ctx, r, rhs, LookupKind::Lookup, &mut pending)?;
            return match pending {
                Some(e) => Err(e.to_pyerror()),
                None => Ok(Value::Bool(found.is_some())),
            };
        }
        _ => return Err(PyError::PyRaise),
    };
    out.map_err(SetError::to_pyerror)
}

/// `npy_set_print` (objset.zig:109-130): empty → `set()` /
/// `frozenset()`; otherwise `{a, b}` / `frozenset({a, b})`,
/// elements in PRINT_REPR, table order. The `kind` is ignored —
/// the Zig always prints REPR here.
pub fn set_print(ctx: &mut CallCtx, v: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let frozen = is_frozenset(ctx.arena, ctx.types, v);
    let items = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Set(s) => set_items(&s.table),
            _ => panic!("set_print on a non-set value"),
        },
        _ => panic!("set_print on a non-heap value"),
    };
    if items.is_empty() {
        let _ = sink.write_str(if frozen { "frozenset()" } else { "set()" });
        return;
    }
    if frozen {
        let _ = sink.write_str("frozenset({");
    } else {
        let _ = sink.write_str("{");
    }
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            let _ = sink.write_str(", ");
        }
        obj_print_helper(ctx, ctx.types, *item, PrintKind::Repr, sink);
    }
    let _ = sink.write_str(if frozen { "})" } else { "}" });
}

/// `npy_set_getiter` (objset.zig:167-173): an `Obj::Iterator` over
/// the set, header type `None` until C12 (the dict-view precedent);
/// `set_it_next` advances it in table order.
pub fn set_getiter(ctx: &mut CallCtx, self_v: Value) -> PyResult<Value> {
    Ok(Value::Heap(ctx.alloc(
        None,
        Obj::Iterator(IteratorData {
            source: self_v,
            index: 0,
            extra: vec![],
        }),
    )))
}

/// The set iterator's next step (C11 wires it): the next live
/// table element at/after `index`, `None` when exhausted
/// (`StopIteration`) — the `dict_view_it_next` convention
/// (`Result<Option<Value>, DictError>` there).
pub fn set_it_next(ctx: &mut CallCtx, iter_v: Value) -> Result<Option<Value>, SetError> {
    let (set_v, mut cur) = match iter_v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(it) if it.extra.is_empty() => (it.source, it.index),
            _ => panic!("set_it_next on a non-set-iterator value"),
        },
        _ => panic!("set_it_next on a non-heap value"),
    };
    let r = set_ref_of(ctx.arena, set_v);
    let mut next = None;
    {
        let mut i = cur;
        if let Obj::Set(s) = ctx.arena.get(r) {
            while i < s.table.alloc {
                if let SetSlot::Live(v) = s.table.table[i] {
                    next = Some(v);
                    i += 1;
                    break;
                }
                i += 1;
            }
        }
        cur = i;
    }
    if let Value::Heap(r) = iter_v {
        if let Obj::Iterator(it) = ctx.arena.get_mut(r) {
            it.index = cur;
        }
    }
    Ok(next)
}

// ── type data ───────────────────────────────────────────────────────

/// `mp_type_set` ROM data (objset.zig): flags `0x0000`, slots
/// make_new/print/unary_op/binary_op/getiter. `make_new` is the
/// type-call path, wired by C12, not a `TypeData` slot (the
/// `dict_type_data` precedent). `dict` is the `locals_dict` ref —
/// built by C12 from the `SET_METHODS` table.
pub fn set_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"set").expect("MP_QSTR_set is in the static table"),
        flags: 0x0000,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(set_unary_op),
        binary_op: Some(set_binary_op),
        call: None,
        getiter: Some(set_getiter),
        subscr: None,
        buffer: None,
        print: Some(set_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}

/// `mp_type_frozenset` ROM data (objset.zig): flags `0x0008`
/// (`FLAG_EQ_CHECKS_OTHER_TYPE`); same slots as `set`, different
/// locals dict (C12 builds it from `FROZENSET_METHODS`).
pub fn frozenset_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"frozenset").expect("MP_QSTR_frozenset is in the static table"),
        flags: 0x0008,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(set_unary_op),
        binary_op: Some(set_binary_op),
        call: None,
        getiter: Some(set_getiter),
        subscr: None,
        buffer: None,
        print: Some(set_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}
