//! Tests for range (C9c): make_new, len/bool, print, subscr (int +
//! slice), getiter/next, attr, type data. Every error case is
//! verified against the reference binary (see `objrange.rs` docs
//! for the probe citations).

use super::objrange::{
    range_attr, range_getiter, range_it_next, range_len, range_make_new, range_print,
    range_subscr, range_type_data, range_unary_op, RangeError,
};
use super::objslice::slice_make_new;
use super::objslice::SliceError;
use super::tests::{empty_dict, Fixture};
use crate::objects::obj::obj_get_int;
use crate::objects::obj::ObjError;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::PyError;
use crate::vendor::mpz::BigInt;

/// A heap `Int` holding `z` (for out-of-window bounds like 2**62).
fn heap_int(fx: &mut Fixture, z: BigInt) -> Value {
    let t = fx.types.int;
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(Some(t), Obj::Int(z)))
}

/// Allocate a real slotted `range` type in the fixture (rooted, the
/// `Fixture::new` pattern).
fn range_type(fx: &mut Fixture) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let t = fx
        .arena
        .alloc(None, Obj::Type(range_type_data(dict)), &fx.roots);
    fx.roots.push(Value::Heap(t));
    t
}

/// Read a range's `(start, stop, step)` back.
fn parts_of(fx: &Fixture, v: Value) -> (i64, i64, i64) {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Range(d) => (d.start, d.stop, d.step),
            _ => panic!("expected a range"),
        },
        _ => panic!("expected a heap range"),
    }
}

/// Build a range via `range_make_new` (the fixture's real type).
fn mk_range(fx: &mut Fixture, args: &[Value]) -> Value {
    let t = range_type(fx);
    let mut ctx = fx.ctx();
    range_make_new(&mut ctx, t, args, 0).expect("mk_range")
}

/// Build a slice value (for the range-subscr slice tests).
fn mk_slice(fx: &mut Fixture, args: &[Value]) -> Value {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let t = fx.arena.alloc(
        None,
        Obj::Type(super::objslice::slice_type_data(dict)),
        &fx.roots,
    );
    fx.roots.push(Value::Heap(t));
    let mut ctx = fx.ctx();
    slice_make_new(&mut ctx, t, args, 0).expect("mk_slice")
}

/// Print a range through its print slot into a `String`.
fn print_of(fx: &mut Fixture, v: Value) -> String {
    let mut out = String::new();
    let mut ctx = fx.ctx();
    range_print(&mut ctx, v, PrintKind::Str, &mut out);
    out
}

/// Assert a `RangeError`'s type name + message.
fn assert_err(e: RangeError, name: &str, msg: &str) {
    assert_eq!(e.exc_name(), name);
    assert_eq!(e.msg(), Some(msg));
}

/// Read an int-valued result back as `i64` (small or heap int).
fn as_i64(fx: &mut Fixture, v: Value) -> i64 {
    let mut ctx = fx.ctx();
    let types = ctx.types;
    obj_get_int(&mut ctx, types, v).expect("int result")
}

// ── make_new ─────────────────────────────────────────────────────────

#[test]
fn range_make_new_one_arg() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    assert_eq!(parts_of(&fx, r), (0, 5, 1));
}

#[test]
fn range_make_new_two_args() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(1), Value::SmallInt(5)]);
    assert_eq!(parts_of(&fx, r), (1, 5, 1));
}

#[test]
fn range_make_new_three_args() {
    let mut fx = Fixture::new();
    let r = mk_range(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(10), Value::SmallInt(2)],
    );
    assert_eq!(parts_of(&fx, r), (1, 10, 2));
}

#[test]
fn range_make_new_arg_errors() {
    let mut fx = Fixture::new();
    let t = range_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = range_make_new(&mut ctx, t, &[], 0).expect_err("range()");
    assert_err(e, "TypeError", "argument num/types mismatch");
    let e = range_make_new(
        &mut ctx,
        t,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
        ],
        0,
    )
    .expect_err("range(1, 2, 3, 4)");
    assert_err(e, "TypeError", "argument num/types mismatch");
    // `range(1, x=1)`: kwargs are rejected (n_kw != 0).
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let e = range_make_new(
        &mut ctx,
        t,
        &[Value::SmallInt(1), Value::Qstr(x), Value::SmallInt(1)],
        1,
    )
    .expect_err("range(1, x=1)");
    assert_err(e, "TypeError", "argument num/types mismatch");
}

#[test]
fn range_make_new_zero_step() {
    let mut fx = Fixture::new();
    let t = range_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = range_make_new(
        &mut ctx,
        t,
        &[Value::SmallInt(0), Value::SmallInt(10), Value::SmallInt(0)],
        0,
    )
    .expect_err("range(0, 10, 0)");
    assert_err(e, "ValueError", "zero step");
}

