//! Lane C8b tests — `%`-format and `str.format`.
//!
//! All expected values are probe records from the reference binary
//! (`~/workspace/petitpy-orig/zig-out/bin/petitpy`, 0.9.0); see
//! `scripts/strings_b_result.txt`. Where the reference binary aborts
//! (rc=134) the Rust behaviour is the documented intentional
//! divergence (wrap / saturate / return-self) and the test pins the
//! Rust behaviour, not the crash.

use super::objstr_tests::{
    assert_err, assert_str, big_2_pow_70, bytes_of, dict_of, float_of, fmt_pct, fmt_str, iv,
    qid, Fx,
};
use crate::objects::strings::objstr_a::{StrError, StrExc};
use crate::objects::strings::objstr_format::str_modulo_format;
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::value::Value;

// ── `%`-format ────────────────────────────────────────────────────────

#[test]
fn modulo_ints() {
    let mut fx = Fx::new();
    let r = fmt_pct(&mut fx, b"%d", iv(3)).unwrap();
    assert_str(&fx, r, b"3");
    let f = float_of(&mut fx, 3.9);
    let r = fmt_pct(&mut fx, b"%d", f).unwrap();
    assert_str(&fx, r, b"3");
    let r = fmt_pct(&mut fx, b"%d", Value::Bool(true)).unwrap();
    assert_str(&fx, r, b"1");
    let r = fmt_pct(&mut fx, b"%05d", iv(-42)).unwrap();
    assert_str(&fx, r, b"-0042");
    let t = fx.tuple(vec![iv(42), iv(42)]);
    let r = fmt_pct(&mut fx, b"%+d % d", t).unwrap();
    assert_str(&fx, r, b"+42  42");
    let t = fx.tuple(vec![iv(255), iv(255), iv(8), iv(8)]);
    let r = fmt_pct(&mut fx, b"%x %#x %o %#o", t).unwrap();
    assert_str(&fx, r, b"ff 0xff 10 0o10");
    let big = big_2_pow_70(&mut fx);
    let r = fmt_pct(&mut fx, b"%d", big).unwrap();
    assert_str(&fx, r, b"1180591620717411303424");
    let big = big_2_pow_70(&mut fx);
    let r = fmt_pct(&mut fx, b"%x", big).unwrap();
    assert_str(&fx, r, b"400000000000000000");
    let r = fmt_pct(&mut fx, b"%#x", iv(-255)).unwrap();
    assert_str(&fx, r, b"-0xff");
    let r = fmt_pct(&mut fx, b"%-05d|", iv(42)).unwrap();
    assert_str(&fx, r, b"42000|");
    let r = fmt_pct(&mut fx, b"%5.2d|", iv(3)).unwrap();
    assert_str(&fx, r, b"   03|");
    let r = fmt_pct(&mut fx, b"%-5.2d|", iv(3)).unwrap();
    assert_str(&fx, r, b"03   |");
    let t = fx.tuple(vec![iv(5), iv(42)]);
    let r = fmt_pct(&mut fx, b"%*d", t).unwrap();
    assert_str(&fx, r, b"   42");
    let f = float_of(&mut fx, 3.14159);
    let t = fx.tuple(vec![iv(2), f]);
    let r = fmt_pct(&mut fx, b"%.*f", t).unwrap();
    assert_str(&fx, r, b"3.14");
    // Negative width via `*` is NO padding (not left-adjust).
    let t = fx.tuple(vec![iv(-5), iv(42)]);
    let r = fmt_pct(&mut fx, b"%*d|", t).unwrap();
    assert_str(&fx, r, b"42|");
    // Precision 0 does NOT suppress zero.
    let r = fmt_pct(&mut fx, b"%5.0d|", iv(0)).unwrap();
    assert_str(&fx, r, b"    0|");
    let r = fmt_pct(&mut fx, b"%.0d|", iv(0)).unwrap();
    assert_str(&fx, r, b"0|");
    let f = float_of(&mut fx, 3.14);
    let r = fmt_pct(&mut fx, b"%05.2f", f).unwrap();
    assert_str(&fx, r, b"03.14");
    let f = float_of(&mut fx, -3.9);
    let r = fmt_pct(&mut fx, b"%d", f).unwrap();
    assert_str(&fx, r, b"-3");
}

