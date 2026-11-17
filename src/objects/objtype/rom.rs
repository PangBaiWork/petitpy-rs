//! The ROM registry (`objtype.zig:577-745`): the builtin type
//! objects and singletons that exist once per runtime.
//!
//! In the Zig these are static `MpObjTypeFull` ROM structs
//! (`mp_type_type`, `mp_type_super`, `mp_type_staticmethod`,
//! `mp_type_classmethod`, `mp_builtin_issubclass_obj`,
//! `mp_builtin_isinstance_obj`, `native_base_init_wrapper_obj`)
//! plus the const singletons (`mp_const_empty_tuple_obj`,
//! `mp_const_empty_bytes_obj`). In Rust they are heap `TypeData`
//! / objects built once by [`build_rom`], which C13 calls during
//! VM initialisation.
//!
//! Shape decision (brief §"ROM singletons"): the singletons live
//! in [`BuiltinTypes`] (`empty_tuple`, `empty_bytes`), because
//! that is where the code that needs them already looks
//! (`tuple_hash` seeds from `types.empty_tuple`,
//! `new_empty_bytes` returns `types.empty_bytes`,
//! `new_tuple(0)` returns `types.empty_tuple`). The
//! `issubclass` / `isinstance` FunBuiltin objects live on the
//! [`Rom`] itself — they are module builtins (modbuiltins
//! installs them, Phase D), not types.
//!
//! `build_rom` takes the ctx's types as the base and replaces
//! ONLY the objtype-owned fields (`type_`, `object`, `super_`,
//! `staticmethod`, `classmethod`, `boundmeth`, `property`,
//! `empty_tuple`, `native_init_wrapper`, `empty_bytes`). The
//! other builtin types (`int`, `str`, `tuple`, `dict`, …) are
//! built by their own lanes' `*_type_data` constructors and are
//! passed in — C13 owns assembling them with proper headers
//! (header = the type type); this lane does not duplicate that
//! assembly. The replaced fields are exactly the ones the
//! containers fixture builds as plain `make_type` values (see
//! the fixture comment at `containers/tests.rs:159`).
//!
//! Every type built here gets header = the type type (probed:
//! `type(object)`, `type(type)`, `type(super)`,
//! `type(staticmethod)` are all `<class 'type'>`), and
//! `mro = [self, object]` (probed: `type.__bases__`,
//! `super.__bases__`, `staticmethod.__bases__` are all
//! `(object,)`; `issubclass(type, object)` and
//! `issubclass(super, object)` are True). The Zig's ROM types
//! have no parent slot, so `base` stays `None` — the `(object,)`
//! bases tuple and the `(self, object)` MRO come from the
//! no-parent rules in `type_bases_tuple` / `mro_of`, not from a
//! stored parent. The stored `mro` is still written so that
//! `is_subclass_fast`'s MRO fallback sees it.
//!
//! REBASE NOTE (merged 2026-11-18): the `object` type now comes
//! from C12b's `objobject::object_type_data` with the four object
//! locals installed (`object_locals_dict`: `__init__`, `__new__`,
//! `__setattr__`, `__delattr__`), and its `make_new` slot is
//! `object_make_new_slot` below — the Zig's `object_make_new`
//! (arg check + bare alloc), NOT `instance_make_new`: with the
//! locals installed, the `__new__` lookup in `instance_make_new`
//! would find `object.__new__` and raise "arg must be user-type"
//! (the Zig's `object()` never consults `__new__`).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::bound_meth_type_data;
use crate::objects::containers::objdict::dict_new_typed;
use crate::objects::containers::objproperty::property_type_data;
use crate::objects::obj::BuiltinTypes;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{BytesData, GcRef, Obj, TypeData, Value};
use crate::pyerr::PyResult;

use super::errors::arg_check_num_sig;
use super::instance_create::{
    native_base_init_wrapper_obj, new_instance, static_class_method_make_new,
};
use super::objobject::{object_locals_dict, object_type_data};
use super::subclass::{isinstance_fun_obj, issubclass_fun_obj};
use super::typecreate::{
    super_attr, super_make_new, super_print, type_attr, type_call, type_make_new, type_print,
};

