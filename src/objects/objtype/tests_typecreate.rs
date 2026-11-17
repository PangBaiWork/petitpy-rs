//! Tests for the type-creation half (`typecreate.rs`,
//! `instance_create.rs`): `mp_obj_new_type`, `type`, `super`,
//! `staticmethod` / `classmethod`, instance construction and the
//! native-base wrapper.
//!
//! NOTE on the fixture: NO builtin type has a `make_new` slot yet
//! (the adapters are the next step — see `typecreate.rs`'s docs
//! and the result file). The native-base tests therefore use
//! hand-built native marker types (`native_marker_type`, whose
//! `make_new` returns 77); a `NotAcceptableBase` test uses a
//! hand-built native type WITHOUT a slot, so it stays green when
//! the real adapters land.

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::containers::tests::empty_dict;
use crate::objects::obj::{type_data, FLAG_HAS_SPECIAL_ACCESSORS};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, GcRef, MapData, Obj, TypeData, Value};

use super::errors::ObjTypeError;
use super::instance_create::{instance_make_new, load_super_method, new_instance};
use super::tests::Fx;
use super::tests_instance::{builtin, class_c, fun_bc, ret_42, ret_none};
use super::tests_instance_create::native_marker_type;
use super::typecreate::{
    mp_obj_new_type, super_attr, super_make_new, type_attr, type_call,
    type_make_new, type_store_attr, TYPE_BASE_FLAGS,
};
use super::subclass::is_type_value;

#[test]
fn is_type_value_cases() {
    let mut fx = Fx::new();
    // Fixture types have header `None`, so only the payload check
    // sees them as types.
    assert!(is_type_value(&fx.arena, Value::Heap(fx.types.int)));
    assert!(!is_type_value(&fx.arena, Value::SmallInt(1)));
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx.ctx(), c);
    assert!(!is_type_value(&fx.arena, inst));
}

/// A bases tuple value for `mp_obj_new_type` / `type()`.
pub(crate) fn bases_tuple(fx: &mut Fx, items: &[Value]) -> Value {
    let tuple_type = fx.types.tuple;
    Value::Heap(new_tuple(&mut fx.ctx(), tuple_type, items))
}

/// An empty locals dict value WITH the dict header type. The
/// `mp_obj_new_type` locals check is a header-type check
/// (`isDictType`), and the containers fixture's `empty_dict`
/// allocates with header `None` — so it cannot serve as a class
/// dict here.
pub(crate) fn empty_locals(fx: &mut Fx) -> Value {
    let dict = fx.arena.alloc(
        Some(fx.types.dict),
        Obj::Dict(DictData { map: MapData::new() }),
        &fx.roots,
    );
    fx.roots.push(Value::Heap(dict));
    Value::Heap(dict)
}

/// Give a type a `make_new` slot (`instance_make_new` — the shape
/// `mp_obj_new_type` installs for user types). `user_type` types
/// carry none, but `mp_obj_new_type` requires every base to have
/// one.
pub(crate) fn make_newable(fx: &mut Fx, t: GcRef) {
    if let Obj::Type(d) = fx.arena.get_mut(t) {
        d.make_new = Some(instance_make_new);
    }
}

/// `mp_obj_new_type` with stub seams.
pub(crate) fn new_type(
    fx: &mut Fx,
    name: &[u8],
    bases: Value,
    locals: Value,
) -> Result<Value, ObjTypeError> {
    let name = fx.q(name);
    fx.with_seams(|ctx, seams| mp_obj_new_type(ctx, name, bases, locals, seams))
}

/// A native (flags 0) type with a DYNAMIC name. The marker names
/// (`M1` / `N` / …) are not static qstrs, so the containers
/// fixture's `make_type` (which requires a static name) cannot
/// build them.
pub(crate) fn native_type_named(fx: &mut Fx, name: &[u8]) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = TypeData {
        name: fx.q(name),
        flags: 0,
        base: None,
        bases: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        attr: None,
        make_new: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    };
    let r = fx.arena.alloc(Some(fx.types.type_), Obj::Type(data), &fx.roots);
    fx.roots.push(Value::Heap(r));
    r
}

/// A native (flags 0) type with NO `make_new` slot — the
/// `NotAcceptableBase` shape.
fn native_no_make_new(fx: &mut Fx, name: &[u8]) -> GcRef {
    native_type_named(fx, name)
}

