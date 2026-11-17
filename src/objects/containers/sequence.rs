//! `sequence.zig` — the shared sequence helpers (`py/sequence.c`).
//!
//! Ported from `src/objects/containers/sequence.zig` (192 lines,
//! 5 class-A exports). Only the helpers C8a did NOT already cover:
//! `mp_seq_cmp_bytes` and `mp_seq_get_fast_slice_indexes` live in
//! C8a's `objstr_a.rs` / `objstr_methods_c.rs` (`seq_cmp_bytes`,
//! `fast_slice_indexes`) and are imported by the lanes that need
//! them — this file adds only the missing `mp_seq_*`:
//! `mp_seq_multiply`, `mp_seq_cmp_objs`, `mp_seq_index_obj`,
//! `mp_seq_count_obj`, `mp_seq_extract_slice`.
//!
//! Note on the header: `MICROPY_PY_BUILTINS_SLICE=0` ⇒ the two
//! slice fns are `#if`-dead in vendor C, but the Zig ports them
//! from `sequence_shim.c` (added in 0.2) — and C8a ported them.
//!
//! Faithful points:
//! - `mp_seq_cmp_objs`: EQUAL + length mismatch → false;
//!   LESS/LESS_EQUAL swap operands into MORE/MORE_EQUAL; the first
//!   unequal element decides (`binopIsTrue` is `== OBJ_TRUE`
//!   exactly — a truthy non-bool does NOT count); the tail is the
//!   same prefix/length rule as `seq_cmp_bytes`.
//! - `mp_seq_index_obj`: `start`/`stop` resolve through
//!   `mp_get_index(..., is_slice=true)` (clamping); a miss raises
//!   `ValueError("object not in sequence")`.
//! - `mp_seq_extract_slice` builds a LIST (here: a `Vec` the
//!   caller wraps); `step < 0` loops `start >= stop`, else
//!   `start < stop`.
//! - `mp_seq_multiply` is a byte `memcpy` × `times` — generic
//!   over `Clone` here (`times <= 0` yields empty; callers clamp
//!   negatives first, exactly as list/tuple do).

use crate::memory::gc::CallCtx;
use crate::objects::obj::ObjError;
use crate::objects::obj_ops::{get_index, obj_binary_op, obj_equal};
use crate::objects::slots::BinaryOp;
use crate::objects::strings::objstr_a::BoundSlice;
use crate::objects::value::Value;
use crate::pyerr::PyError;

