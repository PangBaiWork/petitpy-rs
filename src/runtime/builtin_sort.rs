//! VM-intercepted `sorted` / `list.sort` (`modbuiltins.zig`, D1).
//!
//! Split out of `builtin_vm.rs` (E2d file-size rule): the sort
//! paths are the only builtins that run the in-place quicksort
//! (`list_sort_builtin`) or the decorate-sort-undecorate path
//! (`sort_items_with_key`) with the VM's real call hook, so a
//! FunBc key function or `__lt__` actually executes. A `None`
//! key delegates to `list_sort_builtin` (the in-place
//! quicksort); a real key decorates once per item (the
//! original calls the key per comparison inside its quicksort,
//! and ABORTS outright when a key call or a comparison fails:
//! `sort(key=len)` on ints panics at `mp_mirrors.zig:745`;
//! Rust raises instead and leaves the list unchanged on
//! failure), stable-sorts, and writes the items back into the
//! original list in place. A comparison failure is stashed and
//! raised AFTER the sort completes (the C9b rule).
//!
//! `sorted` with a key decorates once per item (the key fn is
//! called through `call_function_ctx`, so `FunBc` keys work)
//! and sorts with a stable 3-way comparator over
//! `binary_op_ctx` — ties keep their original order (probed:
//! `reverse=True` does NOT reverse ties).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict_collect::collect_iterable;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objlist_methods::{list_sort_builtin, parse_sort_args};
use crate::objects::slots::BinaryOp;
use crate::objects::value::{Obj, Value};
use crate::runtime::builtin_vm::{kw_args, mismatch};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};
use crate::runtime::vm_rt::binary_op_ctx;

/// `mp_builtin_sorted` (`modbuiltins.zig:443-451`): more than
/// one positional arg → `TypeError("must use keyword argument
/// for key function")`; else `list(iterable)` + `list.sort`
/// with the kwargs. A non-`None` key decorates once per item
/// (see the module docs); `key=None` delegates to
/// `list_sort_builtin`.
pub fn call_sorted_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 0 {
        return Err(mismatch(ctx, state));
    }
    if n_args > 1 {
        return Err(type_error(
            ctx,
            state,
            "must use keyword argument for key function",
        ));
    }
    let types = ctx.types;
    // E2: the input iterable is materialised through the VM's
    // real call hook (the same pattern as `call_type_ctx` in
    // vm_call.rs), so `sorted(map(f, xs))` runs `f`. E2b: the
    // hook is the `VmHook` pair, so `sorted(gen())` also
    // resumes the generator. A raise from `f` / the generator
    // is stashed on `state.pending_exc` by the hook and drained
    // first below; anything else goes through the recorded
    // `pending_raise` as before. The hook lives in this block
    // only — it must be dropped before the sort below builds
    // a fresh one (both borrow `state` mutably).
    let items = {
        let mut hook = crate::runtime::vm_seams::make_hook(state);
        collect_iterable(ctx, args[0], Some(&mut hook)).map_err(|e| {
            if let Some(exc) = state.pending_exc.take() {
                return exc;
            }
            let pe = ctx.record_typed(e);
            exc_from_pyerror(ctx, state, pe)
        })?
    };
    let list = new_list(ctx, types.list, &items);
    let list_v = Value::Heap(list);
    let kws = kw_args(args, n_args, n_kw);
    let (key, reverse) = parse_sort_args(ctx, &[list_v], &kws).map_err(|e| {
        let pe = ctx.record_typed(e);
        exc_from_pyerror(ctx, state, pe)
    })?;
    match key {
        Some(k) if k != Value::None => sort_with_key(ctx, state, items, k, reverse),
        _ => {
            // E2d: the no-key path runs the in-place quicksort
            // with the real hook, so a FunBc `__lt__` executes.
            // Pin the fresh list across the sort (it is not on
            // the VM stack yet) and drain the hook's stashed
            // exception first on failure (the `HookRaised`
            // variant records nothing itself).
            let extra_base = ctx.roots.extra.len();
            ctx.roots.extra.push(list_v);
            let r = {
                let mut hook = crate::runtime::vm_seams::make_hook(state);
                list_sort_builtin(ctx, &[list_v], &kws, Some(&mut hook))
            };
            ctx.roots.extra.truncate(extra_base);
            r.map_err(|e| {
                if let Some(exc) = state.pending_exc.take() {
                    return exc;
                }
                exc_from_pyerror(ctx, state, e)
            })?;
            Ok(list_v)
        }
    }
}

