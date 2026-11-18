//! Tests for `instance.rs` (objtype.zig:1050-1500).
//!
//! The seams are the stub seams unless a test says otherwise
//! (`with_load` for a custom `load_maybe`, `with_subscr` for a
//! custom `subscr`). FunBuiltin members are used where the test
//! needs the member CALL to run (their `call` slots are live);
//! FunBc members are used where the test needs the BINDING shape
//! only (their call slot raises until C13's runner lands).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::{
    fun_bc_type_data, fun_builtin_0_type_data, fun_builtin_1_type_data,
    fun_builtin_2_type_data, fun_builtin_3_type_data, new_fun_bc,
};
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objproperty::property_make_new;
use crate::objects::containers::tests::empty_dict;
use crate::objects::obj::{FLAG_HAS_SPECIAL_ACCESSORS, FLAG_ITER_IS_ITERNEXT};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, BuiltinFn, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{
    wrap_small_int, FunBuiltinData, GcRef, InstanceData, MapData, Obj, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};

use super::instance::*;
use super::seams::SubscrError;
use super::tests::Fx;

/// Allocate a `fun_builtin_N` type (header type = the `type`
/// type) for a test builtin's `call` slot.
pub(crate) fn fun_type(fx: &mut Fx, n: u8) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = match n {
        0 => fun_builtin_0_type_data(dict),
        1 => fun_builtin_1_type_data(dict),
        2 => fun_builtin_2_type_data(dict),
        _ => fun_builtin_3_type_data(dict),
    };
    let r = fx.arena.alloc(Some(fx.types.type_), Obj::Type(data), &fx.roots);
    fx.roots.push(Value::Heap(r));
    r
}

/// A heap `FunBuiltin` value (header type = `fun_type(fx, n)`).
pub(crate) fn builtin(fx: &mut Fx, n: u8, name: &str, func: BuiltinFn) -> Value {
    let type_ = fun_type(fx, n);
    let name_id = static_qstr_id(name.as_bytes()).unwrap_or_else(|| fx.q(name.as_bytes()));
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(
        Some(type_),
        Obj::FunBuiltin(FunBuiltinData {
            name: name_id,
            n_args: fun_n_args(n as u16, n as u16),
            func,
            kw_func: None,
        }),
    ))
}

/// A heap `FunBc` value (its call slot raises until C13 — used
/// only where the test needs the BINDING shape).
pub(crate) fn fun_bc(fx: &mut Fx, name: &str) -> Value {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let type_ = fx.arena.alloc(
        Some(fx.types.type_),
        Obj::Type(fun_bc_type_data(dict)),
        &fx.roots,
    );
    fx.roots.push(Value::Heap(type_));
    let name_id = static_qstr_id(name.as_bytes()).unwrap_or_else(|| fx.q(name.as_bytes()));
    let mut ctx = fx.ctx();
    Value::Heap(new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None))
}

/// A heap `Instance` of `type_` with `subobj` native-base slots.
fn new_instance(fx: &mut Fx, type_: GcRef, subobj: Vec<Value>) -> Value {
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj,
        }),
    ))
}

/// Give a fixture user type the eight `mp_obj_instance_*` slots
/// (what `mp_obj_new_type` installs — typecreate.rs).
pub(crate) fn with_instance_slots(fx: &mut Fx, type_: GcRef) {
    if let Obj::Type(d) = fx.arena.get_mut(type_) {
        d.unary_op = Some(instance_unary_op_slot);
        d.binary_op = Some(instance_binary_op_slot);
        d.call = Some(instance_call_slot);
        d.getiter = Some(instance_getiter_slot);
        d.subscr = Some(instance_subscr_slot);
        d.buffer = Some(instance_get_buffer_slot);
        d.print = Some(instance_print_slot);
        d.attr = Some(instance_attr);
    }
}

