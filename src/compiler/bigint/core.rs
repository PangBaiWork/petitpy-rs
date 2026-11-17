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


//! BigInt core arithmetic (sign-magnitude, `Vec<u32>` limbs).

use super::BinOp;
use std::cmp::Ordering;


/// Sign-magnitude arbitrary-precision integer.
///
/// `limbs` are little-endian `u32`s with no trailing zero limbs;
/// the empty vec is zero and `neg` is then always false. This is the
/// Rust stand-in for Zig `std.math.big.int.Managed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigInt {
    neg: bool,
    limbs: Vec<u32>,
}

impl BigInt {
    /// Zero.
    pub fn zero() -> Self {
        BigInt {
            neg: false,
            limbs: Vec::new(),
        }
    }

    /// Small signed value.
    pub fn from_i64(v: i64) -> Self {
        if v == 0 {
            return BigInt::zero();
        }
        let neg = v < 0;
        let mag = v.unsigned_abs();
        let mut limbs = vec![mag as u32];
        let hi = (mag >> 32) as u32;
        if hi != 0 {
            limbs.push(hi);
        }
        BigInt { neg, limbs }
    }

    /// Drop trailing zero limbs; zero is never negative. Mirrors the
    /// normalisation `Managed` maintains after every op.
    fn normalize(&mut self) {
        while let Some(&0) = self.limbs.last() {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.neg = false;
        }
    }

    /// Is this zero?
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Is this negative? (`Managed.isNegative`.)
    pub fn is_negative(&self) -> bool {
        self.neg
    }

    /// Is this non-negative? (`Managed.isPositive` — probed: it
    /// returns true for zero, so this is simply `!neg`.)
    pub fn is_positive(&self) -> bool {
        !self.neg
    }

    /// Negate in place (`Managed.negate`).
    pub fn negate(&mut self) {
        if !self.limbs.is_empty() {
            self.neg = !self.neg;
        }
    }

