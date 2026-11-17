//! Lane C9a dict tests — `make_new` / get / pop / setdefault /
//! popitem / update / fromkeys / copy / equal / print / views /
//! subscr / `|` / fixed maps, against the reference binary's
//! probed behaviour (see `scripts/containers_a_result.txt`).
//!
//! Keys are small ints (hash == itself), bools (dedup with int
//! keys — `{1: 'a'}[True] == 'a'` on the reference binary), `None`
//! and heap `Str`s; float keys need C18's float HASH slot and are
//! covered there, tuple keys C9b's.

use super::objdict::*;
use super::tests::Fixture;
use crate::objects::obj::{call_binary, call_unary, SlotOutcome};
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{MapData, Obj, Value};
use crate::pyerr::PyError;

fn i(n: i64) -> Value {
    Value::SmallInt(n)
}

fn print_of(fx: &mut Fixture, d: Value) -> String {
    let mut out = String::new();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    dict_print(ctx, d, PrintKind::Repr, &mut out);
    out
}

fn view_print_of(fx: &mut Fixture, v: Value) -> String {
    let mut out = String::new();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    dict_view_print(ctx, v, PrintKind::Repr, &mut out);
    out
}

#[test]
fn dict_make_new_empty_and_kwargs() {
    let mut fx = Fixture::new();
    // `dict()` — no positionals, no kwargs.
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_make_new(ctx, ctx.types.dict, &[], 0).unwrap()
    };
    assert_eq!(fx.pairs(d), Vec::<(Value, Value)>::new());
    // `dict(a=1, b=2)` — kwargs as interleaved (key, value) pairs.
    let (k1, k2) = (fx.heap_str(b"a"), fx.heap_str(b"b"));
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_make_new(ctx, ctx.types.dict, &[k1, i(1), k2, i(2)], 2).unwrap()
    };
    assert_eq!(fx.pairs(d), vec![(k1, i(1)), (k2, i(2))]);
}

#[test]
fn dict_make_new_from_pairs_and_dict() {
    let mut fx = Fixture::new();
    // `dict([(1, 'a'), (2, 'b')])` — a list of tuples.
    let sa = fx.heap_str(b"a");
    let sb = fx.heap_str(b"b");
    let t1 = fx.heap_tuple(vec![i(1), sa]);
    let t2 = fx.heap_tuple(vec![i(2), sb]);
    let pairs = fx.heap_list(vec![t1, t2]);
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_make_new(ctx, ctx.types.dict, &[pairs], 0).unwrap()
    };
    assert_eq!(fx.pairs(d).len(), 2);
    assert_eq!(fx.pairs(d)[0].0, i(1));
    // `dict(d)` — copy of a dict.
    let d2 = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_make_new(ctx, ctx.types.dict, &[d], 0).unwrap()
    };
    assert_eq!(fx.pairs(d2), fx.pairs(d));
    // `dict({}, {})` → `TypeError("argument num/types mismatch")`.
    let empty = fx.dict();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    match dict_make_new(ctx, ctx.types.dict, &[empty, empty], 0) {
        Err(DictError::TypeError(msg)) => assert_eq!(msg, "argument num/types mismatch"),
        other => panic!("dict(d, d): {other:?}"),
    }
}

#[test]
fn dict_make_new_bad_pair_length() {
    let mut fx = Fixture::new();
    // `dict([(1, 2), (3,)])` → ValueError.
    let t1 = fx.heap_tuple(vec![i(1), i(2)]);
    let t2 = fx.heap_tuple(vec![i(3)]);
    let pairs = fx.heap_list(vec![t1, t2]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    match dict_make_new(ctx, ctx.types.dict, &[pairs], 0) {
        Err(DictError::ValueError(msg)) => {
            assert_eq!(msg, "dict update sequence has wrong length")
        }
        other => panic!("bad pair: {other:?}"),
    }
    // `dict('ab')` — a str is an iterable of 1-char strs, each of
    // length 1 → the same ValueError.
    let s = fx.heap_str(b"ab");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert!(matches!(
        dict_make_new(ctx, ctx.types.dict, &[s], 0),
        Err(DictError::ValueError(_))
    ));
}

#[test]
fn dict_make_new_ordered_dict_flag() {
    let mut fx = Fixture::new();
    // `OrderedDict([(1, 2)])` — the header type is `OrderedDict`,
    // so it prints with the prefix even though the layout is the
    // same compact table.
    let t1 = fx.heap_tuple(vec![i(1), i(2)]);
    let pairs = fx.heap_list(vec![t1]);
    let od_type = fx.ordered_dict_type();
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_make_new(ctx, od_type, &[pairs], 0).unwrap()
    };
    assert_eq!(print_of(&mut fx, d), "OrderedDict({1: 2})");
}

