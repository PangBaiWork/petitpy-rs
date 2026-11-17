//! Tests for `lookup.rs` (class lookup, member conversion, C3
//! MRO, special accessors, readonly views, op tables).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::{fun_bc_type_data, fun_builtin_2_type_data, new_fun_bc};
use crate::objects::containers::tests::empty_dict;
use crate::objects::obj::{FLAG_BINDS_SELF, FLAG_BUILTIN_FUN};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::{BinaryOp, BuiltinFn, UnaryOp};
use crate::objects::value::{
    FunBuiltinData, GcRef, InstanceData, MapData, Obj, PropertyData,
    StaticClassMethodData, Value,
};

use super::errors::ObjTypeError;
use super::lookup::{
    attr_value_is_special_accessor, binary_op_method_name, c3_mro, class_lookup,
    convert_member_lookup, instance_dict_readonly, mro_of, scan_special_accessors_flag,
    type_bases_tuple, type_locals_dict_readonly, unary_op_method_name, LookupHit,
    SlotKind, FLAG_HAS_SPECIAL_ACCESSORS, MAX_BASES,
};
use super::subclass::isinstance_builtin;
use super::tests::{Fx, USER_FLAGS};

#[test]
fn op_method_tables() {
    let mut fx = Fx::new();
    assert_eq!(unary_op_method_name(UnaryOp::Bool), Some(fx.q(b"__bool__")));
    assert_eq!(unary_op_method_name(UnaryOp::Len), Some(fx.q(b"__len__")));
    assert_eq!(unary_op_method_name(UnaryOp::Hash), Some(fx.q(b"__hash__")));
    assert_eq!(unary_op_method_name(UnaryOp::IntMaybe), Some(fx.q(b"__int__")));
    assert_eq!(unary_op_method_name(UnaryOp::Positive), Some(fx.q(b"__pos__")));
    assert_eq!(unary_op_method_name(UnaryOp::Negative), Some(fx.q(b"__neg__")));
    assert_eq!(unary_op_method_name(UnaryOp::Invert), Some(fx.q(b"__invert__")));
    assert_eq!(unary_op_method_name(UnaryOp::Abs), Some(fx.q(b"__abs__")));
    assert_eq!(unary_op_method_name(UnaryOp::FloatMaybe), Some(fx.q(b"__float__")));
    assert_eq!(unary_op_method_name(UnaryOp::ComplexMaybe), Some(fx.q(b"__complex__")));
    assert_eq!(unary_op_method_name(UnaryOp::Not), None);
    assert_eq!(unary_op_method_name(UnaryOp::Sizeof), None);
    assert_eq!(binary_op_method_name(BinaryOp::Less), Some(fx.q(b"__lt__")));
    assert_eq!(binary_op_method_name(BinaryOp::Add), Some(fx.q(b"__add__")));
    assert_eq!(binary_op_method_name(BinaryOp::Contains), Some(fx.q(b"__contains__")));
    assert_eq!(binary_op_method_name(BinaryOp::InplaceAdd), Some(fx.q(b"__iadd__")));
    assert_eq!(binary_op_method_name(BinaryOp::ReverseAdd), Some(fx.q(b"__radd__")));
    assert_eq!(binary_op_method_name(BinaryOp::Divmod), Some(fx.q(b"__divmod__")));
    assert_eq!(binary_op_method_name(BinaryOp::Power), Some(fx.q(b"__pow__")));
    assert_eq!(binary_op_method_name(BinaryOp::In), None);
    assert_eq!(binary_op_method_name(BinaryOp::Is), None);
    assert_eq!(binary_op_method_name(BinaryOp::ExceptionMatch), None);
    assert_eq!(binary_op_method_name(BinaryOp::NotIn), None);
    assert_eq!(binary_op_method_name(BinaryOp::IsNot), None);
    assert_eq!(MAX_BASES, 15);
}

