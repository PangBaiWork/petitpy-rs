//! Unit tests for `mpz` (the `BigInt` type).
//!
//! Kept in a separate file (rather than the usual inline
//! `mod tests`) so `mpz.rs` stays under the 800-line cap.
use std::cmp::Ordering;

use super::mpz::*;

#[test]
fn from_int_cases() {
    assert_eq!(BigInt::from_int(0).to_string(), "0");
    assert_eq!(BigInt::from_int(1).to_string(), "1");
    assert_eq!(BigInt::from_int(-1).to_string(), "-1");
    assert_eq!(BigInt::from_int(123456789).to_string(), "123456789");
    assert_eq!(BigInt::from_int(-123456789).to_string(), "-123456789");
    assert_eq!(
        BigInt::from_int(i64::MAX).to_string(),
        "9223372036854775807"
    );
    assert_eq!(
        BigInt::from_int(i64::MIN).to_string(),
        "-9223372036854775808"
    );
    // i64::MIN magnitude is 2^63, digits [0, 0x80000000].
    assert_eq!(BigInt::from_int(i64::MIN).digits(), &[0, 0x8000_0000]);
}

#[test]
fn from_ll_cases() {
    assert_eq!(BigInt::from_ll(0, true).to_string(), "0");
    assert_eq!(BigInt::from_ll(-5, true).to_string(), "-5");
    assert_eq!(BigInt::from_ll(5, false).to_string(), "5");
    // Unsigned reinterpretation of a negative value.
    assert_eq!(
        BigInt::from_ll(-1, false).to_string(),
        "18446744073709551615"
    );
    assert_eq!(
        BigInt::from_ll(i64::MIN, true).to_string(),
        "-9223372036854775808"
    );
    assert_eq!(
        BigInt::from_ll(i64::MIN, false).to_string(),
        "9223372036854775808"
    );
}

#[test]
fn set_from_str_cases() {
    let mut z = BigInt::zero();
    assert_eq!(z.set_from_str(b"123", false, 10), 3);
    assert_eq!(z.to_string(), "123");
    assert_eq!(z.set_from_str(b"ff", false, 16), 2);
    assert_eq!(z.to_string(), "255");
    assert_eq!(z.set_from_str(b"FF", false, 16), 2);
    assert_eq!(z.to_string(), "255");
    assert_eq!(z.set_from_str(b"101", false, 2), 3);
    assert_eq!(z.to_string(), "5");
    // Stops at the first invalid character.
    assert_eq!(z.set_from_str(b"12z34", false, 10), 2);
    assert_eq!(z.to_string(), "12");
    // Digit >= base stops parsing ('g' is 16, base 16 stops).
    assert_eq!(z.set_from_str(b"1g", false, 16), 1);
    assert_eq!(z.to_string(), "1");
    // Empty input: value 0, neg flag as passed.
    assert_eq!(z.set_from_str(b"", true, 10), 0);
    assert_eq!(z.to_string(), "0");
    assert!(z.is_neg()); // negative zero, exactly as the C does
    // "-0": value 0 with the neg flag set (the C quirk).
    let neg_zero = bi("-0");
    assert!(neg_zero.is_zero());
    assert!(neg_zero.is_neg());
    assert_eq!(neg_zero.to_string(), "0");
    // Negative zero compares less than zero (mpz_cmp sign first).
    assert_eq!(neg_zero.cmp_mpz(&BigInt::zero()), Ordering::Less);
    assert_eq!(BigInt::zero().cmp_mpz(&neg_zero), Ordering::Greater);
}

#[test]
fn set_from_bytes_cases() {
    assert_eq!(
        BigInt::from_bytes(false, &[0x01, 0x00, 0x00, 0x00]).to_string(),
        "1"
    );
    assert_eq!(
        BigInt::from_bytes(true, &[0x01, 0x00, 0x00, 0x00]).to_string(),
        "16777216"
    );
    assert_eq!(
        BigInt::from_bytes(false, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).to_string(),
        "1099511627775"
    );
    assert_eq!(BigInt::from_bytes(false, &[0, 0, 0]).to_string(), "0");
    assert_eq!(BigInt::from_bytes(true, &[]).to_string(), "0");
}

#[test]
fn set_from_float_cases() {
    assert_eq!(BigInt::from_float(0.0).to_string(), "0");
    assert_eq!(BigInt::from_float(-0.0).to_string(), "0");
    assert_eq!(BigInt::from_float(0.5).to_string(), "0");
    assert_eq!(BigInt::from_float(-0.5).to_string(), "0");
    assert_eq!(BigInt::from_float(1.5).to_string(), "1");
    assert_eq!(BigInt::from_float(-1.5).to_string(), "-1");
    assert_eq!(BigInt::from_float(-1.0).to_string(), "-1");
    assert_eq!(BigInt::from_float(2.5).to_string(), "2");
    assert_eq!(BigInt::from_float(-2.5).to_string(), "-2");
    assert_eq!(BigInt::from_float(1e30).to_string(), "1000000000000000019884624838656");
    assert_eq!(BigInt::from_float(-1e30).to_string(), "-1000000000000000019884624838656");
    assert_eq!(BigInt::from_float(2f64.powi(70)).to_string(), "1180591620717411303424");
    // Subnormal: exp == 0 -> 0.
    assert_eq!(BigInt::from_float(5e-324).to_string(), "0");
    // inf / NaN -> 0 ("should be handled by caller").
    assert_eq!(BigInt::from_float(f64::INFINITY).to_string(), "0");
    assert_eq!(BigInt::from_float(f64::NEG_INFINITY).to_string(), "0");
    assert_eq!(BigInt::from_float(f64::NAN).to_string(), "0");
}

