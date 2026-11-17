//! Lane C18a float tests — the float type slots (vendor
//! `objfloat.c`), the int-slot delegation, and the C14 case list,
//! end to end through the VM plus slot-level checks for hash /
//! print / `NotHandled` arms.
//!
//! Every numeric expectation was probed on the reference binary
//! 2026-11-18 (see `scripts/vendorc_float_result.txt`); the
//! `// DIVERGENCE` notes mark the cases where the reference
//! ABORTS (rc=134, the `@panic` complex stub) and Rust returns a
//! value or raises catchably instead.

use super::test_fx::{out_f64, out_f64_pair, outcome, slot_ctx, Fx};
use crate::memory::gc::CallCtx;
use crate::objects::primitives::objfloat::{float_binary_op, float_hash, float_unary_op};
use crate::objects::primitives::ops::PrimOutcome;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::Value;

#[test]
fn c14_list_arithmetic() {
    let mut fx = Fx::new();
    fx.run_ok("a = 1 + 2.5\nb = 2.5 + 1\nc = 1.5 + 2.5\nd = 2.5 - 5.0\ne = 2.0 * 3.0\nf = 5.0 / 2.0\ng = 5 / 2.0\nh = 2.5 + 2\n");
    assert_eq!(fx.gf("a"), 3.5);
    assert_eq!(fx.gf("b"), 3.5);
    assert_eq!(fx.gf("c"), 4.0);
    assert_eq!(fx.gf("d"), -2.5);
    assert_eq!(fx.gf("e"), 6.0);
    assert_eq!(fx.gf("f"), 2.5);
    assert_eq!(fx.gf("g"), 2.5);
    assert_eq!(fx.gf("h"), 4.5);
}

#[test]
fn c14_list_zero_division() {
    let mut fx = Fx::new();
    for src in ["x = 1 / 0.0", "x = 1.0 / 0", "x = 0.0 ** -1", "x = 0.0 ** -2.0"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "ZeroDivisionError", "{src}");
        assert_eq!(msg, "divide by zero", "{src}");
    }
}

#[test]
fn c14_list_negative_base_power() {
    let mut fx = Fx::new();
    for src in ["x = (-8.0) ** 0.5", "x = (-8) ** 0.5"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "ValueError", "{src}");
        assert_eq!(msg, "complex values not supported", "{src}");
    }
    // DIVERGENCE: `-(2**70)` is a heap int, so the Zig's
    // `floatOpPrecheck` never runs for it — the vendor op
    // delegates to the `@panic` complex stub and the reference
    // ABORTS (rc=134, probed). Rust raises the same ValueError
    // the small-int path raises (uniform precheck semantics).
    let (name, msg) = fx.run_err("x = (-(2**70)) ** 0.5");
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "complex values not supported");
}

#[test]
fn c14_list_power_values() {
    let mut fx = Fx::new();
    fx.run_ok("a = 2.0 ** -1\nb = 2 ** 2.5\nc = 0.0 ** 0.0\nd = (-8.0) ** 2\ne = 2.0 ** -2.0\nf = 0.5 ** -1.0\ng = 2.5 ** 2.5\nh = 0.5 ** 0.5\n");
    assert_eq!(fx.gf("a"), 0.5);
    assert_eq!(fx.gf("b"), 5.656854249492381);
    assert_eq!(fx.gf("c"), 1.0);
    assert_eq!(fx.gf("d"), 64.0);
    assert_eq!(fx.gf("e"), 0.25);
    assert_eq!(fx.gf("f"), 2.0);
    assert_eq!(fx.gf("g"), 9.882117688026186);
    assert_eq!(fx.gf("h"), 0.7071067811865476);
}

