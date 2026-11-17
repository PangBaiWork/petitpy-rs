//! Lane C7r dispatch tests — prove C6's `call_unary` /
//! `call_binary` / `obj_print_helper` reach the int/bool slots wired
//! by this lane. The fixture builds the int/bool type objects from
//! `int_type_data` / `bool_type_data` (slots filled), unlike the C7
//! `Fixture` which builds plain types (all slots `None`).

use super::tests::*;
use super::*;
use crate::memory::gc::{CallCtx, GcArena, RootSet};
use crate::objects::obj::{call_binary, call_unary, SlotOutcome};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::PyError;
use crate::runtime::qstr::RuntimeQstr;
use crate::vendor::mpz::BigInt;

/// Fixture with slotted int/bool types (the C7 `Fixture` builds
/// plain types with all slots `None`).
struct SlotFixture {
    arena: GcArena,
    roots: RootSet,
    types: BuiltinTypes,
    pool: RuntimeQstr,
}

/// A call context borrowing the arena + roots + types fields only,
/// so `&fx.types` stays usable alongside it (a `Fixture::ctx`
/// method would tie the returned lifetime to the whole `&mut self` —
/// the C6 `obj_tests.rs` precedent).
fn make_ctx<'a>(
    arena: &'a mut GcArena,
    roots: &'a mut RootSet,
    types: &'a BuiltinTypes,
    pool: &'a RuntimeQstr,
) -> CallCtx<'a> {
    CallCtx::new(arena, roots, types, pool)
}

impl SlotFixture {
    fn new() -> SlotFixture {
        let mut arena = GcArena::new();
        let mut roots = RootSet::new();
        let type_ = make_type(&mut arena, &roots, b"type");
        let int_dict = empty_dict(&mut arena, &roots);
        let int = arena.alloc(None, Obj::Type(int_type_data(int_dict)), &roots);
        let bool_dict = empty_dict(&mut arena, &roots);
        let bool_ = arena.alloc(None, Obj::Type(bool_type_data(bool_dict)), &roots);
        let str_ = make_type(&mut arena, &roots, b"str");
        let bytes = make_type(&mut arena, &roots, b"bytes");
        let none_type = make_type(&mut arena, &roots, b"NoneType");
        let float = make_type(&mut arena, &roots, b"float");
        let complex = make_type(&mut arena, &roots, b"complex");
        let tuple = make_type(&mut arena, &roots, b"tuple");
        let list = make_type(&mut arena, &roots, b"list");
        let dict = make_type(&mut arena, &roots, b"dict");
        for t in [int, str_, bytes, bool_, none_type, float, complex, tuple, list, dict, type_] {
            roots.push(Value::Heap(t));
        }
        let types = BuiltinTypes {
            int,
            str_,
            bytes,
            bool_,
            none_type,
            float,
            complex,
            tuple,
            list,
            dict,
            type_,
        };
        SlotFixture { arena, roots, types, pool: RuntimeQstr::new() }
    }

    fn heap_int(&mut self, z: BigInt) -> Value {
        let t = self.types.int;
        Value::Heap(self.arena.alloc(Some(t), Obj::Int(z), &self.roots))
    }

    fn heap_float(&mut self, f: f64) -> Value {
        let t = self.types.float;
        Value::Heap(self.arena.alloc(Some(t), Obj::Float(f), &self.roots))
    }
}

#[test]
fn dispatch_int_maybe_small_int_fast_path() {
    let mut fx = SlotFixture::new();
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    // The runtime `mp_unary_op` small-int fast path (C6's
    // `call_unary`): INT_MAYBE on a small int returns it without
    // consulting the slot.
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::IntMaybe, Value::SmallInt(42));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(42)))));
}

#[test]
fn dispatch_int_maybe_heap_int_via_slot() {
    let mut fx = SlotFixture::new();
    let big = fx.heap_int(pow2(70));
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    // A heap int has no fast path — INT_MAYBE reaches the int slot,
    // which returns the value itself.
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::IntMaybe, big);
    assert!(matches!(r, Ok(SlotOutcome::Value(v)) if v == big));
}

#[test]
fn dispatch_int_unary_via_slot() {
    let mut fx = SlotFixture::new();
    let big = fx.heap_int(pow2(70));
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Negative, Value::SmallInt(5));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(-5)))));
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Bool, Value::SmallInt(0));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(false)))));
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Bool, Value::SmallInt(3));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(true)))));
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Hash, Value::SmallInt(7));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(7)))));
    // Heap result: the arena cannot be read while `ctx` borrows it,
    // so the call runs in its own block and the payload is checked
    // after the context is dropped.
    let v = {
        let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
        match call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Negative, big) {
            Ok(SlotOutcome::Value(v)) => v,
            _ => panic!("expected a value from int -2**70"),
        }
    };
    assert_eq!(
        crate::objects::primitives::ops::heap_int(&fx.arena, v),
        Some(&(-pow2(70)))
    );
}

#[test]
fn dispatch_int_binary_add_and_compare() {
    let mut fx = SlotFixture::new();
    let big = fx.heap_int(pow2(70));
    let one = Value::SmallInt(1);
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let r = call_binary(
        &mut ctx,
        Some(fx.types.int),
        BinaryOp::Add,
        Value::SmallInt(3),
        Value::SmallInt(4),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(7)))));
    let v = {
        let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
        match call_binary(&mut ctx, Some(fx.types.int), BinaryOp::Add, big, one) {
            Ok(SlotOutcome::Value(v)) => v,
            _ => panic!("expected a value from 2**70 + 1"),
        }
    };
    assert_eq!(
        crate::objects::primitives::ops::heap_int(&fx.arena, v),
        Some(&(pow2(70) + BigInt::from_int(1)))
    );
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let r = call_binary(
        &mut ctx,
        Some(fx.types.int),
        BinaryOp::Less,
        Value::SmallInt(1),
        Value::SmallInt(2),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(true)))));
    let r = call_binary(
        &mut ctx,
        Some(fx.types.int),
        BinaryOp::Equal,
        Value::SmallInt(5),
        Value::SmallInt(5),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(true)))));
}

