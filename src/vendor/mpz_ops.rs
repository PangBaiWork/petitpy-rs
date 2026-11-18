//! `mpz_*_inpl` operations: port of the second half of
//! `vendor/micropython/py/mpz.c` (everything after the `mpn_*`
//! helpers and the setters in [`super::mpz`]).
//!
//! Each operation is a free function with the C name minus the
//! `mpz_` prefix (e.g. C `mpz_add_inpl` is [`add_inpl`]), plus
//! ergonomic methods on [`BigInt`] and the usual operator traits.
//! The mapping is listed in `scripts/vendorc_result.txt`.
//!
//! Aliasing: the C functions allow `dest` to be the same object as
//! an operand (`mpz_mul_inpl(dest, dest, x)`). Rust borrows forbid
//! that at the API boundary, so the free functions require `dest`
//! distinct from the operands; the `*_assign` methods and the
//! internal uses in `pow_inpl` / `pow3_inpl` clone first, exactly
//! as the C's `mpz_mul_inpl` clones on aliasing. Observable behaviour
//! is identical.
//!
//! Error paths: the C `mpz_divmod_inpl` asserts `rhs != 0` and the
//! caller raises `ZeroDivisionError`; `mpz_pow3_inpl` with a zero
//! modulus would divide by zero the same way and its caller raises
//! `ValueError`. The Rust versions return `Err(PyError::PyRaise)` in
//! those cases (the exception object itself is the runtime's job).

use std::cmp::Ordering;
use std::ops::{Add, BitAnd, BitOr, BitXor, Mul, Neg, Not, Shl, Shr, Sub};

use super::mpn;
use super::mpz::BigInt;
use crate::pyerr::{PyError, PyResult};

/// `mpz_abs_inpl`: `dest = abs(z)`.
pub fn abs_inpl(dest: &mut BigInt, z: &BigInt) {
    dest.set(z);
    dest.neg = false;
}

/// `mpz_neg_inpl`: `dest = -z`.
pub fn neg_inpl(dest: &mut BigInt, z: &BigInt) {
    dest.set(z);
    if !dest.dig.is_empty() {
        dest.neg = !dest.neg;
    }
}

/// `mpz_not_inpl`: `dest = ~z` (= `-z - 1`).
pub fn not_inpl(dest: &mut BigInt, z: &BigInt) {
    dest.set(z);
    if dest.dig.is_empty() {
        dest.dig.push(1);
        dest.neg = true;
    } else if dest.neg {
        dest.neg = false;
        let tmp = dest.dig.clone();
        let len = mpn::sub(&mut dest.dig, &tmp, &[1]);
        dest.dig.truncate(len);
    } else {
        dest.dig.resize(dest.dig.len() + 1, 0);
        let tmp = dest.dig.clone();
        let len = mpn::add(&mut dest.dig, &tmp, &[1]);
        dest.dig.truncate(len);
        dest.neg = true;
    }
}

/// `mpz_shl_inpl`: `dest = lhs << rhs`.
pub fn shl_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: u64) {
    if lhs.dig.is_empty() || rhs == 0 {
        dest.set(lhs);
    } else {
        let extra = ((rhs + 31) / 32) as usize;
        dest.dig.resize(lhs.dig.len() + extra, 0);
        let len = mpn::shl(&mut dest.dig, &lhs.dig, rhs);
        dest.dig.truncate(len);
        dest.neg = lhs.neg;
    }
}

