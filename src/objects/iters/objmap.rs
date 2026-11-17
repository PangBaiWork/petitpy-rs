//! `map` (`objmap.zig`, 100 lines, 0 test blocks).
//!
//! `MpObjMap { base, n_iters, fun, iters[] }` — the function in
//! `source`, the sub-iterators in `extra[1..]` (tag 110 in
//! `extra[0]`). `make_new` takes the function plus ≥1 iterable
//! (sig `(2, MAX, no kw)`); it calls `mp_getiter` on each
//! iterable AT CONSTRUCTION (a non-iterable raises here, not at
//! the first `next` — probed: `map(str, [1], 5)` raises
//! `TypeError("object not iterable")` from `map(...)` itself).
//! `iternext` pulls one item from EACH sub-iterator (the first
//! `StopIteration` ends the map, without calling the function),
//! then calls the function on the pulled items.

use super::iter::{
    getiter, iternext, new_iter, ARG_NUM_TYPES_MISMATCH, CallOutcome, IterError, IterSeams,
    MAP_ITER_TAG, NOT_AN_ITERATOR,
};
use crate::memory::gc::CallCtx;
use crate::objects::value::{IteratorData, Value};

/// `mp_obj_map_make_new` (`objmap.zig:19-44`): the sig check is
/// polled (`mp_arg_check_num_sig` sets `pending_exc` and returns;
/// a later error overwrites it — C9b). `fun` is NOT checked for
/// callability here (`map(None, [1])` constructs fine and raises
/// `TypeError("object not callable")` at the first `next` —
/// probed).
pub fn map_make_new(
    ctx: &mut CallCtx,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, IterError> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    // The Zig loops over the POSITIONALS only (`n_iters =
    // n_args - 1`, objmap.zig:66) — keyword pairs are never
    // `getiter`'d.
    let sig_err = n_kw > 0 || n_pos < 2;
    let Some(&fun) = args.first() else {
        // No positional at all: nothing to read, the sig error is
        // the only possible outcome (the Zig reads garbage here —
        // unobservable, the pending error is raised regardless).
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    let mut iters = Vec::with_capacity(n_pos.saturating_sub(1));
    for &arg in args.get(1..n_pos).unwrap_or(&[]) {
        // A getiter failure overwrites the polled sig error
        // (probed: `map(str, [1], 5)` → "object not iterable",
        // not the sig error — but `map(str)` alone, with no
        // getiter to fail, reports the sig error).
        iters.push(getiter(ctx, arg)?);
    }
    if sig_err {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let mut extra = Vec::with_capacity(iters.len() + 1);
    extra.push(Value::SmallInt(MAP_ITER_TAG));
    extra.extend(iters);
    Ok(new_iter(
        ctx,
        IteratorData {
            source: fun,
            index: 0,
            extra,
        },
    ))
}

/// `mp_obj_map_iternext` (`objmap.zig:46-99`): pull one item
/// from each sub-iterator, then `mp_call_function_n_kw(fun, n,
/// 0, nextses)`. A callee raise (`Stop` or `Raise` from the
/// seam) propagates as an error — never as end-of-iteration.
pub fn map_iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let (fun, iters) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            crate::objects::value::Obj::Iterator(d) => (d.source, d.extra[1..].to_vec()),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    // The pulled items are rooted across the call (D6): the call
    // may allocate and collect, and the items are only referenced
    // by this local `Vec` until the call returns.
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
    let out = (seams.call)(ctx, fun, &items);
    ctx.roots.extra.truncate(mark);
    match out {
        CallOutcome::Value(v) => Ok(Some(v)),
        CallOutcome::Stop | CallOutcome::Raise => Err(IterError::PyRaise),
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::{CallOutcome, IterError, IterSeams, NOT_ITERABLE};
    use super::super::tests::{collect_all, heap_str, token_bytes};
    use super::map_make_new;
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::tests::Fixture;
    use crate::objects::value::{Obj, Value};

    /// The call seam for these tests. Function values are `Qstr`
    /// tokens: `"str"` (int → decimal string), `"add"` (int +
    /// int), `"raises"` (callee raises), `"stops"` (callee raises
    /// `StopIteration`), anything else — including `None` — is
    /// not callable (`Raise`, i.e. `TypeError("object not
    /// callable")` at the VM).
    fn call_seam() -> impl FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome {
        |ctx, fun, args| match token_bytes(ctx, fun) {
            Some(b"str") => {
                let Value::SmallInt(n) = args[0] else {
                    panic!("str token got a non-small-int");
                };
                CallOutcome::Value(heap_str(ctx, &n.to_string()))
            }
            Some(b"add") => {
                let (Value::SmallInt(a), Value::SmallInt(b)) = (args[0], args[1]) else {
                    panic!("add token did not get two small ints");
                };
                CallOutcome::Value(Value::SmallInt(a + b))
            }
            Some(b"raises") => CallOutcome::Raise,
            Some(b"stops") => CallOutcome::Stop,
            _ => CallOutcome::Raise,
        }
    }

    fn str_bytes(ctx: &CallCtx, v: Value) -> Vec<u8> {
        match v {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Str(s) => s.bytes.clone(),
                _ => panic!("expected a heap Str, got {v:?}"),
            },
            _ => panic!("expected a heap Str, got {v:?}"),
        }
    }

    #[test]
    fn map_str_over_list() {
        let mut fx = Fixture::new();
        let str_id = fx.pool.from_str("str").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let mut ctx = fx.ctx();
        let m = map_make_new(&mut ctx, &[Value::Qstr(str_id), list], 0).unwrap();
        let mut call = call_seam();
        let mut getitem = |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome {
            panic!("map must not call the getitem seam")
        };
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let items = collect_all(&mut ctx, m, &mut seams).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(str_bytes(&ctx, items[0]), b"1");
        assert_eq!(str_bytes(&ctx, items[1]), b"2");
    }

    #[test]
    fn map_stops_at_shortest() {
        let mut fx = Fixture::new();
        let add_id = fx.pool.from_str("add").unwrap() as u32;
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let b = fx.heap_list(vec![
            Value::SmallInt(10),
            Value::SmallInt(20),
            Value::SmallInt(30),
        ]);
        let mut ctx = fx.ctx();
        let m = map_make_new(&mut ctx, &[Value::Qstr(add_id), a, b], 0).unwrap();
        let mut call = call_seam();
        let mut getitem = |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome {
            panic!("map must not call the getitem seam")
        };
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let items = collect_all(&mut ctx, m, &mut seams).unwrap();
        assert_eq!(items, vec![Value::SmallInt(11), Value::SmallInt(22)]);
    }

    #[test]
    fn map_sig_errors_are_polled() {
        let mut fx = Fixture::new();
        let str_id = fx.pool.from_str("str").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        // 0 iters → sig error.
        assert_eq!(
            map_make_new(&mut ctx, &[Value::Qstr(str_id)], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // 0 args → sig error.
        assert_eq!(
            map_make_new(&mut ctx, &[], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // Kwargs → sig error (the Zig's `takes_kw = false`).
        assert_eq!(
            map_make_new(&mut ctx, &[Value::Qstr(str_id), list], 1),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // A getiter failure OVERWRITES the polled sig error only
        // when the sig is fine; here the sig is fine and the
        // non-iterable raises at construction (probed).
        assert_eq!(
            map_make_new(&mut ctx, &[Value::Qstr(str_id), list, Value::SmallInt(5)], 0),
            Err(IterError::TypeError(NOT_ITERABLE))
        );
    }

    #[test]
    fn map_func_raise_is_not_stop() {
        let mut fx = Fixture::new();
        let raises_id = fx.pool.from_str("raises").unwrap() as u32;
        let stops_id = fx.pool.from_str("stops").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        for fun in [Value::Qstr(raises_id), Value::Qstr(stops_id), Value::None] {
            let m = map_make_new(&mut ctx, &[fun, list], 0).unwrap();
            let mut call = call_seam();
            let mut getitem = |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome {
                panic!("map must not call the getitem seam")
            };
            let mut seams = IterSeams {
                call: &mut call,
                getitem: &mut getitem,
            };
            // Construction succeeds (callability is not checked
            // there); the FIRST next raises — including when the
            // callee raises StopIteration (probed: `map(f, …)`
            // with a raising `f` propagates the ValueError).
            let out = collect_all(&mut ctx, m, &mut seams);
            assert_eq!(out, Err(IterError::PyRaise));
        }
    }

    #[test]
    fn map_nests() {
        let mut fx = Fixture::new();
        let str_id = fx.pool.from_str("str").unwrap() as u32;
        let add_id = fx.pool.from_str("add").unwrap() as u32;
        let a = fx.heap_list(vec![Value::SmallInt(1)]);
        let b = fx.heap_list(vec![Value::SmallInt(10)]);
        let mut ctx = fx.ctx();
        let inner = map_make_new(&mut ctx, &[Value::Qstr(add_id), a, b], 0).unwrap();
        let outer = map_make_new(&mut ctx, &[Value::Qstr(str_id), inner], 0).unwrap();
        let mut call = call_seam();
        let mut getitem = |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome {
            panic!("map must not call the getitem seam")
        };
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let items = collect_all(&mut ctx, outer, &mut seams).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(str_bytes(&ctx, items[0]), b"11");
    }
}
