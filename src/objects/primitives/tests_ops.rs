//! Lane C7 tests (part 2) — unary / binary / pow3 / from_bytes /
//! to_bytes / bool / cell / type ROM / formatting. Fixture and
//! helpers live in `tests.rs` (same module).

use super::tests::*;
use super::*;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOpFn, PrintFn, UnaryOpFn};
use crate::objects::value::{Obj, Value};
use crate::pyerr::PyError;
use crate::vendor::mpz::BigInt;

// ── unary ───────────────────────────────────────────────────────────────

fn unary(fx: &mut Fixture, op: UnaryOp, v: Value) -> PrimResult {
    fx.call(|ctx, t| int_unary_op(ctx, t, op, v))
}

fn unary_val(fx: &mut Fixture, op: UnaryOp, v: Value) -> Value {
    match unary(fx, op, v).unwrap() {
        PrimOutcome::Value(v) => v,
        PrimOutcome::NotHandled => panic!("op {op:?} unexpectedly NotHandled"),
    }
}

#[test]
fn unary_ops_small() {
    let mut fx = Fixture::new();
    assert_eq!(as_i64(unary_val(&mut fx, UnaryOp::Negative, Value::SmallInt(5)), &fx.arena), Some(-5));
    assert_eq!(as_i64(unary_val(&mut fx, UnaryOp::Invert, Value::SmallInt(5)), &fx.arena), Some(-6));
    assert_eq!(as_i64(unary_val(&mut fx, UnaryOp::Abs, Value::SmallInt(-5)), &fx.arena), Some(5));
    assert_eq!(as_i64(unary_val(&mut fx, UnaryOp::Positive, Value::SmallInt(-5)), &fx.arena), Some(-5));
    assert_eq!(as_i64(unary_val(&mut fx, UnaryOp::Hash, Value::SmallInt(7)), &fx.arena), Some(7));
    assert_eq!(as_bool(unary_val(&mut fx, UnaryOp::Bool, Value::SmallInt(0))), Some(false));
    assert_eq!(as_bool(unary_val(&mut fx, UnaryOp::Bool, Value::SmallInt(3))), Some(true));
    assert_eq!(unary(&mut fx, UnaryOp::Not, Value::SmallInt(1)).unwrap(), PrimOutcome::NotHandled);
    assert_eq!(unary(&mut fx, UnaryOp::Len, Value::SmallInt(1)).unwrap(), PrimOutcome::NotHandled);
    assert_eq!(unary(&mut fx, UnaryOp::FloatMaybe, Value::SmallInt(1)).unwrap(), PrimOutcome::NotHandled);
}

#[test]
fn unary_ops_big_window_edges() {
    let mut fx = Fixture::new();
    let min = Value::SmallInt(crate::objects::value::SMALL_INT_MIN);
    let v = unary_val(&mut fx, UnaryOp::Negative, min);
    assert_eq!(as_big(v, &fx.arena).unwrap(), pow2(62));
    let v = unary_val(&mut fx, UnaryOp::Abs, min);
    assert_eq!(as_big(v, &fx.arena).unwrap(), pow2(62));
    let big = fx.heap_int(pow2(70));
    let v = unary_val(&mut fx, UnaryOp::Negative, big);
    assert_eq!(as_big(v, &fx.arena).unwrap(), -pow2(70));
    let big = fx.heap_int(pow2(70));
    let v = unary_val(&mut fx, UnaryOp::Hash, big);
    assert_eq!(as_i64(v, &fx.arena), Some(0));
    let big = fx.heap_int(pow2(70));
    let v = unary_val(&mut fx, UnaryOp::Bool, big);
    assert_eq!(as_bool(v), Some(true));
    let big = fx.heap_int(BigInt::zero());
    let v = unary_val(&mut fx, UnaryOp::Bool, big);
    assert_eq!(as_bool(v), Some(false));
    let v = unary_val(&mut fx, UnaryOp::IntMaybe, Value::SmallInt(9));
    assert_eq!(v, Value::SmallInt(9));
}

