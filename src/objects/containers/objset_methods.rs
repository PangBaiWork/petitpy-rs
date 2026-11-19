//! Set/frozenset methods (C9c: `src/objects/containers/objset.zig`
//! method section + binary-op helpers, split out of `objset.rs`
//! for the 800-line cap — the C9a `objdict_methods.rs` precedent).
//!
//! The shared internals (`set_diff_impl`, `set_intersect_impl`,
//! …) are used by BOTH the method adapters below and the
//! `set_binary_op` slot in `objset.rs`; the Zig shares the same
//! static fns the same way (`setDiffInt`, `setIntersectInt`, …).
//! The pending model is documented in `objset.rs` — the short
//! version: every raise overwrites the single pending slot; a
//! getiter failure overwrites an earlier hash pending; a store
//! hash failure inserts at hash 0 and stops the loop (the next
//! `mp_iternext` returns 0); the identity checks (`self == other`)
//! run even while pending is set.

use super::map_set::MpSet;
use super::objdict_methods::collect_iterable;
use super::objset::{
    alloc_set, check_arity, clone_set, first, replace_set_table, set_binary_op, set_lookup_quirk,
    set_lookup_quirk_hook, set_ref_of, set_store, set_store_hook, set_used, to_set, SetError,
};
use super::map::LookupKind;
use crate::memory::gc::CallCtx;
use crate::objects::slots::{BinaryOp, BuiltinFn, CallHook};
use crate::objects::value::Value;
use crate::pyerr::PyResult;

// ── shared set operations (binary op + methods) ─────────────────────

