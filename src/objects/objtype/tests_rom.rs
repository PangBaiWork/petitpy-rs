//! Tests for the ROM registry (`rom.rs`).

use crate::objects::callables::objfun::call_fun_builtin;
use crate::objects::containers::objtuple::{new_tuple, tuple_binary_op, tuple_hash, tuple_items};
use crate::objects::iters::objnamedtuple::{namedtuple_make_new, new_namedtuple_type};
use crate::objects::obj::{obj_id, type_data};
use crate::objects::slots::{BinaryOp, PrintKind};
use crate::objects::strings::objstr_a::new_empty_bytes;
use crate::objects::value::{Obj, Value};
use crate::pyerr::PyResult;

use super::instance::instance_load_attr;
use super::instance_create::{instance_make_new, new_instance};
use super::instance_print::instance_print;
use super::objexcept::new_exception_args;
use super::objexcept_types::build_exception_types;
use super::rom::{build_rom, Rom};
use super::subclass::is_subclass_fast;
use super::tests::Fx;
use super::tests_typecreate::{bases_tuple, empty_locals, new_type};
use super::typecreate::{type_attr, type_call};

/// Build a fresh `Rom` over a fresh fixture's base types (the
/// same construction `Fx::new` performs).
fn fresh_rom() -> (Fx, Rom) {
    let mut fx = Fx::new();
    // `Fx::new` already built the Rom once; build a second one
    // over the (Rom-upgraded) base types to exercise `build_rom`
    // as a standalone constructor. Its types are fresh GcRefs,
    // so adopt them as the fixture's types — otherwise the test
    // `CallCtx` (built from `fx.types`) would answer with the
    // first Rom's object / empty tuple while the test expects
    // the second Rom's.
    let mut ctx = fx.ctx();
    let rom = build_rom(&mut ctx);
    drop(ctx);
    fx.types = rom.types;
    (fx, rom)
}

#[test]
fn rom_types_shape() {
    let (fx, rom) = fresh_rom();
    let t = rom.types;
    // The type type is self-referential.
    assert_eq!(fx.arena.header(t.type_).type_, Some(t.type_));
    // Every ROM type's header is the type type.
    for ty in [t.object, t.super_, t.staticmethod, t.classmethod, t.boundmeth, t.property] {
        assert_eq!(fx.arena.header(ty).type_, Some(t.type_));
    }
    // The `type` type carries make_new / print / call / attr.
    let td = type_data(&fx.arena, t.type_).expect("type data");
    assert!(td.make_new.is_some());
    assert!(td.print.is_some());
    assert!(td.call.is_some());
    assert!(td.attr.is_some());
    // `super`: make_new / print / attr, NO call slot.
    let sd = type_data(&fx.arena, t.super_).expect("super data");
    assert!(sd.make_new.is_some());
    assert!(sd.print.is_some());
    assert!(sd.attr.is_some());
    assert!(sd.call.is_none());
    // staticmethod / classmethod: make_new only (no print slot —
    // `repr(staticmethod(...))` is the VM default print).
    for ty in [t.staticmethod, t.classmethod] {
        let d = type_data(&fx.arena, ty).expect("scm data");
        assert!(d.make_new.is_some());
        assert!(d.print.is_none());
        assert!(d.call.is_none());
        assert!(d.attr.is_none());
    }
    // boundmeth carries the wired `attr` slot (C10a's
    // `bound_meth_attr`).
    let bd = type_data(&fx.arena, t.boundmeth).expect("boundmeth data");
    assert!(bd.attr.is_some());
    assert!(bd.call.is_some());
    // The MROs are [self, object]; object's is [object].
    for ty in [t.type_, t.super_, t.staticmethod, t.classmethod, t.boundmeth, t.property] {
        let d = type_data(&fx.arena, ty).expect("mro data");
        assert_eq!(d.mro, vec![ty, t.object]);
    }
    let od = type_data(&fx.arena, t.object).expect("object data");
    assert_eq!(od.mro, vec![t.object]);
    assert!(od.make_new.is_some());
}

