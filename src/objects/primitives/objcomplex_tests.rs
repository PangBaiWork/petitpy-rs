//! Lane C18a complex tests — the complex type slots (vendor
//! `objcomplex.c`), end to end through the VM plus slot-level
//! checks for hash / `NotHandled` arms.
//!
//! NOTE — the reference binary ABORTS (rc=134) on ANY
//! `complex(...)` call and on any complex arithmetic: `objcomplex.c`
//! is not compiled by `build.zig` and `full_stubs.zig` provides
//! `@panic` stubs. This lane implements complex properly from the
//! vendor C code (brief hard requirement 4); every expectation
//! below follows the vendor code, not the reference binary — see
//! `scripts/vendorc_float_result.txt`.

use super::test_fx::{out_f64, outcome, slot_ctx, Fx};
use crate::memory::gc::CallCtx;
use crate::objects::primitives::objcomplex::complex_binary_op;
use crate::objects::primitives::ops::PrimOutcome;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{Obj, Value};

#[test]
fn complex_make_new_forms() {
    let mut fx = Fx::new();
    fx.run_ok("a = complex()\nb = complex(1)\nc = complex(1, 2)\nd = complex(3.5)\ne = complex('1+2j')\nf = complex('3')\ng = complex('4j')\nh = complex('1.5-2.5j')\ni = complex(2 ** 70)\nj = complex(True)\n");
    assert_eq!(fx.gc("a"), (0.0, 0.0));
    assert_eq!(fx.gc("b"), (1.0, 0.0));
    assert_eq!(fx.gc("c"), (1.0, 2.0));
    assert_eq!(fx.gc("d"), (3.5, 0.0));
    assert_eq!(fx.gc("e"), (1.0, 2.0));
    assert_eq!(fx.gc("f"), (3.0, 0.0));
    assert_eq!(fx.gc("g"), (0.0, 4.0));
    assert_eq!(fx.gc("h"), (1.5, -2.5));
    assert_eq!(fx.gc("i"), (1.1805916207174113e21, 0.0));
    assert_eq!(fx.gc("j"), (1.0, 0.0));
    // A complex argument returns as-is.
    fx.run_ok("k = complex(complex(1, 2))\n");
    assert_eq!(fx.gc("k"), (1.0, 2.0));
}

#[test]
fn complex_make_new_two_arg_quirk() {
    // The vendor 2-arg rule is NOT CPython's `complex(re, im)`
    // when an argument is complex (`objcomplex.c:92-103`): arg1
    // complex → `real -= im1; imag += re1`.
    let mut fx = Fx::new();
    fx.run_ok("a = complex(complex(1, 2), complex(3, 4))\nb = complex(1, complex(3, 4))\nc = complex(complex(1, 2), 3)\nd = complex(1, 2)\n");
    assert_eq!(fx.gc("a"), (-3.0, 5.0));
    assert_eq!(fx.gc("b"), (-3.0, 3.0));
    assert_eq!(fx.gc("c"), (1.0, 5.0));
    assert_eq!(fx.gc("d"), (1.0, 2.0));
}

#[test]
fn complex_make_new_errors() {
    let mut fx = Fx::new();
    for src in ["x = complex('x')", "x = complex('1+')", "x = complex('1 + 2j')"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "ValueError", "{src}");
        assert_eq!(msg, "invalid syntax for number", "{src}");
    }
    for src in ["x = complex(None)", "x = complex([1])"] {
        let (name, msg) = fx.run_err(src);
        assert_eq!(name, "TypeError", "{src}");
        assert_eq!(msg, "can't convert to complex", "{src}");
    }
    let (name, msg) = fx.run_err("x = complex(1, 2, 3)");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "argument num/types mismatch");
}

// ── complex arithmetic ───────────────────────────────────────────────

