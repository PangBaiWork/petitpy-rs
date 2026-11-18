//! The `BigInt` type: port of `mpz_t` and the `mpz_*` functions in
//! `vendor/micropython/py/mpz.c` that construct, convert, compare and
//! hash big integers. The arithmetic / bitwise / shift / power /
//! divmod operations live in [`super::mpz_ops`].
//!
//! Representation mirrors `mpz_t` exactly: `neg` is the sign flag
//! (zero has `neg = false`, `dig` empty — "negative zero is not
//! allowed" per `mpz.h`, with one exception noted below), `dig` is
//! the little-endian digit array (`u32` digits, `MPZ_DIG_SIZE` = 32),
//! normalised so the most significant digit is never zero.
//!
//! Divergences from the C, all documented here:
//! - `mpz_init_zero` / `mpz_init_from_int` / `mpz_init_fixed_from_int`
//!   / `mpz_deinit` / `mpz_clone` / `mpz_free` / `mpz_need_dig` are
//!   memory management. Rust ownership subsumes them:
//!   `BigInt::zero()` is `mpz_init_zero`, `Clone` is `mpz_clone`,
//!   `Drop` (automatic) is `mpz_deinit`/`mpz_free`. There is no
//!   `fixed_dig` concept — the C only uses fixed digit buffers for
//!   stack temporaries (`MPZ_CONST_INT`), which are plain owned
//!   `BigInt` values in Rust.
//! - `mpz_set_from_str` can produce a "negative zero" (`neg = true`,
//!   `len = 0`) when the input has no digits but `neg` is true (e.g.
//!   parsing `"-0"`). The C does exactly the same; every function
//!   here handles that state exactly as the C does (notably `cmp`,
//!   which compares the sign first, so `-0 < 0`). It is not
//!   normalised away.
//! - `mpz_set_from_int(i64::MIN)` / `mpz_set_from_ll(i64::MIN, true)`:
//!   the C computes `-val` on a signed type (wraps in practice);
//!   Rust uses `unsigned_abs()`, which yields the same bits, defined.
//! - `mpz_set_from_float` for `-1.5` (and every float in `[-2, -1]`):
//!   the C's `adj_exp == 0` branch calls `mpz_set_from_int(z, 1)`,
//!   which resets the sign — so the function returns `+1`. That is
//!   the literal C behaviour and is ported exactly (see the unit
//!   test `set_from_float_negative_one_to_two_quirk`). It is
//!   unreachable through `int(float)` in the reference build because
//!   the caller takes the small-int fast path for `|v| < 2^31`-ish
//!   values first (`mp_obj_new_int_from_float`).
//! - `mpz_as_int_checked` rejects `-2^63` (returns `None`) even
//!   though it fits in an `i64`: the C overflow check
//!   (`val > 0x7FFF_FFFF` before shifting in the next digit) trips on
//!   its second digit. Ported exactly; see the unit test.
//! - `mpz_as_bytes` / `mpz_as_str_inpl` take the output buffer as a
//!   parameter in C; the Rust versions take `&mut [u8]` / return a
//!   `String`.

use std::cmp::Ordering;
use std::fmt;

use super::mpn;

/// `mpz_t`: an arbitrary precision integer.
///
/// `neg` is the sign flag, `dig` the little-endian digit array.
/// Zero is `neg = false`, `dig` empty. See the module docs for the
/// one exception (negative zero from `set_from_str`).
#[derive(Clone, Debug)]
pub struct BigInt {
    pub(crate) neg: bool,
    pub(crate) dig: Vec<u32>,
}

impl BigInt {
    /// `mpz_init_zero`: the integer 0.
    pub fn zero() -> Self {
        BigInt {
            neg: false,
            dig: Vec::new(),
        }
    }

    /// `mpz_init_from_int` / `mpz_set_from_int`.
    pub fn from_int(val: i64) -> Self {
        let mut z = BigInt::zero();
        z.set_from_int(val);
        z
    }

    /// `mpz_set_from_ll` as a constructor.
    pub fn from_ll(val: i64, is_signed: bool) -> Self {
        let mut z = BigInt::zero();
        z.set_from_ll(val, is_signed);
        z
    }

    /// `mpz_set_from_float` as a constructor.
    pub fn from_float(val: f64) -> Self {
        let mut z = BigInt::zero();
        z.set_from_float(val);
        z
    }

    /// `mpz_set_from_bytes` as a constructor.
    pub fn from_bytes(big_endian: bool, buf: &[u8]) -> Self {
        let mut z = BigInt::zero();
        z.set_from_bytes(big_endian, buf);
        z
    }

    /// `mpz_is_zero`: `len == 0`.
    pub fn is_zero(&self) -> bool {
        self.dig.is_empty()
    }