/// A user type `C` (base `object`, MRO `[C, object]`) with the
/// instance slots installed.
pub(crate) fn class_c(fx: &mut Fx) -> GcRef {
    let object = fx.types.object;
    let c = fx.user_type(b"C", Some(object), vec![]);
    // The fixture `user_type` takes the MRO as given; repair it
    // to [C, object] (the C12a tests build it the same way).
    // Also set HAS_SPECIAL_ACCESSORS: `mp_obj_new_type`'s accessor
    // scan sets it for a type carrying a property/descriptor
    // member (the fixture's USER_FLAGS = 0x021C does not include
    // the bit), and the property/descriptor tests need the branch.
    if let Obj::Type(d) = fx.arena.get_mut(c) {
        d.mro = vec![c, object];
        d.flags |= FLAG_HAS_SPECIAL_ACCESSORS;
    }
    with_instance_slots(fx, c);
    c
}

pub(crate) fn ret_42(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(42))
}

pub(crate) fn ret_first_arg(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[0])
}

fn ret_second_arg(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[1])
}

pub(crate) fn ret_none(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::None)
}

#[test]
fn load_attr_members_map_and_store_delete() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let x_q = fx.q(b"x");
    // Store through the public store path, load through the load path.
    let stored = fx.with_seams(|ctx, seams| {
        instance_store_attr(ctx, inst, x_q, Some(Value::SmallInt(7)), seams)
    });
    assert_eq!(stored, Ok(true));
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, x_q, seams));
    assert_eq!(loaded, Ok(Some(Value::SmallInt(7))));
    // Delete: returns true, the member is gone.
    let deleted = fx.with_seams(|ctx, seams| instance_store_attr(ctx, inst, x_q, None, seams));
    assert_eq!(deleted, Ok(true));
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, x_q, seams));
    assert_eq!(loaded, Ok(None));
    // Delete a missing member: not handled.
    let deleted = fx.with_seams(|ctx, seams| instance_store_attr(ctx, inst, x_q, None, seams));
    assert_eq!(deleted, Ok(false));
}

#[test]
fn load_attr_dict_returns_readonly_copy() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let x_q = fx.q(b"x");
    fx.with_seams(|ctx, seams| {
        instance_store_attr(ctx, inst, x_q, Some(Value::SmallInt(3)), seams).unwrap();
    });
    let dict_q = static_qstr_id(b"__dict__").unwrap();
    let dict = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, dict_q, seams));
    let dict = dict.unwrap().expect("__dict__ loads");
    let mut ctx = fx.ctx();
    let v = dict_get_value(&mut ctx, dict, Value::Qstr(x_q)).expect("member in __dict__");
    assert_eq!(v, Value::SmallInt(3));
}

#[test]
fn load_attr_fun_bc_binds_and_builtin_does_not() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    // A FunBc member binds (BINDS_SELF, no BUILTIN_FUN).
    let m_q = fx.q(b"m");
    let f = fun_bc(&mut fx, "m");
    fx.set_local(c, m_q, f);
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, m_q, seams));
    let loaded = loaded.unwrap().expect("method loads");
    let Value::Heap(r) = loaded else {
        panic!("bound method is a heap value");
    };
    match fx.arena.get(r) {
        Obj::BoundMeth(d) => {
            assert_eq!(d.func, f);
            assert_eq!(d.self_, inst);
        }
        _ => panic!("FunBc member binds to a BoundMeth"),
    }
    // A FunBuiltin member on a user type does NOT bind
    // (BINDS_SELF | BUILTIN_FUN — a staticmethod).
    let n_q = fx.q(b"n");
    let b = builtin(&mut fx, 1, "n", ret_42);
    fx.set_local(c, n_q, b);
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, n_q, seams));
    assert_eq!(loaded, Ok(Some(b)));
}