#[test]
fn dict_get_store_delete_keyerror_carries_key() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    let va = fx.heap_str(b"a");
    fx.store(d, i(1), va);
    // Overwrite keeps the insertion position.
    fx.store(d, i(2), i(20));
    let vcap = fx.heap_str(b"A");
    fx.store(d, i(1), vcap);
    assert_eq!(fx.pairs(d)[0].0, i(1));
    assert_eq!(fx.pairs(d).len(), 2);
    let got = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        dict_get_value(ctx, d, i(1)).unwrap()
    };
    // The second store overwrote the entry, so the get returns the
    // NEW value (`vcap`), in the ORIGINAL position.
    assert_eq!(got, vcap);
    // Missing key: `KeyError` carrying the key.
    let missing = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        dict_get_value(ctx, d, i(9))
    };
    match missing {
        Err(DictError::KeyError(k)) => assert_eq!(k, i(9)),
        other => panic!("missing get: {other:?}"),
    }
    {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        dict_delete(ctx, d, i(2)).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 1);
    let missing_del = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        dict_delete(ctx, d, i(2))
    };
    match missing_del {
        Err(DictError::KeyError(k)) => assert_eq!(k, i(2)),
        other => panic!("missing delete: {other:?}"),
    }
}

#[test]
fn dict_bool_int_none_key_dedup() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    // `{1: 'a'}[True] == 'a'` and `{True: 'a'}[1] == 'a'` (the
    // reference binary dedups bool/int/float keys via hash+equal).
    let va = fx.heap_str(b"a");
    fx.store(d, i(1), va);
    let got = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        let v = dict_get_value(ctx, d, Value::Bool(true)).unwrap();
        assert_eq!(dict_len(ctx.arena, d), 1);
        v
    };
    assert_eq!(got, va);
    // Storing under `True` overwrites the `1` entry, in place.
    let vb = fx.heap_str(b"b");
    fx.store(d, Value::Bool(true), vb);
    assert_eq!(dict_len(&fx.arena, d), 1);
    assert_eq!(fx.pairs(d)[0].0, i(1));
    // `None` is a key (`hash(None) == 6`).
    fx.store(d, Value::None, i(0));
    let got_none = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        dict_get_value(ctx, d, Value::None).unwrap()
    };
    assert_eq!(got_none, i(0));
}

#[test]
fn dict_heap_str_keys_dedup_by_bytes() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    let k1 = fx.heap_str(b"abc");
    let k2 = fx.heap_str(b"abc");
    assert_ne!(k1.as_heap(), k2.as_heap(), "two allocations");
    fx.store(d, k1, i(1));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(dict_get_value(ctx, d, k2).unwrap(), i(1));
    assert_eq!(dict_len(ctx.arena, d), 1);
}