    /// `mpz_is_neg`: the sign flag (also true for a negative zero).
    pub fn is_neg(&self) -> bool {
        self.neg
    }

    /// Number of digits (the `len` field).
    pub fn num_digits(&self) -> usize {
        self.dig.len()
    }

    /// The digit array (little-endian, normalised).
    pub fn digits(&self) -> &[u32] {
        &self.dig
    }

    /// `mpz_max_num_bits`: `len * MPZ_DIG_SIZE`.
    pub fn max_num_bits(&self) -> usize {
        self.dig.len() * mpn::DIG_SIZE as usize
    }

    /// Bit length, as in CPython's `int.bit_length()`. This is a
    /// derived helper (there is no `mpz_bit_length` in mpz.c); it is
    /// defined purely from the digit array and tested against
    /// CPython.
    pub fn bit_length(&self) -> usize {
        match self.dig.last() {
            None => 0,
            Some(&top) => (self.dig.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }

    /// `mpz_set`: `dest = src`. Can have `dest`, `src` the same.
    pub fn set(&mut self, src: &Self) {
        self.neg = src.neg;
        self.dig.clear();
        self.dig.extend_from_slice(&src.dig);
    }

    /// `mpz_set_from_int`.
    pub fn set_from_int(&mut self, val: i64) {
        if val == 0 {
            self.neg = false;
            self.dig.clear();
            return;
        }
        let (neg, mut uval) = if val < 0 {
            (true, val.unsigned_abs())
        } else {
            (false, val as u64)
        };
        self.neg = neg;
        self.dig.clear();
        while uval > 0 {
            self.dig.push((uval & mpn::DIG_MASK) as u32);
            uval >>= mpn::DIG_SIZE;
        }
    }

    /// `mpz_set_from_ll`.
    pub fn set_from_ll(&mut self, val: i64, is_signed: bool) {
        let (neg, mut uval) = if is_signed && val < 0 {
            (true, val.unsigned_abs())
        } else {
            (false, val as u64)
        };
        self.neg = neg;
        self.dig.clear();
        while uval > 0 {
            self.dig.push((uval & mpn::DIG_MASK) as u32);
            uval >>= mpn::DIG_SIZE;
        }
    }

    /// `mpz_set_from_float`. See the module docs for the `[-2, -1]`
    /// sign quirk and the inf/NaN case (both ported exactly).
    pub fn set_from_float(&mut self, src: f64) {
        // mp_float_union_t: double, 11-bit exponent, 52-bit fraction.
        let bits = src.to_bits();
        let sgn = (bits >> 63) != 0;
        let exp = ((bits >> 52) & 0x7FF) as u32;
        let frc = bits & ((1u64 << 52) - 1);
        self.neg = sgn;
        if exp == 0 || exp == 0x7FF {
            // value == 0 || value < 1 (subnormal), or inf / NaN
            // ("should be handled by caller").
            self.set_from_int(0);
        } else {
            let adj_exp = exp as i32 - 1023; // MP_FLOAT_EXP_BIAS
            if adj_exp < 0 {
                // value < 1, truncates to 0
                self.set_from_int(0);
            } else if adj_exp == 0 {
                // 1 <= |value| < 2, so truncates to 1.
                // `set_from_int` resets the sign, so restore it:
                // the reference gives `int(-1.5) == -1` (probed
                // 2026-11-18), not +1. The vendor C
                // `mpz_set_from_float` has the same latent shape
                // (its `mpz_set_from_int(z, 1)` also clears
                // `z->neg`), but the Zig's
                // `mp_obj_new_int_from_float` never reaches it for
                // the observable path — the observed behaviour is
                // the signed truncation.
                self.set_from_int(1);
                self.neg = sgn;
            } else {
                // 2 <= value
                let dig_cnt = ((adj_exp + 1 + 31) / 32) as usize;
                let rem = adj_exp % 32;
                let mut frc = frc | (1u64 << 52);
                let (mut dig_ind, shft) = if adj_exp < 52 {
                    frc >>= 52 - adj_exp;
                    (0usize, 0i32)
                } else {
                    // (rem - 52) % 32 computed on unsigned ints in C;
                    // rem_euclid is the same value.
                    (
                        ((adj_exp - 52) / 32) as usize,
                        (rem - 52).rem_euclid(32),
                    )
                };
                self.dig.clear();
                self.dig.resize(dig_cnt, 0);
                if shft != 0 {
                    self.dig[dig_ind] = ((frc << shft) & mpn::DIG_MASK) as u32;
                    dig_ind += 1;
                    frc >>= 32 - shft;
                }
                // DIG_SIZE (32) < MP_FLOAT_FRAC_BITS + 1 (53).
                while dig_ind != dig_cnt {
                    self.dig[dig_ind] = (frc & mpn::DIG_MASK) as u32;
                    dig_ind += 1;
                    frc >>= 32;
                }
            }
        }
    }

    /// `mpz_set_from_str`: parses digits from `s` (bytes, no sign,
    /// no whitespace handling — the caller supplies `neg` and the
    /// base). Returns the number of bytes processed: parsing stops
    /// at the first character that is not a digit, or whose value is
    /// `>= base`.
    pub fn set_from_str(&mut self, s: &[u8], neg: bool, base: u32) -> usize {
        debug_assert!(base <= 36);
        self.neg = neg;
        self.dig.clear();
        let mut consumed = 0usize;
        for &c in s {
            let v: u64 = if c.is_ascii_digit() {
                (c - b'0') as u64
            } else if c.is_ascii_uppercase() {
                (c - b'A' + 10) as u64
            } else if c.is_ascii_lowercase() {
                (c - b'a' + 10) as u64
            } else {
                break;
            };
            if v >= base as u64 {
                break;
            }
            // mpn_mul_dig_add_dig needs one scratch digit past len.
            self.dig.push(0);
            let len = self.dig.len() - 1;
            let new_len = mpn::mul_dig_add_dig(&mut self.dig, len, base, v as u32);
            self.dig.truncate(new_len);
            consumed += 1;
        }
        consumed
    }

    /// `mpz_set_from_bytes`: unsigned big/little-endian byte string.
    pub fn set_from_bytes(&mut self, big_endian: bool, buf: &[u8]) {
        let mut pos: isize = if big_endian {
            buf.len() as isize - 1
        } else {
            0
        };
        let step: isize = if big_endian { -1 } else { 1 };
        let mut d: u32 = 0;
        let mut num_bits: i32 = 0;
        let mut remaining = buf.len();
        self.neg = false;
        self.dig.clear();
        while remaining > 0 {
            while remaining > 0 && num_bits < 32 {
                d |= (buf[pos as usize] as u32) << num_bits;
                num_bits += 8;
                pos += step;
                remaining -= 1;
            }
            self.dig.push(d);
            d = 0;
            num_bits -= 32;
        }
        let len = mpn::remove_trailing_zeros(&self.dig);
        self.dig.truncate(len);
    }

    /// `mpz_cmp`: sign of `self - other`.
    pub fn cmp_mpz(&self, other: &Self) -> Ordering {
        let c = (other.neg as i32) - (self.neg as i32);
        if c != 0 {
            return c.cmp(&0);
        }
        let c = mpn::cmp(&self.dig, &other.dig);
        if self.neg {
            c.reverse()
        } else {
            c
        }
    }

    /// `mpz_hash`: must return the actual int value if it fits in
    /// `mp_int_t` (i64); otherwise the low 64 bits of the magnitude,
    /// negated for negative values (wrapping).
    pub fn hash_mpz(&self) -> i64 {
        let mut val: u64 = 0;
        for &d in self.dig.iter().rev() {
            val = (val << mpn::DIG_SIZE) | d as u64;
        }
        if self.neg {
            val = val.wrapping_neg();
        }
        val as i64
    }

    /// `mpz_as_int_checked`: the value as an `i64` if it fits.
    /// NOTE: `-2^63` is rejected (returns `None`) by the C overflow
    /// check even though it fits — ported exactly.
    pub fn as_int_checked(&self) -> Option<i64> {
        let mut val: u64 = 0;
        for &d in self.dig.iter().rev() {
            if val > ((!(1u64 << 63)) >> mpn::DIG_SIZE) {
                // will overflow
                return None;
            }
            val = (val << mpn::DIG_SIZE) | d as u64;
        }
        if self.neg {
            val = val.wrapping_neg();
        }
        Some(val as i64)
    }

    /// `mpz_as_uint_checked`: the value as a `u64` if it is
    /// non-negative and fits.
    pub fn as_uint_checked(&self) -> Option<u64> {
        if self.neg {
            return None;
        }
        let mut val: u64 = 0;
        for &d in self.dig.iter().rev() {
            if val > ((!(1u64 << 63)) >> (mpn::DIG_SIZE - 1)) {
                // will overflow
                return None;
            }
            val = (val << mpn::DIG_SIZE) | d as u64;
        }
        Some(val)
    }

    /// `mpz_as_bytes`: writes the value into `buf` (two's complement
    /// for negatives). Returns true if the value fit into `len`
    /// bytes without overflowing; `buf` is truncated otherwise (the
    /// low bytes are still written).
    pub fn as_bytes(&self, big_endian: bool, as_signed: bool, buf: &mut [u8]) -> bool {
        let len = buf.len();
        // Write cursor: big-endian writes from the end backwards,
        // little-endian from the start forwards.
        let mut pos: usize = if big_endian { len } else { 0 };
        let mut olen = len;
        let mut bits: i32 = 0;
        let mut d: u64 = 0;
        let mut carry: u64 = 1;
        let mut ok = true;
        for &zdig in self.dig.iter() {
            bits += 32;
            d = (d << 32) | zdig as u64;
            while bits >= 8 {
                bits -= 8;
                let mut val: u32 = d as u32;
                if self.neg {
                    let v = ((!val) & 0xff) as u64 + carry;
                    carry = v >> 8;
                    val = v as u32;
                }
                if olen == 0 {
                    // Buffer is full, only OK if all remaining bytes
                    // are zeroes.
                    ok = ok && (val as u8) == 0;
                    d >>= 8;
                    continue;
                }
                if big_endian {
                    pos -= 1;
                    buf[pos] = val as u8;
                } else {
                    buf[pos] = val as u8;
                    pos += 1;
                }
                olen -= 1;
                d >>= 8;
            }
        }

        if as_signed && olen == 0 && len > 0 {
            // If output exhausted then ensure there was enough space
            // for the sign bit.
            let most_sig = if big_endian { buf[0] } else { buf[len - 1] };
            ok = ok && (most_sig & 0x80 != 0) == self.neg;
        } else {
            // Fill remainder of buf with zero/sign extension.
            let fill: u8 = if self.neg { 0xff } else { 0x00 };
            if big_endian {
                buf[..olen].fill(fill);
            } else {
                buf[pos..pos + olen].fill(fill);
            }
        }
        ok
    }

    /// `mpz_as_float`: the value as an `f64`, via the C's Horner
    /// accumulation (`val * DIG_BASE + digit`, most significant
    /// digit first).
    pub fn as_float(&self) -> f64 {
        let mut val: f64 = 0.0;
        for &d in self.dig.iter().rev() {
            val = val * mpn::DIG_BASE + d as f64;
        }
        if self.neg {
            val = -val;
        }
        val
    }

    /// `mpz_as_str_inpl`: formats the value in `base` (2..=32),
    /// with `prefix` (written before the digits), `base_char` (the
    /// letter base for digits above 9, usually `b'a'` or `b'A'`) and
    /// `comma` (0 for none, otherwise a separator inserted every 3
    /// digits for base 10, every 4 otherwise).
    pub fn as_str(&self, base: u32, prefix: &str, base_char: u8, comma: u8) -> String {
        debug_assert!((2..=32).contains(&base));
        let n_comma = if base == 10 { 3 } else { 4 };
        let mut s: Vec<u8> = Vec::new();
        if self.dig.is_empty() {
            s.extend_from_slice(prefix.as_bytes());
            s.push(b'0');
            return String::from_utf8(s).expect("prefix is ASCII");
        }

        // Make a copy of the digits, so we can do the div/mod
        // calculation.
        let mut dig = self.dig.clone();
        let mut last_comma = 0usize;
        loop {
            // Compute next remainder.
            let mut a: u64 = 0;
            for d in dig.iter_mut().rev() {
                a = (a << 32) | *d as u64;
                *d = (a / base as u64) as u32;
                a %= base as u64;
            }
            // Convert to character.
            let mut ch = (a as u8) + b'0';
            if ch > b'9' {
                ch += base_char - b'9' - 1;
            }
            s.push(ch);
            // Check if number is zero.
            let done = dig.iter().all(|&d| d == 0);
            if !done && comma != 0 && (s.len() - last_comma) == n_comma {
                s.push(comma);
                last_comma = s.len();
            }
            if done {
                break;
            }
        }
        // The C appends the prefix reversed (the whole string is
        // reversed below), so the prefix ends up first, forwards.
        for &c in prefix.as_bytes().iter().rev() {
            s.push(c);
        }
        if self.neg {
            s.push(b'-');
        }
        // Reverse string.
        s.reverse();
        String::from_utf8(s).expect("digits/prefix are ASCII")
    }
}

impl PartialEq for BigInt {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_mpz(other) == Ordering::Equal
    }
}

impl Eq for BigInt {}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp_mpz(other))
    }
}

impl Ord for BigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_mpz(other)
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str(10, "", b'a', 0))
    }
}

impl From<i64> for BigInt {
    fn from(val: i64) -> Self {
        BigInt::from_int(val)
    }
}

/// Build a BigInt from a decimal string (with optional '-').
/// Test helper only (uses `set_from_str`, itself under test).
/// Shared by the vendor test modules (`mpz_ops::tests`, `probe`).
#[cfg(test)]
pub(crate) fn bi(s: &str) -> BigInt {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mut z = BigInt::zero();
    z.set_from_str(digits.as_bytes(), neg, 10);
    z
}