#[test]
fn load_attr_property_getter_and_no_getter() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let p_q = fx.q(b"p");
    // Property with a getter returning 99.
    let getter = builtin(&mut fx, 1, "get", ret_42);
    let prop = {
        let property = fx.types.property;
        let mut ctx = fx.ctx();
        property_make_new(&mut ctx, property, &[getter], 0).expect("property")
    };
    fx.set_local(c, p_q, prop);
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, p_q, seams));
    assert_eq!(loaded, Ok(Some(Value::SmallInt(42))));
    // Property without a getter: the lookup FAILS (no __getattr__
    // fallback — objtype.zig:1320).
    let prop2 = {
        let property = fx.types.property;
        let mut ctx = fx.ctx();
        property_make_new(&mut ctx, property, &[], 0).expect("property")
    };
    fx.set_local(c, p_q, prop2);
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, p_q, seams));
    // E2: a getter-less property RAISES (probed on the reference:
    // `AttributeError("no attribute 'prop'")` — objtype.zig:1320
    // leaves dest NULL without the SENTINEL, so the caller raises)
    // instead of returning the property object or `Ok(None)`.
    assert_eq!(loaded, Err(PyError::PyRaise));
    let raise = fx.pending_raise.as_ref().expect("pending raise recorded");
    assert_eq!(raise.exc_name, "AttributeError");
    assert_eq!(raise.message.as_deref(), Some("no attribute 'p'"));
}

#[test]
fn load_attr_descriptor_get_and_getattr_fallback() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let d_q = fx.q(b"d");
    let member = Value::SmallInt(5);
    fx.set_local(c, d_q, member);
    // The member has a `__get__` (via the load seam): the load
    // returns `__get__(self, type)`'s result — the instance.
    let get_q = static_qstr_id(b"__get__").unwrap();
    let get_fun = builtin(&mut fx, 3, "__get__", ret_second_arg);
    let load = move |_ctx: &mut CallCtx, obj: Value, attr: u32| {
        if obj == member && attr == get_q {
            Some((get_fun, Some(member)))
        } else {
            None
        }
    };
    let loaded = fx.with_load(load, |ctx, seams| instance_load_attr(ctx, inst, d_q, seams));
    assert_eq!(loaded, Ok(Some(inst)));
    // `__getattr__` fallback for a missing attribute.
    let getattr_q = static_qstr_id(b"__getattr__").unwrap();
    let getattr_fun = builtin(&mut fx, 2, "__getattr__", ret_second_arg);
    let missing_q = fx.q(b"zz");
    let load = move |_ctx: &mut CallCtx, obj: Value, attr: u32| {
        if obj == inst && attr == getattr_q {
            Some((getattr_fun, Some(inst)))
        } else {
            None
        }
    };
    let loaded = fx.with_load(load, |ctx, seams| instance_load_attr(ctx, inst, missing_q, seams));
    assert_eq!(loaded, Ok(Some(Value::Qstr(missing_q))));
    // The descriptor dunders are guarded: `__get__` itself is
    // never delegated to `__getattr__`.
    let loaded = fx.with_load(load, |ctx, seams| instance_load_attr(ctx, inst, get_q, seams));
    assert_eq!(loaded, Ok(None));
}

