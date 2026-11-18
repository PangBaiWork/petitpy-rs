//! Tests for `objlist.rs` + `objlist_methods.rs`.
//!
//! Reference-binary probe values (2026-11-17, `/tmp/c9b_probe*.py`):
//! - `list(5)` → "object not iterable"; `list("ab")` → `['a',
//!   'b']`; `tuple(t) is t` but `list(l) is l` is False (no
//!   exact-list fast path — a fresh list is built).
//! - `insert(False, x)` lands at index 7, `insert(True, x)`
//!   at 15, `insert(None, x)` at 3, `insert(2**70, x)` and
//!   `insert(1.5, x)` append (the raw REPR_A word).
//! - `remove` on a miss leaves the list unchanged.
//! - Sort is IN PLACE with the polled-sentinel model: a mixed
//!   sort raises the FIRST comparison failure AFTER the sort
//!   finishes, leaving the list partially sorted:
//!   `[3, "a", 1, 2]` → `TypeError('str', 'int')`, list
//!   `[1, 2, 'a', 3]`; `[3, 1, "a", 2]` → `TypeError('int',
//!   'str')`, list `[1, 'a', 2, 3]`; `[2, "a", 1]` → `['a',
//!   1, 2]`; `[2, 1, "a"]` → `[1, 'a', 2]`.
//! - `sort(key=len)` on ints ABORTS the reference (rc=134,
//!   `fromPtr(NULL)`); Rust returns `PyRaise` instead.
//!
//! Discipline: build every fixture value first, then create the
//! `CallCtx` — a live `ctx` borrows the fixture mutably, so no
//! `fx.heap_*` call may follow it in the same scope.

use super::objlist::*;
use super::objlist_methods::*;
use super::tests::{empty_dict, Fixture};
use crate::objects::obj::ObjError;
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::value::{Obj, SliceData, Value};
use crate::vendor::mpz::BigInt;

fn ints(items: &[i64]) -> Vec<Value> {
    items.iter().map(|&n| Value::SmallInt(n)).collect()
}

fn lst(fx: &mut Fixture, items: Vec<Value>) -> Value {
    fx.heap_list(items)
}

/// A heap int `n << shift` (for out-of-window values like 2**70).
fn heap_big(fx: &mut Fixture, n: i64, shift: u32) -> Value {
    let t = fx.types.int;
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(Some(t), Obj::Int(BigInt::from_int(n) << (shift as u64))))
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

fn items_of(ctx: &crate::memory::gc::CallCtx, list: Value) -> Vec<Value> {
    list_get(ctx, list).unwrap()
}

fn print_to_string(ctx: &mut crate::memory::gc::CallCtx, v: Value) -> String {
    let mut s = String::new();
    list_print(ctx, v, PrintKind::Repr, &mut s);
    s
}

#[test]
fn list_make_new_basics() {
    let mut fx = Fixture::new();
    let t = fx.types.list;
    let l = lst(&mut fx, ints(&[1, 2]));
    let s = fx.heap_str(b"ab");
    let d = fx.dict();
    let a_key = fx.heap_str(b"a");
    fx.store(d, a_key, Value::SmallInt(1));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `list()` → empty.
    let out = list_make_new(ctx, t, &[], 0, None).unwrap();
    assert_eq!(items_of(ctx, out), &[] as &[Value]);
    // `list([1, 2])` → [1, 2] (a FRESH list — no exact-list
    // fast path, unlike tuple).
    let out = list_make_new(ctx, t, &[l], 0, None).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[1, 2]));
    assert_ne!(out, l);
    // `list("ab")` → ['a', 'b'] — heap `Str` chars.
    let out = list_make_new(ctx, t, &[s], 0, None).unwrap();
    let got = items_of(ctx, out);
    assert_eq!(got.len(), 2);
    assert!(matches!(got[0], Value::Heap(_)) && matches!(got[1], Value::Heap(_)));
    // `list({"a": 1})` → ['a'] — dict iterates its keys.
    let out = list_make_new(ctx, t, &[d], 0, None).unwrap();
    assert_eq!(items_of(ctx, out), vec![a_key]);
}

