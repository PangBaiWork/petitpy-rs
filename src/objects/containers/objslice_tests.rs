//! Tests for slice (C9c): make_new, the `indices` method (incl.
//! the polled-pending rules — step-zero overwrites, the
//! `mp_obj_get_int_maybe` guard), unary op, print, attr, type data.
//! Every error case is verified against the reference binary (see
//! `objslice.rs` docs for the probe citations).

use super::objslice::{
    slice_attr, slice_indices_builtin, slice_indices_method, slice_indices_polled,
    slice_make_new, slice_print, slice_type_data, slice_unary_op, SliceError,
};
use super::tests::{empty_dict, Fixture};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind, UnaryOp};
use crate::objects::strings::objstr_slice::BoundSlice;
use crate::objects::value::{GcRef, Obj, SliceData, Value};
use crate::pyerr::PyError;

/// Allocate a real slotted `slice` type in the fixture (rooted, the
/// `Fixture::new` pattern).
fn slice_type(fx: &mut Fixture) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let t = fx
        .arena
        .alloc(None, Obj::Type(slice_type_data(dict)), &fx.roots);
    fx.roots.push(Value::Heap(t));
    t
}

/// Read a slice's stored bounds back (cloned).
fn bounds_of(fx: &Fixture, v: Value) -> SliceData {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Slice(s) => s.clone(),
            _ => panic!("expected a slice"),
        },
        _ => panic!("expected a heap slice"),
    }
}

/// Extract a 3-tuple of small ints as plain `i64`s.
fn tuple_ints(fx: &Fixture, v: Value) -> Vec<i64> {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Tuple(items) => items
                .iter()
                .map(|x| match x {
                    Value::SmallInt(n) => *n,
                    _ => panic!("expected small-int tuple items"),
                })
                .collect(),
            _ => panic!("expected a tuple"),
        },
        _ => panic!("expected a heap tuple"),
    }
}

/// Print a slice through its print slot into a `String`.
fn print_of(fx: &mut Fixture, v: Value) -> String {
    let mut out = String::new();
    let mut ctx = fx.ctx();
    slice_print(&mut ctx, v, PrintKind::Str, &mut out);
    out
}

/// Assert a `SliceError`'s type name + message.
fn assert_err(e: SliceError, name: &str, msg: &str) {
    assert_eq!(e.exc_name(), name);
    assert_eq!(e.msg(), Some(msg));
}

// ── make_new ─────────────────────────────────────────────────────────

#[test]
fn slice_make_new_one_arg() {
    let mut fx = Fixture::new();
    let t = slice_type(&mut fx);
    let mut ctx = fx.ctx();
    let s = slice_make_new(&mut ctx, t, &[Value::SmallInt(5)], 0).expect("slice(5)");
    drop(ctx);
    let b = bounds_of(&fx, s);
    assert_eq!(b.start, None);
    assert_eq!(b.stop, Some(Value::SmallInt(5)));
    assert_eq!(b.step, None);
}

#[test]
fn slice_make_new_two_args() {
    let mut fx = Fixture::new();
    let t = slice_type(&mut fx);
    let mut ctx = fx.ctx();
    let s = slice_make_new(&mut ctx, t, &[Value::SmallInt(1), Value::SmallInt(2)], 0)
        .expect("slice(1, 2)");
    drop(ctx);
    let b = bounds_of(&fx, s);
    assert_eq!(b.start, Some(Value::SmallInt(1)));
    assert_eq!(b.stop, Some(Value::SmallInt(2)));
    assert_eq!(b.step, None);
}

#[test]
fn slice_make_new_three_args() {
    let mut fx = Fixture::new();
    let t = slice_type(&mut fx);
    let mut ctx = fx.ctx();
    let s = slice_make_new(
        &mut ctx,
        t,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        0,
    )
    .expect("slice(1, 2, 3)");
    drop(ctx);
    let b = bounds_of(&fx, s);
    assert_eq!(b.start, Some(Value::SmallInt(1)));
    assert_eq!(b.stop, Some(Value::SmallInt(2)));
    assert_eq!(b.step, Some(Value::SmallInt(3)));
}