#[test]
fn store_attr_property_setter_deleter_and_no_setter() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let p_q = fx.q(b"p");
    let setter = builtin(&mut fx, 2, "set", ret_none);
    let deleter = builtin(&mut fx, 1, "del", ret_none);
    // Property (None, setter, deleter) positionally.
    let prop = {
        let property = fx.types.property;
        let mut ctx = fx.ctx();
        property_make_new(&mut ctx, property, &[Value::None, setter, deleter], 0)
            .expect("property")
    };
    fx.set_local(c, p_q, prop);
    let stored = fx.with_seams(|ctx, seams| {
        instance_store_attr(ctx, inst, p_q, Some(Value::SmallInt(1)), seams)
    });
    assert_eq!(stored, Ok(true));
    let deleted =
        fx.with_seams(|ctx, seams| instance_store_attr(ctx, inst, p_q, None, seams));
    assert_eq!(deleted, Ok(true));
    // Property with only a getter (no setter): not handled, and
    // the value does NOT land in the members map.
    let getter = builtin(&mut fx, 1, "get", ret_42);
    let prop2 = {
        let property = fx.types.property;
        let mut ctx = fx.ctx();
        property_make_new(&mut ctx, property, &[getter], 0).expect("property")
    };
    fx.set_local(c, p_q, prop2);
    let stored = fx.with_seams(|ctx, seams| {
        instance_store_attr(ctx, inst, p_q, Some(Value::SmallInt(1)), seams)
    });
    assert_eq!(stored, Ok(false));
    // Nothing landed in the members map (loading the property
    // would still invoke its getter, so read the map directly).
    let Value::Heap(r) = inst else { panic!("instance") };
    match fx.arena.get(r) {
        Obj::Instance(d) => assert!(d.members.find_qstr(&fx.pool, p_q).is_none()),
        _ => panic!("not an instance"),
    }
}

#[test]
fn store_attr_descriptor_set_and_setattr_hook() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let d_q = fx.q(b"d");
    let member = Value::SmallInt(5);
    fx.set_local(c, d_q, member);
    // Descriptor `__set__` (via the load seam).
    let set_q = static_qstr_id(b"__set__").unwrap();
    let set_fun = builtin(&mut fx, 3, "__set__", ret_none);
    let load = move |_ctx: &mut CallCtx, obj: Value, attr: u32| {
        if obj == member && attr == set_q {
            Some((set_fun, Some(member)))
        } else {
            None
        }
    };
    let stored = fx.with_load(load, |ctx, seams| {
        instance_store_attr(ctx, inst, d_q, Some(Value::SmallInt(9)), seams)
    });
    assert_eq!(stored, Ok(true));
    // The descriptor handled it: nothing landed in the members map.
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, d_q, seams));
    assert_eq!(loaded, Ok(Some(member)));
    // `__setattr__` hook for a plain attribute.
    let setattr_q = static_qstr_id(b"__setattr__").unwrap();
    let hook_fun = builtin(&mut fx, 3, "__setattr__", ret_none);
    let y_q = fx.q(b"y");
    let load = move |_ctx: &mut CallCtx, obj: Value, attr: u32| {
        if obj == inst && attr == setattr_q {
            Some((hook_fun, Some(inst)))
        } else {
            None
        }
    };
    let stored = fx.with_load(load, |ctx, seams| {
        instance_store_attr(ctx, inst, y_q, Some(Value::SmallInt(2)), seams)
    });
    assert_eq!(stored, Ok(true));
    // The hook handled it: nothing landed in the members map.
    let loaded = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, y_q, seams));
    assert_eq!(loaded, Ok(None));
}

#[test]
fn unary_hash_default_eq_unhashable_and_member_truncates() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    // No `__hash__`, no `__eq__`: the identity hash.
    let h = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::Hash, inst, seams));
    let Value::Heap(r) = inst else { panic!("instance") };
    assert_eq!(h, Ok(Some(Value::SmallInt(r.0 as i64))));
    // `__eq__` without `__hash__`: unhashable.
    let eq_q = static_qstr_id(b"__eq__").unwrap();
    let eq_fun = builtin(&mut fx, 2, "__eq__", ret_42);
    fx.set_local(c, eq_q, eq_fun);
    let h = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::Hash, inst, seams));
    assert_eq!(h, Ok(None));
    // `__hash__` member returning 2**62: tag-wrapped to -(2**62).
    let hash_q = static_qstr_id(b"__hash__").unwrap();
    let hash_fun = builtin(&mut fx, 1, "__hash__", |_ctx, _args| {
        Ok(Value::SmallInt(1 << 62))
    });
    fx.set_local(c, hash_q, hash_fun);
    let h = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::Hash, inst, seams));
    assert_eq!(h, Ok(Some(Value::SmallInt(wrap_small_int(1 << 62)))));
    assert_eq!(wrap_small_int(1 << 62), -(1 << 62));
}