#[test]
fn list_make_new_sig_error_ordering() {
    let mut fx = Fixture::new();
    let t = fx.types.list;
    let l = lst(&mut fx, ints(&[1]));
    let a = fx.heap_str(b"a");
    let b = fx.heap_str(b"b");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `list(1, 2)` → "object not iterable" (the `getiter`
    // failure overwrites the recorded sig error).
    assert_eq!(
        list_make_new(ctx, t, &[Value::SmallInt(1), Value::SmallInt(2)], 0, None),
        Err(ListError::NotIterable)
    );
    // `list([1], 99)` → "argument num/types mismatch" (the
    // extend succeeds, so the sig error survives).
    assert_eq!(
        list_make_new(ctx, t, &[l, Value::SmallInt(99)], 0, None),
        Err(ListError::ArgNumTypesMismatch)
    );
    // `list("a", "b")` → "argument num/types mismatch".
    assert_eq!(
        list_make_new(ctx, t, &[a, b], 0, None),
        Err(ListError::ArgNumTypesMismatch)
    );
    // `list(5)` → "object not iterable".
    assert_eq!(
        list_make_new(ctx, t, &[Value::SmallInt(5)], 0, None),
        Err(ListError::NotIterable)
    );
    // kwargs with no positional args → arg mismatch.
    assert_eq!(
        list_make_new(ctx, t, &[], 1, None),
        Err(ListError::ArgNumTypesMismatch)
    );
}

#[test]
fn list_append_guard() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1]));
    let tup = fx.heap_tuple(ints(&[1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // The ADR 012 self-type guard: only exact lists append.
    assert_eq!(
        list_append(ctx, Value::SmallInt(1), Value::SmallInt(2)),
        Err(ListError::SelfType)
    );
    assert_eq!(
        list_append(ctx, tup, Value::SmallInt(2)),
        Err(ListError::SelfType)
    );
    assert_eq!(
        list_append(ctx, l, Value::SmallInt(2)),
        Ok(Value::None)
    );
    assert_eq!(items_of(ctx, l), ints(&[1, 2]));
}

#[test]
fn list_extend_paths() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let other = lst(&mut fx, ints(&[3, 4]));
    let s = fx.heap_str(b"ab");
    let d = fx.dict();
    let a_key = fx.heap_str(b"a");
    fx.store(d, a_key, Value::SmallInt(1));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Exact-list bulk copy.
    list_extend(ctx, l, other, None).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[1, 2, 3, 4]));
    // Self-extend doubles (the snapshot is cloned first).
    list_extend(ctx, l, l, None).unwrap();
    assert_eq!(
        items_of(ctx, l),
        ints(&[1, 2, 3, 4, 1, 2, 3, 4])
    );
    // Extend from a str appends its chars.
    let l2 = list_make_new(ctx, ctx.types.list, &[], 0, None).unwrap();
    list_extend(ctx, l2, s, None).unwrap();
    let got = items_of(ctx, l2);
    assert_eq!(got.len(), 2);
    // Extend from a dict appends its keys.
    let l3 = list_make_new(ctx, ctx.types.list, &[], 0, None).unwrap();
    list_extend(ctx, l3, d, None).unwrap();
    assert_eq!(items_of(ctx, l3), vec![a_key]);
    // Non-iterable → NotIterable.
    assert_eq!(
        list_extend(ctx, l, Value::SmallInt(5), None),
        Err(ListError::NotIterable)
    );
}

#[test]
fn list_clear_copy() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let c = list_copy(ctx, l).unwrap();
    assert_eq!(items_of(ctx, c), ints(&[1, 2]));
    assert_ne!(c, l);
    list_clear(ctx, l).unwrap();
    assert_eq!(items_of(ctx, l), &[] as &[Value]);
    // The copy is unaffected by the clear.
    assert_eq!(items_of(ctx, c), ints(&[1, 2]));
}

