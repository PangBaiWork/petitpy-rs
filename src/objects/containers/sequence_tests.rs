//! Tests for `sequence.rs`.

use super::sequence::*;
use super::tests::Fixture;
use crate::objects::obj::ObjError;
use crate::objects::slots::BinaryOp;
use crate::objects::strings::objstr_a::BoundSlice;
use crate::objects::value::Value;

fn ints(items: &[i64]) -> Vec<Value> {
    items.iter().map(|&n| Value::SmallInt(n)).collect()
}

#[test]
fn seq_multiply_basics() {
    assert_eq!(
        seq_multiply(&ints(&[1, 2]), 3),
        Some(ints(&[1, 2, 1, 2, 1, 2]))
    );
    assert_eq!(seq_multiply(&ints(&[1, 2]), 0), Some(Vec::<Value>::new()));
    assert_eq!(seq_multiply(&ints(&[1, 2]), -1), Some(Vec::<Value>::new()));
    assert_eq!(seq_multiply(&ints(&[]), 5), Some(Vec::<Value>::new()));
    assert_eq!(
        seq_multiply(&[b'a', b'b'], 2),
        Some(vec![b'a', b'b', b'a', b'b'])
    );
    // An unrepresentable total is `None`, not a panic (the
    // Zig's `allocList` returns NULL → `MemoryError`).
    assert_eq!(seq_multiply(&ints(&[1, 2]), i64::MAX), None);
}

#[test]
fn seq_cmp_objs_equal_and_ordering() {
    let mut fx = Fixture::new();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // EQUAL.
    assert!(seq_cmp_objs(ctx, BinaryOp::Equal, &ints(&[1, 2]), &ints(&[1, 2])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::Equal, &ints(&[1, 2]), &ints(&[1, 3])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::Equal, &ints(&[1, 2]), &ints(&[1, 2, 3])).unwrap());
    assert!(seq_cmp_objs(ctx, BinaryOp::Equal, &ints(&[]), &ints(&[])).unwrap());
    // LESS / MORE via the swap: prefix rules.
    assert!(seq_cmp_objs(ctx, BinaryOp::Less, &ints(&[1, 2]), &ints(&[1, 3])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::Less, &ints(&[1, 3]), &ints(&[1, 2])).unwrap());
    assert!(seq_cmp_objs(ctx, BinaryOp::Less, &ints(&[1, 2]), &ints(&[1, 2, 3])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::Less, &ints(&[1, 2, 3]), &ints(&[1, 2])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::Less, &ints(&[1, 2]), &ints(&[1, 2])).unwrap());
    assert!(seq_cmp_objs(ctx, BinaryOp::More, &ints(&[1, 3]), &ints(&[1, 2])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::More, &ints(&[1, 2]), &ints(&[1, 2])).unwrap());
    assert!(seq_cmp_objs(ctx, BinaryOp::LessEqual, &ints(&[1, 2]), &ints(&[1, 2])).unwrap());
    assert!(seq_cmp_objs(ctx, BinaryOp::MoreEqual, &ints(&[1, 2]), &ints(&[1, 2])).unwrap());
    assert!(!seq_cmp_objs(ctx, BinaryOp::MoreEqual, &ints(&[1, 2]), &ints(&[1, 3])).unwrap());
}

#[test]
fn seq_cmp_objs_mixed_types_raise() {
    // ["a"] < [1]: element equality is false, then the LESS
    // comparison has no slot answer → PyRaise (the VM's
    // "unsupported types for __lt__" is C14's binopUnsupported).
    let mut fx = Fixture::new();
    let a = fx.heap_str(b"a");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        seq_cmp_objs(ctx, BinaryOp::Less, &[a], &[Value::SmallInt(1)]),
        Err(SeqError::PyRaise)
    );
    // EQUAL with mixed types is just false, no raise.
    assert!(!seq_cmp_objs(ctx, BinaryOp::Equal, &[a], &[Value::SmallInt(1)]).unwrap());
}

