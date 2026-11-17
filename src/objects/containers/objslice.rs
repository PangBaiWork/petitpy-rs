//! slice (C9c: `src/objects/containers/objslice.zig`, 256 lines, 0 test blocks).
//!
//! Full config only (`SLICE_ATTRS=1`): `indices` is reached through
//! the attr slot, not a locals dict (the nano `slice_locals_dict`
//! is out of scope). The type has no binary op, no getiter, no
//! subscr — and its unary op opts out of hashing entirely
//! (objslice.zig:78-82).
//!
//! # The polled-pending helper (read this before touching `indices`)
//!
//! `mpObjSliceIndicesImpl` (objslice.zig:134-180) never polls
//! `pending_exc` after `mp_obj_get_int`: a failed conversion reads
//! back 0 and the FIRST error is the one that survives — EXCEPT a
//! zero step, which `try raiseStepZero()`s and OVERWRITES whatever
//! pending was recorded. Verified on the reference binary:
//!
//! * `slice(1,2,"a").indices(5)` → `ValueError: slice step can't be zero`
//!   (step conversion fails → 0 → step-zero overwrites the
//!   int-conversion pending);
//! * `slice("a",2,3).indices(5)` → `TypeError: can't convert to int`
//!   (start conversion fails → 0, clamped, no step-zero, the first
//!   pending survives);
//! * `slice(1,2).indices("a")` → `TypeError: can't convert to int`
//!   (the LENGTH conversion fails first — recorded, length reads
//!   back 0 — and the impl still runs; step is 1, so nothing
//!   overwrites it).
//!
//! C8a's `slice_indices` (objstr_slice.rs) raises EAGERLY on a
//! failed conversion and must NOT be used for the `indices` method
//! or for range's subscr — both call `slice_indices_polled` here,
//! which threads a local `pending: &mut Option<SliceError>`
//! (Rust has no global pending) through the same algorithm. C8a's
//! helper stays untouched: str/bytes slicing shares the eager
//! divergence (documented in the result file).
//!
//! `Some(Value::None)` counts as a missing bound — the Zig compares
//! each component against `OBJ_NONE`, so an explicit `None` and a
//! missing bound are the same object. `slice_make_new` normalizes
//! explicit `None` args to `None` on construction, and
//! `slice_indices_polled` normalizes again, so a `SliceData` built
//! by C13's `BUILD_SLICE` is handled correctly either way.

