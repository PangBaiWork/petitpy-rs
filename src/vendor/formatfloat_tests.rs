//! Unit tests for the formatfloat port.
//!
//! Expected values here are taken from the reference binary
//! (`~/workspace/petitpy-orig/zig-out/bin/petitpy`, `%` formatting)
//! or from the C code by hand where noted; the differential probe
//! battery (`probe_fmt.rs`, `scripts/fmt_vectors.txt`) is the
//! exhaustive check against both oracles.

use crate::vendor::formatfloat::*;

fn into_buf(f: f64, fmt: u8, prec: i32, sign: u8, size: usize) -> (i32, String) {
    let mut buf = vec![0u8; size];
    let len = format_float_into_buf(f, fmt, prec, sign, &mut buf);
    assert_eq!(buf[len as usize], 0, "NUL terminator missing");
    (
        len,
        String::from_utf8(buf[..len as usize].to_vec()).unwrap(),
    )
}

fn check(f: f64, fmt: u8, prec: i32, sign: u8, want: &str) {
    let (len, got) = into_buf(f, fmt, prec, sign, 33);
    assert_eq!(got, want, "f={f:?} fmt={} prec={prec} sign={sign}", fmt as char);
    assert_eq!(len as usize, want.len());
}

#[test]
fn fmt_kinds() {
    check(1.5, b'f', 6, 0, "1.500000");
    check(1.5, b'F', 6, 0, "1.500000");
    check(1.5, b'e', 6, 0, "1.500000e+00");
    check(1.5, b'E', 6, 0, "1.500000E+00");
    check(1.5, b'g', 6, 0, "1.5");
    check(1.5, b'G', 6, 0, "1.5");
    check(1234567.0, b'g', 6, 0, "1.23457e+06");
    check(1234567.0, b'G', 6, 0, "1.23457E+06");
    // Any other fmt byte behaves like 'e' (C: else branch).
    check(1.5, b'x', 6, 0, "1.500000e+00");
    check(1.5, b'r', 2, 0, "1.50e+00");
}

#[test]
fn prec_boundaries() {
    // prec < 0 defaults to 6.
    check(1.5, b'f', -1, 0, "1.500000");
    check(1.5, b'f', -100, 0, "1.500000");
    check(2.5, b'f', 0, 0, "3");
    check(3.5, b'f', 0, 0, "4");
    // 0.5 'f' 0 -> "0", not "1": the dichotomic search ties prefer
    // mantissa 0 (|0-0.5| == |1-0.5| and 0 % 10 == 0). Binary agrees.
    check(0.5, b'f', 0, 0, "0");
    check(1.25, b'f', 1, 0, "1.3"); // round-half-up via +0.5
    check(1.5, b'e', 0, 0, "2e+00");
    check(1.5, b'g', 0, 0, "2");
    check(1.5, b'g', 1, 0, "2");
    // 'f' 17/19: last digits ...45660, not CPython's ...45669 — the
    // vendor algorithm is approximate. Binary agrees.
    check(1.23456789012345678, b'f', 17, 0, "1.23456789012345660");
    check(1.23456789012345678, b'f', 19, 0, "1.2345678901234566000");
    // prec 99 is the optimal-repr path (prec -> 19, max_exp_zeros 16).
    check(std::f64::consts::PI, b'g', 99, 0, "3.141592653589793");
    check(1e17, b'g', 99, 0, "1e+17");
    // 1e16 'g' 99 -> "1e+16", not "10000000000000000": at prec 99 the
    // 'g' threshold keeps it in 'e' style. Verified via harness_ziglike
    // (the binary intercepts prec 99 'g' for repr, so it cannot be
    // reached through % formatting).
    check(1e16, b'g', 99, 0, "1e+16");
    check(123456789.123456789, b'g', 99, 0, "123456789.12345679");
    // Huge prec is clamped by the 33-byte buffer.
    check(1.5, b'f', 100, 0, "1.50000000000000000000000000000");
}

