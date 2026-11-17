//! Low-level digit-array (`mpn_*`) primitives.
//!
//! Port of the `static mpn_*` helpers at the top of
//! `vendor/micropython/py/mpz.c`. These act on non-negative integers
//! represented as little-endian arrays of 32-bit digits (`u32`,
//! `MPZ_DIG_SIZE` = 32 on x86-64); the caller supplies the length
//! separately. There is no public API for `mpn` — the functions are
//! used by [`super::mpz`] / [`super::mpz_ops`] to implement the
//! `mpz_*` features.
//!
//! Two deliberate divergences from the C, both allocation-only with no
//! observable behaviour change:
//! - The C functions allow the output buffer to alias an input buffer
//!   (`mpn_shl`/`mpn_shr`/`mpn_add`/`mpn_sub` with `i == j`). Rust
//!   slices cannot express that, so callers clone the input first
//!   where the C relies on aliasing. Results are identical.
//! - `mpn_mul`'s C contract says the output must already be zeroed;
//!   the Rust callers zero it (as the C callers do with `memset`).

use std::cmp::Ordering;

/// Digits per value: `MPZ_DIG_SIZE` = 32 on x86-64.
pub const DIG_SIZE: u32 = 32;
/// `DIG_MASK` = `(1 << DIG_SIZE) - 1`.
pub const DIG_MASK: u64 = 0xFFFF_FFFF;
/// `DIG_BASE` = `1 << DIG_SIZE`.
pub const DIG_BASE: f64 = 4294967296.0;

/// `mpn_remove_trailing_zeros`: number of significant digits in `dig`
/// (i.e. the length after stripping trailing zero digits).
pub(crate) fn remove_trailing_zeros(dig: &[u32]) -> usize {
    let mut len = dig.len();
    while len > 0 && dig[len - 1] == 0 {
        len -= 1;
    }
    len
}

/// `mpn_cmp`: compares `a` with `b`, returns the sign of `a - b`.
/// Assumes `a`, `b` are normalised.
pub(crate) fn cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() < b.len() {
        return Ordering::Less;
    }
    if a.len() > b.len() {
        return Ordering::Greater;
    }
    for i in (0..a.len()).rev() {
        let c = (a[i] as u64).cmp(&(b[i] as u64));
        if c != Ordering::Equal {
            return c;
        }
    }
    Ordering::Equal
}

/// `mpn_shl`: computes `out = inp << n`, returns the number of digits
/// in `out`. Assumes enough memory in `out` (`inp.len() +
/// (n + 31) / 32` digits), `inp` normalised, `n > 0`.
pub(crate) fn shl(out: &mut [u32], inp: &[u32], n: u64) -> usize {
    debug_assert!(n > 0);
    debug_assert!(out.len() >= inp.len() + ((n + DIG_SIZE as u64 - 1) / DIG_SIZE as u64) as usize);
    let n_whole = ((n + DIG_SIZE as u64 - 1) / DIG_SIZE as u64) as usize;
    let mut n_part = (n % DIG_SIZE as u64) as u32;
    if n_part == 0 {
        n_part = DIG_SIZE;
    }

    // Start from the high end of the digit arrays.
    let mut d: u64 = 0;
    for k in 0..inp.len() {
        let i = inp.len() - 1 - k;
        let o = inp.len() + n_whole - 1 - k;
        d |= inp[i] as u64;
        out[o] = ((d >> (DIG_SIZE - n_part)) & DIG_MASK) as u32;
        d <<= DIG_SIZE;
    }

    // Store remaining bits.
    out[n_whole - 1] = ((d >> (DIG_SIZE - n_part)) & DIG_MASK) as u32;
    for o in out.iter_mut().take(n_whole - 1) {
        *o = 0;
    }

    // Work out length of result.
    let mut len = inp.len() + n_whole;
    while len != 0 && out[len - 1] == 0 {
        len -= 1;
    }
    len
}