use super::objdict::ARG_NUM_TYPES_MISMATCH;
use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{obj_get_int, BuiltinTypes, ObjError};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind, UnaryOp};
use crate::objects::strings::objstr_slice::BoundSlice;
use crate::objects::value::{BoundMethData, FunBuiltinData, GcRef, Obj, SliceData, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// Errors from slice operations, mirroring `DictError` (C9a) /
/// `SetError` (objset.rs): the exception type name + message travel
/// in the enum; the slot boundary drops them to `PyError` (the
/// accepted C6/C7r convention — C13 constructs the exception object
/// from the rich error instead).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceError {
    /// `TypeError(msg)` — arg-count mismatch, non-int bound/length.
    TypeError(&'static str),
    /// `ValueError(msg)` — zero step.
    ValueError(&'static str),
    /// `OverflowError(msg)` — a bound/length outside the machine word.
    OverflowError(&'static str),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary).
    PyRaise,
}

impl SliceError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            SliceError::TypeError(_) => "TypeError",
            SliceError::ValueError(_) => "ValueError",
            SliceError::OverflowError(_) => "OverflowError",
            SliceError::OutOfMemory => "MemoryError",
            SliceError::PyRaise => "",
        }
    }

    /// The exception message.
    pub fn msg(&self) -> Option<&'static str> {
        match self {
            SliceError::TypeError(m)
            | SliceError::ValueError(m)
            | SliceError::OverflowError(m) => Some(m),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — see the
    /// enum docs).
    pub fn to_pyerror(self) -> PyError {
        match self {
            SliceError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<SliceError> for PyError {
    fn from(e: SliceError) -> PyError {
        e.to_pyerror()
    }
}

impl From<ObjError> for SliceError {
    fn from(e: ObjError) -> SliceError {
        match e {
            ObjError::IntConversion => SliceError::TypeError("can't convert to int"),
            ObjError::Overflow => {
                SliceError::OverflowError("overflow converting long int to machine word")
            }
            ObjError::OutOfMemory => SliceError::OutOfMemory,
            _ => SliceError::PyRaise,
        }
    }
}

impl From<PyError> for SliceError {
    fn from(e: PyError) -> SliceError {
        match e {
            PyError::OutOfMemory => SliceError::OutOfMemory,
            PyError::PyRaise => SliceError::PyRaise,
        }
    }
}

/// One `mp_obj_get_int` call inside `slice_indices_polled`: on
/// failure record the FIRST error in `pending` and read back 0
/// (the Zig's `mp_obj_get_int` returns 0 with pending set). While
/// `pending` is already set, `mp_obj_get_int_maybe` returns false
/// WITHOUT converting — its guard (`if (arg == MP_OBJ_NULL or
/// pyerr.pending_exc != 0) return false`, obj.zig:197) — so even a
/// valid int reads back 0. That is why a failed LENGTH conversion
/// makes the step conversion read 0 and raise step-zero, while a
/// `None` step (no conversion) lets the length error survive:
/// `slice(1,2,3).indices("x")` → step-zero,
/// `slice("a",2).indices("x")` → int-conversion (both probed).
fn conv_int(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    v: Value,
    pending: &mut Option<SliceError>,
) -> i64 {
    if pending.is_some() {
        return 0;
    }
    match obj_get_int(ctx, types, v) {
        Ok(n) => n,
        Err(e) => {
            *pending = Some(SliceError::from(e));
            0
        }
    }
}

/// `mpObjSliceIndicesImpl` (objslice.zig:134-180), Zig-faithful
/// polled version: conversion failures do NOT abort — the failed
/// component reads back 0 and the first error is recorded in
/// `pending`; a zero step OVERWRITES it with `ValueError: slice
/// step can't be zero` (the impl `try raiseStepZero()`s after
/// `mp_obj_get_int` returns 0). While `pending` is set, every
/// further conversion reads back 0 too (`mp_obj_get_int_maybe`'s
/// guard, obj.zig:197) — so a step conversion attempted after a
/// failed length conversion raises step-zero, while a `None` step
/// (no conversion) lets the earlier error survive. On a step-zero
/// the impl returns early WITHOUT writing the result, so this
/// returns `(0, 0, 0)` — the value the range subscr's
/// `MpBoundSlice` was initialized to. Callers check `pending`
/// after the call and raise it.
pub(crate) fn slice_indices_polled(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    slice: &SliceData,
    length: i64,
    pending: &mut Option<SliceError>,
) -> BoundSlice {
    // Step first (objslice.zig:137-142).
    let step: i64 = match slice.step {
        None | Some(Value::None) => 1,
        Some(v) => {
            let n = conv_int(ctx, types, v, pending);
            if n == 0 {
                *pending = Some(SliceError::ValueError("slice step can't be zero"));
                return BoundSlice {
                    start: 0,
                    stop: 0,
                    step: 0,
                };
            }
            n
        }
    };
    if step > 0 {
        let start = match slice.start {
            None | Some(Value::None) => 0,
            Some(v) => {
                let mut s = conv_int(ctx, types, v, pending);
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        let stop = match slice.stop {
            None | Some(Value::None) => length,
            Some(v) => {
                let mut s = conv_int(ctx, types, v, pending);
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        BoundSlice { start, stop, step }
    } else {
        let start = match slice.start {
            None | Some(Value::None) => length - 1,
            Some(v) => {
                let mut s = conv_int(ctx, types, v, pending);
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        let stop = match slice.stop {
            None | Some(Value::None) => -1,
            Some(v) => {
                let mut s = conv_int(ctx, types, v, pending);
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        BoundSlice { start, stop, step }
    }
}

/// Extract a cloned `SliceData` from a heap slice value (panics on
/// any other value — the slot/type-data contract).
fn slice_data_of(arena: &GcArena, v: Value) -> SliceData {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Slice(s) => s.clone(),
            _ => panic!("slice op on a non-slice value"),
        },
        _ => panic!("slice op on a non-heap value"),
    }
}

/// `slice_indices` method (`sliceIndicesMethodImpl`,
/// objslice.zig:93-104): the length conversion runs first and its
/// failure is recorded, NOT returned — the impl still runs, and a
/// zero step OVERWRITES the length error. Returns a 3-tuple of
/// small ints (`newSmallInt` in the Zig is a raw shift with no
/// window check; the Rust port uses `Value::int`, which allocates a
/// heap `Int` when the bound leaves the 62-bit window — the Zig's
/// value there is address-dependent garbage, the Rust value is the
/// true bound; documented corner).
pub fn slice_indices_method(
    ctx: &mut CallCtx,
    slice_v: Value,
    length_v: Value,
) -> Result<Value, SliceError> {
    let mut pending: Option<SliceError> = None;
    let length = match obj_get_int(ctx, ctx.types, length_v) {
        Ok(n) => n,
        Err(e) => {
            pending = Some(SliceError::from(e));
            0
        }
    };
    let data = slice_data_of(ctx.arena, slice_v);
    let b = slice_indices_polled(ctx, ctx.types, &data, length, &mut pending);
    if let Some(e) = pending {
        return Err(e);
    }
    let items = [
        Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, b.start),
        Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, b.stop),
        Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, b.step),
    ];
    Ok(Value::Heap(ctx.alloc(
        Some(ctx.types.tuple),
        Obj::Tuple(items.to_vec()),
    )))
}

/// `slice_indices_obj` (`mp_obj_fun_builtin_2`, objslice.zig:111-117):
/// fixed arity 2 — `self` + `length`.
pub fn slice_indices_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() != 2 {
        return Err(SliceError::TypeError(ARG_NUM_TYPES_MISMATCH).to_pyerror());
    }
    slice_indices_method(ctx, args[0], args[1]).map_err(|e| e.to_pyerror())
}

/// `slice_make_new` (`slice(stop)`, `slice(start, stop[, step])`,
/// objslice.zig:188-194): `n_kw != 0` or fewer than 1 / more than 3
/// args raises `TypeError: slice expected 1 to 3 arguments`
/// (direct raise, no argcheck). One arg → `(None, arg, None)`.
/// An explicit `None` arg is normalized to a missing bound — the
/// Zig stores `OBJ_NONE` and the impl compares against it, so the
/// two are the same object.
pub fn slice_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, SliceError> {
    if n_kw != 0 || args.is_empty() || args.len() > 3 {
        return Err(SliceError::TypeError(
            "slice expected 1 to 3 arguments",
        ));
    }
    let norm = |v: Value| -> Option<Value> {
        if v == Value::None {
            None
        } else {
            Some(v)
        }
    };
    let (start, stop, step) = if args.len() == 1 {
        (None, norm(args[0]), None)
    } else if args.len() == 2 {
        (norm(args[0]), norm(args[1]), None)
    } else {
        (norm(args[0]), norm(args[1]), norm(args[2]))
    };
    Ok(Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Slice(SliceData { start, stop, step }),
    )))
}

/// `slice_unary_op` (objslice.zig:78-82): NULL for every op —
/// explicitly opts out of the default `__hash__`.
pub fn slice_unary_op(_ctx: &mut CallCtx, _op: UnaryOp, _self_v: Value) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `slice_print` (objslice.zig:56-66): `slice(` + repr(start) +
/// `, ` + repr(stop) + `, ` + repr(step) + `)`. A missing bound
/// prints as `None` (the Zig prints the stored `OBJ_NONE`).
pub fn slice_print(ctx: &mut CallCtx, v: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let data = slice_data_of(ctx.arena, v);
    let _ = sink.write_str("slice(");
    print_bound(ctx, data.start, sink);
    let _ = sink.write_str(", ");
    print_bound(ctx, data.stop, sink);
    let _ = sink.write_str(", ");
    print_bound(ctx, data.step, sink);
    let _ = sink.write_str(")");
}

/// One bound of a slice print: missing → `None`, else PRINT_REPR.
fn print_bound(ctx: &mut CallCtx, b: Option<Value>, sink: &mut dyn std::fmt::Write) {
    match b {
        None => {
            let _ = sink.write_str("None");
        }
        Some(v) => obj_print_helper(ctx, ctx.types, v, PrintKind::Repr, sink),
    }
}

/// `slice_attr` (objslice.zig:118-124, full config): `start` /
/// `stop` / `step` return the stored bound (`None` reads back as
/// the value `None`, NOT as "no such attr"); `indices` returns a
/// bound method (fun + self, mirroring `dest[0]=fun, dest[1]=self`
/// in the Zig). Any other attr returns `None` (not found — C12
/// tries the next lookup).
pub fn slice_attr(ctx: &mut CallCtx, slice_v: Value, attr: u32) -> Option<Value> {
    let data = slice_data_of(ctx.arena, slice_v);
    if attr == static_qstr_id(b"start").expect("MP_QSTR_start is in the static table") {
        return Some(data.start.unwrap_or(Value::None));
    }
    if attr == static_qstr_id(b"stop").expect("MP_QSTR_stop is in the static table") {
        return Some(data.stop.unwrap_or(Value::None));
    }
    if attr == static_qstr_id(b"step").expect("MP_QSTR_step is in the static table") {
        return Some(data.step.unwrap_or(Value::None));
    }
    if attr == static_qstr_id(b"indices").expect("MP_QSTR_indices is in the static table") {
        let fun = ctx.alloc(
            None,
            Obj::FunBuiltin(FunBuiltinData {
                name: attr,
                n_args: fun_n_args(2, 2),
                func: slice_indices_builtin,
                // Fixed-arity builtin: no keyword parameters (C10a).
                kw_func: None,
            }),
        );
        return Some(Value::Heap(ctx.alloc(
            None,
            Obj::BoundMeth(BoundMethData {
                func: Value::Heap(fun),
                self_: slice_v,
            }),
        )));
    }
    None
}

/// `mp_type_slice` ROM data (objslice.zig): flags `0x0000`; slots
/// unary_op + print only (attr is a standalone fn for C12 —
///
/// `TypeData` has no `attr` field by the frozen C0 design).
pub fn slice_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"slice").expect("MP_QSTR_slice is in the static table"),
        flags: 0x0000,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(slice_unary_op),
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(slice_print),
    }
}