/// C9b follow-up: the vendor tag-wraps the int hash with no
/// window check (`objint_mpz.c:147-148`), so `hash(2**62)` is
/// `-(2**62)` — a small int — not a heap int `2**62` (probed
/// on the reference binary 2026-11-17).
#[test]
fn int_hash_tag_wrap() {
    let mut fx = Fixture::new();
    let big = fx.heap_int(pow2(62));
    let v = unary_val(&mut fx, UnaryOp::Hash, big);
    assert_eq!(v, Value::SmallInt(-(1i64 << 62)));
    let big = fx.heap_int(pow2(62) - BigInt::from_int(1));
    let v = unary_val(&mut fx, UnaryOp::Hash, big);
    assert_eq!(v, Value::SmallInt((1i64 << 62) - 1));
    let big = fx.heap_int(-pow2(62));
    let v = unary_val(&mut fx, UnaryOp::Hash, big);
    assert_eq!(v, Value::SmallInt(-(1i64 << 62)));
}

// ── binary ──────────────────────────────────────────────────────────────

fn binary(fx: &mut Fixture, op: BinaryOp, lhs: Value, rhs: Value) -> PrimResult {
    fx.call(|ctx, t| int_binary_op(ctx, t, op, lhs, rhs))
}

fn binary_val(fx: &mut Fixture, op: BinaryOp, lhs: Value, rhs: Value) -> Value {
    match binary(fx, op, lhs, rhs).unwrap() {
        PrimOutcome::Value(v) => v,
        PrimOutcome::NotHandled => panic!("op {op:?} unexpectedly NotHandled"),
    }
}

#[test]
fn binary_arithmetic_sign_rules() {
    let mut fx = Fixture::new();
    let n = Value::SmallInt;
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Add, n(2), n(3)), &fx.arena), Some(5));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::FloorDivide, n(-7), n(2)), &fx.arena), Some(-4));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Modulo, n(-7), n(2)), &fx.arena), Some(1));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Modulo, n(7), n(-2)), &fx.arena), Some(-1));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Multiply, n(6), n(7)), &fx.arena), Some(42));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::And, n(5), n(3)), &fx.arena), Some(1));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Or, n(5), n(3)), &fx.arena), Some(7));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Xor, n(5), n(3)), &fx.arena), Some(6));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Lshift, n(1), n(70)), &fx.arena), None);
    assert_eq!(
        as_big(binary_val(&mut fx, BinaryOp::Lshift, n(1), n(70)), &fx.arena).unwrap(),
        pow2(70)
    );
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Rshift, n(1), n(100)), &fx.arena), Some(0));
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Rshift, n(-1), n(1)), &fx.arena), Some(-1));
    assert_eq!(as_f64(binary_val(&mut fx, BinaryOp::TrueDivide, n(7), n(2)), &fx.arena), Some(3.5));
    assert_eq!(as_f64(binary_val(&mut fx, BinaryOp::Power, n(2), n(-1)), &fx.arena), Some(0.5));
    let t = binary_val(&mut fx, BinaryOp::Divmod, n(-7), n(2));
    let items = match t {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Tuple(items) => items.clone(),
            _ => panic!("not a tuple"),
        },
        _ => panic!("not heap"),
    };
    assert_eq!(as_i64(items[0], &fx.arena), Some(-4));
    assert_eq!(as_i64(items[1], &fx.arena), Some(1));
}

fn bool_binary_val(fx: &mut Fixture, op: BinaryOp, lhs: Value, rhs: Value) -> Value {
    match fx.call(|ctx, t| bool_binary_op(ctx, t, op, lhs, rhs)).unwrap() {
        PrimOutcome::Value(v) => v,
        PrimOutcome::NotHandled => panic!("op {op:?} unexpectedly NotHandled"),
    }
}

