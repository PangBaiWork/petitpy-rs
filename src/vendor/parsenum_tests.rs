//! Tests for the canonical `parsenum` entry points (Lane C17).
//!
//! Every vector below was probed on the reference binary first
//! (`~/workspace/petitpy-orig/zig-out/bin/petitpy`, `int()` /
//! `float()` / literal forms) or derived from the Zig source
//! (`src/frontend/parsenum.zig`, `parsenumbase.zig`) with the
//! probe noted. The `.mpy` const-loader cases live in
//! `runtime::vm_raise_tests` (they need a `CallCtx`).

use super::parsenum::{
    parse_float_internal, parse_float_internal_with_len, parse_num_base, parse_num_complex,
    parse_num_decimal, parse_num_float, parse_num_integer, DecimalNum, ParseNumError,
    ParseNumIntError,
};
use crate::vendor::mpz::BigInt;

fn int_text(s: &str, base: i32) -> String {
    let z = parse_num_integer(s.as_bytes(), base).expect("parse_num_integer");
    format!("{z}")
}

fn int_err(s: &str, base: i32) -> ParseNumIntError {
    parse_num_integer(s.as_bytes(), base).expect_err("expected parse error")
}

fn float_val(s: &str) -> f64 {
    match parse_num_float(s.as_bytes(), false).expect("parse_num_float") {
        DecimalNum::Float(f) => f,
        DecimalNum::Complex { .. } => panic!("{s} parsed as complex"),
    }
}

fn float_err(s: &str, allow_imag: bool) -> ParseNumError {
    parse_num_float(s.as_bytes(), allow_imag).expect_err("expected parse error")
}

// ── `mp_parse_num_base` ─────────────────────────────────────────────

#[test]
fn parse_num_base_auto_detect() {
    // (input, base_in) -> (consumed, base_out)
    let cases: &[(&[u8], i32, usize, i32)] = &[
        (b"0x10", 0, 2, 16),
        (b"0X10", 0, 2, 16),
        (b"0o17", 0, 2, 8),
        (b"0O17", 0, 2, 8),
        (b"0b11", 0, 2, 2),
        (b"0B11", 0, 2, 2),
        (b"010", 0, 0, 1), // leading zero, no prefix letter → base 1
        (b"0z", 0, 0, 1),
        (b"10", 0, 0, 10),
        (b"1", 0, 0, 10),
    ];
    for &(s, base_in, want_n, want_base) in cases {
        let mut base = base_in;
        assert_eq!(parse_num_base(s, &mut base), want_n, "input {s:?}");
        assert_eq!(base, want_base, "input {s:?}");
    }
}

#[test]
fn parse_num_base_explicit_base() {
    // A prefix letter only counts when it matches the requested base.
    let cases: &[(&[u8], i32, usize, i32)] = &[
        (b"0x10", 16, 2, 16),
        (b"0x10", 10, 0, 10),
        (b"0o17", 8, 2, 8),
        (b"0o17", 10, 0, 10),
        (b"0b11", 2, 2, 2),
        (b"0b11", 10, 0, 10),
        (b"0x10", 2, 0, 2),
        (b"ff", 16, 0, 16), // tests.zig: no prefix at all -> 0 consumed
        (b"010", 8, 0, 8),
        (b"010", 16, 0, 16),
    ];
    for &(s, base_in, want_n, want_base) in cases {
        let mut base = base_in;
        assert_eq!(parse_num_base(s, &mut base), want_n, "input {s:?}");
        assert_eq!(base, want_base, "input {s:?}");
    }
}

#[test]
fn parse_num_base_short_and_bare_prefix() {
    // len <= 1 falls through to the `*base == 0 → 10` tail; the
    // prefix is consumed even with no digits after it.
    let cases: &[(&[u8], i32, usize, i32)] = &[
        (b"", 0, 0, 10),
        (b"0", 0, 0, 10),
        (b"1", 0, 0, 10),
        (b"0", 16, 0, 16),
        (b"0b", 0, 2, 2),
        (b"0x", 0, 2, 16),
        (b"0o", 0, 2, 8),
    ];
    for &(s, base_in, want_n, want_base) in cases {
        let mut base = base_in;
        assert_eq!(parse_num_base(s, &mut base), want_n, "input {s:?}");
        assert_eq!(base, want_base, "input {s:?}");
    }
}

