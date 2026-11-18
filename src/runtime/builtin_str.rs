//! VM-intercepted `str` / `repr` / `bool` / `len` / `print`
//! (E2, Fix C follow-up).
//!
//! These five builtins are ctx-only `FunBuiltin`s (or, for
//! `str` / `bool`, the type objects themselves), so an
//! instance's `__str__` / `__repr__` / `__bool__` / `__len__`
//! never runs through them — the slot adapters use stub seams.
//! They are intercepted by GcRef identity in
//! `vm_call_intercept` (the D1 pattern) and routed here, where
//! `state` is live for the real call hook.
//!
//! Fidelity notes (all probed on the reference binary):
//! - `str(x)` calls `__str__`, falling back to `__repr__` when
//!   `__str__` is missing; `repr(x)` calls `__repr__` only.
//!   The dunder's RESULT is printed as STR into the return
//!   value (`str(A())` is `"1"` when `__str__` returns `1` —
//!   the result is rendered, not returned raw).
//! - `len(x)` returns the `__len__` result RAW (`len(C())` is
//!   `"x"` when `__len__` returns `"x"`).
//! - `bool(x)` uses the `__bool__` identity rule (`__bool__`
//!   returning `1` is `False` — see `vm_seams`).
//! - A dunder that raises PROPAGATES here. (The reference
//!   ABORTS on a raising `__str__` inside `str()` — a Zig
//!   null-pointer panic in `instance_print` — so Rust raising
//!   the exception is a deliberate divergence, same class as
//!   the other abort-vs-raise cases. The print SLOT swallows
//!   the raise and falls back to the default rendering — that
//!   is the slot's infallible signature, not the builtin's.)

use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_is_true;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::objtype::lookup::{class_lookup, LookupHit, SlotKind};
use crate::objects::objtype::seams::with_stub_seams;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind};
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_call::call_type_ctx;
use crate::runtime::vm_seams::{is_instance, try_instance_bool, try_instance_unary_op};

/// Look a print dunder up the way `instance_print` does
/// (`lookup_print_method`): `class_lookup` with the PRINT slot
/// (slot 0 for the `__repr__` fallback after a missed
/// `__str__`), stub seams. Returns the BOUND method value
/// (`bind_pair`), or `None` when there is no such dunder.
fn lookup_print_dunder(
    ctx: &mut CallCtx,
    value: Value,
    type_: crate::objects::value::GcRef,
    attr: u32,
    slot: Option<SlotKind>,
) -> Option<Value> {
    use crate::objects::objtype::instance::bind_pair;
    let hit = with_stub_seams(|seams| class_lookup(ctx, Some(value), attr, slot, false, type_, seams));
    match hit {
        Some(LookupHit::Member(fun, bound)) => Some(bind_pair(ctx, fun, bound)),
        _ => None,
    }
}

/// Render an instance through its `__str__` / `__repr__`
/// dunder, the way `instance_print` does: call the dunder with
/// 0 args, print the RESULT as STR. `Ok(Some(s))` = a dunder
/// ran; `Ok(None)` = no dunder (fall back to the default
/// rendering); `Err(exc)` = the dunder raised (propagated —
/// see the module docs).
fn render_instance(
    ctx: &mut CallCtx,
    state: &mut VmState,
    value: Value,
    kind: PrintKind,
) -> Result<Option<String>, Value> {
    let Some(type_) = crate::objects::obj::obj_get_type(ctx.arena, ctx.types, value) else {
        return Ok(None);
    };
    let mut fun = None;
    if kind == PrintKind::Str {
        if let Some(q) = static_qstr_id(b"__str__") {
            fun = lookup_print_dunder(ctx, value, type_, q, Some(SlotKind::Print));
        }
        if fun.is_none() {
            if let Some(q) = static_qstr_id(b"__repr__") {
                fun = lookup_print_dunder(ctx, value, type_, q, None);
            }
        }
    } else if let Some(q) = static_qstr_id(b"__repr__") {
        fun = lookup_print_dunder(ctx, value, type_, q, Some(SlotKind::Print));
    }
    let Some(fun) = fun else {
        return Ok(None);
    };
    let ret = call_function_ctx(ctx, state, fun, 0, 0, &[])?;
    let mut s = String::new();
    obj_print_helper(ctx, ctx.types, ret, PrintKind::Str, &mut s);
    Ok(Some(s))
}

/// Allocate a fresh heap `Str` from bytes.
fn new_str_value(ctx: &mut CallCtx, bytes: &[u8]) -> Value {
    use crate::objects::strings::objstr_a::new_str;
    new_str(ctx, ctx.types, bytes)
}