#[test]
fn list_count_index() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        list_count(ctx, l, Value::SmallInt(1)).unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        list_count(ctx, l, Value::SmallInt(9)).unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        list_index(ctx, l, Value::SmallInt(2), None, None).unwrap(),
        Value::SmallInt(1)
    );
    assert_eq!(
        list_index(
            ctx,
            l,
            Value::SmallInt(1),
            Some(Value::SmallInt(1)),
            None
        )
        .unwrap(),
        Value::SmallInt(2)
    );
    assert_eq!(
        list_index(
            ctx,
            l,
            Value::SmallInt(1),
            Some(Value::SmallInt(0)),
            Some(Value::SmallInt(2))
        )
        .unwrap(),
        Value::SmallInt(0)
    );
    // start/stop clamp like a slice.
    assert_eq!(
        list_index(
            ctx,
            l,
            Value::SmallInt(1),
            Some(Value::SmallInt(-5)),
            Some(Value::SmallInt(5))
        )
        .unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        list_index(ctx, l, Value::SmallInt(9), None, None),
        Err(ListError::NotInSequence)
    );
    assert_eq!(
        list_index(
            ctx,
            l,
            Value::SmallInt(1),
            Some(Value::SmallInt(2)),
            Some(Value::SmallInt(1))
        ),
        Err(ListError::NotInSequence)
    );
}

#[test]
fn list_insert_raw_word() {
    let mut fx = Fixture::new();
    let big = heap_big(&mut fx, 1, 70);
    let base: Vec<Value> = (0..20).map(Value::SmallInt).collect();
    let l = lst(&mut fx, base.clone());
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `False` → word 14 >> 1 = 7, `True` → 30 >> 1 = 15,
    // `None` → 6 >> 1 = 3.
    list_insert(ctx, l, Value::Bool(false), Value::SmallInt(99)).unwrap();
    assert_eq!(items_of(ctx, l)[7], Value::SmallInt(99));
    list_insert(ctx, l, Value::Bool(true), Value::SmallInt(88)).unwrap();
    assert_eq!(items_of(ctx, l)[15], Value::SmallInt(88));
    list_insert(ctx, l, Value::None, Value::SmallInt(77)).unwrap();
    assert_eq!(items_of(ctx, l)[3], Value::SmallInt(77));
    // A huge index appends.
    list_insert(ctx, l, big, Value::SmallInt(66)).unwrap();
    let items = items_of(ctx, l);
    assert_eq!(*items.last().unwrap(), Value::SmallInt(66));
    // A very negative index inserts at the front.
    list_insert(ctx, l, Value::SmallInt(-100), Value::SmallInt(55)).unwrap();
    assert_eq!(items_of(ctx, l)[0], Value::SmallInt(55));
}

#[test]
fn list_pop_remove() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 3]));
    let empty = lst(&mut fx, vec![]);
    let l2 = lst(&mut fx, ints(&[1, 2, 3]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(list_pop(ctx, l, None).unwrap(), Value::SmallInt(3));
    assert_eq!(
        list_pop(ctx, l, Some(Value::SmallInt(0))).unwrap(),
        Value::SmallInt(1)
    );
    assert_eq!(items_of(ctx, l), ints(&[2]));
    assert_eq!(
        list_pop(ctx, l, Some(Value::SmallInt(5))),
        Err(ListError::Obj(ObjError::IndexOutOfRange))
    );
    assert_eq!(list_pop(ctx, empty, None), Err(ListError::PopEmpty));
    // `remove` on a miss leaves the list unchanged.
    assert_eq!(
        list_remove(ctx, l2, Value::SmallInt(9)),
        Err(ListError::NotInSequence)
    );
    assert_eq!(items_of(ctx, l2), ints(&[1, 2, 3]));
    list_remove(ctx, l2, Value::SmallInt(2)).unwrap();
    assert_eq!(items_of(ctx, l2), ints(&[1, 3]));
}

#[test]
fn list_reverse_works() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 3]));
    let empty = lst(&mut fx, vec![]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    list_reverse(ctx, l).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[3, 2, 1]));
    list_reverse(ctx, empty).unwrap();
    assert_eq!(items_of(ctx, empty), &[] as &[Value]);
}

