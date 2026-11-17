//! Tests for instance creation (`instance_create.rs`):
//! `static_class_method_make_new`, `count_native_bases`,
//! `new_instance`, `instance_make_new`,
//! `native_base_init_wrapper`, and the tuple ADD subclass-fast
//! fix (which needs a tuple-subclass type built here).
//! Shared type-creation helpers live in `tests_typecreate.rs`.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objtuple::{new_tuple, tuple_binary_op, tuple_items};
use crate::objects::containers::tests::empty_dict;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind};
use crate::objects::value::{GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

use super::instance_create::{
    count_native_bases, instance_make_new, native_base_init_wrapper, native_base_init_wrapper_obj,
    new_instance, static_class_method_make_new,
};
use super::tests::Fx;
use super::tests_instance::{builtin, class_c, fun_bc, ret_42, ret_none};
use super::tests_typecreate::{
    bases_tuple, empty_locals, native_type_named, new_type,
};
use super::typecreate::{is_fun, super_make_new, super_print, type_print, TYPE_BASE_FLAGS};

pub(crate) fn marker_make_new(
    _ctx: &mut CallCtx,
    _type: GcRef,
    _args: &[Value],
    _n_kw: usize,
) -> PyResult<Value> {
    Ok(Value::SmallInt(77))
}

/// A native (flags 0) type whose `make_new` returns 77.
pub(crate) fn native_marker_type(fx: &mut Fx, name: &[u8]) -> GcRef {
    let t = native_type_named(fx, name);
    if let Obj::Type(d) = fx.arena.get_mut(t) {
        d.make_new = Some(marker_make_new);
    }
    t
}

/// A `__new__` builtin that returns a BLANK instance of the type
/// it is called with (its first arg is the type).
pub(crate) fn new_returns_blank_instance(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let Value::Heap(t) = args[0] else {
        return Err(PyError::PyRaise);
    };
    Ok(new_instance(ctx, t))
}

/// How many times the recording `__init__` below ran (test-only).
static INIT_CALLS: std::sync::Mutex<u32> = std::sync::Mutex::new(0);

/// An `__init__` builtin that only records that it ran. (A builtin
/// `__init__` is UNBOUND, so it receives no self and cannot call
/// the wrapper itself — the `super().__init__()` flow needs a real
/// Python `__init__`, which is C13's; the wrapper itself is pinned
/// by `native_base_init_wrapper_direct`.)
fn init_recorder(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    *INIT_CALLS.lock().unwrap() += 1;
    Ok(Value::None)
}

// ── mp_obj_new_type ──────────────────────────────────────────────────

#[test]
fn static_class_method_make_new_cases() {
    let mut fx = Fx::new();
    let fun = builtin(&mut fx, 0, "f", ret_42);
    let staticmethod = fx.types.staticmethod;
    let classmethod = fx.types.classmethod;
    let mut ctx = fx.ctx();
    let got = static_class_method_make_new(&mut ctx, staticmethod, &[fun], 0)
        .expect("staticmethod(f)");
    let Value::Heap(r) = got else { panic!("not heap") };
    match fx.arena.get(r) {
        Obj::StaticClassMethod(d) => {
            assert_eq!(d.fun, fun);
            assert_eq!(fx.arena.header(r).type_, Some(fx.types.staticmethod));
        }
        other => panic!("not a staticmethod: {other:?}"),
    }
    let mut ctx = fx.ctx();
    let got = static_class_method_make_new(&mut ctx, classmethod, &[fun], 0)
        .expect("classmethod(f)");
    let Value::Heap(r) = got else { panic!("not heap") };
    assert_eq!(fx.arena.header(r).type_, Some(fx.types.classmethod));
    // Wrong arities → the sig error.
    let mut ctx = fx.ctx();
    assert!(static_class_method_make_new(&mut ctx, staticmethod, &[], 0).is_err());
    let mut ctx = fx.ctx();
    assert!(static_class_method_make_new(&mut ctx, staticmethod, &[fun, fun], 0).is_err());
}

// ── instances ────────────────────────────────────────────────────────

#[test]
fn count_native_bases_cases() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let int = fx.types.int;
    assert_eq!(count_native_bases(&fx.ctx(), int), (1, Some(int)));
    assert_eq!(count_native_bases(&fx.ctx(), object), (0, None));
    let c = fx.user_type(b"C", Some(object), vec![]);
    assert_eq!(count_native_bases(&fx.ctx(), c), (0, None));
    let m1 = native_marker_type(&mut fx, b"M1");
    let ci = fx.user_type(b"CI", Some(m1), vec![]);
    assert_eq!(count_native_bases(&fx.ctx(), ci), (1, Some(m1)));
    // MI over two native-based types counts 2.
    let m2 = native_marker_type(&mut fx, b"M2");
    let cj = fx.user_type(b"CJ", Some(m2), vec![]);
    let both = fx.mi_type(
        b"Both",
        vec![Value::Heap(ci), Value::Heap(cj)],
        vec![],
    );
    assert_eq!(count_native_bases(&fx.ctx(), both), (2, Some(m2)));
}

