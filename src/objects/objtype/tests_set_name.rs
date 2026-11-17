//! Tests for `mp_obj_new_type`'s `__set_name__` handling
//! (`typecreate.rs` `call_set_name_methods`): the snapshot-then-call
//! protocol (`objtype.zig:1840-2010`) — a `__set_name__` body may
//! mutate sibling slots, so the (fun, bound, owner, key) tuples
//! are snapshotted first and called afterwards. Shared
//! type-creation helpers live in `tests_typecreate.rs`.

use crate::memory::gc::CallCtx;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};

use super::errors::ObjTypeError;
use super::tests::Fx;
use super::tests_instance::builtin;
use super::tests_typecreate::{bases_tuple, empty_locals};
use super::typecreate::mp_obj_new_type;

/// `__set_name__` calls recorded here (test-only): `(owner arena
/// index, key qstr id)`. One test uses this static; tests run in
/// parallel, so no other test may touch it.
static SET_NAME_CALLS: std::sync::Mutex<Vec<(u32, u32)>> =
    std::sync::Mutex::new(Vec::new());

fn set_name_recorder(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // args: [bound descriptor, owner, key].
    let Value::Heap(owner) = args[1] else {
        return Err(PyError::PyRaise);
    };
    let Value::Qstr(key) = args[2] else {
        return Err(PyError::PyRaise);
    };
    SET_NAME_CALLS.lock().unwrap().push((owner.0, key));
    Ok(Value::None)
}

/// A `__set_name__` that ALSO stores a NEW descriptor entry into
/// the locals dict (the snapshot test: the new entry's
/// `__set_name__` must NOT be called — it was not in the snapshot).
fn set_name_adder(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    set_name_recorder(ctx, args)?;
    let Value::Heap(owner) = args[1] else {
        return Err(PyError::PyRaise);
    };
    // Store a fresh descriptor under `__class__` (a static qstr).
    let class_q = static_qstr_id(b"__class__").expect("__class__ is a static qstr");
    let dict = match ctx.arena.get(owner) {
        Obj::Type(d) => Value::Heap(d.dict),
        _ => return Err(PyError::PyRaise),
    };
    crate::objects::containers::objdict::dict_store(
        ctx,
        dict,
        Value::Qstr(class_q),
        Value::SmallInt(102),
    )
    .map_err(|_| PyError::PyRaise)?;
    Ok(Value::None)
}

#[test]
fn new_type_set_name_snapshot() {
    let mut fx = Fx::new();
    SET_NAME_CALLS.lock().unwrap().clear();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    // locals = {k1: desc1(101)}; desc1's `__set_name__` adds
    // {__class__: desc2(102)} — desc2 must NOT be called.
    let k1 = fx.q(b"k1");
    let desc1 = Value::SmallInt(101);
    let mut ctx = fx.ctx();
    crate::objects::containers::objdict::dict_store(
        &mut ctx, locals, Value::Qstr(k1), desc1,
    )
    .expect("fixture store");
    drop(ctx);
    let fun1 = builtin(&mut fx, 3, "__set_name__", set_name_adder);
    let fun2 = builtin(&mut fx, 3, "__set_name__", set_name_recorder);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let name = fx.q(b"D");
    let t = fx
        .with_load(
            move |_ctx, value, attr| {
                let set_name_q =
                    static_qstr_id(b"__set_name__").expect("__set_name__ is static");
                if attr != set_name_q {
                    return None;
                }
                match value {
                    Value::SmallInt(101) => Some((fun1, Some(desc1))),
                    Value::SmallInt(102) => Some((fun2, Some(Value::SmallInt(102)))),
                    _ => None,
                }
            },
            |ctx, seams| mp_obj_new_type(ctx, name, bases, locals, seams),
        )
        .expect("type with __set_name__");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    let calls = SET_NAME_CALLS.lock().unwrap();
    // Exactly ONE call: (owner = the new type, key = k1).
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], (tr.0, k1));
}

fn set_name_raiser(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

#[test]
fn new_type_set_name_raise_propagates() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let k1 = fx.q(b"k1");
    let desc1 = Value::SmallInt(101);
    let mut ctx = fx.ctx();
    crate::objects::containers::objdict::dict_store(
        &mut ctx, locals, Value::Qstr(k1), desc1,
    )
    .expect("fixture store");
    drop(ctx);
    let fun1 = builtin(&mut fx, 3, "__set_name__", set_name_raiser);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let name = fx.q(b"D");
    let err = fx
        .with_load(
            move |_ctx, value, attr| {
                let set_name_q =
                    static_qstr_id(b"__set_name__").expect("__set_name__ is static");
                if attr == set_name_q && value == desc1 {
                    Some((fun1, Some(desc1)))
                } else {
                    None
                }
            },
            |ctx, seams| mp_obj_new_type(ctx, name, bases, locals, seams),
        )
        .expect_err("raise propagates");
    assert_eq!(err, ObjTypeError::Raised);
}
