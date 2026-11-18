//! `enumerate` (`objenumerate.zig`, 90 lines, 0 test blocks).
//!
//! `MpObjEnumerate { base, iter, cur: isize }` — the
//! sub-iterator in `source`, the counter as a `Value` in
//! `extra[1]` (tag 112 in `extra[0]`). `make_new` parses
//! keyword args (`mp_arg_parse_all_kw_array`: `iterable`
//! required, `start` int default 0). `iternext` yields
//! `(cur, next)` and `cur += 1`.
//!
//! The counter is a `Value`, not a raw `isize`: the Zig's
//! `newSmallInt` is a raw shift with no window check (a start
//! outside the 62-bit window is address-dependent garbage in
//! the original); Rust stores the true value — a `SmallInt`
//! inside the window, a heap `Int` outside it (the C9c §4
//! precedent). `start = 2**70` still raises `OverflowError`
//! (`mp_obj_get_int` bounds — probed).

use super::iter::{
    getiter_hook, new_iter, next_sub, ARG_NUM_TYPES_MISMATCH, IterError, IterSeams,
    ENUMERATE_ITER_TAG, FLAG_ITER_IS_ITERNEXT, NOT_AN_ITERATOR,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_int;
use crate::objects::slots::CallHook;
use crate::objects::objtype::make_new::enumerate_make_new_slot;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, IteratorData, Obj, TypeData, Value};

/// `mp_obj_enumerate_make_new` (`objenumerate.zig:19-54`):
/// the kw-args path. The parse is NOT polled here — the Zig
/// returns `NULL` as soon as the parse sets `pending_exc`
/// (`if (pyerr.pending_exc != 0) return OBJ_NULL`), so a bad
/// `start` conversion fails before `getiter` runs.
pub fn enumerate_make_new(
    ctx: &mut CallCtx,
    args: &[Value],
    n_kw: usize,
    mut hook: Option<CallHook<'_, '_>>,
) -> Result<Value, IterError> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    if n_pos > 2 {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let mut iterable: Option<Value> = None;
    let mut start: Option<Value> = None;
    if n_pos >= 1 {
        iterable = Some(args[0]);
    }
    if n_pos >= 2 {
        start = Some(args[1]);
    }
    for pair in args[n_pos..].chunks(2) {
        let (key, val) = (pair[0], pair[1]);
        let name = match key {
            Value::Qstr(id) => ctx.pool.qstr_str(id as u16),
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Str(s) => s.bytes.as_slice(),
                _ => return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH)),
            },
            _ => return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH)),
        };
        // Keyword names are compared by content (the Zig
        // compares qstrs after `mp_obj_str_get_qstr`; a heap-Str
        // key interns to the same qstr).
        let slot = if name == b"iterable" {
            &mut iterable
        } else if name == b"start" {
            &mut start
        } else {
            return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
        };
        if slot.is_some() {
            // Positional + keyword for the same slot (probed:
            // `enumerate(['a'], 1, start=2)` → mismatch).
            return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
        }
        *slot = Some(val);
    }
    let Some(iterable) = iterable else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    let start_n: i64 = match start {
        Some(v) => obj_get_int(ctx, ctx.types, v).map_err(IterError::from)?,
        None => 0,
    };
    let iter = getiter_hook(ctx, iterable, hook.as_deref_mut())?;
    let cur = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, start_n);
    Ok(new_iter(
        ctx,
        IteratorData {
            source: iter,
            index: 0,
            extra: vec![Value::SmallInt(ENUMERATE_ITER_TAG), cur],
        },
    ))
}

/// `mp_obj_enumerate_iternext` (`objenumerate.zig:56-89`):
/// pull the next item, yield `(cur, next)`, `cur += 1`.
/// Exhaustion returns `None` without advancing `cur`.
pub fn enumerate_iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let (sub, cur_v) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => (d.source, d.extra[1]),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    let Some(next) = next_sub(ctx, sub, seams)? else {
        return Ok(None);
    };
    let cur = obj_get_int(ctx, ctx.types, cur_v).map_err(IterError::from)?;
    // `next` and the fresh tuple are rooted across the two
    // allocations below (D6).
    let mark = ctx.roots.extra.len();
    ctx.roots.extra.push(next);
    let tuple = Value::Heap(new_tuple(ctx, ctx.types.tuple, &[cur_v, next]));
    ctx.roots.extra.push(tuple);
    let new_cur = Value::int(
        ctx.arena,
        Some(ctx.types.int),
        ctx.roots,
        cur.wrapping_add(1),
    );
    if let Value::Heap(r) = it {
        if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
            d.extra[1] = new_cur;
        }
    }
    ctx.roots.extra.truncate(mark);
    Ok(Some(tuple))
}