#[test]
fn unary_int_maybe_requires_int() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let int_q = static_qstr_id(b"__int__").unwrap();
    let int_fun = builtin(&mut fx, 1, "__int__", ret_42);
    fx.set_local(c, int_q, int_fun);
    let v = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::IntMaybe, inst, seams));
    assert_eq!(v, Ok(Some(Value::SmallInt(42))));
    // A non-int result is a bare TypeError.
    let int_fun2 = builtin(&mut fx, 1, "__int__", |_ctx, _args| Ok(Value::None));
    fx.set_local(c, int_q, int_fun2);
    let v = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::IntMaybe, inst, seams));
    assert!(v.is_err());
}

#[test]
fn binary_add_member_contains_boolifies_and_native_base() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    // `__add__` member: an unbound builtin on a user type is
    // called with [rhs] only (no self) — returns rhs.
    let add_q = static_qstr_id(b"__add__").unwrap();
    let add_fun = builtin(&mut fx, 1, "__add__", ret_first_arg);
    fx.set_local(c, add_q, add_fun);
    let v = fx.with_seams(|ctx, seams| {
        instance_binary_op(ctx, BinaryOp::Add, inst, Value::SmallInt(7), seams)
    });
    assert_eq!(v, Ok(Some(Value::SmallInt(7))));
    // `__contains__` member: the result is bool-ified.
    let contains_q = static_qstr_id(b"__contains__").unwrap();
    let contains_fun = builtin(&mut fx, 1, "__contains__", |_ctx, _args| {
        Ok(Value::SmallInt(0))
    });
    fx.set_local(c, contains_q, contains_fun);
    let v = fx.with_seams(|ctx, seams| {
        instance_binary_op(ctx, BinaryOp::Contains, inst, Value::SmallInt(1), seams)
    });
    assert_eq!(v, Ok(Some(Value::Bool(false))));
    // Native base: CI(int) with subobj0 = 5; the SENTINEL path
    // runs the runtime op on the base: 5 + 1 == 6.
    let int_type = fx.types.int;
    let ci = fx.user_type(b"CI", Some(int_type), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(ci) {
        d.mro = vec![ci, int_type, fx.types.object];
    }
    with_instance_slots(&mut fx, ci);
    let ci_inst = new_instance(&mut fx, ci, vec![Value::SmallInt(5)]);
    let v = fx.with_seams(|ctx, seams| {
        instance_binary_op(ctx, BinaryOp::Add, ci_inst, Value::SmallInt(1), seams)
    });
    assert_eq!(v, Ok(Some(Value::SmallInt(6))));
    // Same instance, unary HASH on the native base: 5.
    let v = fx.with_seams(|ctx, seams| instance_unary_op(ctx, UnaryOp::Hash, ci_inst, seams));
    assert_eq!(v, Ok(Some(Value::SmallInt(5))));
}