#[test]
fn dict_get_pop_setdefault() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(dict_get(ctx, d, i(1), None).unwrap(), i(10));
    assert_eq!(dict_get(ctx, d, i(9), None).unwrap(), Value::None);
    assert_eq!(dict_get(ctx, d, i(9), Some(i(99))).unwrap(), i(99));
    // `.pop` returns the value and removes the entry.
    assert_eq!(dict_pop(ctx, d, i(1), None).unwrap(), i(10));
    assert_eq!(dict_len(ctx.arena, d), 0);
    match dict_pop(ctx, d, i(1), None) {
        Err(DictError::KeyError(k)) => assert_eq!(k, i(1)),
        other => panic!("pop missing: {other:?}"),
    }
    assert_eq!(dict_pop(ctx, d, i(1), Some(i(7))).unwrap(), i(7));
    // `.setdefault` inserts the default on a miss, returns the
    // existing value on a hit.
    assert_eq!(
        dict_setdefault(ctx, d, i(2), Some(i(20))).unwrap(),
        i(20)
    );
    assert_eq!(dict_get_value(ctx, d, i(2)).unwrap(), i(20));
    assert_eq!(
        dict_setdefault(ctx, d, i(2), Some(i(21))).unwrap(),
        i(20)
    );
    assert_eq!(
        dict_setdefault(ctx, d, i(3), None).unwrap(),
        Value::None
    );
    assert_eq!(dict_get_value(ctx, d, i(3)).unwrap(), Value::None);
}

#[test]
fn dict_popitem_lifo_and_empty() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    fx.store(d, i(2), i(20));
    fx.store(d, i(3), i(30));
    // LIFO: last inserted first (as probed on the reference binary).
    let (t1, t2) = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        let t1 = dict_popitem(ctx, d).unwrap();
        let t2 = dict_popitem(ctx, d).unwrap();
        dict_popitem(ctx, d).unwrap();
        // Empty → `KeyError("popitem(): dictionary is empty")`.
        match dict_popitem(ctx, d) {
            Err(DictError::KeyErrorPopitemEmpty) => {}
            other => panic!("popitem empty: {other:?}"),
        }
        (t1, t2)
    };
    assert!(matches!(t1, Value::Heap(_)));
    if let Value::Heap(r) = t1 {
        match fx.arena.get(r) {
            Obj::Tuple(items) => assert_eq!(items, &[i(3), i(30)]),
            other => panic!("popitem not a tuple: {other:?}"),
        }
    }
    if let Value::Heap(r) = t2 {
        match fx.arena.get(r) {
            Obj::Tuple(items) => assert_eq!(items, &[i(2), i(20)]),
            other => panic!("popitem not a tuple: {other:?}"),
        }
    }
}

#[test]
fn dict_update_sources() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    // Dict source.
    let src = fx.dict();
    fx.store(src, i(1), i(10));
    fx.store(src, i(2), i(20));
    {
        let empty = MapData::new();
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_update(ctx, d, Some(src), &empty).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 2);
    // Pairs source (list of tuples) — overwrites + appends.
    let t1 = fx.heap_tuple(vec![i(2), i(22)]);
    let t2 = fx.heap_tuple(vec![i(3), i(30)]);
    let pairs = fx.heap_list(vec![t1, t2]);
    {
        let empty = MapData::new();
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_update(ctx, d, Some(pairs), &empty).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 3);
    assert_eq!(fx.pairs(d)[1].1, i(22));
    // Kwargs.
    let k = fx.heap_str(b"x");
    let kw = MapData::from_pairs(vec![(k, i(99))]);
    {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_update(ctx, d, None, &kw).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 4);
    // Self-update is a no-op.
    {
        let empty = MapData::new();
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_update(ctx, d, Some(d), &empty).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 4);
    // Bad pair → ValueError.
    let bt = fx.heap_tuple(vec![i(1)]);
    let bad = fx.heap_list(vec![bt]);
    {
        let empty = MapData::new();
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        match dict_update(ctx, d, Some(bad), &empty) {
            Err(DictError::ValueError(msg)) => {
                assert_eq!(msg, "dict update sequence has wrong length")
            }
            other => panic!("bad update pair: {other:?}"),
        }
    }
    // Non-iterable → TypeError("object not iterable").
    {
        let empty = MapData::new();
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        match dict_update(ctx, d, Some(i(5)), &empty) {
            Err(DictError::TypeError(msg)) => assert_eq!(msg, "object not iterable"),
            other => panic!("non-iterable update: {other:?}"),
        }
    }
}

