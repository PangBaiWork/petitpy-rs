//! Local VM seams (C10's runner-seam pattern).
//!
//! `mp_call_function_n_kw` / `mp_call_method_n_kw` /
//! `mp_load_method_maybe` / `mp_unary_op` / `mp_binary_op` /
//! `mp_obj_subscr` live in `runtime.zig` (C14's file), and the
//! bytecode step lives in the VM (C13). The instance machinery
//! in this lane cannot run without them, so they arrive as explicit
//! parameters — C13/C14 replace the closures with the real
//! implementations when the VM lands. The shapes are the contract:
//!
//! * `call(ctx, fun, n_args, n_kw, args) -> PyResult<Value>` —
//!   call `fun` as a function. `args` holds `n_args` positionals
//!   followed by `2 * n_kw` `(name, value)` keyword pairs (the
//!   `CallFn` layout, `slots.rs`). This is the Rust spelling of
//!   Zig `mp_call_function_n_kw`.
//! * `load_maybe(ctx, obj, attr) -> Option<(fun, self)>` —
//!   `mp_load_method_maybe`: `Some((fun, Some(self)))` = bound
//!   method, `Some((fun, None))` = plain attribute (e.g. the
//!   `__class__` special case), `None` = no such attribute.
//! * `unary(ctx, op, value) -> PyResult<Option<Value>>` — the
//!   runtime `mp_unary_op`: `Ok(None)` = not implemented (the
//!   Zig's NULL), `Ok(Some(v))` = the result, `Err` = a raise.
//! * `binary(ctx, op, lhs, rhs) -> PyResult<Option<Value>>` —
//!   the runtime `mp_binary_op` (including the reflected-op
//!   fallback), same outcome shape.
//! * `subscr(ctx, base, index, arg) -> Result<Value,
//!   SubscrError>` — the runtime `mp_obj_subscr`. `KeyError` is
//!   its own variant because the `__missing__` protocol
//!   (ADR 010 §C1) must distinguish a KeyError raise from any
//!   other raise, and the plain `PyError` signal carries no
//!   exception object (that arrives with C13's `pending_exc`).
//!
//! `call_via_type_slot` is the builtin-live spelling of
//! `mp_call_function_n_kw` (look the fun's type up, invoke its
//! `call` slot — C10a's slots are live for builtins, `PyRaise` for
//! `FunBc` until C13 replaces the runner). `with_stub_seams`
//! builds all five closures from the builtin-live paths (the
//! unary/binary/subscr stubs are C6's slot dispatch — they cannot
//! propagate a real slot raise, per the C6 convention, and the
//! subscr stub never produces `KeyError`; C14's real
//! `mp_obj_subscr` reports it explicitly). Slot adapters
//! (`instance_attr`, `instance_call`, …) use `with_stub_seams`:
//! their frozen signatures cannot take seams, so the builtin-live
//! path is what a slot can do today, and C13's runner replacement
//! makes it complete.

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::obj::{call_binary, call_subscr, call_unary, obj_get_type, ObjError, SlotOutcome};
use crate::objects::slots::{BinaryOp, CallHook, SubscrArg, UnaryOp};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

/// What `mp_obj_subscr` raised. `KeyError` is separate because
/// the `__missing__` protocol must tell it apart from other
/// raises (see the module docs).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SubscrError {
    /// A `KeyError` raise (the `__missing__` protocol's trigger).
    KeyError,
    /// Any other raise (or an allocation failure).
    Raise(PyError),
}

impl SubscrError {
    /// The plain `PyError` signal.
    pub fn to_pyerror(self) -> PyError {
        match self {
            SubscrError::KeyError => PyError::PyRaise,
            SubscrError::Raise(e) => e,
        }
    }
}

