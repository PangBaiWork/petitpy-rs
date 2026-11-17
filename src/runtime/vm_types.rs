//! The VM's type construction — the heap `TypeData` objects and
//! method tables the VM builds at startup (C13). Split out of
//! `vm.rs` (file-size cap).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::{fun_bc_type_data, fun_builtin_var_type_data};
use crate::objects::callables::objgenerator_types::{
    gen_instance_locals_dict, gen_instance_type_data, gen_wrap_type_data,
};
use crate::objects::containers::objarray::{
    array_type_data, bytearray_type_data, memoryview_type_data,
};
use crate::objects::containers::objarray_methods::ARRAY_METHODS;
use crate::objects::containers::objdict::dict_type_data;
use crate::objects::containers::objdict_methods::DICT_METHODS;
use crate::objects::containers::objlist::list_type_data;
use crate::objects::containers::objlist_methods::LIST_METHODS;
use crate::objects::containers::objrange::range_type_data;
use crate::objects::containers::objset::{frozenset_type_data, set_type_data};
use crate::objects::containers::objset_methods::{FROZENSET_METHODS, SET_METHODS};
use crate::objects::containers::objslice::{slice_type_data, SLICE_METHODS};
use crate::objects::containers::objtuple::{tuple_type_data, TUPLE_METHODS};
use crate::objects::containers::objproperty::PROPERTY_METHODS;
use crate::objects::iters::objenumerate::enumerate_type_data;
use crate::objects::iters::objfilter::filter_type_data;
use crate::objects::iters::objmap::map_type_data;
use crate::objects::iters::objzip::zip_type_data;
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::objexcept_types::{build_exception_types, ExceptionTypes};
use crate::objects::objtype::objmodule::module_type_data;
use crate::objects::objtype::objobject::object_type_data;
use crate::objects::objtype::rom::{
    classmethod_type_data, object_make_new_slot, staticmethod_type_data, super_type_data,
    type_type_data,
};
use crate::objects::primitives::objbool::bool_type_data;
use crate::objects::primitives::objcell::cell_type_data;
use crate::objects::primitives::objint::{fun_n_args, int_locals_dict, int_type_data};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn, MethodEntry};
use crate::objects::strings::objstr_b::{BYTES_METHODS, STR_METHODS};
use crate::objects::strings::objstr_methods_b::{bytes_type_data, str_type_data};
use crate::objects::value::{BytesData, DictData, FunBuiltinData, GcRef, MapData, Obj, TypeData, Value};
use crate::runtime::vm::VmTypes;
use crate::runtime::vm_builtins::{ellipsis_print, format_kw_wrapper};

// ── Type construction ──────────────────────────────────────────────────

/// An empty heap dict (the bootstrap pattern — the type objects do
/// not exist yet, so the header type is `None`).
fn empty_dict(ctx: &mut CallCtx) -> GcRef {
    ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }))
}

/// A plain type object (no slots, empty mro) — the fixture
/// `make_type` pattern, for types whose lane has no `*_type_data`
/// yet (`float`, `complex`, `NoneType`, the `type` placeholder,
/// the ellipsis type).
fn plain_type(ctx: &mut CallCtx, name: u32) -> GcRef {
    let dict = empty_dict(ctx);
    ctx.alloc(
        None,
        Obj::Type(TypeData {
            name,
            flags: 0,
            base: None,
            mro: Vec::new(),
            dict,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
            namedtuple_fields: None,
            attr: None,
            make_new: None,
            bases: None,
        }),
    )
}

/// Build a type's locals dict from a `MethodEntry` table: one
/// `FunBuiltin` per method (header type = the var builtin type —
/// its call slot dispatches on the entry's own sig, observably
/// identical to the fixed-arity call slots, C10a). `kw_overrides`
/// replaces the `kw_func` for a named method (the `str.format`
/// pass-through — see `format_kw_wrapper`).
fn locals_dict_from_entries(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    entries: &[MethodEntry],
    kw_overrides: &[(&str, BuiltinKwFn)],
) -> GcRef {
    let pairs: Vec<(Value, Value)> = entries
        .iter()
        .map(|e| {
            let name = static_qstr_id(e.name.as_bytes())
                .unwrap_or_else(|| panic!("method name is not a static qstr: {}", e.name));
            let kw_func = kw_overrides
                .iter()
                .find(|(n, _)| *n == e.name)
                .map(|(_, f)| *f)
                .or(e.kw_func);
            let fun = ctx.alloc(
                Some(fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name,
                    n_args: fun_n_args(e.min, e.max),
                    func: e.func,
                    kw_func,
                }),
            );
            (Value::Qstr(name), Value::Heap(fun))
        })
        .collect();
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(pairs),
        }),
    )
}