#[test]
fn rom_bases_and_mro_via_type_attr() {
    // Probed on the reference binary (`/tmp/c12a_probe_rom.py`):
    // `type.__bases__` / `super.__bases__` /
    // `staticmethod.__bases__` are all `(object,)`; the MROs are
    // `(type, object)` / `(super, object)` /
    // `(staticmethod, object)`.
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    for (ty, name) in [(t.type_, "type"), (t.super_, "super"), (t.staticmethod, "staticmethod")] {
        let bases_q = fx.q(b"__bases__");
        let mro_q = fx.q(b"__mro__");
        // Scope each `CallCtx` so the arena borrow ends before
        // the next read (E0502).
        let bases = {
            let mut ctx = fx.ctx();
            type_attr(&mut ctx, Value::Heap(ty), bases_q)
                .expect("bases load")
                .expect("bases value")
        };
        let Value::Heap(br) = bases else { panic!("bases is not a tuple") };
        let Obj::Tuple(bases_items) = fx.arena.get(br) else { panic!("bases is not a tuple") };
        let bases_items = bases_items.clone();
        assert_eq!(bases_items.as_slice(), &[Value::Heap(t.object)], "{name}.__bases__");
        let mro = {
            let mut ctx = fx.ctx();
            type_attr(&mut ctx, Value::Heap(ty), mro_q)
                .expect("mro load")
                .expect("mro value")
        };
        let Value::Heap(mr) = mro else { panic!("mro is not a tuple") };
        let Obj::Tuple(mro_items) = fx.arena.get(mr) else { panic!("mro is not a tuple") };
        let mro_items = mro_items.clone();
        assert_eq!(
            mro_items.as_slice(),
            &[Value::Heap(ty), Value::Heap(t.object)],
            "{name}.__mro__"
        );
    }
    // And the subclass predicate agrees: `issubclass(type,
    // object)` / `issubclass(super, object)` are True.
    assert!(is_subclass_fast(&fx.arena, Value::Heap(t.type_), Value::Heap(t.object)));
    assert!(is_subclass_fast(&fx.arena, Value::Heap(t.super_), Value::Heap(t.object)));
}

#[test]
fn rom_singletons_identity() {
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    // `new_tuple(0)` IS the singleton.
    let mut ctx = fx.ctx();
    let empty = Value::Heap(new_tuple(&mut ctx, t.tuple, &[]));
    assert_eq!(empty, Value::Heap(t.empty_tuple));
    // `new_empty_bytes` IS the singleton (twice).
    let b1 = new_empty_bytes(&mut ctx, &t);
    let b2 = new_empty_bytes(&mut ctx, &t);
    assert_eq!(b1, Value::Heap(t.empty_bytes));
    assert_eq!(b1, b2);
    // `hash(()) == id(())` holds in Rust now.
    let h = tuple_hash(&mut ctx, &[]).expect("hash(())");
    assert_eq!(h, t.empty_tuple.0 as i64);
    assert_eq!(obj_id(empty), Value::SmallInt(t.empty_tuple.0 as i64));
}

#[test]
fn rom_issubclass_isinstance_builtins() {
    // The Rom's FunBuiltin objects work through the builtin
    // call path (probed: `issubclass(type, object)` True,
    // `issubclass(super, object)` True, `issubclass(int,
    // object)` True).
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    let mut ctx = fx.ctx();
    let issub = Value::Heap(rom.issubclass);
    let isinst = Value::Heap(rom.isinstance);
    let got = call_fun_builtin(&mut ctx, issub, 2, 0, &[Value::Heap(t.type_), Value::Heap(t.object)])
        .expect("issubclass(type, object)");
    assert_eq!(got, Value::Bool(true));
    let got = call_fun_builtin(&mut ctx, issub, 2, 0, &[Value::Heap(t.super_), Value::Heap(t.object)])
        .expect("issubclass(super, object)");
    assert_eq!(got, Value::Bool(true));
    let got = call_fun_builtin(&mut ctx, isinst, 2, 0, &[Value::SmallInt(1), Value::Heap(t.int)])
        .expect("isinstance(1, int)");
    assert_eq!(got, Value::Bool(true));
    let got = call_fun_builtin(&mut ctx, isinst, 2, 0, &[Value::SmallInt(1), Value::Heap(t.str_)])
        .expect("isinstance(1, str)");
    assert_eq!(got, Value::Bool(false));
    // `issubclass` arg 1 must be a class.
    let e = call_fun_builtin(&mut ctx, issub, 2, 0, &[Value::SmallInt(1), Value::Heap(t.int)]);
    assert!(e.is_err());
}

