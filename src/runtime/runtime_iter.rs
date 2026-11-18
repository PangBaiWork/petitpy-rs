//! Iteration (`runtime.zig:1277-1362`): `mp_getiter`,
//! `mp_iternext`, `mp_iternext_allow_raise`; unpack
//! (`runtime.zig:1055-1171`): `mp_unpack_sequence`,
//! `mp_unpack_ex`.
//!
//! `mp_getiter` dispatches on the type: `iterIsIdentity`
//! (`FLAG_ITER_IS_ITERNEXT`, 0x0080) → the object itself; the
//! type's `getiter` slot (a `PyRaise` from it falls through —
//! the C6 convention; `OutOfMemory` propagates); `__getitem__`
//! via `load_method_maybe` → C11's `new_getitem_iter` (the
//! Zig stores the BOUND method eagerly; the Rust seam loads it
//! lazily at each `next` — C11's documented divergence, same
//! observable `next()` behaviour); else `TypeError("object not
//! iterable")`.
//!
//! `mp_iternext` / `mp_iternext_allow_raise` dispatch on the
//! payload first (`Obj::Iterator` → C11's tag dispatch;
//! `Obj::Generator` → `mp_resume` with `send = None`), then
//! the `__next__` method path. The Zig checks `hasIterNext`
//! (flags & 0x0180) first and calls the type's `iternext` slot;
//! `TypeData` has no `iternext` slot, so the payload dispatch
//! IS the Rust spelling of that arm (no value in the current
//! tree carries the flag with any other payload). The
//! `__next__` path translates `StopIteration` → `Stop` in
//! plain `iternext`, but the exception object is unobservable
//! through the call slot until C13's `pending_exc` — KNOWN GAP
//! #1: a user iterator's `__next__` raising `StopIteration`
//! propagates as `RtError::PyRaise` instead of ending the
//! iteration (native-iterator and generator paths are
//! unaffected). No pending pre-flight lives here (`CallCtx`
//! has no pending field; the VM checks first — the C9c rule
//! at VM level), even though the Zig's `mp_iternext` export
//! wrapper has one.
//!
//! The unpack functions return their items in NATURAL order;
//! the Zig's `items` array is reversed (a VM stack-layout
//! detail — the VM pops them in order), which is not
//! observable through this API.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objlist::new_list;
use crate::objects::iters::iter::{
    iternext as iter_iternext, CallOutcome, IterError, IterSeams,
};
use crate::objects::iters::objgetitemiter::new_getitem_iter;
use crate::objects::obj::{obj_get_type, type_data, ObjError, FLAG_ITER_IS_ITERNEXT};
use crate::objects::obj_ops::obj_get_array;
use crate::objects::value::{Obj, Value};
use crate::pyerr::PyError;
use crate::runtime::runtime::{dunder, RtError, RuntimeSeams};
use crate::runtime::runtime_method::{load_method_maybe, MethodDest};
use crate::runtime::runtime_call::{call_function_n_kw, call_method_n_kw};
use crate::runtime::runtime_resume::{resume_gen, ResumeOutcome};

/// `mp_iternext` / `mp_iternext_allow_raise` outcome:
/// `Value` = the next item; `Stop(v)` = exhaustion, `v` the
/// `StopIteration` value (`None` = bare — the Zig's
/// `raiseStopIter` distinguishes NULL from a value, and the
/// probes confirm bare SI args `()` vs generator `return 99`
/// → args `(99,)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IterNext {
    Value(Value),
    Stop(Option<Value>),
}