#[test]
fn binary_comparisons_and_bool_rhs() {
    let mut fx = Fixture::new();
    let n = Value::SmallInt;
    assert_eq!(as_bool(binary_val(&mut fx, BinaryOp::Less, n(2), n(3))), Some(true));
    assert_eq!(as_bool(binary_val(&mut fx, BinaryOp::More, n(2), n(3))), Some(false));
    assert_eq!(as_bool(binary_val(&mut fx, BinaryOp::Equal, n(2), n(2))), Some(true));
    assert_eq!(as_bool(binary_val(&mut fx, BinaryOp::LessEqual, n(3), n(3))), Some(true));
    assert_eq!(as_bool(binary_val(&mut fx, BinaryOp::MoreEqual, n(2), n(3))), Some(false));
    assert_eq!(binary(&mut fx, BinaryOp::NotEqual, n(2), n(3)).unwrap(), PrimOutcome::NotHandled);
    // Bool lhs goes through the bool slot (it converts to small int
    // 0/1 first — the int slot never sees a bool lhs).
    assert_eq!(as_i64(bool_binary_val(&mut fx, BinaryOp::Add, Value::Bool(true), Value::Bool(true)), &fx.arena), Some(2));
    assert_eq!(as_i64(bool_binary_val(&mut fx, BinaryOp::Multiply, Value::Bool(true), n(5)), &fx.arena), Some(5));
    assert_eq!(as_i64(bool_binary_val(&mut fx, BinaryOp::Add, Value::Bool(false), n(1)), &fx.arena), Some(1));
    // Bool rhs reaches the int slot's extra_cases directly.
    assert_eq!(as_i64(binary_val(&mut fx, BinaryOp::Add, n(1), Value::Bool(true)), &fx.arena), Some(2));
}

#[test]
fn binary_errors() {
    let mut fx = Fixture::new();
    let n = Value::SmallInt;
    for op in [BinaryOp::TrueDivide, BinaryOp::FloorDivide, BinaryOp::Modulo, BinaryOp::Divmod] {
        let e = binary(&mut fx, op, n(1), n(0)).unwrap_err();
        assert_eq!(err_kind(e), (Some(ExcType::ZeroDivisionError), Some("divide by zero")));
    }
    let e = binary(&mut fx, BinaryOp::Power, n(0), n(-1)).unwrap_err();
    assert_eq!(e, PrimError::DivideByZero);
    let e = binary(&mut fx, BinaryOp::Lshift, n(1), n(-1)).unwrap_err();
    assert_eq!(err_kind(e), (Some(ExcType::ValueError), Some("negative shift count")));
    let big = fx.heap_int(pow2(70));
    let e = binary(&mut fx, BinaryOp::Lshift, n(1), big).unwrap_err();
    assert_eq!(e, PrimError::OverflowWord);
}

#[test]
fn binary_not_handled_boundaries() {
    let mut fx = Fixture::new();
    let n = Value::SmallInt;
    let f = fx.heap_float(1.5);
    // C18a: the int slot delegates a float rhs DIRECTLY to the
    // float op (vendor `objint_mpz.c:175`) — `2 + 1.5` is `3.5`,
    // no longer `NotHandled`.
    assert_eq!(
        as_f64(binary_val(&mut fx, BinaryOp::Add, n(2), f), &fx.arena),
        Some(3.5)
    );
    let s = fx.heap_str(b"ab");
    // The `MULTIPLY` delegation (vendor `objint.c:381`) calls
    // the rhs type's slot — but THIS fixture builds every type
    // plain (no slots), so the delegation finds no slot and the
    // int slot returns `NotHandled` here. The real behavior
    // (`2 * 'ab' == 'abab'`, `2 * [1] == [1, 1]`) is covered
    // end to end in `objfloat_tests::int_float_interop`, where
    // the VM wires the real `str` / `list` / `tuple` slots.
    assert_eq!(binary(&mut fx, BinaryOp::Multiply, n(2), s).unwrap(), PrimOutcome::NotHandled);
    // `int + str` is still `NotHandled` (extra_cases only
    // handles `MULTIPLY`).
    assert_eq!(binary(&mut fx, BinaryOp::Add, n(2), s).unwrap(), PrimOutcome::NotHandled);
    let l = fx.heap_list(vec![Value::SmallInt(1)]);
    assert_eq!(binary(&mut fx, BinaryOp::Multiply, n(2), l).unwrap(), PrimOutcome::NotHandled);
    let t = fx.heap_tuple(vec![Value::SmallInt(1)]);
    assert_eq!(binary(&mut fx, BinaryOp::Multiply, n(2), t).unwrap(), PrimOutcome::NotHandled);
    assert_eq!(binary(&mut fx, BinaryOp::MatMultiply, n(2), n(3)).unwrap(), PrimOutcome::NotHandled);
}

// ── pow3 ────────────────────────────────────────────────────────────────