/// The VM entry points the instance machinery needs.
pub struct VmSeams<'a> {
    /// Call `fun` as a function (see the module docs).
    pub call: &'a mut dyn FnMut(&mut CallCtx, Value, usize, usize, &[Value]) -> PyResult<Value>,
    /// `mp_load_method_maybe` (see the module docs).
    pub load_maybe:
        &'a mut dyn FnMut(&mut CallCtx, Value, u32) -> Option<(Value, Option<Value>)>,
    /// The runtime `mp_unary_op` (see the module docs).
    pub unary: &'a mut dyn FnMut(&mut CallCtx, UnaryOp, Value) -> PyResult<Option<Value>>,
    /// The runtime `mp_binary_op` (see the module docs).
    pub binary: &'a mut dyn FnMut(&mut CallCtx, BinaryOp, Value, Value) -> PyResult<Option<Value>>,
    /// The runtime `mp_obj_subscr` (see the module docs).
    pub subscr: &'a mut dyn FnMut(
        &mut CallCtx,
        Value,
        Value,
        SubscrArg,
    ) -> Result<Value, SubscrError>,
}

impl<'a> VmSeams<'a> {
    /// `mp_call_function_n_kw(fun, n_args, n_kw, args)` through the
    /// seam.
    pub fn call(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        n_args: usize,
        n_kw: usize,
        args: &[Value],
    ) -> PyResult<Value> {
        (self.call)(ctx, fun, n_args, n_kw, args)
    }

    /// `mp_call_method_n_kw(n_args, n_kw, [fun, self, args...])`:
    /// bind `fun` to `self_` and call it through the seam. The Zig
    /// passes the pair on the stack; Rust allocates the bound
    /// method (same observable behaviour).
    pub fn call_method(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        self_: Value,
        n_args: usize,
        n_kw: usize,
        args: &[Value],
    ) -> PyResult<Value> {
        let boundmeth_type = ctx.types.boundmeth;
        let bound = new_bound_meth(ctx, boundmeth_type, fun, self_);
        (self.call)(ctx, Value::Heap(bound), n_args, n_kw, args)
    }

    /// Call a (fun, bound-self) pair as a method: a bound pair
    /// goes through `call_method`, an unbound one is called
    /// directly (the Zig's `mp_call_method_n_kw` with a NULL
    /// `dest[1]` calls the function with the args alone).
    pub fn call_pair(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        bound: Option<Value>,
        n_args: usize,
        n_kw: usize,
        args: &[Value],
    ) -> PyResult<Value> {
        match bound {
            Some(self_) => self.call_method(ctx, fun, self_, n_args, n_kw, args),
            None => self.call(ctx, fun, n_args, n_kw, args),
        }
    }

    /// `mp_load_method_maybe(obj, attr)` through the seam.
    pub fn load_maybe(
        &mut self,
        ctx: &mut CallCtx,
        obj: Value,
        attr: u32,
    ) -> Option<(Value, Option<Value>)> {
        (self.load_maybe)(ctx, obj, attr)
    }

    /// The runtime `mp_unary_op` through the seam.
    pub fn unary(
        &mut self,
        ctx: &mut CallCtx,
        op: UnaryOp,
        value: Value,
    ) -> PyResult<Option<Value>> {
        (self.unary)(ctx, op, value)
    }

    /// The runtime `mp_binary_op` through the seam.
    pub fn binary(
        &mut self,
        ctx: &mut CallCtx,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    ) -> PyResult<Option<Value>> {
        (self.binary)(ctx, op, lhs, rhs)
    }

    /// The runtime `mp_obj_subscr` through the seam.
    pub fn subscr(
        &mut self,
        ctx: &mut CallCtx,
        base: Value,
        index: Value,
        arg: SubscrArg,
    ) -> Result<Value, SubscrError> {
        (self.subscr)(ctx, base, index, arg)
    }
}

/// `mp_call_function_n_kw` for the builtin-live path: look the
/// fun's type up and invoke its `call` slot. `Err(PyRaise)` when
/// the fun has no type or the type has no call slot (the Zig
/// returns NULL there — the caller treats it as "not callable").
/// C14's `mp_call_function_n_kw` replaces this when the VM lands.
pub fn call_via_type_slot(
    ctx: &mut CallCtx,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let Some(type_) = obj_get_type(ctx.arena, ctx.types, fun) else {
        return Err(PyError::PyRaise);
    };
    let Some(call) = type_call_slot(ctx, type_) else {
        return Err(PyError::PyRaise);
    };
    call(ctx, fun, n_args, n_kw, args)
}

