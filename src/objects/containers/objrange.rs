//! range (C9c: `src/objects/containers/objrange.zig`, 310 lines, 0 test blocks).
//!
//! Full config: NO binary op (`RANGE_BINOP=0` under full —
//! `range(5) == range(5)` is False, `r == r` is True via the
//! identity fallback; the nano `range_binary_op` (equality only)
//! is out of scope), attr live (`start`/`stop`/`step`), NO locals
//! dict (`range(10).index` → `AttributeError: no attribute
//! 'index'` — verified on the reference binary). Boundaries are
//! `Int = isize` (objrange.zig), so the i64 `RangeData` stub is
//! correct as-is; no big-int rework.
//!
//! # Wrapping arithmetic
//!
//! `rangeLenFromParts` and the subscr/iterator arithmetic use
//! Zig wrapping operators (`-%`, `+%`, `*%`); the Rust port uses
//! `wrapping_*` throughout. Two corners where the Zig aborts and
//! the Rust port does not (documented intentional divergences):
//!
//! * A step of 0 can only arise from a wrapped slice product —
//!   `range(0, 10, 2**32)[::2**32]` is `range(0, 4294967296, 0)`
//!   (verified: the object itself prints fine). Its `len()`
//!   divides by zero and CRASHES the reference binary;
//!   `range_len` returns 0 instead.
//! * `@divTrunc(isize::MIN, -1)` overflows: `range_len` uses
//!   `wrapping_div` (returns MIN, then clamped to 0 by the
//!   `len < 0` rule) where the Zig would abort. Reachable only
//!   for `range(isize::MIN, 0, -1)`-shaped ranges.

