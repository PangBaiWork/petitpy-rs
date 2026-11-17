//! Native-type operator methods (`runtime/opmethods.zig`, full
//! Zig port of vendor `py/opmethods.c`).
//!
//! Four slot-dispatch fns plus four ROM const fun-objects. The
//! dispatch fns are the `__getitem__` / `__setitem__` /
//! `__delitem__` / `__contains__` of native types such as
//! `dict`: called on an instance of a Python subclass (as in
//! `dict.__setitem__(self, k, v)`) they act on the instance's
//! native base object — the instance's own subscr would call the
//! subclass's override, which calls them again, until the stack
//! runs out (`opmethods.zig:37-43`).
//!
//! Divergence from the Zig, deliberate and documented: the Zig's
//! `callSubscr` / `callBinaryOp` (`mp_mirrors.zig:863-874`)
//! **panic** on a null slot. Rust never panics there: a missing
//! slot records the tree's generic error instead —
//! `ObjError::NotSubscriptable` ("object isn't subscriptable")
//! for the subscr ops, `binop_unsupported` (C14's
//! `runtime_ops.rs:150`) for `__contains__` — the same generics
//! the VM's own dispatch raises. When the slot itself recorded
//! a raise, that raise propagates untouched (the
//! `obj.zig:604-605` rule: a slot failure WITH a pending
//! exception propagates THAT error).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    call_binary, call_subscr, obj_get_type, obj_is_true, ObjError, SlotOutcome, FLAG_INSTANCE_TYPE,
};
use crate::objects::objtype::instance_create::count_native_bases;
use crate::objects::slots::{BinaryOp, SubscrArg};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::argcheck::{arg_check_num_sig, make_sig};
use crate::runtime::runtime_ops::binop_unsupported;

/// `nativeBase` (`opmethods.zig:27-34`): the native base object
/// of an instance of a Python subclass of a native type (the
/// `dict` inside an instance of `class E(dict)`); any other
/// object is returned as it is. The Zig returns
/// `subobj[0]` as-is — including the `native_base_init_wrapper`
/// sentinel before `__init__` runs — and this does the same.
/// An instance whose `subobj` is empty (cannot happen through
/// `new_instance`, which primes it) falls back to the value
/// itself rather than indexing out of bounds.
pub fn native_base(ctx: &mut CallCtx, value: Value) -> Value {
    let Value::Heap(r) = value else {
        return value;
    };
    let Obj::Instance(inst) = ctx.arena.get(r) else {
        return value;
    };
    let Some(type_) = obj_get_type(ctx.arena, ctx.types, value) else {
        return value;
    };
    // The Zig checks the flag first (`!t.flagInstanceType() →
    // return self_in`); `count_native_bases` alone would report
    // 1 for a native type itself, so the flag check is load
    // bearing, not redundant.
    let is_instance_type = matches!(ctx.arena.get(type_), Obj::Type(d) if d.flags & FLAG_INSTANCE_TYPE != 0);
    if !is_instance_type {
        return value;
    }
    let (count, _) = count_native_bases(ctx, type_);
    if count == 0 {
        return value;
    }
    inst.subobj.first().copied().unwrap_or(value)
}

/// The generic error for a missing subscr slot: the slot
/// recorded nothing, so record "object isn't subscriptable"
/// (`ObjError::NotSubscriptable`). When the slot DID record a
/// raise, that raise propagates instead (first record wins).
fn not_subscriptable(ctx: &mut CallCtx) -> PyError {
    if ctx.has_pending_raise() {
        return PyError::PyRaise;
    }
    ctx.record_typed(ObjError::NotSubscriptable)
}

fn slot_error(ctx: &mut CallCtx, e: ObjError) -> PyError {
    match e {
        ObjError::OutOfMemory => PyError::OutOfMemory,
        e => ctx.record_typed(e),
    }
}

/// `npy_op_fn_getitem` (`opmethods.zig:45-47`): the native
/// base's subscr slot with the SENTINEL (`SubscrArg::Get`).
pub fn getitem_op(ctx: &mut CallCtx, self_: Value, index: Value) -> PyResult<Value> {
    let base = native_base(ctx, self_);
    let type_ = obj_get_type(ctx.arena, ctx.types, base);
    match call_subscr(ctx, type_, base, index, SubscrArg::Get) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => Err(not_subscriptable(ctx)),
        Err(e) => Err(slot_error(ctx, e)),
    }
}