#[test]
fn modulo_str_repr() {
    let mut fx = Fx::new();
    let a = fx.s(b"ab");
    let r = fmt_pct(&mut fx, b"%s", a).unwrap();
    assert_str(&fx, r, b"ab");
    let a = fx.s(b"ab");
    let r = fmt_pct(&mut fx, b"%r", a).unwrap();
    assert_str(&fx, r, b"'ab'");
    let b = fx.b(b"ab");
    let r = fmt_pct(&mut fx, b"%s", b).unwrap();
    assert_str(&fx, r, b"b'ab'");
    // A bytes pattern prints a bytes arg undecorated (PRINT_RAW).
    let pat = fx.b(b"%s");
    let b = fx.b(b"ab");
    let types = fx.types;
    let r = str_modulo_format(&mut fx.ctx(), &types, pat, b).unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    let b = fx.b(b"x");
    let r = fmt_pct(&mut fx, b"%r", b).unwrap();
    assert_eq!(bytes_of(&fx, r), b"b'x'");
    let a = fx.s(b"abc");
    let r = fmt_pct(&mut fx, b"%10.3s|", a).unwrap();
    assert_str(&fx, r, b"       abc|");
    let a = fx.s(b"abc");
    let r = fmt_pct(&mut fx, b"%5.2s|", a).unwrap();
    assert_str(&fx, r, b"   ab|");
    let a = fx.s(b"ab");
    let r = fmt_pct(&mut fx, b"%-5s|", a).unwrap();
    assert_str(&fx, r, b"ab   |");
    let a = fx.s(b"ab");
    let r = fmt_pct(&mut fx, b"%5s|", a).unwrap();
    assert_str(&fx, r, b"   ab|");
    let r = fmt_pct(&mut fx, b"%s", Value::None).unwrap();
    assert_str(&fx, r, b"None");
    let r = fmt_pct(&mut fx, b"%r", Value::None).unwrap();
    assert_str(&fx, r, b"None");
    let r = fmt_pct(&mut fx, b"%5c|", iv(65)).unwrap();
    assert_str(&fx, r, b"    A|");
    let r = fmt_pct(&mut fx, b"%c", iv(256)).unwrap();
    assert_str(&fx, r, b"\x00");
    let r = fmt_pct(&mut fx, b"%c", iv(-1)).unwrap();
    assert_str(&fx, r, b"\xff");
}

#[test]
fn modulo_floats() {
    let mut fx = Fx::new();
    let f = float_of(&mut fx, 3.14159);
    let r = fmt_pct(&mut fx, b"%5.2f", f).unwrap();
    assert_str(&fx, r, b" 3.14");
    let f = float_of(&mut fx, 12345.678);
    let r = fmt_pct(&mut fx, b"%e", f).unwrap();
    assert_str(&fx, r, b"1.234568e+04");
    let f = float_of(&mut fx, 12345.678);
    let r = fmt_pct(&mut fx, b"%E", f).unwrap();
    assert_str(&fx, r, b"1.234568E+04");
    let f = float_of(&mut fx, 0.00001);
    let r = fmt_pct(&mut fx, b"%g", f).unwrap();
    assert_str(&fx, r, b"1e-05");
    let f = float_of(&mut fx, 100000.0);
    let r = fmt_pct(&mut fx, b"%g", f).unwrap();
    assert_str(&fx, r, b"100000");
    let f = float_of(&mut fx, 1000000.0);
    let r = fmt_pct(&mut fx, b"%g", f).unwrap();
    assert_str(&fx, r, b"1e+06");
    let r = fmt_pct(&mut fx, b"%f", iv(42)).unwrap();
    assert_str(&fx, r, b"42.000000");
    let f = float_of(&mut fx, 0.1);
    let r = fmt_pct(&mut fx, b"%.20f", f).unwrap();
    assert_str(&fx, r, b"0.10000000000000000000");
    let f = float_of(&mut fx, 0.0);
    let r = fmt_pct(&mut fx, b"%e", f).unwrap();
    assert_str(&fx, r, b"0.000000e+00");
    let f = float_of(&mut fx, f64::INFINITY);
    let r = fmt_pct(&mut fx, b"%f", f).unwrap();
    assert_str(&fx, r, b"inf");
    let f = float_of(&mut fx, f64::NAN);
    let r = fmt_pct(&mut fx, b"%f", f).unwrap();
    assert_str(&fx, r, b"nan");
    // `%o` / `%x` do NOT convert floats (unlike `%d`).
    let f = float_of(&mut fx, 3.0);
    let e = fmt_pct(&mut fx, b"%o", f).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to int"));
    let f = float_of(&mut fx, 3.0);
    let e = fmt_pct(&mut fx, b"%x", f).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to int"));
    let a = fx.s(b"a");
    let e = fmt_pct(&mut fx, b"%f", a).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to float"));
    let r = fmt_pct(&mut fx, b"%o", Value::Bool(true)).unwrap();
    assert_str(&fx, r, b"1");
}