/// `mp_type_enumerate` ROM (`objenumerate.zig:61-73`):
/// flags `0x0080` (`MP_TYPE_FLAG_ITER_IS_ITERNEXT` — the
/// object is its own iterator, so `getiter` returns it
/// unchanged, the `gen_instance` precedent), slots
/// make_new/iter only, no locals dict. (C13b: the type
/// object itself — the VM binds it as the `enumerate`
/// builtin.)
pub fn enumerate_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"enumerate").expect("enumerate is a static qstr"),
        flags: FLAG_ITER_IS_ITERNEXT,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: Some(enumerate_make_new_slot),
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
    use super::super::iter::IterError;
    use super::super::tests::{collect_all, PanicSeams};
    use super::enumerate_make_new;
    use crate::objects::containers::tests::Fixture;
    use crate::objects::value::{Obj, Value};

    fn tuple_items(ctx: &crate::memory::gc::CallCtx, v: Value) -> Vec<Value> {
        match v {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => items.clone(),
                _ => panic!("expected a tuple, got {v:?}"),
            },
            _ => panic!("expected a tuple, got {v:?}"),
        }
    }

    #[test]
    fn enumerate_basic_and_start() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![
            Value::SmallInt(10),
            Value::SmallInt(20),
            Value::SmallInt(30),
        ]);
        let mut ctx = fx.ctx();
        let mut ps = PanicSeams::new();
        let e = enumerate_make_new(&mut ctx, &[list], 0, None).unwrap();
        let items = collect_all(&mut ctx, e, &mut ps.seams()).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(0), Value::SmallInt(10)]
        );
        assert_eq!(
            tuple_items(&ctx, items[2]),
            vec![Value::SmallInt(2), Value::SmallInt(30)]
        );
        // start = 5 (positional).
        let e5 = enumerate_make_new(&mut ctx, &[list, Value::SmallInt(5)], 0, None).unwrap();
        let items = collect_all(&mut ctx, e5, &mut ps.seams()).unwrap();
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(5), Value::SmallInt(10)]
        );
        // start = -1.
        let em = enumerate_make_new(&mut ctx, &[list, Value::SmallInt(-1)], 0, None).unwrap();
        let items = collect_all(&mut ctx, em, &mut ps.seams()).unwrap();
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(-1), Value::SmallInt(10)]
        );
    }

    #[test]
    fn enumerate_start_out_of_small_window() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![Value::SmallInt(7)]);
        let mut ctx = fx.ctx();
        // 2**62 is outside the 62-bit small-int window: the
        // counter becomes a heap Int (the Zig's raw shift would
        // be garbage here — Rust keeps the true value).
        let big = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, 1i64 << 62);
        let e = enumerate_make_new(&mut ctx, &[list, big], 0, None).unwrap();
        let mut ps = PanicSeams::new();
        let items = collect_all(&mut ctx, e, &mut ps.seams()).unwrap();
        assert_eq!(items.len(), 1);
        let pair = tuple_items(&ctx, items[0]);
        assert_eq!(pair[1], Value::SmallInt(7));
        match pair[0] {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Int(z) => assert_eq!(z.to_string(), "4611686018427387904"),
                _ => panic!("expected a heap Int counter"),
            },
            other => panic!("expected a heap Int counter, got {other:?}"),
        }
    }

    #[test]
    fn enumerate_kwargs() {
        let mut fx = Fixture::new();
        let start_id = fx.pool.from_str("start").unwrap() as u32;
        let iter_id = fx.pool.from_str("iterable").unwrap() as u32;
        let bad_id = fx.pool.from_str("bogus").unwrap() as u32;
        let list = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let other = fx.heap_list(vec![Value::SmallInt(9)]);
        let mut ctx = fx.ctx();
        let mut ps = PanicSeams::new();
        // start= as a keyword.
        let e = enumerate_make_new(
            &mut ctx,
            &[list, Value::Qstr(start_id), Value::SmallInt(3)],
            1,
            None
        )
        .unwrap();
        let items = collect_all(&mut ctx, e, &mut ps.seams()).unwrap();
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(3), Value::SmallInt(1)]
        );
        // iterable= as a keyword.
        let e = enumerate_make_new(
            &mut ctx,
            &[Value::Qstr(iter_id), other, Value::Qstr(start_id), Value::SmallInt(4)],
            2,
            None
        )
        .unwrap();
        let items = collect_all(&mut ctx, e, &mut ps.seams()).unwrap();
        assert_eq!(
            tuple_items(&ctx, items[0]),
            vec![Value::SmallInt(4), Value::SmallInt(9)]
        );
        // Duplicate (positional + keyword) → mismatch (probed).
        assert_eq!(
            enumerate_make_new(
                &mut ctx,
                &[list, Value::SmallInt(1), Value::Qstr(start_id), Value::SmallInt(2)],
                1,
                None
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // Unknown keyword → mismatch.
        assert_eq!(
            enumerate_make_new(
                &mut ctx,
                &[list, Value::Qstr(bad_id), Value::SmallInt(2)],
                1,
                None
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // 3 positional → mismatch; missing iterable → mismatch.
        assert_eq!(
            enumerate_make_new(
                &mut ctx,
                &[list, Value::SmallInt(1), Value::SmallInt(2)],
                0,
                None
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            enumerate_make_new(&mut ctx, &[], 0, None),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
    }

    #[test]
    fn enumerate_start_conversion() {
        let mut fx = Fixture::new();
        let big = fx.heap_bigint(crate::vendor::mpz::BigInt::from_int(1) << 70u64);
        let not_int = fx.heap_str(b"x");
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        // 2**70 → OverflowError (probed).
        assert_eq!(
            enumerate_make_new(&mut ctx, &[list, big], 0, None),
            Err(IterError::Obj(crate::objects::obj::ObjError::Overflow))
        );
        // Non-int start → "can't convert to int" (probed for a
        // float start; the str goes through the same conversion).
        assert_eq!(
            enumerate_make_new(&mut ctx, &[list, not_int], 0, None),
            Err(IterError::Obj(
                crate::objects::obj::ObjError::IntConversion
            ))
        );
        // Non-iterable → "object not iterable".
        assert_eq!(
            enumerate_make_new(&mut ctx, &[Value::SmallInt(5)], 0, None),
            Err(IterError::TypeError("object not iterable"))
        );
    }
}
