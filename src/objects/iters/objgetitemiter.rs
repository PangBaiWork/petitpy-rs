//! Getitem iterator (`objgetitemiter.zig`, 101 lines, 0 test
//! blocks).
//!
//! `MpObjGetitemIterFull { base, args[3] }` — in Rust the
//! object is `source`, the next index is `index`, the tag 115
//! is in `extra[0]`. The Zig stores the BOUND `__getitem__`
//! method (loaded eagerly at `getiter` time) and calls it on
//! each `next`; here the call + the `excIsStopOrIndex`
//! classification are one seam operation
//! ([`IterSeams::getitem`]) because the bound method value
//! cannot be produced without C12's method lookup — see the
//! module docs in [`super::iter`]. KNOWN DIVERGENCE: the Zig
//! loads the method eagerly (a missing `__getitem__` fails at
//! `getiter` time); the Rust seam loads it lazily at each
//! `next` step. C13 may hoist the load into `getiter` once
//! method lookup exists; the observable `next()` behaviour is
//! identical either way.
//!
//! Classification (probed): `StopIteration` or `IndexError`
//! from `__getitem__` ends the iteration; `ValueError`,
//! `KeyError`, or anything else propagates.

use super::iter::{new_iter, CallOutcome, IterError, IterSeams, GETITEM_ITER_TAG, NOT_AN_ITERATOR};
use crate::memory::gc::CallCtx;
use crate::objects::value::{IteratorData, Obj, Value};

/// Build a getitem iterator for `obj` (the `getiter` fallback
/// constructs it; `new_iter` allocates it).
pub fn new_getitem_iter(ctx: &mut CallCtx, obj: Value) -> Value {
    new_iter(
        ctx,
        IteratorData {
            source: obj,
            index: 0,
            extra: vec![Value::SmallInt(GETITEM_ITER_TAG)],
        },
    )
}