#[test]
fn convert_plain_and_funbc() {
    let mut fx = Fx::new();
    // A plain (non-heap) member passes through unbound.
    let int_type = fx.types.int;
    let (fun, bound) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(Value::SmallInt(1)), int_type, Value::SmallInt(7))
    });
    assert_eq!((fun, bound), (Value::SmallInt(7), None));
    // A FunBc member (binds-self, not builtin) binds `self`.
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let fb_type = fx.arena.alloc(
        None,
        Obj::Type(fun_bc_type_data(dict)),
        &fx.roots,
    );
    let self_v = Value::SmallInt(9);
    let int_type = fx.types.int;
    let m_q = fx.q(b"m");
    let funbc = Value::Heap(new_fun_bc(
        &mut fx.ctx(),
        fb_type,
        m_q,
        fun_n_args(1, 1),
        0,
        None,
        None,
        None,
    ));
    let (fun, bound) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(self_v), int_type, funbc)
    });
    assert_eq!((fun, bound), (funbc, Some(self_v)));
}

#[test]
fn convert_builtin_fun_native_vs_user() {
    let mut fx = Fx::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let fb2_type = fx.arena.alloc(
        None,
        Obj::Type(fun_builtin_2_type_data(dict)),
        &fx.roots,
    );
    let f_q = fx.q(b"f");
    let builtin = Value::Heap(fx.arena.alloc(
        Some(fb2_type),
        Obj::FunBuiltin(FunBuiltinData {
            name: f_q,
            n_args: fun_n_args(2, 2),
            func: isinstance_builtin as BuiltinFn,
            kw_func: None,
        }),
        &fx.roots,
    ));
    // Sanity: the fun_builtin_2 type really carries both flags.
    let flags = match fx.arena.get(fb2_type) {
        Obj::Type(t) => t.flags,
        _ => 0,
    };
    assert_ne!(flags & FLAG_BINDS_SELF, 0);
    assert_ne!(flags & FLAG_BUILTIN_FUN, 0);
    let self_v = Value::SmallInt(3);
    let int_type = fx.types.int;
    // On a native type (int) the builtin binds `self`.
    let (fun, bound) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(self_v), int_type, builtin)
    });
    assert_eq!((fun, bound), (builtin, Some(self_v)));
    // On a user (instance) type it does NOT bind (staticmethod).
    let object = fx.types.object;
    let user = fx.user_type(b"U", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(user) {
        t.mro = vec![user, object];
    }
    let (fun, bound) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(self_v), user, builtin)
    });
    assert_eq!((fun, bound), (builtin, None));
}

#[test]
fn convert_static_and_classmethod() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let user = fx.user_type(b"U", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(user) {
        t.mro = vec![user, object];
    }
    let inst = Value::Heap(fx.arena.alloc(
        Some(user),
        Obj::Instance(InstanceData { members: MapData::new(), subobj: vec![] }),
        &fx.roots,
    ));
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let fb2_type = fx.arena.alloc(
        None,
        Obj::Type(fun_builtin_2_type_data(dict)),
        &fx.roots,
    );
    let g_q = fx.q(b"g");
    let fun = Value::Heap(fx.arena.alloc(
        Some(fb2_type),
        Obj::FunBuiltin(FunBuiltinData {
            name: g_q,
            n_args: fun_n_args(1, 1),
            func: isinstance_builtin as BuiltinFn,
            kw_func: None,
        }),
        &fx.roots,
    ));
    let sm = Value::Heap(fx.arena.alloc(
        Some(fx.types.staticmethod),
        Obj::StaticClassMethod(StaticClassMethodData { fun }),
        &fx.roots,
    ));
    let cm = Value::Heap(fx.arena.alloc(
        Some(fx.types.classmethod),
        Obj::StaticClassMethod(StaticClassMethodData { fun }),
        &fx.roots,
    ));
    // staticmethod unwraps to the bare fun, unbound.
    let (f, b) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(inst), user, sm)
    });
    assert_eq!((f, b), (fun, None));
    // classmethod binds the INSTANCE's type.
    let (f, b) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(inst), user, cm)
    });
    assert_eq!((f, b), (fun, Some(Value::Heap(user))));
    // classmethod with `self` = a TYPE binds that type itself.
    let (f, b) = fx.with_seams(|ctx, _seams| {
        convert_member_lookup(ctx, Some(Value::Heap(user)), user, cm)
    });
    assert_eq!((f, b), (fun, Some(Value::Heap(user))));
}