// ── `mp_parse_num_integer` ──────────────────────────────────────────

#[test]
fn parse_num_integer_decimal_sign_whitespace() {
    // Probed: int(" 42 ") == 42, int("+42") == 42.
    assert_eq!(int_text("42", 10), "42");
    assert_eq!(int_text("  42  ", 10), "42");
    assert_eq!(int_text("+42", 10), "42");
    assert_eq!(int_text("-42", 10), "-42");
    assert_eq!(int_text("0", 10), "0");
    assert_eq!(int_text("00", 10), "0");
    assert_eq!(int_text("\t\n 7\r ", 10), "7");
    assert_eq!(int_text("10", 2), "2");
    assert_eq!(int_text("ff", 16), "255");
    assert_eq!(int_text("FF", 16), "255");
    assert_eq!(int_text("z", 36), "35");
}

#[test]
fn parse_num_integer_prefixes() {
    // Probed: 0o17/0x1f/0b101 literals == 15/31/5; int("010") == 10
    // (explicit base 10), int("010", 0) errors (base-0 → base 1).
    assert_eq!(int_text("0x1f", 0), "31");
    assert_eq!(int_text("0XFF", 0), "255");
    assert_eq!(int_text("0o17", 0), "15");
    assert_eq!(int_text("0b101", 0), "5");
    assert_eq!(int_text("0x0", 0), "0");
    assert_eq!(int_text("-0x10", 0), "-16");
    assert_eq!(int_text("  -0b11 ", 0), "-3");
    assert_eq!(int_text("010", 10), "10");
    assert_eq!(int_text("0x10", 16), "16");
}

#[test]
fn parse_num_integer_underscores_anywhere() {
    // Probed: int("_") == 0, int("1_000") == 1000. The `_` is
    // skipped anywhere, including leading/trailing/doubled.
    assert_eq!(int_text("_", 10), "0");
    assert_eq!(int_text("_1", 10), "1");
    assert_eq!(int_text("1_", 10), "1");
    assert_eq!(int_text("1__2", 10), "12");
    assert_eq!(int_text("1_000", 10), "1000");
    assert_eq!(int_text("0x_ff", 0), "255");
    assert_eq!(int_text("__", 10), "0");
}

#[test]
fn parse_num_integer_big_values() {
    // Outside the 62-bit small-int window the Zig re-parses via
    // mpz; the BigInt accumulation gives the identical value.
    assert_eq!(int_text("4611686018427387904", 10), "4611686018427387904"); // 2**62
    assert_eq!(int_text("1180591620717411303424", 10), "1180591620717411303424"); // 2**70
    assert_eq!(
        int_text("-1180591620717411303424", 10),
        "-1180591620717411303424"
    );
    assert_eq!(
        int_text("0xffffffffffffffff", 0),
        "18446744073709551615"
    );
    assert_eq!(
        parse_num_integer(b"4611686018427387904", 10).unwrap(),
        BigInt::from_int(4611686018427387904)
    );
}

#[test]
fn parse_num_integer_bad_base() {
    for base in [1, -1, 37, 100] {
        assert_eq!(int_err("1", base), ParseNumIntError::BadBase);
    }
    // The message literal lives on PrimError::message (the
    // objint::parse_int_text mapping target).
    assert_eq!(
        crate::objects::primitives::ops::PrimError::IntBaseRange.message(),
        Some("int() arg 2 must be >= 2 and <= 36")
    );
}

#[test]
fn parse_num_integer_invalid_syntax() {
    // Probed: int("010", 0), int("0b102"), int("12z") all raise
    // ValueError "invalid syntax for integer".
    for (s, base) in [
        ("010", 0),
        ("0b102", 0),
        ("12z", 10),
        ("1.5", 10),
        ("", 10),
        ("  ", 10),
        ("0x", 0),
        ("12", 2),
        ("-", 10),
        ("+", 10),
    ] {
        assert_eq!(int_err(s, base), ParseNumIntError::InvalidSyntax, "{s:?}");
    }
    assert_eq!(
        crate::objects::primitives::ops::PrimError::InvalidSyntaxForInt.message(),
        Some("invalid syntax for integer")
    );
}

// ── `mp_parse_float_internal` (with length) ─────────────────────────