#[test]
fn c14_list_inf_nan_power() {
    let mut fx = Fx::new();
    let (name, msg) = fx.run_err("x = 0.0 ** float('-inf')");
    assert_eq!(name, "ZeroDivisionError");
    assert_eq!(msg, "divide by zero");
    let (name, msg) = fx.run_err("x = (-8.0) ** float('nan')");
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "complex values not supported");
    fx.run_ok("a = 1.0 ** float('nan')\nb = float('nan') ** 0.0\nc = 2.0 ** float('nan')\nd = float('nan') ** 2.0\ne = (-8.0) ** float('inf')\nf = (-8) ** float('inf')\ng = 0.0 ** float('nan')\n");
    assert_eq!(fx.gf("a"), 1.0);
    assert_eq!(fx.gf("b"), 1.0);
    assert_eq!(fx.gf("e"), f64::INFINITY);
    assert_eq!(fx.gf("f"), f64::INFINITY);
    assert!(fx.gf("c").is_nan());
    assert!(fx.gf("d").is_nan());
    assert!(fx.gf("g").is_nan());
}

// ── float arithmetic ─────────────────────────────────────────────────

#[test]
fn float_floor_div_modulo() {
    let mut fx = Fx::new();
    fx.run_ok("a = 5.5 // 2.0\nb = -5.5 // 2.0\nc = 5.5 // -2.0\nd = -5.5 // -2.0\ne = 5.5 % 2.0\nf = -5.5 % 2.0\ng = 5.5 % -2.0\nh = -5.5 % -2.0\ni = 2.5 % 0.5\nj = 10.0 % 3.0\nk = -10.0 % 3.0\nl = 10.0 % -3.0\nm = -10.0 % -3.0\nr = str(2.5 % -0.5)\ns = str(-2.5 % -0.5)\nt = str(1.0 // 2)\n");
    assert_eq!(fx.gf("a"), 2.0);
    assert_eq!(fx.gf("b"), -3.0);
    assert_eq!(fx.gf("c"), -3.0);
    assert_eq!(fx.gf("d"), 2.0);
    assert_eq!(fx.gf("e"), 1.5);
    assert_eq!(fx.gf("f"), 0.5);
    assert_eq!(fx.gf("g"), -0.5);
    assert_eq!(fx.gf("h"), -1.5);
    assert_eq!(fx.gf("i"), 0.0);
    assert_eq!(fx.gf("j"), 1.0);
    assert_eq!(fx.gf("k"), 2.0);
    assert_eq!(fx.gf("l"), -2.0);
    assert_eq!(fx.gf("m"), -1.0);
    // The zero result carries the divisor's sign (`copysign`):
    // `2.5 % -0.5` is `-0.0`, not `0.0` — only `repr` shows it.
    assert_eq!(fx.gs("r"), "-0.0");
    assert_eq!(fx.gs("s"), "-0.0");
    assert_eq!(fx.gs("t"), "0.0");
}

#[test]
fn float_divmod_tuple() {
    // `divmod()` is not a VM builtin yet (D1) — the `DIVMOD`
    // op is tested slot-level here; the zero-divisor raise is
    // also checked end to end via `//` / `%`.
    slot_ctx(|ctx, types| {
        let rhs = crate::objects::primitives::objfloat::new_float(ctx, types, 2.0);
        let r = float_binary_op(ctx, types, BinaryOp::Divmod, 5.5, rhs);
        assert_eq!(out_f64_pair(ctx.arena, outcome(r)), (2.0, 1.5));
        let rhs = crate::objects::primitives::objfloat::new_float(ctx, types, 2.0);
        let r = float_binary_op(ctx, types, BinaryOp::Divmod, -5.5, rhs);
        assert_eq!(out_f64_pair(ctx.arena, outcome(r)), (-3.0, 0.5));
        let rhs = crate::objects::primitives::objfloat::new_float(ctx, types, -2.0);
        let r = float_binary_op(ctx, types, BinaryOp::Divmod, 5.5, rhs);
        assert_eq!(out_f64_pair(ctx.arena, outcome(r)), (-3.0, -0.5));
        let rhs = crate::objects::primitives::objfloat::new_float(ctx, types, 0.0);
        let e = float_binary_op(ctx, types, BinaryOp::Divmod, 5.5, rhs);
        assert_eq!(e, Err(crate::objects::primitives::ops::PrimError::DivideByZero));
    });
    let mut fx = Fx::new();
    for src in ["x = 5.5 // 0.0", "x = 5.5 % 0.0"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "ZeroDivisionError", "{src}");
        assert_eq!(msg, "divide by zero", "{src}");
    }
}