#[test]
fn list_sort_plain() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[3, 1, 2]));
    let dups = lst(&mut fx, ints(&[3, 1, 2, 1, 3]));
    let big: Vec<Value> = (1..=20).rev().map(Value::SmallInt).collect();
    let big_l = lst(&mut fx, big);
    let one = lst(&mut fx, ints(&[1]));
    let empty = lst(&mut fx, vec![]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    list_sort(ctx, l, None, false, None).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[1, 2, 3]));
    list_sort(ctx, l, None, true, None).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[3, 2, 1]));
    list_sort(ctx, dups, None, false, None).unwrap();
    assert_eq!(items_of(ctx, dups), ints(&[1, 1, 2, 3, 3]));
    list_sort(ctx, big_l, None, false, None).unwrap();
    let expect: Vec<Value> = (1..=20).map(Value::SmallInt).collect();
    assert_eq!(items_of(ctx, big_l), expect);
    // len <= 1 is a no-op.
    list_sort(ctx, one, None, false, None).unwrap();
    list_sort(ctx, empty, None, true, None).unwrap();
    assert_eq!(items_of(ctx, one), ints(&[1]));
    assert_eq!(items_of(ctx, empty), &[] as &[Value]);
}

#[test]
fn list_sort_reverse_kw() {
    let mut fx = Fixture::new();
    let rev_q = crate::objects::primitives::ops::static_qstr_id(b"reverse").unwrap();
    let yes = fx.heap_str(b"yes");
    let l1 = lst(&mut fx, ints(&[2, 1]));
    let l2 = lst(&mut fx, ints(&[2, 1]));
    let l3 = lst(&mut fx, ints(&[2, 1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `reverse=1` → reversed; `reverse=0` → plain.
    list_sort_builtin(ctx, &[l1], &[KwArg { name: rev_q, value: Value::SmallInt(1) }], None)
        .unwrap();
    assert_eq!(items_of(ctx, l1), ints(&[2, 1]));
    list_sort_builtin(ctx, &[l2], &[KwArg { name: rev_q, value: Value::SmallInt(0) }], None)
        .unwrap();
    assert_eq!(items_of(ctx, l2), ints(&[1, 2]));
    // `reverse="yes"` → truthy → reversed (probed).
    list_sort_builtin(ctx, &[l3], &[KwArg { name: rev_q, value: yes }], None).unwrap();
    assert_eq!(items_of(ctx, l3), ints(&[2, 1]));
}

#[test]
fn list_sort_mixed_partial() {
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let l1 = lst(&mut fx, vec![Value::SmallInt(3), a, Value::SmallInt(1), Value::SmallInt(2)]);
    let l2 = lst(&mut fx, vec![Value::SmallInt(3), Value::SmallInt(1), a, Value::SmallInt(2)]);
    let l3 = lst(&mut fx, vec![Value::SmallInt(2), a, Value::SmallInt(1)]);
    let l4 = lst(&mut fx, vec![Value::SmallInt(2), Value::SmallInt(1), a]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // The FIRST failed comparison wins; the sort runs to
    // completion and the list is left partially sorted.
    let err = list_sort(ctx, l1, None, false, None).unwrap_err();
    assert_eq!(
        err,
        ListError::CompareUnsupported { lhs: "str".to_string(), rhs: "int".to_string() }
    );
    assert_eq!(
        err.message_string().as_deref(),
        Some("unsupported types for __lt__: 'str', 'int'")
    );
    assert_eq!(
        items_of(ctx, l1),
        vec![Value::SmallInt(1), Value::SmallInt(2), a, Value::SmallInt(3)]
    );
    assert_eq!(
        list_sort(ctx, l2, None, false, None),
        Err(ListError::CompareUnsupported { lhs: "int".to_string(), rhs: "str".to_string() })
    );
    assert_eq!(
        items_of(ctx, l2),
        vec![Value::SmallInt(1), a, Value::SmallInt(2), Value::SmallInt(3)]
    );
    assert_eq!(
        list_sort(ctx, l3, None, false, None),
        Err(ListError::CompareUnsupported { lhs: "int".to_string(), rhs: "str".to_string() })
    );
    assert_eq!(
        items_of(ctx, l3),
        vec![a, Value::SmallInt(1), Value::SmallInt(2)]
    );
    assert_eq!(
        list_sort(ctx, l4, None, false, None),
        Err(ListError::CompareUnsupported { lhs: "int".to_string(), rhs: "str".to_string() })
    );
    assert_eq!(
        items_of(ctx, l4),
        vec![Value::SmallInt(1), a, Value::SmallInt(2)]
    );
}

#[test]
fn list_sort_key_deferred_and_bad_args() {
    let mut fx = Fixture::new();
    let key_q = crate::objects::primitives::ops::static_qstr_id(b"key").unwrap();
    let foo_q = crate::objects::primitives::ops::static_qstr_id(b"keys").unwrap();
    let l = lst(&mut fx, ints(&[2, 1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Any non-`None` key is `PyRaise` for now (the VM call
    // machinery is C13/C14's; the reference ABORTS when the
    // key call fails, so a clean error is the intended
    // divergence — see the `list_sort` docs).
    assert_eq!(
        list_sort(ctx, l, Some(l), false, None),
        Err(ListError::PyRaise)
    );
    // `sort(foo=1)` / `sort(1, 2, 3)` → arg mismatch.
    assert_eq!(
        list_sort_builtin(ctx, &[l], &[KwArg { name: foo_q, value: Value::SmallInt(1) }], None)
            .map_err(|_| ListError::ArgNumTypesMismatch),
        Err(ListError::ArgNumTypesMismatch)
    );
    assert_eq!(
        list_sort_builtin(
            ctx,
            &[l, Value::SmallInt(1), Value::SmallInt(2)],
            &[],
            None
        )
        .map_err(|_| ListError::ArgNumTypesMismatch),
        Err(ListError::ArgNumTypesMismatch)
    );
    // `key=None` sorts normally.
    list_sort_builtin(ctx, &[l], &[KwArg { name: key_q, value: Value::None }], None).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[1, 2]));
}

#[test]
fn list_slice_load_store() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 3, 4, 5]));
    let s13 = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(3)), None);
    let s_step2 = slice_val(&mut fx, None, None, Some(Value::SmallInt(2)));
    let s_rev = slice_val(&mut fx, None, None, Some(Value::SmallInt(-1)));
    let s_step0 = slice_val(&mut fx, None, None, Some(Value::SmallInt(0)));
    let s12 = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(2)), None);
    let s13b = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(3)), None);
    let rhs = lst(&mut fx, ints(&[9, 8, 7, 6]));
    let rhs2 = lst(&mut fx, ints(&[9, 9]));
    let l2 = lst(&mut fx, ints(&[1, 2, 3]));
    let l3 = lst(&mut fx, ints(&[1, 2, 3, 4]));
    let l4 = lst(&mut fx, ints(&[1, 2, 3]));
    let l5 = lst(&mut fx, ints(&[1, 2, 3]));
    let l6 = lst(&mut fx, ints(&[1, 2, 3]));
    let empty_rhs = lst(&mut fx, vec![]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Load: fast slice, step slice (via `extract_slice`),
    // reversed slice.
    let out = list_subscr(ctx, l, s13, SubscrArg::Get).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[2, 3]));
    let out = list_subscr(ctx, l, s_step2, SubscrArg::Get).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[1, 3, 5]));
    let out = list_subscr(ctx, l, s_rev, SubscrArg::Get).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[5, 4, 3, 2, 1]));
    // `[::0]` → ValueError "slice step can't be zero".
    assert_eq!(
        list_subscr(ctx, l, s_step0, SubscrArg::Get).map_err(|_| ()),
        Err(())
    );
    // Store (grow): `[1, 2, 3][1:2] = [9, 8, 7, 6]`.
    list_subscr(ctx, l2, s12, SubscrArg::Set(rhs)).unwrap();
    assert_eq!(items_of(ctx, l2), ints(&[1, 9, 8, 7, 6, 3]));
    // Store (shrink): `[1, 2, 3, 4][1:3] = []`.
    list_subscr(ctx, l3, s13b, SubscrArg::Set(empty_rhs)).unwrap();
    assert_eq!(items_of(ctx, l3), ints(&[1, 4]));
    // Delete: `del l[1:2]`.
    list_subscr(ctx, l4, s12, SubscrArg::Delete).unwrap();
    assert_eq!(items_of(ctx, l4), ints(&[1, 3]));
    // Non-step-1 store → bare `NotImplementedError`.
    assert_eq!(
        list_subscr(ctx, l5, s_step2, SubscrArg::Set(rhs2)),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // Non-tuple/list rhs → "expected tuple/list".
    assert_eq!(
        list_subscr(ctx, l6, s12, SubscrArg::Set(Value::SmallInt(5)))
            .map_err(|_| ListError::ExpectedTupleList),
        Err(ListError::ExpectedTupleList)
    );
}