/// The `call` slot of a type (`None` = the Zig's NULL slot).
pub fn type_call_slot(ctx: &CallCtx, type_: GcRef) -> Option<crate::objects::slots::CallFn> {
    match ctx.arena.get(type_) {
        crate::objects::value::Obj::Type(t) => t.call,
        _ => None,
    }
}

/// Run `f` with seams built from a custom `call`, a custom
/// `load_maybe` and a custom `subscr` closure, with the
/// builtin-live stubs for the other two entries (see
/// `with_stub_seams` for the fidelity limits). `with_seams` /
/// `with_subscr_seams` / `with_stub_seams` pass the builtin-live
/// `call_via_type_slot` here; `with_call_hook_seams` (E2)
/// passes the VM's real call hook instead.
pub fn with_custom_seams<R>(
    mut call: impl FnMut(&mut CallCtx, Value, usize, usize, &[Value]) -> PyResult<Value>,
    mut load: impl FnMut(&mut CallCtx, Value, u32) -> Option<(Value, Option<Value>)>,
    mut subscr: impl FnMut(&mut CallCtx, Value, Value, SubscrArg) -> Result<Value, SubscrError>,
    f: impl FnOnce(&mut VmSeams) -> R,
) -> R {
    let mut unary = |ctx: &mut CallCtx, op: UnaryOp, v: Value| {
        let t = obj_get_type(ctx.arena, ctx.types, v);
        match call_unary(ctx, t, op, v) {
            Ok(SlotOutcome::Value(v)) => Ok(Some(v)),
            Ok(SlotOutcome::NotImplemented) => Ok(None),
            Err(ObjError::OutOfMemory) => Err(PyError::OutOfMemory),
            Err(e) => Err(e.to_pyerror()),
        }
    };
    let mut binary = |ctx: &mut CallCtx, op: BinaryOp, l: Value, r: Value| {
        let t = obj_get_type(ctx.arena, ctx.types, l);
        match call_binary(ctx, t, op, l, r) {
            Ok(SlotOutcome::Value(v)) => Ok(Some(v)),
            Ok(SlotOutcome::NotImplemented) => Ok(None),
            Err(ObjError::OutOfMemory) => Err(PyError::OutOfMemory),
            Err(e) => Err(e.to_pyerror()),
        }
    };
    let mut seams = VmSeams {
        call: &mut call,
        load_maybe: &mut load,
        unary: &mut unary,
        binary: &mut binary,
        subscr: &mut subscr,
    };
    f(&mut seams)
}

/// Run `f` with seams built from a custom `load_maybe` closure
/// and the builtin-live stubs for the other four entries.
pub fn with_seams<R>(
    load: impl FnMut(&mut CallCtx, Value, u32) -> Option<(Value, Option<Value>)>,
    f: impl FnOnce(&mut VmSeams) -> R,
) -> R {
    with_custom_seams(
        |ctx: &mut CallCtx, fun: Value, n: usize, k: usize, a: &[Value]| {
            call_via_type_slot(ctx, fun, n, k, a)
        },
        load,
        stub_subscr,
        f,
    )
}

/// A `VmHook` over a plain `call` closure (E2b): the seam's
/// `call` entry is a `dyn FnMut`, not a `VmHook` — when a native
/// base's `make_new` needs the hook (the `instance_make_new`
/// tail forwards the seam's call so a native base built from an
/// iterable gets the same hook the `__init__` path runs on),
/// this adapter carries the seam's call down. `call` delegates;
/// `resume` has no VM behind it (the ctx-only path never resumes
/// a generator), so it records the collector's not-iterable
/// raise and returns `PyRaise` — the same error
/// `collect_iterable` produced for a generator before E2b added
/// the `Generator` arm.
pub struct SeamHook<'a> {
    call: &'a mut dyn FnMut(&mut CallCtx, Value, usize, usize, &[Value]) -> PyResult<Value>,
}

