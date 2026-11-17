//! Tests for set/frozenset, part 2 (C9c): binary op, subset /
//! superset / disjoint, difference / update / union / intersection /
//! symdiff, iteration, print, tables, type data. Test doubles and
//! helpers live in `objset_tests.rs` (split for the 800-line cap).

use super::objset_tests::{elems_of, mk_frozenset, mk_set, print_of, quirk_value};
use super::objset::{
    frozenset_type_data, set_binary_op, set_getiter, set_it_next, set_make_new, set_type_data,
};
use super::objset_methods::{
    lookup_frozenset_method, lookup_set_method, set_add, set_contains_builtin,
    set_difference_builtin, set_difference_update_builtin, set_diff_impl,
    set_intersection_builtin, set_intersection_update_builtin, set_isdisjoint_builtin,
    set_isdisjoint_impl, set_issubset_builtin, set_issubset_impl, set_issuperset_builtin,
    set_symmetric_difference_builtin, set_symmetric_difference_update_builtin, set_union_builtin,
    set_update_impl, FROZENSET_METHODS, SET_METHODS,
};
use super::tests::{empty_dict, Fixture};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::BinaryOp;
use crate::objects::value::{Obj, Value};
use crate::pyerr::PyError;

// ── binary op ───────────────────────────────────────────────────────

#[test]
fn set_binary_or_preserves_type() {
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let fa = mk_frozenset(&mut fx, &[Value::SmallInt(1)]);
    let fb = mk_frozenset(&mut fx, &[Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let u = set_binary_op(&mut ctx, BinaryOp::Or, a, b).expect("{1}|{2}");
    let fu = set_binary_op(&mut ctx, BinaryOp::Or, fa, fb).expect("fz|fz");
    drop(ctx);
    assert_eq!(print_of(&mut fx, u), "{2, 1}");
    assert_eq!(print_of(&mut fx, fu), "frozenset({2, 1})");
}

#[test]
fn set_binary_and_always_plain_set() {
    // `&` builds a fresh plain `set` even for frozensets.
    let mut fx = Fixture::new();
    let fa = mk_frozenset(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let u = set_binary_op(&mut ctx, BinaryOp::And, fa, b).expect("fz & s");
    drop(ctx);
    assert_eq!(print_of(&mut fx, u), "{2}");
    let set_t = fx.types.set;
    let ur = match u {
        Value::Heap(r) => r,
        _ => panic!("& result is not heap"),
    };
    assert_eq!(fx.arena.header(ur).type_, Some(set_t));
}

#[test]
fn set_binary_inplace_on_frozenset() {
    // In-place ops only mutate an exact `set` lhs: `|=` on a
    // frozenset makes a new frozenset, `&=` makes a plain `set`.
    let mut fx = Fixture::new();
    let fa = mk_frozenset(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let three = mk_set(&mut fx, &[Value::SmallInt(3)]);
    let two = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let u = set_binary_op(&mut ctx, BinaryOp::InplaceOr, fa, three).expect("fz |= {3}");
    let v = set_binary_op(&mut ctx, BinaryOp::InplaceAnd, fa, two).expect("fz &= {2}");
    drop(ctx);
    assert_eq!(print_of(&mut fx, u), "frozenset({1, 2, 3})");
    assert_eq!(print_of(&mut fx, v), "{2}");
    // The original frozenset is untouched by `|=`.
    assert_eq!(print_of(&mut fx, fa), "frozenset({2, 1})");
}

#[test]
fn set_binary_subtract_xor() {
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let c = mk_set(&mut fx, &[Value::SmallInt(2), Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let d = set_binary_op(&mut ctx, BinaryOp::Subtract, a, b).expect("{1,2}-{2}");
    let x = set_binary_op(&mut ctx, BinaryOp::Xor, a, c).expect("{1,2}^{2,3}");
    drop(ctx);
    assert_eq!(print_of(&mut fx, d), "{1}");
    assert_eq!(print_of(&mut fx, x), "{3, 1}");
}

#[test]
fn set_binary_contains_and_quirk() {
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let sq = mk_set(&mut fx, &[]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Contains, s, Value::SmallInt(1)).expect("1 in s"),
        Value::Bool(true)
    );
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Contains, s, Value::SmallInt(3)).expect("3 in s"),
        Value::Bool(false)
    );
    // `[2] in {[2]}` → the hash `TypeError` (pending wins over False).
    set_add(&mut ctx, sq, q).expect_err("add quirk raises");
    let e = set_binary_op(&mut ctx, BinaryOp::Contains, sq, q).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
}

#[test]
fn set_binary_or_nonset_rhs() {
    // Non-set rhs (except CONTAINS) → NULL (`Err(PyRaise)`).
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let list = fx.heap_list(vec![Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let e = set_binary_op(&mut ctx, BinaryOp::Or, s, list).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
}

#[test]
fn set_binary_equal_frozenset() {
    // `set == frozenset` with equal elements is True (no type check).
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let fa = mk_frozenset(&mut fx, &[Value::SmallInt(1)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Equal, a, fa).expect("s == fz"),
        Value::Bool(true)
    );
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Equal, a, b).expect("s == s2"),
        Value::Bool(false)
    );
}

#[test]
fn set_binary_proper_subset() {
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Less, a, b).expect("a < b"),
        Value::Bool(true)
    );
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::Less, b, b).expect("b < b"),
        Value::Bool(false)
    );
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::LessEqual, b, b).expect("b <= b"),
        Value::Bool(true)
    );
    assert_eq!(
        set_binary_op(&mut ctx, BinaryOp::More, b, a).expect("b > a"),
        Value::Bool(true)
    );
}