#[test]
fn sign_handling() {
    check(1.5, b'f', 6, b'+', "+1.500000");
    check(1.5, b'f', 6, b' ', " 1.500000");
    check(-1.5, b'f', 6, b'+', "-1.500000");
    check(-1.5, b'f', 6, 0, "-1.500000");
    check(-1.5, b'f', 6, b' ', "-1.500000");
    check(-0.0, b'f', 6, 0, "-0.000000");
    check(-0.0, b'g', 6, 0, "-0");
    check(0.0, b'f', 6, b'+', "+0.000000");
    check(0.0, b'g', 6, 0, "0");
    check(0.0, b'e', 6, 0, "0.000000e+00");
    check(0.0, b'f', 0, 0, "0");
}

#[test]
fn inf_and_nan() {
    check(f64::INFINITY, b'f', 6, 0, "inf");
    check(f64::INFINITY, b'F', 6, 0, "INF");
    check(f64::INFINITY, b'e', 6, 0, "inf");
    check(f64::INFINITY, b'E', 6, 0, "INF");
    check(f64::INFINITY, b'g', 6, 0, "inf");
    check(f64::INFINITY, b'G', 6, 0, "INF");
    check(f64::INFINITY, b'f', 6, b'+', "+inf");
    check(f64::NEG_INFINITY, b'f', 6, 0, "-inf");
    check(f64::NEG_INFINITY, b'f', 6, b'+', "-inf");
    check(f64::NAN, b'f', 6, 0, "nan");
    check(f64::NAN, b'F', 6, 0, "NAN");
    check(f64::NAN, b'f', 6, b'+', "+nan");
    // Negative NaN: signbit is ignored for NaN (C: !isnan guard).
    check(-f64::NAN, b'f', 6, 0, "nan");
    check(-f64::NAN, b'f', 6, b'+', "+nan");
    // Unknown fmt byte: case bit 0x20 clear for 'X' -> "INF".
    check(f64::INFINITY, b'X', 6, 0, "INF");
}

#[test]
fn magnitudes() {
    // %f with a huge exponent switches to %g (req_size > buf_size).
    check(1e300, b'f', 6, 0, "1e+300");
    check(1e300, b'f', 6, b'+', "+1e+300");
    check(1.7976931348623157e308, b'e', 6, 0, "1.797693e+308");
    // Last digit ...3158, not ...3157: the vendor algorithm is
    // approximate (CPython prints ...3157). Binary agrees.
    check(1.7976931348623157e308, b'g', 17, 0, "1.7976931348623158e+308");
    check(1e-300, b'f', 6, 0, "0.000000");
    check(1e-300, b'e', 6, 0, "1.000000e-300");
    check(1e-300, b'g', 6, 0, "1e-300");
    check(0.0001, b'g', 6, 0, "0.0001");
    check(0.00001, b'g', 6, 0, "1e-05");
    check(0.000123, b'g', 6, 0, "0.000123");
    check(0.0000123, b'g', 6, 0, "1.23e-05");
    check(123456.0, b'g', 6, 0, "123456");
    check(1234567.0, b'g', 6, 0, "1.23457e+06");
    check(100000.0, b'g', 6, 0, "100000");
    check(1000000.0, b'g', 6, 0, "1e+06");
    // Last digit ...952, not CPython's ...951 — approximate. Binary agrees.
    check(2.0f64.sqrt(), b'g', 17, 0, "1.4142135623730952");
    // 'f' 17: ...9520, not CPython's ...9515 — approximate. Binary agrees.
    check(2.0f64.sqrt(), b'f', 17, 0, "1.41421356237309520");
}

#[test]
fn subnormals() {
    // Vendor algorithm is approximate here (see module docs); these
    // values are what the reference binary prints.
    check(5e-324, b'f', 6, 0, "0.000000");
    check(5e-324, b'e', 6, 0, "0.955161e-323");
    check(5e-324, b'g', 6, 0, "9.55161e-323");
    check(5e-324, b'g', 17, 0, "4.467440737095516e-323");
    check(5e-324, b'e', 1, 0, "0.9e-323");
    check(1e-320, b'e', 6, 0, "5.410060e-319");
    check(1e-320, b'g', 17, 0, "4.467440737095516e-319");
    // ...2012, not ...2014: the Zig parsenum comment's own example
    // ("2.2250738585072014e-308") — the approximate parse is off by
    // an ulp here; CPython prints ...2014. Binary agrees.
    check(2.2250738585072014e-308, b'g', 17, 0, "2.2250738585072012e-308");
}

