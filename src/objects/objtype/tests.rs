//! Tests for the objtype lane. Shared fixture + subclass tests;
//! lookup tests live in `tests_lookup.rs`, instance tests in
//! `tests_instance.rs`, type-creation tests in
//! `tests_typecreate.rs`.

use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::containers::tests::{empty_dict, Fixture as CFixture};
use crate::objects::obj::BuiltinTypes;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, TypeData, Value};
use crate::runtime::qstr::RuntimeQstr;

use super::seams::{
    with_seams as run_with_seams, with_stub_seams, with_subscr_seams as run_with_subscr_seams,
    SubscrError, VmSeams,
};
use crate::objects::slots::SubscrArg;
use super::tests_instance::fun_bc;
use crate::objects::callables::objboundmeth::bound_meth_type_data;
use crate::objects::callables::objclosure::closure_type_data;
use crate::objects::callables::objfun::fun_bc_type_data;
use crate::objects::obj::type_data;

/// The fixture's user-type flags — the canonical
/// `TYPE_BASE_FLAGS` (`objtype.zig:2015`, `mp_obj_new_type`):
/// `EQ_NOT_REFLEXIVE | EQ_CHECKS_OTHER_TYPE | EQ_HAS_NEQ_TEST |
/// ITER_IS_GETITER(0) | INSTANCE_TYPE = 0x021C`.
pub(crate) use super::typecreate::TYPE_BASE_FLAGS as USER_FLAGS;

/// The objtype test fixture: the containers fixture (real
/// int/str/dict/tuple/list/set/range types with real slots) plus
/// helpers for building user types.
pub(crate) struct Fx {
    pub arena: GcArena,
    pub roots: RootSet,
    pub types: BuiltinTypes,
    pub pool: RuntimeQstr,
    /// C13's `CallCtx` carries the pending-raise slot (5th
    /// `CallCtx::new` arg); the fixture owns one so `ctx()` can
    /// hand out a `CallCtx` on demand.
    pub pending_raise: Option<PendingRaise>,
}