#[test]
fn new_instance_wrapper_prime() {
    let mut fx = Fx::new();
    // Install the REAL wrapper (the Rom registry does this).
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let wrapper = native_base_init_wrapper_obj(&mut fx.ctx(), dict);
    fx.roots.push(wrapper);
    fx.types.native_init_wrapper = match wrapper {
        Value::Heap(r) => r,
        _ => unreachable!(),
    };
    let marker = native_marker_type(&mut fx, b"N");
    let cn = fx.user_type(b"CN", Some(marker), vec![]);
    let inst = new_instance(&mut fx.ctx(), cn);
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj == vec![wrapper]));
    // An object-based class has no native base → no subobj slot.
    let c = fx.user_type(b"C", Some(fx.types.object), vec![]);
    let inst = new_instance(&mut fx.ctx(), c);
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj.is_empty()));
}

#[test]
fn instance_make_new_blank_init_none() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    // A builtin `__init__` is UNBOUND (see the module docs), so it
    // takes arity 0 for a 0-arg construction.
    let init = builtin(&mut fx, 0, "__init__", ret_none);
    fx.set_local(c, static_qstr_id(b"__init__").expect("__init__ is static"), init);
    let mut ctx = fx.ctx();
    let inst = instance_make_new(&mut ctx, c, &[], 0).expect("C()");
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert_eq!(fx.arena.header(ir).type_, Some(c));
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj.is_empty()));
}

#[test]
fn instance_make_new_init_must_return_none() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    // `__init__` returning 42 → the `__init__() should return None` error.
    let init = builtin(&mut fx, 0, "__init__", ret_42);
    fx.set_local(c, static_qstr_id(b"__init__").expect("__init__ is static"), init);
    let mut ctx = fx.ctx();
    assert!(instance_make_new(&mut ctx, c, &[], 0).is_err());
}

#[test]
fn instance_make_new_new_returns_non_instance() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    // `__new__` returns 42 (not an instance) → returned as-is,
    // `__init__` never runs (it would raise if it did).
    let new = builtin(&mut fx, 1, "__new__", ret_42);
    fx.set_local(c, static_qstr_id(b"__new__").expect("__new__ is static"), new);
    let init = builtin(&mut fx, 1, "__init__", ret_42);
    fx.set_local(c, static_qstr_id(b"__init__").expect("__init__ is static"), init);
    let mut ctx = fx.ctx();
    assert_eq!(
        instance_make_new(&mut ctx, c, &[], 0).expect("N()"),
        Value::SmallInt(42)
    );
}

#[test]
fn instance_make_new_new_returns_instance_runs_init() {
    let mut fx = Fx::new();
    let c = class_c(&mut fx);
    // `__new__` returns a blank instance → `__init__` runs (and
    // its non-None return raises).
    let new = builtin(&mut fx, 1, "__new__", new_returns_blank_instance);
    fx.set_local(c, static_qstr_id(b"__new__").expect("__new__ is static"), new);
    let init = builtin(&mut fx, 0, "__init__", ret_42);
    fx.set_local(c, static_qstr_id(b"__init__").expect("__init__ is static"), init);
    let mut ctx = fx.ctx();
    assert!(instance_make_new(&mut ctx, c, &[], 0).is_err());
}