#[test]
fn pow3_cases() {
    let mut fx = Fixture::new();
    let n = Value::SmallInt;
    let v = fx.call(|ctx, t| int_pow3(ctx, t, n(2), n(3), n(5)).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(3));
    let v = fx.call(|ctx, t| int_pow3(ctx, t, n(2), n(-1), n(5)).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(0));
    let e = fx.call(|ctx, t| int_pow3(ctx, t, n(2), n(3), n(0)).unwrap_err());
    assert_eq!(err_kind(e), (Some(ExcType::ValueError), Some("divide by zero")));
    assert_eq!(e, PrimError::Pow3ModZero);
    let e = fx.call(|ctx, t| int_pow3(ctx, t, n(2), n(3), Value::Bool(true)).unwrap_err());
    assert_eq!(err_kind(e), (Some(ExcType::TypeError), Some("pow() with 3 arguments requires integers")));
    let f = fx.heap_float(5.0);
    let e = fx.call(|ctx, t| int_pow3(ctx, t, n(2), n(3), f).unwrap_err());
    assert_eq!(e, PrimError::Pow3RequiresInts);
}

// ── from_bytes / to_bytes ───────────────────────────────────────────────

#[test]
fn from_bytes_cases() {
    let mut fx = Fixture::new();
    let cls = fx.types.int;
    let v = fx.call(|ctx, _t| int_from_bytes(ctx, cls, b"\xff", false));
    assert_eq!(as_i64(v, &fx.arena), Some(255));
    let v = fx.call(|ctx, _t| int_from_bytes(ctx, cls, b"", false));
    assert_eq!(as_i64(v, &fx.arena), Some(0));
    let v = fx.call(|ctx, _t| int_from_bytes(ctx, cls, b"\x01\x00", true));
    assert_eq!(as_i64(v, &fx.arena), Some(1));
    let v = fx.call(|ctx, _t| int_from_bytes(ctx, cls, b"\x01\x00", false));
    assert_eq!(as_i64(v, &fx.arena), Some(256));
    let v = fx.call(|ctx, _t| int_from_bytes(ctx, cls, b"\x40\x00\x00\x00\x00\x00\x00\x00\x00", false));
    assert_eq!(as_big(v, &fx.arena).unwrap(), pow2(70));
}

#[test]
fn from_bytes_builtin_wrapper() {
    let mut fx = Fixture::new();
    let cls = Value::Heap(fx.types.int);
    let buf = fx.heap_bytes(b"\xff\x00");
    let little = Value::Qstr(static_qstr_id(b"little").unwrap());
    // 3 user args (cls, buf, byteorder): little → 255.
    let v = fx.call(|ctx, _t| int_from_bytes_builtin(ctx, &[cls, buf, little]).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(255));
    // 4th positional arg is ignored (no `signed` support).
    let v = fx.call(|ctx, _t| {
        int_from_bytes_builtin(ctx, &[cls, buf, little, Value::Bool(true)]).unwrap()
    });
    assert_eq!(as_i64(v, &fx.arena), Some(255));
    // Non-bytes buffer → BufferProtocol (data dropped at this boundary).
    let s = fx.heap_str(b"\xff");
    let e = fx.call(|ctx, _t| int_from_bytes_builtin(ctx, &[cls, s]).unwrap_err());
    assert_eq!(e, PyError::PyRaise);
    // Too few args → bare raise.
    let e = fx.call(|ctx, _t| int_from_bytes_builtin(ctx, &[cls]).unwrap_err());
    assert_eq!(e, PyError::PyRaise);
}