#[test]
fn slice_make_new_arg_errors() {
    let mut fx = Fixture::new();
    let t = slice_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = slice_make_new(&mut ctx, t, &[], 0).expect_err("slice()");
    assert_err(e, "TypeError", "slice expected 1 to 3 arguments");
    let e = slice_make_new(
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
    .expect_err("slice(1, 2, 3, 4)");
    assert_err(e, "TypeError", "slice expected 1 to 3 arguments");
    // `slice(1, x=1)`: kwargs are rejected (n_kw != 0). The kw name
    // itself is irrelevant — use a static qstr as the key.
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let e = slice_make_new(
        &mut ctx,
        t,
        &[Value::SmallInt(1), Value::Qstr(x), Value::SmallInt(1)],
        1,
    )
    .expect_err("slice(1, x=1)");
    assert_err(e, "TypeError", "slice expected 1 to 3 arguments");
}

#[test]
fn slice_make_new_none_normalized() {
    let mut fx = Fixture::new();
    let t = slice_type(&mut fx);
    let mut ctx = fx.ctx();
    let s = slice_make_new(&mut ctx, t, &[Value::None, Value::None, Value::None], 0)
        .expect("slice(None, None, None)");
    drop(ctx);
    let b = bounds_of(&fx, s);
    assert_eq!(b.start, None);
    assert_eq!(b.stop, None);
    assert_eq!(b.step, None);
    let mut ctx = fx.ctx();
    let s = slice_make_new(&mut ctx, t, &[Value::None, Value::SmallInt(5)], 0)
        .expect("slice(None, 5)");
    drop(ctx);
    let b = bounds_of(&fx, s);
    assert_eq!(b.start, None);
    assert_eq!(b.stop, Some(Value::SmallInt(5)));
    assert_eq!(b.step, None);
}

// ── indices method ───────────────────────────────────────────────────

/// Build a slice via `slice_make_new` for the `indices` tests.
fn mk_slice(fx: &mut Fixture, args: &[Value]) -> Value {
    let t = slice_type(fx);
    let mut ctx = fx.ctx();
    slice_make_new(&mut ctx, t, args, 0).expect("mk_slice")
}

#[test]
fn slice_indices_basic() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(10)).expect("indices(10)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![1, 2, 3]);
}

#[test]
fn slice_indices_all_none() {
    let mut fx = Fixture::new();
    let s = mk_slice(&mut fx, &[Value::None, Value::None, Value::None]);
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect("indices(5)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![0, 5, 1]);
}

#[test]
fn slice_indices_negative_step() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::None, Value::None, Value::SmallInt(-1)],
    );
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect("indices(5)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![4, -1, -1]);
}

#[test]
fn slice_indices_negative_bounds() {
    let mut fx = Fixture::new();
    let s = mk_slice(&mut fx, &[Value::SmallInt(-3), Value::None, Value::None]);
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(10)).expect("indices(10)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![7, 10, 1]);
    let s = mk_slice(
        &mut fx,
        &[Value::None, Value::None, Value::SmallInt(-2)],
    );
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(10)).expect("indices(10)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![9, -1, -2]);
}

#[test]
fn slice_indices_step_zero_string() {
    // `slice(1,2,"a").indices(5)` → step-zero: the step conversion
    // fails → 0 → `raiseStepZero` OVERWRITES the int-conversion
    // pending (reference binary, probe 2).
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), a],
    );
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect_err("step 'a'");
    assert_err(e, "ValueError", "slice step can't be zero");
}

#[test]
fn slice_indices_step_zero_int() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(0)],
    );
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect_err("step 0");
    assert_err(e, "ValueError", "slice step can't be zero");
}