#[test]
fn modulo_dict_keys() {
    let mut fx = Fx::new();
    // "x"/"y" are NOT static qstrs: the dict keys and the lookup
    // keys are heap strs and go through `obj_equal`.
    let kx = fx.s(b"x");
    let d = dict_of(&mut fx, vec![(kx, iv(1))]);
    let r = fmt_pct(&mut fx, b"%(x)s", d).unwrap();
    assert_str(&fx, r, b"1");
    let d = dict_of(&mut fx, vec![]);
    let e = fmt_pct(&mut fx, b"%(x)s", d).unwrap_err();
    assert!(matches!(&e, StrError::KeyError(k) if bytes_of(&fx, *k) == b"x"));
    assert_err(e, StrExc::KeyError, None);
    let z = fx.s(b"z");
    let kx = fx.s(b"x");
    let ky = fx.s(b"y");
    let d = dict_of(&mut fx, vec![(kx, iv(1)), (ky, z)]);
    let r = fmt_pct(&mut fx, b"%(x)d %(y)s", d).unwrap();
    assert_str(&fx, r, b"1 z");
    // No dict → `%(key)` raises "format needs a dict".
    let e = fmt_pct(&mut fx, b"%(x)s", iv(1)).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("format needs a dict"));
}

#[test]
fn modulo_errors() {
    let mut fx = Fx::new();
    let a = fx.s(b"a");
    let e = fmt_pct(&mut fx, b"%s %s", a).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("format string needs more arguments"));
    let t = fx.tuple(vec![iv(1), iv(2)]);
    let e = fmt_pct(&mut fx, b"%d", t).unwrap_err();
    assert_err(
        e,
        StrExc::TypeError,
        Some("format string didn't convert all arguments"),
    );
    let e = fmt_pct(&mut fx, b"%b", iv(1)).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let a = fx.s(b"a");
    let e = fmt_pct(&mut fx, b"%d", a).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to int"));
    let a = fx.s(b"ab");
    let e = fmt_pct(&mut fx, b"%c", a).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("%c needs int or char"));
    let b = fx.b(b"a");
    let e = fmt_pct(&mut fx, b"%c", b).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("integer needed"));
    let e = fmt_pct(&mut fx, b"%", iv(1)).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let e = fmt_pct(&mut fx, b"%q", iv(1)).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let e = fmt_pct(&mut fx, b"100%%", iv(1)).unwrap_err();
    assert_err(
        e,
        StrExc::TypeError,
        Some("format string didn't convert all arguments"),
    );
    let t = fx.tuple(vec![]);
    let r = fmt_pct(&mut fx, b"100%%", t).unwrap();
    assert_str(&fx, r, b"100%");
}

#[test]
fn modulo_bytes_pattern() {
    let mut fx = Fx::new();
    let r = fmt_pct(&mut fx, b"%d", iv(42)).unwrap();
    assert_eq!(bytes_of(&fx, r), b"42");
    // A bytes pattern returns bytes.
    let pat = fx.b(b"%d");
    let types = fx.types;
    let r = str_modulo_format(&mut fx.ctx(), &types, pat, iv(42)).unwrap();
    assert_eq!(bytes_of(&fx, r), b"42");
    assert!(matches!(r, Value::Heap(_)));
}

// ── `str.format` ──────────────────────────────────────────────────────