#[test]
fn to_bytes_cases() {
    let mut fx = Fixture::new();
    let little = Value::Qstr(static_qstr_id(b"little").unwrap());
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(255), Some(Value::SmallInt(1)), None).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xff]));
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(-2), Some(Value::SmallInt(1)), None).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xfe]));
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(255), Some(Value::SmallInt(2)), Some(little)).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xff, 0x00]));
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(-256), Some(Value::SmallInt(2)), None).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xff, 0x00]));
    let v = fx.call(|ctx, t| int_to_bytes(ctx, t, Value::SmallInt(0), Some(Value::SmallInt(0)), None).unwrap());
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![]));
    let v = fx.call(|ctx, t| int_to_bytes(ctx, t, Value::SmallInt(0), None, None).unwrap());
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0x00]));
    let big = fx.heap_int(pow2(70));
    let v = fx.call(|ctx, t| int_to_bytes(ctx, t, big, Some(Value::SmallInt(9)), None).unwrap());
    assert_eq!(
        as_bytes(v, &fx.arena),
        Some(vec![0x40, 0, 0, 0, 0, 0, 0, 0, 0])
    );
    // Too small → BufferTooSmall; negative length → bare ValueError.
    let e = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(256), Some(Value::SmallInt(1)), None).unwrap_err()
    });
    assert_eq!(err_kind(e), (Some(ExcType::OverflowError), Some("buffer too small")));
    let e = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(1), Some(Value::SmallInt(-1)), None).unwrap_err()
    });
    assert_eq!(e, PrimError::ValueErrorBare);
    assert_eq!(e.exc_type(), Some(ExcType::ValueError));
    assert_eq!(e.message(), None);
    // Bool length counts as 1.
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, Value::SmallInt(255), Some(Value::Bool(true)), None).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xff]));
    // Negative heap int: the vendor `mpz_as_bytes` overflow check
    // needs ceil(digits * 32 / 8) bytes, not the minimal length —
    // 10 and 11 bytes both fail on the reference binary, 12 succeeds.
    let neg_big = fx.heap_int(-pow2(70));
    let e = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, neg_big, Some(Value::SmallInt(11)), None).unwrap_err()
    });
    assert_eq!(e, PrimError::BufferTooSmall);
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, neg_big, Some(Value::SmallInt(12)), None).unwrap()
    });
    assert_eq!(
        as_bytes(v, &fx.arena),
        Some(vec![0xff, 0xff, 0xff, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0])
    );
    let neg_big = fx.heap_int(-pow2(62) - BigInt::from_int(1));
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, neg_big, Some(Value::SmallInt(8)), None).unwrap()
    });
    assert_eq!(as_bytes(v, &fx.arena), Some(vec![0xbf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]));
    let v = fx.call(|ctx, t| {
        int_to_bytes(ctx, t, neg_big, Some(Value::SmallInt(9)), None).unwrap()
    });
    assert_eq!(
        as_bytes(v, &fx.arena),
        Some(vec![0xff, 0xbf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
    );
}

// ── bool ────────────────────────────────────────────────────────────────

#[test]
fn bool_make_new_forms() {
    let mut fx = Fixture::new();
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[], 0).unwrap());
    assert_eq!(v, Value::Bool(false));
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[Value::SmallInt(0)], 0).unwrap());
    assert_eq!(v, Value::Bool(false));
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[Value::SmallInt(1)], 0).unwrap());
    assert_eq!(v, Value::Bool(true));
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[Value::None], 0).unwrap());
    assert_eq!(v, Value::Bool(false));
    let s = fx.heap_str(b"");
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[s], 0).unwrap());
    assert_eq!(v, Value::Bool(false));
    let s = fx.heap_str(b"x");
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[s], 0).unwrap());
    assert_eq!(v, Value::Bool(true));
    let l = fx.heap_list(vec![]);
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[l], 0).unwrap());
    assert_eq!(v, Value::Bool(false));
    let l = fx.heap_list(vec![Value::SmallInt(0)]);
    let v = fx.call(|ctx, t| bool_make_new(ctx, t, &[l], 0).unwrap());
    assert_eq!(v, Value::Bool(true));
    let e = fx.call(|ctx, t| bool_make_new(ctx, t, &[Value::SmallInt(1), Value::SmallInt(2)], 0).unwrap_err());
    assert_eq!(e, PrimError::ArgNumTypesMismatch);
}