#[test]
fn list_slice_step_zero_error() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 3]));
    let s_step0 = slice_val(&mut fx, None, None, Some(Value::SmallInt(0)));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // List keeps "slice step can't be zero" (unlike tuple,
    // where the step-1 error overwrites it — probed).
    let err = list_slice_load(ctx, ctx.types.list, &items_of(ctx, l), &SliceData {
        start: None,
        stop: None,
        step: Some(Value::SmallInt(0)),
    });
    assert_eq!(err, Err(ListError::SliceStepZero));
    assert_eq!(
        list_subscr(ctx, l, s_step0, SubscrArg::Get).map_err(|_| ()),
        Err(())
    );
}

#[test]
fn list_subscr_get_set_delete() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[10, 20, 30]));
    let a = fx.heap_str(b"a");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        list_subscr(ctx, l, Value::SmallInt(1), SubscrArg::Get).unwrap(),
        Value::SmallInt(20)
    );
    assert_eq!(
        list_subscr(ctx, l, Value::Bool(true), SubscrArg::Get).unwrap(),
        Value::SmallInt(20)
    );
    assert_eq!(
        list_subscr(ctx, l, Value::SmallInt(-1), SubscrArg::Get).unwrap(),
        Value::SmallInt(30)
    );
    list_subscr(ctx, l, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(9))).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[9, 20, 30]));
    list_subscr(ctx, l, Value::SmallInt(1), SubscrArg::Delete).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[9, 30]));
    assert_eq!(
        list_subscr(ctx, l, Value::SmallInt(5), SubscrArg::Get)
            .map_err(|_| ListError::Obj(ObjError::IndexOutOfRange)),
        Err(ListError::Obj(ObjError::IndexOutOfRange))
    );
    assert_eq!(
        list_subscr(ctx, l, a, SubscrArg::Get)
            .map_err(|_| ListError::Obj(ObjError::IndicesMustBeInts)),
        Err(ListError::Obj(ObjError::IndicesMustBeInts))
    );
}