/// Build a type's locals dict from an old-style `(name, fn)` table
/// (C8/C9c's `STR_METHODS` / `SET_METHODS` shape, which carries no
/// sigs): every method gets the permissive `(1, 255)` sig — the
/// method body itself enforces arity with a typed error, so the
/// observable behaviour matches the sig-checked path. `kw_overrides`
/// as above.
fn locals_dict_from_pairs(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    entries: &[(&str, BuiltinFn)],
    kw_overrides: &[(&str, BuiltinKwFn)],
) -> GcRef {
    let pairs: Vec<(Value, Value)> = entries
        .iter()
        .map(|(name, func)| {
            let id = static_qstr_id(name.as_bytes())
                .unwrap_or_else(|| panic!("method name is not a static qstr: {}", name));
            let kw_func = kw_overrides
                .iter()
                .find(|(n, _)| *n == *name)
                .map(|(_, f)| *f);
            let fun = ctx.alloc(
                Some(fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name: id,
                    n_args: fun_n_args(1, 255),
                    func: *func,
                    kw_func,
                }),
            );
            (Value::Qstr(id), Value::Heap(fun))
        })
        .collect();
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(pairs),
        }),
    )
}

/// Build every type object the VM needs, the builtins dict, and the
/// singletons. Called once from `Vm::new` (which then pins the
/// results in `roots.extra`). The type objects are allocated with
/// a `None` header type (the bootstrap pattern — the `type` type
/// does not exist yet) and pointed at `type_` once it exists
/// (see the fix-up below).
///
/// Order matters: the var builtin type comes first (the method
/// `FunBuiltin`s take it as their header type); the object type
/// comes before the module type and the exception types (both
/// take it as a base).
pub(crate) fn build_vm_types(
    ctx: &mut CallCtx,
) -> (BuiltinTypes, VmTypes, ExceptionTypes, Value, Value) {
    // The var builtin type first (header type for every method
    // FunBuiltin below).
    let fbv_dict = empty_dict(ctx);
    let fun_builtin_var = ctx.alloc(
        None,
        Obj::Type(fun_builtin_var_type_data(fbv_dict)),
    );

    // Object (base of the module type and the exception types).
    // The `make_new` slot is the ROM's override (C13b mirrors
    // `build_rom`): `mp_obj_new_type` rejects a base without
    // `make_new`, so `class C: pass` needs it (its default
    // parent is `object`).
    let object_dict = crate::objects::objtype::objobject::object_locals_dict(ctx);
    let mut object_data = object_type_data(object_dict);
    object_data.make_new = Some(object_make_new_slot);
    let object = ctx.alloc(None, Obj::Type(object_data));

    // Primitives.
    let int_dict = int_locals_dict(ctx);
    let int = ctx.alloc(None, Obj::Type(int_type_data(int_dict)));
    let bool_dict = empty_dict(ctx);
    let bool_ = ctx.alloc(None, Obj::Type(bool_type_data(bool_dict)));
    let cell_dict = empty_dict(ctx);
    let cell = ctx.alloc(None, Obj::Type(cell_type_data(cell_dict)));
    let float = plain_type(
        ctx,
        static_qstr_id(b"float").expect("float is a static qstr"),
    );
    let complex = plain_type(
        ctx,
        static_qstr_id(b"complex").expect("complex is a static qstr"),
    );
    let none_type = plain_type(
        ctx,
        static_qstr_id(b"NoneType").expect("NoneType is a static qstr"),
    );

    // Strings (the `format` entry takes the kwargs pass-through —
    // C8b's deferred item, see `format_kw_wrapper`).
    let str_dict = locals_dict_from_pairs(
        ctx,
        fun_builtin_var,
        STR_METHODS,
        &[("format", format_kw_wrapper)],
    );
    let str_ = ctx.alloc(
        None,
        Obj::Type(str_type_data(
            static_qstr_id(b"str").expect("str is a static qstr"),
            str_dict,
        )),
    );
    let bytes_dict = locals_dict_from_pairs(ctx, fun_builtin_var, BYTES_METHODS, &[]);
    let bytes = ctx.alloc(
        None,
        Obj::Type(bytes_type_data(
            static_qstr_id(b"bytes").expect("bytes is a static qstr"),
            bytes_dict,
        )),
    );

    // Containers.
    let tuple_dict = locals_dict_from_entries(ctx, fun_builtin_var, TUPLE_METHODS, &[]);
    let tuple = ctx.alloc(None, Obj::Type(tuple_type_data(tuple_dict)));
    let list_dict = locals_dict_from_entries(ctx, fun_builtin_var, LIST_METHODS, &[]);
    let list = ctx.alloc(None, Obj::Type(list_type_data(list_dict)));
    let dict_dict = locals_dict_from_entries(ctx, fun_builtin_var, DICT_METHODS, &[]);
    let dict = ctx.alloc(None, Obj::Type(dict_type_data(dict_dict)));
    let set_dict = locals_dict_from_pairs(ctx, fun_builtin_var, SET_METHODS, &[]);
    let set = ctx.alloc(None, Obj::Type(set_type_data(set_dict)));
    let frozenset_dict =
        locals_dict_from_pairs(ctx, fun_builtin_var, FROZENSET_METHODS, &[]);
    let frozenset = ctx.alloc(None, Obj::Type(frozenset_type_data(frozenset_dict)));
    let range_dict = empty_dict(ctx);
    let range = ctx.alloc(None, Obj::Type(range_type_data(range_dict)));
    let slice_dict = locals_dict_from_entries(ctx, fun_builtin_var, SLICE_METHODS, &[]);
    let slice = ctx.alloc(None, Obj::Type(slice_type_data(slice_dict)));
    let array_dict = locals_dict_from_entries(ctx, fun_builtin_var, ARRAY_METHODS, &[]);
    let array = ctx.alloc(None, Obj::Type(array_type_data(array_dict)));
    let bytearray_dict = locals_dict_from_entries(ctx, fun_builtin_var, ARRAY_METHODS, &[]);
    let bytearray = ctx.alloc(None, Obj::Type(bytearray_type_data(bytearray_dict)));
    let memoryview_dict = empty_dict(ctx);
    let memoryview = ctx.alloc(None, Obj::Type(memoryview_type_data(memoryview_dict)));

    // `map` / `filter` / `zip` / `enumerate` (C13b): the ROM
    // types from `objmap` / `objfilter` / `objzip` /
    // `objenumerate` (flags `0x0080`, make_new + iter slots, no
    // locals dict); bound as builtins in `Vm::build_builtins`.
    let map_dict = empty_dict(ctx);
    let map = ctx.alloc(None, Obj::Type(map_type_data(map_dict)));
    let filter_dict = empty_dict(ctx);
    let filter = ctx.alloc(None, Obj::Type(filter_type_data(filter_dict)));
    let zip_dict = empty_dict(ctx);
    let zip = ctx.alloc(None, Obj::Type(zip_type_data(zip_dict)));
    let enumerate_dict = empty_dict(ctx);
    let enumerate = ctx.alloc(None, Obj::Type(enumerate_type_data(enumerate_dict)));

    // Callables.
    let fun_bc_dict = empty_dict(ctx);
    let fun_bc = ctx.alloc(None, Obj::Type(fun_bc_type_data(fun_bc_dict)));
    let gen_wrap_dict = empty_dict(ctx);
    let gen_wrap = ctx.alloc(None, Obj::Type(gen_wrap_type_data(gen_wrap_dict)));
    let gen_instance_dict = gen_instance_locals_dict(ctx);
    let gen_instance = ctx.alloc(
        None,
        Obj::Type(gen_instance_type_data(gen_instance_dict)),
    );
    let closure_dict = empty_dict(ctx);
    let closure = ctx.alloc(
        None,
        Obj::Type(crate::objects::callables::objclosure::closure_type_data(closure_dict)),
    );
    let bound_meth_dict = empty_dict(ctx);
    let bound_meth = ctx.alloc(
        None,
        Obj::Type(crate::objects::callables::objboundmeth::bound_meth_type_data(bound_meth_dict)),
    );

    // Module + property (the property type's methods live in its
    // locals dict; property CREATION is a builtin call, D lane).
    let module_dict = empty_dict(ctx);
    let module = ctx.alloc(None, Obj::Type(module_type_data(module_dict, object)));
    let property_dict = locals_dict_from_pairs(ctx, fun_builtin_var, PROPERTY_METHODS, &[]);
    let property = ctx.alloc(
        None,
        Obj::Type(crate::objects::containers::objproperty::property_type_data(property_dict)),
    );

    // Ellipsis: plain type, name qstr 0 (`""` — the reference
    // binary prints `type(...)` as `<class ''>`, probed
    // 2026-11-17), print slot `Ellipsis`.
    let ellipsis_dict = empty_dict(ctx);
    let ellipsis_data = TypeData {
        name: 0,
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict: ellipsis_dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(ellipsis_print),
        namedtuple_fields: None,
        attr: None,
        make_new: None,
        bases: None,
    };
    let ellipsis = ctx.alloc(None, Obj::Type(ellipsis_data));
    let ellipsis_singleton = ctx.alloc(Some(ellipsis), Obj::Ellipsis);

    // The real `mp_type_type` (C13b replaces C13's placeholder):
    // `type_type_data` carries the `make_new` (`type_make_new`)
    // and `call` (`type_call`) slots, so `type(x)` and
    // `type(name, bases, dict)` — and `__build_class__`'s
    // meta-call — go through `type_call`.
    let type_dict = empty_dict(ctx);
    let type_ = ctx.alloc(None, Obj::Type(type_type_data(type_dict)));

    // Exception types (C12b — built on the object type).
    let exc = build_exception_types(ctx, object);

    // The empty-tuple singleton (`mp_const_empty_tuple`) —
    // allocated directly, NOT via `new_tuple`: the bootstrap
    // `ctx.types` here is still the dummy, whose `empty_tuple`
    // is `GcRef(0)` — the first dict — so `new_tuple` would
    // return that dict, and `tuple()` / `type("X", (), {})` /
    // `class C: pass` (all read the `()` singleton) would see
    // a dict. Found by C13b's `tuple()` test.
    let empty_tuple_ref = ctx.alloc(Some(tuple), Obj::Tuple(vec![]));
    let empty_tuple = Value::Heap(empty_tuple_ref);

    // C12a's remaining `BuiltinTypes` fields. `super` /
    // `staticmethod` / `classmethod` use the ROM registry's real
    // `*_type_data` fns (empty locals dicts — the VM builds no
    // user classes, and the MRO patch is the ROM's, not the VM's);
    // `empty_bytes` is the VM-local singleton (header = the VM's
    // `bytes` type, same shape as `rom.rs`); `native_init_wrapper`
    // is the fixture pattern (a plain type named `function` — the
    // VM never calls `instance_make_new`, the only reader of this
    // field, `typecreate.rs:362`).
    let super_dict = empty_dict(ctx);
    let super_ = ctx.alloc(None, Obj::Type(super_type_data(super_dict)));
    let staticmethod_dict = empty_dict(ctx);
    let staticmethod = ctx.alloc(
        None,
        Obj::Type(staticmethod_type_data(staticmethod_dict)),
    );
    let classmethod_dict = empty_dict(ctx);
    let classmethod = ctx.alloc(
        None,
        Obj::Type(classmethod_type_data(classmethod_dict)),
    );
    let empty_bytes = ctx.alloc(
        Some(bytes),
        Obj::Bytes(BytesData { bytes: Vec::new() }),
    );
    let native_init_wrapper = plain_type(
        ctx,
        static_qstr_id(b"function").expect("function is a static qstr"),
    );

    // Point every type object's header at `type_` (C13b): a type
    // is an instance of `type` (the Zig sets `mp_type_type` as
    // the header at creation; Rust allocates the types first
    // because `type_` does not exist yet). Without this
    // `obj_get_type` returns `None` for a type value, so
    // `call_via_type_slot` could not call `str(1)` from an
    // iterator seam (`list(map(str, …))` raised a bare
    // `TypeError("None")`) and `type(str)` had no type. The
    // exception types are included (their builder allocates
    // them with a `None` header too).
    for t in [
        fun_builtin_var, object, int, bool_, cell, float, complex, none_type, str_, bytes,
        tuple, list, dict, set, frozenset, range, slice, array, bytearray, memoryview, map,
        filter, zip, enumerate, fun_bc, gen_wrap, gen_instance, closure, bound_meth, module,
        property, ellipsis, type_, super_, staticmethod, classmethod, native_init_wrapper,
    ] {
        ctx.arena.set_header_type(t, Some(type_));
    }
    for t in exc.all() {
        ctx.arena.set_header_type(t, Some(type_));
    }

    let types = BuiltinTypes {
        int,
        str_,
        bytes,
        bool_,
        none_type,
        float,
        complex,
        tuple,
        list,
        set,
        frozenset,
        range,
        dict,
        array,
        bytearray,
        type_,
        object,
        super_,
        staticmethod,
        classmethod,
        boundmeth: bound_meth,
        empty_tuple: empty_tuple_ref,
        empty_bytes,
        native_init_wrapper,
        property,
    };
    let vm_types = VmTypes {
        fun_bc,
        gen_wrap,
        gen_instance,
        closure,
        bound_meth,
        module,
        object,
        ellipsis,
        slice,
        fun_builtin_var,
        array,
        bytearray,
        memoryview,
        cell,
        property,
        map,
        filter,
        zip,
        enumerate,
    };
    (types, vm_types, exc, empty_tuple, Value::Heap(ellipsis_singleton))
}