/// The marker `make_new`: every construction returns 77.
#[test]
fn new_type_single_base() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let t = new_type(&mut fx, b"D", bases, locals).expect("new type");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    let d = type_data(&fx.arena, tr).expect("type data");
    assert_eq!(d.flags, TYPE_BASE_FLAGS);
    assert_eq!(d.base, Some(object));
    assert_eq!(d.bases, None);
    assert_eq!(d.dict, match locals {
        Value::Heap(r) => r,
        _ => unreachable!(),
    });
    assert_eq!(d.mro, vec![tr, object]);
    // The 9 instance slots are installed.
    assert!(d.make_new.is_some());
    assert!(d.print.is_some());
    assert!(d.call.is_some());
    assert!(d.unary_op.is_some());
    assert!(d.binary_op.is_some());
    assert!(d.attr.is_some());
    assert!(d.subscr.is_some());
    assert!(d.getiter.is_some());
    assert!(d.buffer.is_some());
    // Single base WITHOUT a cached MRO: no `__mro__` in locals.
    let mut ctx = fx.ctx();
    let mro_q = static_qstr_id(b"__mro__").expect("__mro__ is a static qstr");
    assert!(dict_get_value(&mut ctx, locals, Value::Qstr(mro_q)).is_err());
}

#[test]
fn new_type_mi_stores_mro() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(a) {
        d.mro = vec![a, object];
    }
    make_newable(&mut fx, a);
    let b = fx.user_type(b"B", Some(object), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(b) {
        d.mro = vec![b, object];
    }
    make_newable(&mut fx, b);
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(a), Value::Heap(b)]);
    let t = new_type(&mut fx, b"D", bases, locals).expect("MI type");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    let d = type_data(&fx.arena, tr).expect("type data");
    assert_eq!(d.base, None);
    assert_eq!(d.bases, Some(match bases {
        Value::Heap(r) => r,
        _ => unreachable!(),
    }));
    assert_eq!(d.mro, vec![tr, a, b, object]);
    // `__mro__` IS stored in the locals dict for MI.
    let mut ctx = fx.ctx();
    let mro_q = static_qstr_id(b"__mro__").expect("__mro__ is a static qstr");
    let mro_v = dict_get_value(&mut ctx, locals, Value::Qstr(mro_q)).expect("mro stored");
    let Value::Heap(mr) = mro_v else { panic!("mro not a tuple") };
    assert!(matches!(fx.arena.get(mr), Obj::Tuple(items) if items.len() == 4));
}

#[test]
fn new_type_single_base_cached_mro() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    // A single base WITH a cached `__mro__` (the `set_mro` shape).
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(a) {
        d.mro = vec![a, object];
    }
    make_newable(&mut fx, a);
    fx.set_mro(a, vec![Value::Heap(a), Value::Heap(object)]);
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(a)]);
    let t = new_type(&mut fx, b"D", bases, locals).expect("cached-mro type");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    let d = type_data(&fx.arena, tr).expect("type data");
    assert_eq!(d.mro, vec![tr, a, object]);
    let mut ctx = fx.ctx();
    let mro_q = static_qstr_id(b"__mro__").expect("__mro__ is a static qstr");
    assert!(dict_get_value(&mut ctx, locals, Value::Qstr(mro_q)).is_ok());
}

#[test]
fn new_type_errors() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    // Bases not a tuple.
    assert_eq!(
        new_type(&mut fx, b"D", Value::SmallInt(1), locals),
        Err(ObjTypeError::BareTypeError)
    );
    // Locals not a dict.
    assert_eq!(
        new_type(&mut fx, b"D", bases, Value::SmallInt(1)),
        Err(ObjTypeError::BareTypeError)
    );
    // A base that is not a type.
    let bad_bases = bases_tuple(&mut fx, &[Value::SmallInt(1)]);
    assert_eq!(
        new_type(&mut fx, b"D", bad_bases, locals),
        Err(ObjTypeError::BareTypeError)
    );
    // A base without a `make_new` slot.
    let no_mk = native_no_make_new(&mut fx, b"NoMk");
    let no_mk_bases = bases_tuple(&mut fx, &[Value::Heap(no_mk)]);
    assert_eq!(
        new_type(&mut fx, b"D", no_mk_bases, locals),
        Err(ObjTypeError::NotAcceptableBase)
    );
}