/// `mp_obj_getitem_iter_iternext` (`objgetitemiter.zig:60-99`):
/// call `__getitem__(index)`; `Stop` ends the iteration, `Value`
/// advances the index and yields, `Raise` propagates.
pub fn getitem_iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let (obj, index) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => (d.source, d.index),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    match (seams.getitem)(ctx, obj, index as i64) {
        CallOutcome::Value(v) => {
            if let Value::Heap(r) = it {
                if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
                    d.index = index.wrapping_add(1);
                }
            }
            Ok(Some(v))
        }
        CallOutcome::Stop => Ok(None),
        CallOutcome::Raise => Err(IterError::PyRaise),
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::{CallOutcome, IterError, IterSeams};
    use super::super::tests::{collect_all, heap_str, token_bytes};
    use super::getitem_iternext;
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::objtuple::tuple_subscr;
    use crate::objects::containers::tests::{empty_dict, Fixture};
    use crate::objects::value::{Obj, TypeData, Value};

    /// The getitem seam for these tests. A heap `List` is
    /// indexed directly (`Stop` past the end — the list's
    /// `__getitem__` raising `IndexError`); a heap `Str`
    /// yields 1-char strings; `Qstr` tokens simulate
    /// `__getitem__` behaviours: `"raises-value"` (ValueError
    /// at 1), `"raises-key"` (KeyError at 1), `"stops-at-2"`
    /// (IndexError/StopIteration at 2).
    fn getitem_seam() -> impl FnMut(&mut CallCtx, Value, i64) -> CallOutcome {
        |ctx, obj, idx| match obj {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::List(l) => {
                    let items = l.items.clone();
                    if idx >= 0 && (idx as usize) < items.len() {
                        CallOutcome::Value(items[idx as usize])
                    } else {
                        CallOutcome::Stop
                    }
                }
                Obj::Tuple(t) => {
                    let items = t.clone();
                    if idx >= 0 && (idx as usize) < items.len() {
                        CallOutcome::Value(items[idx as usize])
                    } else {
                        CallOutcome::Stop
                    }
                }
                Obj::Str(s) => {
                    let bytes = s.bytes.clone();
                    if idx >= 0 && (idx as usize) < bytes.len() {
                        let ch = std::str::from_utf8(&bytes[idx as usize..idx as usize + 1])
                            .unwrap();
                        CallOutcome::Value(heap_str(ctx, ch))
                    } else {
                        CallOutcome::Stop
                    }
                }
                _ => CallOutcome::Raise,
            },
            Value::Qstr(_) => match token_bytes(ctx, obj) {
                Some(b"raises-value") | Some(b"raises-key") if idx >= 1 => CallOutcome::Raise,
                Some(b"stops-at-2") if idx >= 2 => CallOutcome::Stop,
                _ => CallOutcome::Value(Value::SmallInt(idx * 10)),
            },
            _ => CallOutcome::Raise,
        }
    }

    fn seams<'a>(
        call: &'a mut dyn FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome,
        getitem: &'a mut dyn FnMut(&mut CallCtx, Value, i64) -> CallOutcome,
    ) -> IterSeams<'a> {
        IterSeams { call, getitem }
    }

    /// A panicking call closure (getitem tests never call it).
    fn no_call() -> impl FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome {
        |_, _, _| panic!("getitem tests must not call the call seam")
    }

    #[test]
    fn getitem_iter_over_list() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![
            Value::SmallInt(0),
            Value::SmallInt(10),
            Value::SmallInt(20),
        ]);
        let mut ctx = fx.ctx();
        let it = super::new_getitem_iter(&mut ctx, list);
        let mut g = getitem_seam();
        let mut call = no_call();
        let items = collect_all(&mut ctx, it, &mut seams(&mut call, &mut g)).unwrap();
        assert_eq!(
            items,
            vec![Value::SmallInt(0), Value::SmallInt(10), Value::SmallInt(20)]
        );
    }

    #[test]
    fn getitem_iter_classification() {
        let mut fx = Fixture::new();
        let rv_id = fx.pool.from_str("raises-value").unwrap() as u32;
        let rk_id = fx.pool.from_str("raises-key").unwrap() as u32;
        let st_id = fx.pool.from_str("stops-at-2").unwrap() as u32;
        let mut ctx = fx.ctx();
        // ValueError at 1 propagates (probed).
        let it = super::new_getitem_iter(&mut ctx, Value::Qstr(rv_id));
        let mut g = getitem_seam();
        let mut call = no_call();
        assert_eq!(
            collect_all(&mut ctx, it, &mut seams(&mut call, &mut g)),
            Err(IterError::PyRaise)
        );
        // KeyError at 1 propagates too — only StopIteration /
        // IndexError end the iteration (probed).
        let it = super::new_getitem_iter(&mut ctx, Value::Qstr(rk_id));
        let mut g = getitem_seam();
        let mut call = no_call();
        assert_eq!(
            collect_all(&mut ctx, it, &mut seams(&mut call, &mut g)),
            Err(IterError::PyRaise)
        );
        // Stop at 2 ends the iteration (probed).
        let it = super::new_getitem_iter(&mut ctx, Value::Qstr(st_id));
        let mut g = getitem_seam();
        let mut call = no_call();
        let items = collect_all(&mut ctx, it, &mut seams(&mut call, &mut g)).unwrap();
        assert_eq!(items, vec![Value::SmallInt(0), Value::SmallInt(10)]);
    }

    #[test]
    fn getitem_iter_via_getiter_fallback() {
        let mut fx = Fixture::new();
        let name_id = fx.pool.from_str("SubOnly").unwrap() as u32;
        // A type with a `subscr` slot but no `getiter` slot (the
        // shape the Zig's `mp_getiter` falls back on): a heap
        // tuple whose header type is this custom type. The
        // fixture's plain str type carries neither slot, so the
        // str case is not exercised here — the production str
        // type (C8a) has the subscr slot, and the reference
        // probe gives iter('ab') → ['a', 'b'].
        let dict = empty_dict(&mut fx.arena, &fx.roots);
        let t = fx.arena.alloc(
            None,
            Obj::Type(TypeData {
                name: name_id,
                flags: 0,
                base: None,
                mro: Vec::new(),
                dict,
                attr: None,
                make_new: None,
                bases: None,
                unary_op: None,
                binary_op: None,
                call: None,
                getiter: None,
                subscr: Some(tuple_subscr),
                buffer: None,
                print: None,
                namedtuple_fields: None,
                protocol: None,
            }),
            &fx.roots,
        );
        fx.roots.push(Value::Heap(t));
        let obj = Value::Heap(fx.arena.alloc(
            Some(t),
            Obj::Tuple(vec![Value::SmallInt(10), Value::SmallInt(20)]),
            &fx.roots,
        ));
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        // No `getiter` slot, but a `subscr` slot → the getitem
        // iterator, same values as direct indexing, same
        // `'iterator'` type name.
        let it = super::super::iter::getiter(&mut ctx, obj).unwrap();
        match it {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Iterator(d) => {
                    assert_eq!(d.source, obj);
                    assert_eq!(d.extra, vec![Value::SmallInt(115)]);
                }
                _ => panic!("getiter(obj) did not return an iterator"),
            },
            _ => panic!("getiter(obj) did not return a heap value"),
        }
        let mut g = getitem_seam();
        let mut call = no_call();
        let items = collect_all(&mut ctx, it, &mut seams(&mut call, &mut g)).unwrap();
        assert_eq!(items, vec![Value::SmallInt(10), Value::SmallInt(20)]);
        // A list takes the getiter slot, not the fallback.
        let lit = super::super::iter::getiter(&mut ctx, list).unwrap();
        match lit {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Iterator(d) => {
                    assert_eq!(d.extra, vec![Value::SmallInt(100)]);
                }
                _ => panic!("getiter(list) did not return an iterator"),
            },
            _ => panic!("getiter(list) did not return a heap value"),
        }
        // An iterator is its own iterator; a non-iterable raises.
        let again = super::super::iter::getiter(&mut ctx, lit).unwrap();
        assert_eq!(again, lit);
        assert_eq!(
            super::super::iter::getiter(&mut ctx, Value::SmallInt(5)),
            Err(IterError::TypeError("object not iterable"))
        );
    }

    #[test]
    fn iternext_on_non_iterator() {
        let mut fx = Fixture::new();
        let mut ctx = fx.ctx();
        let mut g = getitem_seam();
        let mut call = no_call();
        assert_eq!(
            getitem_iternext(&mut ctx, Value::SmallInt(5), &mut seams(&mut call, &mut g)),
            Err(IterError::TypeError("object not an iterator"))
        );
        assert_eq!(
            super::super::iter::iternext(&mut ctx, Value::SmallInt(5), &mut seams(&mut call, &mut g)),
            Err(IterError::TypeError("object not an iterator"))
        );
    }
}