/// Merge-integration (C12a rebase TODO 1): `object()` through
/// `type_call` on the ROM object type, and the four object locals
/// installed by `build_rom`. Probed on the reference binary:
/// `object()` is an instance, `object(1)` → mismatch,
/// `object.__dict__` keys are `['__delattr__', '__init__',
/// '__new__', '__setattr__']`, and `o.__init__` is a bound
/// method returning None.
#[test]
fn rom_object_call_and_locals() {
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    let object_v = Value::Heap(t.object);
    // `object()` → a blank Instance (empty members, no subobj).
    let inst = {
        let mut ctx = fx.ctx();
        type_call(&mut ctx, object_v, 0, 0, &[]).expect("object()")
    };
    match fx.arena.get(match inst {
        Value::Heap(r) => r,
        _ => panic!("object() is not a heap value"),
    }) {
        Obj::Instance(d) => {
            assert!(d.members.used == 0);
            assert!(d.subobj.is_empty());
        }
        _ => panic!("object() is not an Instance"),
    }
    // `object(1)` → the (0, 0, false) sig error.
    let e = {
        let mut ctx = fx.ctx();
        type_call(&mut ctx, object_v, 1, 0, &[Value::SmallInt(1)])
    };
    assert!(e.is_err(), "object(1) must fail the sig check");
    // The four locals are all present on the object type, and
    // `__init__` called on the instance returns None.
    for name in [b"__init__".as_slice(), b"__new__", b"__setattr__", b"__delattr__"] {
        let q = fx.q(name);
        let got = {
            let mut ctx = fx.ctx();
            type_attr(&mut ctx, object_v, q).expect("locals load")
        };
        assert!(got.is_some(), "object type lacks {name:?}");
    }
    let init_q = fx.q(b"__init__");
    let init_fun = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, object_v, init_q).expect("init load").expect("init value")
    };
    let got = {
        let mut ctx = fx.ctx();
        call_fun_builtin(&mut ctx, init_fun, 1, 0, &[inst]).expect("object.__init__(o)")
    };
    assert_eq!(got, Value::None);
}

/// Merge-integration (C12a rebase TODO 1): the object locals
/// are NOT inherited through the chain — the Zig's
/// `npy_ty_class_lookup` stops the parent walk BEFORE searching
/// `object`'s dict (objtype.zig:886-897: the single-parent
/// branch returns when the parent IS `mp_type_object`, and the
/// C3 branch skips it too). Probed on the reference binary:
/// `hasattr(X(), '__init__')` is False and `X().__init__`
/// raises AttributeError, while `object().__init__` IS a bound
/// method returning None. The Rust `class_lookup` reproduces
/// this exactly — this test pins it so a later "fix" does not
/// silently change the original's behaviour.
#[test]
fn rom_object_locals_not_inherited_via_chain() {
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    let bases = bases_tuple(&mut fx, &[Value::Heap(t.object)]);
    let locals = empty_locals(&mut fx);
    let class = new_type(&mut fx, b"X", bases, locals).expect("class X");
    let class_ref = match class {
        Value::Heap(r) => r,
        _ => panic!("class X is not a heap value"),
    };
    let inst = {
        let mut ctx = fx.ctx();
        instance_make_new(&mut ctx, class_ref, &[], 0).expect("X()")
    };
    let init_q = fx.q(b"__init__");
    // A subclass instance does NOT see `object.__init__`.
    let got = fx.with_seams(|ctx, seams| instance_load_attr(ctx, inst, init_q, seams).expect("load"));
    assert!(got.is_none(), "X().__init__ must NOT resolve (original quirk)");
    // The `object()` instance itself DOES (its own type's dict
    // is searched).
    let obj_inst = {
        let mut ctx = fx.ctx();
        type_call(&mut ctx, Value::Heap(t.object), 0, 0, &[]).expect("object()")
    };
    let got = fx.with_seams(|ctx, seams| {
        instance_load_attr(ctx, obj_inst, init_q, seams).expect("load")
    });
    let bound = got.expect("object().__init__ must resolve");
    let (func, self_) = match fx.arena.get(match bound {
        Value::Heap(r) => r,
        _ => panic!("__init__ is not a heap value"),
    }) {
        Obj::BoundMeth(d) => (d.func, d.self_),
        _ => panic!("__init__ is not a bound method"),
    };
    assert_eq!(self_, obj_inst, "__init__ is bound to the instance");
    // The bound function IS object.__init__ (the same FunBuiltin
    // value the object type's locals dict holds).
    let init_fun = {
        let mut ctx = fx.ctx();
        type_attr(&mut ctx, Value::Heap(t.object), init_q).expect("init load").expect("init value")
    };
    assert_eq!(func, init_fun);
}