#[test]
fn getiter_member_and_native_iternest() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let iter_q = static_qstr_id(b"__iter__").unwrap();
    let iter_fun = builtin(&mut fx, 0, "__iter__", ret_42);
    fx.set_local(c, iter_q, iter_fun);
    let v = fx.with_seams(|ctx, seams| instance_getiter(ctx, inst, seams));
    assert_eq!(v, Ok(Value::SmallInt(42)));
    // Native base whose type is ITER_IS_ITERNEXT: getiter returns
    // subobj0 directly. The parent must be NATIVE (the lookup's
    // slot fast path only fires for native types — objtype.zig:904)
    // and carry a getiter slot (else the lookup misses before the
    // flag is ever read). The slot itself is never called here.
    let it_name = fx.q(b"IT");
    let object = fx.types.object;
    let type_ = fx.types.type_;
    let it_dict = empty_dict(&mut fx.arena, &fx.roots);
    let it_type = fx.arena.alloc(
        Some(type_),
        Obj::Type(TypeData {
            name: it_name,
            flags: FLAG_ITER_IS_ITERNEXT,
            base: Some(object),
            bases: None,
            mro: vec![object],
            dict: it_dict,
            namedtuple_fields: None,
            protocol: None,
            print: None,
            unary_op: None,
            binary_op: None,
            subscr: None,
            buffer: None,
            getiter: Some(|_ctx, v| Ok(v)),
            call: None,
            attr: None,
            make_new: None,
        }),
        &fx.roots,
    );
    fx.roots.push(Value::Heap(it_type));
    let it_inst = new_instance(&mut fx, it_type, vec![]);
    let ci = fx.user_type(b"CI2", Some(it_type), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(ci) {
        d.mro = vec![ci, it_type, fx.types.object];
    }
    with_instance_slots(&mut fx, ci);
    let ci_inst = new_instance(&mut fx, ci, vec![it_inst]);
    let v = fx.with_seams(|ctx, seams| instance_getiter(ctx, ci_inst, seams));
    assert_eq!(v, Ok(it_inst));
}

#[test]
fn subscr_getitem_setitem_and_missing() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let get_q = static_qstr_id(b"__getitem__").unwrap();
    let get_fun = builtin(&mut fx, 1, "__getitem__", ret_first_arg);
    fx.set_local(c, get_q, get_fun);
    let v = fx.with_seams(|ctx, seams| {
        instance_subscr(ctx, inst, Value::SmallInt(3), SubscrArg::Get, seams)
    });
    assert_eq!(v, Ok(Value::SmallInt(3)));
    let set_q = static_qstr_id(b"__setitem__").unwrap();
    let set_fun = builtin(&mut fx, 2, "__setitem__", ret_42);
    fx.set_local(c, set_q, set_fun);
    let v = fx.with_seams(|ctx, seams| {
        instance_subscr(ctx, inst, Value::SmallInt(3), SubscrArg::Set(Value::SmallInt(9)), seams)
    });
    assert_eq!(v, Ok(Value::None));
    // `__missing__`: the subscr seam reports KeyError, the class
    // defines `__missing__` returning the key. A dict subclass
    // with no `__getitem__` member (the lookup walks to the dict
    // parent for the SENTINEL path) and a subobj0 present.
    let dict_type = fx.types.dict;
    let c3 = fx.user_type(b"CD", Some(dict_type), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(c3) {
        d.mro = vec![c3, dict_type, fx.types.object];
        d.flags |= FLAG_HAS_SPECIAL_ACCESSORS;
    }
    with_instance_slots(&mut fx, c3);
    let dict_v = Value::Heap(empty_dict(&mut fx.arena, &fx.roots));
    let inst3 = new_instance(&mut fx, c3, vec![dict_v]);
    let missing_q = static_qstr_id(b"__missing__").unwrap();
    let missing_fun = builtin(&mut fx, 1, "__missing__", ret_first_arg);
    fx.set_local(c3, missing_q, missing_fun);
    let v = fx.with_subscr(
        |_ctx, _base, _index, _arg| Err(SubscrError::KeyError),
        |ctx, seams| instance_subscr(ctx, inst3, Value::SmallInt(11), SubscrArg::Get, seams),
    );
    assert_eq!(v, Ok(Value::SmallInt(11)));
    // Without `__missing__`, the KeyError stands (as PyRaise).
    let c2 = fx.user_type(b"CD2", Some(dict_type), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(c2) {
        d.mro = vec![c2, dict_type, fx.types.object];
        d.flags |= FLAG_HAS_SPECIAL_ACCESSORS;
    }
    with_instance_slots(&mut fx, c2);
    let inst2 = new_instance(&mut fx, c2, vec![dict_v]);
    let v = fx.with_subscr(
        |_ctx, _base, _index, _arg| Err(SubscrError::KeyError),
        |ctx, seams| instance_subscr(ctx, inst2, Value::SmallInt(11), SubscrArg::Get, seams),
    );
    assert_eq!(v, Err(PyError::PyRaise));
}

#[test]
fn get_call_and_call() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    // No `__call__`: not callable.
    let v = fx.with_seams(|ctx, seams| instance_call(ctx, inst, 0, 0, &[], seams));
    assert!(v.is_err());
    // `__call__` member: called with (self), returns 42.
    let call_q = static_qstr_id(b"__call__").unwrap();
    let call_fun = builtin(&mut fx, 0, "__call__", ret_42);
    fx.set_local(c, call_q, call_fun);
    let v = fx.with_seams(|ctx, seams| instance_call(ctx, inst, 0, 0, &[], seams));
    assert_eq!(v, Ok(Value::SmallInt(42)));
    assert!(fx.with_seams(|ctx, seams| instance_is_callable(ctx, inst, seams)));
}

