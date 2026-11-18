//! Instance dispatch (`mp_obj_instance_*`, objtype.zig:1050-1500).
//!
//! The per-instance entry points the type system routes through:
//! getiter / unary / binary / subscr / get_call / get_buffer /
//! load_attr / store_attr / attr / call / print. Each logic
//! function takes `&mut VmSeams` (the runtime dispatch of C13/C14
//! — see `seams.rs`); each `*_slot` adapter builds the stub seams
//! and is what lands in a `TypeData`.
//!
//! Two shapes to keep straight (the Zig conflates them in the
//! `dest` buffer; Rust splits them):
//!
//! * The **logic** functions return the single value the caller
//!   needs. `instance_load_attr` returns `PyResult<Option<Value>>`
//!   — a class-lookup member that binds becomes a `BoundMeth`
//!   object (allocated), an unbound one is returned as-is, a
//!   members-map hit is returned as-is. C14's `mp_load_method`
//!   decomposes a `BoundMeth` result back into its (fun, self)
//!   pair when it needs one.
//! * The **store** path is a free function, not a slot: the frozen
//!   `AttrFn` is load-only (`slots.rs`), so `instance_store_attr`
//!   (`PyResult<bool>`: `Ok(true)` = handled, `Ok(false)` = not
//!   handled) is what C14's `mp_store_attr` calls. The Zig's
//!   `mp_obj_instance_attr` (dest[0] == NULL → load, else store)
//!   has no Rust slot spelling — C14 routes loads through the
//!   `attr` slot and stores through this free function.
//!
//! Fidelity notes (all from the Zig, line refs in the code):
//!
//! * `instance_unary_op`'s HASH fallback re-looks `__eq__` up
//!   (objtype.zig:1148-1151) and its default hash is the identity
//!   word (`obj_id`, the arena index).
//! * `instance_binary_op` looks ONLY in the class dictionary
//!   (objtype.zig:1152 comment — "For ducktyping, CPython does not
//!   look in the instance members or use `__getattr__`").
//!   A member that returns `NotImplemented` cannot fall back to
//!   the reflected op: the `Value` model has no NotImplemented
//!   value to compare against, so the member's result is returned
//!   as-is. C13's Value-model decision must close this (the VM's
//!   own binary-op dispatch needs the same sentinel).
//! * `instance_subscr`'s `__missing__` protocol (ADR 010 §C1) is
//!   driven by `SubscrError::KeyError` from the seam — the stub
//!   seam never produces it (C6's `call_subscr` drops the raise
//!   data), so the protocol is testable only with a seam that
//!   reports `KeyError` explicitly; C14's real `mp_obj_subscr`
//!   must.
//! * `instance_print`'s exception-subclass branch defers to
//!   C12b's `mp_obj_is_native_exception_instance`
//!   (objexcept.zig:100): until the rebase wires it in, the
//!   SENTINEL path prints the native base directly. Narrow:
//!   only user-defined exception subclasses' `repr`.
//! * `instance_print` falls through to the `<%s object at %p>`
//!   fallback when the looked-up `__str__`/`__repr__` call raises
//!   (documented choice — the Zig's print path has no raise
//!   channel; a `PrintFn` returns `()`).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::obj::{
    buffer_slot, obj_get_int_truncated, obj_get_type, obj_id, obj_is_true, type_data,
};
use crate::objects::primitives::ops::{is_exact_int, static_qstr_id};
use crate::objects::slots::{
    AttrFn, BinaryOp, BufferFn, BufferInfo, GetIterFn, PrintFn, SubscrArg, UnaryOp,
};
use crate::objects::value::{wrap_small_int, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};

use super::errors::ObjTypeError;
use super::lookup::{class_lookup, instance_dict_readonly, LookupHit, SlotKind};
pub use super::instance_print::{instance_print, instance_print_slot};
use super::seams::{with_stub_seams, SubscrError, VmSeams};
use super::subclass::instance_subobj0;
use crate::objects::containers::objproperty::property_proxy;
use crate::objects::obj::{FLAG_HAS_SPECIAL_ACCESSORS, FLAG_ITER_IS_ITERNEXT};

/// A dunder name's static qstr id (`None` = not a static qstr —
/// the attribute cannot exist, so the lookup misses).
fn dunder(name: &[u8]) -> Option<u32> {
    static_qstr_id(name)
}