/// `mpz_shr_inpl`: `dest = lhs >> rhs` (arithmetic shift, rounding
/// toward negative infinity for negative values).
pub fn shr_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: u64) {
    if lhs.dig.is_empty() || rhs == 0 {
        dest.set(lhs);
    } else {
        dest.dig.resize(lhs.dig.len(), 0);
        let len = mpn::shr(&mut dest.dig, &lhs.dig, rhs);
        dest.dig.truncate(len);
        dest.neg = lhs.neg;
        if dest.neg {
            // Arithmetic shift right, rounding to negative infinity.
            let n_whole = (rhs / 32) as usize;
            let n_part = (rhs % 32) as u32;
            let mut round_up = false;
            for i in 0..lhs.dig.len().min(n_whole) {
                if lhs.dig[i] != 0 {
                    round_up = true;
                    break;
                }
            }
            if n_whole < lhs.dig.len() && (lhs.dig[n_whole] & ((1u32 << n_part) - 1)) != 0 {
                round_up = true;
            }
            if round_up {
                if dest.dig.is_empty() {
                    // dest == 0, so add 1 by hand (answer will be -1).
                    dest.dig.push(1);
                } else {
                    // dest > 0, so can use mpn_add to add 1.
                    // mpn::add needs out.len() >= a.len() + 1 for
                    // the final carry — the truncate above shrank
                    // dest to its logical length, so regrow the
                    // spare digit the vendor's
                    // mpz_need_dig(dest, lhs->len) provides
                    // (mpz.c:1101); without it the carry write
                    // panics (index out of bounds) when the
                    // shifted value is all 1s, e.g.
                    // (-0xffffffffffffffff) >> 32.
                    let tmp = dest.dig.clone();
                    dest.dig.resize(tmp.len() + 1, 0);
                    let len = mpn::add(&mut dest.dig, &tmp, &[1]);
                    dest.dig.truncate(len);
                }
            }
        }
    }
}

/// `mpz_add_inpl`: `dest = lhs + rhs`.
pub fn add_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    let (lhs, rhs) = if mpn::cmp(&lhs.dig, &rhs.dig) == Ordering::Less {
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    if lhs.neg == rhs.neg {
        dest.dig.resize(lhs.dig.len() + 1, 0);
        let len = mpn::add(&mut dest.dig, &lhs.dig, &rhs.dig);
        dest.dig.truncate(len);
    } else {
        dest.dig.resize(lhs.dig.len(), 0);
        let len = mpn::sub(&mut dest.dig, &lhs.dig, &rhs.dig);
        dest.dig.truncate(len);
    }
    dest.neg = lhs.neg && !dest.dig.is_empty();
}

/// `mpz_sub_inpl`: `dest = lhs - rhs`.
pub fn sub_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    let mut neg = false;
    let (lhs, rhs) = if mpn::cmp(&lhs.dig, &rhs.dig) == Ordering::Less {
        neg = true;
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    if lhs.neg != rhs.neg {
        dest.dig.resize(lhs.dig.len() + 1, 0);
        let len = mpn::add(&mut dest.dig, &lhs.dig, &rhs.dig);
        dest.dig.truncate(len);
    } else {
        dest.dig.resize(lhs.dig.len(), 0);
        let len = mpn::sub(&mut dest.dig, &lhs.dig, &rhs.dig);
        dest.dig.truncate(len);
    }
    if dest.dig.is_empty() {
        dest.neg = false;
    } else if neg {
        dest.neg = !lhs.neg;
    } else {
        dest.neg = lhs.neg;
    }
}

/// `mpz_and_inpl`: `dest = lhs & rhs`. (`MICROPY_OPT_MPZ_BITWISE`
/// is 0, so the general `mpn_and_neg` form is used for every AND.)
pub fn and_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    // Make sure lhs has the most digits.
    let (lhs, rhs) = if lhs.dig.len() < rhs.dig.len() {
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    dest.dig.resize(lhs.dig.len() + usize::from(lhs.neg || rhs.neg), 0);
    let len = mpn::and_neg(
        &mut dest.dig,
        &lhs.dig,
        &rhs.dig,
        u64::from(lhs.neg && rhs.neg),
        u64::from(lhs.neg),
        u64::from(rhs.neg),
    );
    dest.dig.truncate(len);
    dest.neg = lhs.neg && rhs.neg;
}