/// `mpn_shr`: computes `out = inp >> n`, returns the number of digits
/// in `out`. Assumes enough memory in `out`, `inp` normalised, `n > 0`.
pub(crate) fn shr(out: &mut [u32], inp: &[u32], n: u64) -> usize {
    debug_assert!(n > 0);
    debug_assert!(out.len() >= inp.len());
    let n_whole = (n / DIG_SIZE as u64) as usize;
    let n_part = (n % DIG_SIZE as u64) as u32;

    if n_whole >= inp.len() {
        return 0;
    }

    let mut len = inp.len() - n_whole;
    for i in 0..len {
        let mut d = inp[n_whole + i] as u64;
        if i + 1 < len {
            d |= (inp[n_whole + i + 1] as u64) << DIG_SIZE;
        }
        d >>= n_part;
        out[i] = (d & DIG_MASK) as u32;
    }

    if out[len - 1] == 0 {
        len -= 1;
    }
    len
}

/// `mpn_add`: computes `out = a + b`, returns the number of digits in
/// `out`. Assumes enough memory in `out`, `a`, `b` normalised,
/// `a.len() >= b.len()`.
pub(crate) fn add(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    debug_assert!(a.len() >= b.len());
    debug_assert!(out.len() >= a.len() + 1);
    let mut carry: u64 = 0;
    let mut i = 0;
    for &bd in b {
        carry += a[i] as u64 + bd as u64;
        out[i] = (carry & DIG_MASK) as u32;
        carry >>= DIG_SIZE;
        i += 1;
    }
    while i < a.len() {
        carry += a[i] as u64;
        out[i] = (carry & DIG_MASK) as u32;
        carry >>= DIG_SIZE;
        i += 1;
    }
    if carry != 0 {
        out[i] = carry as u32;
        i += 1;
    }
    i
}

/// `mpn_sub`: computes `out = a - b`, returns the number of digits in
/// `out`. Assumes enough memory in `out`, `a`, `b` normalised,
/// `a >= b`.
pub(crate) fn sub(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    debug_assert!(a.len() >= b.len());
    debug_assert!(out.len() >= a.len());
    let mut borrow: i64 = 0;
    let mut i = 0;
    for &bd in b {
        borrow += a[i] as i64 - bd as i64;
        out[i] = (borrow & DIG_MASK as i64) as u32;
        borrow >>= DIG_SIZE;
        i += 1;
    }
    while i < a.len() {
        borrow += a[i] as i64;
        out[i] = (borrow & DIG_MASK as i64) as u32;
        borrow >>= DIG_SIZE;
        i += 1;
    }
    remove_trailing_zeros(&out[..i])
}

/// `mpn_and_neg`: the general bitwise-AND engine used by
/// `mpz_and_inpl` when at least one operand is negative (and, with
/// `MICROPY_OPT_MPZ_BITWISE` = 0, for every AND in this build).
/// Computes `out = (im ^ (((a ^ jm) + jc) & ((b ^ km) + kc))) + ic`
/// where `Xm` is `DIG_MASK` iff `Xc != 0`. Assumes `a.len() >=
/// b.len()`, enough memory in `out` (`a.len() + 1` digits).
///
/// The `klen`/`jlen` counters are decremented with wrapping
/// arithmetic exactly as in the C: once `klen` wraps past zero it
/// stays above `jlen`, so the `kmask` branch is taken for the rest
/// of the loop.
pub(crate) fn and_neg(
    out: &mut [u32],
    a: &[u32],
    b: &[u32],
    mut carryi: u64,
    mut carryj: u64,
    mut carryk: u64,
) -> usize {
    debug_assert!(a.len() >= b.len());
    debug_assert!(out.len() >= a.len() + 1);
    let imask: u64 = if carryi == 0 { 0 } else { DIG_MASK };
    let jmask: u64 = if carryj == 0 { 0 } else { DIG_MASK };
    let kmask: u64 = if carryk == 0 { 0 } else { DIG_MASK };

    let mut klen = b.len();
    let mut jlen = a.len();
    let mut ki = 0usize;
    let mut i = 0usize;
    while jlen > 0 {
        carryj += (a[i] as u64) ^ jmask;
        klen = klen.wrapping_sub(1);
        jlen -= 1;
        if klen <= jlen {
            carryk += (b[ki] as u64) ^ kmask;
            ki += 1;
        } else {
            carryk += kmask;
        }
        carryi += ((carryj & carryk) ^ imask) & DIG_MASK;
        out[i] = (carryi & DIG_MASK) as u32;
        carryk >>= DIG_SIZE;
        carryj >>= DIG_SIZE;
        carryi >>= DIG_SIZE;
        i += 1;
    }

    if carryi != 0 {
        out[i] = carryi as u32;
        i += 1;
    }
    remove_trailing_zeros(&out[..i])
}

