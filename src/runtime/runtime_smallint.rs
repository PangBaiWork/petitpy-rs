//! The small-int x small-int fast path of `mp_binary_op`
//! (`runtime.zig:576-701`), split out of `runtime_ops.rs` so
//! that file stays under 800 lines.
//!
//! The chain runs only when BOTH operands are small ints (the
//! Zig's `if (isSmallInt(lhs)) { if (isSmallInt(rhs)) { ... } }`
//! — a small int on the left with a non-small-int right falls
//! through to the float/complex arms and then the generic loop).
//! `GoGeneric` is the `go_big` escape: the caller continues at
//! the generic loop with the heap-converted lhs (the Zig's
//! `break :to_generic`).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::slots::BinaryOp;
use crate::objects::value::{wrap_small_int, Obj, Value, SMALL_INT_MAX, SMALL_INT_MIN};
use crate::runtime::runtime::RtError;
use crate::runtime::runtime_ops::binop_unsupported;
use crate::support::smallint::{
    small_int_floor_divide as floored_div, small_int_modulo as floored_mod,
};
use crate::vendor::mpz::BigInt;

/// The two canonical small-int functions live in
/// `crate::support::smallint` (`support/smallint.zig`) and are
/// imported above as `floored_div` / `floored_mod` (C16
/// unification — the local copies C14 hand-rolled here were a
/// second implementation of the same two functions; the Zig's
/// numerator-adjustment form and C14's quotient-adjustment form
/// agree on every in-window input, and the canonical Zig-faithful
/// form is now the single home).

/// Force a heap int (the Zig's `mp_obj_new_int_from_ll` in the
/// `go_big` arms — `Value::int` would return the same small int,
/// since the converted value is always the in-window original
/// lhs; the forced heap allocation is behaviorally neutral in
/// Rust but keeps the Zig's state).
fn force_heap_int(ctx: &mut CallCtx, n: i64) -> Value {
    Value::Heap(ctx.alloc(Some(ctx.types.int), Obj::Int(BigInt::from_int(n))))
}

/// The small-int × small-int fast path (`runtime.zig:576-701`).
/// `Ok(SmallIntOutcome::Done(v))` = finished;
/// `Ok(SmallIntOutcome::GoGeneric(heap_lhs))` = the `go_big`
/// escape — the caller continues at the generic loop with the
/// heap-converted lhs.
pub(crate) enum SmallIntOutcome {
    Done(Value),
    GoGeneric(Value),
}