impl<'a> SeamHook<'a> {
    /// Wrap the seam's `call` closure.
    pub fn new(
        call: &'a mut dyn FnMut(&mut CallCtx, Value, usize, usize, &[Value]) -> PyResult<Value>,
    ) -> Self {
        SeamHook { call }
    }
}

impl crate::objects::slots::VmHook for SeamHook<'_> {
    fn call(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        n: usize,
        k: usize,
        a: &[Value],
    ) -> PyResult<Value> {
        (self.call)(ctx, fun, n, k, a)
    }

    fn resume(&mut self, ctx: &mut CallCtx, _gen: Value) -> Result<Option<Value>, PyError> {
        ctx.record_raise("TypeError", Some("object not iterable"));
        Err(PyError::PyRaise)
    }

    fn binary_op(
        &mut self,
        ctx: &mut CallCtx,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    ) -> PyResult<Value> {
        crate::objects::obj_ops::obj_binary_op(ctx, ctx.types, op, lhs, rhs)
    }
}

/// Run `f` with seams whose `call` is the VM's real call hook
/// (E2 — a `FunBc` callee actually executes) and the other four
/// entries are the builtin-live stubs. `instance_make_new` uses
/// this on the VM's type-call path; the ctx-only path keeps
/// `with_stub_seams` (`call_via_type_slot`, `FunBc` raises).
/// E2b: the hook is the `VmHook` trait object — its `call`
/// operation is adapted into the seam's plain-`FnMut` `call`
/// closure here (the seam itself only ever calls). The hook is
/// reborrowed by the adapter closure, so the caller keeps it
/// afterwards.
pub fn with_call_hook_seams<R>(hook: CallHook<'_, '_>, f: impl FnOnce(&mut VmSeams) -> R) -> R {
    with_custom_seams(
        |ctx: &mut CallCtx, fun: Value, n: usize, k: usize, a: &[Value]| {
            hook.call(ctx, fun, n, k, a)
        },
        |_ctx: &mut CallCtx, _obj: Value, _attr: u32| None,
        stub_subscr,
        f,
    )
}

/// Run `f` with seams built from a custom `subscr` closure and
/// the builtin-live stubs for the other four entries. The
/// `__missing__` protocol is testable only through this shape
/// (the stub subscr never reports `KeyError` — see the module
/// docs).
pub fn with_subscr_seams<R>(
    subscr: impl FnMut(&mut CallCtx, Value, Value, SubscrArg) -> Result<Value, SubscrError>,
    f: impl FnOnce(&mut VmSeams) -> R,
) -> R {
    with_custom_seams(
        |ctx: &mut CallCtx, fun: Value, n: usize, k: usize, a: &[Value]| {
            call_via_type_slot(ctx, fun, n, k, a)
        },
        |_ctx: &mut CallCtx, _obj: Value, _attr: u32| None,
        subscr,
        f,
    )
}

/// Run `f` with the stub seams: `call` is the builtin-live
/// `call_via_type_slot`, `load_maybe` finds nothing, and
/// unary/binary/subscr are C6's slot dispatch (see the module
/// docs for the fidelity limits). Slot adapters use this; tests
/// that need a real `load_maybe` or `subscr` use `with_seams` /
/// `with_subscr_seams`.
pub fn with_stub_seams<R>(f: impl FnOnce(&mut VmSeams) -> R) -> R {
    with_seams(|_ctx: &mut CallCtx, _obj: Value, _attr: u32| None, f)
}

/// The stub `subscr` closure: C6's `call_subscr` mapped onto
/// `SubscrError` (never `KeyError` — see the module docs).
fn stub_subscr(
    ctx: &mut CallCtx,
    b: Value,
    i: Value,
    a: SubscrArg,
) -> Result<Value, SubscrError> {
    let t = obj_get_type(ctx.arena, ctx.types, b);
    match call_subscr(ctx, t, b, i, a) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => Err(SubscrError::Raise(PyError::PyRaise)),
        Err(ObjError::OutOfMemory) => Err(SubscrError::Raise(PyError::OutOfMemory)),
        Err(e) => Err(SubscrError::Raise(e.to_pyerror())),
    }
}