/// Map a C11 `IterError` onto `RtError`. `IndexError` has no
/// `RtError` variant — it degrades to `PyRaise` (the same
/// kind-erasure the `ObjError` boundary already applies);
/// `iternext` never produces it in practice.
fn iter_err_to_rt(ctx: &CallCtx, e: IterError) -> RtError {
    match e {
        IterError::TypeError(m) => RtError::TypeError(m.to_string()),
        IterError::ValueError(m) => RtError::ValueError(m.to_string()),
        IterError::OutOfMemory => RtError::OutOfMemory,
        IterError::PyRaise => RtError::PyRaise,
        IterError::Obj(ObjError::OutOfMemory) => RtError::OutOfMemory,
        IterError::Obj(_) => RtError::PyRaise,
        IterError::IndexError(_) => RtError::PyRaise,
        IterError::UnexpectedKwArg(q) => RtError::TypeError(format!(
            "unexpected keyword argument '{}'",
            String::from_utf8_lossy(ctx.pool.qstr_str(q as u16))
        )),
        IterError::MultipleValues(q) => RtError::TypeError(format!(
            "function got multiple values for argument '{}'",
            String::from_utf8_lossy(ctx.pool.qstr_str(q as u16))
        )),
        IterError::WrongArgCount { expected, got } => RtError::TypeError(format!(
            "function takes {expected} positional arguments but {got} were given"
        )),
    }
}

/// `mp_getiter` (`runtime.zig:1277-1300`).
pub fn getiter(
    ctx: &mut CallCtx,
    value: Value,
    next_obj: Value,
    _seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    if let Value::Heap(r) = value {
        if matches!(ctx.arena.get(r), Obj::Iterator(_)) {
            return Ok(value);
        }
    }
    let Some(t) = obj_get_type(ctx.arena, &ctx.types, value) else {
        // B2 defensive: NULL type means the value was the
        // `MP_OBJ_NULL` sentinel from a polled raise.
        return Err(RtError::PyRaise);
    };
    let (flags, getiter_fn) = match type_data(ctx.arena, t) {
        Some(d) => (d.flags, d.getiter),
        None => return Err(RtError::PyRaise),
    };
    if flags & FLAG_ITER_IS_ITERNEXT != 0 {
        return Ok(value);
    }
    if let Some(f) = getiter_fn {
        match f(ctx, value) {
            Ok(v) => return Ok(v),
            Err(PyError::OutOfMemory) => return Err(RtError::OutOfMemory),
            Err(PyError::PyRaise) => {}
        }
    }
    let getitem_q = dunder(b"__getitem__").expect("__getitem__ is a static qstr");
    if load_method_maybe(ctx, value, getitem_q, next_obj)?.is_some() {
        return Ok(new_getitem_iter(ctx, value));
    }
    Err(RtError::TypeError("object not iterable".to_string()))
}

/// The shared `__next__`-method path of `mp_iternext` /
/// `mp_iternext_allow_raise` (`runtime.zig:1304-1361`):
/// load `__next__` (which may be the `next`-builtin fast
/// path), call it `(0, 0)`; missing → `TypeError("object not
/// an iterator")`.
fn iternext_via_dunder_next(
    ctx: &mut CallCtx,
    it: Value,
    next_obj: Value,
    _seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let next_q = dunder(b"__next__").expect("__next__ is a static qstr");
    let dest: MethodDest = match load_method_maybe(ctx, it, next_q, next_obj)? {
        Some(d) => d,
        None => {
            return Err(RtError::TypeError(
                "object not an iterator".to_string(),
            ));
        }
    };
    call_method_n_kw(ctx, 0, 0, &dest, &[])
}

