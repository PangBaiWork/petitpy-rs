//! Lane C12b tests, part 2 — `objmodule` + `objobject`.
//! (Fixture `Fx` + helpers live in `objtype_tests.rs`.)

use super::objtype_tests::{q, render, Fx};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_get_value, DictError};
use crate::objects::objtype::objmodule::{
    module_attr_load, module_generic_attr, module_get_builtin, module_print,
    module_store_attr, new_module, GenericAttrOp,
};
use crate::objects::objtype::objobject::{
    is_instance_type, object_delattr, object_init_builtin, object_locals_dict,
    object_make_new, object_new, object_setattr, ObjectError,
};
use crate::objects::slots::PrintKind;
use crate::objects::value::{DictData, GcRef, MapData, Obj, Value};
use crate::pyerr::PyError;

// ── objmodule ────────────────────────────────────────────────────────

#[test]
fn new_module_name_and_dict() {
    let mut fx = Fx::new();
    let mt = fx.module_type;
    let nope = fx.dq(b"__nope__");
    let mut ctx = fx.ctx();
    let m = new_module(&mut ctx, q(b"sys"), mt);
    assert_eq!(
        module_attr_load(&mut ctx, m, q(b"__name__"), None).unwrap(),
        Some(Value::Qstr(q(b"sys")))
    );
    // __dict__ is the module's globals dict.
    let d = module_attr_load(&mut ctx, m, q(b"__dict__"), None).unwrap().expect("__dict__");
    assert!(matches!(d, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Dict(_))));
    assert_eq!(module_attr_load(&mut ctx, m, nope, None).unwrap(), None);
}

