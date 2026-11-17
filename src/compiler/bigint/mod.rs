//! Arbitrary-precision integer support for the compiler track.
//!
//! Port of `src/compiler/bigint.zig`. Two responsibilities, exactly as
//! in the original:
//!
//!   * `parse_literal` — accept the raw text from an integer literal
//!     (with optional `0x`/`0o`/`0b` prefix, internal `_` separators,
//!     and an optional leading `-` for fold-injected negatives) and
//!     return an owned value.
//!   * `to_decimal_string` — emit the canonical signed-decimal text
//!     representation. Matches mpy-cross's `mp_obj_print_helper`
//!     (PRINT_REPR) output: no leading zeros, leading `-` for
//!     negatives, "0" for zero.
//!
//! ## Divergence: no `std.math.big.int` in Rust
//!
//! The Zig original is a thin layer over `std.math.big.int.Managed`.
//! Rust's std has no big-integer type, so this module carries a
//! hand-rolled sign-magnitude `BigInt` (`Vec<u32>` little-endian
//! limbs, pure std, no crates). The *observable* semantics are the
//! Zig ones, verified against the original by probe (`/tmp/zigprobe`):
//! `is_positive(0) == true` (so `0` counts as positive for the
//! bitwise-fold gate), `setString` rejects `+1`, skips `_` anywhere,
//! shifts are arithmetic/floor (`-8 >> 1 == -4`, `-1 << 2 == -4`),
//! `divFloor(-7, 3) == (-3, 2)`, `toInt` errors out of range.
//!
//! `OutOfMemory` exists in `BigIntError` for signature parity with
//! the Zig error set, but Rust allocation failure aborts the process
//! (same as everywhere else in this port) — the variant is only ever
//! *returned* for the deterministic rejections (invalid character /
//! invalid base). Callers map it the same way the Zig callers do.


mod core;

pub use core::BigInt;

use std::fmt;

/// Error set for integer-literal parsing and folding.
///
/// Mirrors Zig `bigint.zig`'s `ParseError = error{InvalidCharacter,
/// InvalidBase, OutOfMemory}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigIntError {
    /// A character is not a digit in the literal's base.
    InvalidCharacter,
    /// The base prefix is not one of 2/8/10/16 (unreachable from
    /// `parse_literal`, which only passes those four).
    InvalidBase,
    /// An allocation failed. Unreachable in practice (see module docs).
    OutOfMemory,
}

impl fmt::Display for BigIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BigIntError::InvalidCharacter => write!(f, "invalid character in integer literal"),
            BigIntError::InvalidBase => write!(f, "invalid base for integer literal"),
            BigIntError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for BigIntError {}

/// `Result<T, BigIntError>` — the Rust spelling of Zig's
/// `ParseError!T` in `bigint.zig`.
pub type BigIntResult<T> = Result<T, BigIntError>;

/// Parse an integer literal into a freshly-owned `BigInt`.
///
/// Mirrors Zig `parseLiteral`: optional leading `-` (single sign;
/// `+` is rejected — probed: `setString("+1")` is InvalidCharacter),
/// optional `0x`/`0X`, `0o`/`0O`, `0b`/`0B` base prefix, internal
/// `_` separators (skipped anywhere — probed: `1_` and `1__2` both
/// parse), decimal default. Empty after the prefix is InvalidCharacter.
pub fn parse_literal(src: &str) -> BigIntResult<BigInt> {
    let bytes = src.as_bytes();
    let mut s = bytes;
    let mut negative = false;
    if !s.is_empty() && s[0] == b'-' {
        negative = true;
        s = &s[1..];
    }
    let mut base: u32 = 10;
    if s.len() >= 2 && s[0] == b'0' {
        match s[1] {
            b'x' | b'X' => {
                base = 16;
                s = &s[2..];
            }
            b'o' | b'O' => {
                base = 8;
                s = &s[2..];
            }
            b'b' | b'B' => {
                base = 2;
                s = &s[2..];
            }
            _ => {}
        }
    }
    if s.is_empty() {
        return Err(BigIntError::InvalidCharacter);
    }
    let mut m = BigInt::zero();
    for &c in s {
        if c == b'_' {
            continue;
        }
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'f' => (c - b'a') as u32 + 10,
            b'A'..=b'F' => (c - b'A') as u32 + 10,
            _ => return Err(BigIntError::InvalidCharacter),
        };
        if d >= base {
            return Err(BigIntError::InvalidCharacter);
        }
        m.mul_add_small(base, d);
    }
    if negative {
        m.negate();
    }
    Ok(m)
}

/// Canonical signed-decimal text representation. Matches the bytes
/// mpy-cross writes for an int_big object: no leading zeros, leading
/// `-` for negatives, "0" for zero. Mirrors Zig `toDecimalString`.
pub fn to_decimal_string(m: &BigInt) -> String {
    m.to_decimal_string()
}

