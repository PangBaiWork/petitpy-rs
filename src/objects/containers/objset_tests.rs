//! Tests for set/frozenset (C9c: `src/objects/containers/objset.zig`).
//!
//! The reference-binary probes these pin are in the result file
//! (`scripts/containers_c_result.txt`); the notable ones: table
//! order differs between the literal path (`alloc = n`) and the
//! make_new path (`alloc = 0`, rehashing), and the unhashable-insert
//! quirk (`s.add([2])` raises but the element stays in the set).

use super::map_set::set_items;
use super::objset::{set_make_new, set_new, set_print, set_unary_op, SetError};
use super::objset_methods::{
    set_add, set_add_builtin, set_clear_builtin, set_copy_builtin, set_discard,
    set_discard_builtin, set_pop, set_pop_builtin, set_remove, set_remove_builtin,
    set_update_impl,
};
use super::tests::{empty_dict, Fixture};
use crate::memory::gc::CallCtx;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{ListData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

// ── test doubles ────────────────────────────────────────────────────

/// Unary slot for the quirk test type: every op unimplemented, so
/// HASH fails with "unsupported type for operator" (via `hash_of`).
fn quirk_unary_op(_ctx: &mut CallCtx, _op: UnaryOp, _v: Value) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// Whether a value has the quirk test shape: a heap list `[2]`.
/// The quirk objects are all lists `[2]`; no other list `[2]`
/// appears in these tests, so the shape identifies them.
fn is_quirk_shaped(ctx: &CallCtx, v: Value) -> bool {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => l.items.len() == 1 && l.items[0] == Value::SmallInt(2),
            _ => false,
        },
        _ => false,
    }
}

/// Binary slot for the quirk test type: EQUAL is true exactly
/// between two quirk-shaped values, unimplemented otherwise — so a
/// quirk compares equal to another quirk but to nothing else (the
/// Zig's real `[2]` behaves the same way: `[2] == [2]` is True,
/// `[2] == 1` is False).
fn quirk_binary_op(ctx: &mut CallCtx, op: BinaryOp, l: Value, r: Value) -> PyResult<Value> {
    match op {
        BinaryOp::Equal if is_quirk_shaped(ctx, l) && is_quirk_shaped(ctx, r) => {
            Ok(Value::Bool(true))
        }
        _ => Err(PyError::PyRaise),
    }
}

/// A heap object that is unhashable (HASH slot → NULL) but compares
/// equal to every other quirk object (EQUAL slot → True). The
/// discriminating double for the `mp_obj_is_true` pending guard
/// (obj.zig:470): without the guard, a probe would "find" a
/// different quirk object; with it, it never does. Flags `0x0008`
/// (`FLAG_EQ_CHECKS_OTHER_TYPE`) so the cross-type equality check
/// in `obj_equal_not_equal` reaches the slot — two quirk objects
/// have different type objects, so without the flag they would
/// never compare equal at all and the test would not discriminate.
pub(crate) fn quirk_value(fx: &mut Fixture) -> Value {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = TypeData {
        name: static_qstr_id(b"list").expect("list is a static qstr"),
        flags: 0x0008,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(quirk_unary_op),
        binary_op: Some(quirk_binary_op),
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    };
    let t = fx.arena.alloc(None, Obj::Type(data), &fx.roots);
    fx.roots.push(Value::Heap(t));
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(Some(t), Obj::List(ListData { items: vec![Value::SmallInt(2)] })))
}

/// A set from literal items (`mp_obj_new_set`, `alloc = n`).
pub(crate) fn mk_set(fx: &mut Fixture, items: &[Value]) -> Value {
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    set_new(&mut ctx, set_t, items).expect("mk_set")
}

/// A frozenset from literal items.
pub(crate) fn mk_frozenset(fx: &mut Fixture, items: &[Value]) -> Value {
    let fs_t = fx.types.frozenset;
    let mut ctx = fx.ctx();
    set_new(&mut ctx, fs_t, items).expect("mk_frozenset")
}