#[test]
fn parse_float_internal_with_len_consumed() {
    // The Zig returns the pointer just past the scan.
    assert_eq!(parse_float_internal_with_len(b"1.5x"), Some((3, 1.5)));
    assert_eq!(parse_float_internal_with_len(b"12abc"), Some((2, 12.0)));
    assert_eq!(
        parse_float_internal_with_len(b"1_000.5rest"),
        Some((7, 1000.5))
    );
    assert_eq!(parse_float_internal_with_len(b"1e-3tail"), Some((4, 0.001)));
    assert_eq!(parse_float_internal_with_len(b"5."), Some((2, 5.0)));
    assert_eq!(parse_float_internal_with_len(b".5"), Some((2, 0.5)));
    assert_eq!(parse_float_internal_with_len(b""), Some((0, 0.0)));
    assert_eq!(parse_float_internal_with_len(b"abc"), Some((0, 0.0)));
}

#[test]
fn parse_float_internal_with_len_dangling_exponent() {
    // The Zig returns NULL here; the f64 wrapper maps it to 0.0.
    assert_eq!(parse_float_internal_with_len(b"1e+"), None);
    assert_eq!(parse_float_internal_with_len(b"1e-"), None);
    assert_eq!(parse_float_internal_with_len(b"1e"), None);
    assert_eq!(parse_float_internal(b"1e+"), 0.0);
    assert_eq!(parse_float_internal(b"1.5x"), 1.5);
}

// ── `mp_parse_num_float` ────────────────────────────────────────────

#[test]
fn parse_num_float_plain_values() {
    // Probed as literals: 1.5, .5, 5., 1e3, 1e+3, 1e-3.
    assert_eq!(float_val("1.5"), 1.5);
    assert_eq!(float_val(".5"), 0.5);
    assert_eq!(float_val("5."), 5.0);
    assert_eq!(float_val("1e3"), 1000.0);
    assert_eq!(float_val("1E3"), 1000.0);
    assert_eq!(float_val("1e+3"), 1000.0);
    assert_eq!(float_val("1e-3"), 0.001);
    assert_eq!(float_val("0.001"), 0.001);
    assert_eq!(float_val(" 1.5 "), 1.5);
    assert_eq!(float_val("1_0.5"), 10.5);
    assert_eq!(float_val("1_000.5"), 1000.5);
    assert_eq!(float_val("+2.5"), 2.5);
    assert_eq!(float_val("-2.5"), -2.5);
    // Correctly-rounded subnormal (the FMT strtod finding).
    assert_eq!(
        float_val("2.2250738585072014e-308"),
        "2.2250738585072014e-308".parse::<f64>().unwrap()
    );
}

#[test]
fn parse_num_float_inf_nan() {
    // Probed: float("inf") / ("infinity") / ("nan") — case-insensitive.
    assert_eq!(float_val("inf"), f64::INFINITY);
    assert_eq!(float_val("Infinity"), f64::INFINITY);
    assert_eq!(float_val("INFINITY"), f64::INFINITY);
    assert_eq!(float_val("-inf"), f64::NEG_INFINITY);
    assert!(float_val("nan").is_nan());
    assert!(float_val("NaN").is_nan());
    assert!(float_val("+nan").is_nan());
    assert!(float_val("-nan").is_nan());
    // "in" / "na" are too short for the inf/nan branches.
    assert_eq!(float_err("in", false), ParseNumError::InvalidNumber);
    assert_eq!(float_err("na", false), ParseNumError::InvalidNumber);
}

#[test]
fn parse_num_float_errors() {
    // Probed: float("1e+"), float("1e"), float("1.5j"),
    // float("j"), float("3+4j") all raise ValueError
    // "invalid syntax for number".
    for s in [
        "1e", "1e+", "1e-", "1.5j", "j", "3+4j", "", "abc", "1.5.2", "  ", "-", "+",
        "1.5e2.5",
    ] {
        assert_eq!(float_err(s, false), ParseNumError::InvalidNumber, "{s:?}");
    }
    // allow_imag=true still rejects a bare "3+4j" (force_complex
    // is false for mp_parse_num_float).
    assert_eq!(float_err("3+4j", true), ParseNumError::InvalidNumber);
    assert_eq!(float_err("1+2j", true), ParseNumError::InvalidNumber);
    assert_eq!(
        ParseNumError::InvalidNumber.message(),
        "invalid syntax for number"
    );
}