#[test]
fn seq_index_obj_basics() {
    let mut fx = Fixture::new();
    let items = ints(&[10, 20, 10, 30]);
    let a_str = fx.heap_str(b"a");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        seq_index_obj(ctx, &items, Value::SmallInt(10), None, None),
        Ok(0)
    );
    assert_eq!(
        seq_index_obj(
            ctx,
            &items,
            Value::SmallInt(10),
            Some(Value::SmallInt(1)),
            None
        ),
        Ok(2)
    );
    assert_eq!(
        seq_index_obj(
            ctx,
            &items,
            Value::SmallInt(10),
            Some(Value::SmallInt(1)),
            Some(Value::SmallInt(2))
        ),
        Err(SeqError::NotInSequence)
    );
    // Clamping: start -100 → 0, stop 100 → len.
    assert_eq!(
        seq_index_obj(
            ctx,
            &items,
            Value::SmallInt(30),
            Some(Value::SmallInt(-100)),
            Some(Value::SmallInt(100))
        ),
        Ok(3)
    );
    // start > stop → empty range → miss.
    assert_eq!(
        seq_index_obj(
            ctx,
            &items,
            Value::SmallInt(10),
            Some(Value::SmallInt(3)),
            Some(Value::SmallInt(1))
        ),
        Err(SeqError::NotInSequence)
    );
    // Miss → ValueError("object not in sequence").
    let e = seq_index_obj(ctx, &items, Value::SmallInt(99), None, None).unwrap_err();
    assert_eq!(e.exc_name(), "ValueError");
    assert_eq!(e.message(), Some("object not in sequence"));
    // Non-int start → IndicesMustBeInts.
    let e = seq_index_obj(ctx, &items, Value::SmallInt(10), Some(a_str), None).unwrap_err();
    assert_eq!(e, SeqError::Obj(ObjError::IndicesMustBeInts));
}

#[test]
fn seq_count_obj_basics() {
    let mut fx = Fixture::new();
    let items = ints(&[1, 2, 1, 1, 3]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        seq_count_obj(ctx, &items, Value::SmallInt(1)).unwrap(),
        3
    );
    assert_eq!(
        seq_count_obj(ctx, &items, Value::SmallInt(9)).unwrap(),
        0
    );
    assert_eq!(
        seq_count_obj(ctx, &[], Value::SmallInt(1)).unwrap(),
        0
    );
}

#[test]
fn seq_extract_slice_steps() {
    let items = ints(&[1, 2, 3, 4, 5]);
    // step 1.
    let bound = BoundSlice { start: 1, stop: 4, step: 1 };
    assert_eq!(seq_extract_slice(&items, &bound), ints(&[2, 3, 4]));
    // step 2.
    let bound = BoundSlice { start: 0, stop: 5, step: 2 };
    assert_eq!(seq_extract_slice(&items, &bound), ints(&[1, 3, 5]));
    // step -1 (the listSliceLoad fallback — probe: [1,2,3,4,5][4:1:-1]
    // == [5,4,3]; the bound slice is already adjusted by
    // fast_slice_indexes: start=4, stop=2 for the [4:1:-1] slice).
    let bound = BoundSlice { start: 4, stop: 2, step: -1 };
    assert_eq!(seq_extract_slice(&items, &bound), ints(&[5, 4, 3]));
    // Empty range.
    let bound = BoundSlice { start: 3, stop: 3, step: 1 };
    assert_eq!(seq_extract_slice(&items, &bound), Vec::<Value>::new());
}

#[test]
fn iter_tags_do_not_collide_with_dict_views() {
    // DictViewKind is 0/1/2; the C9 tags sit outside that range so
    // neither C9a reader claims a list/tuple/array iterator.
    assert!(LIST_ITER_TAG != 0 && LIST_ITER_TAG != 1 && LIST_ITER_TAG != 2);
    assert!(TUPLE_ITER_TAG != 0 && TUPLE_ITER_TAG != 1 && TUPLE_ITER_TAG != 2);
    assert!(ARRAY_ITER_TAG != 0 && ARRAY_ITER_TAG != 1 && ARRAY_ITER_TAG != 2);
    assert_ne!(LIST_ITER_TAG, TUPLE_ITER_TAG);
    assert_ne!(LIST_ITER_TAG, ARRAY_ITER_TAG);
    assert_ne!(TUPLE_ITER_TAG, ARRAY_ITER_TAG);
}