/// The ROM registry: the objtype-owned builtin types +
/// singletons, plus the `issubclass` / `isinstance` builtins.
pub struct Rom {
    /// The base types with the objtype-owned fields replaced by
    /// the Rom-built ones (see the module docs).
    pub types: BuiltinTypes,
    /// The `issubclass` FunBuiltin object (`objtype.zig:577`).
    pub issubclass: GcRef,
    /// The `isinstance` FunBuiltin object (`objtype.zig:581`).
    pub isinstance: GcRef,
}

/// A fresh empty dict for a type's locals (`dict_new_typed` with
/// the dict type as header, hint 0). Rooted immediately: the
/// dict is held in a local across the next alloc, and the GC
/// hazard rule (see `value.rs` D6) requires every value held
/// across an alloc to be rooted.
fn fresh_dict(ctx: &mut CallCtx) -> GcRef {
    let r = match dict_new_typed(ctx, ctx.types.dict, 0) {
        Value::Heap(r) => r,
        _ => unreachable!("dict_new_typed always returns a heap dict"),
    };
    ctx.roots.push(Value::Heap(r));
    r
}

/// `mp_type_type` (`objtype.zig:597-630`): the meta-type.
/// Self-referential (header type = itself), flags 0, name
/// `type`, slots make_new / print / call / attr. No parent slot,
/// no other slots.
pub fn type_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"type").expect("type is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: Some(type_call),
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(type_print),
        attr: Some(type_attr),
        make_new: Some(type_make_new),
        bases: None,
    }
}

/// `mp_type_super` (`objtype.zig:631-663`): flags 0, name
/// `super`, slots make_new / print / attr. No call slot.
pub fn super_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"super").expect("super is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(super_print),
        attr: Some(super_attr),
        make_new: Some(super_make_new),
        bases: None,
    }
}

/// `mp_type_staticmethod` (`objtype.zig:666-695`): flags 0,
/// name `staticmethod`, make_new only (no print slot —
/// `repr(staticmethod(...))` is the VM default print,
/// `<staticmethod>`).
pub fn staticmethod_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"staticmethod").expect("staticmethod is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: Some(static_class_method_make_new),
        bases: None,
    }
}

/// `mp_type_classmethod` (`objtype.zig:698-726`): same shape as
/// staticmethod, name `classmethod`; the runtime distinguishes
/// them by type identity.
pub fn classmethod_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"classmethod").expect("classmethod is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: Some(static_class_method_make_new),
        bases: None,
    }
}

/// `object()`'s `make_new` slot — the Zig's `object_make_new`
/// (`objobject.zig:60`): sig `(0, 0, false)`, then a bare
/// allocation. The Zig allocates a bare `MpObjBase`; the Rust
/// allocates the decided `object()` shape instead (an `Instance`
/// with empty members and no subobj — D4 note, observationally
/// identical). This is NOT `instance_make_new` (see the module
/// docs' rebase note) and NOT C12b's `object_make_new` (that
/// stub's `PyRaise` tail is pinned by its test — allocation was
/// C12a's job, and this fn is it).
fn object_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    let n_pos = args.len().saturating_sub(2 * n_kw);
    if let Some(e) = arg_check_num_sig(n_pos, n_kw, 0, 0, false) {
        return Err(e.to_pyerror());
    }
    Ok(new_instance(ctx, type_))
}