#[test]
fn format_positional() {
    let mut fx = Fx::new();
    let a = fx.s(b"a");
    let f = float_of(&mut fx, 2.5);
    let r = fmt_str(&mut fx, b"{} {} {}", &[iv(1), a, f], &[]).unwrap();
    assert_str(&fx, r, b"1 a 2.5");
    let a = fx.s(b"a");
    let b = fx.s(b"b");
    let r = fmt_str(&mut fx, b"{0} {1} {0}", &[a, b], &[]).unwrap();
    assert_str(&fx, r, b"a b a");
    let a = fx.s(b"a");
    let e = fmt_str(&mut fx, b"{1}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::IndexError, Some("tuple index out of range"));
    let e = fmt_str(&mut fx, b"{}", &[], &[]).unwrap_err();
    assert_err(e, StrExc::IndexError, Some("tuple index out of range"));
    let a = fx.s(b"a");
    let e = fmt_str(&mut fx, b"{0} {1}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::IndexError, Some("tuple index out of range"));
}

#[test]
fn format_keyword() {
    let mut fx = Fx::new();
    // "name"/"value"/"key" are static qstrs ("age"/"x"/"a" are not).
    let name = fx.s(b"bob");
    let kwargs = vec![
        KwArg { name: qid(b"name"), value: name },
        KwArg { name: qid(b"value"), value: iv(3) },
    ];
    let r = fmt_str(&mut fx, b"{name} is {value}", &[], &kwargs).unwrap();
    assert_str(&fx, r, b"bob is 3");
    let e = fmt_str(&mut fx, b"{key}", &[], &[]).unwrap_err();
    assert!(matches!(e, StrError::KeyError(Value::Qstr(id)) if id == qid(b"key")));
    assert_err(e, StrExc::KeyError, None);
    let a = fx.s(b"a");
    let e = fmt_str(&mut fx, b"{0.x}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::NotImplementedError, Some("attributes not supported"));
    let a = fx.s(b"a");
    let e = fmt_str(&mut fx, b"{0[0]}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::NotImplementedError, Some("attributes not supported"));
}

#[test]
fn format_align_fill() {
    let mut fx = Fx::new();
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{:>10}|", &[a], &[]).unwrap();
    assert_str(&fx, r, b"        ab|");
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{:<10}|", &[a], &[]).unwrap();
    assert_str(&fx, r, b"ab        |");
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{:^10}|", &[a], &[]).unwrap();
    assert_str(&fx, r, b"    ab    |");
    let r = fmt_str(&mut fx, b"{:05d}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"00042");
    let r = fmt_str(&mut fx, b"{:05d}", &[iv(-42)], &[]).unwrap();
    assert_str(&fx, r, b"-0042");
    let r = fmt_str(&mut fx, b"{:+d}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"+42");
    let r = fmt_str(&mut fx, b"{: d}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b" 42");
    let r = fmt_str(&mut fx, b"{:=+8d}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"+     42");
    let r = fmt_str(&mut fx, b"{:=8d}", &[iv(-42)], &[]).unwrap();
    assert_str(&fx, r, b"-     42");
    let f = float_of(&mut fx, 3.14);
    let r = fmt_str(&mut fx, b"{:08.2f}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"00003.14");
    let r = fmt_str(&mut fx, b"{:>5}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"   42");
    let r = fmt_str(&mut fx, b"{:<5}|", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"42   |");
    let r = fmt_str(&mut fx, b"{:^5}|", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b" 42  |");
}

#[test]
fn format_types() {
    let mut fx = Fx::new();
    let t = [iv(255), iv(255), iv(8), iv(5)];
    let r = fmt_str(&mut fx, b"{:x} {:X} {:o} {:b}", &t, &[]).unwrap();
    assert_str(&fx, r, b"ff FF 10 101");
    let f = float_of(&mut fx, 12345.678);
    let r = fmt_str(&mut fx, b"{:e}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"1.234568e+04");
    let f = float_of(&mut fx, 0.00001);
    let r = fmt_str(&mut fx, b"{:g}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"1e-05");
    let r = fmt_str(&mut fx, b"{:c}", &[iv(65)], &[]).unwrap();
    assert_str(&fx, r, b"A");
    let f = float_of(&mut fx, 3.14159);
    let r = fmt_str(&mut fx, b"{:.2f}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"3.14");
    let r = fmt_str(&mut fx, b"{:n}", &[iv(1234567)], &[]).unwrap();
    assert_str(&fx, r, b"1234567");
    let r = fmt_str(&mut fx, b"{:,}", &[iv(1234567)], &[]).unwrap();
    assert_str(&fx, r, b"1,234,567");
    let r = fmt_str(&mut fx, b"{:_}", &[iv(1234567)], &[]).unwrap();
    assert_str(&fx, r, b"1_234_567");
    let t = [iv(255), iv(8)];
    let r = fmt_str(&mut fx, b"{:#x} {:#o}", &t, &[]).unwrap();
    assert_str(&fx, r, b"0xff 0o10");
    // Bool counts as integer here (`arg_looks_integer`).
    let r = fmt_str(&mut fx, b"{:d}", &[Value::Bool(true)], &[]).unwrap();
    assert_str(&fx, r, b"1");
    let e = fmt_str(&mut fx, b"{:s}", &[Value::Bool(true)], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let a = fx.s(b"ab");
    let e = fmt_str(&mut fx, b"{:d}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let f = float_of(&mut fx, 3.5);
    let e = fmt_str(&mut fx, b"{:d}", &[f], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    // An int falls through to the float branch for `e`.
    let r = fmt_str(&mut fx, b"{:e}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"4.200000e+01");
    // Precision is IGNORED for ints (hardcoded 0 in the int branch).
    let r = fmt_str(&mut fx, b"{:05.2d}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"00042");
    // `0` flag on a non-numeric arg: zero fill, left align.
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{:05}", &[a], &[]).unwrap();
    assert_str(&fx, r, b"ab000");
}

#[test]
fn format_conversion() {
    let mut fx = Fx::new();
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{!r}", &[a], &[]).unwrap();
    assert_str(&fx, r, b"'ab'");
    let r = fmt_str(&mut fx, b"{!s}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"42");
    let r = fmt_str(&mut fx, b"{{}}", &[], &[]).unwrap();
    assert_str(&fx, r, b"{}");
    let r = fmt_str(&mut fx, b"{{x}}", &[], &[]).unwrap();
    assert_str(&fx, r, b"{x}");
    let a = fx.s(b"ab");
    let e = fmt_str(&mut fx, b"{!x}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let e = fmt_str(&mut fx, b"{:z}", &[iv(42)], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{!r:>10}", &[a], &[]).unwrap();
    assert_str(&fx, r, b"      'ab'");
}

#[test]
fn format_spec_nested_and_pct() {
    let mut fx = Fx::new();
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{0:>{1}}", &[a, iv(6)], &[]).unwrap();
    assert_str(&fx, r, b"    ab");
    let f = float_of(&mut fx, 0.25);
    let r = fmt_str(&mut fx, b"{:%}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"25.000000%");
    let f = float_of(&mut fx, 0.256);
    let r = fmt_str(&mut fx, b"{:.1%}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"25.6%");
    let f = float_of(&mut fx, 3.9);
    let r = fmt_str(&mut fx, b"{:.0f}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"4");
    let f = float_of(&mut fx, 3.4);
    let r = fmt_str(&mut fx, b"{:.0f}", &[f], &[]).unwrap();
    assert_str(&fx, r, b"3");
    // Precision truncates BYTES (é is 2 bytes; 1 byte is 0xC3).
    let a = fx.s("é".as_bytes());
    let r = fmt_str(&mut fx, b"{:.1s}", &[a], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"\xc3");
    // `{:}` with no spec is the plain value.
    let r = fmt_str(&mut fx, b"{:}", &[iv(42)], &[]).unwrap();
    assert_str(&fx, r, b"42");
}

#[test]
fn format_mixing_errors() {
    let mut fx = Fx::new();
    let a = fx.s(b"a");
    let b = fx.s(b"b");
    let e = fmt_str(&mut fx, b"{0} {}", &[a, b], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    let a = fx.s(b"a");
    let b = fx.s(b"b");
    let e = fmt_str(&mut fx, b"{} {0}", &[a, b], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    // Sign flag with an explicit `s`/`c` type raises; with no type
    // it does not.
    let a = fx.s(b"ab");
    let r = fmt_str(&mut fx, b"{:+}", &[a], &[]).unwrap();
    assert_str(&fx, r, b"ab");
    let a = fx.s(b"ab");
    let e = fmt_str(&mut fx, b"{:+s}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    // `=` align on a non-numeric arg raises.
    let a = fx.s(b"ab");
    let e = fmt_str(&mut fx, b"{:=5}", &[a], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    // A lone `}` raises.
    let e = fmt_str(&mut fx, b"a}b", &[], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bad format string"));
    // A non-str value in the `s` branch raises.
    let l = fx.list(vec![iv(1)]);
    let e = fmt_str(&mut fx, b"{:>5}", &[l], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to str implicitly"));
}