#[test]
fn float_inplace_ops() {
    let mut fx = Fx::new();
    fx.run_ok("x = 1.5\nx += 2.5\na = x\nx -= 1.0\nb = x\nx *= 2.0\nc = x\nx /= 2.0\nd = x\nx **= 2.0\ne = x\n");
    assert_eq!(fx.gf("a"), 4.0);
    assert_eq!(fx.gf("b"), 3.0);
    assert_eq!(fx.gf("c"), 6.0);
    assert_eq!(fx.gf("d"), 3.0);
    assert_eq!(fx.gf("e"), 9.0);
}

#[test]
fn float_overflow_to_inf() {
    let mut fx = Fx::new();
    fx.run_ok("a = 1e308 * 10.0\nb = -1e308 * 10.0\nc = 1e308 + 1e308\nd = (2 ** 70) / 2.0\ne = 2 ** 70 + 2.5\nf = 2.5 + 2 ** 70\n");
    assert_eq!(fx.gf("a"), f64::INFINITY);
    assert_eq!(fx.gf("b"), f64::NEG_INFINITY);
    assert_eq!(fx.gf("c"), f64::INFINITY);
    assert_eq!(fx.gf("d"), 5.902958103587057e20);
    assert_eq!(fx.gf("e"), 1.1805916207174113e21);
    assert_eq!(fx.gf("f"), 1.1805916207174113e21);
}

#[test]
fn float_comparisons_and_equality() {
    let mut fx = Fx::new();
    fx.run_ok("a = 1 < 2.5\nb = 2.5 > 2\nc = 1.5 < 2\nd = 2 < 1.5\ne = 1 == 1.0\nf = 1.0 == 1\ng = 2 ** 70 == 2.0 ** 70\nh = 2.5 == 2.5\ni = 2.5 != 2.0\nj = 2.5 <= 2.5\nk = 2.5 >= 3.0\n");
    for (name, want) in [
        ("a", true),
        ("b", true),
        ("c", true),
        ("d", false),
        ("e", true),
        ("f", true),
        ("g", true),
        ("h", true),
        ("i", true),
        ("j", true),
        ("k", false),
    ] {
        assert_eq!(fx.gb(name), want, "{name}");
    }
}

#[test]
fn float_bool_abs_neg() {
    // `abs()` is not a VM builtin yet (D1) — the `ABS` unary op
    // is tested slot-level in `float_abs_slot_level` below.
    let mut fx = Fx::new();
    fx.run_ok("a = bool(0.0)\nb = bool(2.5)\nd = -2.5\ne = +2.5\ng = str(-0.0)\n");
    assert!(!fx.gb("a"));
    assert!(fx.gb("b"));
    assert_eq!(fx.gf("d"), -2.5);
    assert_eq!(fx.gf("e"), 2.5);
    assert_eq!(fx.gs("g"), "-0.0");
}

#[test]
fn float_abs_slot_level() {
    slot_ctx(|ctx, types| {
        let v = crate::objects::primitives::objfloat::new_float(ctx, types, -2.5);
        let r = float_unary_op(ctx, types, UnaryOp::Abs, v);
        assert_eq!(out_f64(ctx.arena, outcome(r)), 2.5);
        let v = crate::objects::primitives::objfloat::new_float(ctx, types, 2.5);
        let r = float_unary_op(ctx, types, UnaryOp::Abs, v);
        assert_eq!(out_f64(ctx.arena, outcome(r)), 2.5);
        // `abs(-0.0)` is `0.0` (vendor: `signbit` → new float).
        let v = crate::objects::primitives::objfloat::new_float(ctx, types, -0.0);
        let r = float_unary_op(ctx, types, UnaryOp::Abs, v);
        let a = out_f64(ctx.arena, outcome(r));
        assert_eq!(a, 0.0);
        assert!(!a.is_sign_negative());
        // `NEGATIVE` / `POSITIVE` arms.
        let v = crate::objects::primitives::objfloat::new_float(ctx, types, 2.5);
        let r = float_unary_op(ctx, types, UnaryOp::Negative, v);
        assert_eq!(out_f64(ctx.arena, outcome(r)), -2.5);
        let v = crate::objects::primitives::objfloat::new_float(ctx, types, 2.5);
        let r = float_unary_op(ctx, types, UnaryOp::Positive, v);
        assert_eq!(out_f64(ctx.arena, outcome(r)), 2.5);
    });
}