use super::objdict::ARG_NUM_TYPES_MISMATCH;
use super::objslice::{slice_indices_polled, SliceError};
use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::obj_ops::get_index;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{GcRef, IteratorData, Obj, RangeData, SliceData, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// Errors from range operations, mirroring `DictError` (C9a) /
/// `SetError` (objset.rs): the exception type name + message travel
/// in the enum; the slot boundary drops them to `PyError` (the
/// accepted C6/C7r convention — C13 constructs the exception object
/// from the rich error instead).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    /// `TypeError(msg)` — arg-count mismatch, non-int bound/index.
    TypeError(&'static str),
    /// `ValueError(msg)` — zero step.
    ValueError(&'static str),
    /// `IndexError(msg)` — int index out of range.
    IndexError(&'static str),
    /// `OverflowError(msg)` — a bound outside the machine word.
    OverflowError(&'static str),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary).
    PyRaise,
}

impl RangeError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            RangeError::TypeError(_) => "TypeError",
            RangeError::ValueError(_) => "ValueError",
            RangeError::IndexError(_) => "IndexError",
            RangeError::OverflowError(_) => "OverflowError",
            RangeError::OutOfMemory => "MemoryError",
            RangeError::PyRaise => "",
        }
    }

    /// The exception message.
    pub fn msg(&self) -> Option<&'static str> {
        match self {
            RangeError::TypeError(m)
            | RangeError::ValueError(m)
            | RangeError::IndexError(m)
            | RangeError::OverflowError(m) => Some(m),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — see the
    /// enum docs).
    pub fn to_pyerror(self) -> PyError {
        match self {
            RangeError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for RangeError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.msg().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.to_pyerror()
    }
}

impl From<ObjError> for RangeError {
    fn from(e: ObjError) -> RangeError {
        match e {
            ObjError::IntConversion => RangeError::TypeError("can't convert to int"),
            ObjError::Overflow => {
                RangeError::OverflowError("overflow converting long int to machine word")
            }
            ObjError::IndexOutOfRange => RangeError::IndexError("index out of range"),
            ObjError::IndicesMustBeInts => RangeError::TypeError("indices must be integers"),
            ObjError::OutOfMemory => RangeError::OutOfMemory,
            _ => RangeError::PyRaise,
        }
    }
}

impl From<PyError> for RangeError {
    fn from(e: PyError) -> RangeError {
        match e {
            PyError::OutOfMemory => RangeError::OutOfMemory,
            PyError::PyRaise => RangeError::PyRaise,
        }
    }
}

impl From<SliceError> for RangeError {
    fn from(e: SliceError) -> RangeError {
        match e {
            SliceError::TypeError(m) => RangeError::TypeError(m),
            SliceError::ValueError(m) => RangeError::ValueError(m),
            SliceError::OverflowError(m) => RangeError::OverflowError(m),
            SliceError::OutOfMemory => RangeError::OutOfMemory,
            SliceError::PyRaise => RangeError::PyRaise,
        }
    }
}

/// Extract a range's `(start, stop, step)` (panics on any other
/// value — the slot/type-data contract).
fn range_data_of(arena: &GcArena, v: Value) -> (i64, i64, i64) {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Range(d) => (d.start, d.stop, d.step),
            _ => panic!("range op on a non-range value"),
        },
        _ => panic!("range op on a non-heap value"),
    }
}

/// `rangeLenFromParts` (objrange.zig:72-78): wrapping arithmetic
/// throughout. `step == 0` returns 0 (see the module docs — the
/// Zig divides by zero and aborts there).
pub fn range_len(start: i64, stop: i64, step: i64) -> i64 {
    if step == 0 {
        return 0;
    }
    let mut len = stop.wrapping_sub(start).wrapping_add(step);
    if step > 0 {
        len = len.wrapping_sub(1);
    } else {
        len = len.wrapping_add(1);
    }
    // `@divTrunc`: truncating division. `wrapping_div` returns MIN
    // for `MIN / -1` where the Zig would abort (see module docs).
    let len = len.wrapping_div(step);
    if len < 0 {
        0
    } else {
        len
    }
}

/// `range_make_new` (`rngFnMakeNewImpl`, objrange.zig:132-160):
/// arg sig (1, 3, no kwargs) — `mp_arg_check_num_sig` is a bare
/// statement (return ignored): the mismatch is recorded and the
/// first conversion reads it back via `mp_obj_get_int_maybe`'s
/// guard and returns early, so the observable is the mismatch and
/// it is returned directly. Conversions run in order with a pending
/// check after each (first pending wins); `step == 0` raises
/// `ValueError: zero step` (only reachable with no pending).
/// One arg → `stop`; two → `start, stop`; three → `+ step`.
pub fn range_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, RangeError> {
    if n_kw != 0 || args.is_empty() || args.len() > 3 {
        return Err(RangeError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let (start, stop, step) = if args.len() == 1 {
        (
            0,
            obj_get_int(ctx, ctx.types, args[0]).map_err(RangeError::from)?,
            1,
        )
    } else {
        let start = obj_get_int(ctx, ctx.types, args[0]).map_err(RangeError::from)?;
        let stop = obj_get_int(ctx, ctx.types, args[1]).map_err(RangeError::from)?;
        let step = if args.len() == 3 {
            obj_get_int(ctx, ctx.types, args[2]).map_err(RangeError::from)?
        } else {
            1
        };
        if step == 0 {
            return Err(RangeError::ValueError("zero step"));
        }
        (start, stop, step)
    };
    Ok(Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Range(RangeData { start, stop, step }),
    )))
}

/// `npy_rng_fn_unary_op` (objrange.zig:165-170): BOOL → `len > 0`,
/// LEN → `len` (`mp_obj_new_int` — a heap `Int` when the length
/// leaves the 62-bit window); anything else → NULL.
pub fn range_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_v: Value) -> PyResult<Value> {
    let (start, stop, step) = range_data_of(ctx.arena, self_v);
    let len = range_len(start, stop, step);
    match op {
        UnaryOp::Bool => Ok(Value::Bool(len > 0)),
        UnaryOp::Len => Ok(Value::int(
            ctx.arena,
            Some(ctx.types.int),
            ctx.roots,
            len,
        )),
        _ => Err(PyError::PyRaise),
    }
}