#[test]
fn dispatch_bool_ops_via_slot() {
    let mut fx = SlotFixture::new();
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let r = call_unary(&mut ctx, Some(fx.types.bool_), UnaryOp::Bool, Value::Bool(true));
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(true)))));
    let r = call_binary(
        &mut ctx,
        Some(fx.types.bool_),
        BinaryOp::Add,
        Value::Bool(true),
        Value::Bool(true),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(2)))));
    let r = call_binary(
        &mut ctx,
        Some(fx.types.bool_),
        BinaryOp::Multiply,
        Value::Bool(true),
        Value::SmallInt(3),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::SmallInt(3)))));
    let r = call_binary(
        &mut ctx,
        Some(fx.types.bool_),
        BinaryOp::Less,
        Value::Bool(true),
        Value::Bool(false),
    );
    assert!(matches!(r, Ok(SlotOutcome::Value(Value::Bool(false)))));
}

#[test]
fn dispatch_nothandled_is_not_implemented() {
    let mut fx = SlotFixture::new();
    let fl = fx.heap_float(1.5);
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    // LEN is not implemented by the int slot (the original's
    // `mp_obj_int_unary_op` returns NULL for it).
    let r = call_unary(&mut ctx, Some(fx.types.int), UnaryOp::Len, Value::SmallInt(5));
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
    // Float rhs: the original delegates DIRECTLY to the float slot
    // (C18) — the int slot returns NotHandled.
    let r = call_binary(
        &mut ctx,
        Some(fx.types.int),
        BinaryOp::Add,
        Value::SmallInt(1),
        fl,
    );
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
    // LEN on bool is NULL in `npy_bool_fn_unary_op` too.
    let r = call_unary(&mut ctx, Some(fx.types.bool_), UnaryOp::Len, Value::Bool(true));
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
}

#[test]
fn dispatch_raise_signals_pyraise() {
    let mut fx = SlotFixture::new();
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    // The adapter converts a rich raise to `Err(PyError::PyRaise)`.
    let r = int_binary_slot(
        &mut ctx,
        BinaryOp::FloorDivide,
        Value::SmallInt(1),
        Value::SmallInt(0),
    );
    assert!(matches!(r, Err(PyError::PyRaise)));
    // ... and a NotHandled the same way (the two are indistinguishable
    // at the slot boundary — the accepted C6 convention).
    let r = int_unary_slot(&mut ctx, UnaryOp::Len, Value::SmallInt(5));
    assert!(matches!(r, Err(PyError::PyRaise)));
    // A handled op still returns `Ok`.
    let r = int_unary_slot(&mut ctx, UnaryOp::Negative, Value::SmallInt(5));
    assert!(matches!(r, Ok(Value::SmallInt(-5))));
    // Through `call_binary` the raise is likewise NotImplemented —
    // the raise data is dropped at the slot boundary until C13 adds
    // `pending_exc` (C6 convention, not a bug).
    let r = call_binary(
        &mut ctx,
        Some(fx.types.int),
        BinaryOp::FloorDivide,
        Value::SmallInt(1),
        Value::SmallInt(0),
    );
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
}

#[test]
fn dispatch_print_int_and_bool() {
    let mut fx = SlotFixture::new();
    let big = fx.heap_int(pow2(70));
    let fl = fx.heap_float(1.5);
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let mut s = String::new();
    obj_print_helper(&mut ctx, &fx.types, Value::SmallInt(42), PrintKind::Str, &mut s);
    assert_eq!(s, "42");
    // Kind is ignored by the int print slot (original takes `_`).
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, Value::SmallInt(42), PrintKind::Repr, &mut s);
    assert_eq!(s, "42");
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, big, PrintKind::Str, &mut s);
    assert_eq!(s, "1180591620717411303424");
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, Value::Bool(true), PrintKind::Str, &mut s);
    assert_eq!(s, "True");
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, Value::Bool(false), PrintKind::Repr, &mut s);
    assert_eq!(s, "False");
    // No print slot on the plain float type → the `<name>` fallback.
    s.clear();
    obj_print_helper(&mut ctx, &fx.types, fl, PrintKind::Str, &mut s);
    assert_eq!(s, "<float>");
}

#[test]
fn dispatch_cell_slots_stay_none() {
    let mut fx = SlotFixture::new();
    let cell_dict = empty_dict(&mut fx.arena, &fx.roots);
    let cell_type: GcRef =
        fx.arena
            .alloc(None, Obj::Type(cell_type_data(cell_dict)), &fx.roots);
    let mut ctx = make_ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool);
    let cell = new_cell(&mut ctx, cell_type, Value::SmallInt(9));
    // All 11 cell slots are null in the original — dispatch finds
    // no slot at all.
    let r = call_unary(&mut ctx, Some(cell_type), UnaryOp::Bool, Value::Heap(cell));
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
    let r = call_binary(
        &mut ctx,
        Some(cell_type),
        BinaryOp::Add,
        Value::Heap(cell),
        Value::SmallInt(1),
    );
    assert!(matches!(r, Ok(SlotOutcome::NotImplemented)));
}