#[test]
fn bool_ops_delegate_to_int() {
    let mut fx = Fixture::new();
    let t = Value::Bool(true);
    let f = Value::Bool(false);
    let v = fx.call(|ctx, ty| bool_binary_op(ctx, ty, BinaryOp::Add, t, t).unwrap());
    assert_eq!(as_i64(match v { PrimOutcome::Value(v) => v, _ => panic!("not handled") }, &fx.arena), Some(2));
    let v = fx.call(|ctx, ty| bool_binary_op(ctx, ty, BinaryOp::Multiply, t, Value::SmallInt(5)).unwrap());
    assert_eq!(as_i64(match v { PrimOutcome::Value(v) => v, _ => panic!("not handled") }, &fx.arena), Some(5));
    let v = fx.call(|ctx, ty| bool_unary_op(ctx, ty, UnaryOp::Negative, t).unwrap());
    assert_eq!(as_i64(match v { PrimOutcome::Value(v) => v, _ => panic!("not handled") }, &fx.arena), Some(-1));
    let v = fx.call(|ctx, ty| bool_unary_op(ctx, ty, UnaryOp::Invert, t).unwrap());
    assert_eq!(as_i64(match v { PrimOutcome::Value(v) => v, _ => panic!("not handled") }, &fx.arena), Some(-2));
    let v = fx.call(|ctx, ty| bool_unary_op(ctx, ty, UnaryOp::Len, t).unwrap());
    assert_eq!(v, PrimOutcome::NotHandled);
    assert_eq!(bool_to_string(true), "True");
    assert_eq!(bool_to_string(false), "False");
    let _ = f;
}

// ── cell / type ROM ─────────────────────────────────────────────────────

#[test]
fn cell_create_read_write() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let cell_type = fx.call(|ctx, _t| {
        let data = cell_type_data(dict);
        assert_eq!(data.name, 0);
        assert_eq!(data.flags, 0);
        assert_eq!(data.base, None);
        assert!(data.unary_op.is_none() && data.binary_op.is_none());
        ctx.alloc(None, Obj::Type(data))
    });
    let cell = fx.call(|ctx, _t| new_cell(ctx, cell_type, Value::SmallInt(7)));
    assert_eq!(cell_get(&fx.arena, cell), Some(Value::SmallInt(7)));
    cell_set(&mut fx.arena, cell, Value::SmallInt(8));
    assert_eq!(cell_get(&fx.arena, cell), Some(Value::SmallInt(8)));
    let none_cell = fx.call(|ctx, _t| new_cell(ctx, cell_type, Value::None));
    assert_eq!(cell_get(&fx.arena, none_cell), Some(Value::None));
}

#[test]
fn type_rom_data() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let d = int_type_data(dict);
    assert_eq!(d.name, static_qstr_id(b"int").unwrap());
    assert_eq!(d.flags, 0);
    assert_eq!(d.base, None);
    assert!(d.mro.is_empty());
    // C7r: the op slots are wired to the thin adapters; call /
    // getiter / subscr / buffer stay None (the original's int type
    // has no such slots). (Fn pointers are compared as addresses:
    // `assert_eq!` needs `Debug`, which fn items lack.)
    assert_eq!(d.unary_op.map(|f| f as usize), Some(int_unary_slot as UnaryOpFn as usize));
    assert_eq!(d.binary_op.map(|f| f as usize), Some(int_binary_slot as BinaryOpFn as usize));
    assert_eq!(d.print.map(|f| f as usize), Some(int_print as PrintFn as usize));
    assert!(d.call.is_none() && d.getiter.is_none() && d.subscr.is_none() && d.buffer.is_none());
    let d = bool_type_data(dict);
    assert_eq!(d.name, static_qstr_id(b"bool").unwrap());
    assert_eq!(d.flags, 0x0008);
    assert_eq!(d.base, None);
    assert_eq!(d.unary_op.map(|f| f as usize), Some(bool_unary_slot as UnaryOpFn as usize));
    assert_eq!(d.binary_op.map(|f| f as usize), Some(bool_binary_slot as BinaryOpFn as usize));
    assert_eq!(d.print.map(|f| f as usize), Some(bool_print as PrintFn as usize));
    assert!(d.call.is_none() && d.getiter.is_none() && d.subscr.is_none() && d.buffer.is_none());
    let d = cell_type_data(dict);
    assert_eq!(d.name, 0);
    assert_eq!(d.flags, 0);
    // Cell: ALL slots None (objcell.zig: all 11 slots null).
    assert!(
        d.unary_op.is_none()
            && d.binary_op.is_none()
            && d.call.is_none()
            && d.getiter.is_none()
            && d.subscr.is_none()
            && d.buffer.is_none()
            && d.print.is_none()
    );
    // bool is NOT an int subclass: both have base None.
    assert!(int_type_data(dict).base.is_none() && bool_type_data(dict).base.is_none());
}