/// `npy_op_fn_setitem` (`opmethods.zig:48-50`): the native
/// base's subscr slot with the value (`SubscrArg::Set`).
pub fn setitem_op(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    value: Value,
) -> PyResult<Value> {
    let base = native_base(ctx, self_);
    let type_ = obj_get_type(ctx.arena, ctx.types, base);
    match call_subscr(ctx, type_, base, index, SubscrArg::Set(value)) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => Err(not_subscriptable(ctx)),
        Err(e) => Err(slot_error(ctx, e)),
    }
}

/// `npy_op_fn_delitem` (`opmethods.zig:51-53`): the native
/// base's subscr slot with NULL (`SubscrArg::Delete`).
pub fn delitem_op(ctx: &mut CallCtx, self_: Value, index: Value) -> PyResult<Value> {
    let base = native_base(ctx, self_);
    let type_ = obj_get_type(ctx.arena, ctx.types, base);
    match call_subscr(ctx, type_, base, index, SubscrArg::Delete) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => Err(not_subscriptable(ctx)),
        Err(e) => Err(slot_error(ctx, e)),
    }
}

/// `npy_op_fn_contains` (`opmethods.zig:54-57`): the type's
/// binary slot with `CONTAINS` — **without** `native_base`
/// (the Zig does not unwrap here). The slot result is
/// converted with `mp_obj_is_true`, as the VM's
/// `contains_op_ctx` does for its own slot call. A missing
/// slot records C14's `binop_unsupported` TypeError.
pub fn contains_op(ctx: &mut CallCtx, self_: Value, item: Value) -> PyResult<bool> {
    let type_ = obj_get_type(ctx.arena, ctx.types, self_);
    match call_binary(ctx, type_, BinaryOp::Contains, self_, item) {
        Ok(SlotOutcome::Value(v)) => obj_is_true(ctx, ctx.types, v),
        Ok(SlotOutcome::NotImplemented) => {
            if ctx.has_pending_raise() {
                return Err(PyError::PyRaise);
            }
            // `RtError` has no `TypedRaise` impl (its
            // `to_pyerror` drops the name/message by design, the
            // C6/C7r slot-boundary convention); record the
            // name/message directly instead.
            let e = binop_unsupported(ctx, BinaryOp::Contains, self_, item);
            let msg = e.message();
            ctx.record_raise(e.exc_name(), Some(&msg));
            Err(PyError::PyRaise)
        }
        Err(e) => Err(slot_error(ctx, e)),
    }
}

// ── `BuiltinFn` adapters for the dict locals dict ────────────────────

/// `mp_op_getitem_obj` (`opmethods.zig:61-64`): a 2-arg
/// `fun_builtin_2` (`__getitem__(self, key)`). Wired into the
/// dict locals dict by `DICT_METHODS` (`objdict.zig:768,785`).
/// The adapter checks arity with the NEW canonical
/// `arg_check_num_sig` (real use of the canonical module in
/// new code), then dispatches through `getitem_op`.
pub fn dict_getitem_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arg_check_num_sig(ctx, args.len(), 0, make_sig(2, 2, false))?;
    getitem_op(ctx, args[0], args[1])
}

/// `mp_op_setitem_obj` (`opmethods.zig:66-69`): a 3-arg
/// `fun_builtin_3` (`__setitem__(self, key, value)`).
pub fn dict_setitem_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arg_check_num_sig(ctx, args.len(), 0, make_sig(3, 3, false))?;
    setitem_op(ctx, args[0], args[1], args[2])
}

/// `mp_op_delitem_obj` (`opmethods.zig:70-73`): a 2-arg
/// `fun_builtin_2` (`__delitem__(self, key)`).
pub fn dict_delitem_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arg_check_num_sig(ctx, args.len(), 0, make_sig(2, 2, false))?;
    delitem_op(ctx, args[0], args[1])
}

/// `mp_op_contains_obj` (`opmethods.zig:74-77`): a 2-arg
/// `fun_builtin_2` (`__contains__(self, item)`). **Unreferenced
/// in the original** — no locals dict or method table points at
/// it (grep: only the definition and the comptime keep-alive at
/// `:84-87`) — so it is provided here as the constructor's
/// adapter but is NOT wired into `DICT_METHODS`, exactly as the
/// Zig leaves it unreferenced. (The dict's `in` operator goes
/// through the type's binary slot, not through a method.)
pub fn dict_contains_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arg_check_num_sig(ctx, args.len(), 0, make_sig(2, 2, false))?;
    let found = contains_op(ctx, args[0], args[1])?;
    Ok(Value::Bool(found))
}