/// `mpz_or_inpl`: `dest = lhs | rhs`. (`MICROPY_OPT_MPZ_BITWISE`
/// is 0, so the general `mpn_or_neg` form is used for every OR.)
pub fn or_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    // Make sure lhs has the most digits.
    let (lhs, rhs) = if lhs.dig.len() < rhs.dig.len() {
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    dest.dig.resize(lhs.dig.len() + usize::from(lhs.neg || rhs.neg), 0);
    let len = mpn::or_neg(
        &mut dest.dig,
        &lhs.dig,
        &rhs.dig,
        u64::from(lhs.neg || rhs.neg),
        u64::from(lhs.neg),
        u64::from(rhs.neg),
    );
    dest.dig.truncate(len);
    dest.neg = lhs.neg || rhs.neg;
}

/// `mpz_xor_inpl`: `dest = lhs ^ rhs`. (`MICROPY_OPT_MPZ_BITWISE`
/// is 0, so the general `mpn_xor_neg` form is used for every XOR.)
pub fn xor_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    // Make sure lhs has the most digits.
    let (lhs, rhs) = if lhs.dig.len() < rhs.dig.len() {
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    dest.dig.resize(lhs.dig.len() + usize::from(lhs.neg || rhs.neg), 0);
    let len = mpn::xor_neg(
        &mut dest.dig,
        &lhs.dig,
        &rhs.dig,
        u64::from(lhs.neg != rhs.neg),
        u64::from(!lhs.neg),
        u64::from(!rhs.neg),
    );
    dest.dig.truncate(len);
    dest.neg = lhs.neg ^ rhs.neg;
}

/// `mpz_mul_inpl`: `dest = lhs * rhs`.
pub fn mul_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    if lhs.dig.is_empty() || rhs.dig.is_empty() {
        dest.set_from_int(0);
        return;
    }
    dest.dig.resize(lhs.dig.len() + rhs.dig.len(), 0);
    dest.dig.fill(0);
    let len = mpn::mul(&mut dest.dig, &lhs.dig, &rhs.dig);
    dest.dig.truncate(len);
    dest.neg = lhs.neg != rhs.neg;
}

/// `mpz_pow_inpl`: `dest = lhs ** rhs`.
/// Returns 0 if `lhs == 0` or `rhs < 0` (the C's behaviour; the
/// caller converts a negative exponent to a float before calling),
/// and 1 if `rhs == 0` (except `lhs == 0`, which returns 0).
pub fn pow_inpl(dest: &mut BigInt, lhs: &BigInt, rhs: &BigInt) {
    if lhs.dig.is_empty() || rhs.neg {
        dest.set_from_int(0);
        return;
    }
    if rhs.dig.is_empty() {
        dest.set_from_int(1);
        return;
    }
    let mut x = lhs.clone();
    let mut n = rhs.clone();
    dest.set_from_int(1);
    while !n.dig.is_empty() {
        if (n.dig[0] & 1) != 0 {
            let t = dest.clone();
            mul_inpl(dest, &t, &x);
        }
        let tmp = n.dig.clone();
        let len = mpn::shr(&mut n.dig, &tmp, 1);
        n.dig.truncate(len);
        if n.dig.is_empty() {
            break;
        }
        let t = x.clone();
        let mut x2 = BigInt::zero();
        mul_inpl(&mut x2, &t, &t);
        x = x2;
    }
}