#[test]
fn complex_arithmetic() {
    let mut fx = Fx::new();
    fx.run_ok("a = complex(1, 2) + complex(3, 4)\nb = complex(1, 2) - complex(3, 4)\nc = complex(1, 2) * complex(3, 4)\nd = complex(1, 2) / complex(3, 4)\ne = complex(1, 2) * 2\nf = 2 * complex(1, 2)\ng = complex(1, 2) + 1\nh = 1 + complex(1, 2)\ni = complex(1, 2) - 1\nj = complex(1, 2) / 2\n");
    assert_eq!(fx.gc("a"), (4.0, 6.0));
    assert_eq!(fx.gc("b"), (-2.0, -2.0));
    assert_eq!(fx.gc("c"), (-5.0, 10.0));
    // `(1+2j)/(3+4j)` is `(0.44, 0.08)` — the vendor's
    // multiply-by-conjugate path; the reference cannot compute it
    // (stub abort), so the last ulp has no oracle: epsilon.
    let (re, im) = fx.gc("d");
    assert!((re - 0.44).abs() < 1e-15, "(1+2j)/(3+4j) re = {re}");
    assert!((im - 0.08).abs() < 1e-15, "(1+2j)/(3+4j) im = {im}");
    assert_eq!(fx.gc("e"), (2.0, 4.0));
    assert_eq!(fx.gc("f"), (2.0, 4.0));
    assert_eq!(fx.gc("g"), (2.0, 2.0));
    assert_eq!(fx.gc("h"), (2.0, 2.0));
    assert_eq!(fx.gc("i"), (0.0, 2.0));
    assert_eq!(fx.gc("j"), (0.5, 1.0));
}

#[test]
fn complex_power() {
    let mut fx = Fx::new();
    fx.run_ok("a = complex(0, 0) ** complex(0, 0)\nb = complex(0, 0) ** complex(2, 0)\nc = complex(1, 0) ** complex(2, 0)\nd = complex(0, 1) ** complex(0, 1)\ne = complex(2, 0) ** complex(0.5, 0)\n");
    assert_eq!(fx.gc("a"), (1.0, 0.0));
    assert_eq!(fx.gc("b"), (0.0, 0.0));
    assert_eq!(fx.gc("c"), (1.0, 0.0));
    // `i ** i == exp(-pi/2)` — irrational, epsilon-compared.
    let (re, im) = fx.gc("d");
    assert!((re - 0.20787957635076193).abs() < 1e-15, "i**i re = {re}");
    assert!(im.abs() < 1e-15, "i**i im = {im}");
    // `sqrt(2)` via `exp(0.5*ln 2)` — epsilon, no oracle.
    let (re, im) = fx.gc("e");
    assert!((re - 1.4142135623730951).abs() < 1e-15, "2**0.5 re = {re}");
    assert!(im.abs() < 1e-15, "2**0.5 im = {im}");
    let (name, msg) = fx.run_err("x = complex(0, 0) ** complex(0, 1)");
    assert_eq!(name, "ZeroDivisionError");
    assert_eq!(msg, "0.0 to a complex power");
    let (name, msg) = fx.run_err("x = complex(0, 0) ** complex(-1, 0)");
    assert_eq!(name, "ZeroDivisionError");
    assert_eq!(msg, "0.0 to a complex power");
}

#[test]
fn complex_div_errors() {
    let mut fx = Fx::new();
    let (name, msg) = fx.run_err("x = complex(1, 2) / complex(0, 0)");
    assert_eq!(name, "ZeroDivisionError");
    assert_eq!(msg, "complex divide by zero");
    let (name, msg) = fx.run_err("x = complex(1, 2) // complex(3, 4)");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "can't truncate-divide a complex number");
}

#[test]
fn complex_unary_and_bool() {
    // `abs()` is not a VM builtin yet (D1) — the complex `ABS`
    // op is tested slot-level in `complex_abs_slot_level` below.
    let mut fx = Fx::new();
    fx.run_ok("b = -complex(1, 2)\nc = +complex(1, 2)\nd = bool(complex(0, 0))\ne = bool(complex(0, 1))\n");
    assert_eq!(fx.gc("b"), (-1.0, -2.0));
    assert_eq!(fx.gc("c"), (1.0, 2.0));
    assert!(!fx.gb("d"));
    assert!(fx.gb("e"));
}

#[test]
fn complex_abs_slot_level() {
    slot_ctx(|ctx, types| {
        use crate::objects::primitives::objcomplex::{complex_unary_op, new_complex};
        let z = new_complex(ctx, types, 3.0, 4.0);
        let r = complex_unary_op(ctx, types, UnaryOp::Abs, z);
        assert_eq!(out_f64(ctx.arena, outcome(r)), 5.0);
        let z = new_complex(ctx, types, 0.0, 0.0);
        let r = complex_unary_op(ctx, types, UnaryOp::Abs, z);
        assert_eq!(out_f64(ctx.arena, outcome(r)), 0.0);
        let z = new_complex(ctx, types, 1.0, 2.0);
        let r = complex_unary_op(ctx, types, UnaryOp::Negative, z);
        match outcome(r) {
            PrimOutcome::Value(Value::Heap(rr)) => match ctx.arena.get(rr) {
                Obj::Complex { re, im } => assert_eq!((*re, *im), (-1.0, -2.0)),
                other => panic!("neg not a complex: {other:?}"),
            },
            other => panic!("neg -> {other:?}"),
        }
    });
}