/// `sorted` with a real key function, minus the result list:
/// decorate once per item, stable-sort the pairs with a 3-way
/// comparator over the dispatch loop's `binary_op_ctx`, raise
/// the FIRST comparison failure after the sort completes (the
/// C9b rule). Returns the sorted items; `sort_with_key` wraps
/// them in a fresh list, `call_list_sort_ctx` writes them back
/// into the sorted list in place.
fn sort_items_with_key(
    ctx: &mut CallCtx,
    state: &mut VmState,
    items: Vec<Value>,
    key_fn: Value,
    reverse: bool,
) -> Result<Vec<Value>, Value> {
    let mut pairs: Vec<(Value, Value)> = Vec::with_capacity(items.len());
    for item in items {
        let key = call_function_ctx(ctx, state, key_fn, 1, 0, &[item])?;
        pairs.push((key, item));
    }
    // Pin the keys + items across the sort: the comparator
    // allocates, and the pairs are only referenced here.
    let extra_base = ctx.roots.extra.len();
    for (k, item) in &pairs {
        ctx.roots.extra.push(*k);
        ctx.roots.extra.push(*item);
    }
    let mut first_err: Option<Value> = None;
    pairs.sort_by(|a, b| {
        let lt_ab = match binary_op_ctx(ctx, state, BinaryOp::Less, a.0, b.0) {
            Ok(v) => v == Value::Bool(true),
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
                false
            }
        };
        let lt_ba = if lt_ab {
            false
        } else {
            match binary_op_ctx(ctx, state, BinaryOp::Less, b.0, a.0) {
                Ok(v) => v == Value::Bool(true),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                    false
                }
            }
        };
        if reverse {
            if lt_ba {
                std::cmp::Ordering::Less
            } else if lt_ab {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        } else if lt_ab {
            std::cmp::Ordering::Less
        } else if lt_ba {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    ctx.roots.extra.truncate(extra_base);
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok(pairs.into_iter().map(|(_, item)| item).collect())
}

/// `sorted` with a real key function: decorate once per item,
/// stable-sort the pairs with a 3-way comparator over the
/// dispatch loop's `binary_op_ctx`, raise the FIRST comparison
/// failure after the sort completes (the C9b rule).
fn sort_with_key(
    ctx: &mut CallCtx,
    state: &mut VmState,
    items: Vec<Value>,
    key_fn: Value,
    reverse: bool,
) -> Result<Value, Value> {
    let types = ctx.types;
    let sorted = sort_items_with_key(ctx, state, items, key_fn, reverse)?;
    let list = new_list(ctx, types.list, &sorted);
    Ok(Value::Heap(list))
}

/// `list.sort` (E2c): `list_sort` is ctx-only and returns
/// `PyRaise` for a non-`None` key, so the VM intercepts the
/// method by GcRef identity (`VmState.list_sort_builtin`) and
/// runs the key through the real call hook here. A `None` key
/// delegates to `list_sort_builtin` (the in-place quicksort);
/// a real key decorates once per item (the `sorted` pattern —
/// the original calls the key per comparison inside its
/// quicksort, and ABORTS outright when a key call or a
/// comparison fails: `sort(key=len)` on ints panics at
/// mp_mirrors.zig:745; Rust raises instead and leaves the list
/// unchanged on failure), stable-sorts, and writes the items
/// back into the original list in place. Returns `None`.
pub fn call_list_sort_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args != 1 {
        return Err(mismatch(ctx, state));
    }
    let Some(self_v) = args.first().copied() else {
        return Err(mismatch(ctx, state));
    };
    let items: Vec<Value> = match self_v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => l.items.clone(),
            _ => return Err(mismatch(ctx, state)),
        },
        _ => return Err(mismatch(ctx, state)),
    };
    let kws = kw_args(args, n_args, n_kw);
    let (key, reverse) = parse_sort_args(ctx, &[self_v], &kws).map_err(|e| {
        let pe = ctx.record_typed(e);
        exc_from_pyerror(ctx, state, pe)
    })?;
    match key {
        Some(k) if k != Value::None => {
            let sorted = sort_items_with_key(ctx, state, items, k, reverse)?;
            if let Value::Heap(r) = self_v {
                if let Obj::List(l) = ctx.arena.get_mut(r) {
                    l.items = sorted;
                }
            }
            Ok(Value::None)
        }
        _ => {
            // E2d: the no-key path runs the in-place quicksort
            // with the real hook, so a FunBc `__lt__` executes.
            // Pin the receiver across the sort (the `__lt__`
            // calls allocate) and drain the hook's stashed
            // exception first on failure.
            let extra_base = ctx.roots.extra.len();
            ctx.roots.extra.push(self_v);
            let r = {
                let mut hook = crate::runtime::vm_seams::make_hook(state);
                list_sort_builtin(ctx, &[self_v], &kws, Some(&mut hook))
            };
            ctx.roots.extra.truncate(extra_base);
            r.map_err(|e| {
                if let Some(exc) = state.pending_exc.take() {
                    return exc;
                }
                exc_from_pyerror(ctx, state, e)
            })?;
            Ok(Value::None)
        }
    }
}