#[test]
fn float_raises_are_catchable() {
    // The Zig `floatOpPrecheck` exists so these raises are
    // catchable (vendor C would longjmp past `try/except`).
    let mut fx = Fx::new();
    fx.run_ok("try:\n    x = 1 / 0.0\nexcept ZeroDivisionError:\n    a = 'caught'\ntry:\n    y = (-8.0) ** 0.5\nexcept ValueError:\n    b = 'caught-ve'\n");
    assert_eq!(fx.gs("a"), "caught");
    assert_eq!(fx.gs("b"), "caught-ve");
}

#[test]
fn float_unsupported_ops() {
    let mut fx = Fx::new();
    let (name, msg) = fx.run_err("x = 2.5 * 'ab'");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "unsupported types for __mul__: 'float', 'str'");
    let (name, _) = fx.run_err("x = 2.5 << 1");
    assert_eq!(name, "TypeError");
}

// ── float() ────────────────────────────────────────────────────────

#[test]
fn float_make_new_forms() {
    let mut fx = Fx::new();
    fx.run_ok("a = float()\nb = float(1)\nc = float('3.5')\nd = float(b'3.5')\ne = float(2 ** 70)\nf = float(True)\ng = float(False)\nh = float(' 3.5 ')\ni = float('inf')\nj = float('1_0')\nk = float('1e5')\nl = float(3.7)\nm = float(-0.5)\nn = float('nan')\no = float(2.5)\n");
    assert_eq!(fx.gf("a"), 0.0);
    assert_eq!(fx.gf("b"), 1.0);
    assert_eq!(fx.gf("c"), 3.5);
    assert_eq!(fx.gf("d"), 3.5);
    assert_eq!(fx.gf("e"), 1.1805916207174113e21);
    assert_eq!(fx.gf("f"), 1.0);
    assert_eq!(fx.gf("g"), 0.0);
    assert_eq!(fx.gf("h"), 3.5);
    assert_eq!(fx.gf("i"), f64::INFINITY);
    assert_eq!(fx.gf("j"), 10.0);
    assert_eq!(fx.gf("k"), 100000.0);
    assert_eq!(fx.gf("l"), 3.7);
    assert_eq!(fx.gf("m"), -0.5);
    assert!(fx.gf("n").is_nan());
    // An exact float returns as-is.
    assert_eq!(fx.gf("o"), 2.5);
}

#[test]
fn float_make_new_errors() {
    let mut fx = Fx::new();
    for src in ["x = float('')", "x = float('abc')", "x = float('0x10')"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "ValueError", "{src}");
        assert_eq!(msg, "invalid syntax for number", "{src}");
    }
    for src in ["x = float(None)", "x = float([1])"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "TypeError", "{src}");
        assert_eq!(msg, "can't convert to float", "{src}");
    }
    for src in ["x = float(1, 2)", "x = float(x=1)"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "TypeError", "{src}");
        assert_eq!(msg, "argument num/types mismatch", "{src}");
    }
}

#[test]
fn int_and_complex_kwargs_raise_mismatch() {
    // Same flat-layout bug class as `float(x=1)` (fixed in
    // `int_make_new` / `complex_make_new` this lane): the branch
    // must run on the positionals, not parse the kwarg key as
    // the value. Before the fix, `int(x=1)` raised ValueError
    // "invalid syntax for integer" (it parsed `"x"`).
    let mut fx = Fx::new();
    for src in ["x = int(x=1)", "x = complex(x=1)"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "TypeError", "{src}");
        assert_eq!(msg, "argument num/types mismatch", "{src}");
    }
}