/// Build the ROM registry. `ctx.types` is the base (the builtin
/// types built by their lanes); the returned [`Rom::types`] is
/// that base with the objtype-owned fields replaced. Every built
/// object is rooted in `ctx.roots` during construction (the
/// caller keeps the `Rom` alive afterwards).
pub fn build_rom(ctx: &mut CallCtx) -> Rom {
    let base = *ctx.types;
    // The type type first (its header is itself).
    let type_dict = fresh_dict(ctx);
    let type_ = ctx.alloc(None, Obj::Type(type_type_data(type_dict)));
    ctx.arena.set_header_type(type_, Some(type_));
    ctx.roots.push(Value::Heap(type_));
    // object next (the MRO terminator for the rest): C12b's type
    // data with the four object locals installed, and the
    // `object()` make_new slot (see `object_make_new_slot`).
    let d = object_locals_dict(ctx);
    let mut td = object_type_data(d);
    td.make_new = Some(object_make_new_slot);
    let object = ctx.alloc(Some(type_), Obj::Type(td));
    ctx.roots.push(Value::Heap(object));
    // The remaining ROM types, each with mro [self, object].
    // Each is rooted immediately after its alloc (GC hazard:
    // the next alloc may collect an unrooted local).
    let d = fresh_dict(ctx);
    let super_ = ctx.alloc(Some(type_), Obj::Type(super_type_data(d)));
    ctx.roots.push(Value::Heap(super_));
    let d = fresh_dict(ctx);
    let staticmethod = ctx.alloc(
        Some(type_),
        Obj::Type(staticmethod_type_data(d)),
    );
    ctx.roots.push(Value::Heap(staticmethod));
    let d = fresh_dict(ctx);
    let classmethod = ctx.alloc(
        Some(type_),
        Obj::Type(classmethod_type_data(d)),
    );
    ctx.roots.push(Value::Heap(classmethod));
    let d = fresh_dict(ctx);
    let boundmeth = ctx.alloc(
        Some(type_),
        Obj::Type(bound_meth_type_data(d)),
    );
    ctx.roots.push(Value::Heap(boundmeth));
    let d = fresh_dict(ctx);
    let property = ctx.alloc(Some(type_), Obj::Type(property_type_data(d)));
    ctx.roots.push(Value::Heap(property));
    // The mro patch (no allocs in between, so no GC hazard).
    for t in [type_, super_, staticmethod, classmethod, boundmeth, property] {
        if let Obj::Type(d) = ctx.arena.get_mut(t) {
            d.mro = vec![t, object];
        }
    }
    if let Obj::Type(d) = ctx.arena.get_mut(object) {
        d.mro = vec![object];
    }
    // The singletons (headers = the base tuple / bytes types).
    let empty_tuple = ctx.alloc(Some(base.tuple), Obj::Tuple(Vec::new()));
    ctx.roots.push(Value::Heap(empty_tuple));
    let empty_bytes = ctx.alloc(
        Some(base.bytes),
        Obj::Bytes(BytesData { bytes: Vec::new() }),
    );
    ctx.roots.push(Value::Heap(empty_bytes));
    // The native-base `__init__` wrapper singleton.
    let d = fresh_dict(ctx);
    let Value::Heap(native_init_wrapper) = native_base_init_wrapper_obj(ctx, d) else {
        unreachable!("native_base_init_wrapper_obj always returns a heap value");
    };
    ctx.roots.push(Value::Heap(native_init_wrapper));
    // The `issubclass` / `isinstance` FunBuiltin objects.
    let d = fresh_dict(ctx);
    let Value::Heap(issubclass) = issubclass_fun_obj(ctx, d) else {
        unreachable!("issubclass_fun_obj always returns a heap value");
    };
    ctx.roots.push(Value::Heap(issubclass));
    let d = fresh_dict(ctx);
    let Value::Heap(isinstance) = isinstance_fun_obj(ctx, d) else {
        unreachable!("isinstance_fun_obj always returns a heap value");
    };
    ctx.roots.push(Value::Heap(isinstance));
    let mut types = base;
    types.type_ = type_;
    types.object = object;
    types.super_ = super_;
    types.staticmethod = staticmethod;
    types.classmethod = classmethod;
    types.boundmeth = boundmeth;
    types.property = property;
    types.empty_tuple = empty_tuple;
    types.native_init_wrapper = native_init_wrapper;
    types.empty_bytes = empty_bytes;
    Rom {
        types,
        issubclass,
        isinstance,
    }
}
