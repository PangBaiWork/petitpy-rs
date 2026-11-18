//! `reversed` (`objreversed.zig`, 87 lines, 0 test blocks).
//!
//! `MpObjReversed { base, seq, cur_index: usize }` — the
//! sequence in `source`, the cursor in `index` (tag 114 in
//! `extra[0]`). `make_new` takes exactly 1 arg, no kwargs, and
//! FIRST tries `__reversed__` (`mp_load_method_maybe`): when
//! found, the method's result is returned directly (NOT
//! wrapped — `reversed(obj)` IS whatever `__reversed__`
//! returned, even a non-iterator; the caller `getiter`s it —
//! probed: `__reversed__` returning `5` gives
//! `TypeError("object not iterable")` later). Otherwise
//! `cur_index = mp_obj_get_int(mp_obj_len(seq))` and `iternext`
//! walks down via `mp_obj_subscr(seq, cur, SENTINEL)`.
//!
//! The Zig does NOT check for a sequence type: `reversed` on a
//! dict takes `len` then subscripts it (`reversed({'a': 1})`
//! raises `KeyError(0)` — probed). The port reproduces that
//! through the generic `len` + `subscr` slots.
//!
//! The `__reversed__` load is an interim (C12 owns method
//! lookup): it reads the operand type's dict for a
//! `"__reversed__"` entry. Instance-dict lookup is C12's. The
//! bound call passes the object as the method's first argument
//! through the call seam (the Zig's `dest[1]`).

use super::iter::{
    new_iter, ARG_NUM_TYPES_MISMATCH, CallOutcome, IterError, IterSeams, NOT_AN_ITERATOR,
    REVERSED_ITER_TAG, FLAG_ITER_IS_ITERNEXT,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_get_value, DictError};
use crate::objects::obj::{obj_get_int, obj_get_type, obj_len, ObjError};
use crate::objects::objtype::make_new::reversed_make_new_slot;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::SubscrArg;
use crate::objects::value::{GcRef, IteratorData, Obj, TypeData, Value};

/// The `reversed` ROM type (`objreversed.zig`, `MP_TYPE_FLAG_ITER_IS_ITERNEXT` —
/// the object is its own iterator; `make_new` + `getiter`, no
/// locals dict, no print slot — the VM's default print renders
/// `<reversed>`, as the reference does). Bound as the `reversed`
/// builtin in `Vm::build_builtins` (D5: the original registers
/// it in `modbuiltins.zig:1082`).
pub fn reversed_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"reversed").expect("reversed is a static qstr"),
        flags: FLAG_ITER_IS_ITERNEXT,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: Some(reversed_make_new_slot),
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

/// `mp_obj_reversed_make_new` (`objreversed.zig:17-51`).
pub fn reversed_make_new(
    ctx: &mut CallCtx,
    args: &[Value],
    n_kw: usize,
    seams: &mut IterSeams<'_>,
) -> Result<Value, IterError> {
    let sig_err = n_kw > 0 || args.len() != 1;
    let Some(&obj) = args.first() else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    // 1) `__reversed__` delegation (objreversed.zig:28-35).
    let rev_id = ctx.pool.find_strn(b"__reversed__");
    if rev_id != 0 {
        if let Some(type_) = obj_get_type(ctx.arena, ctx.types, obj) {
            let dict = match ctx.arena.get(type_) {
                Obj::Type(d) => Value::Heap(d.dict),
                _ => Value::None,
            };
            if let Value::Heap(_) = dict {
                match dict_get_value(ctx, dict, Value::Qstr(rev_id as u32)) {
                    Ok(fun) => {
                        // The bound call: the method with the
                        // object as its first (self) argument.
                        return match (seams.call)(ctx, fun, &[obj]) {
                            CallOutcome::Value(v) => {
                                if sig_err {
                                    Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH))
                                } else {
                                    Ok(v)
                                }
                            }
                            CallOutcome::Stop | CallOutcome::Raise => {
                                Err(IterError::PyRaise)
                            }
                        };
                    }
                    Err(DictError::KeyError(_)) => {}
                    Err(e) => return Err(map_dict_err(e)),
                }
            }
        }
    }
    // 2) Length + subscript fallback (objreversed.zig:36-50).
    let len_v = obj_len(ctx, ctx.types, obj).map_err(IterError::from)?;
    let len_i = obj_get_int(ctx, ctx.types, len_v).map_err(IterError::from)?;
    if sig_err {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    Ok(new_iter(
        ctx,
        IteratorData {
            source: obj,
            index: len_i as usize,
            extra: vec![Value::SmallInt(REVERSED_ITER_TAG)],
        },
    ))
}