/// `mpz_pow3_inpl`: `dest = (lhs ** rhs) % mod`.
/// Returns 0 if `lhs == 0`, `rhs < 0`, or `mod == 1`; 1 if
/// `rhs == 0`. A zero modulus is an error (`Err(PyRaise)`) — the C
/// would hit the divmod assert; the real caller raises `ValueError`
/// before calling.
pub fn pow3_inpl(
    dest: &mut BigInt,
    lhs: &BigInt,
    rhs: &BigInt,
    modulus: &BigInt,
) -> PyResult<()> {
    if lhs.dig.is_empty()
        || rhs.neg
        || (modulus.dig.len() == 1 && modulus.dig[0] == 1)
    {
        dest.set_from_int(0);
        return Ok(());
    }
    dest.set_from_int(1);
    if rhs.dig.is_empty() {
        return Ok(());
    }
    let mut x = lhs.clone();
    let mut n = rhs.clone();
    let mut quo = BigInt::zero();
    while !n.dig.is_empty() {
        if (n.dig[0] & 1) != 0 {
            let t = dest.clone();
            mul_inpl(dest, &t, &x);
            let t = dest.clone();
            divmod_inpl(&mut quo, dest, &t, modulus)?;
        }
        let tmp = n.dig.clone();
        let len = mpn::shr(&mut n.dig, &tmp, 1);
        n.dig.truncate(len);
        if n.dig.is_empty() {
            break;
        }
        let t = x.clone();
        let mut x2 = BigInt::zero();
        mul_inpl(&mut x2, &t, &t);
        x = x2;
        let t = x.clone();
        divmod_inpl(&mut quo, &mut x, &t, modulus)?;
    }
    Ok(())
}

/// `mpz_divmod_inpl`: `quo * rhs + rem = lhs`, `0 <= rem < rhs`
/// (Python-style modulo: the remainder takes the sign of `rhs`).
/// `Err(PyRaise)` if `rhs == 0` (the C asserts; the caller raises
/// `ZeroDivisionError`).
pub fn divmod_inpl(
    quo: &mut BigInt,
    rem: &mut BigInt,
    lhs: &BigInt,
    rhs: &BigInt,
) -> PyResult<()> {
    if rhs.dig.is_empty() {
        return Err(PyError::PyRaise);
    }
    quo.dig.resize(lhs.dig.len() + 1, 0);
    quo.dig.fill(0);
    quo.neg = false;
    rem.set(lhs);
    // mpn_div needs one scratch digit past the numerator length.
    rem.dig.resize(lhs.dig.len() + 1, 0);
    let (qlen, rlen) = mpn::div(&mut rem.dig, lhs.dig.len(), &rhs.dig, &mut quo.dig);
    quo.dig.truncate(qlen);
    rem.dig.truncate(rlen);
    rem.neg = rem.neg && !rem.dig.is_empty();

    // Check signs and do Python style modulo.
    if lhs.neg != rhs.neg {
        quo.neg = !quo.dig.is_empty();
        if !rem.dig.is_empty() {
            let mpzone = BigInt::from_int(-1);
            let t = quo.clone();
            add_inpl(quo, &t, &mpzone);
            let t = rem.clone();
            add_inpl(rem, &t, rhs);
        }
    }
    Ok(())
}

impl BigInt {
    /// `abs(self)`.
    pub fn abs(&self) -> BigInt {
        let mut out = BigInt::zero();
        abs_inpl(&mut out, self);
        out
    }
    /// `self ** rhs` (mpz semantics: 0 for a zero base or a
    /// negative exponent, 1 for a zero exponent with a non-zero
    /// base).
    pub fn pow(&self, rhs: &BigInt) -> BigInt {
        let mut out = BigInt::zero();
        pow_inpl(&mut out, self, rhs);
        out
    }

    /// `(self ** rhs) % modulus` (mpz semantics).
    pub fn pow3(&self, rhs: &BigInt, modulus: &BigInt) -> PyResult<BigInt> {
        let mut out = BigInt::zero();
        pow3_inpl(&mut out, self, rhs, modulus)?;
        Ok(out)
    }

    /// `(self // rhs, self % rhs)` (Python-style, floor division).
    pub fn divmod(&self, rhs: &BigInt) -> PyResult<(BigInt, BigInt)> {
        let mut quo = BigInt::zero();
        let mut rem = BigInt::zero();
        divmod_inpl(&mut quo, &mut rem, self, rhs)?;
        Ok((quo, rem))
    }

    /// `self += rhs`.
    pub fn add_assign(&mut self, rhs: &BigInt) {
        let lhs = self.clone();
        add_inpl(self, &lhs, rhs);
    }

    /// `self -= rhs`.
    pub fn sub_assign(&mut self, rhs: &BigInt) {
        let lhs = self.clone();
        sub_inpl(self, &lhs, rhs);
    }