#[test]
fn int_locals_dict_has_from_bytes() {
    let mut fx = Fixture::new();
    // The fixture has no `fun_builtin_var` (it lives on
    // `VmTypes`, not `BuiltinTypes`); a fresh type named
    // `function` stands in for it here.
    let fun_builtin_var = make_type(&mut fx.arena, &fx.roots, b"function");
    let classmethod = fx.types.classmethod;
    let dict_ref = fx.call(|ctx, _t| int_locals_dict(ctx, fun_builtin_var, classmethod));
    let entries = match fx.arena.get(dict_ref) {
        Obj::Dict(d) => d.map.entries.clone(),
        _ => panic!("not a dict"),
    };
    // C13 added `to_bytes` (sig 1–4 counting `self`).
    assert_eq!(entries.len(), 2);
    let from_bytes_id = static_qstr_id(b"from_bytes").unwrap();
    assert_eq!(entries[0].key, Some(Value::Qstr(from_bytes_id)));
    // C18b: `from_bytes` is a classmethod object wrapping the
    // `FunBuiltin` (the original's `MpObjStaticClassMethod`,
    // `objint.zig:431-438`) — the header type is `classmethod`,
    // so it prints `<classmethod>`.
    let (name, n_args, func) = match entries[0].value {
        Value::Heap(r) => {
            assert_eq!(fx.arena.header(r).type_, Some(classmethod));
            match fx.arena.get(r) {
                Obj::StaticClassMethod(d) => match d.fun {
                    Value::Heap(fr) => match fx.arena.get(fr) {
                        Obj::FunBuiltin(f) => {
                            assert_eq!(fx.arena.header(fr).type_, Some(fun_builtin_var));
                            let fp: crate::objects::slots::BuiltinFn = f.func;
                            (f.name, f.n_args, fp as usize)
                        }
                        _ => panic!("classmethod does not wrap a builtin"),
                    },
                    _ => panic!("classmethod fun is not heap"),
                },
                _ => panic!("from_bytes is not a classmethod object"),
            }
        }
        _ => panic!("not heap"),
    };
    assert_eq!(name, from_bytes_id);
    assert_eq!(n_args, fun_n_args(2, 4));
    let expected_fn: crate::objects::slots::BuiltinFn = int_from_bytes_builtin;
    assert_eq!(func, expected_fn as usize);
    let to_bytes_id = static_qstr_id(b"to_bytes").unwrap();
    assert_eq!(entries[1].key, Some(Value::Qstr(to_bytes_id)));
    let (name2, n_args2, func2) = match entries[1].value {
        Value::Heap(r) => {
            // `to_bytes` is a plain builtin with the var
            // function type as its header, so it prints
            // `<function>`.
            assert_eq!(fx.arena.header(r).type_, Some(fun_builtin_var));
            match fx.arena.get(r) {
                Obj::FunBuiltin(f) => {
                    let fp: crate::objects::slots::BuiltinFn = f.func;
                    (f.name, f.n_args, fp as usize)
                }
                _ => panic!("not a builtin"),
            }
        }
        _ => panic!("not heap"),
    };
    assert_eq!(name2, to_bytes_id);
    assert_eq!(n_args2, fun_n_args(1, 4));
    let expected_fn2: crate::objects::slots::BuiltinFn = int_to_bytes_builtin;
    assert_eq!(func2, expected_fn2 as usize);
}

// ── from_bytes classmethod binding (C18b) ───────────────────────────────