fn map_dict_err(e: DictError) -> IterError {
    match e {
        DictError::OutOfMemory => IterError::OutOfMemory,
        _ => IterError::PyRaise,
    }
}

/// `mp_obj_reversed_iternext` (`objreversed.zig:53-86`):
/// `cur == 0` → stop; else `cur -= 1` and subscript. The
/// cursor is decremented BEFORE the subscript (the Zig's
/// order — a raising subscript leaves the iterator advanced).
/// A missing `subscr` slot raises `TypeError("object isn't
/// subscriptable")` (the `mp_obj_subscr` fallback, obj.zig:172
/// — probed: `reversed({1, 2})`).
pub fn reversed_iternext(ctx: &mut CallCtx, it: Value) -> Result<Option<Value>, IterError> {
    let (seq, cur) = match it {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Iterator(d) => (d.source, d.index),
            _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
        },
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    if cur == 0 {
        return Ok(None);
    }
    let next_cur = cur - 1;
    if let Value::Heap(r) = it {
        if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
            d.index = next_cur;
        }
    }
    let type_ = obj_get_type(ctx.arena, ctx.types, seq);
    let slot = match type_ {
        Some(t) => match ctx.arena.get(t) {
            Obj::Type(d) => d.subscr,
            _ => None,
        },
        None => None,
    };
    match slot {
        Some(f) => match f(ctx, seq, Value::SmallInt(next_cur as i64), SubscrArg::Get) {
            Ok(v) => Ok(Some(v)),
            Err(crate::pyerr::PyError::OutOfMemory) => Err(IterError::OutOfMemory),
            Err(crate::pyerr::PyError::PyRaise) => Err(IterError::PyRaise),
        },
        None => Err(IterError::Obj(ObjError::NotSubscriptable)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::iter::{CallOutcome, IterError, IterSeams};
    use super::super::tests::{collect_all, token_bytes, PanicSeams};
    use super::{reversed_iternext, reversed_make_new};
    use crate::memory::gc::CallCtx;
    use crate::objects::containers::objrange::range_make_new;
    use crate::objects::containers::tests::{empty_dict, Fixture};
    use crate::objects::obj::ObjError;
    use crate::objects::slots::UnaryOp;
    use crate::objects::value::{Obj, StrData, TypeData, Value};

    /// The `reversed` call seam is only used for the
    /// `__reversed__` delegation: the token `"rev-list"`
    /// returns the captured list value.
    fn rev_seams(
        list: Value,
    ) -> (
        impl FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome,
        impl FnMut(&mut CallCtx, Value, i64) -> CallOutcome,
    ) {
        let call = move |ctx: &mut CallCtx, fun: Value, _args: &[Value]| -> CallOutcome {
            match token_bytes(ctx, fun) {
                Some(b"rev-list") => CallOutcome::Value(list),
                _ => CallOutcome::Raise,
            }
        };
        let getitem =
            |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome { CallOutcome::Raise };
        (call, getitem)
    }

    fn collect_reversed(
        ctx: &mut CallCtx,
        seq: Value,
        list_for_rev: Value,
    ) -> Result<Vec<Value>, IterError> {
        let (mut call, mut getitem) = rev_seams(list_for_rev);
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let r = reversed_make_new(ctx, &[seq], 0, &mut seams)?;
        // `iternext` needs no seams (subscript is direct).
        let mut out = Vec::new();
        while let Some(v) = reversed_iternext(ctx, r)? {
            out.push(v);
        }
        Ok(out)
    }

    #[test]
    fn reversed_sequences() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
        ]);
        let tuple = fx.heap_tuple(vec![
            Value::SmallInt(10),
            Value::SmallInt(20),
        ]);
        let range_t = fx.types.range;
        let mut ctx = fx.ctx();
        assert_eq!(
            collect_reversed(&mut ctx, list, Value::None).unwrap(),
            vec![Value::SmallInt(3), Value::SmallInt(2), Value::SmallInt(1)]
        );
        assert_eq!(
            collect_reversed(&mut ctx, tuple, Value::None).unwrap(),
            vec![Value::SmallInt(20), Value::SmallInt(10)]
        );
        // `str` subscripts yield 1-char strings (probed:
        // reversed('ab') → ['b','a']) — but the fixture's plain
        // str type carries no LEN/subscr slots (the production
        // str type, C8a, does), so the str case is not exercised
        // here; list/tuple/range below cover the reversed logic.
        // A range subscripts through its own slot.
        let range = range_make_new(&mut ctx, range_t, &[Value::SmallInt(0), Value::SmallInt(3)], 0)
            .unwrap();
        assert_eq!(
            collect_reversed(&mut ctx, range, Value::None).unwrap(),
            vec![Value::SmallInt(2), Value::SmallInt(1), Value::SmallInt(0)]
        );
    }

    #[test]
    fn reversed_len_errors() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let it = {
            let mut ctx = fx.ctx();
            crate::objects::containers::objtuple::tuple_getiter(&mut ctx, list).unwrap()
        };
        let mut ctx = fx.ctx();
        // An iterator has no len (probed: `reversed(iter([1]))`).
        assert_eq!(
            collect_reversed(&mut ctx, it, Value::None),
            Err(IterError::Obj(ObjError::NoLen))
        );
        // An int has no len (probed: `reversed(5)`).
        assert_eq!(
            collect_reversed(&mut ctx, Value::SmallInt(5), Value::None),
            Err(IterError::Obj(ObjError::NoLen))
        );
    }

    #[test]
    fn reversed_dict_key_error() {
        let mut fx = Fixture::new();
        let d = fx.dict();
        let k = fx.heap_str(b"a");
        fx.store(d, k, Value::SmallInt(1));
        let mut ctx = fx.ctx();
        // len(dict) == 1, so the first next subscripts (dict, 0)
        // → KeyError (probed: `reversed({'a': 1})`). The key
        // payload is dropped at the slot boundary (C6), so the
        // test asserts the raise, not the payload.
        assert_eq!(
            collect_reversed(&mut ctx, d, Value::None),
            Err(IterError::PyRaise)
        );
    }

    #[test]
    fn reversed_not_subscriptable() {
        let mut fx = Fixture::new();
        let name_id = fx.pool.from_str("LenOnly").unwrap() as u32;
        // A type with a LEN slot but no `subscr` slot (the set's
        // shape — probed: `reversed({1, 2})` → "object isn't
        // subscriptable").
        let dict = empty_dict(&mut fx.arena, &fx.roots);
        fn len2(
            _ctx: &mut CallCtx,
            op: UnaryOp,
            _v: Value,
        ) -> Result<Value, crate::pyerr::PyError> {
            match op {
                UnaryOp::Len => Ok(Value::SmallInt(2)),
                _ => Err(crate::pyerr::PyError::PyRaise),
            }
        }
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
                unary_op: Some(len2),
                binary_op: None,
                call: None,
                getiter: None,
                subscr: None,
                buffer: None,
                print: None,
                namedtuple_fields: None,
                protocol: None,
            }),
            &fx.roots,
        );
        fx.roots.push(Value::Heap(t));
        let mut ctx = fx.ctx();
        let obj = Value::Heap(ctx.alloc(
            Some(t),
            Obj::Str(StrData {
                bytes: b"x".to_vec(),
            }),
        ));
        let r = {
            let (mut call, mut getitem) = rev_seams(Value::None);
            let mut seams = IterSeams {
                call: &mut call,
                getitem: &mut getitem,
            };
            reversed_make_new(&mut ctx, &[obj], 0, &mut seams).unwrap()
        };
        assert_eq!(
            reversed_iternext(&mut ctx, r),
            Err(IterError::Obj(ObjError::NotSubscriptable))
        );
    }

    #[test]
    fn reversed_dunder_reversed() {
        let mut fx = Fixture::new();
        let rev_id = fx.pool.from_str("__reversed__").unwrap() as u32;
        let tok_id = fx.pool.from_str("rev-list").unwrap() as u32;
        let name_id = fx.pool.from_str("Revvy").unwrap() as u32;
        let target = fx.heap_list(vec![Value::SmallInt(9), Value::SmallInt(8)]);
        let d = fx.dict();
        fx.store(d, Value::Qstr(rev_id), Value::Qstr(tok_id));
        let dict_ref = match d {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        let t = fx.arena.alloc(
            None,
            Obj::Type(TypeData {
                name: name_id,
                flags: 0,
                base: None,
                mro: Vec::new(),
                dict: dict_ref,
                attr: None,
                make_new: None,
                bases: None,
                unary_op: None,
                binary_op: None,
                call: None,
                getiter: None,
                subscr: None,
                buffer: None,
                print: None,
                namedtuple_fields: None,
                protocol: None,
            }),
            &fx.roots,
        );
        fx.roots.push(Value::Heap(t));
        let mut ctx = fx.ctx();
        let obj = Value::Heap(ctx.alloc(
            Some(t),
            Obj::Str(StrData {
                bytes: b"x".to_vec(),
            }),
        ));
        // The method's result is returned as-is (a list here —
        // the reference probe's `[9, 8]` delegation).
        let (mut call, mut getitem) = rev_seams(target);
        let mut seams = IterSeams {
            call: &mut call,
            getitem: &mut getitem,
        };
        let out = reversed_make_new(&mut ctx, &[obj], 0, &mut seams).unwrap();
        assert_eq!(out, target);
    }

    #[test]
    fn reversed_sig_errors() {
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![Value::SmallInt(1)]);
        let mut ctx = fx.ctx();
        let mut ps = PanicSeams::new();
        assert_eq!(
            reversed_make_new(&mut ctx, &[], 0, &mut ps.seams()),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            reversed_make_new(&mut ctx, &[list, list], 0, &mut ps.seams()),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
        assert_eq!(
            reversed_make_new(&mut ctx, &[list], 1, &mut ps.seams()),
            Err(IterError::TypeError("argument num/types mismatch"))
        );
    }

    #[test]
    fn reversed_iterator_shape() {
        // The iterator carries the sequence + cursor (tag 114).
        let mut fx = Fixture::new();
        let list = fx.heap_list(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let mut ctx = fx.ctx();
        let mut ps = PanicSeams::new();
        let r = reversed_make_new(&mut ctx, &[list], 0, &mut ps.seams()).unwrap();
        match r {
            Value::Heap(rr) => match ctx.arena.get(rr) {
                Obj::Iterator(d) => {
                    assert_eq!(d.source, list);
                    assert_eq!(d.index, 2);
                    assert_eq!(d.extra, vec![Value::SmallInt(114)]);
                }
                _ => panic!("reversed() did not return an iterator"),
            },
            _ => panic!("reversed() did not return a heap value"),
        }
        // `collect_all` through the unified dispatch also works.
        let items = collect_all(&mut ctx, r, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(2), Value::SmallInt(1)]);
    }
}