/// Render a set/frozenset with its print slot.
pub(crate) fn print_of(fx: &mut Fixture, v: Value) -> String {
    let mut ctx = fx.ctx();
    let mut s = String::new();
    set_print(&mut ctx, v, PrintKind::Repr, &mut s);
    s
}

/// Table-order elements of a set.
pub(crate) fn elems_of(fx: &Fixture, v: Value) -> Vec<Value> {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Set(s) => set_items(&s.table),
            _ => panic!("elems_of on a non-set value"),
        },
        _ => panic!("elems_of on a non-heap value"),
    }
}

pub(crate) fn small(v: Value) -> i64 {
    match v {
        Value::SmallInt(n) => n,
        _ => panic!("not a small int: {v:?}"),
    }
}

pub(crate) fn is(v: Value) -> i64 {
    small(v)
}

// ── construction ────────────────────────────────────────────────────

#[test]
fn set_new_table_order_literal() {
    // `mp_obj_new_set(3, …)`: alloc = 3, so 1→slot 1, 2→slot 2,
    // 3→slot 0 — the reference binary prints `{3, 1, 2}`.
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    assert_eq!(print_of(&mut fx, s), "{3, 1, 2}");
    assert_eq!(elems_of(&fx, s), vec![Value::SmallInt(3), Value::SmallInt(1), Value::SmallInt(2)]);
}

#[test]
fn set_make_new_table_order() {
    // make_new builds from alloc = 0 (rehash 0→2→4), so the same
    // elements land in a different order: the reference binary's
    // `frozenset({1,2,3})` prints `{1, 2, 3}`.
    let mut fx = Fixture::new();
    let list = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let s = set_make_new(&mut ctx, set_t, &[list], 0).expect("make_new");
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "{1, 2, 3}");
}

#[test]
fn set_make_new_empty_and_new_empty() {
    let mut fx = Fixture::new();
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let s = set_make_new(&mut ctx, set_t, &[], 0).expect("make_new()");
    let e = set_new(&mut ctx, set_t, &[]).expect("new()");
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "set()");
    assert_eq!(print_of(&mut fx, e), "set()");
    assert_eq!(elems_of(&fx, s), Vec::<Value>::new());
}

#[test]
fn set_make_new_from_str() {
    // The reference binary prints `set("abc")` as `{'a', 'c', 'b'}`.
    let mut fx = Fixture::new();
    let st = fx.heap_str(b"abc");
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let s = set_make_new(&mut ctx, set_t, &[st], 0).expect("make_new");
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "{'a', 'c', 'b'}");
}

#[test]
fn set_make_new_too_many_args() {
    let mut fx = Fixture::new();
    let a = fx.heap_list(vec![Value::SmallInt(1)]);
    let b = fx.heap_list(vec![Value::SmallInt(2)]);
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let e = set_make_new(&mut ctx, set_t, &[a, b], 0).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("argument num/types mismatch"));
}

#[test]
fn set_make_new_not_iterable_overwrites_mismatch() {
    // `set(1,2)`: the arg-check mismatch is recorded, but getiter
    // runs anyway and its error OVERWRITES it (reference binary).
    let mut fx = Fixture::new();
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let e = set_make_new(&mut ctx, set_t, &[Value::SmallInt(1), Value::SmallInt(2)], 0).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("object not iterable"));
}

#[test]
fn set_make_new_not_iterable() {
    let mut fx = Fixture::new();
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let e = set_make_new(&mut ctx, set_t, &[Value::SmallInt(1)], 0).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("object not iterable"));
}

#[test]
fn set_make_new_kw_mismatch() {
    let mut fx = Fixture::new();
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let e = set_make_new(&mut ctx, set_t, &[Value::SmallInt(1), Value::SmallInt(2)], 1).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("argument num/types mismatch"));
}