#[test]
fn slice_indices_start_not_int() {
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let s = mk_slice(&mut fx, &[a, Value::SmallInt(2), Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect_err("start 'a'");
    assert_err(e, "TypeError", "can't convert to int");
}

#[test]
fn slice_indices_stop_not_int() {
    let mut fx = Fixture::new();
    let b = fx.heap_str(b"b");
    let s = mk_slice(&mut fx, &[Value::SmallInt(1), b, Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, Value::SmallInt(5)).expect_err("stop 'b'");
    assert_err(e, "TypeError", "can't convert to int");
}

#[test]
fn slice_indices_length_not_int() {
    // `slice(1,2).indices("a")` → int-conversion: the length error
    // is recorded first and the impl still runs; step is `None`
    // (→ 1), so nothing overwrites it (reference binary, probe 3).
    let mut fx = Fixture::new();
    let s = mk_slice(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let a = fx.heap_str(b"a");
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, a).expect_err("length 'a'");
    assert_err(e, "TypeError", "can't convert to int");
}

#[test]
fn slice_indices_length_error_overwritten_by_step() {
    // `slice(1,2,3).indices("x")` → step-zero: with pending set by
    // the failed length conversion, the step conversion reads back 0
    // (`mp_obj_get_int_maybe`'s guard, obj.zig:197) and raises
    // step-zero, OVERWRITING the length error (reference binary).
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let x = fx.heap_str(b"x");
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, x).expect_err("length 'x', step 3");
    assert_err(e, "ValueError", "slice step can't be zero");
}

#[test]
fn slice_indices_length_error_survives_none_step() {
    // `slice("a",2).indices("x")` → int-conversion: step is `None`
    // (→ 1, no conversion), so the length error survives — the
    // start conversion reads back 0 with pending already set and
    // records nothing (reference binary).
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let s = mk_slice(&mut fx, &[a, Value::SmallInt(2)]);
    let x = fx.heap_str(b"x");
    let mut ctx = fx.ctx();
    let e = slice_indices_method(&mut ctx, s, x).expect_err("length 'x', start 'a'");
    assert_err(e, "TypeError", "can't convert to int");
}

#[test]
fn slice_indices_zero_length() {
    let mut fx = Fixture::new();
    let s = mk_slice(&mut fx, &[Value::None, Value::None, Value::None]);
    let mut ctx = fx.ctx();
    let out = slice_indices_method(&mut ctx, s, Value::SmallInt(0)).expect("indices(0)");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![0, 0, 1]);
}

// ── unary op / print / attr / type data ──────────────────────────────

#[test]
fn slice_unary_op_opts_out() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let mut ctx = fx.ctx();
    // Every op — including HASH — returns NULL (PyRaise at the slot
    // boundary): the type explicitly opts out of default hashing.
    for op in [UnaryOp::Hash, UnaryOp::Len, UnaryOp::Bool] {
        assert_eq!(
            slice_unary_op(&mut ctx, op, s),
            Err(PyError::PyRaise),
            "op {op:?} must opt out"
        );
    }
}

#[test]
fn slice_print_forms() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    assert_eq!(print_of(&mut fx, s), "slice(1, 2, 3)");
    let s = mk_slice(&mut fx, &[Value::None, Value::None, Value::None]);
    assert_eq!(print_of(&mut fx, s), "slice(None, None, None)");
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::None, Value::SmallInt(-1)],
    );
    assert_eq!(print_of(&mut fx, s), "slice(1, None, -1)");
}

#[test]
fn slice_attr_bounds() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let start = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let stop = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let step = static_qstr_id(b"step").expect("MP_QSTR_step is static");
    let mut ctx = fx.ctx();
    assert_eq!(slice_attr(&mut ctx, s, start), Some(Value::SmallInt(1)));
    assert_eq!(slice_attr(&mut ctx, s, stop), Some(Value::SmallInt(2)));
    assert_eq!(slice_attr(&mut ctx, s, step), Some(Value::SmallInt(3)));
    drop(ctx);
    // A missing bound reads back as the VALUE `None`, not as
    // "no such attr".
    let s = mk_slice(&mut fx, &[Value::SmallInt(5)]);
    let mut ctx = fx.ctx();
    assert_eq!(slice_attr(&mut ctx, s, start), Some(Value::None));
    assert_eq!(slice_attr(&mut ctx, s, stop), Some(Value::SmallInt(5)));
    assert_eq!(slice_attr(&mut ctx, s, step), Some(Value::None));
    // Unknown attr → not found (`len` is a static qstr that is not
    // a slice attribute).
    let bogus = static_qstr_id(b"len").expect("MP_QSTR_len is static");
    assert_eq!(slice_attr(&mut ctx, s, bogus), None);
}