#[test]
fn new_type_layout_conflict() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    // Two int-based user types (each counts 1 native base).
    let m1 = native_marker_type(&mut fx, b"M1");
    let m2 = native_marker_type(&mut fx, b"M2");
    let ci = fx.user_type(b"CI", Some(m1), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(ci) {
        d.mro = vec![ci, m1, object];
    }
    make_newable(&mut fx, ci);
    let cj = fx.user_type(b"CJ", Some(m2), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(cj) {
        d.mro = vec![cj, m2, object];
    }
    make_newable(&mut fx, cj);
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(ci), Value::Heap(cj)]);
    assert_eq!(
        new_type(&mut fx, b"D", bases, locals),
        Err(ObjTypeError::LayoutConflict)
    );
    // The `__mro__` was stored in locals BEFORE the layout check.
    let mut ctx = fx.ctx();
    let mro_q = static_qstr_id(b"__mro__").expect("__mro__ is a static qstr");
    assert!(dict_get_value(&mut ctx, locals, Value::Qstr(mro_q)).is_ok());
}

#[test]
fn new_type_new_wrap() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    // `__new__` = a FunBc (`is_fun`) → wrapped in a StaticClassMethod.
    let locals = empty_locals(&mut fx);
    let fun = fun_bc(&mut fx, "__new__");
    let new_q = static_qstr_id(b"__new__").expect("__new__ is a static qstr");
    let mut ctx = fx.ctx();
    crate::objects::containers::objdict::dict_store(
        &mut ctx,
        locals,
        Value::Qstr(new_q),
        fun,
    )
    .expect("fixture store");
    drop(ctx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    new_type(&mut fx, b"D", bases, locals).expect("wrapped type");
    let mut ctx = fx.ctx();
    let wrapped = dict_get_value(&mut ctx, locals, Value::Qstr(new_q)).expect("new kept");
    match wrapped {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::StaticClassMethod(d) => {
                assert_eq!(d.fun, fun);
                assert_eq!(fx.arena.header(r).type_, Some(fx.types.staticmethod));
            }
            other => panic!("not wrapped: {other:?}"),
        },
        _ => panic!("not a heap value"),
    }
    // `__new__` = a non-function (a small int) → left alone.
    let locals2 = empty_locals(&mut fx);
    let mut ctx = fx.ctx();
    crate::objects::containers::objdict::dict_store(
        &mut ctx,
        locals2,
        Value::Qstr(new_q),
        Value::SmallInt(3),
    )
    .expect("fixture store");
    drop(ctx);
    let bases2 = bases_tuple(&mut fx, &[Value::Heap(object)]);
    new_type(&mut fx, b"E", bases2, locals2).expect("plain type");
    let mut ctx = fx.ctx();
    assert_eq!(
        dict_get_value(&mut ctx, locals2, Value::Qstr(new_q)).expect("new kept"),
        Value::SmallInt(3)
    );
}


// ── type() ───────────────────────────────────────────────────────────

#[test]
fn type_make_new_one_arg() {
    let mut fx = Fx::new();
    let int = fx.types.int;
    let type_ = fx.types.type_;
    let mut ctx = fx.ctx();
    let got = type_make_new(&mut ctx, type_, &[Value::SmallInt(1)], 0).expect("type(1)");
    assert_eq!(got, Value::Heap(int));
    // `type` of a type value is the `type` type itself.
    let mut ctx = fx.ctx();
    let got = type_make_new(&mut ctx, type_, &[Value::Heap(int)], 0).expect("type(int)");
    assert_eq!(got, Value::Heap(type_));
}

#[test]
fn type_make_new_three_args() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let name = Value::Qstr(fx.q(b"T"));
    let type_ = fx.types.type_;
    let mut ctx = fx.ctx();
    let got = type_make_new(&mut ctx, type_, &[name, bases, locals], 0).expect("type(T)");
    let Value::Heap(tr) = got else { panic!("not a heap type") };
    let t_q = fx.q(b"T");
    assert_eq!(type_data(&fx.arena, tr).expect("type data").name, t_q);
}