#[test]
fn module_attr_getattr_seam() {
    let mut fx = Fx::new();
    let mt = fx.module_type;
    let missing = fx.dq(b"missing");
    let mut ctx = fx.ctx();
    let m = new_module(&mut ctx, q(b"sys"), mt);
    // No seam: __getattr__ path is skipped.
    assert_eq!(module_attr_load(&mut ctx, m, missing, None).unwrap(), None);
    // With a seam: the seam's return value is the answer.
    let mut seam = |_ctx: &mut CallCtx, _fun: Value, _name: Value| -> Result<Value, PyError> {
        Ok(Value::SmallInt(42))
    };
    // First install a __getattr__ entry so the seam is reached.
    let dict = match m {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(md) => Value::Heap(md.dict),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    crate::objects::containers::objdict::dict_store(
        &mut ctx,
        dict,
        Value::Qstr(q(b"__getattr__")),
        Value::SmallInt(7),
    )
    .expect("store __getattr__");
    assert_eq!(
        module_attr_load(&mut ctx, m, missing, Some(&mut seam)).unwrap(),
        Some(Value::SmallInt(42))
    );
}

#[test]
fn module_get_builtin_maps_and_usys() {
    let mut fx = Fx::new();
    let mt = fx.module_type;
    let foo = fx.dq(b"foo");
    let nope = fx.dq(b"nope");
    let mut ctx = fx.ctx();
    let sys_m = new_module(&mut ctx, q(b"sys"), mt);
    let math_m = new_module(&mut ctx, q(b"math"), mt);
    let modules: &[(u32, Value)] = &[(q(b"math"), math_m)];
    let extensible: &[(u32, Value)] = &[(foo, math_m)];
    assert_eq!(
        module_get_builtin(&mut ctx, q(b"math"), false, modules, extensible, Some(sys_m), None, None).unwrap(),
        Some(math_m)
    );
    // usys -> sys (MICROPY_PY_SYS == 1 in full).
    assert_eq!(
        module_get_builtin(&mut ctx, q(b"usys"), false, modules, extensible, Some(sys_m), None, None).unwrap(),
        Some(sys_m)
    );
    assert_eq!(
        module_get_builtin(&mut ctx, nope, false, modules, extensible, Some(sys_m), None, None).unwrap(),
        None
    );
    // Extensible miss returns None immediately (no usys/u-strip).
    assert_eq!(
        module_get_builtin(&mut ctx, nope, true, modules, extensible, Some(sys_m), None, None).unwrap(),
        None
    );
}

#[test]
fn module_get_builtin_u_strip_and_init_seam() {
    let mut fx = Fx::new();
    // Pre-intern the dynamic names (the pool is owned by Fx; the
    // intern seam below is a lookup double over these ids).
    let foo_id = fx.pool.from_strn(b"foo").expect("dynamic qstr") as u32;
    let ufoo_id = fx.pool.from_strn(b"ufoo").expect("dynamic qstr") as u32;
    let mt = fx.module_type;
    let mut ctx = fx.ctx();
    let math_m = new_module(&mut ctx, q(b"math"), mt);
    let modules: &[(u32, Value)] = &[];
    // "ufoo" strips to "foo" in the extensible map.
    let extensible: &[(u32, Value)] = &[(foo_id, math_m)];
    let mut intern = |s: &[u8]| -> Result<u32, PyError> {
        if s == b"foo" {
            Ok(foo_id)
        } else {
            Err(PyError::PyRaise)
        }
    };
    assert_eq!(
        module_get_builtin(&mut ctx, ufoo_id, false, modules, extensible, None, None, Some(&mut intern)).unwrap(),
        Some(math_m)
    );
    // __init__ seam runs when the module has __init__ in its dict.
    let init_fun = ctx.alloc(
        None,
        Obj::FunBuiltin(crate::objects::value::FunBuiltinData {
            name: q(b"__init__"),
            n_args: 0,
            func: |_, _| Ok(Value::None),
            kw_func: None,
        }),
    );
    let dict = match math_m {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(md) => Value::Heap(md.dict),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    crate::objects::containers::objdict::dict_store(
        &mut ctx,
        dict,
        Value::Qstr(q(b"__init__")),
        Value::Heap(init_fun),
    )
    .expect("store __init__");
    let mut called = false;
    let mut init = |_ctx: &mut CallCtx, _fun: Value| -> Result<(), PyError> {
        called = true;
        Ok(())
    };
    let got = module_get_builtin(
        &mut ctx,
        q(b"math"),
        false,
        &[(q(b"math"), math_m)],
        &[],
        None,
        Some(&mut init),
        None,
    )
    .unwrap();
    assert_eq!(got, Some(math_m));
    assert!(called, "__init__ seam ran");
}

#[test]
fn module_store_attr_roundtrip_and_delete_missing() {
    let mut fx = Fx::new();
    let mt = fx.module_type;
    let foo = fx.dq(b"foo");
    let missing = fx.dq(b"missing");
    let mut ctx = fx.ctx();
    let m = new_module(&mut ctx, q(b"sys"), mt);
    let mut override_dict: Option<GcRef> = None;
    assert!(module_store_attr(&mut ctx, m, foo, Some(Value::SmallInt(42)), None, &mut override_dict).unwrap());
    assert_eq!(
        module_attr_load(&mut ctx, m, foo, None).unwrap(),
        Some(Value::SmallInt(42))
    );
    assert!(module_store_attr(&mut ctx, m, foo, None, None, &mut override_dict).unwrap());
    assert_eq!(module_attr_load(&mut ctx, m, foo, None).unwrap(), None);
    // Probed (user module): del m.missing -> KeyError 'missing'.
    assert!(matches!(
        module_store_attr(&mut ctx, m, missing, None, None, &mut override_dict),
        Err(DictError::KeyError(_))
    ));
}

#[test]
fn module_store_attr_fixed_dict_paths() {
    let mut fx = Fx::new();
    let mt = fx.module_type;
    let foo = fx.dq(b"foo");
    let mut ctx = fx.ctx();
    // Fixed non-builtins dict -> unhandled (Ok(false)); the
    // caller raises AttributeError (probed: sys.foo = 42).
    let fixed = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new_fixed(vec![]) }));
    let m = ctx.alloc(Some(mt), Obj::Module(crate::objects::value::ModuleData { name: q(b"sys"), dict: fixed, delegation: None }));
    let mut override_dict: Option<GcRef> = None;
    assert!(!module_store_attr(&mut ctx, Value::Heap(m), foo, Some(Value::SmallInt(1)), None, &mut override_dict).unwrap());
    // Fixed builtins dict -> the override dict (created on first use).
    let mut override_dict2: Option<GcRef> = None;
    assert!(module_store_attr(
        &mut ctx,
        Value::Heap(m),
        foo,
        Some(Value::SmallInt(9)),
        Some(fixed),
        &mut override_dict2
    )
    .unwrap());
    let od = override_dict2.expect("override dict created");
    assert_eq!(
        dict_get_value(&mut ctx, Value::Heap(od), Value::Qstr(foo)).unwrap(),
        Value::SmallInt(9)
    );
}

