//! Tests for Lane C8c (`mpprint.rs`): the five `mp_print_*`
//! exports, the `Print` sink, the pad tables, `format_float_repr`,
//! and the two platform printers — plus C8b-probe pins through the
//! new `Print`-based API (the unification must not move C8b's
//! observable behaviour).

use super::mpprint::*;
use super::objstr_print::{
    PF_ADD_PERCENT, PF_ALWAYS_DECIMAL, PF_CENTER_ADJUST, PF_LEFT_ADJUST,
    PF_PAD_AFTER_SIGN, PF_SEP_POS, PF_SHOW_OCTAL_LETTER, PF_SHOW_PREFIX,
    PF_SHOW_SIGN, PF_SPACE_SIGN,
};
use super::objstr_tests::{big_2_pow_70, float_of, iv, Fx};
use super::vstr::Vstr;
use crate::objects::value::Value;

fn out() -> Vec<u8> {
    Vec::new()
}

fn text(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf).into_owned()
}

#[test]
fn mp_print_str_basic() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(mp_print_str(&mut p, b"hello").unwrap(), 5);
    assert_eq!(mp_print_str(&mut p, b"").unwrap(), 0);
    assert_eq!(text(&buf), "hello");
}

#[test]
fn mp_print_strn_right_pad() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(mp_print_strn(&mut p, b"ab", 0, b' ', 5).unwrap(), 5);
    assert_eq!(text(&buf), "   ab");
}

#[test]
fn mp_print_strn_left_adjust() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_strn(&mut p, b"ab", PF_LEFT_ADJUST, b' ', 5).unwrap(),
        5
    );
    assert_eq!(text(&buf), "ab   ");
}

#[test]
fn mp_print_strn_center() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_strn(&mut p, b"ab", PF_CENTER_ADJUST, b' ', 6).unwrap(),
        6
    );
    assert_eq!(text(&buf), "  ab  ");
}

#[test]
fn mp_print_strn_zero_fill() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(mp_print_strn(&mut p, b"42", 0, b'0', 5).unwrap(), 5);
    assert_eq!(text(&buf), "00042");
}

#[test]
fn mp_print_strn_grouped_comma() {
    // ',' grouping, fill '0', width 10: window is PAD_COMMON[18..22]
    // ("00,0"), left pad 3 → "00,".
    let flags = (b',' as u32) << PF_SEP_POS;
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_strn(&mut p, b"1234567", flags, b'0', 10).unwrap(),
        10
    );
    assert_eq!(text(&buf), "00,1234567");
}

#[test]
fn mp_print_strn_grouped_bump() {
    // Width a multiple of the window size (8 % 4 == 0): pad += 1,
    // width += 1, window offset shifts — the result never starts
    // with a grouping character.
    let flags = (b',' as u32) << PF_SEP_POS;
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_strn(&mut p, b"1234567", flags, b'0', 8).unwrap(),
        9
    );
    assert_eq!(text(&buf), "0,1234567");
}

#[test]
fn pad_tables_byte_exact() {
    // `mpprint.zig:39-48`, byte-identical to vendor `mpprint.c`.
    assert_eq!(PAD_SPACES, b"                ");
    assert_eq!(PAD_COMMON, b"0000000000000000_000,00");
    assert_eq!(&PAD_COMMON[12..17], b"0000_");
    assert_eq!(&PAD_COMMON[17..21], b"000,");
}

#[test]
fn mp_print_int_decimal() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(mp_print_int(&mut p, 42, 1, 10, b'a' as i32, 0, b' ', 0).unwrap(), 2);
    assert_eq!(text(&buf), "42");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_int(&mut p, (-42i64) as usize, 1, 10, b'a' as i32, 0, b' ', 0).unwrap(),
        3
    );
    assert_eq!(text(&buf), "-42");
}

#[test]
fn mp_print_int_sign_flags() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 42, 1, 10, b'a' as i32, PF_SHOW_SIGN, b' ', 0).unwrap();
    assert_eq!(text(&buf), "+42");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 42, 1, 10, b'a' as i32, PF_SPACE_SIGN, b' ', 0).unwrap();
    assert_eq!(text(&buf), " 42");
    // `sgn == 0`: no sign even with SHOW_SIGN.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 42, 0, 10, b'a' as i32, PF_SHOW_SIGN, b' ', 0).unwrap();
    assert_eq!(text(&buf), "42");
}

#[test]
fn mp_print_int_bases() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 255, 0, 16, b'a' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "ff");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 255, 0, 16, b'A' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "FF");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 255, 0, 8, b'a' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "377");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, 255, 0, 2, b'a' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "11111111");
}

