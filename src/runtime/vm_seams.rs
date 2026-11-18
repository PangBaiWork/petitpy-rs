//! VM-seam helpers for the instance dunder paths (E2, Fix C).
//!
//! The instance slot fns (`instance_binary_op` /
//! `instance_unary_op` / `instance_subscr` / `instance_call` /
//! `instance_load_attr`, `objtype/instance.rs`) already take a
//! `&mut VmSeams` — but the slot adapters the dispatchers call
//! (`instance_*_slot`) build theirs with `with_stub_seams`, so a
//! `FunBc` dunder raises the bare `TypeError` there. The VM's
//! opcode dispatchers HAVE `state`, so they try the instance
//! path here first, with the VM's real call hook (Fix A —
//! `call_function_ctx` + the `state.pending_exc` side channel),
//! and fall through to the slot path when there is no such
//! dunder (`Ok(None)`).
//!
//! Contract (shared by every helper): `Ok(Some(v))` = the
//! dunder ran and produced `v`; `Ok(None)` = no such dunder
//! (fall through to the existing slot dispatch — the stub seams
//! there return the same `None`); `Err(exc)` = the exception
//! OBJECT, drained from `state.pending_exc` first (a raise from
//! the hooked call) else from the recorded `pending_raise`
//! (a raise from a builtin slot).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::instance::{
    instance_binary_op, instance_call, instance_get_call, instance_getiter, instance_load_attr,
    instance_subscr, instance_unary_op,
};
use crate::objects::objtype::lookup::{binary_op_method_name, class_lookup, LookupHit, SlotKind};
use crate::objects::objtype::seams::{with_call_hook_seams, with_hook_load_seams, with_stub_seams};
use crate::objects::objtype::subclass::instance_subobj0;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, SubscrArg, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_ops::exc_from_pyerror;

/// Is `v` a user-class instance?
pub(crate) fn is_instance(arena: &GcArena, v: Value) -> bool {
    matches!(v, Value::Heap(r) if matches!(arena.get(r), Obj::Instance(_)))
}

/// Drain a hook raise: `state.pending_exc` first (a raise from
/// the hooked `FunBc` call), else the recorded `pending_raise`
/// via `exc_from_pyerror` (a raise from a builtin slot).
pub(crate) fn drain_hook_raise(
    ctx: &mut CallCtx,
    state: &mut VmState,
    e: PyError,
) -> Value {
    if let Some(exc) = state.pending_exc.take() {
        exc
    } else {
        exc_from_pyerror(ctx, state, e)
    }
}

/// The VM's user-code hook (E2b): ONE borrow of `state` serving
/// both `VmHook` operations, so a collection site can call user
/// code AND resume a generator without two competing `&mut
/// VmState` borrows (E0499 — the two-closure shape cannot be
/// built). `call` runs a `FunBc` callee through
/// `call_function_ctx` + `execute_frame`; `resume` steps a
/// generator once via `resume_generator_ctx` in the `iternext`
/// form (`Ok(Some(v))` = yielded, `Ok(None)` = exhausted). A
/// raise from either operation stashes the exception object on
/// `state.pending_exc` and returns `PyRaise` (the C14 contract);
/// the caller drains it (`drain_hook_raise` /
/// `call_type_ctx`'s drain / `sorted`'s drain).
pub(crate) struct StateHook<'s> {
    state: &'s mut VmState,
}