#[test]
fn list_iter() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let empty = lst(&mut fx, vec![]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let it = list_getiter(ctx, l).unwrap();
    assert_eq!(list_iternext(ctx, it).unwrap(), Some(Value::SmallInt(1)));
    assert_eq!(list_iternext(ctx, it).unwrap(), Some(Value::SmallInt(2)));
    assert_eq!(list_iternext(ctx, it).unwrap(), None);
    let it2 = list_getiter(ctx, empty).unwrap();
    assert_eq!(list_iternext(ctx, it2).unwrap(), None);
}

#[test]
fn list_print_repr() {
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let l = lst(&mut fx, vec![Value::SmallInt(1), a]);
    let nested_t = fx.heap_tuple(ints(&[3]));
    let inner = lst(&mut fx, ints(&[1, 2]));
    let nested = lst(&mut fx, vec![inner, nested_t]);
    let empty = lst(&mut fx, vec![]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(print_to_string(ctx, l), "[1, 'a']");
    // The fixture now carries the real list type (C9b), so the
    // inner list prints recursively.
    assert_eq!(print_to_string(ctx, nested), "[[1, 2], (3,)]");
    assert_eq!(print_to_string(ctx, empty), "[]");
}

#[test]
fn list_unary_binary_slots() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let empty = lst(&mut fx, vec![]);
    let other = lst(&mut fx, ints(&[3]));
    let tup = fx.heap_tuple(ints(&[3]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(list_unary_op(ctx, UnaryOp::Bool, l).unwrap(), Value::Bool(true));
    assert_eq!(list_unary_op(ctx, UnaryOp::Bool, empty).unwrap(), Value::Bool(false));
    assert_eq!(list_unary_op(ctx, UnaryOp::Len, l).unwrap(), Value::SmallInt(2));
    assert_eq!(
        list_unary_op(ctx, UnaryOp::Hash, l),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // ADD concatenates exact lists only.
    let out = list_binary_op(ctx, BinaryOp::Add, l, other).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[1, 2, 3]));
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Add, l, tup),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // MULTIPLY repeats; n <= 0 → empty.
    let out = list_binary_op(ctx, BinaryOp::Multiply, l, Value::SmallInt(2)).unwrap();
    assert_eq!(items_of(ctx, out), ints(&[1, 2, 1, 2]));
    let out = list_binary_op(ctx, BinaryOp::Multiply, l, Value::SmallInt(-1)).unwrap();
    assert_eq!(items_of(ctx, out), &[] as &[Value]);
    // A non-int multiplier → `PyRaise`.
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Multiply, l, Value::None),
        Err(crate::pyerr::PyError::PyRaise)
    );
}