#[test]
fn dict_fromkeys_ctor() {
    let mut fx = Fixture::new();
    // `dict.fromkeys([1, 2, 3])` → `{1: None, 2: None, 3: None}`.
    let keys = fx.heap_list(vec![i(1), i(2), i(3)]);
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_fromkeys(ctx, keys, None).unwrap()
    };
    assert_eq!(fx.pairs(d).len(), 3);
    assert_eq!(fx.pairs(d)[0].1, Value::None);
    // With a default value; duplicates dedup.
    let keys = fx.heap_list(vec![i(1), i(1), i(2)]);
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_fromkeys(ctx, keys, Some(i(0))).unwrap()
    };
    assert_eq!(fx.pairs(d).len(), 2);
    assert_eq!(fx.pairs(d)[0].1, i(0));
    // `dict.fromkeys('ab')` → `{'a': None, 'b': None}`.
    let s = fx.heap_str(b"ab");
    let d = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_fromkeys(ctx, s, None).unwrap()
    };
    assert_eq!(fx.pairs(d).len(), 2);
    // Non-iterable → TypeError.
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert!(matches!(
        dict_fromkeys(ctx, i(5), None),
        Err(DictError::TypeError(_))
    ));
}

#[test]
fn dict_copy_is_independent_and_keeps_type() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    fx.store(d, i(2), i(20));
    let c = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_copy(ctx, d).unwrap()
    };
    assert_ne!(c.as_heap(), d.as_heap());
    assert_eq!(fx.pairs(c), fx.pairs(d));
    // Mutating the copy does not touch the original.
    {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_delete(ctx, c, i(1)).unwrap();
    }
    assert_eq!(fx.pairs(d).len(), 2);
    assert_eq!(fx.pairs(c).len(), 1);
    // An OrderedDict copy stays ordered (same header type).
    let od = fx.ordered_dict();
    fx.store(od, i(1), i(10));
    let oc = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_copy(ctx, od).unwrap()
    };
    assert_eq!(print_of(&mut fx, oc), "OrderedDict({1: 10})");
}

#[test]
fn dict_equal_plain_and_ordered() {
    let mut fx = Fixture::new();
    let a = fx.dict();
    fx.store(a, i(1), i(10));
    fx.store(a, i(2), i(20));
    let b = fx.dict();
    fx.store(b, i(2), i(20));
    fx.store(b, i(1), i(10));
    // Plain dicts: order-insensitive.
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, a, b).unwrap(),
        Value::Bool(true)
    );
    // Different value → not equal.
    fx.store(b, i(1), i(11));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, a, b).unwrap(),
        Value::Bool(false)
    );
    // OrderedDict vs OrderedDict: order-sensitive.
    let oa = fx.ordered_dict();
    fx.store(oa, i(1), i(10));
    fx.store(oa, i(2), i(20));
    let ob = fx.ordered_dict();
    fx.store(ob, i(2), i(20));
    fx.store(ob, i(1), i(10));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, oa, ob).unwrap(),
        Value::Bool(false)
    );
    // Rebuild in the same order as `oa` instead.
    let ob2 = fx.ordered_dict();
    fx.store(ob2, i(1), i(10));
    fx.store(ob2, i(2), i(20));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, oa, ob2).unwrap(),
        Value::Bool(true)
    );
    // Plain == OrderedDict (same entries) → False (rhs is not an
    // exact dict); OrderedDict == plain dict → True (lookup compare).
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, a, oa).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, oa, a).unwrap(),
        Value::Bool(true)
    );
    // Non-dict rhs → False, not an error.
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Equal, a, i(5)).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn dict_print_forms() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    assert_eq!(print_of(&mut fx, d), "{}");
    let va = fx.heap_str(b"a");
    let kk = fx.heap_str(b"k");
    fx.store(d, i(1), va);
    fx.store(d, kk, i(2));
    assert_eq!(print_of(&mut fx, d), "{1: 'a', 'k': 2}");
    let od = fx.ordered_dict();
    fx.store(od, i(1), i(2));
    assert_eq!(print_of(&mut fx, od), "OrderedDict({1: 2})");
}