/// The instance's type (`None` = not a heap instance).
fn instance_type_of(ctx: &CallCtx, self_in: Value) -> Option<GcRef> {
    obj_get_type(ctx.arena, ctx.types, self_in)
}

/// Turn a `class_lookup` dest pair into the single value the
/// `AttrFn` returns: a bound member becomes a `BoundMeth` object
/// (allocated), an unbound one is returned as-is.
pub(crate) fn bind_pair(ctx: &mut CallCtx, fun: Value, bound: Option<Value>) -> Value {
    match bound {
        Some(self_) => Value::Heap(new_bound_meth(ctx, ctx.types.boundmeth, fun, self_)),
        None => fun,
    }
}

/// Is `member` a `property` object (header type == the property
/// type)? `memberIsProperty` (objtype.zig:349-adjacent).
fn is_property(ctx: &CallCtx, member: Value) -> bool {
    match member {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Property(_) => true,
            _ => false,
        },
        _ => false,
    }
}

// ── getiter ─────────────────────────────────────────────────────────

/// `mp_obj_instance_getiter` (objtype.zig:1050-1147): look
/// `__iter__` up with SLOT_OFFSET_ITER. NULL → NULL; SENTINEL →
/// the native base's iterator (ITER_IS_ITERNEXT returns subobj0
/// directly, else the native type's iter slot on subobj0); member
/// → call the bound method with 0 args.
pub fn instance_getiter(
    ctx: &mut CallCtx,
    self_in: Value,
    seams: &mut VmSeams,
) -> PyResult<Value> {
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Err(PyError::PyRaise);
    };
    let Some(iter_q) = dunder(b"__iter__") else {
        return Err(PyError::PyRaise);
    };
    match class_lookup(ctx, Some(self_in), iter_q, Some(SlotKind::Iter), false, type_, seams) {
        None => Err(PyError::PyRaise),
        Some(LookupHit::NativeSlot(_)) => {
            let Some(sub0) = instance_subobj0(ctx.arena, self_in) else {
                return Err(PyError::PyRaise);
            };
            let Some(sub_type) = obj_get_type(ctx.arena, ctx.types, sub0) else {
                return Err(PyError::PyRaise);
            };
            let Some(sub_data) = type_data(ctx.arena, sub_type) else {
                return Err(PyError::PyRaise);
            };
            if sub_data.flags & FLAG_ITER_IS_ITERNEXT != 0 {
                Ok(sub0)
            } else {
                let Some(getiter) = sub_data.getiter else {
                    return Err(PyError::PyRaise);
                };
                getiter(ctx, sub0)
            }
        }
        Some(LookupHit::Member(fun, bound)) => seams.call_pair(ctx, fun, bound, 0, 0, &[]),
    }
}

/// `mp_obj_instance_getiter` as a `GetIterFn` slot (stub seams).
pub fn instance_getiter_slot(ctx: &mut CallCtx, self_in: Value) -> PyResult<Value> {
    with_stub_seams(|seams| instance_getiter(ctx, self_in, seams))
}

// ── unary ───────────────────────────────────────────────────────────