#[test]
fn mp_print_int_wrapping_negate() {
    // `isize::MIN` as unsigned, `sgn` set: wrapping negate gives
    // 2^63, printed with the `-` sign.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, isize::MIN as usize, 1, 10, b'a' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "-9223372036854775808");
    // `usize::MAX` is -1 as `isize`: negate → 1.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_int(&mut p, usize::MAX, 1, 10, b'a' as i32, 0, b' ', 0).unwrap();
    assert_eq!(text(&buf), "-1");
}

#[test]
fn mp_print_int_truncation_base2() {
    // `INT_BUF_SIZE` is 32: a base-2 value needing 64 digits is
    // truncated to its LOW 32 digits (the vendor C loop breaks
    // when the buffer fills). Ported verbatim.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_int(&mut p, usize::MAX, 0, 2, b'a' as i32, 0, b' ', 0).unwrap(),
        32
    );
    assert_eq!(text(&buf), "1".repeat(32));
}

#[test]
fn mp_print_int_pad_after_sign() {
    // `%05d`-style: sign first, then zero-filled digits.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_int(
            &mut p,
            42,
            1,
            10,
            b'a' as i32,
            PF_SHOW_SIGN | PF_PAD_AFTER_SIGN,
            b'0',
            5
        )
        .unwrap(),
        5
    );
    assert_eq!(text(&buf), "+0042");
}

#[test]
fn mp_print_mp_int_small() {
    let mut fx = Fx::new();
    let types = fx.types;
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert_eq!(
        mp_print_mp_int(&mut p, &mut fx.ctx(), &types, iv(42), 10, b'a', 0, b' ', 5, 0)
            .unwrap(),
        5
    );
    assert_eq!(text(&buf), "   42");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(&mut p, &mut fx.ctx(), &types, iv(-42), 10, b'a', 0, b' ', 0, 0)
        .unwrap();
    assert_eq!(text(&buf), "-42");
}

#[test]
fn mp_print_mp_int_big() {
    let mut fx = Fx::new();
    let types = fx.types;
    let big = big_2_pow_70(&mut fx);
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(&mut p, &mut fx.ctx(), &types, big, 10, b'a', 0, b' ', 0, 0)
        .unwrap();
    assert_eq!(text(&buf), "1180591620717411303424");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(&mut p, &mut fx.ctx(), &types, big, 16, b'a', 0, b' ', 0, 0)
        .unwrap();
    assert_eq!(text(&buf), "400000000000000000");
}

#[test]
fn mp_print_mp_int_prefix_prec() {
    // `prec > 1` with a prefix: "0x" first, then zero-filled
    // digits to the precision.
    let mut fx = Fx::new();
    let types = fx.types;
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(
        &mut p,
        &mut fx.ctx(),
        &types,
        iv(255),
        16,
        b'a',
        PF_SHOW_PREFIX,
        b' ',
        0,
        4,
    )
    .unwrap();
    assert_eq!(text(&buf), "0x00ff");
    // Octal letter gated on SHOW_OCTAL_LETTER.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(
        &mut p,
        &mut fx.ctx(),
        &types,
        iv(8),
        8,
        b'a',
        PF_SHOW_PREFIX | PF_SHOW_OCTAL_LETTER,
        b' ',
        0,
        0,
    )
    .unwrap();
    assert_eq!(text(&buf), "0o10");
}

#[test]
fn mp_print_mp_int_non_int() {
    // No int conversion: the Zig's `mp_obj_get_int` raise
    // (`TypeError: can't convert to int`).
    let mut fx = Fx::new();
    let types = fx.types;
    let fl = float_of(&mut fx, 3.0);
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert!(
        mp_print_mp_int(&mut p, &mut fx.ctx(), &types, fl, 10, b'a', 0, b' ', 0, 0)
            .is_err()
    );
    let s = fx.s(b"ab");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    assert!(
        mp_print_mp_int(&mut p, &mut fx.ctx(), &types, s, 10, b'a', 0, b' ', 0, 0)
            .is_err()
    );
}

#[test]
fn mp_print_float_basic() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 3.14159, b'f', 0, b' ', 0, 2).unwrap();
    assert_eq!(text(&buf), "3.14");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 3.14159, b'f', 0, b' ', 0, -1).unwrap();
    assert_eq!(text(&buf), "3.141590");
}

#[test]
fn mp_print_float_flags() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 3.14159, b'f', PF_SHOW_SIGN, b' ', 0, 2).unwrap();
    assert_eq!(text(&buf), "+3.14");
    // ALWAYS_DECIMAL appends ".0" when there is no '.', 'e', 'n'.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 42.0, b'g', PF_ALWAYS_DECIMAL, b' ', 0, -1).unwrap();
    assert_eq!(text(&buf), "42.0");
    // ADD_PERCENT appends '%'.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 3.14159, b'f', PF_ADD_PERCENT, b' ', 0, 2).unwrap();
    assert_eq!(text(&buf), "3.14%");
}

