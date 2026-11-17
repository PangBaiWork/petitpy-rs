//! Tests for `obj_ops.rs` (Lane C6) — equality, printing,
//! indexing, exceptions. The fixture + slot stubs live in
//! `obj_tests.rs` (shared harness).

use crate::objects::obj::*;
use crate::objects::obj_ops::*;
use crate::objects::slots::*;
use crate::objects::obj_tests::{
    buffer_ok_slot, ctx, eq_false_slot, eq_true_slot, len3_slot, print_marker_slot, qstr_id,
    subscr_echo_slot, Fixture, BUF_BYTES,
};
use crate::objects::value::*;
use crate::vendor::mpz::BigInt;

#[test]
fn get_type_str_names() {
    let mut fx = Fixture::new();
    let f = fx.value_of(fx.types.float, Obj::Float(1.5));
    assert_eq!(obj_get_type_str(&fx.arena, Value::SmallInt(1)), b"int");
    assert_eq!(obj_get_type_str(&fx.arena, Value::Bool(true)), b"bool");
    assert_eq!(obj_get_type_str(&fx.arena, Value::None), b"NoneType");
    assert_eq!(obj_get_type_str(&fx.arena, Value::Qstr(0)), b"str");
    assert_eq!(obj_get_type_str(&fx.arena, f), b"float");
}

// ── equality ──────────────────────────────────────────────────────────

#[test]
fn equal_reflexive_and_small_ints() {
    let mut fx = Fixture::new();
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::Equal, Value::SmallInt(5), Value::SmallInt(5)).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::Equal, Value::SmallInt(5), Value::SmallInt(6)).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::NotEqual, Value::SmallInt(5), Value::SmallInt(6)).unwrap(),
        Value::Bool(true)
    );
    assert!(obj_equal(&mut ctx, &fx.types, Value::SmallInt(5), Value::SmallInt(5)).unwrap());
}