#[test]
fn dict_views_repr_iter_contains() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    fx.store(d, i(2), i(20));
    // Reprs.
    let (k, it, v) = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        (
            dict_keys(ctx, d).unwrap(),
            dict_items(ctx, d).unwrap(),
            dict_values(ctx, d).unwrap(),
        )
    };
    assert_eq!(view_print_of(&mut fx, k), "dict_keys([1, 2])");
    assert_eq!(view_print_of(&mut fx, it), "dict_items([(1, 10), (2, 20)])");
    assert_eq!(view_print_of(&mut fx, v), "dict_values([10, 20])");
    // Empty views.
    let e = fx.dict();
    let ek = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_keys(ctx, e).unwrap()
    };
    assert_eq!(view_print_of(&mut fx, ek), "dict_keys([])");
    // Iteration via the view iterator (keys), to exhaustion.
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let view_it = dict_getiter(ctx, d).unwrap();
    assert_eq!(dict_view_it_next(ctx, view_it).unwrap(), Some(i(1)));
    assert_eq!(dict_view_it_next(ctx, view_it).unwrap(), Some(i(2)));
    assert_eq!(dict_view_it_next(ctx, view_it).unwrap(), None);
    // Items view iterator yields tuples.
    let items_it = {
        let it_view = dict_items(ctx, d).unwrap();
        dict_view_getiter(ctx, it_view).unwrap()
    };
    let first = dict_view_it_next(ctx, items_it).unwrap().unwrap();
    if let Value::Heap(r) = first {
        match ctx.arena.get(r) {
            Obj::Tuple(items) => assert_eq!(items, &[i(1), i(10)]),
            other => panic!("items it not a tuple: {other:?}"),
        }
    } else {
        panic!("items it not heap");
    }
    // CONTAINS: only the KEYS view supports it.
    assert_eq!(
        dict_view_binary_op(ctx, BinaryOp::Contains, k, i(1)).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        dict_view_binary_op(ctx, BinaryOp::Contains, k, i(9)).unwrap(),
        Value::Bool(false)
    );
    assert!(matches!(
        dict_view_binary_op(ctx, BinaryOp::Contains, v, i(10)),
        Err(PyError::PyRaise)
    ));
    // HASH: only the VALUES view supports it (identity hash).
    assert_eq!(
        dict_view_unary_op(ctx, UnaryOp::Hash, v).unwrap(),
        crate::objects::obj::obj_id(v)
    );
    assert!(matches!(
        dict_view_unary_op(ctx, UnaryOp::Hash, k),
        Err(PyError::PyRaise)
    ));
    // LEN/BOOL delegate to the dict.
    assert_eq!(
        dict_view_unary_op(ctx, UnaryOp::Len, k).unwrap(),
        i(2)
    );
}

#[test]
fn dict_subscr_slot_and_unary() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Store / load / delete through the subscr slot.
    dict_subscr(ctx, d, i(1), SubscrArg::Set(i(10))).unwrap();
    assert_eq!(
        dict_subscr(ctx, d, i(1), SubscrArg::Get).unwrap(),
        i(10)
    );
    assert_eq!(
        dict_subscr(ctx, d, i(1), SubscrArg::Set(i(11))).unwrap(),
        Value::None,
        "store returns None (the Zig's `mp_obj_dict_store` ABI returns the dict instead)"
    );
    dict_subscr(ctx, d, i(1), SubscrArg::Delete).unwrap();
    assert!(matches!(
        dict_subscr(ctx, d, i(1), SubscrArg::Get),
        Err(PyError::PyRaise)
    ));
    // BOOL / LEN through the unary slot.
    assert_eq!(
        dict_unary_op(ctx, UnaryOp::Bool, d).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        dict_unary_op(ctx, UnaryOp::Len, d).unwrap(),
        i(0)
    );
    // CONTAINS through the binary slot.
    assert_eq!(
        dict_binary_op(ctx, BinaryOp::Contains, d, i(1)).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn dict_or_operator() {
    let mut fx = Fixture::new();
    let a = fx.dict();
    fx.store(a, i(1), i(10));
    let b = fx.dict();
    fx.store(b, i(2), i(20));
    fx.store(b, i(1), i(11));
    // `a | b`: a NEW dict, rhs wins on overlap.
    let c = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_binary_op(ctx, BinaryOp::Or, a, b).unwrap()
    };
    assert_ne!(c.as_heap(), a.as_heap());
    assert_eq!(fx.pairs(c), vec![(i(1), i(11)), (i(2), i(20))]);
    assert_eq!(fx.pairs(a).len(), 1, "`|` does not mutate lhs");
    // `a |= b`: in place, same object.
    let r = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        dict_binary_op(ctx, BinaryOp::InplaceOr, a, b).unwrap()
    };
    assert_eq!(r, a);
    assert_eq!(fx.pairs(a), vec![(i(1), i(11)), (i(2), i(20))]);
}