#[test]
fn module_generic_attr_table() {
    // Keys are bare qstr ids — any distinct ids work (the table
    // compares ids only); no fixture pool needed here.
    let mut values = vec![Value::SmallInt(1), Value::SmallInt(2)];
    let keys = [11u32, 22u32];
    assert_eq!(module_generic_attr(11, &keys, &mut values, GenericAttrOp::Load), Some(Value::SmallInt(1)));
    assert_eq!(module_generic_attr(33, &keys, &mut values, GenericAttrOp::Load), None);
    assert_eq!(
        module_generic_attr(22, &keys, &mut values, GenericAttrOp::Store(Value::SmallInt(5))),
        Some(Value::None)
    );
    assert_eq!(values[1], Value::SmallInt(5));
    assert_eq!(
        module_generic_attr(11, &keys, &mut values, GenericAttrOp::Delete),
        Some(Value::None)
    );
    assert_eq!(values[0], Value::None);
}

#[test]
fn module_print_name_and_file() {
    let mut fx = Fx::new();
    let file = fx.mk_str("f.py");
    // `__file__` is a DYNAMIC qstr (config-gated out of the
    // static table — see objmodule.rs `module_print`).
    let file_id = fx.dq(b"__file__");
    let mt = fx.module_type;
    let mut ctx = fx.ctx();
    let m = new_module(&mut ctx, q(b"sys"), mt);
    // Probed: repr(sys) == "<module 'sys'>".
    assert_eq!(render(&mut ctx, m, PrintKind::Repr, module_print), "<module 'sys'>");
    let dict = match m {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(md) => Value::Heap(md.dict),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    crate::objects::containers::objdict::dict_store(&mut ctx, dict, Value::Qstr(file_id), file)
        .expect("store __file__");
    assert_eq!(render(&mut ctx, m, PrintKind::Repr, module_print), "<module 'sys' from 'f.py'>");
}

// ── objobject ────────────────────────────────────────────────────────

#[test]
fn object_make_new_sig() {
    let mut fx = Fx::new();
    let obj = fx.object;
    let mut ctx = fx.ctx();
    // object(1) -> terse mismatch (probed).
    assert!(matches!(
        object_make_new(&mut ctx, obj, &[Value::SmallInt(1)], 0),
        Err(ObjectError::TypeError(_))
    ));
    assert!(matches!(
        object_make_new(&mut ctx, obj, &[], 1),
        Err(ObjectError::TypeError(_))
    ));
    // No args: checks pass, allocation is C12a's (PyRaise stub).
    assert!(matches!(object_make_new(&mut ctx, obj, &[], 0), Err(ObjectError::PyRaise)));
}

#[test]
fn object_init_builtin_arity() {
    let mut fx = Fx::new();
    let mut ctx = fx.ctx();
    assert_eq!(object_init_builtin(&mut ctx, &[Value::None]).unwrap(), Value::None);
    assert!(matches!(object_init_builtin(&mut ctx, &[]), Err(PyError::PyRaise)));
    assert!(matches!(
        object_init_builtin(&mut ctx, &[Value::None, Value::None]),
        Err(PyError::PyRaise)
    ));
}

#[test]
fn object_new_checks() {
    let mut fx = Fx::new();
    let foo = fx.mk_instance_type(b"Foo");
    let obj = fx.object;
    let int_t = fx.types.int;
    let mut ctx = fx.ctx();
    // Not a type object at all.
    assert!(matches!(
        object_new(&mut ctx, Value::SmallInt(1)),
        Err(ObjectError::TypeError(m)) if m == "arg must be user-type"
    ));
    // object itself: a type, but NOT an instance type (probed).
    assert!(matches!(
        object_new(&mut ctx, Value::Heap(obj)),
        Err(ObjectError::TypeError(m)) if m == "arg must be user-type"
    ));
    // int: same (probed).
    assert!(matches!(
        object_new(&mut ctx, Value::Heap(int_t)),
        Err(ObjectError::TypeError(m)) if m == "arg must be user-type"
    ));
    // An instance type passes both checks; allocation succeeds
    // (E2g — `object.__new__` allocates via `new_instance`).
    let v = object_new(&mut ctx, Value::Heap(foo)).expect("instance type allocates");
    let Value::Heap(r) = v else {
        panic!("object_new returns a heap value");
    };
    assert!(matches!(ctx.arena.get(r), Obj::Instance(_)));
    assert_eq!(ctx.arena.header(r).type_, Some(foo));
}

#[test]
fn object_setattr_delattr_checks() {
    let mut fx = Fx::new();
    let foo = fx.mk_instance_type(b"Foo");
    let a = fx.dq(b"a");
    // A header-tagged dummy instance (header type = Foo) so the
    // instance-type check passes without an Instance payload.
    let mut ctx = fx.ctx();
    let dummy = ctx.alloc(Some(foo), Obj::Dict(DictData { map: MapData::new() }));
    let dummy_v = Value::Heap(dummy);
    assert!(is_instance_type(ctx.arena, dummy_v));
    assert!(!is_instance_type(ctx.arena, Value::SmallInt(1)));
    // Checks pass, but the dummy has no Instance payload, so the
    // members-map store has nowhere to go (E2g defensive arm).
    assert!(matches!(
        object_setattr(&mut ctx, dummy_v, Value::Qstr(a), Value::SmallInt(1)),
        Err(ObjectError::PyRaise)
    ));
    assert!(matches!(
        object_delattr(&mut ctx, dummy_v, Value::Qstr(a)),
        Err(ObjectError::PyRaise)
    ));
    // Non-str attr -> bare TypeError.
    assert!(matches!(
        object_setattr(&mut ctx, dummy_v, Value::SmallInt(1), Value::SmallInt(1)),
        Err(ObjectError::BareTypeError)
    ));
    assert!(matches!(
        object_delattr(&mut ctx, dummy_v, Value::SmallInt(1)),
        Err(ObjectError::BareTypeError)
    ));
    // Non-instance self -> "arg must be user-type" (probed:
    // object().__setattr__('a', 1)).
    assert!(matches!(
        object_setattr(&mut ctx, Value::SmallInt(1), Value::Qstr(a), Value::SmallInt(1)),
        Err(ObjectError::TypeError(m)) if m == "arg must be user-type"
    ));
    assert!(matches!(
        object_delattr(&mut ctx, Value::SmallInt(1), Value::Qstr(a)),
        Err(ObjectError::TypeError(m)) if m == "arg must be user-type"
    ));
}

#[test]
fn object_locals_dict_has_four_entries() {
    let mut fx = Fx::new();
    let mut ctx = fx.ctx();
    let dict = object_locals_dict(&mut ctx);
    for name in [b"__init__" as &[u8], b"__new__", b"__setattr__", b"__delattr__"] {
        let v = dict_get_value(&mut ctx, Value::Heap(dict), Value::Qstr(q(name)))
            .expect("locals entry present");
        if name == b"__new__" {
            // E2g: `__new__` is a StaticClassMethod wrapping the
            // FunBuiltin (`objobject.zig:101-107`); the other three
            // are plain builtins. Its header is `None` here (the
            // staticmethod type does not exist yet at build time
            // — the registry fixes it up via
            // `fix_object_new_header`).
            assert!(matches!(v, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::StaticClassMethod(_))));
            assert!(matches!(v, Value::Heap(r) if ctx.arena.header(r).type_.is_none()));
            if let Value::Heap(r) = v {
                if let Obj::StaticClassMethod(d) = ctx.arena.get(r) {
                    assert!(matches!(d.fun, Value::Heap(fr) if matches!(ctx.arena.get(fr), Obj::FunBuiltin(_))));
                }
            }
        } else {
            assert!(matches!(v, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::FunBuiltin(_))));
        }
    }
    // The dict is fixed (ROM table).
    assert!(matches!(ctx.arena.get(dict), Obj::Dict(d) if d.map.is_fixed));
    // Calling __init__ through its builtin returns None.
    let init = dict_get_value(&mut ctx, Value::Heap(dict), Value::Qstr(q(b"__init__"))).unwrap();
    if let Value::Heap(r) = init {
        if let Obj::FunBuiltin(d) = ctx.arena.get(r) {
            assert_eq!((d.func)(&mut ctx, &[Value::None]).unwrap(), Value::None);
        } else {
            panic!("__init__ is not a FunBuiltin");
        }
    }
}