#[test]
fn equal_str_fast_path() {
    let mut fx = Fixture::new();
    let mk = |fx: &mut Fixture, s: &[u8]| {
        fx.value_of(
            fx.types.str_,
            Obj::Str(StrData {
                bytes: s.to_vec(),
            }),
        )
    };
    let a = mk(&mut fx, b"abc");
    let b = mk(&mut fx, b"abc");
    let c = mk(&mut fx, b"abd");
    // qstr vs heap string with the same (static) content.
    let q = Value::Qstr(qstr_id(b"str"));
    let h = mk(&mut fx, b"str");
    // str vs bytes → False (the warning goes to stderr).
    let by = fx.value_of(
        fx.types.bytes,
        Obj::Bytes(BytesData {
            bytes: b"str".to_vec(),
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert!(obj_equal(&mut ctx, &fx.types, a, b).unwrap());
    assert!(!obj_equal(&mut ctx, &fx.types, a, c).unwrap());
    assert!(obj_equal(&mut ctx, &fx.types, q, h).unwrap());
    assert!(!obj_equal(&mut ctx, &fx.types, q, by).unwrap());
    assert!(!obj_equal(&mut ctx, &fx.types, by, q).unwrap());
}

#[test]
fn equal_via_slot_and_neq_first() {
    let mut fx = Fixture::new();
    // EQ_CHECKS_OTHER_TYPE | EQ_HAS_NEQ_TEST: NOT_EQUAL tries __ne__ first.
    let t = fx.slotted_type(
        "float",
        FLAG_EQ_CHECKS_OTHER_TYPE | FLAG_EQ_HAS_NEQ_TEST,
        None,
        Some(eq_true_slot),
        None,
        None,
        None,
        None,
    );
    let v1 = fx.value_of(t, Obj::Float(1.0));
    let v2 = fx.value_of(t, Obj::Float(2.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::Equal, v1, v2).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::NotEqual, v1, v2).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn equal_not_equal_without_neq_test_inverts_equal() {
    let mut fx = Fixture::new();
    // No EQ_HAS_NEQ_TEST: NOT_EQUAL falls back to Equal + is_true.
    let t = fx.slotted_type(
        "float",
        FLAG_EQ_CHECKS_OTHER_TYPE,
        None,
        Some(eq_true_slot),
        None,
        None,
        None,
        None,
    );
    let v1 = fx.value_of(t, Obj::Float(1.0));
    let v2 = fx.value_of(t, Obj::Float(2.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    // Equal says true → NOT_EQUAL is false.
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::NotEqual, v1, v2).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn equal_reflexive_flag_goes_to_slot() {
    let mut fx = Fixture::new();
    // EQ_NOT_REFLEXIVE: the identical-value shortcut must not fire.
    let t = fx.slotted_type(
        "float",
        FLAG_EQ_NOT_REFLEXIVE | FLAG_EQ_CHECKS_OTHER_TYPE,
        None,
        Some(eq_false_slot),
        None,
        None,
        None,
        None,
    );
    let v = fx.value_of(t, Obj::Float(1.0));
    // Without the flag the same value is reflexively true.
    let t2 = fx.slotted_type(
        "float",
        FLAG_EQ_CHECKS_OTHER_TYPE,
        None,
        Some(eq_false_slot),
        None,
        None,
        None,
        None,
    );
    let w = fx.value_of(t2, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::Equal, v, v).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        obj_equal_not_equal(&mut ctx, &fx.types, BinaryOp::Equal, w, w).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn equal_cross_type_flag_skips_dispatch() {
    let mut fx = Fixture::new();
    // No EQ_CHECKS_OTHER_TYPE: comparing with a different type skips
    // the slot entirely → pointer fallback → False.
    let t = fx.slotted_type("float", 0, None, Some(eq_true_slot), None, None, None, None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let f = fx.value_of(fx.types.float, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert!(!obj_equal(&mut ctx, &fx.types, v, f).unwrap());
}

// ── print ─────────────────────────────────────────────────────────────

#[test]
fn print_helper_fallback_and_slot() {
    let mut fx = Fixture::new();
    let f = fx.value_of(fx.types.float, Obj::Float(1.5));
    let t = fx.slotted_type("float", 0, None, None, None, None, Some(print_marker_slot), None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    let mut s = String::new();
    obj_print_helper(&mut ctx, &fx.types, f, PrintKind::Repr, &mut s);
    assert_eq!(s, "<float>");
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, v, PrintKind::Str, &mut s);
    assert_eq!(s, "PRINTED");
}

// ── array / index / subscr / buffer ───────────────────────────────────

#[test]
fn get_array_tuple_list_and_errors() {
    let mut fx = Fixture::new();
    let tup = fx.value_of(
        fx.types.tuple,
        Obj::Tuple(vec![Value::SmallInt(1), Value::SmallInt(2)]),
    );
    let lst = fx.value_of(
        fx.types.list,
        Obj::List(ListData {
            items: vec![Value::SmallInt(3)],
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(
        obj_get_array(&mut ctx, &fx.types, tup).unwrap(),
        vec![Value::SmallInt(1), Value::SmallInt(2)]
    );
    assert_eq!(obj_get_array(&mut ctx, &fx.types, lst).unwrap(), vec![Value::SmallInt(3)]);
    assert_eq!(
        obj_get_array(&mut ctx, &fx.types, Value::SmallInt(1)).unwrap_err(),
        ObjError::ExpectedTupleList
    );
    assert_eq!(
        obj_get_array_fixed_n(&mut ctx, &fx.types, tup, 3).unwrap_err(),
        ObjError::WrongLength
    );
    assert_eq!(obj_get_array_fixed_n(&mut ctx, &fx.types, tup, 2).unwrap().len(), 2);
}

#[test]
fn get_index_wrap_clamp_and_errors() {
    let mut fx = Fixture::new();
    let big70 = fx.bigint(BigInt::from_bytes(true, &[0x40, 0, 0, 0, 0, 0, 0, 0, 0]));
    let s = fx.value_of(
        fx.types.str_,
        Obj::Str(StrData {
            bytes: b"x".to_vec(),
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(get_index(&mut ctx, &fx.types, 5, Value::SmallInt(2), false).unwrap(), 2);
    assert_eq!(get_index(&mut ctx, &fx.types, 5, Value::SmallInt(-1), false).unwrap(), 4);
    assert_eq!(get_index(&mut ctx, &fx.types, 5, Value::SmallInt(7), true).unwrap(), 5);
    assert_eq!(get_index(&mut ctx, &fx.types, 5, Value::SmallInt(-9), true).unwrap(), 0);
    assert_eq!(
        get_index(&mut ctx, &fx.types, 5, Value::SmallInt(5), false).unwrap_err(),
        ObjError::IndexOutOfRange
    );
    assert_eq!(
        get_index(&mut ctx, &fx.types, 5, s, false).unwrap_err(),
        ObjError::IndicesMustBeInts
    );
    // A bignum index keeps its Overflow (the Zig pending_exc check).
    assert_eq!(
        get_index(&mut ctx, &fx.types, 5, big70, false).unwrap_err(),
        ObjError::Overflow
    );
}

#[test]
fn subscr_via_slot_and_fallthrough_raises() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("float", 0, None, None, Some(subscr_echo_slot), None, None, None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(
        obj_subscr(&mut ctx, &fx.types, v, Value::SmallInt(7), SubscrArg::Get).unwrap(),
        Value::SmallInt(7)
    );
    assert_eq!(
        obj_subscr(&mut ctx, &fx.types, v, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(1)))
            .unwrap_err(),
        ObjError::NoItemAssignment
    );
    assert_eq!(
        obj_subscr(&mut ctx, &fx.types, v, Value::SmallInt(0), SubscrArg::Delete).unwrap_err(),
        ObjError::NoItemDeletion
    );
    assert_eq!(
        obj_subscr(&mut ctx, &fx.types, Value::SmallInt(1), Value::SmallInt(0), SubscrArg::Get)
            .unwrap_err(),
        ObjError::NotSubscriptable
    );
}

#[test]
fn identity_and_binary_unary_wrappers() {
    let mut fx = Fixture::new();
    assert_eq!(identity(Value::SmallInt(4)), Value::SmallInt(4));
    let t = fx.slotted_type("float", 0, Some(len3_slot), Some(eq_true_slot), None, None, None, None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_unary_op(&mut ctx, &fx.types, UnaryOp::Len, v).unwrap(), Value::SmallInt(3));
    assert_eq!(
        obj_binary_op(&mut ctx, &fx.types, BinaryOp::Equal, v, v).unwrap(),
        Value::Bool(true)
    );
    // No slot → PyRaise ("not implemented").
    assert!(obj_unary_op(&mut ctx, &fx.types, UnaryOp::Len, Value::SmallInt(1)).is_err());
    assert!(obj_binary_op(&mut ctx, &fx.types, BinaryOp::Equal, Value::SmallInt(1), Value::SmallInt(1)).is_err());
}

#[test]
fn get_buffer_slot_and_fallbacks() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("float", 0, None, None, None, Some(buffer_ok_slot), None, None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let f = fx.value_of(fx.types.float, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    let mut info = BufferInfo::empty();
    assert!(get_buffer(&mut ctx, &fx.types, v, &mut info, BUFFER_READ).unwrap());
    assert_eq!(info.len, 3);
    assert_eq!(info.data, BUF_BYTES.as_ptr());
    assert_eq!(
        get_buffer(
            &mut ctx,
            &fx.types,
            f,
            &mut BufferInfo::empty(),
            BUFFER_READ | BUFFER_RAISE_IF_UNSUPPORTED
        )
        .unwrap_err(),
        ObjError::BufferProtocol
    );
    assert!(!get_buffer(&mut ctx, &fx.types, f, &mut BufferInfo::empty(), BUFFER_READ).unwrap());
}

// ── exception printing ────────────────────────────────────────────────

fn static_resolve(id: u32) -> &'static [u8] {
    crate::qstr_table::QSTR_STRINGS
        .get(id as usize)
        .copied()
        .unwrap_or(b"?")
}

fn exc_value(fx: &mut Fixture, type_name: &str, cause: Option<Value>, frames: Vec<TraceFrame>) -> Value {
    let t = fx.slotted_type(
        type_name,
        0,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    // The exception value's header type is its own type object.
    fx.value_of(
        t,
        Obj::Exception(ExceptionData {
            type_: t,
            args: vec![],
            traceback: frames,
            cause,
        }),
    )
}

#[test]
fn print_exception_simple() {
    let mut fx = Fixture::new();
    let e = exc_value(&mut fx, "ValueError", None, vec![]);
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    let mut s = String::new();
    obj_print_exception(&mut ctx, &fx.types, e, &static_resolve, &mut s);
    assert_eq!(s, "<ValueError>\n");
    drop(ctx);
    assert!(is_exception_instance(&fx.arena, e));
    assert!(!is_exception_instance(&fx.arena, Value::SmallInt(1)));
    assert_eq!(exception_cause(&fx.arena, e), None);
}

#[test]
fn print_exception_cause_chain_and_traceback() {
    let mut fx = Fixture::new();
    let inner = exc_value(&mut fx, "ValueError", None, vec![]);
    let frame = TraceFrame {
        name: qstr_id(b"str"),
        line: 42,
        block: Some(qstr_id(b"int")),
    };
    let outer = exc_value(&mut fx, "TypeError", Some(inner), vec![frame]);
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    let mut s = String::new();
    obj_print_exception(&mut ctx, &fx.types, outer, &static_resolve, &mut s);
    // Cause first, then the separator, then the oldest-first
    // traceback (no line numbers in this config), then the outer
    // exception. `line: 42` is stored but never printed.
    assert_eq!(
        s,
        "<ValueError>\n\nThe above exception was the direct cause of the following exception:\n\nTraceback (most recent call last):\n  File \"str\", in int\n<TypeError>\n"
    );
    drop(ctx);
    assert_eq!(exception_cause(&fx.arena, outer), Some(inner));
}