pub(crate) fn small_int_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs_val: i64,
    rhs_val: i64,
    lhs: Value,
) -> Result<SmallIntOutcome, RtError> {
    let types = ctx.types;
    let mut lv = lhs_val;
    let rv = rhs_val;
    match op {
        BinaryOp::Or | BinaryOp::InplaceOr => lv |= rv,
        BinaryOp::Xor | BinaryOp::InplaceXor => lv ^= rv,
        BinaryOp::And | BinaryOp::InplaceAnd => lv &= rv,
        BinaryOp::Lshift | BinaryOp::InplaceLshift => {
            if rv < 0 {
                return Err(RtError::ValueError("negative shift count".to_string()));
            }
            if rv >= 64
                || lv > (SMALL_INT_MAX >> rv)
                || lv < (SMALL_INT_MIN >> rv)
            {
                return Ok(SmallIntOutcome::GoGeneric(force_heap_int(ctx, lv)));
            }
            lv = ((lv as u64) << rv) as i64;
        }
        BinaryOp::Rshift | BinaryOp::InplaceRshift => {
            if rv < 0 {
                return Err(RtError::ValueError("negative shift count".to_string()));
            }
            let shift = if rv >= 64 { 63 } else { rv };
            lv >>= shift;
        }
        BinaryOp::Add | BinaryOp::InplaceAdd => lv = lv.wrapping_add(rv),
        BinaryOp::Subtract | BinaryOp::InplaceSubtract => lv = lv.wrapping_sub(rv),
        BinaryOp::Multiply | BinaryOp::InplaceMultiply => match lv.checked_mul(rv) {
            Some(p) => lv = p,
            None => return Ok(SmallIntOutcome::GoGeneric(force_heap_int(ctx, lv))),
        },
        BinaryOp::FloorDivide | BinaryOp::InplaceFloorDivide => {
            if rv == 0 {
                return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
            }
            // Floored division (Zig `mp_small_int_floor_divide`,
            // Python `//`). `div_euclid` is EUCLIDEAN (remainder
            // always non-negative) — wrong for a negative divisor
            // (`8 // -3 == -3`, not `-2`). (`div_floor` is not
            // stable on integers, so the truncation is adjusted
            // by hand; small ints cannot overflow here.)
            lv = floored_div(lv, rv);
        }
        BinaryOp::TrueDivide | BinaryOp::InplaceTrueDivide => {
            if rv == 0 {
                return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
            }
            let f = Value::Heap(ctx.alloc(
                Some(types.float),
                Obj::Float(lv as f64 / rv as f64),
            ));
            return Ok(SmallIntOutcome::Done(f));
        }
        BinaryOp::Modulo | BinaryOp::InplaceModulo => {
            if rv == 0 {
                return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
            }
            // Floored modulo (Zig `@mod`, Python `%`): the result
            // takes the DIVISOR's sign. `rem_euclid` is always
            // non-negative — wrong for a negative divisor
            // (`8 % -3 == -1`, probed on the reference binary).
            lv = floored_mod(lv, rv);
        }
        BinaryOp::Power | BinaryOp::InplacePower => {
            if rv < 0 {
                // `int ** -n` is a float (C7's `int_binary_op`
                // does the same inline `powf`).
                if lv == 0 {
                    return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
                }
                let f = Value::Heap(ctx.alloc(
                    Some(types.float),
                    Obj::Float((lv as f64).powf(rv as f64)),
                ));
                return Ok(SmallIntOutcome::Done(f));
            }
            let orig = lv;
            let mut ans: i64 = 1;
            let mut base = lv;
            let mut exp = rv;
            let mut of = false;
            while exp > 0 {
                if exp & 1 != 0 {
                    match ans.checked_mul(base) {
                        Some(p) => ans = p,
                        None => {
                            of = true;
                            break;
                        }
                    }
                }
                if exp == 1 {
                    break;
                }
                exp /= 2;
                match base.checked_mul(base) {
                    Some(p) => base = p,
                    None => {
                        of = true;
                        break;
                    }
                }
            }
            if of {
                return Ok(SmallIntOutcome::GoGeneric(force_heap_int(ctx, orig)));
            }
            lv = ans;
        }
        BinaryOp::Divmod => {
            if rv == 0 {
                return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
            }
            // Probed on the reference binary: `divmod(-(2**62), -1)`
            // → `(-4611686018427387904, 0)` — the Zig's DIVMOD arm
            // builds the pair with raw `newSmallInt`, WITHOUT the
            // window check the other arms get at the end, so a
            // 2**62 quotient mistags to -2**62 (while `//` alone
            // gives the correct 2**62). Ported verbatim via
            // `wrap_small_int` (a no-op in-window).
            let q = Value::SmallInt(wrap_small_int(floored_div(lv, rv)));
            let r = Value::SmallInt(wrap_small_int(floored_mod(lv, rv)));
            let t = Value::Heap(new_tuple(ctx, types.tuple, &[q, r]));
            return Ok(SmallIntOutcome::Done(t));
        }
        BinaryOp::Less => return Ok(SmallIntOutcome::Done(Value::Bool(lv < rv))),
        BinaryOp::More => return Ok(SmallIntOutcome::Done(Value::Bool(lv > rv))),
        BinaryOp::LessEqual => return Ok(SmallIntOutcome::Done(Value::Bool(lv <= rv))),
        BinaryOp::MoreEqual => return Ok(SmallIntOutcome::Done(Value::Bool(lv >= rv))),
        _ => return Err(binop_unsupported(ctx, op, lhs, Value::SmallInt(rv))),
    }
    Ok(SmallIntOutcome::Done(Value::int(
        ctx.arena,
        Some(types.int),
        ctx.roots,
        lv,
    )))
}
