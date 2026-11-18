//! Tests for `objtuple.rs`.
//!
//! Reference-binary probe values (2026-11-17, `/tmp/c9b_probe*.py`,
//! `/tmp/tuphash.py`): the Zig seeds the tuple hash with the ROM
//! empty-tuple object's address, 22815552 in the reference binary
//! (`hash((1,2)) == 22815555`, `hash((None,)) == 22815558`,
//! `hash(('a',)) == 22862084`, `hash((2**62-1, 2**62-1)) ==
//! 22815550`). Rust seeds with the empty tuple's arena index
//! (C12a — the singleton landed), so the values asserted here
//! are the seed-relative versions: `hash(()) == seed`,
//! `hash((1,2)) == seed + 3`, `hash((None,)) == seed + 6`,
//! `hash(('a',)) == seed + 46532`,
//! `hash((2**62-1, 2**62-1)) == seed - 2`.
//! The *structure* (seed + wrapping sum, `newSmallInt` tag-wrap)
//! is what is under test.
//!
//! Discipline: build every fixture value first, then create the
//! `CallCtx` — a live `ctx` borrows the fixture mutably, so no
//! `fx.heap_*` call may follow it in the same scope.

use super::objtuple::*;
use super::tests::{empty_dict, Fixture};
use crate::objects::obj::ObjError;
use crate::objects::slots::{BinaryOp, SubscrArg, UnaryOp};
use crate::objects::value::{Obj, SliceData, Value};
use crate::pyerr::PyError;

fn ints(items: &[i64]) -> Vec<Value> {
    items.iter().map(|&n| Value::SmallInt(n)).collect()
}

fn tup(fx: &mut Fixture, items: Vec<Value>) -> Value {
    fx.heap_tuple(items)
}

/// A heap `Obj::Slice` with these bounds (`None` = omitted).
fn slice_val(
    fx: &mut Fixture,
    start: Option<Value>,
    stop: Option<Value>,
    step: Option<Value>,
) -> Value {
    let r = fx
        .arena
        .alloc(None, Obj::Slice(SliceData { start, stop, step }), &fx.roots);
    Value::Heap(r)
}

fn print_to_string(ctx: &mut crate::memory::gc::CallCtx, v: Value) -> String {
    let mut s = String::new();
    tuple_print(ctx, v, crate::objects::slots::PrintKind::Repr, &mut s);
    s
}

#[test]
fn tuple_make_new_basics() {
    let mut fx = Fixture::new();
    let t = fx.types.tuple;
    let l = fx.heap_list(ints(&[1, 2]));
    let s = fx.heap_str(b"ab");
    let d = fx.dict();
    let a_key = fx.heap_str(b"a");
    fx.store(d, a_key, Value::SmallInt(1));
    let inner = tup(&mut fx, ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `tuple()` → empty.
    let out = tuple_make_new(ctx, t, &[], 0, None).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &[] as &[Value]);
    // `tuple([1, 2])` → (1, 2).
    let out = tuple_make_new(ctx, t, &[l], 0, None).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2]));
    // `tuple("ab")` → ('a', 'b') — heap `Str` chars.
    let out = tuple_make_new(ctx, t, &[s], 0, None).unwrap();
    let got = tuple_items(ctx, out).unwrap().to_vec();
    assert_eq!(got.len(), 2);
    assert!(matches!(got[0], Value::Heap(_)) && matches!(got[1], Value::Heap(_)));
    // `tuple(t) is t` — exact-tuple arg returned as-is.
    let out = tuple_make_new(ctx, t, &[inner], 0, None).unwrap();
    assert_eq!(out, inner);
    // `tuple({"a": 1})` → ('a',) — dict iterates its keys.
    let out = tuple_make_new(ctx, t, &[d], 0, None).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap().len(), 1);
}