#[test]
fn type_make_new_errors() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let name = Value::Qstr(fx.q(b"T"));
    let type_ = fx.types.type_;
    // Two args → the "takes 1 or 3" error (as a bare raise here —
    // the specific variant is pinned at the `mp_obj_new_type`
    // level; the `MakeNewFn` only carries `PyError`).
    let mut ctx = fx.ctx();
    assert!(type_make_new(&mut ctx, type_, &[name, bases], 0).is_err());
    // A non-str name → "can't convert to str implicitly".
    assert!(type_make_new(&mut ctx, type_, &[Value::SmallInt(1), bases, locals], 0).is_err());
    // Three positionals plus a kwarg → the sig error.
    let x_q = fx.q(b"x");
    let mut ctx = fx.ctx();
    assert!(type_make_new(
        &mut ctx,
        type_,
        &[name, bases, locals, Value::Qstr(x_q), Value::SmallInt(1)],
        1,
    )
    .is_err());
}

#[test]
fn type_call_no_make_new() {
    let mut fx = Fx::new();
    // The fixture's int type has no `make_new` slot (adapters are
    // the next step) → `can't create instance`.
    let int = fx.types.int;
    let mut ctx = fx.ctx();
    assert!(type_call(&mut ctx, Value::Heap(int), 0, 0, &[]).is_err());
}

#[test]
fn type_call_with_make_new() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let t = new_type(&mut fx, b"D", bases, locals).expect("new type");
    let mut ctx = fx.ctx();
    let inst = type_call(&mut ctx, t, 0, 0, &[]).expect("type() call");
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert!(matches!(fx.arena.get(ir), Obj::Instance(_)));
    assert_eq!(fx.arena.header(ir).type_, Some(match t {
        Value::Heap(tr) => tr,
        _ => unreachable!(),
    }));
}

// ── super() ──────────────────────────────────────────────────────────

#[test]
fn super_make_new_ok() {
    let mut fx = Fx::new();
    let super_ = fx.types.super_;
    let int = fx.types.int;
    let mut ctx = fx.ctx();
    let got = super_make_new(&mut ctx, super_, &[Value::Heap(int), Value::SmallInt(1)], 0)
        .expect("super(int, 1)");
    let Value::Heap(r) = got else { panic!("not a super") };
    match fx.arena.get(r) {
        Obj::Super(d) => {
            assert_eq!(d.type_, fx.types.int);
            assert_eq!(d.obj, Value::SmallInt(1));
        }
        other => panic!("not a super: {other:?}"),
    }
}

#[test]
fn super_make_new_errors() {
    let mut fx = Fx::new();
    let super_ = fx.types.super_;
    let int = fx.types.int;
    // `super(1, 2)` → the `issubclass() arg 2` error.
    let mut ctx = fx.ctx();
    assert!(super_make_new(
        &mut ctx,
        super_,
        &[Value::SmallInt(1), Value::SmallInt(2)],
        0,
    )
    .is_err());
    // `super(int)` → bare TypeError (the missing arg is `None`,
    // `is_subclass(NoneType, int)` is false).
    let mut ctx = fx.ctx();
    assert!(super_make_new(&mut ctx, super_, &[Value::Heap(int)], 0).is_err());
    // Three args → the sig error on success.
    let mut ctx = fx.ctx();
    assert!(super_make_new(
        &mut ctx,
        super_,
        &[Value::Heap(int), Value::SmallInt(1), Value::SmallInt(2)],
        0,
    )
    .is_err());
}

#[test]
fn super_attr_mro_branch() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = class_c(&mut fx);
    // A FunBc member binds (BINDS_SELF, no BUILTIN_FUN) — a
    // FunBuiltin member would stay unbound (see the unbound test).
    let m = fun_bc(&mut fx, "m");
    let m_q = fx.q(b"m");
    fx.set_local(a, m_q, m);
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(b) {
        d.mro = vec![b, a, object];
    }
    fx.set_mro(b, vec![Value::Heap(b), Value::Heap(a), Value::Heap(object)]);
    let inst = new_instance(&mut fx.ctx(), b);
    let super_ = fx.types.super_;
    let sup = {
        let mut ctx = fx.ctx();
        super_make_new(&mut ctx, super_, &[Value::Heap(b), inst], 0).expect("super(B, b)")
    };
    let m_q = fx.q(b"m");
    let mut ctx = fx.ctx();
    let got = super_attr(&mut ctx, sup, m_q).expect("super attr");
    let Some(Value::Heap(br)) = got else { panic!("no attr") };
    match fx.arena.get(br) {
        Obj::BoundMeth(d) => {
            assert_eq!(d.func, m);
            assert_eq!(d.self_, inst);
        }
        other => panic!("not bound: {other:?}"),
    }
}