#[test]
fn range_make_new_conversion_errors() {
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let t = range_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = range_make_new(&mut ctx, t, &[a], 0).expect_err("range('a')");
    assert_err(e, "TypeError", "can't convert to int");
    drop(ctx);
    // `range(2**70)` → OverflowError (verified on the reference binary).
    let big = heap_int(&mut fx, BigInt::from_int(1) << 70);
    let mut ctx = fx.ctx();
    let e = range_make_new(&mut ctx, t, &[big], 0).expect_err("range(2**70)");
    assert_err(
        e,
        "OverflowError",
        "overflow converting long int to machine word",
    );
}

// ── len / bool / unary ───────────────────────────────────────────────

#[test]
fn range_len_cases() {
    assert_eq!(range_len(0, 10, 1), 10);
    assert_eq!(range_len(0, 10, 3), 4);
    assert_eq!(range_len(10, 0, -3), 4);
    assert_eq!(range_len(0, 10, -3), 0);
    assert_eq!(range_len(0, 0, 1), 0);
    assert_eq!(range_len(5, 5, 1), 0);
    assert_eq!(range_len(0, 10, -1), 0);
    assert_eq!(range_len(10, 0, 1), 0);
    // Big lengths (verified on the reference binary).
    assert_eq!(range_len(0, 1 << 62, 1), 1 << 62);
    assert_eq!(range_len(0, 1 << 62, 2), 1 << 61);
    // A step of 0 returns 0 (the Zig divides by zero and aborts —
    // documented divergence, see `objrange.rs`).
    assert_eq!(range_len(0, 1 << 32, 0), 0);
}

#[test]
fn range_len_big_via_slot() {
    let mut fx = Fixture::new();
    let big = heap_int(&mut fx, BigInt::from_int(1) << 62);
    let r = mk_range(&mut fx, &[Value::SmallInt(0), big]);
    let mut ctx = fx.ctx();
    let len = range_unary_op(&mut ctx, UnaryOp::Len, r).expect("len()");
    drop(ctx);
    // `len(range(0, 2**62))` = 2**62 — a heap `Int` (out of window).
    assert_eq!(as_i64(&mut fx, len), 1 << 62);
}

#[test]
fn range_bool() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_unary_op(&mut ctx, UnaryOp::Bool, r),
        Ok(Value::Bool(true))
    );
    drop(ctx);
    let r = mk_range(&mut fx, &[Value::SmallInt(0)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_unary_op(&mut ctx, UnaryOp::Bool, r),
        Ok(Value::Bool(false))
    );
    drop(ctx);
    let r = mk_range(&mut fx, &[Value::SmallInt(5), Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_unary_op(&mut ctx, UnaryOp::Bool, r),
        Ok(Value::Bool(false))
    );
}

#[test]
fn range_unary_other_ops_opt_out() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_unary_op(&mut ctx, UnaryOp::Hash, r),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        range_unary_op(&mut ctx, UnaryOp::Negative, r),
        Err(PyError::PyRaise)
    );
}

// ── print ────────────────────────────────────────────────────────────

#[test]
fn range_print_forms() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(0), Value::SmallInt(5)]);
    assert_eq!(print_of(&mut fx, r), "range(0, 5)");
    let r = mk_range(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(10), Value::SmallInt(2)],
    );
    assert_eq!(print_of(&mut fx, r), "range(1, 10, 2)");
    let r = mk_range(
        &mut fx,
        &[Value::SmallInt(0), Value::SmallInt(10), Value::SmallInt(-1)],
    );
    assert_eq!(print_of(&mut fx, r), "range(0, 10, -1)");
}

// ── subscr: int index ────────────────────────────────────────────────

#[test]
fn range_subscr_int() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(10)]);
    let mut ctx = fx.ctx();
    let v = range_subscr(&mut ctx, r, Value::SmallInt(0), SubscrArg::Get).expect("r[0]");
    drop(ctx);
    assert_eq!(as_i64(&mut fx, v), 0);
    let mut ctx = fx.ctx();
    let v = range_subscr(&mut ctx, r, Value::SmallInt(9), SubscrArg::Get).expect("r[9]");
    drop(ctx);
    assert_eq!(as_i64(&mut fx, v), 9);
    let mut ctx = fx.ctx();
    let v = range_subscr(&mut ctx, r, Value::SmallInt(-1), SubscrArg::Get).expect("r[-1]");
    drop(ctx);
    assert_eq!(as_i64(&mut fx, v), 9);
    let mut ctx = fx.ctx();
    let v = range_subscr(&mut ctx, r, Value::SmallInt(-10), SubscrArg::Get).expect("r[-10]");
    drop(ctx);
    assert_eq!(as_i64(&mut fx, v), 0);
}