/// `npy_rng_fn_subscr` (objrange.zig:172-196): get only — set/delete
/// → NULL. A slice index builds a new range via
/// `mp_obj_slice_indices` (the Zig-faithful polled helper here):
/// `start = self.start +% s.start *% self.step`,
/// `stop = self.start +% s.stop *% self.step`,
/// `step = s.step *% self.step`; the new range's type is hardcoded
/// to `range`. A failed slice conversion leaves the bound slice at
/// its initialized `(0, 0, 0)` and the pending error is returned
/// (the Zig's `mp_obj_slice_indices` is a `bridge(void)` — the VM
/// raises the pending after the slot returns). An int index goes
/// through `mp_get_index` (`obj_ops::get_index`) and returns
/// `self.start +% idx *% self.step`.
pub fn range_subscr(
    ctx: &mut CallCtx,
    self_v: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let SubscrArg::Get = arg else {
        return Err(PyError::PyRaise);
    };
    let (start, _stop, step) = range_data_of(ctx.arena, self_v);
    let len = range_len(start, _stop, step);
    // Slice index? The Zig checks `isExactType(index, &mp_type_slice)`;
    // the slice type is not registered in `BuiltinTypes` (C12 owns
    // type creation), so the Rust port checks the payload instead —
    // a slice SUBCLASS instance (C12) would take the slice branch
    // here where the Zig falls through to `mp_get_index`. Recorded
    // for C12.
    let slice_data: Option<SliceData> = match index {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Slice(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(slice_data) = slice_data {
        let mut pending: Option<SliceError> = None;
        let types = ctx.types;
        let b = slice_indices_polled(ctx, types, &slice_data, len, &mut pending);
        let new_range = ctx.alloc(
            Some(ctx.types.range),
            Obj::Range(RangeData {
                start: start.wrapping_add(b.start.wrapping_mul(step)),
                stop: start.wrapping_add(b.stop.wrapping_mul(step)),
                step: b.step.wrapping_mul(step),
            }),
        );
        if let Some(e) = pending {
            return Err(ctx.record_typed(RangeError::from(e)));
        }
        return Ok(Value::Heap(new_range));
    }
    let idx = get_index(ctx, ctx.types, len as usize, index, false)
        .map_err(|e| ctx.record_typed(RangeError::from(e)))?;
    Ok(Value::int(
        ctx.arena,
        Some(ctx.types.int),
        ctx.roots,
        start.wrapping_add((idx as i64).wrapping_mul(step)),
    ))
}

/// `range_print` (objrange.zig:89-98): `range(%ld, %ld` + `)` if
/// `step == 1`, else `, %ld)`. `%ld` prints the full decimal; Rust
/// `{}` does the same (no 32-char truncation applies — base-10
/// `i64` is at most 20 digits).
pub fn range_print(ctx: &mut CallCtx, v: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let (start, stop, step) = range_data_of(ctx.arena, v);
    let _ = write!(sink, "range({start}, {stop}");
    if step == 1 {
        let _ = sink.write_str(")");
    } else {
        let _ = write!(sink, ", {step})");
    }
}

/// `range_attr` (objrange.zig:101-107, full config): `start` /
/// `stop` / `step` return `mp_obj_new_int(...)` — a heap `Int`
/// when the bound leaves the 62-bit window. Any other attr returns
/// `None` (not found — C12 tries the next lookup). Standalone fn
/// for C12 (`TypeData` has no `attr` field by the frozen C0 design).
pub fn range_attr(ctx: &mut CallCtx, range_v: Value, attr: u32) -> Option<Value> {
    let (start, stop, step) = range_data_of(ctx.arena, range_v);
    if attr == static_qstr_id(b"start").expect("MP_QSTR_start is in the static table") {
        return Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, start));
    }
    if attr == static_qstr_id(b"stop").expect("MP_QSTR_stop is in the static table") {
        return Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, stop));
    }
    if attr == static_qstr_id(b"step").expect("MP_QSTR_step is in the static table") {
        return Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, step));
    }
    None
}

