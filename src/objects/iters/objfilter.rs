//! `filter` (`objfilter.zig`, 85 lines, 0 test blocks).
//!
//! `MpObjFilter { base, fun, iter }` — the predicate in
//! `source`, the sub-iterator in `extra[1]` (tag 111 in
//! `extra[0]`). `make_new` takes exactly 2 args, no kwargs
//! (sig `(2, 2, no kw)`); it calls `mp_getiter` on the second
//! arg AT CONSTRUCTION (probed: `filter(str, 5)` raises
//! `TypeError("object not iterable")` from `filter(...)` itself).
//! `iternext` loops: pull an item; if the predicate is `None`,
//! keep the item when it is truthy; else call the predicate and
//! keep the item when the result is truthy.

use super::iter::{
    getiter, new_iter, next_sub, ARG_NUM_TYPES_MISMATCH, CallOutcome, IterError, IterSeams,
    FILTER_ITER_TAG, FLAG_ITER_IS_ITERNEXT, NOT_AN_ITERATOR,
};
use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_is_true;
use crate::objects::objtype::make_new::filter_make_new_slot;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, IteratorData, Obj, TypeData, Value};

/// `mp_obj_filter_make_new` (`objfilter.zig:17-36`). The sig
/// check is polled like `map`'s (a later `getiter` failure
/// overwrites it); the predicate is NOT checked for callability
/// here (`filter(None, …)` is the truthiness filter by design).
pub fn filter_make_new(
    ctx: &mut CallCtx,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, IterError> {
    let sig_err = n_kw > 0 || args.len() != 2;
    if args.len() < 2 {
        // Too few args to read the iterable from: the sig error
        // is the only possible outcome.
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let iter = getiter(ctx, args[1])?;
    if sig_err {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    Ok(new_iter(
        ctx,
        IteratorData {
            source: args[0],
            index: 0,
            extra: vec![Value::SmallInt(FILTER_ITER_TAG), iter],
        },
    ))
}

/// `mp_obj_filter_iternext` (`objfilter.zig:38-84`): loop
/// pulling items until one passes. `fun == OBJ_NONE` is the
/// truthiness filter (no call — `mp_obj_is_true` on the item
/// itself); otherwise the predicate is called with one arg and
/// its result is tested with `mp_obj_is_true`. A predicate
/// raise (`Stop` or `Raise` from the seam) propagates as an
/// error — never as end-of-iteration.
pub fn filter_iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let (fun, sub) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => (d.source, d.extra[1]),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    loop {
        let Some(item) = next_sub(ctx, sub, seams)? else {
            return Ok(None);
        };
        // The item is rooted across the predicate call (D6):
        // the call may allocate and collect, and the item is
        // only referenced by this local until it is yielded.
        let mark = ctx.roots.extra.len();
        ctx.roots.extra.push(item);
        let keep = if fun == Value::None {
            obj_is_true(ctx, ctx.types, item).map_err(IterError::from)?
        } else {
            match (seams.call)(ctx, fun, &[item]) {
                CallOutcome::Value(v) => obj_is_true(ctx, ctx.types, v).map_err(IterError::from)?,
                CallOutcome::Stop | CallOutcome::Raise => {
                    ctx.roots.extra.truncate(mark);
                    return Err(IterError::PyRaise);
                }
            }
        };
        ctx.roots.extra.truncate(mark);
        if keep {
            return Ok(Some(item));
        }
    }
}

/// `mp_type_filter` ROM (`objfilter.zig:56-68`): flags
/// `0x0080` (`MP_TYPE_FLAG_ITER_IS_ITERNEXT` — the object is
/// its own iterator, so `getiter` returns it unchanged, the
/// `gen_instance` precedent), slots make_new/iter only, no
/// locals dict. (C13b: the type object itself — the VM binds
/// it as the `filter` builtin.)
pub fn filter_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"filter").expect("filter is a static qstr"),
        flags: FLAG_ITER_IS_ITERNEXT,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: Some(filter_make_new_slot),
        bases: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: Some(|_ctx: &mut CallCtx, self_: Value| Ok(self_)),
        subscr: None,
        buffer: None,
        print: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::{CallOutcome, IterError, IterSeams};
    use super::super::tests::{collect_all, token_bytes};
    use super::filter_make_new;
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::tests::Fixture;
    use crate::objects::value::Value;

    /// Predicate tokens: `"is-even"` (SmallInt → Bool),
    /// `"truthy"` (identity — the item itself is the predicate
    /// result), `"raises"` (callee raises), `"stops"` (callee
    /// raises `StopIteration`).
    fn call_seam() -> impl FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome {
        |ctx, fun, args| match token_bytes(ctx, fun) {
            Some(b"is-even") => {
                let Value::SmallInt(n) = args[0] else {
                    panic!("is-even token got a non-small-int");
                };
                CallOutcome::Value(Value::Bool(n % 2 == 0))
            }
            Some(b"truthy") => CallOutcome::Value(args[0]),
            Some(b"raises") => CallOutcome::Raise,
            Some(b"stops") => CallOutcome::Stop,
            _ => CallOutcome::Raise,
        }
    }

    fn seams<'a>(
        call: &'a mut dyn FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome,
        getitem: &'a mut dyn FnMut(&mut CallCtx, Value, i64) -> CallOutcome,
    ) -> IterSeams<'a> {
        IterSeams { call, getitem, next: None, next_builtin: None }
    }

    /// A panicking getitem closure (filter never calls it).
    fn no_getitem() -> impl FnMut(&mut CallCtx, Value, i64) -> CallOutcome {
        |_, _, _| panic!("filter must not call the getitem seam")
    }

    #[test]
    fn filter_none_is_truthiness() {
        let mut fx = Fixture::new();
        let a_str = fx.heap_str(b"a");
        // [0, 1, 0, 2, "a"] → [1, 2, "a"]. The reference probe
        // also drops "" from [0, 1, 0, 2, "", "a"]; the empty
        // string is falsy only through the str type's LEN slot,
        // which the fixture's plain str type does not carry (the
        // production str type, C8a, does) — so "" is not covered
        // here.
        let list = fx.heap_list(vec![
            Value::SmallInt(0),
            Value::SmallInt(1),
            Value::SmallInt(0),
            Value::SmallInt(2),
            a_str,
        ]);
        let mut ctx = fx.ctx();
        let f = filter_make_new(&mut ctx, &[Value::None, list], 0).unwrap();
        let mut call = call_seam();
        let mut getitem = no_getitem();
        let items = collect_all(&mut ctx, f, &mut seams(&mut call, &mut getitem)).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Value::SmallInt(1));
        assert_eq!(items[1], Value::SmallInt(2));
        assert_eq!(items[2], a_str);
    }

    #[test]
    fn filter_predicate() {
        let mut fx = Fixture::new();
        let pred_id = fx.pool.from_str("is-even").unwrap() as u32;
        let list = fx.heap_list(vec![
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
        ]);
        let mut ctx = fx.ctx();
        let f = filter_make_new(&mut ctx, &[Value::Qstr(pred_id), list], 0).unwrap();
        let mut call = call_seam();
        let mut getitem = no_getitem();
        let items = collect_all(&mut ctx, f, &mut seams(&mut call, &mut getitem)).unwrap();
        assert_eq!(items, vec![Value::SmallInt(2), Value::SmallInt(4)]);
    }

    #[test]
    fn filter_sig_errors() {
        let mut fx = Fixture::new();
        let pred_id = fx.pool.from_str("is-even").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        // 1 arg / 3 args / kwargs → sig error.
        assert_eq!(
            filter_make_new(&mut ctx, &[Value::Qstr(pred_id)], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            filter_make_new(
                &mut ctx,
                &[Value::Qstr(pred_id), list, list],
                0
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            filter_make_new(&mut ctx, &[Value::Qstr(pred_id), list], 1),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // Non-iterable → "object not iterable" at construction.
        assert_eq!(
            filter_make_new(&mut ctx, &[Value::Qstr(pred_id), Value::SmallInt(5)], 0),
            Err(IterError::TypeError("object not iterable"))
        );
    }

    #[test]
    fn filter_predicate_raise_is_not_stop() {
        let mut fx = Fixture::new();
        let raises_id = fx.pool.from_str("raises").unwrap() as u32;
        let stops_id = fx.pool.from_str("stops").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        for fun in [Value::Qstr(raises_id), Value::Qstr(stops_id)] {
            let f = filter_make_new(&mut ctx, &[fun, list], 0).unwrap();
            let mut call = call_seam();
            let mut getitem = no_getitem();
            let out = collect_all(&mut ctx, f, &mut seams(&mut call, &mut getitem));
            assert_eq!(out, Err(IterError::PyRaise));
        }
    }
}