/// `mpn_or_neg`: the general bitwise-OR engine (`MICROPY_OPT_MPZ_BITWISE`
/// = 0 variant, three carries). Computes `out = ~(((a ^ jm) + jc) |
/// ((b ^ km) + kc)) + 1`. Same assumptions as [`and_neg`]. The C ends
/// with `assert(carryi == 0)` — proven by the comment there (at least
/// one of `a`, `b` is negative, so the loop runs at least once and
/// `carried` cannot stay 1) — mirrored here as a `debug_assert!`.
pub(crate) fn or_neg(
    out: &mut [u32],
    a: &[u32],
    b: &[u32],
    mut carryi: u64,
    mut carryj: u64,
    mut carryk: u64,
) -> usize {
    debug_assert!(a.len() >= b.len());
    debug_assert!(out.len() >= a.len() + 1);
    let imask: u64 = if carryi == 0 { 0 } else { DIG_MASK };
    let jmask: u64 = if carryj == 0 { 0 } else { DIG_MASK };
    let kmask: u64 = if carryk == 0 { 0 } else { DIG_MASK };

    let mut klen = b.len();
    let mut jlen = a.len();
    let mut ki = 0usize;
    let mut i = 0usize;
    while jlen > 0 {
        carryj += (a[i] as u64) ^ jmask;
        klen = klen.wrapping_sub(1);
        jlen -= 1;
        if klen <= jlen {
            carryk += (b[ki] as u64) ^ kmask;
            ki += 1;
        } else {
            carryk += kmask;
        }
        carryi += ((carryj | carryk) ^ imask) & DIG_MASK;
        out[i] = (carryi & DIG_MASK) as u32;
        carryk >>= DIG_SIZE;
        carryj >>= DIG_SIZE;
        carryi >>= DIG_SIZE;
        i += 1;
    }

    // See the comment in mpz.c's mpn_or_neg for why carryi must be 0.
    debug_assert!(carryi == 0);
    remove_trailing_zeros(&out[..i])
}

/// `mpn_xor_neg`: the general bitwise-XOR engine. Computes
/// `out = ((a - 1 + jc) ^ (b - 1 + kc)) + ic`. Same assumptions as
/// [`and_neg`].
pub(crate) fn xor_neg(
    out: &mut [u32],
    a: &[u32],
    b: &[u32],
    mut carryi: u64,
    mut carryj: u64,
    mut carryk: u64,
) -> usize {
    debug_assert!(a.len() >= b.len());
    debug_assert!(out.len() >= a.len() + 1);
    let mut klen = b.len();
    let mut jlen = a.len();
    let mut ki = 0usize;
    let mut i = 0usize;
    while jlen > 0 {
        carryj += (a[i] as u64) + DIG_MASK;
        klen = klen.wrapping_sub(1);
        jlen -= 1;
        if klen <= jlen {
            carryk += (b[ki] as u64) + DIG_MASK;
            ki += 1;
        } else {
            carryk += DIG_MASK;
        }
        carryi += (carryj ^ carryk) & DIG_MASK;
        out[i] = (carryi & DIG_MASK) as u32;
        carryk >>= DIG_SIZE;
        carryj >>= DIG_SIZE;
        carryi >>= DIG_SIZE;
        i += 1;
    }

    if carryi != 0 {
        out[i] = carryi as u32;
        i += 1;
    }
    remove_trailing_zeros(&out[..i])
}