/// `mp_obj_instance_unary_op` (objtype.zig:1148-1151): look the
/// op method up with SLOT_OFFSET_UNARY_OP. SENTINEL → the runtime
/// unary op on subobj0; member → call the UNBOUND fun with self
/// as its arg; HASH → truncate to a small int
/// (`obj_get_int_truncated`); INT_MAYBE → must be an int (C7's
/// `is_exact_int` — small ints count) else bare TypeError; NULL
/// with op == HASH → re-look `__eq__` up; both miss → the
/// identity hash (`obj_id`).
pub fn instance_unary_op(
    ctx: &mut CallCtx,
    op: UnaryOp,
    self_in: Value,
    seams: &mut VmSeams,
) -> PyResult<Option<Value>> {
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Ok(None);
    };
    let Some(name_q) = super::lookup::unary_op_method_name(op) else {
        return Ok(None);
    };
    let hit = class_lookup(ctx, Some(self_in), name_q, Some(SlotKind::UnaryOp), false, type_, seams);
    match hit {
        Some(LookupHit::NativeSlot(_)) => {
            let Some(sub0) = instance_subobj0(ctx.arena, self_in) else {
                return Ok(None);
            };
            // The runtime op's result is returned directly (the
            // HASH/INT_MAYBE finishing below applies only to the
            // member path — objtype.zig:1112-1123).
            return seams.unary(ctx, op, sub0);
        }
        Some(LookupHit::Member(fun, _bound)) => {
            // The Zig calls the UNBOUND fun with self as its arg
            // (`mp_call_function_1(member[0], self_in)`) — the
            // bound pair's `self` is the same value either way,
            // so call the fun directly with [self_in].
            let ret = seams.call(ctx, fun, 1, 0, &[self_in])?;
            return finish_unary(ctx, op, ret);
        }
        None => {}
    }
    // NULL: only HASH has a fallback (re-look `__eq__` up —
    // objtype.zig:1124-1143). `__eq__` missing too → the identity
    // hash; `__eq__` present without `__hash__` → unhashable
    // (the CPython rule: defining `__eq__` clears `__hash__`).
    if op == UnaryOp::Hash {
        let Some(eq_q) = dunder(b"__eq__") else {
            return Ok(Some(obj_id(self_in)));
        };
        let hit = class_lookup(ctx, Some(self_in), eq_q, Some(SlotKind::UnaryOp), false, type_, seams);
        if hit.is_none() {
            return Ok(Some(obj_id(self_in)));
        }
        return Ok(None);
    }
    Ok(None)
}

/// Finish a member-path unary result: HASH truncates
/// (`newSmallInt` — the tag-wrap, no window check, exactly like
/// C7's int HASH arm), INT_MAYBE must be an int, everything else
/// passes through.
fn finish_unary(ctx: &mut CallCtx, op: UnaryOp, ret: Value) -> PyResult<Option<Value>> {
    match op {
        UnaryOp::Hash => {
            let n = obj_get_int_truncated(ctx, ctx.types, ret).map_err(|e| e.to_pyerror())?;
            Ok(Some(Value::SmallInt(wrap_small_int(n))))
        }
        UnaryOp::IntMaybe => {
            if is_exact_int(ctx.arena, ctx.types, ret) {
                Ok(Some(ret))
            } else {
                Err(ObjTypeError::BareTypeError.to_pyerror())
            }
        }
        _ => Ok(Some(ret)),
    }
}

/// `mp_obj_instance_unary_op` as a `UnaryOpFn` slot (stub seams;
/// `None` → `PyRaise`, the C6 "not implemented" signal).
pub fn instance_unary_op_slot(
    ctx: &mut CallCtx,
    op: UnaryOp,
    self_in: Value,
) -> PyResult<Value> {
    with_stub_seams(|seams| match instance_unary_op(ctx, op, self_in, seams) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(PyError::PyRaise),
        Err(e) => Err(e),
    })
}

// ── binary ──────────────────────────────────────────────────────────

/// `mp_obj_instance_binary_op` (objtype.zig:1152-1189): look the
/// op method up with SLOT_OFFSET_BINARY_OP. SENTINEL → the runtime
/// binary op on subobj0; member → call the bound method with 1
/// arg (rhs); CONTAINS → bool-ify via `obj_is_true`; NULL → NULL.
/// Only the class dictionary is consulted (see the module docs).
/// A member that returns `NotImplemented` has no fallback in this
/// port (no NotImplemented value exists yet — see the module
/// docs); the member's result is returned as-is.
pub fn instance_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
    seams: &mut VmSeams,
) -> PyResult<Option<Value>> {
    let Some(type_) = instance_type_of(ctx, lhs) else {
        return Ok(None);
    };
    let Some(name_q) = super::lookup::binary_op_method_name(op) else {
        return Ok(None);
    };
    match class_lookup(ctx, Some(lhs), name_q, Some(SlotKind::BinaryOp), false, type_, seams) {
        Some(LookupHit::NativeSlot(_)) => {
            let Some(sub0) = instance_subobj0(ctx.arena, lhs) else {
                return Ok(None);
            };
            // The runtime op's result is returned directly (the
            // CONTAINS bool-ify below applies only to the member
            // path — objtype.zig:1172-1176).
            return seams.binary(ctx, op, sub0, rhs);
        }
        Some(LookupHit::Member(fun, bound)) => {
            let ret = seams.call_pair(ctx, fun, bound, 1, 0, &[rhs])?;
            finish_binary(ctx, op, ret)
        }
        None => Ok(None),
    }
}