/// `str(x)` through the VM (E2): a 1-arg instance call runs
/// the dunder path; anything else delegates to the type-call
/// path (`str_make_new` — `str()`, `str(b, encoding)`, ...).
pub(crate) fn call_str_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                if let Some(s) = render_instance(ctx, state, arg, PrintKind::Str)? {
                    return Ok(new_str_value(ctx, s.as_bytes()));
                }
            }
        }
    }
    let r = ctx.types.str_;
    call_type_ctx(ctx, state, r, n_args, n_kw, args)
}

/// `repr(x)` through the VM (E2): a 1-arg instance call runs
/// the dunder path; anything else delegates to the existing
/// `repr_builtin`.
pub(crate) fn call_repr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                if let Some(s) = render_instance(ctx, state, arg, PrintKind::Repr)? {
                    return Ok(new_str_value(ctx, s.as_bytes()));
                }
            }
        }
    }
    crate::runtime::builtin_fns::repr_builtin(ctx, &args[..n_args])
        .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e))
}

/// `bool(x)` through the VM (E2): a 1-arg instance call runs
/// `__bool__` / `__len__` with the real hook; anything else
/// delegates to the type-call path (`bool()`, `bool(x)` for
/// non-instances, arg errors).
pub(crate) fn call_bool_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                match try_instance_bool(ctx, state, arg)? {
                    Some(b) => return Ok(Value::Bool(b)),
                    None => {}
                }
                let types = ctx.types;
                return obj_is_true(ctx, types, arg)
                    .map(Value::Bool)
                    .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e));
            }
        }
    }
    let r = ctx.types.bool_;
    call_type_ctx(ctx, state, r, n_args, n_kw, args)
}

/// `len(x)` through the VM (E2): a 1-arg instance call runs
/// `__len__` with the real hook and returns the result RAW;
/// anything else delegates to the existing `len_builtin`.
pub(crate) fn call_len_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                use crate::objects::slots::UnaryOp;
                if let Some(v) = try_instance_unary_op(ctx, state, UnaryOp::Len, arg)? {
                    return Ok(v);
                }
            }
        }
    }
    crate::runtime::builtin_fns::len_builtin(ctx, &args[..n_args])
        .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e))
}

/// `print(...)` through the VM (E2): when any positional arg
/// is an instance, render through `print_impl_with` with the
/// instance-aware render (a dunder raise is stashed on
/// `state.pending_exc` and drained below); otherwise delegate
/// to the existing `print_builtin` (bit-identical behaviour
/// for non-instance args).
pub(crate) fn call_print_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let has_instance = args[..n_args]
        .iter()
        .any(|a| is_instance(ctx.arena, *a));
    if !has_instance {
        let pairs: Vec<(u32, Value)> = (0..n_kw)
            .map(|i| {
                let name = args[n_args + 2 * i].as_qstr().unwrap_or(0);
                (name, args[n_args + 2 * i + 1])
            })
            .collect();
        return crate::runtime::vm_builtins::print_builtin(ctx, &args[..n_args], &pairs)
            .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e));
    }
    let pairs: Vec<(u32, Value)> = (0..n_kw)
        .map(|i| {
            let name = args[n_args + 2 * i].as_qstr().unwrap_or(0);
            (name, args[n_args + 2 * i + 1])
        })
        .collect();
    // The render closure borrows `state` for the dunder calls;
    // it is dropped when `print_impl_with` returns, before the
    // error drain below borrows `state` again.
    let result = {
        let mut render = |ctx: &mut CallCtx, v: Value, s: &mut String| -> PyResult<()> {
            if is_instance(ctx.arena, v) {
                // `state` is captured by the closure; a dunder
                // raise stashes the exception object and reports
                // `PyRaise` (the C14 side-channel idiom).
                match render_instance(ctx, state, v, PrintKind::Str) {
                    Ok(Some(text)) => {
                        s.push_str(&text);
                        Ok(())
                    }
                    Ok(None) => {
                        obj_print_helper(ctx, ctx.types, v, PrintKind::Str, s);
                        Ok(())
                    }
                    Err(exc) => {
                        state.pending_exc = Some(exc);
                        Err(PyError::PyRaise)
                    }
                }
            } else {
                obj_print_helper(ctx, ctx.types, v, PrintKind::Str, s);
                Ok(())
            }
        };
        crate::runtime::vm_builtins::print_impl_with(
            ctx,
            &args[..n_args],
            &pairs,
            &mut render,
        )
    };
    result.map_err(|e| crate::runtime::vm_seams::drain_hook_raise(ctx, state, e))
}