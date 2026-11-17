//! Generator resume (`runtime.zig:1363-1491`): `mp_resume` and
//! `mp_make_raise_obj`.
//!
//! `mp_resume`'s shape: a gen-instance type → the `gen_resume`
//! seam (C10b's `gen_resume` + C13's runner — the frame and the
//! runner are C13-side, so this lane calls the seam, never
//! `gen_resume` directly); `hasIterNext` (flags & 0x0180) with
//! `send == None` → the `iternext` fast path; `send == None`
//! → `__next__` via `maybe` + call; `send != None` → `send`
//! via `load_method` (RAISES if missing) + call; the throw
//! path (`throw: Some` — `send` is ignored, the Zig's
//! `send == NULL`) → `GeneratorExit` subclass → `close` via
//! maybe + call, else `throw` via maybe + call, else the PEP
//! 479 tail (`StopIteration` → `RuntimeError("generator raised
//! StopIteration")`, else `mp_make_raise_obj`).
//!
//! `send` is a `Value` (`None` = the `__next__` path) and
//! `throw` is `Option<Value>` (`Some` = the throw path) — the
//! Zig's `send == NULL` ⟺ throw-path encoding, without a
//! null sentinel. The Zig returns `YIELD` unconditionally
//! after the `throw` call (a raise travels via `pending_exc`);
//! the Rust `Err` propagates the same raise — same net
//! outcome. The `StopIteration` translation on the `__next__`
//! / `send` paths is unobservable pre-C13 (KNOWN GAP #1 —
//! the call slot erases the kind); the `RtError::
//! StopIteration` arms below fire only when the callee raised
//! through that variant (e.g. the `next` builtin, or a
//! generator's own resume).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objgenerator::{GenError, GenResume, ResumeKind};
use crate::objects::obj::{obj_get_type, type_data, FLAG_ITER_IS_STREAM};
use crate::objects::objtype::objexcept::{
    exception_match, is_exception_instance, is_exception_type, new_exception_msg,
};
use crate::objects::objtype::subclass::is_subclass_fast;
use crate::objects::value::{Obj, Value};
use crate::runtime::runtime::{gen_error_to_rt, ExcRefs, RtError, RuntimeSeams};
use crate::runtime::runtime_call::call_function_n_kw;
use crate::runtime::runtime_iter::{iternext, IterNext};
use crate::runtime::runtime_method::{load_method, load_method_maybe, MethodDest};

/// `mp_resume`'s return: `Yield(v)` = `MP_VM_RETURN_YIELD`,
/// `Normal(v)` = `MP_VM_RETURN_NORMAL` (the generator
/// returned — `v` is the return value, `None` for a bare
/// return), `Exception(v)` = `MP_VM_RETURN_EXCEPTION` (the
/// generator raised `v`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResumeOutcome {
    Yield(Value),
    Normal(Value),
    Exception(Value),
}

/// The gen-instance fast path of `mp_resume`
/// (`mp_obj_gen_resume`): the `gen_resume` seam (C10b's
/// `gen_resume` + C13's runner — the frame and the runner are
/// C13-side). Needs no `ExcRefs`: the `GenResume` /
/// `GenError` mapping is self-contained. `iternext` uses this
/// directly for `Obj::Generator` values.
pub fn resume_gen(
    ctx: &mut CallCtx,
    gen: Value,
    send: Value,
    throw: Option<Value>,
    seams: &mut RuntimeSeams,
) -> Result<ResumeOutcome, RtError> {
    match (seams.gen_resume)(ctx, gen, send, throw) {
        Ok(GenResume {
            kind: ResumeKind::Yield,
            value,
        }) => Ok(ResumeOutcome::Yield(value)),
        Ok(GenResume {
            kind: ResumeKind::Normal,
            value,
        }) => Ok(ResumeOutcome::Normal(value)),
        Ok(GenResume {
            kind: ResumeKind::Exception,
            value,
        }) => Ok(ResumeOutcome::Exception(value)),
        Err(GenError::Raised(v)) => Ok(ResumeOutcome::Exception(v)),
        Err(GenError::StopIteration(sv)) => {
            Ok(ResumeOutcome::Normal(sv.unwrap_or(Value::None)))
        }
        Err(e) => Err(gen_error_to_rt(e, ctx.pool)),
    }
}

