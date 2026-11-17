//! Tests for the ROM registry (`rom.rs`).

use crate::objects::callables::objfun::call_fun_builtin;
use crate::objects::containers::objtuple::{new_tuple, tuple_hash};
use crate::objects::obj::{obj_id, type_data};
use crate::objects::strings::objstr_a::new_empty_bytes;
use crate::objects::value::{Obj, Value};

use super::rom::{build_rom, Rom};
use super::subclass::is_subclass_fast;
use super::tests::Fx;
use super::typecreate::type_attr;

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
