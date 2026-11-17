//! Tests for attrtuple (C9c): construction, print (incl. the
//! shared helper C11 will use for namedtuple repr), attr lookup,
//! type data. The print form `(start=1, stop=2)` (no type-name prefix)
//! and the `PRINT_REPR` items are verified against the reference
//! binary via `namedtuple` (the only Python-level constructor).

use super::objattrtuple::{
    attrtuple_attr, attrtuple_new, attrtuple_print, attrtuple_print_helper,
    attrtuple_type_data,
};
use super::objtuple::{tuple_binary_op, tuple_getiter, tuple_subscr, tuple_unary_op};
use super::tests::{empty_dict, Fixture};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{GcRef, Obj, Value};

/// Allocate a real `attrtuple` type in the fixture (rooted, the
/// `Fixture::new` pattern). The type NAME is `tuple`.
fn attrtuple_type(fx: &mut Fixture) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let t = fx
        .arena
        .alloc(None, Obj::Type(attrtuple_type_data(dict)), &fx.roots);
    fx.roots.push(Value::Heap(t));
    t
}

/// Build an attrtuple with fields `start`, `stop` and items `1`, `2`.
fn mk_fields(fx: &mut Fixture) -> Value {
    let t = attrtuple_type(fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let y = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let mut ctx = fx.ctx();
    attrtuple_new(
        &mut ctx,
        t,
        vec![x, y],
        vec![Value::SmallInt(1), Value::SmallInt(2)],
    )
}

/// Print an attrtuple through its print slot into a `String`.
fn print_of(fx: &mut Fixture, v: Value) -> String {
    let mut out = String::new();
    let mut ctx = fx.ctx();
    attrtuple_print(&mut ctx, v, PrintKind::Str, &mut out);
    out
}

#[test]
fn attrtuple_new_stores_fields_and_items() {
    let mut fx = Fixture::new();
    let a = mk_fields(&mut fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let y = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let z = static_qstr_id(b"len").expect("MP_QSTR_len is static");
    assert_eq!(attrtuple_attr(&fx.arena, a, x), Some(Value::SmallInt(1)));
    assert_eq!(attrtuple_attr(&fx.arena, a, y), Some(Value::SmallInt(2)));
    assert_eq!(attrtuple_attr(&fx.arena, a, z), None);
}

#[test]
fn attrtuple_print_forms() {
    let mut fx = Fixture::new();
    let a = mk_fields(&mut fx);
    // No type-name prefix — just `(field=item, ...)`.
    assert_eq!(print_of(&mut fx, a), "(start=1, stop=2)");
    // Empty attrtuple.
    let t = attrtuple_type(&mut fx);
    let mut ctx = fx.ctx();
    let empty = attrtuple_new(&mut ctx, t, vec![], vec![]);
    drop(ctx);
    assert_eq!(print_of(&mut fx, empty), "()");
    // Single field.
    let t = attrtuple_type(&mut fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let mut ctx = fx.ctx();
    let one = attrtuple_new(&mut ctx, t, vec![x], vec![Value::SmallInt(1)]);
    drop(ctx);
    assert_eq!(print_of(&mut fx, one), "(start=1)");
}

#[test]
fn attrtuple_print_str_item_repr() {
    // Items print with PRINT_REPR: a str item is quoted (the
    // fixture's str double prints single quotes).
    let mut fx = Fixture::new();
    let s = fx.heap_str(b"a");
    let t = attrtuple_type(&mut fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let y = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let mut ctx = fx.ctx();
    let a = attrtuple_new(
        &mut ctx,
        t,
        vec![x, y],
        vec![s, Value::SmallInt(2)],
    );
    drop(ctx);
    assert_eq!(print_of(&mut fx, a), "(start='a', stop=2)");
}

#[test]
fn attrtuple_attr_first_match_wins() {
    // The Zig scans the fields in order and returns on the first
    // hit, so a duplicated field name resolves to its first item
    // (unreachable from Python — namedtuple rejects duplicates —
    // but the scan order is the Zig's).
    let mut fx = Fixture::new();
    let t = attrtuple_type(&mut fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let mut ctx = fx.ctx();
    let a = attrtuple_new(
        &mut ctx,
        t,
        vec![x, x],
        vec![Value::SmallInt(1), Value::SmallInt(2)],
    );
    drop(ctx);
    assert_eq!(attrtuple_attr(&fx.arena, a, x), Some(Value::SmallInt(1)));
}

#[test]
fn attrtuple_print_helper_direct() {
    // The exported helper, as C11's namedtuple repr will call it:
    // fields + items given directly, no attrtuple object needed.
    let mut fx = Fixture::new();
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let y = static_qstr_id(b"stop").expect("MP_QSTR_stop is static");
    let mut out = String::new();
    let mut ctx = fx.ctx();
    attrtuple_print_helper(
        &mut ctx,
        &[x, y],
        &[Value::SmallInt(1), Value::SmallInt(2)],
        &mut out,
    );
    assert_eq!(out, "(start=1, stop=2)");
}

#[test]
#[should_panic(expected = "attrtuple fields/items length mismatch")]
fn attrtuple_new_length_mismatch_panics() {
    let mut fx = Fixture::new();
    let t = attrtuple_type(&mut fx);
    let x = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let mut ctx = fx.ctx();
    attrtuple_new(
        &mut ctx,
        t,
        vec![x],
        vec![Value::SmallInt(1), Value::SmallInt(2)],
    );
}

#[test]
fn attrtuple_type_data_shape() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = attrtuple_type_data(dict);
    // The type NAME is `tuple`, not `attrtuple`.
    assert_eq!(
        data.name,
        static_qstr_id(b"tuple").expect("MP_QSTR_tuple is static")
    );
    assert_eq!(data.flags, 0x0000);
    assert!(data.print.is_some());
    // The tuple slots are C9b's — wired at the rebase (this worktree
    // predated C9b); no make_new, no locals.
    assert!(data.unary_op.is_some());
    assert!(data.binary_op.is_some());
    assert!(data.call.is_none());
    assert!(data.getiter.is_some());
    assert!(data.subscr.is_some());
    assert!(data.buffer.is_none());
}

/// The four tuple slots delegate (rebase wiring): LEN / subscr /
/// getiter / equality go through the tuple's slots, because in the
/// Zig an attrtuple IS a tuple object (`objattrtuple.zig:3`).
#[test]
fn attrtuple_tuple_slots_delegate() {
    let mut fx = Fixture::new();
    let a = mk_fields(&mut fx);
    let mut ctx = fx.ctx();
    assert_eq!(
        tuple_unary_op(&mut ctx, UnaryOp::Len, a),
        Ok(Value::SmallInt(2))
    );
    assert_eq!(
        tuple_subscr(&mut ctx, a, Value::SmallInt(0), SubscrArg::Get),
        Ok(Value::SmallInt(1))
    );
    assert_eq!(
        tuple_subscr(&mut ctx, a, Value::SmallInt(-1), SubscrArg::Get),
        Ok(Value::SmallInt(2))
    );
    assert_eq!(
        tuple_binary_op(&mut ctx, BinaryOp::Equal, a, a),
        Ok(Value::Bool(true))
    );
    let it = tuple_getiter(&mut ctx, a).expect("getiter");
    // The iterator overlays the attrtuple itself (source == a).
    match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(i) => assert_eq!(i.source, a),
            _ => panic!("getiter did not return an iterator"),
        },
        _ => panic!("getiter did not return a heap value"),
    }
}