/// Errors from the `mp_seq_*` helpers (the C9a `DictError`
/// shape: `exc_name()` for the VM's pending-exception wiring,
/// `to_pyerror()` for the slot boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeqError {
    /// `ValueError("object not in sequence")` — `index` miss.
    NotInSequence,
    /// A `get_index` failure (non-int index, out-of-range,
    /// overflow) — the `ObjError` carries the type/message.
    Obj(ObjError),
    /// A slot raised while comparing (data dropped at the
    /// boundary — see module docs).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl SeqError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            SeqError::NotInSequence => "ValueError",
            SeqError::Obj(e) => e.exc_type().map(|t| t.name()).unwrap_or(""),
            SeqError::OutOfMemory => "MemoryError",
            SeqError::PyRaise => "",
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` /
    /// `PyRaise`).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            SeqError::NotInSequence => Some("object not in sequence"),
            SeqError::Obj(e) => e.message(),
            SeqError::OutOfMemory | SeqError::PyRaise => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            SeqError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<ObjError> for SeqError {
    fn from(e: ObjError) -> SeqError {
        match e {
            ObjError::OutOfMemory => SeqError::OutOfMemory,
            other => SeqError::Obj(other),
        }
    }
}

/// Iterator tags for the C9 `IteratorData` encoding. C9a's dict
/// views claim two shapes: `extra.is_empty()` (a view, `index`
/// is the `DictViewKind`) and `extra.len() == 1` with `extra[0]`
/// in 0..=2 (a view-iterator). List/tuple/array iterators use
/// `extra == [tag]` with a tag `DictViewKind::from_i32` rejects,
/// so neither C9a reader misidentifies them; `index` is the
/// cursor. C11 formalises the iterator protocol; until then these
/// tags are the lane-local discriminator.
pub const LIST_ITER_TAG: i64 = 100;
pub const TUPLE_ITER_TAG: i64 = 101;
pub const ARRAY_ITER_TAG: i64 = 102;

/// `mp_seq_multiply` (`sequence.zig:54-63`): `items` repeated
/// `times` times. `times <= 0` yields an empty `Vec` (the
/// callers clamp negatives first). Returns `None` when the
/// result cannot be allocated (the Zig's `allocList`/`allocTuple`
/// returns NULL on malloc failure → `MemoryError`); the
/// `try_reserve_exact` keeps a huge-but-allocatable request
/// from aborting the process the way `with_capacity` would.
pub fn seq_multiply<T: Clone>(items: &[T], times: i64) -> Option<Vec<T>> {
    if times <= 0 || items.is_empty() {
        return Some(Vec::new());
    }
    let total = items.len().checked_mul(times as usize)?;
    let mut out = Vec::new();
    if out.try_reserve_exact(total).is_err() {
        return None;
    }
    for _ in 0..times {
        out.extend_from_slice(items);
    }
    Some(out)
}

/// `mp_seq_cmp_objs` (`sequence.zig:104-137`): compare two
/// element slices under `op` (EQUAL/LESS/LESS_EQUAL/MORE/
/// MORE_EQUAL — NOT_EQUAL is never passed; the caller inverts
/// EQUAL). Element equality is `obj_equal`; ordering uses the
/// binary-op slot and counts only an exact `Bool(true)` result.
pub fn seq_cmp_objs(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: &[Value],
    rhs: &[Value],
) -> Result<bool, SeqError> {
    let types = ctx.types;
    let (mut lhs, mut rhs, mut op) = (lhs, rhs, op);
    if op == BinaryOp::Equal && lhs.len() != rhs.len() {
        return Ok(false);
    }
    if op == BinaryOp::Less || op == BinaryOp::LessEqual {
        std::mem::swap(&mut lhs, &mut rhs);
        op = if op == BinaryOp::Less {
            BinaryOp::More
        } else {
            BinaryOp::MoreEqual
        };
    }
    let len = lhs.len().min(rhs.len());
    for i in 0..len {
        if obj_equal(ctx, types, lhs[i], rhs[i]).map_err(|_| SeqError::PyRaise)? {
            continue;
        }
        if op == BinaryOp::Equal {
            return Ok(false);
        }
        return Ok(
            obj_binary_op(ctx, types, op, lhs[i], rhs[i])
                .map_err(|_| SeqError::PyRaise)?
                == Value::Bool(true),
        );
    }
    if lhs.len() != rhs.len() {
        return Ok(lhs.len() > rhs.len());
    }
    if op == BinaryOp::More {
        return Ok(false);
    }
    Ok(true)
}

/// `mp_seq_index_obj` (`sequence.zig:139-159`): the index of
/// `value` in `items` within `[start, stop)` (`start`/`stop`
/// resolve through `get_index` with `is_slice=true`, i.e.
/// clamping; defaults 0 / `len`). A miss raises
/// `ValueError("object not in sequence")`.
pub fn seq_index_obj(
    ctx: &mut CallCtx,
    items: &[Value],
    value: Value,
    start: Option<Value>,
    stop: Option<Value>,
) -> Result<usize, SeqError> {
    let types = ctx.types;
    let len = items.len();
    let mut start_idx = 0usize;
    let mut stop_idx = len;
    if let Some(s) = start {
        start_idx = get_index(ctx, types, len, s, true)?;
        if let Some(e) = stop {
            stop_idx = get_index(ctx, types, len, e, true)?;
        }
    }
    let mut i = start_idx;
    while i < stop_idx {
        if obj_equal(ctx, types, items[i], value).map_err(|_| SeqError::PyRaise)? {
            return Ok(i);
        }
        i += 1;
    }
    Err(SeqError::NotInSequence)
}

/// `mp_seq_count_obj` (`sequence.zig:161-169`): the number of
/// elements equal to `value`.
pub fn seq_count_obj(
    ctx: &mut CallCtx,
    items: &[Value],
    value: Value,
) -> Result<usize, SeqError> {
    let types = ctx.types;
    let mut count = 0usize;
    for item in items {
        if obj_equal(ctx, types, *item, value).map_err(|_| SeqError::PyRaise)? {
            count += 1;
        }
    }
    Ok(count)
}

/// `mp_seq_extract_slice` (`sequence.zig:185-199`): the elements
/// of `items` selected by the (already bound-adjusted)
/// `BoundSlice`, as a `Vec` — the caller wraps it in a `List`
/// (list slice load with a non-`step == 1` slice). `step < 0`
/// loops `start >= stop`, else `start < stop`.
pub fn seq_extract_slice<T: Clone>(items: &[T], bound: &BoundSlice) -> Vec<T> {
    let mut out = Vec::new();
    if bound.step < 0 {
        let mut start = bound.start;
        while start >= bound.stop {
            if let Some(v) = items.get(start as usize) {
                out.push(v.clone());
            }
            start += bound.step;
        }
    } else {
        let mut start = bound.start;
        while start < bound.stop {
            if let Some(v) = items.get(start as usize) {
                out.push(v.clone());
            }
            start += bound.step;
        }
    }
    out
}