impl crate::objects::slots::VmHook for StateHook<'_> {
    fn call(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        n: usize,
        k: usize,
        a: &[Value],
    ) -> PyResult<Value> {
        match call_function_ctx(ctx, self.state, fun, n, k, a) {
            Ok(v) => Ok(v),
            Err(exc) => {
                self.state.pending_exc = Some(exc);
                Err(PyError::PyRaise)
            }
        }
    }

    fn resume(&mut self, ctx: &mut CallCtx, gen: Value) -> Result<Option<Value>, PyError> {
        match crate::runtime::vm_gen::resume_generator_ctx(
            ctx,
            self.state,
            gen,
            Value::None,
            None,
            false,
        ) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.state.pending_exc =
                    Some(crate::runtime::vm_ops::exc_from_gen_error(ctx, self.state, e));
                Err(PyError::PyRaise)
            }
        }
    }

    fn binary_op(
        &mut self,
        ctx: &mut CallCtx,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    ) -> PyResult<Value> {
        match crate::runtime::vm_rt::binary_op_ctx(ctx, self.state, op, lhs, rhs) {
            Ok(v) => Ok(v),
            Err(exc) => {
                // First failure wins (the sort's run-to-completion
                // rule): a later comparison failure must not
                // overwrite the exception the caller will drain.
                if self.state.pending_exc.is_none() {
                    self.state.pending_exc = Some(exc);
                }
                Err(PyError::PyRaise)
            }
        }
    }

    /// E2f: the full VM step — `iternext_vm_ctx` (generators,
    /// C11 iterators, streams, user `__next__`) with exhaustion
    /// as `Ok(None)` (its `is_stop_iteration` check is
    /// subclass-aware, so a user `StopIteration` subclass ends
    /// iteration too). A raise is stashed on `pending_exc`
    /// (the `StateHook` contract — the caller drains it).
    fn next(&mut self, ctx: &mut CallCtx, it: Value) -> Result<Option<Value>, PyError> {
        match crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, self.state, it) {
            Ok(v) => Ok(v),
            Err(exc) => {
                self.state.pending_exc = Some(exc);
                Err(PyError::PyRaise)
            }
        }
    }


    /// E2h: take (and clear) the stashed raise — see the
    /// `VmHook::take_pending` contract.
    fn take_pending(&mut self) -> Option<Value> {
        self.state.pending_exc.take()
    }


    /// E2i: the getitem-iterator exhaustion check — take the
    /// stashed exception iff its EXACT type is `StopIteration`
    /// or `IndexError` (`objgetitemiter.zig:excIsStopOrIndex`).
    /// A subclass does NOT count (the vendor compares type
    /// pointers), so this matches on `type_` directly, not on
    /// `is_exception_instance` (which is subclass-aware).
    fn take_pending_if_stop_or_index(&mut self, ctx: &mut CallCtx) -> bool {
        let Some(exc) = self.state.pending_exc.as_ref() else {
            return false;
        };
        let is_stop_or_index = match exc {
            Value::Heap(r) => match ctx.arena.get(*r) {
                crate::objects::value::Obj::Exception(d) => {
                    d.type_ == self.state.exc.stop_iteration
                        || d.type_ == self.state.exc.index_error
                }
                _ => false,
            },
            _ => false,
        };
        if is_stop_or_index {
            self.state.pending_exc.take();
            true
        } else {
            false
        }    }
}

/// Build the VM's hook from `state` (borrows `state` for `'s`).
pub(crate) fn make_hook<'s>(state: &'s mut VmState) -> StateHook<'s> {
    StateHook { state }
}