/// `mpn_mul_dig_add_dig`: computes `dig[0..len] = dig[0..len] * dmul
/// + dadd` in place, returns the new number of digits. Assumes
/// `dig.len() > len` (room for a possible carry digit), `dmul != 0`.
pub(crate) fn mul_dig_add_dig(dig: &mut [u32], len: usize, dmul: u32, dadd: u32) -> usize {
    debug_assert!(dmul != 0);
    debug_assert!(dig.len() > len);
    let mut carry: u64 = dadd as u64;
    let mut i = 0;
    while i < len {
        carry += (dig[i] as u64) * (dmul as u64);
        dig[i] = (carry & DIG_MASK) as u32;
        carry >>= DIG_SIZE;
        i += 1;
    }
    if carry != 0 {
        dig[i] = carry as u32;
        i += 1;
    }
    i
}

/// `mpn_mul`: computes `out = a * b`, returns the number of digits in
/// `out`. Assumes `out` is zeroed and has `a.len() + b.len()` digits,
/// `a`, `b` normalised.
pub(crate) fn mul(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    debug_assert!(out.len() >= a.len() + b.len());
    let mut ilen = 0;
    for (kb, &kdig) in b.iter().enumerate() {
        let mut carry: u64 = 0;
        let mut id = kb;
        for &jd in a {
            carry += out[id] as u64 + (jd as u64) * (kdig as u64);
            out[id] = (carry & DIG_MASK) as u32;
            carry >>= DIG_SIZE;
            id += 1;
        }
        if carry != 0 {
            out[id] = carry as u32;
            id += 1;
        }
        ilen = id;
    }
    ilen
}