#[test]
fn tuple_make_new_sig_error_ordering() {
    let mut fx = Fixture::new();
    let t = fx.types.tuple;
    let a = fx.heap_str(b"a");
    let b = fx.heap_str(b"b");
    let inner = tup(&mut fx, ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `tuple(1, 2)` → "object not iterable" (the `getiter`
    // failure overwrites the recorded sig error).
    assert_eq!(
        tuple_make_new(ctx, t, &[Value::SmallInt(1), Value::SmallInt(2)], 0, None),
        Err(TupleError::NotIterable)
    );
    // `tuple("a", "b")` → "argument num/types mismatch" (the
    // `getiter` succeeds, so the sig error survives).
    assert_eq!(
        tuple_make_new(ctx, t, &[a, b], 0, None),
        Err(TupleError::ArgNumTypesMismatch)
    );
    // `tuple((1, 2), 99)` → "argument num/types mismatch" (the
    // exact-tuple early return leaves the pending error).
    assert_eq!(
        tuple_make_new(ctx, t, &[inner, Value::SmallInt(99)], 0, None),
        Err(TupleError::ArgNumTypesMismatch)
    );
    // `tuple(5)` → "object not iterable".
    assert_eq!(
        tuple_make_new(ctx, t, &[Value::SmallInt(5)], 0, None),
        Err(TupleError::NotIterable)
    );
    // kwargs with no positional args → arg mismatch.
    assert_eq!(
        tuple_make_new(ctx, t, &[], 1, None),
        Err(TupleError::ArgNumTypesMismatch)
    );
}

#[test]
fn tuple_print_basics() {
    let mut fx = Fixture::new();
    let empty = tup(&mut fx, vec![]);
    let one = tup(&mut fx, ints(&[1]));
    let two = tup(&mut fx, ints(&[1, 2]));
    let a = fx.heap_str(b"a");
    let mixed = tup(&mut fx, vec![Value::SmallInt(1), a]);
    let inner = tup(&mut fx, ints(&[1, 2]));
    let nested = tup(&mut fx, vec![inner]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(print_to_string(ctx, empty), "()");
    assert_eq!(print_to_string(ctx, one), "(1,)");
    assert_eq!(print_to_string(ctx, two), "(1, 2)");
    assert_eq!(print_to_string(ctx, mixed), "(1, 'a')");
    // Nested tuples print through the helper.
    assert_eq!(print_to_string(ctx, nested), "((1, 2),)");
    // `str((1, "a"))` is the same — kind is ignored.
    let mut s = String::new();
    tuple_print(ctx, mixed, crate::objects::slots::PrintKind::Str, &mut s);
    assert_eq!(s, "(1, 'a')");
}

#[test]
fn tuple_unary_bool_len() {
    let mut fx = Fixture::new();
    let empty = tup(&mut fx, vec![]);
    let one = tup(&mut fx, ints(&[0]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(tuple_unary_op(ctx, UnaryOp::Bool, empty), Ok(Value::Bool(false)));
    assert_eq!(tuple_unary_op(ctx, UnaryOp::Bool, one), Ok(Value::Bool(true)));
    assert_eq!(tuple_unary_op(ctx, UnaryOp::Len, empty), Ok(Value::SmallInt(0)));
    assert_eq!(tuple_unary_op(ctx, UnaryOp::Len, one), Ok(Value::SmallInt(1)));
    // Anything else → NULL (slot NotImplemented).
    assert_eq!(tuple_unary_op(ctx, UnaryOp::IntMaybe, one), Err(PyError::PyRaise));
}

#[test]
fn tuple_hash_values() {
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let ab = fx.heap_bytes(b"ab");
    // A nested tuple whose header type is the REAL tuple type
    // (with the HASH slot): the fixture's plain tuple type has
    // no slot, so `hash_of` would fall back to the identity
    // word for it. The fixture swap (after `objlist.rs`) makes
    // this the default; until then the test wires the real
    // type explicitly.
    let real_tuple_dict = empty_dict(&mut fx.arena, &fx.roots);
    let real_tuple_t = fx
        .arena
        .alloc(None, Obj::Type(tuple_type_data(real_tuple_dict)), &fx.roots);
    fx.roots.push(Value::Heap(real_tuple_t));
    let inner = {
        let mut c = fx.ctx();
        Value::Heap(c.alloc(Some(real_tuple_t), Obj::Tuple(ints(&[1, 2]))))
    };
    // A heap int out of the small-int window (2**62) as an
    // element — built through `Value::int`.
    let int_t = fx.types.int;
    let huge = {
        let mut c2 = fx.ctx();
        Value::int(&mut c2.arena, Some(int_t), &c2.roots, 1i64 << 62)
    };
    // Seed = the empty tuple's identity word (C12a — the Zig
    // uses its address, 22815552 in the reference binary;
    // `hash(()) == id(())` holds in Rust too now). Hoisted
    // before the `ctx` borrow below.
    let seed = fx.types.empty_tuple.0 as i64;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(tuple_hash(ctx, &[]).unwrap(), seed);
    assert_eq!(tuple_hash(ctx, &ints(&[1, 2])).unwrap(), seed + 3);
    assert_eq!(tuple_hash(ctx, &ints(&[1])).unwrap(), seed + 1);
    assert_eq!(tuple_hash(ctx, &[Value::None]).unwrap(), seed + 6);
    assert_eq!(
        tuple_hash(ctx, &[Value::Bool(true), Value::Bool(false)]).unwrap(),
        seed + 1
    );
    // `hash(('a',)) == seed + 46532` (qstr hash, no str slot needed —
    // `hash_of` fast-paths str/bytes).
    assert_eq!(tuple_hash(ctx, &[a]).unwrap(), seed + 46532);
    assert_eq!(tuple_hash(ctx, &[ab]).unwrap(), seed + 28198);
    // `hash((2**62-1, 2**62-1)) == seed - 2` — the `newSmallInt`
    // tag-wrap (probe: 22815550 with seed 22815552).
    let big = Value::SmallInt((1i64 << 62) - 1);
    assert_eq!(tuple_hash(ctx, &[big, big]).unwrap(), seed - 2);
    assert_eq!(
        tuple_hash(ctx, &[big]).unwrap(),
        wrap_small_int(seed + ((1i64 << 62) - 1))
    );
    // Nested tuples compose through the slot, so the seed counts
    // twice: `hash(((1,2),)) == seed + hash((1,2)) == 2*seed + 3`
    // (reference probe: 45631107 = 22815552 + 22815555).
    assert_eq!(tuple_hash(ctx, &[inner]).unwrap(), seed + (seed + 3));
    // A heap-int element hashes through its slot; the tuple
    // hash is the seed plus that element hash, wrapped through
    // `newSmallInt`. NOTE: the element hash itself currently
    // differs from the reference for out-of-window values —
    // C7's int HASH returns `new_int_from_i64(hash_mpz())`
    // (a heap `Int(2**62)` here), while vendor
    // `objint_mpz.c:148` returns
    // `MP_OBJ_NEW_SMALL_INT(mpz_hash(...))` (tag-wrap, so
    // `hash(2**62) == -(2**62)` on the reference binary,
    // probed). That is a C7 divergence, NOT this lane's to
    // fix — but it cancels out at the tuple level: the
    // reference's tuple hash is `seed + (-(2**62))`, and
    // Rust's is `wrap_small_int(seed + (-(2**62)))`, the
    // same final value. Recorded for the main agent.
    let types = ctx.types;
    let eh =
        crate::objects::containers::map::hash_of(ctx, types, huge).unwrap() as i64;
    assert_eq!(tuple_hash(ctx, &[huge]).unwrap(), wrap_small_int(seed.wrapping_add(eh)));
    assert_eq!(wrap_small_int(eh), -(1i64 << 62));
}

#[test]
fn tuple_hash_unhashable_element() {
    let mut fx = Fixture::new();
    // A dict element: the dict type HAS a unary_op slot that
    // answers NULL for HASH → "unsupported type for operator"
    // (probed: `hash(({},))`).
    let d = fx.dict();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        tuple_hash(ctx, &[d]),
        Err(TupleError::UnsupportedOperator)
    );
    let e = tuple_hash(ctx, &[d]).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.message(), Some("unsupported type for operator"));
}

#[test]
fn tuple_binary_add_multiply() {
    let mut fx = Fixture::new();
    let a = tup(&mut fx, ints(&[1, 2]));
    let b = tup(&mut fx, ints(&[3]));
    let l = fx.heap_list(ints(&[3]));
    let float_t = fx.types.float;
    let f = {
        let mut c2 = fx.ctx();
        Value::Heap(c2.alloc(Some(float_t), Obj::Float(2.0)))
    };
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let out = tuple_binary_op(ctx, BinaryOp::Add, a, b).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2, 3]));
    // INPLACE_ADD is the same concat.
    let out = tuple_binary_op(ctx, BinaryOp::InplaceAdd, a, b).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2, 3]));
    // A non-tuple rhs → NULL.
    assert_eq!(tuple_binary_op(ctx, BinaryOp::Add, a, l), Err(PyError::PyRaise));
    // Multiply.
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::SmallInt(2)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2, 1, 2]));
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::SmallInt(0)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &[] as &[Value]);
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::SmallInt(-1)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &[] as &[Value]);
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::Bool(true)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2]));
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::Bool(false)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &[] as &[Value]);
    // `* 1` is a fresh tuple with the same items (no ROM
    // singleton yet — `is` identity is C12's).
    let out = tuple_binary_op(ctx, BinaryOp::Multiply, a, Value::SmallInt(1)).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[1, 2]));
    // A non-int rhs → NULL (the VM reports __mul__ 'tuple','float').
    assert_eq!(
        tuple_binary_op(ctx, BinaryOp::Multiply, a, f),
        Err(PyError::PyRaise)
    );
    // NOT_EQUAL never reaches the slot — the caller inverts EQUAL.
    assert_eq!(
        tuple_binary_op(ctx, BinaryOp::NotEqual, a, b),
        Err(PyError::PyRaise)
    );
}