#[test]
fn instance_make_new_native_tail() {
    let mut fx = Fx::new();
    // A marker-based class with no `__init__`: the tail constructs
    // the native base (marker `make_new` → 77) into `subobj[0]`.
    let marker = native_marker_type(&mut fx, b"N");
    let cn = fx.user_type(b"CN", Some(marker), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(cn) {
        d.mro = vec![cn, marker, fx.types.object];
    }
    let mut ctx = fx.ctx();
    let inst = instance_make_new(&mut ctx, cn, &[], 0).expect("CN()");
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj == vec![Value::SmallInt(77)]));
}

#[test]
fn native_base_init_wrapper_direct() {
    let mut fx = Fx::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let wrapper = native_base_init_wrapper_obj(&mut fx.ctx(), dict);
    fx.roots.push(wrapper);
    fx.types.native_init_wrapper = match wrapper {
        Value::Heap(r) => r,
        _ => unreachable!(),
    };
    let marker = native_marker_type(&mut fx, b"N");
    let cn = fx.user_type(b"CN", Some(marker), vec![]);
    let inst = new_instance(&mut fx.ctx(), cn);
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj == vec![wrapper]));
    let mut ctx = fx.ctx();
    assert_eq!(
        native_base_init_wrapper(&mut ctx, &[inst], &[]).expect("wrapper call"),
        Value::None
    );
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj == vec![Value::SmallInt(77)]));
}

#[test]
fn instance_make_new_init_runs_then_tail() {
    let mut fx = Fx::new();
    *INIT_CALLS.lock().unwrap() = 0;
    // With an `__init__` present, it runs AND the tail still
    // constructs the native base afterwards.
    let marker = native_marker_type(&mut fx, b"N");
    let cn = fx.user_type(b"CN", Some(marker), vec![]);
    if let Obj::Type(d) = fx.arena.get_mut(cn) {
        d.mro = vec![cn, marker, fx.types.object];
    }
    let init = builtin(&mut fx, 0, "__init__", init_recorder);
    fx.set_local(cn, static_qstr_id(b"__init__").expect("__init__ is static"), init);
    let mut ctx = fx.ctx();
    let inst = instance_make_new(&mut ctx, cn, &[], 0).expect("CN()");
    let Value::Heap(ir) = inst else { panic!("not an instance") };
    assert_eq!(*INIT_CALLS.lock().unwrap(), 1);
    assert!(matches!(fx.arena.get(ir), Obj::Instance(d) if d.subobj == vec![Value::SmallInt(77)]));
}

// ── misc ─────────────────────────────────────────────────────────────

#[test]
fn is_fun_cases() {
    let mut fx = Fx::new();
    let f = fun_bc(&mut fx, "f");
    let g = builtin(&mut fx, 0, "g", ret_42);
    let int = fx.types.int;
    assert!(is_fun(&fx.ctx(), f));
    assert!(is_fun(&fx.ctx(), g));
    assert!(!is_fun(&fx.ctx(), Value::SmallInt(1)));
    assert!(!is_fun(&fx.ctx(), Value::Heap(int)));
}