#[test]
fn set_from_float_negative_one_to_two_quirk() {
    // Lane C documented the vendor shape here: the C's
    // adj_exp == 0 branch calls mpz_set_from_int(z, 1),
    // which resets the sign (-1.5 -> +1). D3 FIXED it:
    // the reference's observable behavior is int(-1.5) ==
    // -1 (probed 2026-11-18 — the Zig's
    // mp_obj_new_int_from_float truncates toward zero), so
    // `set_from_float` restores the sign after the
    // `set_from_int(1)` call. These cases pin the fix.
    assert_eq!(BigInt::from_float(-1.5).to_string(), "-1");
    assert_eq!(BigInt::from_float(-1.0).to_string(), "-1");
    assert_eq!(BigInt::from_float(-1.999).to_string(), "-1");
    assert_eq!(BigInt::from_float(1.999).to_string(), "1");
}

#[test]
fn cmp_cases() {
    assert_eq!(bi("0").cmp_mpz(&bi("0")), Ordering::Equal);
    assert_eq!(bi("-1").cmp_mpz(&bi("0")), Ordering::Less);
    assert_eq!(bi("0").cmp_mpz(&bi("-1")), Ordering::Greater);
    assert_eq!(bi("-5").cmp_mpz(&bi("-3")), Ordering::Less);
    assert_eq!(bi("-3").cmp_mpz(&bi("-5")), Ordering::Greater);
    assert_eq!(bi("12345678901234567890").cmp_mpz(&bi("12345678901234567891")), Ordering::Less);
    assert_eq!(bi("12345678901234567890").cmp_mpz(&bi("12345678901234567890")), Ordering::Equal);
}

#[test]
fn hash_cases() {
    assert_eq!(bi("0").hash_mpz(), 0);
    assert_eq!(bi("1").hash_mpz(), 1);
    assert_eq!(bi("-1").hash_mpz(), -1);
    assert_eq!(bi("123456789").hash_mpz(), 123456789);
    // 2^70 hashes to 0: the low 64 bits shift out entirely.
    assert_eq!(bi("1180591620717411303424").hash_mpz(), 0);
    // 2^64 + 5 -> low 64 bits are 5.
    assert_eq!(bi("18446744073709551621").hash_mpz(), 5);
    assert_eq!(bi("-18446744073709551621").hash_mpz(), -5);
    // i64::MAX fits exactly.
    assert_eq!(bi("9223372036854775807").hash_mpz(), i64::MAX);
    // 2^63 -> low 64 bits are 0x8000_0000_0000_0000 -> i64::MIN.
    assert_eq!(bi("9223372036854775808").hash_mpz(), i64::MIN);
}

#[test]
fn as_int_checked_cases() {
    assert_eq!(bi("0").as_int_checked(), Some(0));
    assert_eq!(bi("9223372036854775807").as_int_checked(), Some(i64::MAX));
    assert_eq!(bi("9223372036854775808").as_int_checked(), None);
    assert_eq!(bi("-9223372036854775807").as_int_checked(), Some(-9223372036854775807));
    // -2^63 is rejected by the C overflow check (ported exactly).
    assert_eq!(bi("-9223372036854775808").as_int_checked(), None);
    assert_eq!(bi("-9223372036854775809").as_int_checked(), None);
}

#[test]
fn as_uint_checked_cases() {
    assert_eq!(bi("0").as_uint_checked(), Some(0));
    assert_eq!(bi("18446744073709551615").as_uint_checked(), Some(u64::MAX));
    assert_eq!(bi("18446744073709551616").as_uint_checked(), None);
    assert_eq!(bi("-1").as_uint_checked(), None);
}