/// Convenience: parse + normalize in one shot. Returns canonical
/// decimal text. Mirrors Zig `normalizeToDecimal`.
pub fn normalize_to_decimal(src: &str) -> BigIntResult<String> {
    Ok(parse_literal(src)?.to_decimal_string())
}

/// Supported binary operators for parse-time constant folding.
/// Mirrors Zig `BinOp` (vendor `binary_op_maybe`, parse.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Floordiv,
    Mod,
    Shl,
    Shr,
    Bor,
    Bxor,
    Band,
    Pow,
}

/// Supported unary operators for parse-time constant folding.
/// Mirrors Zig `UnOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Pos,
    Neg,
    Invert,
}

/// Cap on shift count to avoid runaway memory at compile time.
/// Anything above ~64K-bit shifts is unrealistic for a literal const
/// expression. Mirrors Zig `MAX_SHIFT_BITS`.
const MAX_SHIFT_BITS: usize = 1 << 16;

/// Cap on `**` exponent. Mirrors Zig `MAX_POW_EXPONENT`.
const MAX_POW_EXPONENT: i64 = 4096;

/// Apply a binary op on big-int operand texts and return canonical
/// decimal text, or `None` when the op is unsupported / pre-conditions
/// fail (div-by-zero, negative or oversized shift). Mirrors Zig
/// `foldBinary`.
pub fn fold_binary(
    op: BinOp,
    a_text: &str,
    b_text: &str,
) -> BigIntResult<Option<String>> {
    let a = parse_literal(a_text)?;
    let b = parse_literal(b_text)?;
    let r: BigInt = match op {
        BinOp::Add => a.add(&b),
        BinOp::Sub => a.sub(&b),
        BinOp::Mul => a.mul(&b),
        BinOp::Floordiv | BinOp::Mod => {
            if b.is_zero() {
                return Ok(None);
            }
            let (q, r) = a.div_floor(&b);
            return Ok(Some(if op == BinOp::Floordiv {
                q
            } else {
                r
            }
            .to_decimal_string()));
        }
        BinOp::Shl | BinOp::Shr => {
            let shift_i = match b.to_i64() {
                Some(v) => v,
                None => return Ok(None),
            };
            if shift_i < 0 {
                return Ok(None);
            }
            let shift = shift_i as usize;
            if op == BinOp::Shl {
                if shift > MAX_SHIFT_BITS {
                    return Ok(None);
                }
                a.shift_left(shift)
            } else {
                a.shift_right(shift)
            }
        }
        BinOp::Bor | BinOp::Bxor | BinOp::Band => {
            // Zig's `Managed.bitOr/bitAnd/bitXor` are wrong for
            // mixed-sign large-magnitude inputs (verified against
            // CPython in the original); skip the fold when any operand
            // is negative. `is_positive` is true for zero (probed).
            if !a.is_positive() || !b.is_positive() {
                return Ok(None);
            }
            a.bit_op(op, &b)
        }
        BinOp::Pow => {
            // Integer ** non-negative integer. Negative exponent
            // returns a float in Python, which we never fold — bail.
            let exp_i = match b.to_i64() {
                Some(v) => v,
                None => return Ok(None),
            };
            if exp_i < 0 || exp_i > MAX_POW_EXPONENT {
                return Ok(None);
            }
            a.pow(exp_i as u32)
        }
    };
    Ok(Some(r.to_decimal_string()))
}

/// Apply a unary op on a big-int operand text. Mirrors Zig
/// `foldUnary`.
pub fn fold_unary(op: UnOp, a_text: &str) -> BigIntResult<String> {
    let mut a = parse_literal(a_text)?;
    match op {
        UnOp::Pos => {}
        UnOp::Neg => a.negate(),
        UnOp::Invert => {
            // ~x == -(x + 1)
            a.add_scalar(1);
            a.negate();
        }
    }
    Ok(a.to_decimal_string())
}

/// If a value fits the SMALL_INT window used by the parser for inline
/// integer leaves, return that value; else `None`. Mirrors Zig
/// `tryToSmallInt` (63-bit window for MICROPY_OBJ_REPR_A).
pub fn try_to_small_int(m: &BigInt) -> Option<isize> {
    const SMALL_INT_MAX: isize = (1 << 62) - 1;
    const SMALL_INT_MIN: isize = -(1 << 62);
    let v = m.to_i64()?;
    let iv = v as isize;
    if iv < SMALL_INT_MIN || iv > SMALL_INT_MAX {
        return None;
    }
    Some(iv)
}

/// Parse + classify in one call. Either the value fits SMALL_INT (no
/// allocation needed for `.small`) or it's a true big-int and the
/// canonical decimal text is returned. Mirrors Zig `FoldResult` +
/// `classifyText`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldResult {
    Small(isize),
    BigDecimal(String),
}

pub fn classify_text(src: &str) -> BigIntResult<FoldResult> {
    let m = parse_literal(src)?;
    if let Some(v) = try_to_small_int(&m) {
        return Ok(FoldResult::Small(v));
    }
    Ok(FoldResult::BigDecimal(m.to_decimal_string()))
}

#[cfg(test)]
mod tests;