#[test]
fn int_from_float() {
    let mut fx = Fx::new();
    fx.run_ok("a = int(3.7)\nb = int(2.5)\n");
    assert_eq!(fx.gi("a"), 3);
    assert_eq!(fx.gi("b"), 2);
    let (name, msg) = fx.run_err("x = int(float('inf'))");
    assert_eq!(name, "OverflowError");
    assert_eq!(msg, "can't convert inf to int");
    let (name, msg) = fx.run_err("x = int(float('nan'))");
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "can't convert NaN to int");
}

// ── str formatting with floats ──────────────────────────────────────

#[test]
fn float_percent_format() {
    let mut fx = Fx::new();
    // `%d` truncates via `int_from_float`; `%o` / `%x` go through
    // the `IntMaybe` slot, which float does NOT answer — the C8b
    // hard requirement.
    fx.run_ok("a = '%d' % 3.0\nb = '%d' % -3.7\n");
    assert_eq!(fx.gs("a"), "3");
    assert_eq!(fx.gs("b"), "-3");
    for src in ["x = '%o' % 3.0", "x = '%x' % 3.0"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "TypeError", "{src}");
        assert_eq!(msg, "can't convert to int", "{src}");
    }
}

#[test]
fn float_str_str() {
    let mut fx = Fx::new();
    fx.run_ok("a = str(0.1)\nb = str(1e16)\nc = str(1e-5)\nd = str(123456789.0)\ne = str(-0.0)\nf = str(float('inf'))\ng = str(float('-inf'))\nh = str(float('nan'))\ni = str(2.5)\nj = str(2.5)\nk = str([1, 2.5])\nl = str(2 ** 0.5)\n");
    assert_eq!(fx.gs("a"), "0.1");
    assert_eq!(fx.gs("b"), "1e+16");
    assert_eq!(fx.gs("c"), "1e-05");
    assert_eq!(fx.gs("d"), "123456789.0");
    assert_eq!(fx.gs("e"), "-0.0");
    assert_eq!(fx.gs("f"), "inf");
    assert_eq!(fx.gs("g"), "-inf");
    assert_eq!(fx.gs("h"), "nan");
    assert_eq!(fx.gs("i"), "2.5");
    assert_eq!(fx.gs("j"), "2.5");
    assert_eq!(fx.gs("k"), "[1, 2.5]");
    assert_eq!(fx.gs("l"), "1.4142135623730951");
}

// ── int/float interop (delegation regressions) ──────────────────────

#[test]
fn int_float_interop() {
    let mut fx = Fx::new();
    // `int op float` used to be `binopUnsupported` — the generic
    // loop now reaches the int slot's delegation.
    fx.run_ok("a = 2 * 'ab'\nb = 'ab' * 2\nc = True + 2.5\nd = 2.5 + True\ne = (2 ** 70) ** 0.5\nf = (2 ** 70) ** -1\ng = (-(2 ** 70)) ** 2\nh = (-(2 ** 70)) ** -1\ni = str(2 * [1])\nj = str(2 * (1,))\nk = str([1] * 2)\nr = str(g)\ns = str(h)\n");
    assert_eq!(fx.gs("a"), "abab");
    assert_eq!(fx.gs("b"), "abab");
    assert_eq!(fx.gs("i"), "[1, 1]");
    assert_eq!(fx.gs("j"), "(1, 1)");
    assert_eq!(fx.gs("k"), "[1, 1]");
    assert_eq!(fx.gf("c"), 3.5);
    assert_eq!(fx.gf("d"), 3.5);
    assert_eq!(fx.gf("e"), 34359738368.0);
    assert_eq!(fx.gf("f"), 8.470329472543003e-22);
    // `(-(2**70)) ** 2` is an exact big int — read it via `repr`.
    assert_eq!(fx.gs("r"), "1393796574908163946345982392040522594123776");
    assert_eq!(fx.gs("s"), "-8.470329472543003e-22");
}

// ── complex() ────────────────────────────────────────────────────────