/// Merge-integration (C12a rebase TODO 3): the tuple ADD
/// subclass-fast fix against a C11-constructed namedtuple type.
/// Probed on the reference binary: `P(1,2)+(3,) == (1,2,3)`
/// and `(1,2)+P(1,2) == (1,2,1,2)`, both plain tuples.
#[test]
fn rom_namedtuple_add() {
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    // Build the class `P(x, y)` (the C11 factory pattern).
    let p_id = fx.pool.from_str("P").unwrap() as u32;
    let x_id = fx.pool.from_str("x").unwrap() as u32;
    let y_id = fx.pool.from_str("y").unwrap() as u32;
    let xy_id = fx.pool.from_str("x y").unwrap() as u32;
    let _ = (x_id, y_id);
    let class = {
        let mut ctx = fx.ctx();
        let pool = ctx.pool;
        let mut intern = move |b: &[u8]| -> PyResult<u32> {
            let id = pool.find_strn(b);
            if id == 0 {
                panic!("test intern seam: uninterned name {}", String::from_utf8_lossy(b))
            } else {
                Ok(id as u32)
            }
        };
        new_namedtuple_type(&mut ctx, Value::Qstr(p_id), Value::Qstr(xy_id), &mut intern)
            .expect("namedtuple P")
    };
    let class_ref = match class {
        Value::Heap(r) => r,
        _ => panic!("namedtuple class is not a heap value"),
    };
    // `P(1, 2)` — the make_new needs no interning (positional
    // args only).
    let p = {
        let mut ctx = fx.ctx();
        let pool = ctx.pool;
        let mut intern = move |b: &[u8]| -> PyResult<u32> {
            let id = pool.find_strn(b);
            if id == 0 {
                panic!("test intern seam: uninterned name {}", String::from_utf8_lossy(b))
            } else {
                Ok(id as u32)
            }
        };
        namedtuple_make_new(&mut ctx, class_ref, &[Value::SmallInt(1), Value::SmallInt(2)], 0, &mut intern)
            .expect("P(1, 2)")
    };
    let plain = {
        let mut ctx = fx.ctx();
        Value::Heap(new_tuple(&mut ctx, t.tuple, &[Value::SmallInt(3)]))
    };
    let sum = {
        let mut ctx = fx.ctx();
        tuple_binary_op(&mut ctx, BinaryOp::Add, p, plain).expect("P(1,2)+(3,)")
    };
    assert_eq!(
        tuple_items(&mut fx.ctx(), sum).expect("sum items").to_vec(),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
    );
    // The sum is a PLAIN tuple (header type is the tuple type).
    let Value::Heap(sr) = sum else { panic!("sum is not a heap value") };
    assert_eq!(fx.arena.header(sr).type_, Some(t.tuple));
    let plain2 = {
        let mut ctx = fx.ctx();
        Value::Heap(new_tuple(&mut ctx, t.tuple, &[Value::SmallInt(1), Value::SmallInt(2)]))
    };
    let sum2 = {
        let mut ctx = fx.ctx();
        tuple_binary_op(&mut ctx, BinaryOp::Add, plain2, p).expect("(1,2)+P(1,2)")
    };
    assert_eq!(
        tuple_items(&mut fx.ctx(), sum2).expect("sum2 items").to_vec(),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(1), Value::SmallInt(2)]
    );
}

/// Merge-integration (C12a rebase TODO 2): `instance_print`'s
/// exception-subclass branch — a user exception subclass
/// instance (native `ValueError` as `subobj[0]`) prints the
/// subclass name with the base's args. Probed on the reference
/// binary: `repr(MyError(42)) == "MyError(42,)"` and
/// `str(MyError(42)) == "42"`. (An int arg is used because the
/// containers fixture's str print is a test double that always
/// quotes — the `str` form of a str arg is covered by the C12b
/// fixture, whose str print is the real one.)
#[test]
fn rom_exception_subclass_print() {
    let (mut fx, rom) = fresh_rom();
    let t = rom.types;
    // The 28 ROM exception types over this Rom's object.
    let exc = {
        let mut ctx = fx.ctx();
        build_exception_types(&mut ctx, t.object)
    };
    // `class MyError(ValueError): pass` — the base already has
    // a make_new slot (the exception make_new), so no
    // `make_newable` overwrite.
    let bases = bases_tuple(&mut fx, &[Value::Heap(exc.value_error)]);
    let locals = empty_locals(&mut fx);
    let class = new_type(&mut fx, b"MyError", bases, locals).expect("class MyError");
    let class_ref = match class {
        Value::Heap(r) => r,
        _ => panic!("class MyError is not a heap value"),
    };
    // The native base: `ValueError(42)`.
    let native = {
        let mut ctx = fx.ctx();
        new_exception_args(&mut ctx, exc.value_error, &[Value::SmallInt(42)])
    };
    // A blank instance of the subclass, with the native
    // exception installed as `subobj[0]` (the wrapper primed by
    // `new_instance` for the one native base).
    let inst = {
        let mut ctx = fx.ctx();
        let inst = new_instance(&mut ctx, class_ref);
        let Value::Heap(ir) = inst else { panic!("instance is not a heap value") };
        match ctx.arena.get_mut(ir) {
            Obj::Instance(d) => {
                assert_eq!(d.subobj.len(), 1, "one native base (ValueError)");
                d.subobj[0] = native;
            }
            _ => panic!("instance is not an Instance"),
        }
        inst
    };
    let repr = {
        let mut ctx = fx.ctx();
        let mut out = String::new();
        instance_print(&mut ctx, inst, PrintKind::Repr, &mut out);
        out
    };
    assert_eq!(repr, "MyError(42,)");
    let s = {
        let mut ctx = fx.ctx();
        let mut out = String::new();
        instance_print(&mut ctx, inst, PrintKind::Str, &mut out);
        out
    };
    assert_eq!(s, "42");
}