#[test]
fn mp_print_float_repr() {
    // `prec == 99`, `fmt == 'g'`: shortest round-trip digits.
    for (f, want) in [
        (2.5, "2.5"),
        (1e16, "1e+16"),
        (1.5e-5, "1.5e-05"),
        (0.1, "0.1"),
        (0.0, "0"),
        (-2.5, "-2.5"),
        (1234567.0, "1234567"),
        (1e15, "1000000000000000"),
        (5e-324, "5e-324"),
        (0.0001, "0.0001"),
        (2.0f64.sqrt(), "1.4142135623730951"),
    ] {
        let mut buf = out();
        let mut p = Print::new(&mut buf);
        mp_print_float(&mut p, f, b'g', 0, b' ', 0, MP_FLOAT_REPR_PREC).unwrap();
        assert_eq!(text(&buf), want, "repr({f:?})");
    }
    // The binary's `repr()` is this branch PLUS `ALWAYS_DECIMAL`
    // framing (probed: repr(1234567.0) → '1234567.0', while the
    // bare branch gives '1234567').
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 1234567.0, b'g', PF_ALWAYS_DECIMAL, b' ', 0, MP_FLOAT_REPR_PREC)
        .unwrap();
    assert_eq!(text(&buf), "1234567.0");
}

#[test]
fn format_float_repr_direct() {
    for (f, want) in [
        (2.5, "2.5"),
        (1e16, "1e+16"),
        (0.0001, "0.0001"),
        (1.5e-5, "1.5e-05"),
        (-0.0, "-0"),
        (9999999999999998.0, "9999999999999998"),
        (1e15, "1000000000000000"),
    ] {
        let mut buf = [0u8; 33];
        let n = format_float_repr(f, &mut buf, 0);
        assert_eq!(&buf[..n], want.as_bytes(), "repr({f:?})");
        assert_eq!(buf[n], 0, "NUL terminator");
    }
    // Explicit sign byte when the value is non-negative.
    let mut buf = [0u8; 33];
    let n = format_float_repr(2.5, &mut buf, b'+');
    assert_eq!(&buf[..n], b"+2.5");
}

#[test]
fn print_sink_multi_segment() {
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_str(&mut p, b"ab").unwrap();
    mp_print_strn(&mut p, b"cd", 0, b' ', 4).unwrap();
    mp_print_str(&mut p, b"ef").unwrap();
    assert_eq!(text(&buf), "ab  cdef");
}

#[test]
fn print_sink_vstr() {
    let mut v = Vstr::new(16);
    let mut p = Print::new(&mut v);
    mp_print_strn(&mut p, b"ab", 0, b' ', 5).unwrap();
    assert_eq!(v.as_bytes(), b"   ab");
}

#[test]
fn platform_printers_construct() {
    // The consts back a `Print`; emitting empty bytes is a no-op
    // observable (real fd output is not asserted in unit tests —
    // it is the platform's `write(1)` / `write(2)`).
    let mut pt = MP_PLAT_PRINT;
    let mut p = Print::new(&mut pt);
    assert_eq!(mp_print_str(&mut p, b"").unwrap(), 0);
    let mut pt = MP_STDERR_PRINT;
    let mut p = Print::new(&mut pt);
    assert_eq!(mp_print_str(&mut p, b"").unwrap(), 0);
}

#[test]
fn c8b_probes_through_print_api() {
    // C8b probe pins, re-checked through the new `Print` API:
    // "%5.2f" % 3.14159 → ' 3.14'; "%05d" % -42 → '-0042';
    // "%5.2d" % 3 → '   03'.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_float(&mut p, 3.14159, b'f', 0, b' ', 5, 2).unwrap();
    assert_eq!(text(&buf), " 3.14");
    let mut fx = Fx::new();
    let types = fx.types;
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(
        &mut p,
        &mut fx.ctx(),
        &types,
        iv(-42),
        10,
        b'a',
        PF_PAD_AFTER_SIGN,
        b'0',
        5,
        -1,
    )
    .unwrap();
    assert_eq!(text(&buf), "-0042");
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(&mut p, &mut fx.ctx(), &types, iv(3), 10, b'a', 0, b' ', 5, 2)
        .unwrap();
    assert_eq!(text(&buf), "   03");
    // `%s` on a non-str value is not this layer's job; `Value`
    // passthrough sanity: a bool prints as an int.
    let mut buf = out();
    let mut p = Print::new(&mut buf);
    mp_print_mp_int(
        &mut p,
        &mut fx.ctx(),
        &types,
        Value::Bool(true),
        10,
        b'a',
        0,
        b' ',
        0,
        0,
    )
    .unwrap();
    assert_eq!(text(&buf), "1");
}