impl Fx {
    pub(crate) fn new() -> Fx {
        let cfx = CFixture::new();
        let mut arena = cfx.arena;
        let mut roots = cfx.roots;
        let types0 = cfx.types;
        let pool = cfx.pool;
        // The ROM registry builds the objtype-owned types +
        // singletons (type, object, super, staticmethod,
        // classmethod, boundmeth, property, empty tuple/bytes,
        // native init wrapper) and the issubclass/isinstance
        // builtins over the fixture's base types, and replaces
        // the fixture's plain `make_type` values with them.
        let mut pending_raise = None;
        let mut ctx = CallCtx::new(&mut arena, &mut roots, &types0, &pool, &mut pending_raise);
        let rom = super::rom::build_rom(&mut ctx);
        Fx { arena, roots, types: rom.types, pool, pending_raise }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool, &mut self.pending_raise)
    }

    /// Run `f` with stub VM seams: the call seam is the
    /// builtin-live `call_via_type_slot`, the load seam finds
    /// nothing. Tests that need a custom `load_maybe` build their
    /// own `VmSeams` inline.
    pub(crate) fn with_seams<R>(&mut self, f: impl FnOnce(&mut CallCtx, &mut VmSeams) -> R) -> R {
        let mut ctx = self.ctx();
        with_stub_seams(|seams| f(&mut ctx, seams))
    }

    /// Like `with_seams` but with a custom `load_maybe` closure
    /// (the other four seams stay the builtin-live stubs).
    pub(crate) fn with_load<R>(
        &mut self,
        load: impl FnMut(&mut CallCtx, Value, u32) -> Option<(Value, Option<Value>)>,
        f: impl FnOnce(&mut CallCtx, &mut VmSeams) -> R,
    ) -> R {
        let mut ctx = self.ctx();
        run_with_seams(load, |seams| f(&mut ctx, seams))
    }

    /// Like `with_seams` but with a custom `subscr` closure (the
    /// other four seams stay the builtin-live stubs).
    pub(crate) fn with_subscr<R>(
        &mut self,
        subscr: impl FnMut(&mut CallCtx, Value, Value, SubscrArg) -> Result<Value, SubscrError>,
        f: impl FnOnce(&mut CallCtx, &mut VmSeams) -> R,
    ) -> R {
        let mut ctx = self.ctx();
        run_with_subscr_seams(subscr, |seams| f(&mut ctx, seams))
    }

    /// Resolve a name to a qstr id: static when present, else
    /// interned dynamically in the pool (fixture type/method
    /// names like `"A"` / `"m"` are NOT static qstrs).
    pub(crate) fn q(&mut self, name: &[u8]) -> u32 {
        match static_qstr_id(name) {
            Some(id) => id,
            // The pool's ids are `Qstr` (u16); the value model
            // carries them as u32.
            None => self.pool.from_strn(name).expect("fixture qstr interning") as u32,
        }
    }

    /// Allocate a user type with a single parent (`base`) and the
    /// given MRO (which must start with the type itself).
    pub(crate) fn user_type(
        &mut self,
        name: &[u8],
        base: Option<GcRef>,
        mro: Vec<GcRef>,
    ) -> GcRef {
        let name = self.q(name);
        let dict = empty_dict(&mut self.arena, &self.roots);
        let data = TypeData {
            name,
            flags: USER_FLAGS,
            base,
            bases: None,
            mro,
            dict,
            namedtuple_fields: None,
            print: None,
            unary_op: None,
            binary_op: None,
            subscr: None,
            buffer: None,
            getiter: None,
            call: None,
            attr: None,
            make_new: None,
        };
        let r = self.arena.alloc(Some(self.types.type_), Obj::Type(data), &self.roots);
        self.roots.push(Value::Heap(r));
        r
    }

    /// Allocate a user type with a `bases` tuple (multiple
    /// inheritance) and the given MRO.
    pub(crate) fn mi_type(
        &mut self,
        name: &[u8],
        bases: Vec<Value>,
        mro: Vec<GcRef>,
    ) -> GcRef {
        let name = self.q(name);
        let dict = empty_dict(&mut self.arena, &self.roots);
        let tuple_type = self.types.tuple;
        let bases_ref = crate::objects::containers::objtuple::new_tuple(
            &mut self.ctx(),
            tuple_type,
            &bases,
        );
        let data = TypeData {
            name,
            flags: USER_FLAGS,
            base: None,
            bases: Some(bases_ref),
            mro,
            dict,
            namedtuple_fields: None,
            print: None,
            unary_op: None,
            binary_op: None,
            subscr: None,
            buffer: None,
            getiter: None,
            call: None,
            attr: None,
            make_new: None,
        };
        let r = self.arena.alloc(Some(self.types.type_), Obj::Type(data), &self.roots);
        self.roots.push(Value::Heap(r));
        r
    }

    /// Store `value` under `key` in a type's locals dict.
    pub(crate) fn set_local(&mut self, type_: GcRef, key: u32, value: Value) {
        let dict = match self.arena.get(type_) {
            Obj::Type(t) => Value::Heap(t.dict),
            _ => panic!("set_local on a non-type"),
        };
        let mut ctx = self.ctx();
        crate::objects::containers::objdict::dict_store(
            &mut ctx,
            dict,
            Value::Qstr(key),
            value,
        )
        .expect("fixture dict store");
    }

    /// Store a `__mro__` tuple in a type's locals dict (the
    /// cached-MRO shape `mp_obj_new_type` writes).
    pub(crate) fn set_mro(&mut self, type_: GcRef, mro: Vec<Value>) {
        let tuple_type = self.types.tuple;
        let mro_tuple =
            Value::Heap(crate::objects::containers::objtuple::new_tuple(
                &mut self.ctx(),
                tuple_type,
                &mro,
            ));
        self.set_local(
            type_,
            static_qstr_id(b"__mro__").expect("__mro__ is a static qstr"),
            mro_tuple,
        );
    }
}

use super::errors::ObjTypeError;
use super::subclass::{
    cast_to_native_base, is_subclass, is_subclass_fast, is_subclass_of_value,
    isinstance_builtin, issubclass_builtin,
};
use crate::objects::callables::objfun::call_fun_builtin;
use super::subclass::{isinstance_fun_obj, issubclass_fun_obj};

#[test]
fn subclass_fast_single_chain() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    // Fix the MROs now that the refs exist (A's MRO is [A, object]).
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
    let (av, bv, cv, ov) =
        (Value::Heap(a), Value::Heap(b), Value::Heap(c), Value::Heap(object));
    assert!(is_subclass_fast(&fx.arena, cv, av));
    assert!(is_subclass_fast(&fx.arena, cv, ov));
    assert!(is_subclass_fast(&fx.arena, bv, bv));
    assert!(!is_subclass_fast(&fx.arena, av, cv));
    assert!(!is_subclass_fast(&fx.arena, av, bv));
    // A non-type object is never a subclass of anything.
    assert!(!is_subclass_fast(&fx.arena, Value::SmallInt(1), av));
    assert!(!is_subclass_fast(&fx.arena, cv, Value::SmallInt(1)));
}