#[test]
fn buffer_sizes() {
    // MIN_BUF_SIZE (29) is enough for the default cases.
    let (len, got) = into_buf(1.5, b'f', 6, 0, 29);
    assert_eq!(got, "1.500000");
    assert_eq!(len, 8);
    // 33 is what the real callers pass (36-byte buffer minus 3).
    // Last digit ...566, not ...568 (CPython prints ...567): the
    // vendor algorithm is approximate. Binary agrees.
    let (len, got) = into_buf(1.23456789012345678, b'g', 17, 0, 33);
    assert_eq!(got, "1.2345678901234566");
    assert_eq!(len, 18);
    // A larger buffer lets more digits through for huge prec.
    let (len, got) = into_buf(1.5, b'f', 100, 0, 128);
    assert_eq!(len, 102);
    assert!(got.starts_with("1.50000000000000000000000000000"));
    assert_eq!(got.len(), 102);
    // Buffer just big enough vs just too small for the same output:
    // 1e300 'f' 6 switches to 'g' at buf 33 but a 512 buffer holds 'f'.
    let (_, got33) = into_buf(1e300, b'f', 6, 0, 33);
    assert_eq!(got33, "1e+300");
    let (_, got512) = into_buf(1e300, b'f', 6, 0, 512);
    // The formatter only produces ~19 significant digits ("1" here)
    // and pads the rest with zeros — NOT the exact integer digits
    // of 1e300 (1000000000000000019884624838656...). Both C harnesses
    // agree: "1" + 300 zeros + ".000000" = 308 chars.
    assert_eq!(got512.len(), 308);
    assert!(got512.starts_with("1000000000000000000000000000000"));
    assert!(got512.ends_with("000000"));
}

#[test]
#[should_panic(expected = "MIN_BUF_SIZE")]
fn buffer_too_small_panics() {
    let mut buf = [0u8; 28];
    format_float_into_buf(1.5, b'f', 6, 0, &mut buf);
}

#[test]
fn wrapper() {
    assert_eq!(format_float(1.5, b'f', 6, 0), "1.500000");
    assert_eq!(format_float(1.5, b'g', 99, 0), "1.5");
    assert_eq!(
        format_float(std::f64::consts::PI, b'g', 99, 0),
        "3.141592653589793"
    );
    assert_eq!(format_float(f64::INFINITY, b'f', 6, b'+'), "+inf");
    assert_eq!(format_float(-0.0, b'f', 2, 0), "-0.00");
    // The wrapper's 1024-byte buffer formats huge prec without the
    // 33-byte clamping.
    let s = format_float(1.5, b'f', 100, 0);
    assert_eq!(s.len(), 102);
    assert!(s.starts_with("1.50000000000000000000000000000"));
}

#[test]
fn c_f64_to_u64_vcvttsd2usi() {
    use crate::vendor::formatfloat::c_f64_to_u64;
    // In range: truncation toward zero.
    assert_eq!(c_f64_to_u64(1.5), 1);
    assert_eq!(c_f64_to_u64(0.5), 0);
    assert_eq!(c_f64_to_u64(18446744073709551615.0), u64::MAX);
    // Out of range positive -> u64::MAX (vcvttsd2usi indefinite value),
    // NOT 0 (what gcc's lowering of the same C cast gives) and NOT a
    // saturating Rust `as` of a negative value.
    assert_eq!(c_f64_to_u64(1.13e22), u64::MAX);
    assert_eq!(c_f64_to_u64(1e30), u64::MAX);
    // NaN / negative -> u64::MAX (vcvttsd2usi); Rust `as u64` gives 0.
    assert_eq!(c_f64_to_u64(f64::NAN), u64::MAX);
    assert_eq!(c_f64_to_u64(-1.0), u64::MAX);
}

#[test]
fn decimal_exp_basics() {
    // decimal_exp is private; exercise it through format_float.
    assert_eq!(format_float(100.0, b'f', 0, 0), "100");
    assert_eq!(format_float(0.001, b'e', 6, 0), "1.000000e-03");
    assert_eq!(format_float(123.456, b'e', 3, 0), "1.235e+02");
    assert_eq!(format_float(999.999, b'f', 2, 0), "1000.00");
    assert_eq!(format_float(9.999, b'f', 2, 0), "10.00");
}
