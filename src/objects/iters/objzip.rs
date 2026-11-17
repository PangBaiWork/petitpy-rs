//! `zip` (`objzip.zig`, 101 lines, 0 test blocks).
//!
//! `MpObjZip { base, n_iters, iters[] }` — the sub-iterators in
//! `extra[1..]` (tag 113 in `extra[0]`). `make_new` takes any
//! number of positionals, no kwargs (sig `(0, MAX, no kw)`); it
//! calls `mp_getiter` on each arg AT CONSTRUCTION (probed:
//! `zip([1], 5)` raises `TypeError("object not iterable")` from
//! `zip(...)` itself). `iternext` builds a fresh tuple from one
//! pull off each sub-iterator; the first `StopIteration` ends
//! the zip (the partial tuple is dropped). NO `strict` support
//! (the Zig has none — `zip(…, strict=True)` is a sig error).

use super::iter::{
    getiter, iternext, new_iter, ARG_NUM_TYPES_MISMATCH, IterError, IterSeams,
    FLAG_ITER_IS_ITERNEXT, ZIP_ITER_TAG, NOT_AN_ITERATOR,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::objtype::make_new::zip_make_new_slot;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, IteratorData, Obj, TypeData, Value};

/// `mp_obj_zip_make_new` (`objzip.zig:19-44`). The sig check is
/// polled like `map`'s (a later `getiter` failure overwrites
/// it). Zero iterables is legal — the iterator just stops
/// immediately.
pub fn zip_make_new(
    ctx: &mut CallCtx,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, IterError> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    // Positionals only — keyword pairs are never `getiter`'d
    // (the Zig loops `args[0..n_args]`, objzip.zig).
    let sig_err = n_kw > 0;
    let mut iters = Vec::with_capacity(n_pos);
    for &arg in &args[..n_pos] {
        iters.push(getiter(ctx, arg)?);
    }
    if sig_err {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let mut extra = Vec::with_capacity(iters.len() + 1);
    extra.push(Value::SmallInt(ZIP_ITER_TAG));
    extra.extend(iters);
    Ok(new_iter(
        ctx,
        IteratorData {
            source: Value::None,
            index: 0,
            extra,
        },
    ))
}

/// `mp_obj_zip_iternext` (`objzip.zig:46-100`): with no
/// sub-iterators, stop immediately; else pull one item from
/// each and return the fresh tuple.
pub fn zip_iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let iters = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => d.extra[1..].to_vec(),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    if iters.is_empty() {
        return Ok(None);
    }
    // The pulled items are rooted across the tuple allocation
    // (D6).
    let mark = ctx.roots.extra.len();
    let mut items = Vec::with_capacity(iters.len());
    for sub in &iters {
        match iternext(ctx, *sub, seams)? {
            Some(v) => {
                ctx.roots.extra.push(v);
                items.push(v);
            }
            None => {
                ctx.roots.extra.truncate(mark);
                return Ok(None);
            }
        }
    }
    let tuple = Value::Heap(new_tuple(ctx, ctx.types.tuple, &items));
    ctx.roots.extra.truncate(mark);
    Ok(Some(tuple))
}