#[test]
fn dict_fixed_map_rejects_writes() {
    let mut fx = Fixture::new();
    // A fixed map (as `new_fixed` builds for defkwargs): reads
    // work, writes raise a bare `TypeError` with NO message (the
    // Zig's `raiseTypeErrorNull` — see `ensure_not_fixed`).
    let fixed = {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        let t = ctx.types.dict;
        Value::Heap(ctx.alloc(
            Some(t),
            Obj::Dict(crate::objects::value::DictData {
                map: MapData::new_fixed(vec![(Value::SmallInt(1), Value::SmallInt(10))]),
            }),
        ))
    };
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(dict_get_value(ctx, fixed, i(1)).unwrap(), i(10));
    match dict_store(ctx, fixed, i(2), i(20)) {
        Err(DictError::TypeError(msg)) => assert_eq!(msg, ""),
        other => panic!("fixed store: {other:?}"),
    }
    match dict_delete(ctx, fixed, i(1)) {
        Err(DictError::TypeError(msg)) => assert_eq!(msg, ""),
        other => panic!("fixed delete: {other:?}"),
    }
    // Even an empty update raises: the Zig's `dictUpdateImpl`
    // calls `ensureNotFixed` before anything else (objdict.zig:448).
    let empty = MapData::new();
    match dict_update(ctx, fixed, None, &empty) {
        Err(DictError::TypeError(msg)) => assert_eq!(msg, ""),
        other => panic!("fixed update: {other:?}"),
    }
}

#[test]
fn dict_slots_wired_via_call_unary_binary() {
    // The dict type's slots are reachable through C6's
    // `call_unary` / `call_binary` (the VM path), not just directly.
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert!(matches!(
        call_unary(ctx, Some(ctx.types.dict), UnaryOp::Len, d).unwrap(),
        SlotOutcome::Value(Value::SmallInt(1))
    ));
    assert!(matches!(
        call_binary(ctx, Some(ctx.types.dict), BinaryOp::Contains, d, i(1)).unwrap(),
        SlotOutcome::Value(Value::Bool(true))
    ));
    assert!(matches!(
        call_binary(ctx, Some(ctx.types.dict), BinaryOp::Contains, d, i(9)).unwrap(),
        SlotOutcome::Value(Value::Bool(false))
    ));
}

#[test]
fn dict_builtin_adapters_arity() {
    let mut fx = Fixture::new();
    let d = fx.dict();
    fx.store(d, i(1), i(10));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `d.get(1)` / `d.get(9, 99)` through the adapter.
    assert_eq!(dict_get_builtin(ctx, &[d, i(1)]).unwrap(), i(10));
    assert_eq!(
        dict_get_builtin(ctx, &[d, i(9), i(99)]).unwrap(),
        i(99)
    );
    // Wrong arity → `TypeError("argument num/types mismatch")`.
    assert!(matches!(
        dict_get_builtin(ctx, &[d]),
        Err(PyError::PyRaise)
    ));
    assert!(matches!(
        dict_clear_builtin(ctx, &[d, i(1)]),
        Err(PyError::PyRaise)
    ));
    // `dict.clear()` through the adapter.
    dict_clear_builtin(ctx, &[d]).unwrap();
    assert_eq!(dict_len(ctx.arena, d), 0);
}