/// Finish a binary result: CONTAINS bool-ifies, the rest passes
/// through. (`NotImplemented` handling is the open item in the
/// module docs.)
fn finish_binary(
    ctx: &mut CallCtx,
    op: BinaryOp,
    ret: Value,
) -> PyResult<Option<Value>> {
    if op == BinaryOp::Contains {
        let b = obj_is_true(ctx, ctx.types, ret)?;
        Ok(Some(Value::Bool(b)))
    } else {
        Ok(Some(ret))
    }
}

/// `mp_obj_instance_binary_op` as a `BinaryOpFn` slot (stub
/// seams; `None` → `PyRaise`).
pub fn instance_binary_op_slot(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    with_stub_seams(|seams| match instance_binary_op(ctx, op, lhs, rhs, seams) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(PyError::PyRaise),
        Err(e) => Err(e),
    })
}

// ── subscr ──────────────────────────────────────────────────────────

/// `mp_obj_instance_subscr` (objtype.zig:1190-1247): look
/// `__getitem__` / `__setitem__` / `__delitem__` up with
/// SLOT_OFFSET_SUBSCR. SENTINEL → the runtime subscr on subobj0,
/// plus the ADR 010 §C1 `__missing__` protocol on a `KeyError`
/// get; member → call the bound method with 1 arg (get/del) or 2
/// (set); get returns the result, set/del return None.
pub fn instance_subscr(
    ctx: &mut CallCtx,
    self_in: Value,
    index: Value,
    arg: SubscrArg,
    seams: &mut VmSeams,
) -> PyResult<Value> {
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Err(PyError::PyRaise);
    };
    let (name_q, is_get) = match &arg {
        SubscrArg::Get => (dunder(b"__getitem__"), true),
        SubscrArg::Set(_) => (dunder(b"__setitem__"), false),
        SubscrArg::Delete => (dunder(b"__delitem__"), false),
    };
    let Some(name_q) = name_q else {
        return Err(PyError::PyRaise);
    };
    match class_lookup(ctx, Some(self_in), name_q, Some(SlotKind::Subscr), false, type_, seams) {
        Some(LookupHit::NativeSlot(_)) => {
            let Some(sub0) = instance_subobj0(ctx.arena, self_in) else {
                return Err(PyError::PyRaise);
            };
            match seams.subscr(ctx, sub0, index, arg) {
                Ok(v) => Ok(v),
                Err(SubscrError::KeyError) if is_get => {
                    // ADR 010 §C1: a KeyError get consults
                    // `__missing__` (looked up with slot 0 — the
                    // Zig passes SLOT_OFFSET_SUBSCR for the
                    // `__getitem__` probe and 0 here (the lookup
                    // returns the member or NULL, and only a member
                    // is called).
                    let Some(missing_q) = dunder(b"__missing__") else {
                        return Err(PyError::PyRaise);
                    };
                    // Slot 0 (objtype.zig:1224-1231): with the
                    // SUBSCR slot the parent's own subscr slot
                    // would shadow the `__missing__` member.
                    match class_lookup(ctx, Some(self_in), missing_q, None, false, type_, seams) {
                        Some(LookupHit::Member(fun, bound)) => {
                            seams.call_pair(ctx, fun, bound, 1, 0, &[index])
                        }
                        _ => Err(PyError::PyRaise),
                    }
                }
                Err(e) => Err(e.to_pyerror()),
            }
        }
        Some(LookupHit::Member(fun, bound)) => {
            let (n_args, call_args) = match &arg {
                SubscrArg::Get | SubscrArg::Delete => (1, vec![index]),
                SubscrArg::Set(v) => (2, vec![index, *v]),
            };
            let ret = seams.call_pair(ctx, fun, bound, n_args, 0, &call_args)?;
            match &arg {
                SubscrArg::Get => Ok(ret),
                _ => Ok(Value::None),
            }
        }
        None => Err(PyError::PyRaise),
    }
}

/// `mp_obj_instance_subscr` as a `SubscrFn` slot (stub seams).
pub fn instance_subscr_slot(
    ctx: &mut CallCtx,
    self_in: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    with_stub_seams(|seams| instance_subscr(ctx, self_in, index, arg, seams))
}

// ── get_call / call ─────────────────────────────────────────────────

