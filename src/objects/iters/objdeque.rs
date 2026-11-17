//! Native deque (`objdeque.zig`, 366 lines, 0 test blocks).
//!
//! `MpObjDeque { base, alloc, i_get, i_put, items, flags }` —
//! a ring buffer, ported faithfully: `alloc = maxlen + 1` (the
//! extra slot distinguishes full from empty),
//! `len = (i_put - i_get) mod alloc`. `flags` bit 0 is
//! `FLAG_CHECK_OVERFLOW`. The payload is [`DequeData`] (see
//! `value.rs`); slots outside `[i_get, i_put)` hold
//! `Value::None` (the Zig zeroes them with `m_malloc0`).
//!
//! This is the NATIVE deque — `ucollections.deque`. The
//! `collections.deque` list-backed wrapper is petitpy-lib
//! (Phase D); it accepts kwargs and has `rotate`/`clear`/
//! `count`/`index`/`remove`/`extendleft`, NONE of which exist
//! here (the locals dict has ONLY `append`, `appendleft`,
//! `extend`, `pop`, `popleft` — probed: `hasattr` is `False`
//! for the rest). `in` works through the VM's iteration
//! fallback (there is no binary-op slot); the print slot is 0,
//! so the deque prints as `<deque>` (C6 fallback — probed).
//!
//! `make_new` extends from the iterable with
//! `collect_iterable` (the C9 `tuple`/`dict` pattern), NOT the
//! streaming `mp_iternext` loop — so an infinite iterable with
//! the overflow flag set would hang here where the Zig raises
//! `"full"` after `maxlen + 1` appends (documented; all finite
//! cases match). C11's iterators (map/filter/…) are likewise
//! not `collect_iterable` shapes — `deque(map(…), n)` reports
//! "object not iterable" here, the same limitation `tuple`/
//! `dict` already have; the general protocol is C13's.

use super::iter::{
    new_iter, ARG_NUM_TYPES_MISMATCH, IterError, DEQUE_ITER_TAG,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict_methods::collect_iterable;
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::obj_ops::get_index;
use crate::objects::slots::{SubscrArg, UnaryOp};
use crate::objects::value::{DequeData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// `FLAG_CHECK_OVERFLOW` (`objdeque.zig:14`).
pub const FLAG_CHECK_OVERFLOW: u32 = 1;

/// `dequeLen` (`objdeque.zig:82-88`).
pub fn deque_len(d: &DequeData) -> usize {
    if d.i_put >= d.i_get {
        d.i_put - d.i_get
    } else {
        d.i_put + d.alloc - d.i_get
    }
}

fn deque_data<'a>(ctx: &'a mut CallCtx, self_: Value) -> Result<&'a mut DequeData, IterError> {
    match self_ {
        Value::Heap(r) => match ctx.arena.get_mut(r) {
            Obj::Deque(d) => Ok(d),
            _ => Err(IterError::PyRaise),
        },
        _ => Err(IterError::PyRaise),
    }
}

/// `dequeAppendImpl` (`objdeque.zig:114-133`).
fn deque_append_inner(d: &mut DequeData, arg: Value) -> Result<(), IterError> {
    let mut new_i_put = d.i_put + 1;
    if new_i_put == d.alloc {
        new_i_put = 0;
    }
    if d.flags & FLAG_CHECK_OVERFLOW != 0 && new_i_put == d.i_get {
        return Err(IterError::IndexError("full"));
    }
    d.items[d.i_put] = arg;
    d.i_put = new_i_put;
    if d.i_get == new_i_put {
        d.i_get += 1;
        if d.i_get == d.alloc {
            d.i_get = 0;
        }
    }
    Ok(())
}

/// `dequeAppendleftImpl` (`objdeque.zig:141-163`).
fn deque_appendleft_inner(d: &mut DequeData, arg: Value) -> Result<(), IterError> {
    let new_i_get = if d.i_get == 0 { d.alloc - 1 } else { d.i_get - 1 };
    if d.flags & FLAG_CHECK_OVERFLOW != 0 && new_i_get == d.i_put {
        return Err(IterError::IndexError("full"));
    }
    d.i_get = new_i_get;
    d.items[d.i_get] = arg;
    // Overwriting the first element: move `i_put` back (evict
    // right).
    if d.i_put == new_i_get {
        if d.i_put == 0 {
            d.i_put = d.alloc - 1;
        } else {
            d.i_put -= 1;
        }
    }
    Ok(())
}