    /// `self *= rhs`.
    pub fn mul_assign(&mut self, rhs: &BigInt) {
        let lhs = self.clone();
        mul_inpl(self, &lhs, rhs);
    }
}

impl Neg for BigInt {
    type Output = BigInt;
    fn neg(self) -> BigInt {
        let mut out = BigInt::zero();
        neg_inpl(&mut out, &self);
        out
    }
}

impl Not for BigInt {
    type Output = BigInt;
    fn not(self) -> BigInt {
        let mut out = BigInt::zero();
        not_inpl(&mut out, &self);
        out
    }
}

impl Add for BigInt {
    type Output = BigInt;
    fn add(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        add_inpl(&mut out, &self, &rhs);
        out
    }
}

impl Sub for BigInt {
    type Output = BigInt;
    fn sub(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        sub_inpl(&mut out, &self, &rhs);
        out
    }
}

impl Mul for BigInt {
    type Output = BigInt;
    fn mul(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        mul_inpl(&mut out, &self, &rhs);
        out
    }
}

impl BitAnd for BigInt {
    type Output = BigInt;
    fn bitand(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        and_inpl(&mut out, &self, &rhs);
        out
    }
}

impl BitOr for BigInt {
    type Output = BigInt;
    fn bitor(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        or_inpl(&mut out, &self, &rhs);
        out
    }
}

impl BitXor for BigInt {
    type Output = BigInt;
    fn bitxor(self, rhs: BigInt) -> BigInt {
        let mut out = BigInt::zero();
        xor_inpl(&mut out, &self, &rhs);
        out
    }
}

impl Shl<u64> for BigInt {
    type Output = BigInt;
    fn shl(self, rhs: u64) -> BigInt {
        let mut out = BigInt::zero();
        shl_inpl(&mut out, &self, rhs);
        out
    }
}

impl Shr<u64> for BigInt {
    type Output = BigInt;
    fn shr(self, rhs: u64) -> BigInt {
        let mut out = BigInt::zero();
        shr_inpl(&mut out, &self, rhs);
        out
    }
}

impl<'a> Add<&'a BigInt> for &'a BigInt {
    type Output = BigInt;
    fn add(self, rhs: &'a BigInt) -> BigInt {
        let mut out = BigInt::zero();
        add_inpl(&mut out, self, rhs);
        out
    }
}

impl<'a> Sub<&'a BigInt> for &'a BigInt {
    type Output = BigInt;
    fn sub(self, rhs: &'a BigInt) -> BigInt {
        let mut out = BigInt::zero();
        sub_inpl(&mut out, self, rhs);
        out
    }
}