// ── subset / superset / disjoint ────────────────────────────────────

#[test]
fn set_issubset_from_list() {
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let ab = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
    let just_a = fx.heap_list(vec![Value::SmallInt(1)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_issubset_builtin(&mut ctx, &[a, ab]).expect("subset"),
        Value::Bool(true)
    );
    assert_eq!(
        set_issubset_builtin(&mut ctx, &[b, just_a]).expect("not subset"),
        Value::Bool(false)
    );
}

#[test]
fn set_issubset_unhashable_and_noniterable_arg() {
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let bad = fx.heap_list(vec![q]);
    let mut ctx = fx.ctx();
    let e = set_issubset_impl(&mut ctx, a, bad, false).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
    let e2 = set_issubset_impl(&mut ctx, a, Value::SmallInt(1), false).unwrap_err();
    assert_eq!(e2.exc_name(), "TypeError");
    assert_eq!(e2.msg(), Some("object not iterable"));
}

#[test]
fn set_issuperset_swapped() {
    let mut fx = Fixture::new();
    let ab = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let just_a = fx.heap_list(vec![Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_issuperset_builtin(&mut ctx, &[ab, just_a]).expect("superset"),
        Value::Bool(true)
    );
}

#[test]
fn set_isdisjoint_cases() {
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let a = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let two = fx.heap_list(vec![Value::SmallInt(2)]);
    let one = fx.heap_list(vec![Value::SmallInt(1)]);
    let bad = fx.heap_list(vec![q]);
    let mut ctx = fx.ctx();
    assert_eq!(
        set_isdisjoint_builtin(&mut ctx, &[a, two]).expect("disjoint"),
        Value::Bool(true)
    );
    assert_eq!(
        set_isdisjoint_builtin(&mut ctx, &[a, one]).expect("not disjoint"),
        Value::Bool(false)
    );
    let e = set_isdisjoint_impl(&mut ctx, a, bad).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("unsupported type for operator"));
}

// ── difference / update / union / intersection / symdiff ───────────

#[test]
fn set_difference_multi_and_noop() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    let one = fx.heap_list(vec![Value::SmallInt(1)]);
    let two = fx.heap_list(vec![Value::SmallInt(2)]);
    let mut ctx = fx.ctx();
    let d = set_difference_builtin(&mut ctx, &[s, one, two]).expect("diff [1],[2]");
    let n = set_difference_builtin(&mut ctx, &[s]).expect("diff()");
    drop(ctx);
    assert_eq!(print_of(&mut fx, d), "{3}");
    assert_eq!(print_of(&mut fx, n), print_of(&mut fx, s));
}

#[test]
fn set_difference_update_overwrite() {
    // A later getiter failure OVERWRITES the earlier hash pending.
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let bad = fx.heap_list(vec![q]);
    let mut ctx = fx.ctx();
    let e = set_diff_impl(&mut ctx, s, &[bad, Value::SmallInt(1)], true).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("object not iterable"));
}