#[test]
fn list_comparisons() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let same = lst(&mut fx, ints(&[1, 2]));
    let bigger = lst(&mut fx, ints(&[1, 3]));
    let tup = fx.heap_tuple(ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Equal, l, same).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Equal, l, bigger).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Less, l, bigger).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        list_binary_op(ctx, BinaryOp::LessEqual, l, same).unwrap(),
        Value::Bool(true)
    );
    // `!=` is NOT a list-slot op: the VM inverts `==`
    // (`runtime.zig:556`), so the slot returns `PyRaise`.
    assert_eq!(
        list_binary_op(ctx, BinaryOp::NotEqual, l, bigger),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // Cross-type: `EQUAL` → False, ordering → `PyRaise`.
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Equal, l, tup).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Less, l, tup),
        Err(crate::pyerr::PyError::PyRaise)
    );
    assert_eq!(
        list_binary_op(ctx, BinaryOp::Equal, l, Value::SmallInt(1)).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn list_type_data_slots() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = list_type_data(dict);
    assert_eq!(data.flags, 0);
    assert!(data.unary_op.is_some());
    assert!(data.binary_op.is_some());
    assert!(data.subscr.is_some());
    assert!(data.getiter.is_some());
    assert!(data.print.is_some());
    assert!(data.buffer.is_none());
    assert!(data.call.is_none());
    assert_eq!(data.dict, dict);
}

#[test]
fn list_public_abi() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2, 3]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(list_get(ctx, l).unwrap(), ints(&[1, 2, 3]));
    assert_eq!(list_get(ctx, Value::SmallInt(1)), None);
    assert_eq!(list_len(ctx, l), 3);
    // Truncate (the only in-original use).
    list_set_len(ctx, l, 2);
    assert_eq!(items_of(ctx, l), ints(&[1, 2]));
    // Extend pads with `None`.
    list_set_len(ctx, l, 4);
    assert_eq!(
        items_of(ctx, l),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::None, Value::None]
    );
    // `list_store` (`l[i] = v`).
    list_store(ctx, l, Value::SmallInt(0), Value::SmallInt(9)).unwrap();
    assert_eq!(items_of(ctx, l)[0], Value::SmallInt(9));
    assert_eq!(
        list_store(ctx, l, Value::SmallInt(99), Value::SmallInt(9))
            .map_err(|_| ListError::Obj(ObjError::IndexOutOfRange)),
        Err(ListError::Obj(ObjError::IndexOutOfRange))
    );
}

#[test]
fn list_builtin_adapters_arity() {
    let mut fx = Fixture::new();
    let l = lst(&mut fx, ints(&[1, 2]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `append(1, 2)` / `pop(0, 1)` / `insert(0, 2, 3)` /
    // `extend([2], [3])` → arg mismatch (probed).
    assert_eq!(
        list_append_builtin(ctx, &[l, Value::SmallInt(1), Value::SmallInt(2)]),
        Err(crate::pyerr::PyError::PyRaise)
    );
    assert_eq!(
        list_pop_builtin(ctx, &[l, Value::SmallInt(0), Value::SmallInt(1)]),
        Err(crate::pyerr::PyError::PyRaise)
    );
    assert_eq!(
        list_insert_builtin(
            ctx,
            &[l, Value::SmallInt(0), Value::SmallInt(2), Value::SmallInt(3)]
        ),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // `count()` with no value → arg mismatch.
    assert_eq!(
        list_count_builtin(ctx, &[l]),
        Err(crate::pyerr::PyError::PyRaise)
    );
    // The happy paths through the adapters.
    list_append_builtin(ctx, &[l, Value::SmallInt(3)]).unwrap();
    assert_eq!(items_of(ctx, l), ints(&[1, 2, 3]));
    assert_eq!(
        list_pop_builtin(ctx, &[l]).unwrap(),
        Value::SmallInt(3)
    );
}