#[test]
fn set_make_new_unhashable_item() {
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let list = fx.heap_list(vec![q]);
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let e = set_make_new(&mut ctx, set_t, &[list], 0).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
}

// ── add / discard / remove / pop / clear ───────────────────────────

#[test]
fn set_add_and_dedup() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[]);
    let mut ctx = fx.ctx();
    set_add_builtin(&mut ctx, &[s, Value::SmallInt(1)]).expect("add 1");
    set_add_builtin(&mut ctx, &[s, Value::SmallInt(2)]).expect("add 2");
    set_add_builtin(&mut ctx, &[s, Value::SmallInt(1)]).expect("add 1 again");
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "{2, 1}");
}

#[test]
fn set_add_unhashable_quirk() {
    // `s.add([2])` raises, but the element IS in the set afterwards.
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[]);
    let mut ctx = fx.ctx();
    let e = set_add(&mut ctx, s, q).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![q]);
}

#[test]
fn set_update_stops_after_unhashable() {
    // After the first unhashable element the loop stops: later
    // items are never stored (the `mp_iternext` pre-flight).
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[]);
    let list = fx.heap_list(vec![q, Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let e = set_update_impl(&mut ctx, s, &[list]).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![q]);
}

#[test]
fn set_remove_keyerror_overwrites_pending() {
    // The discriminating test for the `mp_obj_is_true` pending
    // guard: `s` holds a quirk object; removing a DIFFERENT quirk
    // object (EQUAL → True) must NOT find it while the hash error
    // is pending — so `remove` raises `KeyError`, not `TypeError`.
    // Without the guard this test fails (the probe finds it).
    let mut fx = Fixture::new();
    let q1 = quirk_value(&mut fx);
    let q2 = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[]);
    let mut ctx = fx.ctx();
    set_add(&mut ctx, s, q1).expect_err("add quirk raises");
    let e = set_remove(&mut ctx, s, q2).unwrap_err();
    assert!(matches!(e, SetError::KeyError(v) if v == q2));
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![q1]);
}

#[test]
fn set_remove_present_and_missing() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let r = set_remove_builtin(&mut ctx, &[s, Value::SmallInt(1)]).expect("remove 1");
    assert_eq!(r, Value::None);
    let e = set_remove_builtin(&mut ctx, &[s, Value::SmallInt(9)]).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![Value::SmallInt(2)]);
}

#[test]
fn set_discard_unhashable_and_missing() {
    // `discard` of an unhashable element raises the hash error
    // (no KeyError overwrite — discard has no miss branch).
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let e = set_discard(&mut ctx, s, q).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
    let r = set_discard_builtin(&mut ctx, &[s, Value::SmallInt(9)]).expect("discard missing");
    assert_eq!(r, Value::None);
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![Value::SmallInt(1)]);
}

#[test]
fn set_pop_table_order() {
    // Literal path (`mp_obj_new_set`, alloc = 5): table [5,1,2,3,4]
    // — the reference binary pops 5, 1, 2, 3, 4.
    let mut fx = Fixture::new();
    let s = mk_set(
        &mut fx,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
            Value::SmallInt(5),
        ],
    );
    let mut ctx = fx.ctx();
    let mut got = Vec::new();
    for _ in 0..5 {
        got.push(set_pop_builtin(&mut ctx, &[s]).expect("pop"));
    }
    drop(ctx);
    assert_eq!(
        got,
        vec![
            Value::SmallInt(5),
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
        ]
    );
    assert_eq!(print_of(&mut fx, s), "set()");
    // make_new path (`set([1,2,3,4,5])`, alloc 0→2→4→6): the
    // reference binary pops 1, 2, 3, 4, 5.
    let list = fx.heap_list(vec![
        Value::SmallInt(1),
        Value::SmallInt(2),
        Value::SmallInt(3),
        Value::SmallInt(4),
        Value::SmallInt(5),
    ]);
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let s2 = set_make_new(&mut ctx, set_t, &[list], 0).expect("make_new");
    let mut got2 = Vec::new();
    for _ in 0..5 {
        got2.push(set_pop_builtin(&mut ctx, &[s2]).expect("pop"));
    }
    drop(ctx);
    assert_eq!(
        got2,
        vec![
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
            Value::SmallInt(5),
        ]
    );
}