#[test]
fn class_lookup_walks_parents_and_binds() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(b) {
        t.mro = vec![b, a, object];
    }
    // A method `m` defined on A (a FunBc: binds self).
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let fb_type = fx.arena.alloc(None, Obj::Type(fun_bc_type_data(dict)), &fx.roots);
    let m_q = fx.q(b"m");
    let m = Value::Heap(new_fun_bc(
        &mut fx.ctx(),
        fb_type,
        m_q,
        fun_n_args(1, 1),
        0,
        None,
        None,
        None,
    ));
    fx.set_local(a, m_q, m);
    let inst = Value::Heap(fx.arena.alloc(
        Some(b),
        Obj::Instance(InstanceData { members: MapData::new(), subobj: vec![] }),
        &fx.roots,
    ));
    // Found via B's parent walk, bound to the B instance.
    let hit = fx.with_seams(|ctx, seams| {
        class_lookup(ctx, Some(inst), m_q, None, false, b, seams)
    });
    assert_eq!(hit, Some(LookupHit::Member(m, Some(inst))));
    // A missing attribute is None.
    let nope_q = fx.q(b"nope");
    let hit = fx.with_seams(|ctx, seams| {
        class_lookup(ctx, Some(inst), nope_q, None, false, b, seams)
    });
    assert_eq!(hit, None);
    // A class lookup (is_type) returns the function UNBOUND
    // (the Zig passes MP_OBJ_NULL as `self` to convert): `B.m`
    // is the plain function, `C().m` is the bound method.
    let hit = fx.with_seams(|ctx, seams| {
        class_lookup(ctx, Some(Value::Heap(b)), m_q, None, true, b, seams)
    });
    assert_eq!(hit, Some(LookupHit::Member(m, None)));
}

#[test]
fn class_lookup_native_slot_fast_path() {
    let mut fx = Fx::new();
    // `int` is native and carries a unary_op slot.
    let int_type = fx.types.int;
    let len_q = fx.q(b"__len__");
    let hit = fx.with_seams(|ctx, seams| {
        class_lookup(ctx, Some(Value::SmallInt(5)), len_q, Some(SlotKind::UnaryOp), false, int_type, seams)
    });
    assert_eq!(hit, Some(LookupHit::NativeSlot(SlotKind::UnaryOp)));
    // `int` has no attr slot: the fast path does not fire, and
    // the locals dict is empty, so the lookup misses.
    let hit = fx.with_seams(|ctx, seams| {
        class_lookup(ctx, Some(Value::SmallInt(5)), len_q, Some(SlotKind::Attr), false, int_type, seams)
    });
    assert_eq!(hit, None);
}

#[test]
fn mro_of_single_chain() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let b = fx.user_type(b"B", Some(a), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(b) {
        t.mro = vec![b, a, object];
    }
    let c = fx.user_type(b"C", Some(b), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(c) {
        t.mro = vec![c, b, a, object];
    }
    let int_type = fx.types.int;
    fx.with_seams(|ctx, _seams| {
        assert_eq!(
            mro_of(ctx, c),
            vec![Value::Heap(c), Value::Heap(b), Value::Heap(a), Value::Heap(object)]
        );
        // A ROM builtin's parent lives only in its MRO.
        assert_eq!(
            mro_of(ctx, int_type),
            vec![Value::Heap(int_type), Value::Heap(object)]
        );
    });
}

#[test]
fn c3_mro_merge_and_conflict() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let b = fx.user_type(b"B", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(b) {
        t.mro = vec![b, object];
    }
    // X(A, B) and Y(B, A), each with a cached MRO.
    let x = fx.mi_type(b"X", vec![Value::Heap(a), Value::Heap(b)], vec![]);
    fx.set_mro(x, vec![Value::Heap(x), Value::Heap(a), Value::Heap(b), Value::Heap(object)]);
    let y = fx.mi_type(b"Y", vec![Value::Heap(b), Value::Heap(a)], vec![]);
    fx.set_mro(y, vec![Value::Heap(y), Value::Heap(b), Value::Heap(a), Value::Heap(object)]);
    let z = fx.user_type(b"Z", None, vec![]);
    // c3(Z, [X, Y]) is inconsistent: X wants A before B, Y wants
    // B before A.
    let err = fx.with_seams(|ctx, _seams| c3_mro(ctx, Value::Heap(z), &[x, y]));
    match err {
        Err(ObjTypeError::MroConflict(names)) => {
            assert_eq!(names, vec![fx.q(b"X"), fx.q(b"Y")]);
        }
        other => panic!("expected MroConflict, got {:?}", other),
    }
    // c3(D, [A, B]) merges cleanly: (D, A, B, object).
    let d = fx.user_type(b"D", None, vec![]);
    let merged = fx.with_seams(|ctx, _seams| c3_mro(ctx, Value::Heap(d), &[a, b]));
    let merged = merged.expect("c3 merge");
    let items = match merged {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Tuple(items) => items.clone(),
            _ => panic!("c3 result is not a tuple"),
        },
        _ => panic!("c3 result is not heap"),
    };
    assert_eq!(
        items,
        vec![Value::Heap(d), Value::Heap(a), Value::Heap(b), Value::Heap(object)]
    );
    // Too many bases.
    let many: Vec<GcRef> = vec![a; 16];
    let err = fx.with_seams(|ctx, _seams| c3_mro(ctx, Value::Heap(d), &many));
    assert_eq!(err, Err(ObjTypeError::TooManyBases));
}