/// `mp_type_zip` ROM (`objzip.zig:43-55`): flags `0x0080`
/// (`MP_TYPE_FLAG_ITER_IS_ITERNEXT` — the object is its own
/// iterator, so `getiter` returns it unchanged, the
/// `gen_instance` precedent), slots make_new/iter only, no
/// locals dict. (C13b: the type object itself — the VM binds
/// it as the `zip` builtin.)
pub fn zip_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"zip").expect("zip is a static qstr"),
        flags: FLAG_ITER_IS_ITERNEXT,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: Some(zip_make_new_slot),
        bases: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: Some(|_ctx: &mut CallCtx, self_: Value| Ok(self_)),
        subscr: None,
        buffer: None,
        print: None,
        namedtuple_fields: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::{CallOutcome, IterError, IterSeams};
    use super::super::objmap::map_make_new;
    use super::super::tests::{collect_all, token_bytes, PanicSeams};
    use super::zip_make_new;
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::tests::Fixture;
    use crate::objects::value::{Obj, Value};

    fn tuple_items(ctx: &CallCtx, v: Value) -> Vec<Value> {
        match v {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => items.clone(),
                _ => panic!("expected a tuple, got {v:?}"),
            },
            _ => panic!("expected a tuple, got {v:?}"),
        }
    }

    #[test]
    fn zip_basic() {
        let mut fx = Fixture::new();
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let b = fx.heap_list(vec![Value::SmallInt(3)]);
        let c = fx.heap_list(vec![
            Value::SmallInt(10),
            Value::SmallInt(20),
            Value::SmallInt(30),
        ]);
        let mut ctx = fx.ctx();
        let mut ps = PanicSeams::new();
        // zip([1,2],[3]) → [(1,3)].
        let z = zip_make_new(&mut ctx, &[a, b], 0).unwrap();
        let items = collect_all(&mut ctx, z, &mut ps.seams()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(1), Value::SmallInt(3)]
        );
        // zip([1,2]) → [(1,),(2,)].
        let z = zip_make_new(&mut ctx, &[a], 0).unwrap();
        let items = collect_all(&mut ctx, z, &mut ps.seams()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(tuple_items(&ctx, items[0]), vec![Value::SmallInt(1)]);
        assert_eq!(tuple_items(&ctx, items[1]), vec![Value::SmallInt(2)]);
        // zip() → [].
        let z = zip_make_new(&mut ctx, &[], 0).unwrap();
        let items = collect_all(&mut ctx, z, &mut ps.seams()).unwrap();
        assert!(items.is_empty());
        // Three iterables stop at the shortest.
        let z = zip_make_new(&mut ctx, &[a, b, c], 0).unwrap();
        let items = collect_all(&mut ctx, z, &mut ps.seams()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(1), Value::SmallInt(3), Value::SmallInt(10)]
        );
    }

    #[test]
    fn zip_sig_errors() {
        let mut fx = Fixture::new();
        let strict_id = fx.pool.from_str("strict").unwrap() as u32;
        let a = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        // strict= keyword → sig error (no `strict` support).
        assert_eq!(
            zip_make_new(
                &mut ctx,
                &[a, Value::Qstr(strict_id), Value::Bool(true)],
                1
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // Non-iterable → "object not iterable" at construction.
        assert_eq!(
            zip_make_new(&mut ctx, &[a, Value::SmallInt(5)], 0),
            Err(IterError::TypeError("object not iterable"))
        );
    }

    #[test]
    fn zip_over_map() {
        let mut fx = Fixture::new();
        let add_id = fx.pool.from_str("add").unwrap() as u32;
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let b = fx.heap_list(vec![Value::SmallInt(10), Value::SmallInt(20)]);
        let c = fx.heap_list(vec![Value::SmallInt(100), Value::SmallInt(200)]);
        let mut ctx = fx.ctx();
        let m = map_make_new(&mut ctx, &[Value::Qstr(add_id), a, b], 0).unwrap();
        let z = zip_make_new(&mut ctx, &[m, c], 0).unwrap();
        let mut call = |ctx: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
            match token_bytes(ctx, fun) {
                Some(b"add") => {
                    let (Value::SmallInt(x), Value::SmallInt(y)) = (args[0], args[1]) else {
                        panic!("add token did not get two small ints");
                    };
                    CallOutcome::Value(Value::SmallInt(x + y))
                }
                _ => CallOutcome::Raise,
            }
        };
        let mut getitem = |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome {
            panic!("zip/map must not call the getitem seam")
        };
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let items = collect_all(&mut ctx, z, &mut seams).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(11), Value::SmallInt(100)]
        );
        assert_eq!(
            tuple_items(&ctx, items[1]),
            vec![Value::SmallInt(22), Value::SmallInt(200)]
        );
    }
}