/// `mp_obj_instance_get_call` (objtype.zig:1248-1260): look
/// `__call__` up with SLOT_OFFSET_CALL and return the dest pair
/// as-is (`None` = not callable).
pub fn instance_get_call(
    ctx: &mut CallCtx,
    self_in: Value,
    seams: &mut VmSeams,
) -> Option<LookupHit> {
    let type_ = instance_type_of(ctx, self_in)?;
    let call_q = dunder(b"__call__")?;
    class_lookup(ctx, Some(self_in), call_q, Some(SlotKind::Call), false, type_, seams)
}

/// `mp_obj_instance_is_callable` (objtype.zig:454-462): a
/// `__call__` lookup hit exists.
pub fn instance_is_callable(
    ctx: &mut CallCtx,
    self_in: Value,
    seams: &mut VmSeams,
) -> bool {
    instance_get_call(ctx, self_in, seams).is_some()
}

/// `instanceCallImpl` (objtype.zig:478-490): NULL → "object not
/// callable"; SENTINEL → call subobj0 as a function via the call
/// seam; member → the bound call with (fun, self).
pub fn instance_call(
    ctx: &mut CallCtx,
    self_in: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    seams: &mut VmSeams,
) -> PyResult<Value> {
    let Some(hit) = instance_get_call(ctx, self_in, seams) else {
        return Err(ObjTypeError::ObjectNotCallable.to_pyerror());
    };
    match hit {
        LookupHit::NativeSlot(_) => {
            let Some(sub0) = instance_subobj0(ctx.arena, self_in) else {
                return Err(PyError::PyRaise);
            };
            seams.call(ctx, sub0, n_args, n_kw, args)
        }
        LookupHit::Member(fun, bound) => seams.call_pair(ctx, fun, bound, n_args, n_kw, args),
    }
}

/// `instanceCallImpl` as a `CallFn` slot (stub seams).
pub fn instance_call_slot(
    ctx: &mut CallCtx,
    self_in: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    with_stub_seams(|seams| instance_call(ctx, self_in, n_args, n_kw, args, seams))
}

// ── get_buffer ──────────────────────────────────────────────────────

/// `instance_get_buffer` (objtype.zig:1261-1287): look the buffer
/// slot up with attr = the EMPTY qstr (id 0) and
/// SLOT_OFFSET_BUFFER. SENTINEL → the native type's buffer slot on
/// subobj0; ANY other outcome (incl. a Member hit) → not
/// supported (`Ok(false)`).
pub fn instance_get_buffer(
    ctx: &mut CallCtx,
    self_in: Value,
    info: &mut BufferInfo,
    flags: u32,
    seams: &mut VmSeams,
) -> PyResult<bool> {
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Ok(false);
    };
    match class_lookup(ctx, Some(self_in), 0, Some(SlotKind::Buffer), false, type_, seams) {
        Some(LookupHit::NativeSlot(_)) => {
            let Some(sub0) = instance_subobj0(ctx.arena, self_in) else {
                return Ok(false);
            };
            let Some(sub_type) = obj_get_type(ctx.arena, ctx.types, sub0) else {
                return Ok(false);
            };
            match buffer_slot(ctx.arena, Some(sub_type)) {
                Some(f) => f(ctx, sub0, info, flags),
                None => Ok(false),
            }
        }
        _ => Ok(false),
    }
}

/// `instance_get_buffer` as a `BufferFn` slot (stub seams).
pub fn instance_get_buffer_slot(
    ctx: &mut CallCtx,
    self_in: Value,
    info: &mut BufferInfo,
    flags: u32,
) -> PyResult<bool> {
    with_stub_seams(|seams| instance_get_buffer(ctx, self_in, info, flags, seams))
}

// ── load_attr / store_attr / attr ───────────────────────────────────