#[test]
fn type_bases_tuple_cases() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let b = fx.user_type(b"B", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(b) {
        t.mro = vec![b, object];
    }
    let d = fx.mi_type(b"D", vec![Value::Heap(a), Value::Heap(b)], vec![]);
    let empty_tuple = Value::Heap(fx.types.empty_tuple);
    let int_type = fx.types.int;
    fx.with_seams(|ctx, _seams| {
        assert_eq!(type_bases_tuple(ctx, object), empty_tuple);
        // Single parent → a fresh 1-tuple.
        let ab = type_bases_tuple(ctx, a);
        match ab {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => assert_eq!(items, &[Value::Heap(object)]),
                _ => panic!("not a tuple"),
            },
            _ => panic!("not heap"),
        }
        assert_ne!(ab, empty_tuple);
        // MI parent → the bases tuple itself.
        let bases = match ctx.arena.get(d) {
            Obj::Type(t) => Value::Heap(t.bases.unwrap()),
            _ => panic!("not a type"),
        };
        assert_eq!(type_bases_tuple(ctx, d), bases);
        // A parentless non-object type → a fresh 1-tuple (object,).
        let ib = type_bases_tuple(ctx, int_type);
        match ib {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => assert_eq!(items, &[Value::Heap(object)]),
                _ => panic!("not a tuple"),
            },
            _ => panic!("not heap"),
        }
    });
}

#[test]
fn readonly_dict_views() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let x_q = fx.q(b"x");
    fx.set_local(a, x_q, Value::SmallInt(1));
    // Unfixed locals → a fixed copy (a different object, same
    // contents); the original stays unfixed.
    let view = fx.with_seams(|ctx, _seams| type_locals_dict_readonly(ctx, a));
    let view = view.expect("readonly view");
    let view_r = match view {
        Value::Heap(r) => r,
        _ => panic!("not heap"),
    };
    let orig_r = match fx.arena.get(a) {
        Obj::Type(t) => t.dict,
        _ => panic!("not a type"),
    };
    assert_ne!(view_r, orig_r);
    assert!(matches!(fx.arena.get(view_r), Obj::Dict(d) if d.map.is_fixed));
    assert!(matches!(fx.arena.get(orig_r), Obj::Dict(d) if !d.map.is_fixed));
    // A fixed dict is returned as-is.
    let view2 = fx.with_seams(|ctx, _seams| type_locals_dict_readonly(ctx, a));
    let view2 = view2.expect("second view");
    assert_ne!(view2, view); // the copy is re-made each time
    // Mark the original fixed: now it IS returned.
    if let Obj::Dict(d) = fx.arena.get_mut(orig_r) {
        d.map.is_fixed = true;
    }
    let view3 = fx.with_seams(|ctx, _seams| type_locals_dict_readonly(ctx, a));
    assert_eq!(view3.expect("fixed view"), Value::Heap(orig_r));
    // Instance members → a fixed copy.
    let y_q = fx.q(b"y");
    let members = MapData::from_pairs(vec![(Value::Qstr(y_q), Value::SmallInt(2))]);
    let inst = Value::Heap(fx.arena.alloc(
        Some(a),
        Obj::Instance(InstanceData { members, subobj: vec![] }),
        &fx.roots,
    ));
    let iview = fx.with_seams(|ctx, _seams| instance_dict_readonly(ctx, inst));
    let iview = iview.expect("instance view");
    assert!(matches!(fx.arena.get(match iview { Value::Heap(r) => r, _ => panic!("heap") }), Obj::Dict(d) if d.map.is_fixed));
}