#[test]
fn tuple_binary_compare() {
    let mut fx = Fixture::new();
    let a = tup(&mut fx, ints(&[1, 2]));
    let b = tup(&mut fx, ints(&[1, 2]));
    let c = tup(&mut fx, ints(&[1, 3]));
    let l = fx.heap_list(ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(tuple_binary_op(ctx, BinaryOp::Equal, a, b), Ok(Value::Bool(true)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::Equal, a, c), Ok(Value::Bool(false)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::Less, a, c), Ok(Value::Bool(true)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::Less, c, a), Ok(Value::Bool(false)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::LessEqual, a, b), Ok(Value::Bool(true)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::More, c, a), Ok(Value::Bool(true)));
    assert_eq!(tuple_binary_op(ctx, BinaryOp::MoreEqual, a, b), Ok(Value::Bool(true)));
    // A list rhs → NULL at the slot (the dispatch layer turns
    // `(1,2) == [1,2]` into False via the flag gate).
    assert_eq!(
        tuple_binary_op(ctx, BinaryOp::Equal, a, l),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        tuple_binary_op(ctx, BinaryOp::Less, a, l),
        Err(PyError::PyRaise)
    );
}

#[test]
fn tuple_subscr_index_and_slice() {
    let mut fx = Fixture::new();
    let t = tup(&mut fx, ints(&[10, 20, 30]));
    let tuple_t = fx.types.tuple;
    let a = fx.heap_str(b"a");
    let s13 = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(3)), None);
    let s0 = slice_val(&mut fx, None, Some(Value::SmallInt(0)), None);
    let s_rev = slice_val(&mut fx, None, None, Some(Value::SmallInt(-1)));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        tuple_subscr(ctx, t, Value::SmallInt(0), SubscrArg::Get),
        Ok(Value::SmallInt(10))
    );
    assert_eq!(
        tuple_subscr(ctx, t, Value::SmallInt(-1), SubscrArg::Get),
        Ok(Value::SmallInt(30))
    );
    assert_eq!(
        tuple_subscr(ctx, t, Value::Bool(true), SubscrArg::Get),
        Ok(Value::SmallInt(20))
    );
    // Out of range / bad index → NULL at the slot.
    assert_eq!(
        tuple_subscr(ctx, t, Value::SmallInt(5), SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        tuple_subscr(ctx, t, a, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    // Set / delete → NULL at the slot.
    assert_eq!(
        tuple_subscr(ctx, t, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(9))),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        tuple_subscr(ctx, t, Value::SmallInt(0), SubscrArg::Delete),
        Err(PyError::PyRaise)
    );
    // Slices: [1:3] → (20, 30); [:0] → ().
    let out = tuple_subscr(ctx, t, s13, SubscrArg::Get).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &ints(&[20, 30]));
    let out = tuple_subscr(ctx, t, s0, SubscrArg::Get).unwrap();
    assert_eq!(tuple_items(ctx, out).unwrap(), &[] as &[Value]);
    // step -1 → the SliceStep message at the direct fn.
    assert_eq!(
        tuple_subscr(ctx, t, s_rev, SubscrArg::Get),
        Err(PyError::PyRaise)
    );
    let items = tuple_items(ctx, t).unwrap().to_vec();
    let e = tuple_slice_load(
        ctx,
        tuple_t,
        &items,
        &SliceData { start: None, stop: None, step: Some(Value::SmallInt(-1)) },
    )
    .unwrap_err();
    assert_eq!(e, TupleError::SliceStep);
    assert_eq!(
        e.message(),
        Some("only slices with step=1 (aka None) are supported")
    );
    // step 0 → the same message (overwritten, probed).
    let e = tuple_slice_load(
        ctx,
        tuple_t,
        &items,
        &SliceData { start: None, stop: None, step: Some(Value::SmallInt(0)) },
    )
    .unwrap_err();
    assert_eq!(e, TupleError::SliceStep);
    // A non-int bound keeps its own error.
    let e = tuple_slice_load(
        ctx,
        tuple_t,
        &items,
        &SliceData { start: Some(a), stop: None, step: None },
    )
    .unwrap_err();
    assert_eq!(e, TupleError::Obj(ObjError::IntConversion));
}