/// `mp_obj_instance_load_attr` (objtype.zig:1288-1378): (1)
/// members map; (2) `__dict__` → the read-only copy (full only);
/// (3) class lookup with slot 0; (4) no HAS_SPECIAL_ACCESSORS →
/// the member as-is, else property (`proxy[0] == NONE` = the
/// lookup FAILS, no `__getattr__` fallback — objtype.zig:1320)
/// then descriptor `__get__(self, type)`; (5) the `__getattr__`
/// fallback with the descriptor-dunder and
/// `__setattr__`/`__delattr__` guards.
pub fn instance_load_attr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
    seams: &mut VmSeams,
) -> PyResult<Option<Value>> {
    // (1) The members map.
    if let Value::Heap(r) = self_in {
        if let Obj::Instance(d) = ctx.arena.get(r) {
            if let Some(i) = d.members.find_qstr(ctx.pool, attr) {
                return Ok(Some(d.members.entries[i].value));
            }
        }
    }
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Ok(None);
    };
    // (2) `__dict__` (full only — this port is full config).
    if Some(attr) == dunder(b"__dict__") {
        return Ok(Some(instance_dict_readonly(ctx, self_in)?));
    }
    // (3) The class lookup (slot 0 — the Zig passes 0 here, so
    // the native-slot fast path is skipped; the hit is a Member
    // or nothing).
    let hit = class_lookup(ctx, Some(self_in), attr, None, false, type_, seams);
    let Some(hit) = hit else {
        return load_attr_getattr(ctx, self_in, attr, seams);
    };
    let LookupHit::Member(fun, bound) = hit else {
        // A NativeSlot hit cannot occur with slot 0; treat it as
        // "no member" (the `__getattr__` fallback below).
        return load_attr_getattr(ctx, self_in, attr, seams);
    };
    // (4) No special accessors → the member as-is.
    let has_special = match type_data(ctx.arena, type_) {
        Some(t) => t.flags & FLAG_HAS_SPECIAL_ACCESSORS != 0,
        None => false,
    };
    if !has_special {
        return Ok(Some(bind_pair(ctx, fun, bound)));
    }
    // PROPERTY: call fget(self); a missing getter FAILS the
    // lookup outright (no `__getattr__` fallback).
    if is_property(ctx, fun) {
        let (getter, _, _) = property_proxy(ctx.arena, fun);
        if getter == Value::None {
            // Unreadable attribute (objtype.zig:1320): fail
            // outright — NULL without the SENTINEL, so the caller
            // raises `AttributeError("no attribute 'prop'")`
            // instead of falling through to the class dict and
            // returning the property object itself (E2: `d.prop`
            // with `prop = property()` silently returned).
            let name =
                String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
            ctx.record_raise(
                "AttributeError",
                Some(&format!("no attribute '{name}'")),
            );
            return Err(PyError::PyRaise);
        }
        let ret = seams.call(ctx, getter, 1, 0, &[self_in])?;
        return Ok(Some(ret));
    }
    // DESCRIPTORS: `__get__(self, type)` when the member has one.
    if let Some(get_q) = dunder(b"__get__") {
        if let Some((gfun, gbound)) = seams.load_maybe(ctx, fun, get_q) {
            let type_v = Value::Heap(type_);
            let ret = seams.call_pair(ctx, gfun, gbound, 2, 0, &[self_in, type_v])?;
            return Ok(Some(ret));
        }
    }
    Ok(Some(bind_pair(ctx, fun, bound)))
}

/// Step (5) of `mp_obj_instance_load_attr`: the `__getattr__`
/// fallback. Skipped for `__getattr__` itself, for the descriptor
/// dunders {`__get__`, `__set__`, `__delete__`, `__set_name__`}
/// and (full config) for `__setattr__`/`__delattr__`
/// (objtype.zig:1349-1377).
fn load_attr_getattr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
    seams: &mut VmSeams,
) -> PyResult<Option<Value>> {
    let Some(getattr_q) = dunder(b"__getattr__") else {
        return Ok(None);
    };
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
        if Some(attr) == dunder(name) {
            return Ok(None);
        }
    }
    match seams.load_maybe(ctx, self_in, getattr_q) {
        Some((fun, bound)) => {
            let ret = seams.call_pair(ctx, fun, bound, 1, 0, &[Value::Qstr(attr)])?;
            Ok(Some(ret))
        }
        None => Ok(None),
    }
}