#[test]
fn range_subscr_out_of_range() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    // The slot boundary drops the rich error (`IndexError: index out
    // of range` — see `range_error_mappings`) to `PyRaise`.
    assert_eq!(
        range_subscr(&mut ctx, r, Value::SmallInt(10), SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        range_subscr(&mut ctx, r, Value::SmallInt(-6), SubscrArg::Get),
        Err(PyError::PyRaise)
    );
}

#[test]
fn range_subscr_not_int() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    let a = fx.heap_str(b"a");
    let mut ctx = fx.ctx();
    // The slot boundary drops the rich error (`TypeError: indices
    // must be integers` — see `range_error_mappings`) to `PyRaise`.
    assert_eq!(
        range_subscr(&mut ctx, r, a, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    drop(ctx);
    // A float index has no `__index__` (the float type's IntMaybe
    // slot returns NotImplemented — C18 requirement).
    let t = fx.types.float;
    let mut ctx = fx.ctx();
    let f = Value::Heap(ctx.alloc(Some(t), Obj::Float(1.5)));
    assert_eq!(
        range_subscr(&mut ctx, r, f, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
}

#[test]
fn range_subscr_set_del_opt_out() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_subscr(
            &mut ctx,
            r,
            Value::SmallInt(0),
            SubscrArg::Set(Value::SmallInt(9))
        ),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        range_subscr(&mut ctx, r, Value::SmallInt(0), SubscrArg::Delete),
        Err(PyError::PyRaise)
    );
}

// ── subscr: slice index ──────────────────────────────────────────────

#[test]
fn range_subscr_slice() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(10)]);
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(2), Value::SmallInt(8), Value::SmallInt(2)],
    );
    let mut ctx = fx.ctx();
    let out = range_subscr(&mut ctx, r, s, SubscrArg::Get).expect("r[2:8:2]");
    drop(ctx);
    assert_eq!(parts_of(&fx, out), (2, 8, 2));
    let s = mk_slice(
        &mut fx,
        &[Value::None, Value::None, Value::SmallInt(-1)],
    );
    let mut ctx = fx.ctx();
    let out = range_subscr(&mut ctx, r, s, SubscrArg::Get).expect("r[::-1]");
    drop(ctx);
    assert_eq!(parts_of(&fx, out), (9, -1, -1));
    let s = mk_slice(&mut fx, &[Value::SmallInt(5), Value::None, Value::None]);
    let mut ctx = fx.ctx();
    let out = range_subscr(&mut ctx, r, s, SubscrArg::Get).expect("r[5:]");
    drop(ctx);
    assert_eq!(parts_of(&fx, out), (5, 10, 1));
}

#[test]
fn range_subscr_slice_negative_step() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(10)]);
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(8), Value::SmallInt(2), Value::SmallInt(-2)],
    );
    let mut ctx = fx.ctx();
    let out = range_subscr(&mut ctx, r, s, SubscrArg::Get).expect("r[8:2:-2]");
    drop(ctx);
    assert_eq!(parts_of(&fx, out), (8, 2, -2));
}