#[test]
fn set_pop_empty() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[]);
    let mut ctx = fx.ctx();
    let e = set_pop(&mut ctx, s).unwrap_err();
    assert_eq!(e, SetError::KeyErrorPopEmpty);
    assert_eq!(e.exc_name(), "KeyError");
    assert_eq!(e.msg(), Some("pop from an empty set"));
}

#[test]
fn set_clear() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let r = set_clear_builtin(&mut ctx, &[s]).expect("clear");
    assert_eq!(r, Value::None);
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "set()");
}

#[test]
fn set_copy_preserves_header_type() {
    let mut fx = Fixture::new();
    let fs = mk_frozenset(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let fs2 = set_copy_builtin(&mut ctx, &[fs]).expect("copy frozenset");
    let s2 = set_copy_builtin(&mut ctx, &[s]).expect("copy set");
    drop(ctx);
    assert_eq!(print_of(&mut fx, fs2), print_of(&mut fx, fs));
    assert_eq!(print_of(&mut fx, s2), print_of(&mut fx, s));
    // The copy keeps the header type (frozenset stays frozenset).
    let fs_t = fx.types.frozenset;
    let set_t = fx.types.set;
    let fs2r = match fs2 {
        Value::Heap(r) => r,
        _ => panic!("copy is not heap"),
    };
    let s2r = match s2 {
        Value::Heap(r) => r,
        _ => panic!("copy is not heap"),
    };
    assert_eq!(fx.arena.header(fs2r).type_, Some(fs_t));
    assert_eq!(fx.arena.header(s2r).type_, Some(set_t));
}

// ── unary op: bool / len / hash ─────────────────────────────────────

#[test]
fn frozenset_hash_seed_and_sum() {
    // Seed = the frozenset type's identity word (C12a — the Zig
    // uses the type's address, 22810192 in the reference binary;
    // element hashes wrapping-add). `hash(frozenset())` equals
    // the frozenset *type's* id.
    let mut fx = Fixture::new();
    let seed = fx.types.frozenset.0 as i64;
    let empty = mk_frozenset(&mut fx, &[]);
    let two = mk_frozenset(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let five = mk_frozenset(&mut fx, &[Value::SmallInt(5)]);
    let neg = mk_frozenset(&mut fx, &[Value::SmallInt(-1)]);
    let mut ctx = fx.ctx();
    assert_eq!(is(set_unary_op(&mut ctx, UnaryOp::Hash, empty).expect("hash()")), seed);
    assert_eq!(is(set_unary_op(&mut ctx, UnaryOp::Hash, two).expect("hash({1,2})")), seed + 3);
    assert_eq!(is(set_unary_op(&mut ctx, UnaryOp::Hash, five).expect("hash({5})")), seed + 5);
    assert_eq!(is(set_unary_op(&mut ctx, UnaryOp::Hash, neg).expect("hash({-1})")), seed - 1);
}

#[test]
fn set_hash_not_implemented() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let e = set_unary_op(&mut ctx, UnaryOp::Hash, s).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
}

#[test]
fn set_bool_len() {
    let mut fx = Fixture::new();
    let e = mk_set(&mut fx, &[]);
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    assert_eq!(set_unary_op(&mut ctx, UnaryOp::Bool, e).expect("bool()"), Value::Bool(false));
    assert_eq!(set_unary_op(&mut ctx, UnaryOp::Bool, s).expect("bool(s)"), Value::Bool(true));
    assert_eq!(small(set_unary_op(&mut ctx, UnaryOp::Len, e).expect("len()")), 0);
    assert_eq!(small(set_unary_op(&mut ctx, UnaryOp::Len, s).expect("len(s)")), 2);
}