/// `mpn_div`: `num = quo * den + new_num` (i.e. `num` is replaced
/// with the remainder). Assumes `den != 0`, `num` has room for one
/// extra digit (`num.len() >= num_len + 1`), `quo` has room for
/// `num_len` digits, `den` normalised.
///
/// Returns `(quo_len, num_len)` — the quotient length and the
/// remainder length. `num[0..num_len]` holds the remainder on return,
/// `quo[0..quo_len]` the quotient.
///
/// This is Knuth's Algorithm D as implemented in mpz.c, ported with
/// explicit indices instead of C pointers. All `u64` arithmetic is
/// wrapping, matching C unsigned arithmetic; the `i64` `borrow` uses
/// arithmetic (signed) right shifts, as in C.
pub(crate) fn div(num: &mut [u32], num_len: usize, den: &[u32], quo: &mut [u32]) -> (usize, usize) {
    debug_assert!(!den.is_empty());
    debug_assert!(num.len() >= num_len + 1);
    debug_assert!(quo.len() >= num_len);
    let den_len = den.len();

    // Handle simple cases.
    match cmp(&num[..num_len], den) {
        Ordering::Equal => {
            quo[0] = 1;
            return (1, 0);
        }
        Ordering::Less => {
            return (0, num_len);
        }
        Ordering::Greater => {}
    }

    // Count leading zeros in the leading digit of the denominator, so
    // the division routine works on a normalised denominator.
    let mut norm_shift: u32 = 0;
    let mut d = den[den_len - 1];
    while (d & 0x8000_0000) == 0 {
        d <<= 1;
        norm_shift += 1;
    }

    // Shift the numerator by the same amount; first increase its
    // length by one digit (the scratch slot, initially 0).
    let mut nlen = num_len + 1;
    num[num_len] = 0;
    let mut carry: u32 = 0;
    for i in 0..nlen {
        let n = num[i];
        num[i] = ((n << norm_shift) | carry) & (DIG_MASK as u32);
        // Cast to u64 first, exactly as the C does: for norm_shift == 0
        // this is `n >> 32` on a u64, i.e. 0 — no shift-by-32 UB.
        carry = ((n as u64) >> (DIG_SIZE - norm_shift)) as u32;
    }

    // Cache the leading digit of the denominator.
    let mut lead_den_digit: u64 = (den[den_len - 1] as u64) << norm_shift;
    if den_len >= 2 {
        lead_den_digit |= (den[den_len - 2] as u64) >> (DIG_SIZE - norm_shift);
    }

    // Point at the last digit in the numerator.
    let mut top = nlen - 1;
    // Calculate number of digits in the quotient.
    let quo_len = nlen - den_len;
    let mut qpos = quo_len;

    // Keep going while we have enough digits to divide.
    while nlen > den_len {
        let mut quo_est: u64 = ((num[top] as u64) << DIG_SIZE) | num[top - 1] as u64;
        // Get approximate quotient.
        quo_est /= lead_den_digit;

        // Multiply quo_est by den and subtract from num to get the
        // remainder. Both `borrow` and `low` are signed values and
        // need signed right-shift, but `x` is unsigned and may take a
        // full-range value.
        let mut d_norm: u64 = 0;
        let mut borrow: i64 = 0;
        for k in 0..den_len {
            let n_idx = top - den_len + k;
            // Get the next digit in (den).
            d_norm = ((den[k] as u64) << norm_shift) | (d_norm >> DIG_SIZE);
            // Multiply the next digit in (quo * den).
            let x = quo_est.wrapping_mul(d_norm & DIG_MASK);
            // Compute the low DIG_MASK bits of the next digit in
            // (num - quo * den).
            let low: i64 =
                (borrow & DIG_MASK as i64) + num[n_idx] as i64 - (x & DIG_MASK) as i64;
            // Store the digit result for (num).
            num[n_idx] = (low & DIG_MASK as i64) as u32;
            // Compute the borrow, shifted right before summing to
            // avoid overflow.
            borrow = (borrow >> DIG_SIZE) - ((x >> DIG_SIZE) as i64) + (low >> DIG_SIZE);
        }

        // Either quo was correct (borrow + top digit == 0) or quo was
        // too large: reduce quo and add back den until the carry
        // cancels out the borrow.
        borrow += num[top] as i64;
        while borrow != 0 {
            quo_est = quo_est.wrapping_sub(1);
            d_norm = 0;
            let mut add_carry: u64 = 0;
            for k in 0..den_len {
                let n_idx = top - den_len + k;
                d_norm = ((den[k] as u64) << norm_shift) | (d_norm >> DIG_SIZE);
                add_carry += num[n_idx] as u64 + (d_norm & DIG_MASK);
                num[n_idx] = (add_carry & DIG_MASK) as u32;
                add_carry >>= DIG_SIZE;
            }
            borrow += add_carry as i64;
        }

        // Store this digit of the quotient.
        qpos -= 1;
        quo[qpos] = (quo_est & DIG_MASK) as u32;

        // Move down to the next digit of the numerator.
        top -= 1;
        nlen -= 1;
    }

    // Unnormalise numerator (remainder now).
    let mut carry: u32 = 0;
    for i in (0..nlen).rev() {
        let n = num[i];
        num[i] = (((n >> norm_shift) as u64 | carry as u64) & DIG_MASK) as u32;
        carry = ((n as u64) << (DIG_SIZE - norm_shift)) as u32;
    }

    // Strip trailing zeros.
    let mut ql = quo_len;
    while ql > 0 && quo[ql - 1] == 0 {
        ql -= 1;
    }
    let mut nl = nlen;
    while nl > 0 && num[nl - 1] == 0 {
        nl -= 1;
    }
    (ql, nl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(v: &[u32]) -> Vec<u32> {
        v.to_vec()
    }

    #[test]
    fn remove_trailing_zeros_cases() {
        assert_eq!(remove_trailing_zeros(&[]), 0);
        assert_eq!(remove_trailing_zeros(&[0, 0]), 0);
        assert_eq!(remove_trailing_zeros(&[1, 0, 0]), 1);
        assert_eq!(remove_trailing_zeros(&[1, 2, 3]), 3);
    }

    #[test]
    fn cmp_cases() {
        assert_eq!(cmp(&[], &[]), Ordering::Equal);
        assert_eq!(cmp(&[1], &[1]), Ordering::Equal);
        assert_eq!(cmp(&[1], &[2]), Ordering::Less);
        assert_eq!(cmp(&[2], &[1]), Ordering::Greater);
        assert_eq!(cmp(&[0, 1], &[1]), Ordering::Greater);
        assert_eq!(cmp(&[0xFFFF_FFFF, 1], &[1, 1]), Ordering::Greater);
        assert_eq!(cmp(&[1, 1], &[0xFFFF_FFFF, 1]), Ordering::Less);
    }

    #[test]
    fn shl_cases() {
        // 1 << 1 = 2
        let mut out = vec![0u32; 3];
        assert_eq!(shl(&mut out, &[1], 1), 1);
        assert_eq!(&out[..1], &[2]);
        // 0xFFFF_FFFF << 1 = 0x1_FFFF_FFFE
        let mut out = vec![0u32; 3];
        assert_eq!(shl(&mut out, &[0xFFFF_FFFF], 1), 2);
        assert_eq!(&out[..2], &[0xFFFF_FFFE, 1]);
        // 1 << 32 = [0, 1]
        let mut out = vec![0u32; 3];
        assert_eq!(shl(&mut out, &[1], 32), 2);
        assert_eq!(&out[..2], &[0, 1]);
        // 1 << 64 = [0, 0, 1]
        let mut out = vec![0u32; 4];
        assert_eq!(shl(&mut out, &[1], 64), 3);
        assert_eq!(&out[..3], &[0, 0, 1]);
        // 0x8000_0000_0000_0001 << 33 = 2^128 + 2^33
        let mut out = vec![0u32; 6];
        assert_eq!(shl(&mut out, &[1, 0, 0x8000_0000], 33), 5);
        assert_eq!(&out[..5], &[0, 2, 0, 0, 1]);
    }

    #[test]
    fn shr_cases() {
        // 2 >> 1 = 1
        let mut out = vec![0u32; 2];
        assert_eq!(shr(&mut out, &[2], 1), 1);
        assert_eq!(&out[..1], &[1]);
        // 0x1_FFFF_FFFE >> 1 = 0xFFFF_FFFF
        let mut out = vec![0u32; 2];
        assert_eq!(shr(&mut out, &[0xFFFF_FFFE, 1], 1), 1);
        assert_eq!(&out[..1], &[0xFFFF_FFFF]);
        // [0, 1] >> 32 = 1
        let mut out = vec![0u32; 2];
        assert_eq!(shr(&mut out, &[0, 1], 32), 1);
        assert_eq!(&out[..1], &[1]);
        // [0, 0, 1] >> 64 = 1
        let mut out = vec![0u32; 3];
        assert_eq!(shr(&mut out, &[0, 0, 1], 64), 1);
        assert_eq!(&out[..1], &[1]);
        // [5] >> 3 = 0 (whole shift past the value)
        let mut out = vec![0u32; 1];
        assert_eq!(shr(&mut out, &[5], 3), 0);
        // [1] >> 32 = 0 (n_whole >= len)
        let mut out = vec![0u32; 1];
        assert_eq!(shr(&mut out, &[1], 32), 0);
    }

    #[test]
    fn add_sub_cases() {
        // 0xFFFF_FFFF + 1 = 0x1_0000_0000
        let mut out = vec![0u32; 3];
        assert_eq!(add(&mut out, &[0xFFFF_FFFF], &[1]), 2);
        assert_eq!(&out[..2], &[0, 1]);
        // 0x1_0000_0000 - 1 = 0xFFFF_FFFF
        let mut out = vec![0u32; 2];
        assert_eq!(sub(&mut out, &[0, 1], &[1]), 1);
        assert_eq!(&out[..1], &[0xFFFF_FFFF]);
        // (2^64 - 1) + (2^64 - 1) = 2^65 - 2
        let mut out = vec![0u32; 4];
        assert_eq!(
            add(&mut out, &[0xFFFF_FFFF, 0xFFFF_FFFF], &[0xFFFF_FFFF, 0xFFFF_FFFF]),
            3
        );
        assert_eq!(&out[..3], &[0xFFFF_FFFE, 0xFFFF_FFFF, 1]);
        // a - a = 0
        let mut out = vec![0u32; 2];
        assert_eq!(sub(&mut out, &[5, 7], &[5, 7]), 0);
    }

    #[test]
    fn and_neg_cases() {
        // Both positive: 0b1010 & 0b1100 = 0b1000 (carries all 0).
        let mut out = vec![0u32; 2];
        assert_eq!(and_neg(&mut out, &[0b1010], &[0b1100], 0, 0, 0), 1);
        assert_eq!(&out[..1], &[0b1000]);
        // (-1) & 5 = 5: a = -1 (digits [1], neg), b = 5.
        // and_neg(a=[1], b=[5], carryi=0, carryj=1, carryk=0).
        let mut out = vec![0u32; 2];
        assert_eq!(and_neg(&mut out, &[1], &[5], 0, 1, 0), 1);
        assert_eq!(&out[..1], &[5]);
        // (-5) & (-3): (-5) & (-3) = -7 in two's complement.
        // a=[5] neg, b=[3] neg: and_neg(a, b, carryi=1, carryj=1, carryk=1).
        let mut out = vec![0u32; 2];
        assert_eq!(and_neg(&mut out, &[5], &[3], 1, 1, 1), 1);
        assert_eq!(&out[..1], &[7]);
        // 0 & 0 = 0 (empty result, no extra digit).
        let mut out = vec![0u32; 1];
        assert_eq!(and_neg(&mut out, &[], &[], 0, 0, 0), 0);
    }

    #[test]
    fn or_neg_cases() {
        // Both positive is NOT the or_neg path in the OPT build, but the
        // general form handles it: 0b1010 | 0b1100 = 0b1110.
        let mut out = vec![0u32; 2];
        assert_eq!(or_neg(&mut out, &[0b1010], &[0b1100], 0, 0, 0), 1);
        assert_eq!(&out[..1], &[0b1110]);
        // (-1) | 5 = -1: a=[1] neg, b=[5] pos.
        // or_neg(a, b, carryi=1, carryj=1, carryk=0).
        let mut out = vec![0u32; 2];
        assert_eq!(or_neg(&mut out, &[1], &[5], 1, 1, 0), 1);
        assert_eq!(&out[..1], &[1]);
        // (-5) | (-3) = -1: a=[5] neg, b=[3] neg.
        let mut out = vec![0u32; 2];
        assert_eq!(or_neg(&mut out, &[5], &[3], 1, 1, 1), 1);
        assert_eq!(&out[..1], &[1]);
    }

    #[test]
    fn xor_neg_cases() {
        // 0b1010 ^ 0b1100 = 0b0110 (general form, carries 0,1,1).
        let mut out = vec![0u32; 2];
        assert_eq!(xor_neg(&mut out, &[0b1010], &[0b1100], 0, 1, 1), 1);
        assert_eq!(&out[..1], &[0b0110]);
        // (-1) ^ 5 = -6: a=[1] neg, b=[5] pos.
        // xor_neg(a, b, carryi=1, carryj=0, carryk=1).
        let mut out = vec![0u32; 2];
        assert_eq!(xor_neg(&mut out, &[1], &[5], 1, 0, 1), 1);
        assert_eq!(&out[..1], &[6]);
        // (-5) ^ (-3) = 6: a=[5] neg, b=[3] neg.
        // xor_neg(a, b, carryi=0, carryj=0, carryk=0).
        let mut out = vec![0u32; 2];
        assert_eq!(xor_neg(&mut out, &[5], &[3], 0, 0, 0), 1);
        assert_eq!(&out[..1], &[6]);
    }

    #[test]
    fn mul_dig_add_dig_cases() {
        // 0 * 10 + 7 = 7
        let mut dig = vec![0u32; 2];
        assert_eq!(mul_dig_add_dig(&mut dig, 0, 10, 7), 1);
        assert_eq!(dig[0], 7);
        // 0xFFFF_FFFF * 10 + 0 = 0x9_FFFF_FFF6
        let mut dig = vec![0xFFFF_FFFF, 0];
        assert_eq!(mul_dig_add_dig(&mut dig, 1, 10, 0), 2);
        assert_eq!(&dig[..2], &[0xFFFF_FFF6, 9]);
        // 99 * 10 + 9 = 999
        let mut dig = vec![99, 0];
        assert_eq!(mul_dig_add_dig(&mut dig, 1, 10, 9), 1);
        assert_eq!(dig[0], 999);
    }

    #[test]
    fn mul_cases() {
        // 0 * x = 0 — but mpn_mul returns 1 digit (the C's `ilen`
        // counts the inner-loop pass, even when the digit is 0);
        // the caller (mpz_mul_inpl) strips the trailing zero.
        let mut out = vec![0u32; 2];
        assert_eq!(mul(&mut out, &[0], &[5]), 1);
        assert_eq!(out[0], 0);
        // 0xFFFF_FFFF * 0xFFFF_FFFF = 0xFFFFFFFE_00000001
        let mut out = vec![0u32; 2];
        assert_eq!(mul(&mut out, &[0xFFFF_FFFF], &[0xFFFF_FFFF]), 2);
        assert_eq!(&out[..2], &[1, 0xFFFFFFFE]);
        // (2^64 - 1) * 2 = 0x1_FFFF_FFFF_FFFF_FFFE
        let mut out = vec![0u32; 3];
        assert_eq!(
            mul(&mut out, &[0xFFFF_FFFF, 0xFFFF_FFFF], &[2]),
            3
        );
        assert_eq!(&out[..3], &[0xFFFF_FFFE, 0xFFFF_FFFF, 1]);
    }

    #[test]
    fn div_cases() {
        // 10 / 3 = 3 rem 1
        let mut num = d(&[10, 0]);
        let mut quo = vec![0u32; 2];
        assert_eq!(div(&mut num, 1, &[3], &mut quo), (1, 1));
        assert_eq!(quo[0], 3);
        assert_eq!(num[0], 1);
        // 7 / 7 = 1 rem 0 (equal case)
        let mut num = d(&[7, 0]);
        let mut quo = vec![0u32; 2];
        assert_eq!(div(&mut num, 1, &[7], &mut quo), (1, 0));
        assert_eq!(quo[0], 1);
        // 5 / 7 = 0 rem 5 (less case)
        let mut num = d(&[5, 0]);
        let mut quo = vec![0u32; 2];
        assert_eq!(div(&mut num, 1, &[7], &mut quo), (0, 1));
        assert_eq!(num[0], 5);
        // (2^64 - 1) / (2^32 - 1) = 0x1_0000_0001 rem 0
        let mut num = d(&[0xFFFF_FFFF, 0xFFFF_FFFF, 0]);
        let mut quo = vec![0u32; 3];
        assert_eq!(
            div(&mut num, 2, &[0xFFFF_FFFF], &mut quo),
            (2, 0)
        );
        assert_eq!(&quo[..2], &[1, 1]);
        // 2^64 / 3 = 6148914691236517205 rem 1
        let mut num = d(&[0, 0, 1, 0]);
        let mut quo = vec![0u32; 4];
        let (ql, rl) = div(&mut num, 3, &[3], &mut quo);
        assert_eq!((ql, rl), (2, 1));
        assert_eq!(&quo[..2], &[0x5555_5555, 0x5555_5555]);
        assert_eq!(num[0], 1);
        // (2^96 - 1) / (2^64 + 1): multi-digit denominator, norm_shift > 0
        let mut num = d(&[0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0]);
        let mut quo = vec![0u32; 4];
        let (ql, rl) = div(&mut num, 3, &[1, 0, 1], &mut quo);
        // (2^96 - 1) / (2^64 + 1) = 2^32 - 1 rem 2^32 - 1... check: (2^64+1)(2^32-1) = 2^96 - 2^64 + 2^32 - 1; rem = 2^96-1 - that = 2^64 - 2^32 = 0xFFFF_FFFF_0000_0000
        assert_eq!((ql, rl), (1, 2));
        assert_eq!(quo[0], 0xFFFF_FFFF);
        assert_eq!(&num[..2], &[0, 0xFFFF_FFFF]);
    }
}