#[test]
fn complex_attr_real_imag() {
    let mut fx = Fx::new();
    fx.run_ok("z = complex(3, 4)\na = z.real\nb = z.imag\nc = complex(0, -2).real\nd = complex(0, -2).imag\n");
    assert_eq!(fx.gf("a"), 3.0);
    assert_eq!(fx.gf("b"), 4.0);
    assert_eq!(fx.gf("c"), 0.0);
    assert_eq!(fx.gf("d"), -2.0);
    let (name, msg) = fx.run_err("x = complex(1, 2).foo");
    assert_eq!(name, "AttributeError");
    assert_eq!(msg, "no attribute 'foo'");
}

#[test]
fn complex_print() {
    let mut fx = Fx::new();
    fx.run_ok("a = str(complex(1, 2))\nb = str(complex(0, 2))\nc = str(complex(1, -2))\nd = str(complex(0, 0))\ne = str(complex(1, 2))\nf = str(complex(-1, 0))\n");
    assert_eq!(fx.gs("a"), "(1+2j)");
    assert_eq!(fx.gs("b"), "2j");
    assert_eq!(fx.gs("c"), "(1-2j)");
    assert_eq!(fx.gs("d"), "0j");
    assert_eq!(fx.gs("e"), "(1+2j)");
    assert_eq!(fx.gs("f"), "(-1+0j)");
}

#[test]
fn complex_equality() {
    let mut fx = Fx::new();
    fx.run_ok("a = complex(1, 2) == complex(1, 2)\nb = complex(1, 2) != complex(1, 3)\nc = complex(1, 2) == 1\nd = complex(1, 0) == 1\ne = 1 == complex(1, 0)\nf = complex(1, 2) == 1.0\ng = complex(0, 0) == 0\n");
    for (name, want) in [
        ("a", true),
        ("b", true),
        ("c", false),
        ("d", true),
        ("e", true),
        ("f", false),
        ("g", true),
    ] {
        assert_eq!(fx.gb(name), want, "{name}");
    }
}

#[test]
fn complex_literal_fails_at_load() {
    // C17: the compiler writes FLOAT consts `"1j"` / `"4j"` and
    // the loader calls `parse_num_float(allow_imag=false)`, so
    // every complex literal is a load-time ValueError on the
    // reference — and here. The module fails to load at all.
    let mut fx = Fx::new();
    assert!(fx.run("x = 1 + 2j").is_err());
    assert!(fx.run("x = 3 + 4j").is_err());
    assert!(fx.run("x = 1j").is_err());
}

// ── slot-level: hash, NotHandled arms, print ────────────────────────

#[test]
fn complex_hash_values() {
    slot_ctx(|ctx, types| {
        use crate::objects::primitives::objcomplex::{complex_unary_op, new_complex};
        let h = |ctx: &mut CallCtx<'_>, re: f64, im: f64| {
            let z = new_complex(ctx, types, re, im);
            match outcome(complex_unary_op(ctx, types, UnaryOp::Hash, z)) {
                PrimOutcome::Value(Value::SmallInt(n)) => n,
                other => panic!("hash({re}+{im}j) -> {other:?}"),
            }
        };
        assert_eq!(h(ctx, 1.0, 2.0), 3);
        assert_eq!(h(ctx, 0.0, 0.0), 0);
        assert_eq!(h(ctx, 2.5, 0.5), 2);
        assert_eq!(h(ctx, 1.0, 1.0), 0);
    });
}

#[test]
fn complex_maybe_slots_not_handled() {
    slot_ctx(|ctx, types| {
        use crate::objects::primitives::objcomplex::{complex_unary_op, new_complex};
        let z = new_complex(ctx, types, 1.0, 2.0);
        for op in [
            UnaryOp::IntMaybe,
            UnaryOp::FloatMaybe,
            UnaryOp::ComplexMaybe,
            UnaryOp::Len,
        ] {
            assert!(
                matches!(
                    outcome(complex_unary_op(ctx, types, op, z)),
                    PrimOutcome::NotHandled
                ),
                "{op:?} should be NotHandled"
            );
        }
        // `complex < complex` / `complex % complex` are the
        // vendor's `default: NULL`.
        for op in [BinaryOp::Less, BinaryOp::Modulo, BinaryOp::NotEqual] {
            assert!(
                matches!(
                    outcome(complex_binary_op(ctx, types, op, 1.0, 2.0, z)),
                    PrimOutcome::NotHandled
                ),
                "{op:?} should be NotHandled"
            );
        }
    });
}