/// `npy_rng_fn_getiter` (objrange.zig:207-210): a fresh
/// `Obj::Iterator` over the range (no `iter_buf` reuse in Rust).
/// The iterator stores `cur` in `index` (bit-cast) and
/// `stop`/`step` as `Value::int` results in `extra` (a heap `Int`
/// when out of the 62-bit window). Header type `None` until C12
/// (the dict-view precedent); the type's flags are `0x0080`
/// (`ITER_IS_ITERNEXT`, recorded for C11/C12). `self_v` is rooted
/// in `ctx.roots.extra` across the allocations (D6: every Value
/// held across an alloc must be rooted — `alloc_inner` can collect
/// under the stress rule).
pub fn range_getiter(ctx: &mut CallCtx, self_v: Value) -> PyResult<Value> {
    let (start, stop, step) = range_data_of(ctx.arena, self_v);
    let mark = ctx.roots.extra.len();
    ctx.roots.extra.push(self_v);
    let stop_v = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, stop);
    let step_v = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, step);
    let it = ctx.alloc(
        None,
        Obj::Iterator(IteratorData {
            source: self_v,
            index: start as usize,
            extra: vec![stop_v, step_v],
        }),
    );
    ctx.roots.extra.truncate(mark);
    Ok(Value::Heap(it))
}

/// `npy_rng_fn_it_iternext` (objrange.zig:122-130): `step > 0 and
/// cur < stop`, or `step < 0 and cur > stop` → yield `cur`, then
/// `cur +%= step`; else `OBJ_STOP_ITERATION` (`None` here — the
/// `dict_view_it_next` convention). The iterator is rooted across
/// the `Value::int` allocation (D6) and its stored `index` is
/// updated in place (`get_mut`, the `set_it_next` pattern).
pub fn range_it_next(ctx: &mut CallCtx, iter_v: Value) -> Result<Option<Value>, RangeError> {
    let (_source, index, stop_v, step_v) = match iter_v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(it) if it.extra.len() == 2 => {
                (it.source, it.index, it.extra[0], it.extra[1])
            }
            _ => panic!("range_it_next on a non-range-iterator value"),
        },
        _ => panic!("range_it_next on a non-heap value"),
    };
    let cur = index as i64;
    let stop = obj_get_int(ctx, ctx.types, stop_v).map_err(RangeError::from)?;
    let step = obj_get_int(ctx, ctx.types, step_v).map_err(RangeError::from)?;
    if (step > 0 && cur < stop) || (step < 0 && cur > stop) {
        let mark = ctx.roots.extra.len();
        ctx.roots.extra.push(iter_v);
        let out = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, cur);
        ctx.roots.extra.truncate(mark);
        if let Value::Heap(r) = iter_v {
            if let Obj::Iterator(it) = ctx.arena.get_mut(r) {
                it.index = cur.wrapping_add(step) as usize;
            }
        }
        Ok(Some(out))
    } else {
        Ok(None)
    }
}

/// `mp_type_range` ROM data (objrange.zig, full config): flags
/// `0x0000`; slots unary_op + subscr + getiter + print (NO
/// binary_op under full — see the module docs). `make_new` is the
/// type-call path, wired by C12, not a `TypeData` slot (the
/// `dict_type_data` precedent).
pub fn range_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"range").expect("MP_QSTR_range is in the static table"),
        flags: 0x0000,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(range_unary_op),
        binary_op: None,
        call: None,
        getiter: Some(range_getiter),
        subscr: Some(range_subscr),
        buffer: None,
        print: Some(range_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}