/// `dequeMakeNewImpl` (`objdeque.zig:90-108`): sig `(2, 3,
/// no kw)`. `maxlen` is read first (a non-int raises "can't
/// convert to int"; a negative raises a bare `ValueError`);
/// the ring is allocated next (`maxlen + 1` slots — a huge
/// `maxlen` fails the allocation here, matching the Zig's
/// order); `flags` third; the iterable is extended last. The
/// sig check is polled: a later error overwrites it. With 1
/// positional the deque is discarded (the sig error
/// survives) unless extending fails — only the iterable's
/// error is observable there (the Zig reads `maxlen` from a
/// garbage slot in that case; unobservable).
pub fn deque_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, IterError> {
    // `args` is [positionals..., kw-key, kw-value, ...]; only
    // the first `n_pos` entries are real positionals. The Zig
    // reads maxlen/flags from `args[1]`/`args[2]` unconditionally
    // (objdeque.zig:88-96) — with fewer than 2 positionals that
    // read lands on a keyword key or past the end, which is
    // unobservable (the pending sig error is raised regardless),
    // so the Rust model reads maxlen/flags only from real
    // positional slots.
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    let sig_err = n_kw > 0 || n_pos < 2 || n_pos > 3;
    let Some(&iterable) = args.first() else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    if n_pos == 1 {
        // Only the iterable's error is observable (probed:
        // `deque(5)` → "object not iterable", `deque([1, 2])`
        // → the sig error).
        let _ = collect_iterable(ctx, iterable).map_err(map_collect_err)?;
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let maxlen = obj_get_int(ctx, ctx.types, args[1]).map_err(IterError::from)?;
    if maxlen < 0 {
        return Err(IterError::ValueError(""));
    }
    let alloc = (maxlen as usize)
        .checked_add(1)
        .ok_or(IterError::OutOfMemory)?;
    let mut items = Vec::new();
    items
        .try_reserve_exact(alloc)
        .map_err(|_| IterError::OutOfMemory)?;
    items.resize(alloc, Value::None);
    let flags = if n_pos > 2 {
        obj_get_int(ctx, ctx.types, args[2]).map_err(IterError::from)?
    } else {
        0
    };
    let items_in = collect_iterable(ctx, iterable).map_err(map_collect_err)?;
    let mut data = DequeData {
        alloc,
        i_get: 0,
        i_put: 0,
        items,
        flags: flags as u32,
    };
    for item in items_in {
        deque_append_inner(&mut data, item)?;
    }
    if sig_err {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    Ok(Value::Heap(ctx.alloc(Some(type_), Obj::Deque(data))))
}

fn map_collect_err(
    e: crate::objects::containers::objdict::DictError,
) -> IterError {
    use crate::objects::containers::objdict::DictError;
    match e {
        DictError::TypeError(m) => IterError::TypeError(m),
        DictError::ValueError(m) => IterError::ValueError(m),
        DictError::OutOfMemory => IterError::OutOfMemory,
        _ => IterError::PyRaise,
    }
}

/// `deque_unary_op` (`objdeque.zig:114-121`): BOOL is
/// non-empty, LEN is the ring length; other ops answer NULL
/// (`PyRaise` here — the C6 "not implemented" convention).
pub fn deque_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_: Value) -> PyResult<Value> {
    let d = match self_ {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Deque(d) => d,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    match op {
        UnaryOp::Bool => Ok(Value::Bool(d.i_get != d.i_put)),
        UnaryOp::Len => Ok(Value::SmallInt(deque_len(d) as i64)),
        _ => Err(PyError::PyRaise),
    }
}

/// `deque_append` (`objdeque.zig:141-144`).
pub fn deque_append(ctx: &mut CallCtx, self_: Value, arg: Value) -> Result<Value, IterError> {
    let d = deque_data(ctx, self_)?;
    deque_append_inner(d, arg)?;
    Ok(Value::None)
}

/// `deque_appendleft` (`objdeque.zig:165-168`).
pub fn deque_appendleft(ctx: &mut CallCtx, self_: Value, arg: Value) -> Result<Value, IterError> {
    let d = deque_data(ctx, self_)?;
    deque_appendleft_inner(d, arg)?;
    Ok(Value::None)
}

/// `deque_extend` (`objdeque.zig:170-184`): `getiter` +
/// append loop (via `collect_iterable` — see the module docs
/// for the streaming difference).
pub fn deque_extend(ctx: &mut CallCtx, self_: Value, arg: Value) -> Result<Value, IterError> {
    let items = collect_iterable(ctx, arg).map_err(map_collect_err)?;
    let d = deque_data(ctx, self_)?;
    for item in items {
        deque_append_inner(d, item)?;
    }
    Ok(Value::None)
}

/// `deque_popleft` (`objdeque.zig:186-200`): empty →
/// `IndexError("empty")`; the popped slot is cleared to
/// `None` (the Zig writes `OBJ_NULL`).
pub fn deque_popleft(ctx: &mut CallCtx, self_: Value) -> Result<Value, IterError> {
    let d = deque_data(ctx, self_)?;
    if d.i_get == d.i_put {
        return Err(IterError::IndexError("empty"));
    }
    let ret = d.items[d.i_get];
    d.items[d.i_get] = Value::None;
    d.i_get += 1;
    if d.i_get == d.alloc {
        d.i_get = 0;
    }
    Ok(ret)
}

/// `deque_pop` (`objdeque.zig:202-216`): empty →
/// `IndexError("empty")`; the popped slot is cleared.
pub fn deque_pop(ctx: &mut CallCtx, self_: Value) -> Result<Value, IterError> {
    let d = deque_data(ctx, self_)?;
    if d.i_get == d.i_put {
        return Err(IterError::IndexError("empty"));
    }
    if d.i_put == 0 {
        d.i_put = d.alloc - 1;
    } else {
        d.i_put -= 1;
    }
    let ret = d.items[d.i_put];
    d.items[d.i_put] = Value::None;
    Ok(ret)
}

/// `deque_subscr` (`objdeque.zig:218-239`): delete answers
/// NULL (the generic path raises "object doesn't support item
/// deletion"); a slice index raises "indices must be integers"
/// (`mp_get_index` with `is_slice = false` — probed);
/// out-of-range raises "index out of range"; otherwise the
/// ring slot at `(i_get + offset) % alloc`.
pub fn deque_subscr(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let types = ctx.types;
    if matches!(arg, SubscrArg::Delete) {
        return Err(PyError::PyRaise);
    }
    let (len, i_get, alloc) = match self_ {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Deque(d) => (deque_len(d), d.i_get, d.alloc),
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let offset = match get_index(ctx, types, len, index, false) {
        Ok(i) => i,
        Err(ObjError::OutOfMemory) => return Err(PyError::OutOfMemory),
        Err(_) => return Err(PyError::PyRaise),
    };
    let mut ring = i_get + offset;
    if ring >= alloc {
        ring -= alloc;
    }
    match arg {
        SubscrArg::Get => match self_ {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Deque(d) => Ok(d.items[ring]),
                _ => Err(PyError::PyRaise),
            },
            _ => Err(PyError::PyRaise),
        },
        SubscrArg::Set(v) => match self_ {
            Value::Heap(r) => match ctx.arena.get_mut(r) {
                Obj::Deque(d) => {
                    d.items[ring] = v;
                    Ok(Value::None)
                }
                _ => Err(PyError::PyRaise),
            },
            _ => Err(PyError::PyRaise),
        },
        SubscrArg::Delete => unreachable!(),
    }
}

/// `mp_obj_new_deque_it` (`objdeque.zig:258-268`): the
/// iterator's cursor starts at the deque's CURRENT `i_get`
/// and is NOT re-synced afterwards (probed: an `appendleft`
/// between two `next()` calls is visible through the live
/// `i_put`, not through `i_get`).
pub fn deque_getiter(ctx: &mut CallCtx, self_: Value) -> PyResult<Value> {
    let i_get = match self_ {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Deque(d) => d.i_get,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    Ok(new_iter(
        ctx,
        crate::objects::value::IteratorData {
            source: self_,
            index: i_get,
            extra: vec![Value::SmallInt(DEQUE_ITER_TAG)],
        },
    ))
}

/// `deque_it_iternext` (`objdeque.zig:246-256`): `cur !=
/// i_put` → `items[cur]`, `cur += 1` (wrap at `alloc`);
/// else stop. The deque payload is read live on every step.
pub fn deque_iternext(ctx: &mut CallCtx, it: Value) -> Result<Option<Value>, IterError> {
    let (deque, cur) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => (d.source, d.index),
            _ => return Err(IterError::TypeError("object not an iterator")),
        },
        _ => return Err(IterError::TypeError("object not an iterator")),
    };
    let (alloc, item) = match deque {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Deque(d) => (
                d.alloc,
                if d.i_put == cur { None } else { Some(d.items[cur]) },
            ),
            _ => return Err(IterError::PyRaise),
        },
        _ => return Err(IterError::PyRaise),
    };
    match item {
        Some(v) => {
            let mut next = cur + 1;
            if next == alloc {
                next = 0;
            }
            if let Value::Heap(r) = it {
                if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
                    d.index = next;
                }
            }
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// `mp_type_deque` ROM data (`objdeque.zig:335-360`): name
/// `deque`, flags `0x0000`, slots unary_op + subscr + getiter,
/// NO print slot (prints as `<deque>` via the C6 fallback),
/// NO binary-op slot. The name is a parameter — `"deque"` is
/// NOT a static qstr (it lives in `ucollections`, not the
/// core), so the caller interns it (C12's ROM singleton does
/// the same). `make_new` is the type-call path, wired by C12,
/// not a `TypeData` slot (the `dict_type_data` precedent).
pub fn deque_type_data(dict: GcRef, name: u32) -> TypeData {
    TypeData {
        name,
        flags: 0x0000,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: Some(crate::objects::objtype::make_new::deque_make_new_slot),
        bases: None,
        unary_op: Some(deque_unary_op),
        binary_op: None,
        call: None,
        getiter: Some(deque_getiter),
        subscr: Some(deque_subscr),
        buffer: None,
        print: None,
        namedtuple_fields: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::IterError;
    use super::super::tests::{collect_all, PanicSeams};
    use super::{
        deque_append, deque_appendleft, deque_extend, deque_getiter, deque_iternext,
        deque_make_new, deque_pop, deque_popleft, deque_subscr, deque_type_data,
        deque_unary_op,
    };
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::tests::{empty_dict, Fixture};
    use crate::objects::slots::{SubscrArg, UnaryOp};
    use crate::objects::value::{Obj, SliceData, Value};
    use crate::pyerr::PyError;

    fn make_deque_type(fx: &mut Fixture) -> crate::objects::value::GcRef {
        let name = fx.pool.from_str("deque").unwrap() as u32;
        let dict = empty_dict(&mut fx.arena, &fx.roots);
        let t = fx
            .arena
            .alloc(None, Obj::Type(deque_type_data(dict, name)), &fx.roots);
        fx.roots.push(Value::Heap(t));
        t
    }

    fn deque(
        ctx: &mut CallCtx,
        t: crate::objects::value::GcRef,
        args: &[Value],
        n_kw: usize,
    ) -> Result<Value, IterError> {
        deque_make_new(ctx, t, args, n_kw)
    }

    fn len_of(ctx: &mut CallCtx, d: Value) -> i64 {
        match deque_unary_op(ctx, UnaryOp::Len, d) {
            Ok(Value::SmallInt(n)) => n,
            other => panic!("deque len failed: {other:?}"),
        }
    }

    fn bool_of(ctx: &mut CallCtx, d: Value) -> bool {
        match deque_unary_op(ctx, UnaryOp::Bool, d) {
            Ok(Value::Bool(b)) => b,
            other => panic!("deque bool failed: {other:?}"),
        }
    }

    #[test]
    fn deque_make_new_basics() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let b = fx.heap_list(vec![
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
        ]);
        let e = fx.heap_list(vec![]);
        let mut ctx = fx.ctx();
        // deque([1,2], 2) → [1,2]; maxlen eviction on build.
        let d = deque(&mut ctx, t, &[a, Value::SmallInt(2)], 0).unwrap();
        assert_eq!(len_of(&mut ctx, d), 2);
        let d3 = deque(&mut ctx, t, &[b, Value::SmallInt(2)], 0).unwrap();
        assert_eq!(len_of(&mut ctx, d3), 2);
        let mut ps = PanicSeams::new();
        let it = deque_getiter(&mut ctx, d3).unwrap();
        let items = collect_all(&mut ctx, it, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(2), Value::SmallInt(3)]);
        // maxlen 0 stays empty.
        let d0 = deque(&mut ctx, t, &[a, Value::SmallInt(0)], 0).unwrap();
        assert_eq!(len_of(&mut ctx, d0), 0);
        assert!(!bool_of(&mut ctx, d0));
        assert!(bool_of(&mut ctx, d));
        // Empty iterable + maxlen.
        let de = deque(&mut ctx, t, &[e, Value::SmallInt(2)], 0).unwrap();
        assert_eq!(len_of(&mut ctx, de), 0);
    }

    #[test]
    fn deque_make_new_errors() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let ml_id = fx.pool.from_str("maxlen").unwrap() as u32;
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let s = fx.heap_str(b"x");
        let big = fx.heap_bigint(crate::vendor::mpz::BigInt::from_int(1) << 70u64);
        let mut ctx = fx.ctx();
        // 0-1 positional → sig error; kwargs → sig error.
        assert_eq!(
            deque(&mut ctx, t, &[], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            deque(&mut ctx, t, &[a], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            deque(&mut ctx, t, &[a, Value::SmallInt(2), Value::SmallInt(0), Value::SmallInt(0)], 0),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            deque(
                &mut ctx,
                t,
                &[a, Value::Qstr(ml_id), Value::SmallInt(1)],
                1
            ),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        // Non-iterable → "object not iterable" (probed).
        assert_eq!(
            deque(&mut ctx, t, &[Value::SmallInt(5), Value::SmallInt(2)], 0),
            Err(IterError::TypeError("object not iterable"))
        );
        // 1-arg non-iterable → the iterable's error wins over
        // the polled sig error (probed: `deque(5)`).
        assert_eq!(
            deque(&mut ctx, t, &[Value::SmallInt(5)], 0),
            Err(IterError::TypeError("object not iterable"))
        );
        // Non-int maxlen → "can't convert to int" (probed).
        assert_eq!(
            deque(&mut ctx, t, &[a, s], 0),
            Err(IterError::Obj(crate::objects::obj::ObjError::IntConversion))
        );
        // Negative maxlen → bare ValueError (probed).
        assert_eq!(
            deque(&mut ctx, t, &[a, Value::SmallInt(-1)], 0),
            Err(IterError::ValueError(""))
        );
        // 2**70 maxlen → OverflowError (probed).
        assert_eq!(
            deque(&mut ctx, t, &[a, big], 0),
            Err(IterError::Obj(crate::objects::obj::ObjError::Overflow))
        );
    }

    #[test]
    fn deque_append_pop() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let mut ctx = fx.ctx();
        let d = deque(&mut ctx, t, &[a, Value::SmallInt(2)], 0).unwrap();
        // Append evicts left ([1,2] + 3 → [2,3]).
        deque_append(&mut ctx, d, Value::SmallInt(3)).unwrap();
        let mut ps = PanicSeams::new();
        let it = deque_getiter(&mut ctx, d).unwrap();
        let items = collect_all(&mut ctx, it, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(2), Value::SmallInt(3)]);
        // Appendleft evicts right.
        deque_appendleft(&mut ctx, d, Value::SmallInt(0)).unwrap();
        let it = deque_getiter(&mut ctx, d).unwrap();
        let items = collect_all(&mut ctx, it, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(0), Value::SmallInt(2)]);
        // pop / popleft.
        assert_eq!(deque_pop(&mut ctx, d).unwrap(), Value::SmallInt(2));
        assert_eq!(deque_popleft(&mut ctx, d).unwrap(), Value::SmallInt(0));
        assert_eq!(len_of(&mut ctx, d), 0);
        assert_eq!(
            deque_pop(&mut ctx, d),
            Err(IterError::IndexError("empty"))
        );
        assert_eq!(
            deque_popleft(&mut ctx, d),
            Err(IterError::IndexError("empty"))
        );
    }

    #[test]
    fn deque_overflow_flag() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let mut ctx = fx.ctx();
        // flags=1: append/appendleft past maxlen raise "full".
        let d = deque(
            &mut ctx,
            t,
            &[a, Value::SmallInt(2), Value::SmallInt(1)],
            0,
        )
        .unwrap();
        assert_eq!(
            deque_append(&mut ctx, d, Value::SmallInt(3)),
            Err(IterError::IndexError("full"))
        );
        assert_eq!(
            deque_appendleft(&mut ctx, d, Value::SmallInt(0)),
            Err(IterError::IndexError("full"))
        );
        // Without the flag the same appends evict instead.
        let d2 = deque(&mut ctx, t, &[a, Value::SmallInt(2), Value::SmallInt(0)], 0).unwrap();
        deque_append(&mut ctx, d2, Value::SmallInt(3)).unwrap();
        deque_appendleft(&mut ctx, d2, Value::SmallInt(0)).unwrap();
        let mut ps = PanicSeams::new();
        let it = deque_getiter(&mut ctx, d2).unwrap();
        let items = collect_all(&mut ctx, it, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(0), Value::SmallInt(2)]);
    }

    #[test]
    fn deque_extend_and_subscr() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let a = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let ext = fx.heap_list(vec![Value::SmallInt(3)]);
        let mut ctx = fx.ctx();
        let d = deque(&mut ctx, t, &[a, Value::SmallInt(5)], 0).unwrap();
        deque_extend(&mut ctx, d, ext).unwrap();
        assert_eq!(len_of(&mut ctx, d), 3);
        // Indexing (positive + negative), store, errors.
        let v = deque_subscr(&mut ctx, d, Value::SmallInt(0), SubscrArg::Get).unwrap();
        assert_eq!(v, Value::SmallInt(1));
        let v = deque_subscr(&mut ctx, d, Value::SmallInt(-1), SubscrArg::Get).unwrap();
        assert_eq!(v, Value::SmallInt(3));
        deque_subscr(
            &mut ctx,
            d,
            Value::SmallInt(1),
            SubscrArg::Set(Value::SmallInt(9)),
        )
        .unwrap();
        let v = deque_subscr(&mut ctx, d, Value::SmallInt(1), SubscrArg::Get).unwrap();
        assert_eq!(v, Value::SmallInt(9));
        // Out of range → PyRaise (the slot boundary erases the
        // IndexError payload — C6 convention; the Zig raises
        // IndexError("index out of range"), probed).
        assert_eq!(
            deque_subscr(&mut ctx, d, Value::SmallInt(100), SubscrArg::Get),
            Err(PyError::PyRaise)
        );
        assert_eq!(
            deque_subscr(&mut ctx, d, Value::SmallInt(-100), SubscrArg::Get),
            Err(PyError::PyRaise)
        );
        // Delete → PyRaise ("object doesn't support item
        // deletion" at the generic path — probed).
        assert_eq!(
            deque_subscr(&mut ctx, d, Value::SmallInt(0), SubscrArg::Delete),
            Err(PyError::PyRaise)
        );
        // A slice index → "indices must be integers" (probed).
        // A real `Obj::Slice` value (header type is irrelevant
        // — `get_index` only reads the payload).
        let slice = Value::Heap(ctx.alloc(
            None,
            Obj::Slice(SliceData {
                start: Some(Value::SmallInt(1)),
                stop: Some(Value::SmallInt(3)),
                step: None,
            }),
        ));
        assert_eq!(
            deque_subscr(&mut ctx, d, slice, SubscrArg::Get),
            Err(PyError::PyRaise)
        );
        // extend with a non-iterable → "object not iterable".
        assert_eq!(
            deque_extend(&mut ctx, d, Value::SmallInt(5)),
            Err(IterError::TypeError("object not iterable"))
        );
    }

    #[test]
    fn deque_iterator_live() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let a = fx.heap_list(vec![
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
        ]);
        let mut ctx = fx.ctx();
        let d = deque(&mut ctx, t, &[a, Value::SmallInt(5)], 0).unwrap();
        let it = deque_getiter(&mut ctx, d).unwrap();
        let mut ps = PanicSeams::new();
        let mut seams = ps.seams();
        // next → 1; appendleft(0); next → 2 (the cursor is NOT
        // re-synced to i_get — probed on the reference binary).
        assert_eq!(
            deque_iternext(&mut ctx, it),
            Ok(Some(Value::SmallInt(1)))
        );
        deque_appendleft(&mut ctx, d, Value::SmallInt(0)).unwrap();
        assert_eq!(
            deque_iternext(&mut ctx, it),
            Ok(Some(Value::SmallInt(2)))
        );
        let rest = collect_all(&mut ctx, it, &mut seams).unwrap();
        assert_eq!(rest, vec![Value::SmallInt(3)]);
        // The iterator through the unified dispatch.
        let it2 = deque_getiter(&mut ctx, d).unwrap();
        let items = collect_all(&mut ctx, it2, &mut seams).unwrap();
        assert_eq!(
            items,
            vec![Value::SmallInt(0), Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
        );
    }

    #[test]
    fn deque_type_data_shape() {
        let mut fx = Fixture::new();
        let t = make_deque_type(&mut fx);
        let name_id = fx.pool.find_strn(b"deque") as u32;
        let ctx = fx.ctx();
        match ctx.arena.get(t) {
            Obj::Type(d) => {
                assert_eq!(d.name, name_id);
                assert_eq!(d.flags, 0x0000);
                assert!(d.unary_op.is_some());
                assert!(d.subscr.is_some());
                assert!(d.getiter.is_some());
                assert!(d.binary_op.is_none());
                assert!(d.print.is_none());
            }
            _ => panic!("deque type is not a type"),
        }
    }
}
