//! Small-int arithmetic (`support/smallint.zig` — Zig port of
//! vendor `py/smallint.c`).
//!
//! Two pure `mp_int_t` (here `i64`) functions. Faithful to C under
//! `-fno-sanitize=undefined` (the Zig header's lesson #6): C's
//! signed `+` / unary `-` rely on 2's-complement wrap → the
//! wrapping ops below; C `%` is truncated remainder (Rust `%`);
//! C `/` is truncation toward zero (Rust `/`).
//!
//! Caller guarantees (from the Zig header, same here): the
//! divisor is never 0, and callers promote to bigint before the
//! `INT_MIN / -1` pathology — exactly as the C original relies
//! on. These functions are the CANONICAL implementation:
//! `crate::runtime::runtime_smallint`'s private `floored_div` /
//! `floored_mod` delegate here (C16 unification; they were
//! hand-rolled copies of the same two functions).

/// `mp_small_int_modulo` — floored modulo: the result takes the
/// DIVISOR's sign (`8 % -3 == -1`, `-8 % 3 == 1`).
pub fn small_int_modulo(dividend_in: i64, divisor: i64) -> i64 {
    // Python specs require that mod has same sign as second operand.
    let mut dividend = dividend_in % divisor; // C: `dividend %= divisor`
    if (dividend < 0 && divisor > 0) || (dividend > 0 && divisor < 0) {
        dividend = dividend.wrapping_add(divisor);
    }
    dividend
}

/// `mp_small_int_floor_divide` — floored division: the quotient
/// rounds toward -inf (`8 // -3 == -3`, `-8 // 3 == -3`).
/// The Zig adjusts the numerator so a truncating divide lands on
/// the floor; ported verbatim (wrapping arithmetic, same as the
/// Zig's `+%` / `-%`).
pub fn small_int_floor_divide(num_in: i64, denom: i64) -> i64 {
    let mut num = num_in;
    if num >= 0 {
        if denom < 0 {
            num = num.wrapping_add((0i64.wrapping_sub(denom)).wrapping_sub(1)); // C: `num += -denom - 1`
        }
    } else if denom >= 0 {
        num = num.wrapping_add((0i64.wrapping_sub(denom)).wrapping_add(1)); // C: `num += -denom + 1`
    }
    num / denom // C: `num / denom` (trunc toward zero)
}