#[test]
fn type_print_and_super_print() {
    let mut fx = Fx::new();
    // The fixture's `type` / `super` types carry no print slot;
    // install the real ones (the ROM registry does this). The
    // `int` type also needs a real header: `obj_print_helper`
    // dispatches on it, and the fixture builds types with header
    // `None` (see `is_type_value`).
    if let Obj::Type(d) = fx.arena.get_mut(fx.types.type_) {
        d.print = Some(type_print);
    }
    if let Obj::Type(d) = fx.arena.get_mut(fx.types.super_) {
        d.print = Some(super_print);
    }
    let int = fx.types.int;
    let type_ = fx.types.type_;
    fx.arena.set_header_type(int, Some(type_));
    let object = fx.types.object;
    let locals = empty_locals(&mut fx);
    let bases = bases_tuple(&mut fx, &[Value::Heap(object)]);
    let t = new_type(&mut fx, b"T", bases, locals).expect("new type");
    let mut out = String::new();
    let mut ctx = fx.ctx();
    type_print(&mut ctx, t, PrintKind::Str, &mut out);
    assert_eq!(out, "<class 'T'>");
    let super_ = fx.types.super_;
    let mut out = String::new();
    let mut ctx = fx.ctx();
    let sup =
        super_make_new(&mut ctx, super_, &[Value::Heap(int), Value::SmallInt(1)], 0)
            .expect("super(int, 1)");
    let mut ctx = fx.ctx();
    super_print(&mut ctx, sup, PrintKind::Str, &mut out);
    assert_eq!(out, "<super: <class 'int'>, 1>");
}

/// A tuple-subclass type (namedtuple shape): `base` is the tuple
/// type, `mro` is `[self, tuple, object]`. Values of this type
/// are plain `Obj::Tuple` payloads (the Zig's namedtuple
/// instances are plain tuples — C11).
fn tuple_subclass(fx: &mut Fx) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let tuple_t = fx.types.tuple;
    let object = fx.types.object;
    let name = fx.q(b"P");
    let t = fx.arena.alloc(
        Some(fx.types.type_),
        Obj::Type(TypeData {
            name,
            flags: TYPE_BASE_FLAGS,
            base: Some(tuple_t),
            mro: Vec::new(),
            dict,
            namedtuple_fields: None,
            protocol: None,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
            attr: None,
            make_new: None,
            bases: None,
        }),
        &fx.roots,
    );
    if let Obj::Type(d) = fx.arena.get_mut(t) {
        d.mro = vec![t, tuple_t, object];
    }
    t
}

#[test]
fn tuple_add_subclass_fast() {
    // `objtuple.zig:185-189` — the Add arm takes any rhs whose
    // type is a tuple subclass (C12a closes C9c's recorded
    // divergence): `P(1,2) + (3,)` and `(1,2) + P(1,2)` both
    // give plain `(1,2,3)` on the reference binary.
    let mut fx = Fx::new();
    let tuple_t = fx.types.tuple;
    let p = tuple_subclass(&mut fx);
    let sub_val = {
        let mut ctx = fx.ctx();
        Value::Heap(ctx.alloc(Some(p), Obj::Tuple(vec![Value::SmallInt(1), Value::SmallInt(2)])))
    };
    let plain = {
        let mut ctx = fx.ctx();
        Value::Heap(new_tuple(&mut ctx, tuple_t, &[Value::SmallInt(3)]))
    };
    let want = [Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)];
    let mut ctx = fx.ctx();
    let got = tuple_binary_op(&mut ctx, BinaryOp::Add, sub_val, plain).expect("P(1,2)+(3,)");
    assert_eq!(tuple_items(&ctx, got).unwrap(), &want);
    let mut ctx = fx.ctx();
    let got = tuple_binary_op(&mut ctx, BinaryOp::Add, plain, sub_val).expect("(1,2)+P(1,2)");
    // `(1,2) + P(1,2)` is `(1,2,1,2)` on the reference binary;
    // here the rhs is the 2-tuple, so the result is (3,1,2).
    assert_eq!(tuple_items(&ctx, got).unwrap(), &[Value::SmallInt(3), Value::SmallInt(1), Value::SmallInt(2)]);
    // A non-tuple rhs still raises.
    let mut ctx = fx.ctx();
    assert!(tuple_binary_op(&mut ctx, BinaryOp::Add, plain, Value::SmallInt(1)).is_err());
    // An exact-tuple rhs still works.
    let mut ctx = fx.ctx();
    let got = tuple_binary_op(&mut ctx, BinaryOp::Add, plain, plain).expect("(3,)+(3,)");
    assert_eq!(tuple_items(&ctx, got).unwrap(), &[Value::SmallInt(3), Value::SmallInt(3)]);
}