/// `mp_obj_instance_store_attr` (objtype.zig:1379-1458). Returns
/// `Ok(true)` = handled, `Ok(false)` = not handled (the caller
/// raises AttributeError), `Err` = a raise propagated from a
/// setter/descriptor/hook call. `value == None` deletes.
/// The property-without-slot case returns `false` IMMEDIATELY
/// (no hook, no members map); a descriptor miss falls through
/// to the hooks and then the members map.
pub fn instance_store_attr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
    value: Option<Value>,
    seams: &mut VmSeams,
) -> PyResult<bool> {
    let Some(type_) = instance_type_of(ctx, self_in) else {
        return Ok(false);
    };
    let has_special = match type_data(ctx.arena, type_) {
        Some(t) => t.flags & FLAG_HAS_SPECIAL_ACCESSORS != 0,
        None => false,
    };
    if has_special {
        let hit = class_lookup(ctx, Some(self_in), attr, None, false, type_, seams);
        if let Some(LookupHit::Member(fun, _bound)) = hit {
            // PROPERTY: the explicit setter/deleter slot.
            if is_property(ctx, fun) {
                let (_, setter, deleter) = property_proxy(ctx.arena, fun);
                match value {
                    None => {
                        if deleter == Value::None {
                            return Ok(false);
                        }
                        seams.call(ctx, deleter, 1, 0, &[self_in])?;
                        return Ok(true);
                    }
                    Some(v) => {
                        if setter == Value::None {
                            return Ok(false);
                        }
                        seams.call(ctx, setter, 2, 0, &[self_in, v])?;
                        return Ok(true);
                    }
                }
            }
            // DESCRIPTORS: `__set__` / `__delete__` on the member.
            let hook_q = match value {
                None => dunder(b"__delete__"),
                Some(_) => dunder(b"__set__"),
            };
            if let Some(hook_q) = hook_q {
                if let Some((mfun, mbound)) = seams.load_maybe(ctx, fun, hook_q) {
                    match value {
                        None => {
                            seams.call_pair(ctx, mfun, mbound, 1, 0, &[self_in])?;
                        }
                        Some(v) => {
                            seams.call_pair(ctx, mfun, mbound, 2, 0, &[self_in, v])?;
                        }
                    }
                    return Ok(true);
                }
            }
        }
        // `__setattr__` / `__delattr__` hooks (full only).
        let hook_q = match value {
            None => dunder(b"__delattr__"),
            Some(_) => dunder(b"__setattr__"),
        };
        if let Some(hook_q) = hook_q {
            if let Some((hfun, hbound)) = seams.load_maybe(ctx, self_in, hook_q) {
                match value {
                    None => {
                        seams.call_pair(ctx, hfun, hbound, 1, 0, &[Value::Qstr(attr)])?;
                    }
                    Some(v) => {
                        seams.call_pair(ctx, hfun, hbound, 2, 0, &[Value::Qstr(attr), v])?;
                    }
                }
                return Ok(true);
            }
        }
    }
    // The members map (the Zig's `skip_special_accessors` tail).
    let Value::Heap(r) = self_in else {
        return Ok(false);
    };
    let pool = ctx.pool;
    match ctx.arena.get_mut(r) {
        Obj::Instance(d) => match value {
            None => Ok(d.members.remove_qstr(pool, attr)),
            Some(v) => {
                d.members.store_qstr(pool, attr, v);
                Ok(true)
            }
        },
        _ => Ok(false),
    }
}

/// `mp_obj_instance_attr` as an `AttrFn` slot: the LOAD path only
/// (stub seams). The store path is the free function
/// `instance_store_attr` — see the module docs.
pub fn instance_attr(ctx: &mut CallCtx, self_in: Value, attr: u32) -> PyResult<Option<Value>> {
    with_stub_seams(|seams| instance_load_attr(ctx, self_in, attr, seams))
}

// ── re-exports for the slot tables (typecreate.rs) ──────────────────

/// `mp_obj_instance_*` as the slot fns a user type carries
/// (`TypeData` fields, in order): `unary_op`, `binary_op`,
/// `call`, `getiter`, `subscr`, `buffer`, `print`, `attr`.
pub const INSTANCE_UNARY_OP: crate::objects::slots::UnaryOpFn = instance_unary_op_slot;
pub const INSTANCE_BINARY_OP: crate::objects::slots::BinaryOpFn = instance_binary_op_slot;
pub const INSTANCE_CALL: crate::objects::slots::CallFn = instance_call_slot;
pub const INSTANCE_GETITER: GetIterFn = instance_getiter_slot;
pub const INSTANCE_SUBSCR: crate::objects::slots::SubscrFn = instance_subscr_slot;
pub const INSTANCE_GET_BUFFER: BufferFn = instance_get_buffer_slot;
pub const INSTANCE_PRINT: PrintFn = instance_print_slot;
pub const INSTANCE_ATTR: AttrFn = instance_attr;