#[test]
fn as_bytes_cases() {
    // 0x01020304 little-endian, unsigned, exact fit.
    let mut buf = [0u8; 4];
    assert!(bi("16909060").as_bytes(false, false, &mut buf));
    assert_eq!(buf, [0x04, 0x03, 0x02, 0x01]);
    // Same value, big-endian.
    let mut buf = [0u8; 4];
    assert!(bi("16909060").as_bytes(true, false, &mut buf));
    assert_eq!(buf, [0x01, 0x02, 0x03, 0x04]);
    // -1 signed, 4 bytes little-endian: all 0xFF.
    let mut buf = [0u8; 4];
    assert!(bi("-1").as_bytes(false, true, &mut buf));
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
    // -2 signed, 1 byte: the C returns FALSE even though -2
    // fits in one signed byte — the digit stream emits 4 bytes
    // (one 32-bit digit), and the 3 trailing bytes are 0xFF
    // (two's-complement sign extension), which fail the C's
    // "remaining bytes must be zero" check. Same quirk as
    // (-2**70).to_bytes(9) on the reference binary
    // (OverflowError). The low byte IS written (0xFE).
    let mut buf = [0u8; 1];
    assert!(!bi("-2").as_bytes(false, true, &mut buf));
    assert_eq!(buf, [0xFE]);
    // -2 signed, 4 bytes: the full digit fits, sign check passes.
    let mut buf = [0u8; 4];
    assert!(bi("-2").as_bytes(false, true, &mut buf));
    assert_eq!(buf, [0xFE, 0xFF, 0xFF, 0xFF]);
    // 256 unsigned in 1 byte: does not fit, low byte written.
    let mut buf = [0u8; 1];
    assert!(!bi("256").as_bytes(false, false, &mut buf));
    assert_eq!(buf, [0x00]);
    // 255 signed in 1 byte: sign bit mismatch -> not ok.
    let mut buf = [0u8; 1];
    assert!(!bi("255").as_bytes(false, true, &mut buf));
    assert_eq!(buf, [0xFF]);
    // -129 signed in 1 byte: does not fit.
    let mut buf = [0u8; 1];
    assert!(!bi("-129").as_bytes(false, true, &mut buf));
    assert_eq!(buf, [0x7F]);
    // Zero, longer buffer: zero-filled.
    let mut buf = [0xAAu8; 4];
    assert!(bi("0").as_bytes(false, false, &mut buf));
    assert_eq!(buf, [0, 0, 0, 0]);
    // -1, longer buffer, signed: 0xFF-filled.
    let mut buf = [0u8; 6];
    assert!(bi("-1").as_bytes(true, true, &mut buf));
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    // Empty buffer: ok iff value is 0.
    let mut buf = [0u8; 0];
    assert!(bi("0").as_bytes(false, false, &mut buf));
    assert!(!bi("1").as_bytes(false, false, &mut buf));
}

#[test]
fn as_float_cases() {
    assert_eq!(bi("0").as_float(), 0.0);
    assert_eq!(bi("1").as_float(), 1.0);
    assert_eq!(bi("-123456789").as_float(), -123456789.0);
    assert_eq!(bi("9007199254740993").as_float(), 9007199254740992.0);
    assert_eq!(bi("1180591620717411303424").as_float(), 2f64.powi(70));
    assert!(bi("1").as_float().is_sign_positive());
    assert!(bi("-1").as_float().is_sign_negative());
}

#[test]
fn as_str_cases() {
    assert_eq!(bi("0").as_str(10, "", b'a', 0), "0");
    assert_eq!(bi("0").as_str(16, "0x", b'a', 0), "0x0");
    assert_eq!(bi("255").as_str(16, "0x", b'a', 0), "0xff");
    assert_eq!(bi("255").as_str(16, "0x", b'A', 0), "0xFF");
    assert_eq!(bi("-255").as_str(16, "", b'a', 0), "-ff");
    assert_eq!(bi("5").as_str(2, "", b'a', 0), "101");
    assert_eq!(bi("8").as_str(8, "", b'a', 0), "10");
    assert_eq!(bi("12345678901234567890").as_str(10, "", b'a', 0), "12345678901234567890");
    assert_eq!(bi("35").as_str(32, "", b'a', 0), "13"); // base 32
    assert_eq!(bi("1234567").as_str(10, "", b'a', b','), "1,234,567");
    assert_eq!(bi("-1234567").as_str(10, "", b'a', b','), "-1,234,567");
    assert_eq!(bi("123456").as_str(10, "", b'a', b','), "123,456");
    assert_eq!(bi("1234").as_str(10, "", b'a', b','), "1,234");
    assert_eq!(bi("123").as_str(10, "", b'a', b','), "123");
    assert_eq!(bi("12345678").as_str(16, "", b'a', b','), "bc,614e");
    assert_eq!(bi("1000000").as_str(10, "n=", b'a', 0), "n=1000000");
}

#[test]
fn bit_length_cases() {
    assert_eq!(bi("0").bit_length(), 0);
    assert_eq!(bi("1").bit_length(), 1);
    assert_eq!(bi("2").bit_length(), 2);
    assert_eq!(bi("255").bit_length(), 8);
    assert_eq!(bi("256").bit_length(), 9);
    assert_eq!(bi("-255").bit_length(), 8);
    assert_eq!(bi("18446744073709551616").bit_length(), 65);
    assert_eq!(bi("0").max_num_bits(), 0);
    assert_eq!(bi("1").max_num_bits(), 32);
    assert_eq!(bi("4294967296").max_num_bits(), 64);
    assert_eq!(bi("0").num_digits(), 0);
    assert_eq!(bi("1").num_digits(), 1);
    assert_eq!(bi("4294967296").num_digits(), 2);
    assert_eq!(bi("0").digits(), &[] as &[u32]);
}