#[test]
fn range_subscr_slice_bad_bounds() {
    // `range(10)[slice(1,2,"a")]` → step-zero (the pending from
    // `slice_indices_polled` is returned); `range(10)[slice("a",2,3)]`
    // → int-conversion (both verified on the reference binary).
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(10)]);
    let a = fx.heap_str(b"a");
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), a],
    );
    let mut ctx = fx.ctx();
    // The slot boundary drops the rich error (`ValueError: slice
    // step can't be zero` / `TypeError: can't convert to int` — see
    // `range_error_mappings`) to `PyRaise`.
    assert_eq!(
        range_subscr(&mut ctx, r, s, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    drop(ctx);
    let s = mk_slice(&mut fx, &[a, Value::SmallInt(2), Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        range_subscr(&mut ctx, r, s, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
}

#[test]
fn range_error_mappings() {
    // The subscr slot drops the rich error to `PyRaise`, so the
    // exc_name/msg mapping is tested directly here.
    let e = RangeError::from(SliceError::ValueError("slice step can't be zero"));
    assert_err(e, "ValueError", "slice step can't be zero");
    let e = RangeError::from(SliceError::TypeError("can't convert to int"));
    assert_err(e, "TypeError", "can't convert to int");
    let e = RangeError::from(ObjError::IndexOutOfRange);
    assert_err(e, "IndexError", "index out of range");
    let e = RangeError::from(ObjError::IndicesMustBeInts);
    assert_err(e, "TypeError", "indices must be integers");
    let e = RangeError::from(ObjError::Overflow);
    assert_err(
        e,
        "OverflowError",
        "overflow converting long int to machine word",
    );
}

#[test]
fn range_subscr_slice_wrapped_step() {
    // `range(0, 10, 2**32)[::2**32]` is `range(0, 4294967296, 0)`:
    // the step product wraps to 0 (verified on the reference binary).
    let mut fx = Fixture::new();
    let r = mk_range(
        &mut fx,
        &[
            Value::SmallInt(0),
            Value::SmallInt(10),
            Value::SmallInt(1 << 32),
        ],
    );
    let s = mk_slice(
        &mut fx,
        &[Value::None, Value::None, Value::SmallInt(1 << 32)],
    );
    let mut ctx = fx.ctx();
    let out = range_subscr(&mut ctx, r, s, SubscrArg::Get).expect("wrapped step");
    drop(ctx);
    assert_eq!(parts_of(&fx, out), (0, 1 << 32, 0));
}

// ── iterator ─────────────────────────────────────────────────────────

/// Drain a range iterator into plain `i64`s.
fn drain(fx: &mut Fixture, it: Value) -> Vec<i64> {
    let mut out = Vec::new();
    loop {
        let mut ctx = fx.ctx();
        match range_it_next(&mut ctx, it).expect("it_next") {
            Some(v) => {
                drop(ctx);
                out.push(as_i64(fx, v));
            }
            None => break,
        }
    }
    out
}

#[test]
fn range_iter_yields() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let it = range_getiter(&mut ctx, r).expect("getiter");
    drop(ctx);
    assert_eq!(drain(&mut fx, it), vec![0, 1, 2]);
    // Exhausted: a further next returns `None`.
    let mut ctx = fx.ctx();
    assert_eq!(range_it_next(&mut ctx, it).expect("exhausted"), None);
}

#[test]
fn range_iter_negative_step() {
    let mut fx = Fixture::new();
    let r = mk_range(
        &mut fx,
        &[Value::SmallInt(5), Value::SmallInt(0), Value::SmallInt(-2)],
    );
    let mut ctx = fx.ctx();
    let it = range_getiter(&mut ctx, r).expect("getiter");
    drop(ctx);
    assert_eq!(drain(&mut fx, it), vec![5, 3, 1]);
}

#[test]
fn range_iter_empty() {
    let mut fx = Fixture::new();
    let r = mk_range(&mut fx, &[Value::SmallInt(0)]);
    let mut ctx = fx.ctx();
    let it = range_getiter(&mut ctx, r).expect("getiter");
    drop(ctx);
    assert_eq!(drain(&mut fx, it), Vec::<i64>::new());
    let r = mk_range(&mut fx, &[Value::SmallInt(5), Value::SmallInt(0)]);
    let mut ctx = fx.ctx();
    let it = range_getiter(&mut ctx, r).expect("getiter");
    drop(ctx);
    assert_eq!(drain(&mut fx, it), Vec::<i64>::new());
}

// ── attr / type data ─────────────────────────────────────────────────

#[test]
fn range_attr_bounds() {
    let mut fx = Fixture::new();
    let r = mk_range(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(10), Value::SmallInt(3)],
    );
    let start = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let stop = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let step = static_qstr_id(b"step").expect("MP_QSTR_step is static");
    let mut ctx = fx.ctx();
    assert_eq!(range_attr(&mut ctx, r, start), Some(Value::SmallInt(1)));
    assert_eq!(range_attr(&mut ctx, r, stop), Some(Value::SmallInt(10)));
    assert_eq!(range_attr(&mut ctx, r, step), Some(Value::SmallInt(3)));
    // No method table: `index` / `count` are not attributes.
    let index = static_qstr_id(b"index").expect("MP_QSTR_index is static");
    assert_eq!(range_attr(&mut ctx, r, index), None);
}

#[test]
fn range_type_data_shape() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = range_type_data(dict);
    assert_eq!(
        data.name,
        static_qstr_id(b"range").expect("MP_QSTR_range is static")
    );
    assert_eq!(data.flags, 0x0000);
    assert!(data.unary_op.is_some());
    assert!(data.subscr.is_some());
    assert!(data.getiter.is_some());
    assert!(data.print.is_some());
    assert!(data.attr.is_some());
    // Full config: NO binary op (the nano equality-only op is out
    // of scope).
    assert!(data.binary_op.is_none());
    assert!(data.call.is_none());
    assert!(data.buffer.is_none());
}
