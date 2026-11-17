//! Slice helpers (Lane C8a, split out of `objstr_a.rs` to
//! keep files under 800 lines): `mp_bound_slice_t`,
//! `mp_obj_slice_indices`, `mp_seq_get_fast_slice_indexes`,
//! `sliceCount`.

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int, BuiltinTypes};
use crate::objects::strings::objstr_a::StrError;
use crate::objects::value::{SliceData, Value};

// ── slice helpers ─────────────────────────────────────────────────────

/// `mp_bound_slice_t` (vendor `py/obj.h`): resolved slice bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundSlice {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

/// `mp_obj_slice_indices` (vendor `py/objslice.c:129-187`): resolve
/// a slice against a sequence of `length`, resolving missing
/// components, negatives and off-the-end values. `step == 0` raises
/// `ValueError: slice step can't be zero`; a non-int component
/// raises `TypeError: can't convert to int` (via `obj_get_int`).
pub fn slice_indices(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    slice: &SliceData,
    length: i64,
) -> Result<BoundSlice, StrError> {
    let step: i64 = match slice.step {
        None => 1,
        Some(v) => {
            let n = obj_get_int(ctx, types, v)?;
            if n == 0 {
                return Err(StrError::SliceStepZero);
            }
            n
        }
    };
    let mut bound = |v: Option<Value>| -> Result<Option<i64>, StrError> {
        match v {
            None => Ok(None),
            Some(v) => Ok(Some(obj_get_int(ctx, types, v)?)),
        }
    };
    if step > 0 {
        let start = match bound(slice.start)? {
            None => 0,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        let stop = match bound(slice.stop)? {
            None => length,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        Ok(BoundSlice { start, stop, step })
    } else {
        let start = match bound(slice.start)? {
            None => length - 1,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        let stop = match bound(slice.stop)? {
            None => -1,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        Ok(BoundSlice { start, stop, step })
    }
}

/// `mp_seq_get_fast_slice_indexes` (vendor `py/sequence.c:48-64`):
/// `slice_indices` plus the fast-path adjustments (negative step:
/// `stop += 1`; positive step with `start > stop`: `stop = start`;
/// negative step with `start < stop`: `stop = start + 1`). Returns
/// the bounds and whether the step is 1 (the fast path).
pub fn fast_slice_indexes(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    slice: &SliceData,
    length: i64,
) -> Result<(BoundSlice, bool), StrError> {
    let mut b = slice_indices(ctx, types, slice, length)?;
    if b.step < 0 {
        b.stop += 1;
    }
    if b.step > 0 && b.start > b.stop {
        b.stop = b.start;
    } else if b.step < 0 && b.start < b.stop {
        b.stop = b.start + 1;
    }
    let fast = b.step == 1;
    Ok((b, fast))
}

/// `sliceCount` (`objstr.zig:427-433`): how many items a bound
/// slice selects. A signed walk, so negative-step slices work.
pub fn slice_count(b: &BoundSlice) -> usize {
    let step = b.step;
    if step > 0 && b.stop > b.start {
        ((b.stop - b.start + step - 1) / step) as usize
    } else if step < 0 && b.start > b.stop {
        ((b.start - b.stop + (-step) - 1) / (-step)) as usize
    } else {
        0
    }
}