    /// Compare magnitudes: -1 / 0 / +1.
    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        let (al, bl) = (trim_len(a), trim_len(b));
        if al != bl {
            return al.cmp(&bl);
        }
        for i in (0..al).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    /// `|self| + |other|` (magnitude add, sign ignored).
    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry: u64 = 0;
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(0) as u64;
            let y = b.get(i).copied().unwrap_or(0) as u64;
            let s = x + y + carry;
            out.push(s as u32);
            carry = s >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        out
    }

    /// `|a| - |b|` with `|a| >= |b|` (magnitude subtract).
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        debug_assert!(BigInt::cmp_mag(a, b) != Ordering::Less);
        let mut out = Vec::with_capacity(a.len());
        let mut borrow: i64 = 0;
        for i in 0..a.len() {
            let x = a[i] as i64;
            let y = b.get(i).copied().unwrap_or(0) as i64;
            let mut d = x - y + borrow;
            if d < 0 {
                d += 1 << 32;
                borrow = -1;
            } else {
                borrow = 0;
            }
            out.push(d as u32);
        }
        debug_assert_eq!(borrow, 0);
        while let Some(&0) = out.last() {
            out.pop();
        }
        out
    }

    /// `|a| * |b|` (schoolbook magnitude multiply).
    fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        if a.is_empty() || b.is_empty() {
            return Vec::new();
        }
        let mut out = vec![0u32; a.len() + b.len()];
        for (i, &x) in a.iter().enumerate() {
            let mut carry: u64 = 0;
            for (j, &y) in b.iter().enumerate() {
                let cur = out[i + j] as u64 + x as u64 * y as u64 + carry;
                out[i + j] = cur as u32;
                carry = cur >> 32;
            }
            let mut k = i + b.len();
            while carry != 0 {
                let cur = out[k] as u64 + carry;
                out[k] = cur as u32;
                carry = cur >> 32;
                k += 1;
            }
        }
        while let Some(&0) = out.last() {
            out.pop();
        }
        out
    }

    /// `|a| * small + small` in place (used by the digit parser).
    /// `pub(crate)`: the parser in `mod.rs` drives it.
    pub(crate) fn mul_add_small(&mut self, m: u32, add: u32) {
        let mut carry: u64 = add as u64;
        for limb in self.limbs.iter_mut() {
            let cur = *limb as u64 * m as u64 + carry;
            *limb = cur as u32;
            carry = cur >> 32;
        }
        while carry != 0 {
            self.limbs.push(carry as u32);
            carry >>= 32;
        }
        self.normalize();
    }

    /// Unsigned `|a| / |b|` and `|a| % |b|` by bit-by-bit long
    /// division. `b` must be non-zero. Simple and obviously correct;
    /// parse-time literals are small, so the O(n^2)-bit cost is
    /// irrelevant here.
    fn divmod_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
        debug_assert!(!b.is_empty());
        let mut rem: Vec<u32> = Vec::new();
        let total_bits = {
            let mut n = 0usize;
            for (i, &l) in a.iter().enumerate() {
                if l != 0 {
                    n = i * 32 + (32 - l.leading_zeros() as usize);
                }
            }
            n
        };
        let mut quot = vec![0u32; total_bits.div_ceil(32)];
        for i in (0..total_bits).rev() {
            // rem = (rem << 1) | bit_i(a)
            shift_left_one(&mut rem);
            if bit_at(a, i) {
                if rem.is_empty() {
                    rem.push(1);
                } else {
                    rem[0] |= 1;
                }
            }
            if BigInt::cmp_mag(&rem, b) != Ordering::Less {
                rem = BigInt::sub_mag(&rem, b);
                quot[i / 32] |= 1 << (i % 32);
            }
        }
        while let Some(&0) = quot.last() {
            quot.pop();
        }
        (quot, rem)
    }

    /// `|a| / 1_000_000_000`, returning `(quotient, remainder)`.
    /// Used by the decimal printer.
    fn divmod_small(&self, d: u32) -> (BigInt, u32) {
        let mut quot = vec![0u32; self.limbs.len()];
        let mut rem: u64 = 0;
        for i in (0..self.limbs.len()).rev() {
            let cur = (rem << 32) | self.limbs[i] as u64;
            quot[i] = (cur / d as u64) as u32;
            rem = cur % d as u64;
        }
        while let Some(&0) = quot.last() {
            quot.pop();
        }
        (
            BigInt {
                neg: false,
                limbs: quot,
            },
            rem as u32,
        )
    }

    /// Signed add (`Managed.add`).
    pub fn add(&self, other: &BigInt) -> BigInt {
        if self.neg == other.neg {
            let mut r = BigInt {
                neg: self.neg,
                limbs: BigInt::add_mag(&self.limbs, &other.limbs),
            };
            r.normalize();
            r
        } else if BigInt::cmp_mag(&self.limbs, &other.limbs) != Ordering::Less {
            let mut r = BigInt {
                neg: self.neg,
                limbs: BigInt::sub_mag(&self.limbs, &other.limbs),
            };
            r.normalize();
            r
        } else {
            let mut r = BigInt {
                neg: other.neg,
                limbs: BigInt::sub_mag(&other.limbs, &self.limbs),
            };
            r.normalize();
            r
        }
    }

    /// Signed subtract (`Managed.sub`).
    pub fn sub(&self, other: &BigInt) -> BigInt {
        let mut neg_other = other.clone();
        neg_other.negate();
        self.add(&neg_other)
    }

    /// Signed multiply (`Managed.mul`).
    pub fn mul(&self, other: &BigInt) -> BigInt {
        if self.is_zero() || other.is_zero() {
            return BigInt::zero();
        }
        let mut r = BigInt {
            neg: self.neg != other.neg,
            limbs: BigInt::mul_mag(&self.limbs, &other.limbs),
        };
        r.normalize();
        r
    }

    /// Add a small `i32` (`Managed.addScalar`, used by `~x`).
    pub fn add_scalar(&mut self, v: i32) {
        let other = BigInt::from_i64(v as i64);
        let r = self.add(&other);
        *self = r;
    }

    /// Floor division with remainder (`Managed.divFloor`): returns
    /// `(q, r)` with `q = floor(a/b)` and `r = a - q*b` (Python
    /// semantics — the remainder takes the divisor's sign).
    pub fn div_floor(&self, other: &BigInt) -> (BigInt, BigInt) {
        debug_assert!(!other.is_zero());
        if self.is_zero() {
            return (BigInt::zero(), BigInt::zero());
        }
        let (q_mag, r_mag) = BigInt::divmod_mag(&self.limbs, &other.limbs);
        let q_neg = self.neg != other.neg;
        if r_mag.is_empty() {
            let mut q = BigInt {
                neg: q_neg,
                limbs: q_mag,
            };
            q.normalize();
            return (q, BigInt::zero());
        }
        if self.neg == other.neg {
            // Same sign: truncation is already the floor.
            let mut q = BigInt {
                neg: q_neg,
                limbs: q_mag,
            };
            q.normalize();
            let r = BigInt {
                neg: self.neg,
                limbs: r_mag,
            };
            (q, r)
        } else {
            // Different signs: floor is one below truncation, and the
            // remainder becomes |b| - r_mag with the divisor's sign.
            let q = BigInt {
                neg: true,
                limbs: BigInt::add_mag(&q_mag, &[1]),
            };
            let mut q = q;
            q.normalize();
            let r = BigInt {
                neg: other.neg,
                limbs: BigInt::sub_mag(&other.limbs, &r_mag),
            };
            let mut r = r;
            r.normalize();
            (q, r)
        }
    }

    /// Shift left by `n` bits, sign preserved (`Managed.shiftLeft`).
    pub fn shift_left(&self, n: usize) -> BigInt {
        if self.is_zero() || n == 0 {
            return self.clone();
        }
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        let mut limbs = vec![0u32; limb_shift];
        limbs.extend_from_slice(&self.limbs);
        if bit_shift != 0 {
            let mut carry: u32 = 0;
            for limb in limbs.iter_mut().skip(limb_shift) {
                let c = *limb >> (32 - bit_shift);
                *limb = (*limb << bit_shift) | carry;
                carry = c;
            }
            if carry != 0 {
                limbs.push(carry);
            }
        }
        let mut r = BigInt {
            neg: self.neg,
            limbs,
        };
        r.normalize();
        r
    }

    /// Arithmetic shift right by `n` bits (`Managed.shiftRight`):
    /// floor division by 2^n, so negatives round toward -inf.
    pub fn shift_right(&self, n: usize) -> BigInt {
        if self.is_zero() || n == 0 {
            return self.clone();
        }
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        if limb_shift >= self.limbs.len() {
            // Everything shifted out: 0 for non-negative, -1 for
            // negative (floor of a negative fraction).
            return if self.neg {
                BigInt::from_i64(-1)
            } else {
                BigInt::zero()
            };
        }
        let mut limbs: Vec<u32> = self.limbs[limb_shift..].to_vec();
        if bit_shift != 0 {
            let mut carry: u32 = 0;
            for limb in limbs.iter_mut().rev() {
                let c = *limb << (32 - bit_shift);
                *limb = (*limb >> bit_shift) | carry;
                carry = c;
            }
        }
        // Dropped bits: any set bit below the shift makes a negative
        // result one more negative (floor, not truncation).
        let mut dropped = false;
        for (i, &limb) in self.limbs.iter().enumerate().take(limb_shift) {
            if limb != 0 {
                dropped = true;
                let _ = i;
            }
        }
        if bit_shift != 0 && limb_shift < self.limbs.len() {
            let mask: u32 = (1u32 << bit_shift) - 1;
            if self.limbs[limb_shift] & mask != 0 {
                dropped = true;
            }
        }
        let mut r = BigInt {
            neg: self.neg,
            limbs,
        };
        r.normalize();
        if self.neg && dropped {
            // floor: -(mag + 1)
            let one = BigInt::from_i64(1);
            let mag = BigInt {
                neg: false,
                limbs: r.limbs.clone(),
            };
            let bumped = mag.add(&one);
            r = BigInt {
                neg: true,
                limbs: bumped.limbs,
            };
            r.normalize();
        }
        r
    }

    /// Bitwise OR on magnitudes. Caller guarantees both operands are
    /// non-negative (the fold layer bails otherwise — Zig's
    /// `Managed.bitOr` is wrong for mixed-sign large magnitudes, see
    /// `bigint.zig`).
    fn bit_op_mag(a: &[u32], b: &[u32], op: BinOp) -> Vec<u32> {
        let n = a.len().max(b.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            out.push(match op {
                BinOp::Bor => x | y,
                BinOp::Bxor => x ^ y,
                BinOp::Band => x & y,
                _ => unreachable!(),
            });
        }
        while let Some(&0) = out.last() {
            out.pop();
        }
        out
    }

    /// Bitwise op, non-negative operands only (see `bit_op_mag`).
    pub fn bit_op(&self, op: BinOp, other: &BigInt) -> BigInt {
        debug_assert!(self.is_positive() && other.is_positive());
        let mut r = BigInt {
            neg: false,
            limbs: BigInt::bit_op_mag(&self.limbs, &other.limbs, op),
        };
        r.normalize();
        r
    }

    /// Power (`Managed.pow`): `self ** exp`, `exp >= 0`.
    pub fn pow(&self, exp: u32) -> BigInt {
        let mut result = BigInt::from_i64(1);
        let mut base = self.clone();
        let mut e = exp;
        while e > 0 {
            if e & 1 == 1 {
                result = result.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        result
    }

    /// Try to fit in `i64` (`Managed.toInt(i64)` — errors out of
    /// range, probed as `error.TargetTooSmall`).
    pub fn to_i64(&self) -> Option<i64> {
        if self.limbs.is_empty() {
            return Some(0);
        }
        if self.limbs.len() > 2 {
            return None;
        }
        let mag: u64 = self.limbs[0] as u64
            | ((self.limbs.get(1).copied().unwrap_or(0) as u64) << 32);
        if self.neg {
            if mag > (i64::MAX as u64) + 1 {
                return None;
            }
            if mag == (i64::MAX as u64) + 1 {
                return Some(i64::MIN);
            }
            Some(-(mag as i64))
        } else {
            if mag > i64::MAX as u64 {
                return None;
            }
            Some(mag as i64)
        }
    }

    /// Canonical signed-decimal text (`Managed.toString(10)`).
    pub fn to_decimal_string(&self) -> String {
        if self.limbs.is_empty() {
            return "0".to_string();
        }
        // Repeated divmod by 1e9, least-significant chunk first.
        let mut chunks: Vec<u32> = Vec::new();
        let mut cur = self.clone();
        cur.neg = false;
        while !cur.limbs.is_empty() {
            let (q, r) = cur.divmod_small(1_000_000_000);
            chunks.push(r);
            cur = q;
        }
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        let mut it = chunks.iter().rev();
        if let Some(&top) = it.next() {
            s.push_str(&top.to_string());
            for &c in it {
                s.push_str(&format!("{c:09}"));
            }
        }
        s
    }
}

/// Number of significant limbs in `a` (trailing zeros ignored).
fn trim_len(a: &[u32]) -> usize {
    let mut n = a.len();
    while n > 0 && a[n - 1] == 0 {
        n -= 1;
    }
    n
}

/// Bit `i` of a little-endian limb slice.
fn bit_at(a: &[u32], i: usize) -> bool {
    let limb = i / 32;
    if limb >= a.len() {
        return false;
    }
    (a[limb] >> (i % 32)) & 1 == 1
}

/// `v <<= 1` on a little-endian limb vec.
fn shift_left_one(v: &mut Vec<u32>) {
    let mut carry: u32 = 0;
    for limb in v.iter_mut() {
        let c = *limb >> 31;
        *limb = (*limb << 1) | carry;
        carry = c;
    }
    if carry != 0 {
        v.push(carry);
    }
}