#[test]
fn slice_attr_indices_bound_method() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let indices = static_qstr_id(b"indices").expect("MP_QSTR_indices is static");
    let mut ctx = fx.ctx();
    let bound = slice_attr(&mut ctx, s, indices).expect("indices attr");
    drop(ctx);
    match bound {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::BoundMeth(b) => {
                assert_eq!(b.self_, s);
                match b.func {
                    Value::Heap(fr) => match fx.arena.get(fr) {
                        Obj::FunBuiltin(f) => {
                            assert_eq!(f.name, indices);
                            assert_eq!(f.n_args, fun_n_args(2, 2));
                        }
                        _ => panic!("indices func is not a FunBuiltin"),
                    },
                    _ => panic!("indices func is not a heap value"),
                }
            }
            _ => panic!("indices attr is not a BoundMeth"),
        },
        _ => panic!("indices attr is not a heap value"),
    }
}

#[test]
fn slice_indices_builtin_adapter() {
    let mut fx = Fixture::new();
    let s = mk_slice(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    );
    let mut ctx = fx.ctx();
    let out = slice_indices_builtin(&mut ctx, &[s, Value::SmallInt(10)]).expect("builtin");
    drop(ctx);
    assert_eq!(tuple_ints(&fx, out), vec![1, 2, 3]);
    // Wrong arity → mismatch (PyRaise at the slot boundary).
    let mut ctx = fx.ctx();
    assert_eq!(
        slice_indices_builtin(&mut ctx, &[s]),
        Err(PyError::PyRaise)
    );
}

#[test]
fn slice_type_data_shape() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = slice_type_data(dict);
    assert_eq!(
        data.name,
        static_qstr_id(b"slice").expect("MP_QSTR_slice is static")
    );
    assert_eq!(data.flags, 0x0000);
    assert!(data.unary_op.is_some());
    assert!(data.print.is_some());
    assert!(data.binary_op.is_none());
    assert!(data.call.is_none());
    assert!(data.getiter.is_none());
    assert!(data.subscr.is_none());
    assert!(data.buffer.is_none());
}

#[test]
fn slice_indices_polled_pending_guard() {
    // Unit-level check of the `mp_obj_get_int_maybe` guard inside
    // `slice_indices_polled`: with pending already set, a step
    // conversion reads back 0 and raises step-zero (overwriting);
    // with a `None` step the earlier error survives.
    let mut fx = Fixture::new();
    let data = SliceData {
        start: Some(Value::SmallInt(1)),
        stop: Some(Value::SmallInt(2)),
        step: Some(Value::SmallInt(3)),
    };
    let mut pending: Option<SliceError> =
        Some(SliceError::TypeError("can't convert to int"));
    let mut ctx = fx.ctx();
    let types = ctx.types;
    let b = slice_indices_polled(&mut ctx, types, &data, 5, &mut pending);
    drop(ctx);
    assert_eq!(
        b,
        BoundSlice {
            start: 0,
            stop: 0,
            step: 0
        }
    );
    assert_eq!(
        pending,
        Some(SliceError::ValueError("slice step can't be zero"))
    );
    let data = SliceData {
        start: Some(Value::SmallInt(1)),
        stop: Some(Value::SmallInt(2)),
        step: None,
    };
    let mut pending: Option<SliceError> =
        Some(SliceError::TypeError("can't convert to int"));
    let mut ctx = fx.ctx();
    let types = ctx.types;
    let b = slice_indices_polled(&mut ctx, types, &data, 5, &mut pending);
    drop(ctx);
    assert_eq!(
        b,
        BoundSlice {
            start: 0,
            stop: 0,
            step: 1
        }
    );
    assert_eq!(
        pending,
        Some(SliceError::TypeError("can't convert to int"))
    );
}