#[test]
fn set_difference_update_identity_clear() {
    // `self == other` (identity) clears, even with pending set.
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let one = fx.heap_list(vec![Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let r = set_difference_update_builtin(&mut ctx, &[s, one, s]).expect("diff_update");
    assert_eq!(r, Value::None);
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "set()");
}

#[test]
fn set_update_overwrite() {
    // `s.update([[]], 1)` → 'object not iterable' with `s == {[], 1}`.
    let mut fx = Fixture::new();
    let q = quirk_value(&mut fx);
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let bad = fx.heap_list(vec![q]);
    let mut ctx = fx.ctx();
    let e = set_update_impl(&mut ctx, s, &[bad, Value::SmallInt(1)]).unwrap_err();
    assert_eq!(e.exc_name(), "TypeError");
    assert_eq!(e.msg(), Some("object not iterable"));
    drop(ctx);
    assert_eq!(elems_of(&fx, s), vec![q, Value::SmallInt(1)]);
}

#[test]
fn set_union_arity() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let o = mk_set(&mut fx, &[Value::SmallInt(2)]);
    let p = mk_set(&mut fx, &[Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let e = set_union_builtin(&mut ctx, &[s, o, p]).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
}

#[test]
fn set_intersection_identity_and_update() {
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let other = mk_set(&mut fx, &[Value::SmallInt(2), Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let c = set_intersection_builtin(&mut ctx, &[s, s]).expect("intersect self");
    drop(ctx);
    assert_eq!(print_of(&mut fx, c), "{2, 1}");
    let mut ctx = fx.ctx();
    let r = set_intersection_update_builtin(&mut ctx, &[s, other]).expect("intersect_update");
    assert_eq!(r, Value::None);
    drop(ctx);
    assert_eq!(print_of(&mut fx, s), "{2}");
}

#[test]
fn set_symdiff_and_update() {
    let mut fx = Fixture::new();
    let a = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    let b = mk_set(&mut fx, &[Value::SmallInt(2), Value::SmallInt(3)]);
    let mut ctx = fx.ctx();
    let d = set_symmetric_difference_builtin(&mut ctx, &[a, b]).expect("symdiff");
    let r = set_symmetric_difference_update_builtin(&mut ctx, &[a, b]).expect("symdiff_update");
    assert_eq!(r, Value::None);
    drop(ctx);
    assert_eq!(print_of(&mut fx, d), "{3, 1}");
    assert_eq!(print_of(&mut fx, a), "{3, 1}");
}

// ── iteration / print / tables / type data ──────────────────────────

#[test]
fn set_getiter_table_order_and_exhaustion() {
    let mut fx = Fixture::new();
    let list = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    let set_t = fx.types.set;
    let mut ctx = fx.ctx();
    let s = set_make_new(&mut ctx, set_t, &[list], 0).expect("make_new");
    let it = set_getiter(&mut ctx, s).expect("getiter");
    let mut got = Vec::new();
    loop {
        match set_it_next(&mut ctx, it).expect("next") {
            Some(v) => got.push(v),
            None => break,
        }
    }
    drop(ctx);
    assert_eq!(
        got,
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
    );
}

#[test]
fn set_print_frozenset_forms() {
    let mut fx = Fixture::new();
    let fe = mk_frozenset(&mut fx, &[]);
    let f2 = mk_frozenset(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2)]);
    assert_eq!(print_of(&mut fx, fe), "frozenset()");
    // Literal path, alloc = 2: table [2, 1] — the reference binary
    // prints `frozenset({1,2})` (the make_new path) the same way.
    assert_eq!(print_of(&mut fx, f2), "frozenset({2, 1})");
}

#[test]
fn method_table_counts_and_lookup() {
    assert_eq!(SET_METHODS.len(), 18);
    assert_eq!(FROZENSET_METHODS.len(), 9);
    assert!(lookup_set_method("add").is_some());
    assert!(lookup_set_method("update").is_some());
    assert!(lookup_set_method("__contains__").is_some());
    assert!(lookup_set_method("nosuch").is_none());
    assert!(lookup_frozenset_method("add").is_none());
    assert!(lookup_frozenset_method("copy").is_some());
    assert!(lookup_frozenset_method("union").is_some());
    assert!(lookup_frozenset_method("__contains__").is_some());
}

#[test]
fn type_data_flags_and_slots() {
    let mut fx = Fixture::new();
    let d = empty_dict(&mut fx.arena, &fx.roots);
    let sd = set_type_data(d);
    let fd = frozenset_type_data(d);
    assert_eq!(sd.name, static_qstr_id(b"set").expect("set is static"));
    assert_eq!(fd.name, static_qstr_id(b"frozenset").expect("frozenset is static"));
    assert_eq!(sd.flags, 0x0000);
    assert_eq!(fd.flags, 0x0008);
    assert!(sd.unary_op.is_some());
    assert!(sd.binary_op.is_some());
    assert!(sd.getiter.is_some());
    assert!(sd.print.is_some());
    assert!(sd.subscr.is_none());
    assert!(sd.call.is_none());
    assert!(sd.buffer.is_none());
    assert!(fd.unary_op.is_some());
    assert!(fd.binary_op.is_some());
}

#[test]
fn set_new_alloc_is_n() {
    // `mp_obj_new_set(n, items)` sets `alloc = n` exactly.
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    match s {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Set(d) => assert_eq!(d.table.alloc, 3),
            _ => panic!("not a set"),
        },
        _ => panic!("not heap"),
    }
}

#[test]
fn set_contains_builtin_adapter() {
    // `__contains__` routes to the CONTAINS binary op.
    let mut fx = Fixture::new();
    let s = mk_set(&mut fx, &[Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let t = set_contains_builtin(&mut ctx, &[s, Value::SmallInt(1)]).expect("contains");
    assert_eq!(t, Value::Bool(true));
    let f = set_contains_builtin(&mut ctx, &[s, Value::SmallInt(2)]).expect("not contains");
    assert_eq!(f, Value::Bool(false));
}