#[test]
fn get_buffer_unsupported() {
    let mut fx = Fx::new();
    // Plain classes (no instance slots installed): the lookup
    // misses the buffer slot entirely.
    let object = fx.types.object;
    let c = fx.user_type(b"C", Some(object), vec![object]);
    let inst = new_instance(&mut fx, c, vec![]);
    // No buffer slot anywhere: not supported.
    let v = fx.with_seams(|ctx, seams| {
        let mut info = crate::objects::slots::BufferInfo::empty();
        instance_get_buffer(ctx, inst, &mut info, 0, seams)
    });
    assert_eq!(v, Ok(false));
    // A "" member is a Member hit — still not supported.
    fx.set_local(c, 0, Value::SmallInt(1));
    let v = fx.with_seams(|ctx, seams| {
        let mut info = crate::objects::slots::BufferInfo::empty();
        instance_get_buffer(ctx, inst, &mut info, 0, seams)
    });
    assert_eq!(v, Ok(false));
}

#[test]
fn print_fallback_and_repr_member() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    // No `__str__`/`__repr__`: the fallback format, with the
    // arena index as the address.
    let mut out = String::new();
    {
        let mut ctx = fx.ctx();
        instance_print(&mut ctx, inst, PrintKind::Repr, &mut out);
    }
    let Value::Heap(r) = inst else { panic!("instance") };
    assert_eq!(out, format!("<C object at {:x}>", r.0));
    // `__repr__` member returning 7: printed as "7".
    let repr_q = static_qstr_id(b"__repr__").unwrap();
    let repr_fun = builtin(&mut fx, 1, "__repr__", |_ctx, _args| Ok(Value::SmallInt(7)));
    fx.set_local(c, repr_q, repr_fun);
    let mut out = String::new();
    {
        let mut ctx = fx.ctx();
        instance_print(&mut ctx, inst, PrintKind::Repr, &mut out);
    }
    assert_eq!(out, "7");
    // kind STR falls back to `__repr__` when `__str__` is missing.
    let mut out = String::new();
    {
        let mut ctx = fx.ctx();
        instance_print(&mut ctx, inst, PrintKind::Str, &mut out);
    }
    assert_eq!(out, "7");
}

#[test]
fn attr_slot_is_load_only() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx, c, vec![]);
    let x_q = fx.q(b"x");
    fx.with_seams(|ctx, seams| {
        instance_store_attr(ctx, inst, x_q, Some(Value::SmallInt(8)), seams).unwrap();
    });
    let y_q = fx.q(b"y");
    let mut ctx = fx.ctx();
    assert_eq!(instance_attr(&mut ctx, inst, x_q), Ok(Some(Value::SmallInt(8))));
    assert_eq!(instance_attr(&mut ctx, inst, y_q), Ok(None));
}