#[test]
fn int_from_bytes_classmethod_binding() {
    // The `int_locals_dict` fix stores `from_bytes` as a
    // classmethod object wrapping the `FunBuiltin`. The VM
    // cannot call it end-to-end yet (type-object attribute
    // lookup is blocked — see the ignored
    // `int_from_bytes_e2e_after_classmethod_wrap` test), so
    // the binding is verified here at `CallCtx` level:
    // `convert_member_lookup` must return the inner builtin
    // with the int type bound as `args[0]`, and calling it
    // must give the right value.
    use crate::objects::objtype::lookup::convert_member_lookup;
    let mut fx = Fixture::new();
    let fun_builtin_var = make_type(&mut fx.arena, &fx.roots, b"function");
    let classmethod = fx.types.classmethod;
    let int = fx.types.int;
    let dict_ref = fx.call(|ctx, _t| int_locals_dict(ctx, fun_builtin_var, classmethod));
    let member = match fx.arena.get(dict_ref) {
        Obj::Dict(d) => d.map.entries[0].value,
        _ => panic!("not a dict"),
    };
    let (func, bound) = fx.call(|ctx, _t| {
        convert_member_lookup(ctx, Some(Value::Heap(int)), int, member)
    });
    assert_eq!(bound, Some(Value::Heap(int)));
    let buf = fx.heap_bytes(b"\x01\x00");
    // The builtin only tests for the `little` qstr; any other
    // value means big-endian (`"big"` is not a static qstr).
    let big = Value::None;
    let little = Value::Qstr(static_qstr_id(b"little").unwrap());
    let v = fx.call(|ctx, _t| {
        let f = match ctx.arena.get(match func {
            Value::Heap(r) => r,
            _ => panic!("bound func is not heap"),
        }) {
            Obj::FunBuiltin(f) => f.func,
            _ => panic!("bound func is not a builtin"),
        };
        f(ctx, &[Value::Heap(int), buf, big]).unwrap()
    });
    assert_eq!(v, Value::SmallInt(256));
    let v2 = fx.call(|ctx, _t| {
        let f = match ctx.arena.get(match func {
            Value::Heap(r) => r,
            _ => panic!("bound func is not heap"),
        }) {
            Obj::FunBuiltin(f) => f.func,
            _ => panic!("bound func is not a builtin"),
        };
        f(ctx, &[Value::Heap(int), buf, little]).unwrap()
    });
    assert_eq!(v2, Value::SmallInt(1));
}

// ── formatting ──────────────────────────────────────────────────────────

#[test]
fn int_formatting() {
    let mut fx = Fixture::new();
    assert_eq!(int_to_string(Value::SmallInt(0), &fx.arena).unwrap(), "0");
    assert_eq!(int_to_string(Value::SmallInt(-42), &fx.arena).unwrap(), "-42");
    let big = fx.heap_int(pow2(70));
    assert_eq!(int_to_string(big, &fx.arena).unwrap(), "1180591620717411303424");
    assert_eq!(int_format(Value::SmallInt(1000), &fx.arena, 10, "", b'a', b',').unwrap(), "1,000");
    assert_eq!(int_format(Value::SmallInt(255), &fx.arena, 16, "0x", b'a', 0).unwrap(), "0xff");
    assert_eq!(int_format(Value::SmallInt(-255), &fx.arena, 16, "0x", b'a', 0).unwrap(), "-0xff");
    assert_eq!(int_format(Value::SmallInt(255), &fx.arena, 16, "0x", b'A', 0).unwrap(), "0xFF");
    assert_eq!(int_format(Value::SmallInt(0), &fx.arena, 16, "0x", b'a', 0).unwrap(), "0x0");
    assert_eq!(int_to_string(Value::None, &fx.arena), None);
}

// ── op enums ────────────────────────────────────────────────────────────

#[test]
fn op_enum_ordinals_match_vendor() {
    assert_eq!(UnaryOp::Positive.as_u8(), 0);
    assert_eq!(UnaryOp::IntMaybe.as_u8(), 8);
    assert_eq!(UnaryOp::Sizeof.as_u8(), 11);
    assert_eq!(UnaryOp::from_u8(8), Some(UnaryOp::IntMaybe));
    assert_eq!(UnaryOp::from_u8(12), None);
    assert_eq!(BinaryOp::Less.as_u8(), 0);
    assert_eq!(BinaryOp::InplacePower.as_u8(), 21);
    assert_eq!(BinaryOp::Power.as_u8(), 34);
    assert_eq!(BinaryOp::Divmod.as_u8(), 35);
    assert_eq!(BinaryOp::ReversePower.as_u8(), 49);
    assert_eq!(BinaryOp::IsNot.as_u8(), 51);
    assert_eq!(BinaryOp::from_u8(34), Some(BinaryOp::Power));
    assert_eq!(BinaryOp::from_u8(52), None);
}