#[test]
fn special_accessors() {
    let mut fx = Fx::new();
    let setattr_q = fx.q(b"__setattr__");
    let delattr_q = fx.q(b"__delattr__");
    let x_q = fx.q(b"x");
    let p_q = fx.q(b"p");
    let d_q = fx.q(b"d");
    // `__setattr__` / `__delattr__` keys are special.
    let is_special = fx.with_seams(|ctx, seams| {
        attr_value_is_special_accessor(ctx, setattr_q, Value::SmallInt(1), seams)
    });
    assert!(is_special);
    let is_special = fx.with_seams(|ctx, seams| {
        attr_value_is_special_accessor(ctx, delattr_q, Value::SmallInt(1), seams)
    });
    assert!(is_special);
    // A property value is special.
    let prop = Value::Heap(fx.arena.alloc(
        Some(fx.types.property),
        Obj::Property(PropertyData {
            getter: Value::None,
            setter: Value::None,
            deleter: Value::None,
        }),
        &fx.roots,
    ));
    let is_special = fx.with_seams(|ctx, seams| {
        attr_value_is_special_accessor(ctx, x_q, prop, seams)
    });
    assert!(is_special);
    // A plain value with no `__get__`/`__set__`/`__delete__` is not.
    let is_special = fx.with_seams(|ctx, seams| {
        attr_value_is_special_accessor(ctx, x_q, Value::SmallInt(1), seams)
    });
    assert!(!is_special);
    // The scan: a locals dict holding a property flags the type.
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let dict_v = Value::Heap(dict);
    fx.with_seams(|ctx, _seams| {
        crate::objects::containers::objdict::dict_store(ctx, dict_v, Value::Qstr(p_q), prop)
            .expect("store");
    });
    let flag = fx.with_seams(|ctx, seams| scan_special_accessors_flag(ctx, dict_v, seams));
    assert_eq!(flag, FLAG_HAS_SPECIAL_ACCESSORS);
    // The scan: a `__setattr__` key flags the type.
    let dict2 = empty_dict(&mut fx.arena, &fx.roots);
    let dict2_v = Value::Heap(dict2);
    fx.with_seams(|ctx, _seams| {
        crate::objects::containers::objdict::dict_store(
            ctx,
            dict2_v,
            Value::Qstr(setattr_q),
            Value::SmallInt(1),
        )
        .expect("store");
    });
    let flag = fx.with_seams(|ctx, seams| scan_special_accessors_flag(ctx, dict2_v, seams));
    assert_eq!(flag, FLAG_HAS_SPECIAL_ACCESSORS);
    // The scan: a plain dict does not.
    let dict3 = empty_dict(&mut fx.arena, &fx.roots);
    let dict3_v = Value::Heap(dict3);
    fx.with_seams(|ctx, _seams| {
        crate::objects::containers::objdict::dict_store(
            ctx,
            dict3_v,
            Value::Qstr(x_q),
            Value::SmallInt(1),
        )
        .expect("store");
    });
    let flag = fx.with_seams(|ctx, seams| scan_special_accessors_flag(ctx, dict3_v, seams));
    assert_eq!(flag, 0);
    // The scan: a descriptor (an instance-type value exposing
    // `__get__` through the seam) flags the type.
    let object = fx.types.object;
    let user = fx.user_type(b"U", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(user) {
        t.mro = vec![user, object];
    }
    let desc = Value::Heap(fx.arena.alloc(
        Some(user),
        Obj::Instance(InstanceData { members: MapData::new(), subobj: vec![] }),
        &fx.roots,
    ));
    let dict4 = empty_dict(&mut fx.arena, &fx.roots);
    let dict4_v = Value::Heap(dict4);
    fx.with_seams(|ctx, _seams| {
        crate::objects::containers::objdict::dict_store(ctx, dict4_v, Value::Qstr(d_q), desc)
            .expect("store");
    });
    let get_q = fx.q(b"__get__");
    let load = move |_ctx: &mut CallCtx, obj: Value, attr: u32| {
        if obj == desc && attr == get_q {
            Some((Value::SmallInt(0), None))
        } else {
            None
        }
    };
    let flag = fx.with_load(load, |ctx, seams| scan_special_accessors_flag(ctx, dict4_v, seams));
    assert_eq!(flag, FLAG_HAS_SPECIAL_ACCESSORS);
    // USER_FLAGS is the type-creation flags value.
    assert_eq!(USER_FLAGS, 0x021C);
}