#[test]
fn float_hash_values() {
    // `mp_float_hash` is the simple `(mp_int_t)val` (C truncation);
    // the slot wraps once with `wrap_small_int`.
    slot_ctx(|ctx, types| {
        let h = |ctx: &mut CallCtx<'_>, f: f64| {
            let v = crate::objects::primitives::objfloat::new_float(ctx, types, f);
            match outcome(float_unary_op(ctx, types, UnaryOp::Hash, v)) {
                PrimOutcome::Value(Value::SmallInt(n)) => n,
                other => panic!("hash({f}) -> {other:?}"),
            }
        };
        assert_eq!(h(ctx, 2.5), 2);
        assert_eq!(h(ctx, 0.5), 0);
        assert_eq!(h(ctx, 0.25), 0);
        assert_eq!(h(ctx, 1.5), 1);
        assert_eq!(h(ctx, -2.5), -2);
        assert_eq!(h(ctx, 1.0), 1);
        assert_eq!(h(ctx, 0.0), 0);
        assert_eq!(h(ctx, -0.0), 0);
        assert_eq!(h(ctx, f64::INFINITY), 0);
        assert_eq!(h(ctx, f64::NAN), 0);
        assert_eq!(h(ctx, 2f64.powi(62)), -4611686018427387904);
        assert_eq!(h(ctx, 1e30), 0);
        assert_eq!(h(ctx, -1e30), 0);
        assert_eq!(h(ctx, 123456789.987), 123456789);
        assert_eq!(h(ctx, 0.5), 0);
    });
}

#[test]
fn float_hash_raw_c_truncation() {
    // The raw `float_hash` (before `wrap_small_int`): C
    // `cvttsd2si` yields `INT64_MIN` for NaN / inf / `|f| >= 2^63`
    // — Rust's saturating `as` would give `i64::MAX` for large
    // positives, so the range check is load-bearing.
    assert_eq!(float_hash(2.5), 2);
    assert_eq!(float_hash(-2.5), -2);
    assert_eq!(float_hash(f64::INFINITY), i64::MIN);
    assert_eq!(float_hash(f64::NEG_INFINITY), i64::MIN);
    assert_eq!(float_hash(f64::NAN), i64::MIN);
    assert_eq!(float_hash(1e30), i64::MIN);
    assert_eq!(float_hash(2f64.powi(62)), 4611686018427387904);
    assert_eq!(float_hash(-2f64.powi(62)), -4611686018427387904);
}

#[test]
fn float_maybe_slots_not_handled() {
    // The float type has no `IntMaybe` case (the C8b hard
    // requirement — `"%o" % 3.0` raises) and no `FloatMaybe` /
    // `ComplexMaybe` cases (answered before the slot).
    slot_ctx(|ctx, types| {
        let f = crate::objects::primitives::objfloat::new_float(ctx, types, 3.0);
        for op in [
            UnaryOp::IntMaybe,
            UnaryOp::FloatMaybe,
            UnaryOp::ComplexMaybe,
            UnaryOp::Len,
        ] {
            assert!(
                matches!(
                    outcome(float_unary_op(ctx, types, op, f)),
                    PrimOutcome::NotHandled
                ),
                "{op:?} should be NotHandled"
            );
        }
        // ... and the binary op has no reverse / bit-op / NotEqual
        // cases (the vendor's `default: NULL`).
        let s = crate::objects::primitives::objfloat::new_float(ctx, types, 1.0);
        for op in [
            BinaryOp::Lshift,
            BinaryOp::NotEqual,
            BinaryOp::ReverseAdd,
            BinaryOp::Modulo,
        ] {
            if op == BinaryOp::Modulo {
                // `Modulo` with a str rhs: `get_float_maybe`
                // fails first, so it is `NotHandled` too.
                let q = Value::Qstr(0);
                assert!(
                    matches!(
                        outcome(float_binary_op(ctx, types, op, 2.5, q)),
                        PrimOutcome::NotHandled
                    ),
                    "float % str should be NotHandled"
                );
                continue;
            }
            assert!(
                matches!(
                    outcome(float_binary_op(ctx, types, op, 2.5, s)),
                    PrimOutcome::NotHandled
                ),
                "{op:?} should be NotHandled"
            );
        }
    });
}