#[test]
fn super_attr_parent_branch() {
    let mut fx = Fx::new();
    let a = class_c(&mut fx);
    let m = fun_bc(&mut fx, "m");
    let m_q = fx.q(b"m");
    fx.set_local(a, m_q, m);
    // B WITHOUT a cached `__mro__` → the parent branch.
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(b) {
        d.mro = vec![b, a, fx.types.object];
    }
    let inst = new_instance(&mut fx.ctx(), b);
    let super_ = fx.types.super_;
    let sup = {
        let mut ctx = fx.ctx();
        super_make_new(&mut ctx, super_, &[Value::Heap(b), inst], 0).expect("super(B, b)")
    };
    let m_q = fx.q(b"m");
    let mut ctx = fx.ctx();
    let got = super_attr(&mut ctx, sup, m_q).expect("super attr");
    let Some(Value::Heap(br)) = got else { panic!("no attr") };
    match fx.arena.get(br) {
        Obj::BoundMeth(d) => {
            assert_eq!(d.func, m);
            assert_eq!(d.self_, inst);
        }
        other => panic!("not bound: {other:?}"),
    }
}

#[test]
fn super_attr_init_native_slot() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    // A native parent WITH a `make_new` slot: `__init__` resolves
    // to the SENTINEL → the wrapper bound to the instance.
    let marker = native_marker_type(&mut fx, b"N");
    let d = fx.user_type(b"D", Some(marker), vec![]);
    if let Obj::Type(dd) = fx.arena.get_mut(d) {
        dd.mro = vec![d, marker, object];
    }
    fx.set_mro(
        d,
        vec![Value::Heap(d), Value::Heap(marker), Value::Heap(object)],
    );
    let inst = new_instance(&mut fx.ctx(), d);
    let super_ = fx.types.super_;
    let sup = {
        let mut ctx = fx.ctx();
        super_make_new(&mut ctx, super_, &[Value::Heap(d), inst], 0).expect("super(D, d)")
    };
    let init_q = static_qstr_id(b"__init__").expect("__init__ is a static qstr");
    let mut ctx = fx.ctx();
    let got = super_attr(&mut ctx, sup, init_q).expect("super __init__");
    let Some(Value::Heap(br)) = got else { panic!("no attr") };
    match fx.arena.get(br) {
        Obj::BoundMeth(bd) => {
            assert_eq!(bd.func, Value::Heap(fx.types.native_init_wrapper));
            assert_eq!(bd.self_, inst);
        }
        other => panic!("not bound: {other:?}"),
    }
}

#[test]
fn super_attr_missing_is_none() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    let inst = new_instance(&mut fx.ctx(), c);
    let super_ = fx.types.super_;
    let sup = {
        let mut ctx = fx.ctx();
        super_make_new(&mut ctx, super_, &[Value::Heap(c), inst], 0).expect("super(C, c)")
    };
    let nope_q = fx.q(b"nope");
    let mut ctx = fx.ctx();
    assert_eq!(super_attr(&mut ctx, sup, nope_q).expect("load"), None);
}

#[test]
fn load_super_method_ok() {
    let mut fx = Fx::new();
    let a = class_c(&mut fx);
    let m = fun_bc(&mut fx, "m");
    let m_q = fx.q(b"m");
    fx.set_local(a, m_q, m);
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(b) {
        d.mro = vec![b, a, fx.types.object];
    }
    fx.set_mro(b, vec![Value::Heap(b), Value::Heap(a), Value::Heap(fx.types.object)]);
    let inst = new_instance(&mut fx.ctx(), b);
    let mut ctx = fx.ctx();
    let got = load_super_method(&mut ctx, b, inst, m_q).expect("load_super_method");
    let Some(Value::Heap(br)) = got else { panic!("no method") };
    match fx.arena.get(br) {
        Obj::BoundMeth(d) => assert_eq!(d.func, m),
        other => panic!("not bound: {other:?}"),
    }
}