/// Try an instance's binary-op dunder (`__add__`, `__eq__`,
/// `__contains__`, …) with the VM's real call hook.
pub(crate) fn try_instance_binary_op(
    ctx: &mut CallCtx,
    state: &mut VmState,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> Result<Option<Value>, Value> {
    if !is_instance(ctx.arena, lhs) {
        return Ok(None);
    }
    // F3 — SENTINEL pre-check: when the class lookup for the op
    // name hits the NATIVE slot (the type is a subclass of a
    // native type that defines no such dunder), the vendor's
    // `instance_binary_op` SENTINEL arm calls the FULL
    // `mp_binary_op` on `(subobj[0], rhs)`
    // (`objtype.zig:1167-1168`) — NOT the slot-level seam. The
    // slot-level seam loses the full protocol: the reverse step
    // never unwraps the rhs, so `L([1,2]) + L([3])` raised
    // `('L', 'L')` where the reference raises `('list', 'list')`
    // (the vendor's inner `mp_binary_op` reverse-swaps and
    // unwraps the rhs too), and `S('ab') < S('ac')` raised
    // instead of returning True. E2j did this for EQUAL /
    // NOT_EQUAL inside `instance_binary_op` (the 2-pass
    // protocol); the other ops need the full `binary_op_ctx`
    // here, which the slot fn cannot reach (its seams' `binary`
    // is always the slot-level `call_binary`). A Member hit or
    // no hit keeps the existing path below (the dunder runs, or
    // there is none). EQUAL / NOT_EQUAL / CONTAINS are excluded:
    // equality is E2j's protocol, and CONTAINS bool-ifies in
    // `finish_binary` on the member path. The pre-lookup is pure
    // (a dict walk on stub seams, no user calls — the
    // `try_instance_getiter` precedent).
    if !matches!(
        op,
        BinaryOp::Equal | BinaryOp::NotEqual | BinaryOp::Contains
    ) {
        if let Some(name_q) = binary_op_method_name(op) {
            let is_native_slot = with_stub_seams(|seams| {
                let Some(type_) = obj_get_type(ctx.arena, ctx.types, lhs) else {
                    return false;
                };
                matches!(
                    class_lookup(
                        ctx,
                        Some(lhs),
                        name_q,
                        Some(SlotKind::BinaryOp),
                        false,
                        type_,
                        seams
                    ),
                    Some(LookupHit::NativeSlot(_))
                )
            });
            if is_native_slot {
                if let Some(sub0) = instance_subobj0(ctx.arena, lhs) {
                    return crate::runtime::vm_rt::binary_op_ctx(ctx, state, op, sub0, rhs)
                        .map(Some);
                }
            }
        }
    }
    let mut hook = make_hook(state);
    match with_call_hook_seams(&mut hook, |seams| instance_binary_op(ctx, op, lhs, rhs, seams)) {
        Ok(v) => Ok(v),
        Err(e) => Err(drain_hook_raise(ctx, state, e)),
    }
}

/// Try an instance's unary-op dunder (`__neg__`, `__abs__`,
/// `__invert__`, `__pos__`, …) with the VM's real call hook.
pub(crate) fn try_instance_unary_op(
    ctx: &mut CallCtx,
    state: &mut VmState,
    op: UnaryOp,
    arg: Value,
) -> Result<Option<Value>, Value> {
    if !is_instance(ctx.arena, arg) {
        return Ok(None);
    }
    let mut hook = make_hook(state);
    match with_call_hook_seams(&mut hook, |seams| instance_unary_op(ctx, op, arg, seams)) {
        Ok(v) => Ok(v),
        Err(e) => Err(drain_hook_raise(ctx, state, e)),
    }
}

/// Try an instance's truth value with the VM's real call hook:
/// the `__bool__` slot first (the result is true iff it IS the
/// `True` singleton — probed on the reference: `__bool__`
/// returning `1` / `"x"` / `0` is all `False`), else the
/// `__len__` slot (`len != 0`, the `obj_is_true` identity rule),
/// else `None` (fall through to `obj_is_true`'s default).
pub(crate) fn try_instance_bool(
    ctx: &mut CallCtx,
    state: &mut VmState,
    arg: Value,
) -> Result<Option<bool>, Value> {
    if !is_instance(ctx.arena, arg) {
        return Ok(None);
    }
    match try_instance_unary_op(ctx, state, UnaryOp::Bool, arg)? {
        Some(v) => return Ok(Some(v == Value::Bool(true))),
        None => {}
    }
    match try_instance_unary_op(ctx, state, UnaryOp::Len, arg)? {
        Some(l) => Ok(Some(l != Value::SmallInt(0))),
        None => Ok(None),
    }
}

/// `typingCall` (`runtime.zig:537-543`): `typing.<name>(a, b)` —
/// the run-time forms of `list[str]` and `int | None` (F5a).
/// Imports the frozen `typing` module through the VM's real
/// import, loads `<name>` off it, and calls it with two args
/// through the VM's real call — so the callee is the frozen
/// Python (`_class_getitem` / `_union`), not a Rust re-invention.
pub(crate) fn typing_call_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    name: u32,
    a: Value,
    b: Value,
) -> Result<Value, Value> {
    let typing_q =
        crate::runtime::runtime::dunder(b"typing").expect("typing is a static qstr");
    let module = crate::runtime::vm_rt_iter::import_name_ctx(
        ctx,
        state,
        typing_q,
        Value::None,
        Value::SmallInt(0),
    )?;
    let fun = crate::runtime::vm_rt_name::load_attr_ctx(ctx, state, module, name)?;
    crate::runtime::vm_call::call_function_ctx(ctx, state, fun, 2, 0, &[a, b])
}