/// One iterable's worth of `update` (`setUpdateOne`, objset.zig):
/// getiter (its failure OVERWRITES pending) then ADD each item;
/// a store hash failure records pending and stops the loop. Runs
/// even while pending is set (the getiter has no pre-flight); the
/// stores are skipped then (the `mp_iternext` pre-flight).
fn set_update_one(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
    pending: &mut Option<SetError>,
) -> Result<(), SetError> {
    match collect_iterable(ctx, other, None) {
        Err(e) => {
            *pending = Some(SetError::from(e));
        }
        Ok(items) => {
            if pending.is_none() {
                for item in items {
                    set_store(ctx, self_v, item, pending)?;
                    if pending.is_some() {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// `npy_set_update` (var args): `update` over each argument in
/// order; returns `None` (the Zig returns `OBJ_NONE`).
pub(crate) fn set_update_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    others: &[Value],
) -> Result<Value, SetError> {
    let mut pending: Option<SetError> = None;
    for &other in others {
        set_update_one(ctx, self_v, other, &mut pending)?;
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `setUnionImpl` (objset.zig): a copy of self, then one update.
/// The copy preserves the header type (frozenset stays frozenset).
pub(crate) fn set_union_impl(ctx: &mut CallCtx, self_v: Value, other: Value) -> Result<Value, SetError> {
    let out = clone_set(ctx, self_v)?;
    let mut pending: Option<SetError> = None;
    set_update_one(ctx, out, other, &mut pending)?;
    match pending {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// `setDiffInt(n_args, args, update)` (objset.zig): `update` mutates
/// self, otherwise a copy is differenced. For each other: identity
/// (`self == other`, Obj word equality) → clear (runs even while
/// pending is set — verified: `s.difference_update([[]], s)` clears
/// `s`); otherwise getiter (failure overwrites pending) and REMOVE
/// each element. Returns the worked set (the `difference_update`
/// adapter maps it to `None`).
pub(crate) fn set_diff_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    others: &[Value],
    update: bool,
) -> Result<Value, SetError> {
    let work = if update { self_v } else { clone_set(ctx, self_v)? };
    let mut pending: Option<SetError> = None;
    for &other in others {
        if work == other {
            let r = set_ref_of(ctx.arena, work);
            super::objset::with_set_table(ctx, r, |_, table| table.clear());
        } else {
            match collect_iterable(ctx, other, None) {
                Err(e) => {
                    pending = Some(SetError::from(e));
                }
                Ok(items) => {
                    if pending.is_none() {
                        let r = set_ref_of(ctx.arena, work);
                        for item in items {
                            let _ =
                                set_lookup_quirk(ctx, r, item, LookupKind::Remove, &mut pending)?;
                            if pending.is_some() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(work),
    }
}

/// `setIntersectInt(self, other, update)` (objset.zig): identity →
/// copy (or `None` for update) with no iteration; otherwise a
/// fresh plain `set` (`mp_obj_new_set(0, null)` — type `set` even
/// for frozensets) collects the elements of `other` that are in
/// self; `update` swaps self's table with the result's.
pub(crate) fn set_intersect_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
    update: bool,
) -> Result<Value, SetError> {
    if self_v == other {
        return if update {
            Ok(Value::None)
        } else {
            clone_set(ctx, self_v)
        };
    }
    let out = alloc_set(ctx, ctx.types.set, 0);
    let mut pending: Option<SetError> = None;
    match collect_iterable(ctx, other, None) {
        Err(e) => {
            pending = Some(SetError::from(e));
        }
        Ok(items) => {
            let self_r = set_ref_of(ctx.arena, self_v);
            for item in items {
                let found = set_lookup_quirk(ctx, self_r, item, LookupKind::Lookup, &mut pending)?;
                if pending.is_some() {
                    break;
                }
                if found.is_some() {
                    set_store(ctx, out, item, &mut pending)?;
                    if pending.is_some() {
                        break;
                    }
                }
            }
        }
    }
    match pending {
        Some(e) => Err(e),
        None => {
            if update {
                let out_table =
                    replace_set_table(ctx, set_ref_of(ctx.arena, out), MpSet::new(0));
                let _old = replace_set_table(ctx, set_ref_of(ctx.arena, self_v), out_table);
                Ok(Value::None)
            } else {
                Ok(out)
            }
        }
    }
}

/// `setSymdiffUpdateImpl` (objset.zig): toggle each element of
/// `other` (ADD_IF_NOT_FOUND_OR_REMOVE_IF_FOUND).
pub(crate) fn set_symdiff_update_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
) -> Result<Value, SetError> {
    let mut pending: Option<SetError> = None;
    match collect_iterable(ctx, other, None) {
        Err(e) => {
            pending = Some(SetError::from(e));
        }
        Ok(items) => {
            let r = set_ref_of(ctx.arena, self_v);
            for item in items {
                let _ = set_lookup_quirk(ctx, r, item, LookupKind::AddOrRemove, &mut pending)?;
                if pending.is_some() {
                    break;
                }
            }
        }
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `setSymdiffImpl` (objset.zig): copy + symmetric-difference-update.
pub(crate) fn set_symdiff_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
) -> Result<Value, SetError> {
    let out = clone_set(ctx, self_v)?;
    set_symdiff_update_impl(ctx, out, other)?;
    Ok(out)
}

/// `setIssubsetInternal(self, other, proper)` (objset.zig): non-set
/// args are converted via `to_set` (plain `set`); proper + equal
/// sizes → `False` without iterating; otherwise every element of
/// self must be found in other (table-order iteration). A lookup
/// hash failure raises (verified: `{[2]}.issubset({1,2})` →
/// `TypeError 'unsupported type for operator'`).
pub(crate) fn set_issubset_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
    proper: bool,
) -> Result<Value, SetError> {
    let self_c = to_set(ctx, self_v)?;
    let other_c = to_set(ctx, other)?;
    if proper && set_used(ctx.arena, self_c) == set_used(ctx.arena, other_c) {
        return Ok(Value::Bool(false));
    }
    let items = match self_c {
        Value::Heap(r) => match ctx.arena.get(r) {
            crate::objects::value::Obj::Set(s) => super::map_set::set_items(&s.table),
            _ => panic!("set_issubset_impl on a non-set value"),
        },
        _ => panic!("set_issubset_impl on a non-heap value"),
    };
    let other_r = set_ref_of(ctx.arena, other_c);
    let mut pending: Option<SetError> = None;
    for item in items {
        let found = set_lookup_quirk(ctx, other_r, item, LookupKind::Lookup, &mut pending)?;
        if pending.is_some() {
            break;
        }
        if found.is_none() {
            return Ok(Value::Bool(false));
        }
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::Bool(true)),
    }
}

/// `setEqual` (objset.zig): sizes differ → `False`, else subset —
/// no type check, so `set == frozenset` with equal elements is
/// `True` (verified).
pub(crate) fn set_equal_impl(ctx: &mut CallCtx, self_v: Value, other: Value) -> Result<Value, SetError> {
    if set_used(ctx.arena, self_v) != set_used(ctx.arena, other) {
        return Ok(Value::Bool(false));
    }
    set_issubset_impl(ctx, self_v, other, false)
}

/// `npy_set_isdisjoint` (objset.zig): iterate `other` directly
/// (no `to_set` conversion); any element found in self → `False`.
/// A lookup hash failure raises (verified: `{1}.isdisjoint([[]])`
/// → `TypeError 'unsupported type for operator'`).
pub(crate) fn set_isdisjoint_impl(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Value,
) -> Result<Value, SetError> {
    let items = match collect_iterable(ctx, other, None) {
        Err(e) => return Err(SetError::from(e)),
        Ok(v) => v,
    };
    let self_r = set_ref_of(ctx.arena, self_v);
    let mut pending: Option<SetError> = None;
    for item in items {
        let found = set_lookup_quirk(ctx, self_r, item, LookupKind::Lookup, &mut pending)?;
        if pending.is_some() {
            break;
        }
        if found.is_some() {
            return Ok(Value::Bool(false));
        }
    }
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::Bool(true)),
    }
}

// ── single methods ──────────────────────────────────────────────────

/// `setAddImpl` (objset.zig): ADD lookup; the quirk inserts an
/// unhashable element and the pending raise wins (verified:
/// `s.add([2])` raises but `s == {[2]}`).
pub(crate) fn set_add(ctx: &mut CallCtx, self_v: Value, item: Value) -> Result<Value, SetError> {
    let mut pending: Option<SetError> = None;
    set_store(ctx, self_v, item, &mut pending)?;
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `set_add` with a VM hook (G3/H5): the store's probe runs
/// through `set_store_hook`, so an instance element's FunBc
/// `__eq__` runs (`s.add(A())` on a set holding an equal A()
/// does not gain a duplicate). The quirk is unchanged.
pub(crate) fn set_add_hook(
    ctx: &mut CallCtx,
    self_v: Value,
    item: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Value, SetError> {
    let mut pending: Option<SetError> = None;
    set_store_hook(ctx, self_v, item, &mut pending, hook)?;
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `setClearImpl` (objset.zig): drop the table (`alloc = 0`).
pub(crate) fn set_clear(ctx: &mut CallCtx, self_v: Value) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    super::objset::with_set_table(ctx, r, |_, table| table.clear());
    Ok(Value::None)
}

/// `setCopyImpl` (objset.zig): see `clone_set`.
pub(crate) fn set_copy(ctx: &mut CallCtx, self_v: Value) -> Result<Value, SetError> {
    clone_set(ctx, self_v)
}

/// `npy_set_discard` (objset.zig): REMOVE lookup, result ignored;
/// the pending raise wins (verified: `{1}.discard([2])` →
/// `TypeError 'unsupported type for operator'`).
pub(crate) fn set_discard(ctx: &mut CallCtx, self_v: Value, item: Value) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    let mut pending: Option<SetError> = None;
    let _ = set_lookup_quirk(ctx, r, item, LookupKind::Remove, &mut pending)?;
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `set_discard` with a VM hook (G3/H5): the REMOVE lookup
/// runs through `set_lookup_quirk_hook`, so an instance
/// element's FunBc `__eq__` runs (`s.discard(A())` removes
/// the equal element).
pub(crate) fn set_discard_hook(
    ctx: &mut CallCtx,
    self_v: Value,
    item: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    let mut pending: Option<SetError> = None;
    let _ = set_lookup_quirk_hook(ctx, r, item, LookupKind::Remove, &mut pending, hook)?;
    match pending {
        Some(e) => Err(e),
        None => Ok(Value::None),
    }
}

/// `setPopImpl` (objset.zig): first live element in TABLE order;
/// empty → `KeyError("pop from an empty set")`.
pub(crate) fn set_pop(ctx: &mut CallCtx, self_v: Value) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    let first = super::objset::with_set_table(ctx, r, |_, table| table.remove_first());
    match first {
        Some(v) => Ok(v),
        None => Err(SetError::KeyErrorPopEmpty),
    }
}

/// `setRemoveImpl` (objset.zig): REMOVE lookup; a miss raises
/// `KeyError(item)`, which OVERWRITES the hash pending (verified:
/// `s.remove([2])` on `s == {[2]}` → `KeyError '[2]'`).
pub(crate) fn set_remove(ctx: &mut CallCtx, self_v: Value, item: Value) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    let mut pending: Option<SetError> = None;
    let found = set_lookup_quirk(ctx, r, item, LookupKind::Remove, &mut pending)?;
    match found {
        Some(_) => Ok(Value::None),
        None => Err(SetError::KeyError(item)),
    }
}

/// `set_remove` with a VM hook (G3/H5): the REMOVE lookup
/// runs through `set_lookup_quirk_hook`, so an instance
/// element's FunBc `__eq__` runs (`s.remove(A())` removes
/// the equal element; a miss still raises `KeyError`, which
/// overwrites a hash pending exactly as the slot does).
pub(crate) fn set_remove_hook(
    ctx: &mut CallCtx,
    self_v: Value,
    item: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Value, SetError> {
    let r = set_ref_of(ctx.arena, self_v);
    let mut pending: Option<SetError> = None;
    let found = set_lookup_quirk_hook(ctx, r, item, LookupKind::Remove, &mut pending, hook)?;
    match found {
        Some(_) => Ok(Value::None),
        None => Err(SetError::KeyError(item)),
    }
}

// ── `BuiltinFn` adapters ─────────────────────────────────────────────

/// `set.add` (fixed 2 args).
pub fn set_add_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_add(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.clear` (fixed 1 arg).
pub fn set_clear_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    set_clear(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `set.copy` (fixed 1 arg).
pub fn set_copy_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    set_copy(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `set.discard` (fixed 2 args).
pub fn set_discard_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_discard(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.difference` (var args, min 1: self + 0+ others).
pub fn set_difference_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, usize::MAX).map_err(|e| ctx.record_typed(e))?;
    set_diff_impl(ctx, first(args), &args[1..], false).map_err(|e| ctx.record_typed(e))
}

/// `set.difference_update` (var args, min 1) → `None`.
pub fn set_difference_update_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, usize::MAX).map_err(|e| ctx.record_typed(e))?;
    set_diff_impl(ctx, first(args), &args[1..], true).map_err(|e| ctx.record_typed(e))?;
    Ok(Value::None)
}

/// `set.intersection` (fixed 2 args).
pub fn set_intersection_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_intersect_impl(ctx, first(args), args[1], false).map_err(|e| ctx.record_typed(e))
}

/// `set.intersection_update` (fixed 2 args) → `None`.
pub fn set_intersection_update_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_intersect_impl(ctx, first(args), args[1], true).map_err(|e| ctx.record_typed(e))
}

/// `set.isdisjoint` (fixed 2 args).
pub fn set_isdisjoint_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_isdisjoint_impl(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.issubset` (fixed 2 args).
pub fn set_issubset_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_issubset_impl(ctx, first(args), args[1], false).map_err(|e| ctx.record_typed(e))
}

/// `set.issuperset` (fixed 2 args — args swapped, `setIssuperset`
/// calls `setIssubsetInternal(other, self)`).
pub fn set_issuperset_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_issubset_impl(ctx, args[1], first(args), false).map_err(|e| ctx.record_typed(e))
}

/// `set.pop` (fixed 1 arg).
pub fn set_pop_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    set_pop(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `set.remove` (fixed 2 args).
pub fn set_remove_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_remove(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.symmetric_difference` (fixed 2 args).
pub fn set_symmetric_difference_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_symdiff_impl(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.symmetric_difference_update` (fixed 2 args) → `None`.
pub fn set_symmetric_difference_update_builtin(
    ctx: &mut CallCtx,
    args: &[Value],
) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_symdiff_update_impl(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.union` (fixed 2 args).
pub fn set_union_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_union_impl(ctx, first(args), args[1]).map_err(|e| ctx.record_typed(e))
}

/// `set.update` (var args, min 1) → `None`.
pub fn set_update_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, usize::MAX).map_err(|e| ctx.record_typed(e))?;
    set_update_impl(ctx, first(args), &args[1..]).map_err(|e| ctx.record_typed(e))
}

/// `set.__contains__` (fixed 2 args) — the generic contains op
/// routes to the CONTAINS binary op (objset.zig `mp_op_contains_obj`).
pub fn set_contains_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    set_binary_op(ctx, BinaryOp::Contains, first(args), args[1])
}

// ── method tables (objset.zig locals_dicts) ─────────────────────────

/// `set` locals (objset.zig `set_locals_dict_table`): 18 entries,
/// in Zig order.
pub static SET_METHODS: &[(&str, BuiltinFn)] = &[
    ("add", set_add_builtin),
    ("clear", set_clear_builtin),
    ("copy", set_copy_builtin),
    ("discard", set_discard_builtin),
    ("difference", set_difference_builtin),
    ("difference_update", set_difference_update_builtin),
    ("intersection", set_intersection_builtin),
    ("intersection_update", set_intersection_update_builtin),
    ("isdisjoint", set_isdisjoint_builtin),
    ("issubset", set_issubset_builtin),
    ("issuperset", set_issuperset_builtin),
    ("pop", set_pop_builtin),
    ("remove", set_remove_builtin),
    ("symmetric_difference", set_symmetric_difference_builtin),
    ("symmetric_difference_update", set_symmetric_difference_update_builtin),
    ("union", set_union_builtin),
    ("update", set_update_builtin),
    ("__contains__", set_contains_builtin),
];

/// `frozenset` locals (objset.zig `frozenset_locals_dict_table`):
/// 9 entries, in Zig order — no mutating methods.
pub static FROZENSET_METHODS: &[(&str, BuiltinFn)] = &[
    ("copy", set_copy_builtin),
    ("difference", set_difference_builtin),
    ("intersection", set_intersection_builtin),
    ("isdisjoint", set_isdisjoint_builtin),
    ("issubset", set_issubset_builtin),
    ("issuperset", set_issuperset_builtin),
    ("symmetric_difference", set_symmetric_difference_builtin),
    ("union", set_union_builtin),
    ("__contains__", set_contains_builtin),
];

/// Look a method up in the `set` table (name → slot function).
pub fn lookup_set_method(name: &str) -> Option<BuiltinFn> {
    SET_METHODS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// Look a method up in the `frozenset` table.
pub fn lookup_frozenset_method(name: &str) -> Option<BuiltinFn> {
    FROZENSET_METHODS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}