// ── `mp_parse_num_decimal` (imag + force_complex) ───────────────────

#[test]
fn parse_num_decimal_imag_suffix() {
    // Probed as literals: 1j, 1.5j, 1e-2j, 1.5e2j all raise at
    // LOAD time as FLOAT consts — but as a decimal parse with
    // allow_imag they are Complex values (the loader calls this
    // with allow_imag=true only for COMPLEX entries).
    let c = |s: &str| parse_num_decimal(s.as_bytes(), true, false).expect(s);
    assert_eq!(c("1j"), DecimalNum::Complex { re: 0.0, im: 1.0 });
    assert_eq!(c("j"), DecimalNum::Complex { re: 0.0, im: 1.0 });
    assert_eq!(c("-j"), DecimalNum::Complex { re: 0.0, im: -1.0 });
    assert_eq!(c("1.5j"), DecimalNum::Complex { re: 0.0, im: 1.5 });
    assert_eq!(c("1e-2j"), DecimalNum::Complex { re: 0.0, im: 0.01 });
    assert_eq!(c("1.5e2j"), DecimalNum::Complex { re: 0.0, im: 150.0 });
    assert_eq!(
        c("infj"),
        DecimalNum::Complex { re: 0.0, im: f64::INFINITY }
    );
    assert_eq!(
        c("infinityj"),
        DecimalNum::Complex { re: 0.0, im: f64::INFINITY }
    );
    assert!(matches!(c("nanj"), DecimalNum::Complex { re: 0.0, im } if im.is_nan()));
    // No 'j' → a plain float even with allow_imag.
    assert_eq!(c("1.5"), DecimalNum::Float(1.5));
}

#[test]
fn parse_num_decimal_force_complex_two_pass() {
    // `mp_parse_num_complex` (vendor parsenum.h:51-53) is
    // decimal(allow_imag=true, force_complex=true).
    let c = |s: &str| parse_num_complex(s.as_bytes()).expect(s);
    assert_eq!(c("3+4j"), DecimalNum::Complex { re: 3.0, im: 4.0 });
    assert_eq!(c("3-4j"), DecimalNum::Complex { re: 3.0, im: -4.0 });
    assert_eq!(c("1.5"), DecimalNum::Complex { re: 1.5, im: 0.0 });
    assert_eq!(c("1j"), DecimalNum::Complex { re: 0.0, im: 1.0 });
    assert_eq!(c("-3-4j"), DecimalNum::Complex { re: -3.0, im: -4.0 });
    assert_eq!(c("1e2+3j"), DecimalNum::Complex { re: 100.0, im: 3.0 });
    assert_eq!(c("1e-2j"), DecimalNum::Complex { re: 0.0, im: 0.01 });
    // Trailing whitespace is skipped before the end check.
    assert_eq!(c("3+4j "), DecimalNum::Complex { re: 3.0, im: 4.0 });
}

#[test]
fn parse_num_decimal_force_complex_errors() {
    // "3+" with no imag follow-up; a second part without 'j';
    // a third part after the imag part.
    for s in ["3+", "3+4", "3+4j+5j", "3 + "] {
        assert!(
            parse_num_decimal(s.as_bytes(), true, true).is_err(),
            "{s:?} should fail"
        );
    }
    // Whitespace is skipped before the sign, but NOT between the
    // sign and the digits — the second pass of "3 + 4j" sees " 4j"
    // after consuming '+', so the float scan consumes nothing and
    // the parse fails (probed: a COMPLEX .mpy entry "3 + 4j"
    // raises ValueError on the reference binary).
    assert!(parse_num_decimal(b"3 + 4j", true, true).is_err());
    // Without force_complex, "3+4j" fails (trailing garbage).
    assert!(parse_num_decimal(b"3+4j", true, false).is_err());
}

// ── `is_space` agrees with the canonical predicate ─────────────────

#[test]
fn is_space_agrees_with_unichar_isspace() {
    // The one-line copy in `parsenum.rs` must stay identical to
    // the canonical `unichar_isspace` (C16); checked on all 256
    // byte values so a drift fails loudly here, not in a parse.
    for b in 0..=255u8 {
        assert_eq!(
            super::parsenum::is_space(b),
            crate::objects::strings::unicode::unichar_isspace(b as u32),
            "byte {b:#04x}"
        );
    }
}