/// Try an instance's subscr dunder (`__getitem__` /
/// `__setitem__` / `__delitem__`) with the VM's real call hook.
///
/// F5a: the `list[str]` / `__class_getitem__` branch
/// (`obj.zig:600-614`) also lives here. The LOAD_SUBSCR call
/// site is in `vm_dispatch.rs` (the F3 lane's file — this lane
/// must not edit it), and the branch needs the VM's real
/// import+call, which the slot path (`obj_subscr`, a pure
/// `CallCtx` function) cannot do — so the VM-hook path runs it:
/// Get-only, base exactly a type (header type is `type`), and
/// the base's type (its metaclass) has NO subscr slot (the
/// vendor tries the slot first; a metaclass `__getitem__`
/// falls through to `obj_subscr` below). A type with a custom
/// metaclass and no slot does NOT take the branch either (the
/// vendor's `typeOf(base) == &mp_type_type` exact check).
pub(crate) fn try_instance_subscr(
    ctx: &mut CallCtx,
    state: &mut VmState,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> Result<Option<Value>, Value> {
    if matches!(arg, SubscrArg::Get) {
        if let Value::Heap(r) = base {
            if ctx.arena.header(r).type_ == Some(ctx.types.type_) {
                let meta = obj_get_type(ctx.arena, ctx.types, base);
                let has_slot = match meta {
                    Some(t) => matches!(ctx.arena.get(t), Obj::Type(d) if d.subscr.is_some()),
                    None => false,
                };
                if !has_slot {
                    let name = crate::runtime::runtime::dunder(b"_class_getitem")
                        .expect("_class_getitem is a static qstr");
                    return typing_call_ctx(ctx, state, name, base, index).map(Some);
                }
            }
        }
    }
    if !is_instance(ctx.arena, base) {
        return Ok(None);
    }
    let mut hook = make_hook(state);
    match with_call_hook_seams(&mut hook, |seams| instance_subscr(ctx, base, index, arg, seams)) {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(drain_hook_raise(ctx, state, e)),
    }
}

/// Try an instance's `__call__` with the VM's real call hook.
/// The `__call__` lookup runs first with stub seams (a pure
/// dict lookup — no call), so a class without `__call__`
/// falls through without running anything.
pub(crate) fn try_instance_call(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Option<Value>, Value> {
    if !is_instance(ctx.arena, fun) {
        return Ok(None);
    }
    let has_call = with_stub_seams(|seams| instance_get_call(ctx, fun, seams).is_some());
    if !has_call {
        return Ok(None);
    }
    let mut hook = make_hook(state);
    match with_call_hook_seams(&mut hook, |seams| instance_call(ctx, fun, n_args, n_kw, args, seams))
    {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(drain_hook_raise(ctx, state, e)),
    }
}

/// Try an instance's `__iter__` with the VM's real call hook.
/// The lookup runs first with stub seams (a pure dict lookup —
/// no call), so a class without `__iter__` falls through to
/// the slot path (which tries the getitem fallback). Without
/// this, the slot path's `instance_getiter_slot` calls the
/// `FunBc` dunder through stub seams — `bound_meth_call_slot`
/// has no runner for `FunBc` — and `getiter` silently falls
/// back to a getitem iterator (`for x in MyGen()` →
/// "object isn't subscriptable"; `yield from Iter()` ditto).
pub(crate) fn try_instance_getiter(
    ctx: &mut CallCtx,
    state: &mut VmState,
    value: Value,
) -> Result<Option<Value>, Value> {
    if !is_instance(ctx.arena, value) {
        return Ok(None);
    }
    let has_iter = with_stub_seams(|seams| {
        let Some(type_) = obj_get_type(ctx.arena, ctx.types, value) else {
            return false;
        };
        let Some(q) = static_qstr_id(b"__iter__") else {
            return false;
        };
        class_lookup(ctx, Some(value), q, Some(SlotKind::Iter), false, type_, seams).is_some()
    });
    if !has_iter {
        return Ok(None);
    }
    let mut hook = make_hook(state);
    match with_call_hook_seams(&mut hook, |seams| instance_getiter(ctx, value, seams)) {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(drain_hook_raise(ctx, state, e)),
    }
}

/// Try an instance's attribute load with the VM's real call
/// hook: the members map, the class lookup, a `property` fget,
/// a descriptor `__get__`, and the `__getattr__` fallback
/// (`instance_load_attr`). `Ok(None)` = no such attribute.
pub(crate) fn try_instance_load_attr(
    ctx: &mut CallCtx,
    state: &mut VmState,
    obj: Value,
    attr: u32,
) -> Result<Option<Value>, Value> {
    if !is_instance(ctx.arena, obj) {
        return Ok(None);
    }
    let mut hook = make_hook(state);
    // E2f: hook-LOAD seams, not the plain hook seams — the
    // descriptor `__get__` step inside `instance_load_attr`
    // probes methods via `load_maybe`, and the plain hook
    // seams' loader is a None stub (so `a.Forward` returned
    // the descriptor itself instead of calling `__get__`).
    let loaded = match with_hook_load_seams(&mut hook, |seams| instance_load_attr(ctx, obj, attr, seams))
    {
        Ok(v) => v,
        Err(e) => return Err(drain_hook_raise(ctx, state, e)),
    };
    if let Some(v) = loaded {
        return Ok(Some(v));
    }
    // The `__getattr__` fallback (E2): with the old plain hook
    // seams this never fired inside `instance_load_attr`
    // (their `load_maybe` was a None stub — it cannot carry
    // `state`, a second `&mut VmState` closure next to the
    // call hook would be E0499), so it was done here, where
    // state is live. With hook-load seams the fallback now
    // fires inside `instance_load_attr` itself; this stays as
    // the safety net for the paths that return None without
    // firing it (e.g. the descriptor-dunder guard below) — the
    // same guards as `load_attr_getattr` (`instance.rs`):
    // never for `__getattr__` itself, never for the
    // descriptor-protocol dunders. The lookup binds in the
    // class dict (via `load_method_maybe_ctx`, whose own
    // instance try finds it there); the call passes the
    // attribute NAME as a qstr, with the bound self prepended
    // when the lookup returned one.
    let getattr_q = static_qstr_id(b"__getattr__").unwrap_or(u32::MAX);
    if attr == getattr_q {
        return Ok(None);
    }
    for name in [
        b"__get__".as_slice(),
        b"__set__",
        b"__delete__",
        b"__set_name__",
        b"__setattr__",
        b"__delattr__",
    ] {
        if Some(attr) == static_qstr_id(name) {
            return Ok(None);
        }
    }
    match crate::runtime::vm_rt_name::load_method_maybe_ctx(ctx, state, obj, getattr_q) {
        Ok((Some(fun), bound)) => match bound {
            Some(s) => {
                call_function_ctx(ctx, state, fun, 2, 0, &[s, Value::Qstr(attr)]).map(Some)
            }
            None => {
                call_function_ctx(ctx, state, fun, 1, 0, &[Value::Qstr(attr)]).map(Some)
            }
        },
        Ok((None, _)) => Ok(None),
        Err(exc) => Err(exc),
    }
}