/// `mp_iternext` (`runtime.zig:1326-1361`): the next item, or
/// `Stop` at exhaustion. A `StopIteration` raised by a
/// `__next__` METHOD is unobservable pre-C13 (KNOWN GAP #1);
/// the native-iterator and generator paths translate it
/// exactly.
pub fn iternext(
    ctx: &mut CallCtx,
    it: Value,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<IterNext, RtError> {
    match it {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Iterator(_)) => {
            // The C11 `IterSeams` from this lane's call fns.
            // `call`: `call_function_n_kw` — a `StopIteration`
            // raise from the callee ends the iteration
            // (`CallOutcome::Stop`); any other raise → `Raise`
            // (an `OutOfMemory` degrades to `Raise` here — the
            // seam type has no OOM outcome). `getitem`: the
            // bound `__getitem__` call; every error → `Raise`
            // (KNOWN GAP #2 — the kind is erased at the call
            // slot, so `IndexError` / `StopIteration` from a
            // real `__getitem__` cannot end the iteration
            // cleanly until C13).
            let mut call_c = |ctx: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
                match call_function_n_kw(ctx, fun, args.len(), 0, args) {
                    Ok(v) => CallOutcome::Value(v),
                    Err(RtError::StopIteration(_)) => CallOutcome::Stop,
                    Err(_) => CallOutcome::Raise,
                }
            };
            let mut getitem_c = |ctx: &mut CallCtx, obj: Value, index: i64| -> CallOutcome {
                match getitem_call(ctx, obj, index, next_obj) {
                    Ok(v) => CallOutcome::Value(v),
                    Err(_) => CallOutcome::Raise,
                }
            };
            // E2c: the `next` seam — a generator sub-iterator
            // of a map/filter/zip/enumerate resumes through
            // `resume_gen` (the `mp_resume` fast path) instead
            // of raising "object not an iterator". A raise
            // inside the generator degrades to `Raise` here
            // (the C14 seam type carries no exception value —
            // same limitation as the `call` seam above).
            let mut next_c = |ctx: &mut CallCtx, sub: Value| -> CallOutcome {
                match resume_gen(ctx, sub, Value::None, None, seams) {
                    Ok(ResumeOutcome::Yield(v)) => CallOutcome::Value(v),
                    Ok(ResumeOutcome::Normal(_)) => CallOutcome::Stop,
                    Ok(ResumeOutcome::Exception(_)) => CallOutcome::Raise,
                    Err(_) => CallOutcome::Raise,
                }
            };
            let mut s = IterSeams {
                call: &mut call_c,
                getitem: &mut getitem_c,
                next: Some(&mut next_c),
                // No `next` builtin here: the C14 `next`
                // object is the bare adapter (stub seams —
                // `next(gen)` through it returns `PyRaise`),
                // so the `next` closure above is the only
                // resume path (E2c).
                next_builtin: None,
            };
            match iter_iternext(ctx, it, &mut s) {
                Ok(Some(v)) => Ok(IterNext::Value(v)),
                Ok(None) => Ok(IterNext::Stop(None)),
                Err(e) => Err(iter_err_to_rt(ctx, e)),
            }
        }
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Generator(_)) => {
            match resume_gen(ctx, it, Value::None, None, seams)? {
                ResumeOutcome::Yield(v) => Ok(IterNext::Value(v)),
                ResumeOutcome::Normal(v) => Ok(IterNext::Stop(Some(v).filter(|x| *x != Value::None))),
                ResumeOutcome::Exception(v) => Err(RtError::Raised(v)),
            }
        }
        _ => {
            // `__next__` via method load; a `StopIteration`
            // raise is unobservable here (KNOWN GAP #1).
            match iternext_via_dunder_next(ctx, it, next_obj, seams) {
                Ok(v) => Ok(IterNext::Value(v)),
                Err(e) => Err(e),
            }
        }
    }
}

/// `mp_iternext_allow_raise` (`runtime.zig:1304-1324`): like
/// `iternext`, but a raise from `__next__` propagates as-is
/// (no `StopIteration` translation even were it observable).
pub fn iternext_allow_raise(
    ctx: &mut CallCtx,
    it: Value,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<IterNext, RtError> {
    match it {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Iterator(_)) => {
            iternext(ctx, it, next_obj, seams)
        }
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Generator(_)) => {
            match resume_gen(ctx, it, Value::None, None, seams)? {
                ResumeOutcome::Yield(v) => Ok(IterNext::Value(v)),
                ResumeOutcome::Normal(v) => Ok(IterNext::Stop(Some(v).filter(|x| *x != Value::None))),
                ResumeOutcome::Exception(v) => Err(RtError::Raised(v)),
            }
        }
        _ => match iternext_via_dunder_next(ctx, it, next_obj, seams) {
            Ok(v) => Ok(IterNext::Value(v)),
            Err(e) => Err(e),
        },
    }
}