/// `mp_resume` (`runtime.zig:1363-1462`).
pub fn resume(
    ctx: &mut CallCtx,
    obj: Value,
    send: Value,
    throw: Option<Value>,
    next_obj: Value,
    exc: &ExcRefs,
    seams: &mut RuntimeSeams,
) -> Result<ResumeOutcome, RtError> {
    // The gen-instance fast path (`mp_obj_gen_resume`).
    if let Value::Heap(r) = obj {
        if matches!(ctx.arena.get(r), Obj::Generator(_)) {
            return resume_gen(ctx, obj, send, throw, seams);
        }
    }
    // The `iternext` fast path: `hasIterNext` and `send == None`
    // (and no throw — the throw path is below). C11 iterators
    // carry header `None` (`new_iter` allocates without a type),
    // so the flag check below cannot see them: an `Obj::Iterator`
    // payload IS iternext-capable by shape (the same payload
    // rule C11's dispatch uses).
    let has_iter_next = match obj {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Iterator(_)) => true,
        _ => match obj_get_type(ctx.arena, &ctx.types, obj) {
            Some(t) => matches!(type_data(ctx.arena, t), Some(d) if d.flags & FLAG_ITER_IS_STREAM != 0),
            None => false,
        },
    };
    if throw.is_none() && send == Value::None && has_iter_next {
        return match iternext(ctx, obj, next_obj, seams)? {
            IterNext::Value(v) => Ok(ResumeOutcome::Yield(v)),
            IterNext::Stop(sv) => Ok(ResumeOutcome::Normal(sv.unwrap_or(Value::None))),
        };
    }
    // The `__next__` path (`send == None`, no throw).
    if throw.is_none() && send == Value::None {
        let next_q = crate::runtime::runtime::dunder(b"__next__")
            .expect("__next__ is a static qstr");
        let dest: MethodDest = match load_method_maybe(ctx, obj, next_q, next_obj)? {
            Some(d) => d,
            None => {
                return Err(RtError::TypeError(
                    "object not an iterator".to_string(),
                ));
            }
        };
        return match crate::runtime::runtime_call::call_method_n_kw(ctx, 0, 0, &dest, &[]) {
            Ok(v) => Ok(ResumeOutcome::Yield(v)),
            // KNOWN GAP #1: a `StopIteration` raised by the
            // method arrives as `PyRaise` (kind erased) and
            // propagates as an error instead of ending the
            // iteration; the arm below fires only for raises
            // that already carry the variant.
            Err(RtError::StopIteration(sv)) => {
                Ok(ResumeOutcome::Normal(sv.unwrap_or(Value::None)))
            }
            Err(RtError::Raised(v)) => Ok(ResumeOutcome::Exception(v)),
            Err(e) => Err(e),
        };
    }
    // The `send` path (`send != None`, no throw): `send` via
    // `load_method` — it RAISES when the method is missing.
    if throw.is_none() {
        let send_q =
            crate::runtime::runtime::dunder(b"send").expect("send is a static qstr");
        let dest = load_method(ctx, obj, send_q, next_obj)?;
        return match crate::runtime::runtime_call::call_method_n_kw(ctx, 1, 0, &dest, &[send]) {
            Ok(v) => Ok(ResumeOutcome::Yield(v)),
            Err(RtError::StopIteration(sv)) => {
                Ok(ResumeOutcome::Normal(sv.unwrap_or(Value::None)))
            }
            Err(RtError::Raised(v)) => Ok(ResumeOutcome::Exception(v)),
            Err(e) => Err(e),
        };
    }
    // The throw path (`send` ignored — the Zig's `send == NULL`).
    let throw_val = throw.expect("throw path entered with throw == Some");
    resume_throw(ctx, obj, throw_val, next_obj, exc)
}

/// The throw tail of `mp_resume` (`runtime.zig:1436-1462`).
fn resume_throw(
    ctx: &mut CallCtx,
    obj: Value,
    throw_val: Value,
    next_obj: Value,
    exc: &ExcRefs,
) -> Result<ResumeOutcome, RtError> {
    // `GeneratorExit` → `close()` via maybe + call → NORMAL.
    // (`is_subclass_fast` takes the thrown object's TYPE as
    // its first argument, exactly as the Zig's
    // `mp_obj_get_type(throw_value)` does.)
    let throw_type = obj_get_type(ctx.arena, &ctx.types, throw_val);
    let is_gen_exit = match throw_type {
        Some(t) => is_subclass_fast(ctx.arena, Value::Heap(t), Value::Heap(exc.generator_exit)),
        None => false,
    };
    if is_gen_exit {
        let close_q =
            crate::runtime::runtime::dunder(b"close").expect("close is a static qstr");
        if let Some(dest) = load_method_maybe(ctx, obj, close_q, next_obj)? {
            let v = crate::runtime::runtime_call::call_method_n_kw(ctx, 0, 0, &dest, &[])?;
            return Ok(ResumeOutcome::Normal(v));
        }
    } else {
        let throw_q =
            crate::runtime::runtime::dunder(b"throw").expect("throw is a static qstr");
        if let Some(dest) = load_method_maybe(ctx, obj, throw_q, next_obj)? {
            let v =
                crate::runtime::runtime_call::call_method_n_kw(ctx, 1, 0, &dest, &[throw_val])?;
            return Ok(ResumeOutcome::Yield(v));
        }
    }
    // Neither found: PEP 479, else `mp_make_raise_obj`.
    if exception_match(ctx.arena, throw_val, Value::Heap(exc.stop_iteration)) {
        let e = new_exception_msg(ctx, exc.runtime_error, "generator raised StopIteration");
        Ok(ResumeOutcome::Exception(e))
    } else {
        let v = make_raise_obj(ctx, throw_val, exc)?;
        Ok(ResumeOutcome::Exception(v))
    }
}

/// `mp_make_raise_obj` (`runtime.zig:1463-1475`): an exception
/// type is called (0 args); an exception instance is returned
/// as-is; anything else → `TypeError("exceptions must derive
/// from BaseException")`.
pub fn make_raise_obj(ctx: &mut CallCtx, o: Value, exc: &ExcRefs) -> Result<Value, RtError> {
    let mut o = o;
    if is_exception_type(ctx.arena, o, exc.base_exception) {
        o = call_function_n_kw(ctx, o, 0, 0, &[])?;
    }
    if is_exception_instance(ctx.arena, o, exc.base_exception) {
        Ok(o)
    } else {
        Err(RtError::TypeError(
            "exceptions must derive from BaseException".to_string(),
        ))
    }
}