#[test]
fn super_attr_builtin_member_unbound() {
    let mut fx = Fx::new();
    let a = class_c(&mut fx);
    // A FunBuiltin member stays UNBOUND (BINDS_SELF | BUILTIN_FUN
    // → staticmethod per `convert_member_lookup`): `super_attr`
    // returns the fun itself, not a bound method.
    let m = builtin(&mut fx, 1, "m", ret_42);
    let m_q = fx.q(b"m");
    fx.set_local(a, m_q, m);
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(b) {
        d.mro = vec![b, a, fx.types.object];
    }
    fx.set_mro(b, vec![Value::Heap(b), Value::Heap(a), Value::Heap(fx.types.object)]);
    let inst = new_instance(&mut fx.ctx(), b);
    let super_ = fx.types.super_;
    let sup = {
        let mut ctx = fx.ctx();
        super_make_new(&mut ctx, super_, &[Value::Heap(b), inst], 0).expect("super(B, b)")
    };
    let mut ctx = fx.ctx();
    let got = super_attr(&mut ctx, sup, m_q).expect("super attr");
    assert_eq!(got, Some(m));
}

// ── the type type's own slots ────────────────────────────────────────

#[test]
fn type_attr_dunders() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let t = new_type(&mut fx, b"T", bases, locals).expect("new type");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    let t_q = fx.q(b"T");
    let nope_q = fx.q(b"nope");
    let name_q = static_qstr_id(b"__name__").expect("__name__ is a static qstr");
    let bases_q = static_qstr_id(b"__bases__").expect("__bases__ is a static qstr");
    let mro_q = static_qstr_id(b"__mro__").expect("__mro__ is a static qstr");
    let dict_q = static_qstr_id(b"__dict__").expect("__dict__ is a static qstr");
    // Each `CallCtx` is scoped: the arena reads below need `fx`
    // back, and the ctx borrows it mutably.
    // `__name__`.
    let got = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, t, name_q).expect("name")
    };
    assert_eq!(got, Some(Value::Qstr(t_q)));
    // `__bases__` → `(object,)`.
    let got = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, t, bases_q).expect("bases")
    };
    let Some(Value::Heap(br)) = got else { panic!("no bases") };
    match fx.arena.get(br) {
        Obj::Tuple(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], Value::Heap(object));
        }
        other => panic!("not a tuple: {other:?}"),
    }
    // `__mro__` → `(T, object)`.
    let got = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, t, mro_q).expect("mro")
    };
    let Some(Value::Heap(mr)) = got else { panic!("no mro") };
    match fx.arena.get(mr) {
        Obj::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], Value::Heap(tr));
            assert_eq!(items[1], Value::Heap(object));
        }
        other => panic!("not a tuple: {other:?}"),
    }
    // `__dict__` → a FIXED copy of the locals dict.
    let got = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, t, dict_q).expect("dict")
    };
    let Some(Value::Heap(dr)) = got else { panic!("no dict") };
    assert!(matches!(fx.arena.get(dr), Obj::Dict(d) if d.map.is_fixed));
    // A missing attribute → None.
    let got = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, t, nope_q).expect("missing")
    };
    assert_eq!(got, None);
}

#[test]
fn type_store_attr_flag_and_delete() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let t = new_type(&mut fx, b"T", bases, locals).expect("new type");
    let Value::Heap(tr) = t else { panic!("not a heap type") };
    // Storing under `__setattr__` (a special-accessor KEY) sets
    // `HAS_SPECIAL_ACCESSORS` on the type.
    let m = builtin(&mut fx, 1, "__setattr__", ret_none);
    let setattr_q = static_qstr_id(b"__setattr__").expect("__setattr__ is static");
    let mut ctx = fx.ctx();
    assert!(type_store_attr(&mut ctx, t, setattr_q, Some(m)).expect("store"));
    assert_ne!(
        type_data(&fx.arena, tr).expect("type data").flags & FLAG_HAS_SPECIAL_ACCESSORS,
        0
    );
    // Delete it, then delete again (absent → false).
    let mut ctx = fx.ctx();
    assert!(type_store_attr(&mut ctx, t, setattr_q, None).expect("delete"));
    let mut ctx = fx.ctx();
    assert!(!type_store_attr(&mut ctx, t, setattr_q, None).expect("delete again"));
}

// ── staticmethod / classmethod ───────────────────────────────────────