/// The `getitem` seam's call: load `__getitem__` and call it
/// with the index. Every error → `Err` (the caller maps it
/// to `CallOutcome::Raise` — KNOWN GAP #2).
fn getitem_call(
    ctx: &mut CallCtx,
    obj: Value,
    index: i64,
    next_obj: Value,
) -> Result<Value, RtError> {
    let getitem_q = dunder(b"__getitem__").expect("__getitem__ is a static qstr");
    let dest = match load_method_maybe(ctx, obj, getitem_q, next_obj)? {
        Some(d) => d,
        None => return Err(RtError::PyRaise),
    };
    let idx = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, index);
    call_method_n_kw(ctx, 1, 0, &dest, &[idx])
}

// ── unpack ──────────────────────────────────────────────────────────

/// `unpackErr` (`runtime.zig:232-234`).
fn unpack_err() -> RtError {
    RtError::ValueError("wrong number of values to unpack".to_string())
}

/// `mp_unpack_sequence` (`runtime.zig:1055-1081`): exactly
/// `num` items, in natural order. Exact tuple/list fast path
/// via `obj_get_array`; otherwise `getiter` + `iternext`
/// (exhaustion → `unpackErr`; one extra item → `unpackErr`).
pub fn unpack_sequence(
    ctx: &mut CallCtx,
    seq: Value,
    num: usize,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<Vec<Value>, RtError> {
    match obj_get_array(ctx, &ctx.types, seq) {
        Ok(items) => {
            if items.len() != num {
                return Err(unpack_err());
            }
            Ok(items)
        }
        Err(_) => {
            let iterable = getiter(ctx, seq, next_obj, seams)?;
            let mut items = Vec::with_capacity(num);
            for _ in 0..num {
                match iternext(ctx, iterable, next_obj, seams)? {
                    IterNext::Value(v) => items.push(v),
                    IterNext::Stop(_) => return Err(unpack_err()),
                }
            }
            match iternext(ctx, iterable, next_obj, seams)? {
                IterNext::Value(_) => Err(unpack_err()),
                IterNext::Stop(_) => Ok(items),
            }
        }
    }
}

/// `mp_unpack_ex` (`runtime.zig:1083-1124`) outcome: the left
/// items, the rest list, and the right items, all in natural
/// order. `num_in` packs `(num_right << 8) | num_left`.
#[derive(Clone, Debug, PartialEq)]
pub struct UnpackEx {
    pub left: Vec<Value>,
    pub right: Vec<Value>,
    pub rest: Value,
}

pub fn unpack_ex(
    ctx: &mut CallCtx,
    seq: Value,
    num_in: usize,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<UnpackEx, RtError> {
    let num_left = num_in & 0xff;
    let num_right = (num_in >> 8) & 0xff;
    match obj_get_array(ctx, &ctx.types, seq) {
        Ok(items) => {
            if items.len() < num_left + num_right {
                return Err(unpack_err());
            }
            let left = items[..num_left].to_vec();
            let rest_items = &items[num_left..items.len() - num_right];
            let rest = Value::Heap(new_list(ctx, ctx.types.list, rest_items));
            let right = items[items.len() - num_right..].to_vec();
            Ok(UnpackEx { left, right, rest })
        }
        Err(_) => {
            let iterable = getiter(ctx, seq, next_obj, seams)?;
            let mut left = Vec::with_capacity(num_left);
            for _ in 0..num_left {
                match iternext(ctx, iterable, next_obj, seams)? {
                    IterNext::Value(v) => left.push(v),
                    IterNext::Stop(_) => return Err(unpack_err()),
                }
            }
            let mut rest_items = Vec::new();
            loop {
                match iternext(ctx, iterable, next_obj, seams)? {
                    IterNext::Value(v) => rest_items.push(v),
                    IterNext::Stop(_) => break,
                }
            }
            if rest_items.len() < num_right {
                return Err(unpack_err());
            }
            let split = rest_items.len() - num_right;
            let right = rest_items[split..].to_vec();
            let rest = Value::Heap(new_list(ctx, ctx.types.list, &rest_items[..split]));
            Ok(UnpackEx { left, right, rest })
        }
    }
}