impl<'a> Mul<&'a BigInt> for &'a BigInt {
    type Output = BigInt;
    fn mul(self, rhs: &'a BigInt) -> BigInt {
        let mut out = BigInt::zero();
        mul_inpl(&mut out, self, rhs);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::mpz::bi;
    use super::*;

    #[test]
    fn abs_neg_not_cases() {
        assert_eq!(bi("-5").abs().to_string(), "5");
        assert_eq!(bi("5").abs().to_string(), "5");
        assert_eq!(bi("0").abs().to_string(), "0");
        assert_eq!((-bi("5")).to_string(), "-5");
        assert_eq!((-bi("-5")).to_string(), "5");
        assert_eq!((-bi("0")).to_string(), "0");
        assert_eq!((!bi("0")).to_string(), "-1");
        assert_eq!((!bi("-1")).to_string(), "0");
        assert_eq!((!bi("5")).to_string(), "-6");
        assert_eq!((!bi("-5")).to_string(), "4");
        assert_eq!((!bi("12345678901234567890")).to_string(), "-12345678901234567891");
    }

    #[test]
    fn add_sub_cases() {
        assert_eq!((bi("0") + bi("0")).to_string(), "0");
        assert_eq!((bi("1") + bi("2")).to_string(), "3");
        assert_eq!((bi("-1") + bi("1")).to_string(), "0");
        assert_eq!((bi("-5") + bi("-3")).to_string(), "-8");
        assert_eq!((bi("5") + bi("-8")).to_string(), "-3");
        assert_eq!((bi("18446744073709551615") + bi("1")).to_string(), "18446744073709551616");
        assert_eq!((bi("1") - bi("2")).to_string(), "-1");
        assert_eq!((bi("-1") - bi("-1")).to_string(), "0");
        assert_eq!((bi("-5") - bi("3")).to_string(), "-8");
        assert_eq!((bi("5") - bi("-3")).to_string(), "8");
        assert_eq!((bi("0") - bi("18446744073709551616")).to_string(), "-18446744073709551616");
        // Carry across digit boundaries.
        assert_eq!(
            (bi("4294967295") + bi("4294967295")).to_string(),
            "8589934590"
        );
        assert_eq!(
            (bi("18446744073709551616") - bi("1")).to_string(),
            "18446744073709551615"
        );
    }

    #[test]
    fn mul_cases() {
        assert_eq!((bi("0") * bi("123")).to_string(), "0");
        assert_eq!((bi("123") * bi("0")).to_string(), "0");
        assert_eq!((bi("-2") * bi("3")).to_string(), "-6");
        assert_eq!((bi("-2") * bi("-3")).to_string(), "6");
        assert_eq!((bi("4294967295") * bi("4294967295")).to_string(), "18446744065119617025");
        assert_eq!(
            (bi("12345678901234567890") * bi("98765432109876543210")).to_string(),
            "1219326311370217952237463801111263526900"
        );
    }

    #[test]
    fn shl_shr_cases() {
        assert_eq!((bi("1") << 0u64).to_string(), "1");
        assert_eq!((bi("1") << 1u64).to_string(), "2");
        assert_eq!((bi("1") << 32u64).to_string(), "4294967296");
        assert_eq!((bi("1") << 64u64).to_string(), "18446744073709551616");
        assert_eq!((bi("1") << 100u64).to_string(), "1267650600228229401496703205376");
        assert_eq!((bi("-1") << 10u64).to_string(), "-1024");
        assert_eq!((bi("0") << 100u64).to_string(), "0");
        assert_eq!((bi("1024") >> 10u64).to_string(), "1");
        assert_eq!((bi("1025") >> 10u64).to_string(), "1");
        assert_eq!((bi("-1025") >> 10u64).to_string(), "-2"); // floor
        assert_eq!((bi("-1024") >> 10u64).to_string(), "-1");
        assert_eq!((bi("-1") >> 10u64).to_string(), "-1");
        assert_eq!((bi("-1") >> 100u64).to_string(), "-1");
        assert_eq!((bi("1") >> 100u64).to_string(), "0");
        assert_eq!((bi("18446744073709551616") >> 64u64).to_string(), "1");
        assert_eq!((bi("18446744073709551616") >> 65u64).to_string(), "0");
        assert_eq!((bi("-18446744073709551616") >> 64u64).to_string(), "-1");
        assert_eq!((bi("-18446744073709551617") >> 64u64).to_string(), "-2");
    }

    #[test]
    fn bitwise_cases() {
        // Positive.
        assert_eq!((bi("10") & bi("12")).to_string(), "8");
        assert_eq!((bi("10") | bi("12")).to_string(), "14");
        assert_eq!((bi("10") ^ bi("12")).to_string(), "6");
        // Mixed signs.
        assert_eq!((bi("-1") & bi("5")).to_string(), "5");
        assert_eq!((bi("-1") | bi("5")).to_string(), "-1");
        assert_eq!((bi("-1") ^ bi("5")).to_string(), "-6");
        assert_eq!((bi("-5") & bi("-3")).to_string(), "-7");
        assert_eq!((bi("-5") | bi("-3")).to_string(), "-1");
        assert_eq!((bi("-5") ^ bi("-3")).to_string(), "6");
        assert_eq!((bi("5") & bi("-3")).to_string(), "5");
        assert_eq!((bi("5") | bi("-3")).to_string(), "-3");
        assert_eq!((bi("5") ^ bi("-3")).to_string(), "-8");
        // Zero and -1.
        assert_eq!((bi("0") & bi("-1")).to_string(), "0");
        assert_eq!((bi("0") | bi("-1")).to_string(), "-1");
        assert_eq!((bi("0") ^ bi("-1")).to_string(), "-1");
        // Large values crossing digit boundaries.
        assert_eq!(
            (bi("18446744073709551615") & bi("18446744073709551616")).to_string(),
            "0"
        );
        assert_eq!(
            (bi("18446744073709551615") | bi("18446744073709551616")).to_string(),
            "36893488147419103231"
        );
        assert_eq!(
            (bi("-18446744073709551616") & bi("18446744073709551615")).to_string(),
            "0"
        );
    }

    #[test]
    fn free_functions_cover_every_mpz_fn() {
        // Every ported `mpz_*_inpl` free function, called directly
        // (the operator tests above exercise the same code through
        // the trait impls; this test pins the C-named entry points).
        let mut out = BigInt::zero();
        abs_inpl(&mut out, &bi("-7"));
        assert_eq!(out.to_string(), "7");
        neg_inpl(&mut out, &bi("7"));
        assert_eq!(out.to_string(), "-7");
        not_inpl(&mut out, &bi("7"));
        assert_eq!(out.to_string(), "-8");
        shl_inpl(&mut out, &bi("1"), 33);
        assert_eq!(out.to_string(), "8589934592");
        shr_inpl(&mut out, &bi("-1025"), 10);
        assert_eq!(out.to_string(), "-2");
        add_inpl(&mut out, &bi("2"), &bi("3"));
        assert_eq!(out.to_string(), "5");
        sub_inpl(&mut out, &bi("2"), &bi("3"));
        assert_eq!(out.to_string(), "-1");
        and_inpl(&mut out, &bi("10"), &bi("12"));
        assert_eq!(out.to_string(), "8");
        or_inpl(&mut out, &bi("10"), &bi("12"));
        assert_eq!(out.to_string(), "14");
        xor_inpl(&mut out, &bi("10"), &bi("12"));
        assert_eq!(out.to_string(), "6");
        mul_inpl(&mut out, &bi("6"), &bi("7"));
        assert_eq!(out.to_string(), "42");
        pow_inpl(&mut out, &bi("3"), &bi("5"));
        assert_eq!(out.to_string(), "243");
        pow3_inpl(&mut out, &bi("2"), &bi("10"), &bi("1000")).unwrap();
        assert_eq!(out.to_string(), "24");
        let mut quo = BigInt::zero();
        let mut rem = BigInt::zero();
        divmod_inpl(&mut quo, &mut rem, &bi("-7"), &bi("3")).unwrap();
        assert_eq!((quo.to_string(), rem.to_string()), ("-3".to_string(), "2".to_string()));
    }

    #[test]
    fn divmod_cases() {
        assert_eq!(bi("10").divmod(&bi("3")).unwrap(), (bi("3"), bi("1")));
        assert_eq!(bi("-10").divmod(&bi("3")).unwrap(), (bi("-4"), bi("2")));
        assert_eq!(bi("10").divmod(&bi("-3")).unwrap(), (bi("-4"), bi("-2")));
        assert_eq!(bi("-10").divmod(&bi("-3")).unwrap(), (bi("3"), bi("-1")));
        assert_eq!(bi("7").divmod(&bi("7")).unwrap(), (bi("1"), bi("0")));
        assert_eq!(bi("5").divmod(&bi("7")).unwrap(), (bi("0"), bi("5")));
        assert_eq!(bi("-5").divmod(&bi("7")).unwrap(), (bi("-1"), bi("2")));
        assert_eq!(bi("0").divmod(&bi("7")).unwrap(), (bi("0"), bi("0")));
        assert_eq!(
            bi("18446744073709551616").divmod(&bi("3")).unwrap(),
            (bi("6148914691236517205"), bi("1"))
        );
        assert_eq!(
            bi("-18446744073709551616").divmod(&bi("3")).unwrap(),
            (bi("-6148914691236517206"), bi("2"))
        );
        // Division by zero is an error (caller raises ZeroDivisionError).
        assert!(bi("1").divmod(&bi("0")).is_err());
        assert!(bi("0").divmod(&bi("0")).is_err());
    }

    #[test]
    fn pow_cases() {
        assert_eq!(bi("2").pow(&bi("10")).to_string(), "1024");
        assert_eq!(bi("2").pow(&bi("0")).to_string(), "1");
        assert_eq!(bi("0").pow(&bi("5")).to_string(), "0");
        // mpz semantics: 0**0 = 0 (the C returns 0 when lhs is zero).
        assert_eq!(bi("0").pow(&bi("0")).to_string(), "0");
        // mpz semantics: negative exponent = 0 (caller converts to
        // float before calling).
        assert_eq!(bi("2").pow(&bi("-1")).to_string(), "0");
        assert_eq!(bi("-2").pow(&bi("3")).to_string(), "-8");
        assert_eq!(bi("-2").pow(&bi("4")).to_string(), "16");
        assert_eq!(bi("3").pow(&bi("40")).to_string(), "12157665459056928801");
        assert_eq!(bi("2").pow(&bi("100")).to_string(), "1267650600228229401496703205376");
    }

    #[test]
    fn pow3_cases() {
        assert_eq!(bi("2").pow3(&bi("10"), &bi("1000")).unwrap().to_string(), "24");
        assert_eq!(bi("2").pow3(&bi("0"), &bi("5")).unwrap().to_string(), "1");
        assert_eq!(bi("0").pow3(&bi("5"), &bi("7")).unwrap().to_string(), "0");
        // mpz semantics: negative exponent = 0.
        assert_eq!(bi("2").pow3(&bi("-1"), &bi("5")).unwrap().to_string(), "0");
        // mpz semantics: mod == 1 -> 0.
        assert_eq!(bi("2").pow3(&bi("10"), &bi("1")).unwrap().to_string(), "0");
        // Negative modulus (Python-style remainder).
        assert_eq!(bi("2").pow3(&bi("3"), &bi("-5")).unwrap().to_string(), "-2");
        assert_eq!(bi("123456789").pow3(&bi("987654321"), &bi("1000000007")).unwrap().to_string(), "652541198");
        // Zero modulus is an error (caller raises ValueError).
        assert!(bi("2").pow3(&bi("10"), &bi("0")).is_err());
        // ... except with a zero exponent, where the C returns 1
        // without touching the modulus.
        assert_eq!(bi("2").pow3(&bi("0"), &bi("0")).unwrap().to_string(), "1");
    }

    #[test]
    fn assign_and_trait_cases() {
        let mut a = bi("5");
        a.add_assign(&bi("7"));
        assert_eq!(a.to_string(), "12");
        a.sub_assign(&bi("2"));
        assert_eq!(a.to_string(), "10");
        a.mul_assign(&bi("-3"));
        assert_eq!(a.to_string(), "-30");
        assert_eq!((-bi("5")).to_string(), "-5");
        assert_eq!((!bi("5")).to_string(), "-6");
        assert_eq!((bi("5") + bi("7")).to_string(), "12");
        assert_eq!((bi("5") - bi("7")).to_string(), "-2");
        assert_eq!((bi("5") * bi("7")).to_string(), "35");
        assert_eq!((bi("5") & bi("3")).to_string(), "1");
        assert_eq!((bi("5") | bi("3")).to_string(), "7");
        assert_eq!((bi("5") ^ bi("3")).to_string(), "6");
        assert_eq!((bi("1") << 10u64).to_string(), "1024");
        assert_eq!((bi("1024") >> 10u64).to_string(), "1");
        assert_eq!(&bi("5") + &bi("7"), bi("12"));
    }
}