#[test]
fn subclass_fast_mro_fallback() {
    // A type with no recorded parent but an MRO containing
    // `object` (the ROM builtins' shape: `int`'s parent lives
    // only in its MRO).
    let mut fx = Fx::new();
    let object = fx.types.object;
    let x = fx.user_type(b"X", None, vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(x) {
        t.mro = vec![x, object];
    }
    assert!(is_subclass_fast(&fx.arena, Value::Heap(x), Value::Heap(object)));
    assert!(!is_subclass_fast(&fx.arena, Value::Heap(x), Value::Heap(fx.types.int)));
}

#[test]
fn subclass_fast_multiple_inheritance() {
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
    if let Obj::Type(t) = fx.arena.get_mut(d) {
        t.mro = vec![d, a, b, object];
    }
    let (av, bv, dv, ov) =
        (Value::Heap(a), Value::Heap(b), Value::Heap(d), Value::Heap(object));
    assert!(is_subclass_fast(&fx.arena, dv, av));
    assert!(is_subclass_fast(&fx.arena, dv, bv));
    assert!(is_subclass_fast(&fx.arena, dv, ov));
    assert!(!is_subclass_fast(&fx.arena, dv, Value::Heap(fx.types.int)));
    assert!(!is_subclass_fast(&fx.arena, av, bv));
}

#[test]
fn is_subclass_type_tuple_and_errors() {
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
    let (av, bv, iv) = (Value::Heap(a), Value::Heap(b), Value::Heap(fx.types.int));
    let tuple_type = fx.types.tuple;
    let (t1, t2) = fx.with_seams(|ctx, _seams| {
        let t1 = crate::objects::containers::objtuple::new_tuple(
            ctx,
            tuple_type,
            &[av, iv],
        );
        let t2 = crate::objects::containers::objtuple::new_tuple(
            ctx,
            tuple_type,
            &[iv, Value::SmallInt(1)],
        );
        (Value::Heap(t1), Value::Heap(t2))
    });
    fx.with_seams(|ctx, seams| {
        assert_eq!(is_subclass(ctx, bv, av, seams), Ok(Value::Bool(true)));
        assert_eq!(is_subclass(ctx, bv, iv, seams), Ok(Value::Bool(false)));
        assert_eq!(is_subclass(ctx, bv, t1, seams), Ok(Value::Bool(true)));
        // A tuple containing a non-type: no match (the Zig's
        // fast walk returns false for a non-type classinfo).
        assert_eq!(is_subclass(ctx, bv, t2, seams), Ok(Value::Bool(false)));
        // A non-type classinfo is Arg2.
        assert_eq!(
            is_subclass(ctx, bv, Value::SmallInt(1), seams),
            Err(ObjTypeError::IssubclassArg2)
        );
        // `is_subclass_of_value` tests the value's TYPE.
        let one = Value::SmallInt(1);
        assert_eq!(is_subclass_of_value(ctx, one, iv, seams), Ok(Value::Bool(true)));
        assert_eq!(is_subclass_of_value(ctx, one, av, seams), Ok(Value::Bool(false)));
    });
}

#[test]
fn is_subclass_union_via_seam() {
    // A union-like classinfo: the seam answers `__args__` with a
    // tuple and finds no `__origin__` (the `int | None` shape).
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    let av = Value::Heap(a);
    let tuple_type = fx.types.tuple;
    let int_type = fx.types.int;
    let args_tuple = Value::Heap(crate::objects::containers::objtuple::new_tuple(
        &mut fx.ctx(),
        tuple_type,
        &[av, Value::Heap(int_type)],
    ));
    let union_obj = Value::Heap(fx.arena.alloc(
        Some(fx.types.object),
        Obj::Dict(crate::objects::value::DictData {
            map: crate::objects::value::MapData::new(),
        }),
        &fx.roots,
    ));
    let args_q = static_qstr_id(b"__args__").unwrap();
    let origin_q = static_qstr_id(b"__origin__").unwrap();
    let str_type = fx.types.str_;
    let load = move |_ctx: &mut CallCtx, _obj: Value, attr: u32| {
        if attr == args_q {
            Some((args_tuple, None))
        } else {
            None
        }
    };
    let _ = origin_q;
    fx.with_load(load, |ctx, seams| {
        assert_eq!(is_subclass(ctx, av, union_obj, seams), Ok(Value::Bool(true)));
        assert_eq!(
            is_subclass(ctx, Value::Heap(str_type), union_obj, seams),
            Ok(Value::Bool(false))
        );
    });
}

#[test]
fn builtin_issubclass_isinstance_via_dispatch() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    let a = fx.user_type(b"A", Some(object), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(a) {
        t.mro = vec![a, object];
    }
    // An instance of A.
    let inst = Value::Heap(fx.arena.alloc(
        Some(a),
        Obj::Instance(crate::objects::value::InstanceData {
            members: crate::objects::value::MapData::new(),
            subobj: vec![],
        }),
        &fx.roots,
    ));
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let issub = issubclass_fun_obj(&mut fx.ctx(), dict);
    let isinst = isinstance_fun_obj(&mut fx.ctx(), dict);
    let (av, iv) = (Value::Heap(a), Value::Heap(fx.types.int));
    let mut ctx = fx.ctx();
    // `issubclass(A, object)` → True; `issubclass(A, int)` → False.
    assert_eq!(
        call_fun_builtin(&mut ctx, issub, 2, 0, &[av, Value::Heap(object)]),
        Ok(Value::Bool(true))
    );
    assert_eq!(call_fun_builtin(&mut ctx, issub, 2, 0, &[av, iv]), Ok(Value::Bool(false)));
    // Arg1 must be a class.
    assert_eq!(issubclass_builtin(&mut ctx, &[Value::SmallInt(1), iv]), Err(crate::pyerr::PyError::PyRaise));
    assert!(call_fun_builtin(&mut ctx, issub, 1, 0, &[av]).is_err());
    // `isinstance(inst, A)` → True; `isinstance(1, int)` → True.
    assert_eq!(
        call_fun_builtin(&mut ctx, isinst, 2, 0, &[inst, av]),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        call_fun_builtin(&mut ctx, isinst, 2, 0, &[Value::SmallInt(1), iv]),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        call_fun_builtin(&mut ctx, isinst, 2, 0, &[Value::SmallInt(1), av]),
        Ok(Value::Bool(false))
    );
    // Arg2 must be a class or tuple of classes.
    assert!(isinstance_builtin(&mut ctx, &[inst, Value::SmallInt(1)]).is_err());
}

#[test]
fn cast_to_native_base_cases() {
    let mut fx = Fx::new();
    let object = fx.types.object;
    // CI subclasses int: an instance with subobj[0] = 5.
    let ci = fx.user_type(b"CI", Some(fx.types.int), vec![]);
    if let Obj::Type(t) = fx.arena.get_mut(ci) {
        t.mro = vec![ci, fx.types.int, object];
    }
    let with_base = Value::Heap(fx.arena.alloc(
        Some(ci),
        Obj::Instance(crate::objects::value::InstanceData {
            members: crate::objects::value::MapData::new(),
            subobj: vec![Value::SmallInt(5)],
        }),
        &fx.roots,
    ));
    let bare = Value::Heap(fx.arena.alloc(
        Some(ci),
        Obj::Instance(crate::objects::value::InstanceData {
            members: crate::objects::value::MapData::new(),
            subobj: vec![],
        }),
        &fx.roots,
    ));
    let iv = Value::Heap(fx.types.int);
    let sv = Value::Heap(fx.types.str_);
    let types = &fx.types;
    assert_eq!(cast_to_native_base(&fx.arena, types, with_base, iv), Some(Value::SmallInt(5)));
    // A subobj-less instance of an int subclass: the Zig reads
    // subobj[0] unconditionally (UB); Rust returns None.
    assert_eq!(cast_to_native_base(&fx.arena, types, bare, iv), None);
    assert_eq!(cast_to_native_base(&fx.arena, types, Value::SmallInt(5), iv), Some(Value::SmallInt(5)));
    assert_eq!(cast_to_native_base(&fx.arena, types, Value::SmallInt(5), sv), None);
    // CI is not a subclass of str: NULL.
    assert_eq!(cast_to_native_base(&fx.arena, types, with_base, sv), None);
    assert_eq!(cast_to_native_base(&fx.arena, types, bare, sv), None);
}

#[test]
fn attr_slots_wired() {
    // The brief's attr-slot item: C10a's three AttrFn fns are wired
    // into their type datas (objfun.zig:482, objboundmeth.zig:57,
    // objclosure.zig:61).
    let mut fx = Fx::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    assert!(fun_bc_type_data(dict).attr.is_some());
    assert!(bound_meth_type_data(dict).attr.is_some());
    assert!(closure_type_data(dict).attr.is_some());
    // And the slot dispatches through the type data: a FunBc value's
    // `__name__` resolves via the wired `fun_bc_attr`.
    let f = fun_bc(&mut fx, "g");
    let Value::Heap(fr) = f else { unreachable!() };
    let t = fx.arena.header(fr).type_.expect("fun_bc has a header type");
    let attr = type_data(&fx.arena, t).expect("type data").attr.expect("attr slot");
    let name_q = fx.q(b"__name__");
    let mut ctx = fx.ctx();
    let got = attr(&mut ctx, f, name_q).expect("attr call");
    // `__name__` returns the function's own name ("g", dynamically
    // interned — not the queried attr id).
    let Some(Value::Qstr(got_id)) = got else { panic!("expected a qstr, got {got:?}") };
    assert_eq!(fx.pool.qstr_str(got_id as u16), b"g");
}