#[test]
fn tuple_count_index_methods() {
    let mut fx = Fixture::new();
    let t = tup(&mut fx, ints(&[1, 2, 1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let items = ints(&[1, 2, 1]);
    assert_eq!(
        tuple_count(ctx, &items, Value::SmallInt(1)).unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        tuple_count(ctx, &items, Value::SmallInt(9)).unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        tuple_index(ctx, &items, Value::SmallInt(2), None, None).unwrap(),
        Value::SmallInt(1)
    );
    assert_eq!(
        tuple_index(
            ctx,
            &items,
            Value::SmallInt(1),
            Some(Value::SmallInt(1)),
            None
        )
        .unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        tuple_index(
            ctx,
            &items,
            Value::SmallInt(1),
            Some(Value::SmallInt(0)),
            Some(Value::SmallInt(2))
        )
        .unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        tuple_index(ctx, &items, Value::SmallInt(9), None, None),
        Err(TupleError::NotInSequence)
    );
    assert_eq!(
        tuple_index(
            ctx,
            &items,
            Value::SmallInt(1),
            Some(Value::SmallInt(2)),
            Some(Value::SmallInt(1))
        ),
        Err(TupleError::NotInSequence)
    );
    // The builtin adapters enforce the arity (2 / 2..4).
    assert_eq!(
        tuple_count_builtin(ctx, &[t]),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        tuple_count_builtin(ctx, &[t, Value::SmallInt(1)]).unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        tuple_index_builtin(ctx, &[t, Value::SmallInt(1)]).unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        tuple_index_builtin(ctx, &[t, Value::SmallInt(1), Value::SmallInt(1)]).unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        tuple_index_builtin(
            ctx,
            &[
                t,
                Value::SmallInt(1),
                Value::SmallInt(0),
                Value::SmallInt(2),
                Value::SmallInt(3),
            ]
        ),
        Err(PyError::PyRaise)
    );
}

#[test]
fn tuple_getiter_iternext() {
    let mut fx = Fixture::new();
    let t = tup(&mut fx, ints(&[10, 20]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let it = tuple_getiter(ctx, t).unwrap();
    assert_eq!(tuple_iternext(ctx, it).unwrap(), Some(Value::SmallInt(10)));
    assert_eq!(tuple_iternext(ctx, it).unwrap(), Some(Value::SmallInt(20)));
    assert_eq!(tuple_iternext(ctx, it).unwrap(), None);
    assert_eq!(tuple_iternext(ctx, it).unwrap(), None);
    // The iterator carries the C9 tag (not a dict-view tag).
    let Value::Heap(r) = it else { panic!("iterator is a heap object") };
    match ctx.arena.get(r) {
        Obj::Iterator(d) => {
            assert_eq!(d.source, t);
            assert_eq!(d.index, 2);
            assert_eq!(
                d.extra,
                vec![Value::SmallInt(
                    crate::objects::containers::sequence::TUPLE_ITER_TAG
                )]
            );
        }
        _ => panic!("not an iterator"),
    }
}

#[test]
fn tuple_type_data_slots() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = tuple_type_data(dict);
    assert_eq!(data.flags, 0);
    assert!(data.unary_op.is_some());
    assert!(data.binary_op.is_some());
    assert!(data.subscr.is_some());
    assert!(data.getiter.is_some());
    assert!(data.print.is_some());
    assert!(data.call.is_none());
    assert!(data.buffer.is_none());
    // The type data is usable: run the print + hash slots.
    let t = tup(&mut fx, ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let f = data.print.unwrap();
    let mut s = String::new();
    f(ctx, t, crate::objects::slots::PrintKind::Repr, &mut s);
    assert_eq!(s, "(1, 2)");
    let h = data.unary_op.unwrap()(ctx, UnaryOp::Hash, t).unwrap();
    // The HASH slot returns the seed-relative value (C12a).
    let seed = fx.types.empty_tuple.0 as i64;
    assert_eq!(h, Value::SmallInt(seed + 3));
}
