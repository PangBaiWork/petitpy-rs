//! The VM's type construction — the heap `TypeData` objects and
//! method tables the VM builds at startup (C13). Split out of
//! `vm.rs` (file-size cap).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objcode::code_type_data;
use crate::objects::callables::objfun::{fun_bc_type_data, fun_builtin_var_type_data};
use crate::objects::callables::objgenerator_types::{
    gen_instance_locals_dict, gen_instance_type_data, gen_wrap_type_data,
};
use crate::objects::containers::objarray::{
    array_type_data, bytearray_type_data, memoryview_type_data,
};
use crate::objects::containers::objarray_methods::ARRAY_METHODS;
use crate::objects::containers::objdict::dict_type_data;
use crate::objects::containers::objdict_methods::{
    dict_view_it_type_data, dict_view_type_data, DICT_METHODS,
};
use crate::objects::containers::objattrtuple::attrtuple_type_data;
use crate::objects::containers::objfileio::build_fileio_types;
use crate::objects::containers::objlist::list_type_data;
use crate::objects::containers::objlist_methods::LIST_METHODS;
use crate::objects::containers::objrange::{range_it_type_data, range_type_data};
use crate::objects::containers::objset::{frozenset_type_data, set_type_data};
use crate::objects::containers::objset_methods::{FROZENSET_METHODS, SET_METHODS};
use crate::objects::containers::objslice::{slice_type_data, SLICE_METHODS};
use crate::objects::containers::objtuple::{tuple_type_data, TUPLE_METHODS};
use crate::objects::containers::objproperty::PROPERTY_METHODS;
use crate::objects::iters::iter::poly_iter_type_data;
use crate::objects::iters::objenumerate::enumerate_type_data;
use crate::objects::iters::objfilter::filter_type_data;
use crate::objects::iters::objmap::map_type_data;
use crate::objects::iters::objreversed::reversed_type_data;
use crate::objects::iters::objzip::zip_type_data;
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::instance_create::native_base_init_wrapper_obj;
use crate::objects::objtype::objexcept_types::{build_exception_types, ExceptionTypes};
use crate::objects::objtype::objmodule::module_type_data;
use crate::objects::objtype::objobject::{fix_object_new_header, object_type_data};
use crate::objects::objtype::rom::{
    classmethod_type_data, object_make_new_slot, staticmethod_type_data, super_type_data,
    type_type_data,
};
use crate::objects::primitives::objbool::bool_type_data;
use crate::objects::primitives::objcell::cell_type_data;
use crate::objects::primitives::objcomplex::complex_type_data;
use crate::objects::primitives::objfloat::float_type_data;
use crate::objects::primitives::objint::{fun_n_args, int_locals_dict, int_type_data};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn, MethodEntry};
use crate::objects::strings::objstr_b::{
    bytes_decode_kw_wrapper, str_encode_kw_wrapper, str_replace_kw_wrapper,
    str_rsplit_kw_wrapper, str_split_kw_wrapper, str_splitlines_kw_wrapper, BYTES_METHODS,
    STR_METHODS,
};
use crate::objects::strings::objstr_methods_b::{bytes_type_data, str_type_data};
use crate::objects::value::{
    BytesData, DictData, FunBuiltinData, GcRef, MapData, Obj, StaticClassMethodData, TypeData,
    Value,
};
use crate::runtime::vm_builtins::{ellipsis_print, format_kw_wrapper, none_print};

/// The type objects the VM owns beyond `BuiltinTypes` (built once
/// by `build_vm_types`, rooted in `roots.extra` for the VM's
/// lifetime). Moved here from `vm.rs` by E1 (file-size cap).
#[derive(Clone, Copy)]
pub struct VmTypes {
    pub fun_bc: GcRef,
    pub gen_wrap: GcRef,
    pub gen_instance: GcRef,
    pub closure: GcRef,
    pub bound_meth: GcRef,
    pub module: GcRef,
    pub object: GcRef,
    pub ellipsis: GcRef,
    pub slice: GcRef,
    pub fun_builtin_var: GcRef,
    pub array: GcRef,
    pub bytearray: GcRef,
    pub memoryview: GcRef,
    /// The cell type (`MAKE_CLOSURE` allocates cells with it).
    pub cell: GcRef,
    /// The property type (bound as `property` in builtins).
    pub property: GcRef,
    /// The `map` type (bound as `map` in builtins, C13b).
    pub map: GcRef,
    /// The `filter` type (bound as `filter` in builtins, C13b).
    pub filter: GcRef,
    /// The `zip` type (bound as `zip` in builtins, C13b).
    pub zip: GcRef,
    /// The `enumerate` type (bound as `enumerate` in builtins,
    /// C13b).
    pub enumerate: GcRef,
    /// The `reversed` type (bound as `reversed` in builtins, D5).
    pub reversed: GcRef,
    /// The `code` type (the type of `compile()` results, E1).
    pub code: GcRef,
}

// ── Type construction ──────────────────────────────────────────────────

/// An empty heap dict (the bootstrap pattern — the type objects do
/// not exist yet, so the header type is `None`).
fn empty_dict(ctx: &mut CallCtx) -> GcRef {
    ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }))
}

/// A plain type object (no slots, empty mro) — the fixture
/// `make_type` pattern, for types whose lane has no `*_type_data`
/// yet (`NoneType`, the `type` placeholder, the ellipsis type).
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
            protocol: None,
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
/// `MethodEntry` tables into a type's locals dict (C9c's shape,
/// which carries sigs). `kw_overrides` as above; `cm_overrides`
/// names the entries that are classmethods in the original (each
/// with its `(min, max)` sig counting `cls`, e.g.
/// `dict.fromkeys` (2, 3) — `objdict.zig:741-743`): the
/// `FunBuiltin` is wrapped in a `StaticClassMethod` object headed
/// by the `classmethod` type (the `bytes.fromhex` pattern).
/// `classmethod` is `None` for builders that run without the
/// `VmTypes` classmethod type (module locals — none of their
/// tables has a classmethod); then `cm_overrides` must be empty.
pub(crate) fn locals_dict_from_entries(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    classmethod: Option<GcRef>,
    entries: &[MethodEntry],
    kw_overrides: &[(&str, BuiltinKwFn)],
    cm_overrides: &[(&str, u16)],
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
            let cm_sig = cm_overrides
                .iter()
                .find(|(n, _)| *n == e.name)
                .map(|(_, s)| *s);
            let fun = ctx.alloc(
                Some(fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name,
                    n_args: cm_sig.unwrap_or_else(|| fun_n_args(e.min, e.max)),
                    func: e.func,
                    kw_func,
                }),
            );
            let value = if cm_sig.is_some() {
                let classmethod = classmethod
                    .expect("classmethod override needs the classmethod type");
                ctx.alloc(
                    Some(classmethod),
                    Obj::StaticClassMethod(StaticClassMethodData {
                        fun: Value::Heap(fun),
                    }),
                )
            } else {
                fun
            };
            (Value::Qstr(name), Value::Heap(value))
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
/// as above. `cm_overrides` names the entries that are classmethods
/// in the original (each with its `(min, max)` sig counting `cls`,
/// e.g. `bytes.fromhex` (2, 3) — `objstr.zig:3512-3520`): the
/// `FunBuiltin` is wrapped in a `StaticClassMethod` object headed
/// by the `classmethod` type (the `int.from_bytes` pattern,
/// `objint.zig:431-438`), so the class binds as `args[0]` and the
/// entry prints `<bound_method>`.
fn locals_dict_from_pairs(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    classmethod: GcRef,
    entries: &[(&str, BuiltinFn)],
    kw_overrides: &[(&str, BuiltinKwFn)],
    cm_overrides: &[(&str, u16)],
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
            let cm_sig = cm_overrides
                .iter()
                .find(|(n, _)| *n == *name)
                .map(|(_, s)| *s);
            let fun = ctx.alloc(
                Some(fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name: id,
                    n_args: cm_sig.unwrap_or_else(|| fun_n_args(1, 255)),
                    func: *func,
                    kw_func,
                }),
            );
            let value = if cm_sig.is_some() {
                ctx.alloc(
                    Some(classmethod),
                    Obj::StaticClassMethod(StaticClassMethodData {
                        fun: Value::Heap(fun),
                    }),
                )
            } else {
                fun
            };
            (Value::Qstr(id), Value::Heap(value))
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

    // `staticmethod` / `classmethod` come before the int dict:
    // `int.from_bytes` is a classmethod object (see
    // `int_locals_dict`), so the classmethod type must exist
    // when the dict is built. Both use the ROM registry's real
    // `*_type_data` fns (empty locals dicts — the VM builds no
    // user classes, and the MRO patch is the ROM's, not the
    // VM's).
    let staticmethod_dict = empty_dict(ctx);
    let staticmethod = ctx.alloc(
        None,
        Obj::Type(staticmethod_type_data(staticmethod_dict)),
    );
    // E2g: point `object.__new__`'s wrapper at the real
    // staticmethod type (see `object_locals_dict` — it is
    // allocated with header `None`).
    fix_object_new_header(ctx, object_dict, staticmethod);
    let classmethod_dict = empty_dict(ctx);
    let classmethod = ctx.alloc(
        None,
        Obj::Type(classmethod_type_data(classmethod_dict)),
    );

    // Primitives.
    let int_dict = int_locals_dict(ctx, fun_builtin_var, classmethod);
    let int = ctx.alloc(None, Obj::Type(int_type_data(int_dict)));
    let bool_dict = empty_dict(ctx);
    let bool_ = ctx.alloc(None, Obj::Type(bool_type_data(bool_dict)));
    let cell_dict = empty_dict(ctx);
    let cell = ctx.alloc(None, Obj::Type(cell_type_data(cell_dict)));
    // C18a: float / complex carry their vendor slots now
    // (`objfloat.rs` / `objcomplex.rs`); the vendor types have no
    // locals_dict, so the bootstrap empty dict is passed.
    let float_dict = empty_dict(ctx);
    let float = ctx.alloc(None, Obj::Type(float_type_data(float_dict)));
    let complex_dict = empty_dict(ctx);
    let complex = ctx.alloc(None, Obj::Type(complex_type_data(complex_dict)));
    let none_type = plain_type(
        ctx,
        static_qstr_id(b"NoneType").expect("NoneType is a static qstr"),
    );
    // D1: the vendor `objnone.c` type carries `none_print`
    // (`mp_type_roms.zig:191-201`); `plain_type` leaves the
    // print slot empty, which made `repr(None)` print
    // `"<NoneType>"`.
    if let Obj::Type(d) = ctx.arena.get_mut(none_type) {
        d.print = Some(none_print);
    }

    // Strings (the `format` entry takes the kwargs pass-through —
    // C8b's deferred item, see `format_kw_wrapper`; `split` /
    // `rsplit` / `replace` / `encode` take the `kwVar` wrappers —
    // E2f, see `str_split_kw_wrapper`).
    let str_dict = locals_dict_from_pairs(
        ctx,
        fun_builtin_var,
        classmethod,
        STR_METHODS,
        &[
            ("format", format_kw_wrapper),
            ("splitlines", str_splitlines_kw_wrapper),
            ("split", str_split_kw_wrapper),
            ("rsplit", str_rsplit_kw_wrapper),
            ("replace", str_replace_kw_wrapper),
            ("encode", str_encode_kw_wrapper),
        ],
        &[],
    );
    let str_ = ctx.alloc(
        None,
        Obj::Type(str_type_data(
            static_qstr_id(b"str").expect("str is a static qstr"),
            str_dict,
        )),
    );
    let bytes_dict = locals_dict_from_pairs(
        ctx,
        fun_builtin_var,
        classmethod,
        BYTES_METHODS,
        &[
            ("splitlines", str_splitlines_kw_wrapper),
            ("split", str_split_kw_wrapper),
            ("rsplit", str_rsplit_kw_wrapper),
            ("replace", str_replace_kw_wrapper),
            ("decode", bytes_decode_kw_wrapper),
        ],
        &[("fromhex", fun_n_args(2, 3))],
    );
    let bytes = ctx.alloc(
        None,
        Obj::Type(bytes_type_data(
            static_qstr_id(b"bytes").expect("bytes is a static qstr"),
            bytes_dict,
        )),
    );

    // Containers.
    let tuple_dict = locals_dict_from_entries(ctx, fun_builtin_var, Some(classmethod), TUPLE_METHODS, &[], &[]);
    let tuple = ctx.alloc(None, Obj::Type(tuple_type_data(tuple_dict)));
    let list_dict = locals_dict_from_entries(ctx, fun_builtin_var, Some(classmethod), LIST_METHODS, &[], &[]);
    let list = ctx.alloc(None, Obj::Type(list_type_data(list_dict)));
    let dict_dict = locals_dict_from_entries(ctx, fun_builtin_var, Some(classmethod), DICT_METHODS, &[], &[("fromkeys", fun_n_args(2, 3))]);
    let dict = ctx.alloc(None, Obj::Type(dict_type_data(dict_dict)));
    let set_dict = locals_dict_from_pairs(ctx, fun_builtin_var, classmethod, SET_METHODS, &[], &[]);
    let set = ctx.alloc(None, Obj::Type(set_type_data(set_dict)));
    let frozenset_dict =
        locals_dict_from_pairs(ctx, fun_builtin_var, classmethod, FROZENSET_METHODS, &[], &[]);
    let frozenset = ctx.alloc(None, Obj::Type(frozenset_type_data(frozenset_dict)));
    let range_dict = empty_dict(ctx);
    let range = ctx.alloc(None, Obj::Type(range_type_data(range_dict)));
    let slice_dict = locals_dict_from_entries(ctx, fun_builtin_var, Some(classmethod), SLICE_METHODS, &[], &[]);
    let slice = ctx.alloc(None, Obj::Type(slice_type_data(slice_dict)));
    let array_dict = locals_dict_from_entries(ctx, fun_builtin_var, Some(classmethod), ARRAY_METHODS, &[], &[]);
    let array = ctx.alloc(None, Obj::Type(array_type_data(array_dict)));
    // E2e: bytearray's locals are the vendor's shared table
    // (`array_bytearray_str_bytes_locals_table`,
    // `objstr.zig:3533-3614`) — `ARRAY_METHODS` (append, extend)
    // plus `BYTES_METHODS` (hex, fromhex, decode, find, …,
    // isalnum). The bytes entries take the bytes dict's
    // (1, 255) sig (their wrappers enforce their own arity);
    // `fromhex` stays a classmethod and `splitlines` keeps its
    // kw wrapper, as on `bytes`.
    let mut bytearray_entries: Vec<MethodEntry> = ARRAY_METHODS
        .iter()
        .map(|e| MethodEntry {
            name: e.name,
            func: e.func,
            min: e.min,
            max: e.max,
            kw_func: e.kw_func,
        })
        .collect();
    bytearray_entries.extend(BYTES_METHODS.iter().map(|(name, func)| MethodEntry {
        name,
        func: *func,
        min: 1,
        max: 255,
        kw_func: None,
    }));
    let bytearray_dict = locals_dict_from_entries(
        ctx,
        fun_builtin_var,
        Some(classmethod),
        &bytearray_entries,
        &[("splitlines", str_splitlines_kw_wrapper)],
        &[("fromhex", fun_n_args(2, 3))],
    );
    let bytearray = ctx.alloc(None, Obj::Type(bytearray_type_data(bytearray_dict)));
    // Vendor `mp_obj_memoryview_locals_dict` (objstr.zig:3615-3616):
    // just the `hex` entry — `bytes_hex` reads self through the
    // buffer protocol, which covers the memoryview's source chain.
    let memoryview_dict = locals_dict_from_entries(
        ctx,
        fun_builtin_var,
        Some(classmethod),
        &[MethodEntry {
            name: "hex",
            func: crate::objects::strings::objstr_methods_b::bytes_hex_builtin,
            min: 1,
            max: 255,
            kw_func: None,
        }],
        &[],
        &[],
    );
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
    // `reversed` (D5): the ROM type from `objreversed` (flags
    // `0x0080`, make_new + iter slots, no locals dict); bound as
    // a builtin in `Vm::build_builtins`. The original registers
    // it in `modbuiltins.zig:1082` — D1 missed it, which broke
    // frozen `heapq` (`heapify` uses `reversed`).
    let reversed_dict = empty_dict(ctx);
    let reversed = ctx.alloc(None, Obj::Type(reversed_type_data(reversed_dict)));
    // `dict_view` + `dict_view_it` (E2): the ROM types from
    // `objdict_methods` — one `dict_view` type for all three
    // kinds, one `iterator`-named iternnext type for the view
    // iterators. Built here (not lazily) so `make_view` /
    // `make_view_it` can read them from `ctx.types`.
    let dict_view_dict = empty_dict(ctx);
    let dict_view = ctx.alloc(None, Obj::Type(dict_view_type_data(dict_view_dict)));
    let dict_view_it_dict = empty_dict(ctx);
    let dict_view_it = ctx.alloc(None, Obj::Type(dict_view_it_type_data(dict_view_it_dict)));
    // `iterator` + `range_it` (G4/H8): the `mp_type_polymorph_iter`
    // and `mp_type_range_it` ROM types (both named `iterator`,
    // distinct objects — the reference reports `type(iter([1])) is
    // type(iter(range(3)))` False). Built here (not lazily) so the
    // getiter paths can read them from `ctx.types`.
    let iterator_dict = empty_dict(ctx);
    let iterator = ctx.alloc(None, Obj::Type(poly_iter_type_data(iterator_dict)));
    let range_it_dict = empty_dict(ctx);
    let range_it = ctx.alloc(None, Obj::Type(range_it_type_data(range_it_dict)));
    // `code` (E1): the ROM type from `objcode.c` (BASIC level).
    // The name `"code"` IS a static qstr (in the generated
    // table — `from_strn` finds it there); the type has no slots.
    let code_name = ctx
        .pool
        .from_strn(b"code")
        .expect("code name interns") as u32;
    let code_dict = empty_dict(ctx);
    let code = ctx.alloc(None, Obj::Type(code_type_data(code_dict, code_name)));

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
    let property_dict = locals_dict_from_pairs(ctx, fun_builtin_var, classmethod, PROPERTY_METHODS, &[], &[]);
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
        protocol: None,
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

    // C12a's remaining `BuiltinTypes` fields. `super` uses the
    // ROM registry's real `*_type_data` fn (empty locals dict —
    // the VM builds no user classes, and the MRO patch is the
    // ROM's, not the VM's); `staticmethod` / `classmethod` were
    // built above, before the int dict. `empty_bytes` is the
    // VM-local singleton (header = the VM's `bytes` type, same
    // shape as `rom.rs`).
    //
    // The native-base `__init__` wrapper singleton (E2f): the
    // REAL `native_base_init_wrapper_obj` (the same fn `rom.rs`
    // uses), NOT the old fixture plain type named `function`.
    // `super().__init__()` binds THIS value (see
    // `super_attr_impl`'s NativeSlot arm); the plain type had
    // `make_new: None`, so calling it raised
    // `TypeError("can't create instance")` — the frozen `deque`
    // (`super().__init__(iterable)`) never constructed. One
    // wrapper per registry, as in `rom.rs` (its identity is what
    // `new_instance` primes into `subobj[0]` and what
    // `instance_make_new`'s tail compares against). It lives
    // only in `BuiltinTypes` (no builtin dict references it),
    // so it is rooted here like `rom.rs` roots its copy.
    let super_dict = empty_dict(ctx);
    let super_ = ctx.alloc(None, Obj::Type(super_type_data(super_dict)));
    let empty_bytes = ctx.alloc(
        Some(bytes),
        Obj::Bytes(BytesData { bytes: Vec::new() }),
    );
    let wrapper_dict = empty_dict(ctx);
    let Value::Heap(native_init_wrapper) = native_base_init_wrapper_obj(ctx, wrapper_dict)
    else {
        unreachable!("native_base_init_wrapper_obj always returns a heap value");
    };
    ctx.roots.push(Value::Heap(native_init_wrapper));

    // D2: the FILE stream types (one shared locals dict).
    // Allocated with a `None` header here, like the other
    // types; the fix-up loop below points them at `type_`.
    let (fileio, textio) = build_fileio_types(ctx, fun_builtin_var);

    // D2: the shared attrtuple type (ONE ROM type for
    // `os.stat` results and `sys.implementation` — probed:
    // `type(os.stat('.')) is type(sys.implementation)`).
    // Its dict is empty (the tuple slots operate on the
    // payload; the C11 shape, kept).
    let attrtuple_dict = ctx.alloc(
        None,
        Obj::Dict(DictData { map: MapData::new() }),
    );
    let attrtuple = ctx.alloc(None, Obj::Type(attrtuple_type_data(attrtuple_dict)));

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
        filter, zip, enumerate, reversed, fun_bc, gen_wrap, gen_instance, closure, bound_meth, module,
        property, ellipsis, type_, super_, staticmethod, classmethod,
        fileio, textio, attrtuple, code, dict_view, dict_view_it, iterator, range_it,
    ] {
        ctx.arena.set_header_type(t, Some(type_));
    }
    for t in exc.all() {
        ctx.arena.set_header_type(t, Some(type_));
    }

    // ROM type dicts are FIXED (E2): the Zig builds them from
    // static tables, so `int.__dict__ is int.__dict__` (probed)
    // and `int.foo = 1` raises `AttributeError` (probed — the
    // fixed map makes `type_store_attr` decline). User classes
    // created at runtime get fresh unfixed dicts (`Foo.__dict__
    // is Foo.__dict__` is False, probed), so only the ROM types
    // built here are marked.
    for t in [
        fun_builtin_var, object, int, bool_, cell, float, complex, none_type, str_, bytes,
        tuple, list, dict, set, frozenset, range, slice, array, bytearray, memoryview, map,
        filter, zip, enumerate, reversed, fun_bc, gen_wrap, gen_instance, closure, bound_meth, module,
        property, ellipsis, type_, super_, staticmethod, classmethod,
        fileio, textio, attrtuple, code, dict_view, dict_view_it, iterator, range_it,
    ] {
        let dict_ref = match ctx.arena.get(t) {
            Obj::Type(d) => d.dict,
            _ => continue,
        };
        if let Obj::Dict(d) = ctx.arena.get_mut(dict_ref) {
            d.map.is_fixed = true;
        }
    }
    for t in exc.all() {
        let dict_ref = match ctx.arena.get(t) {
            Obj::Type(d) => d.dict,
            _ => continue,
        };
        if let Obj::Dict(d) = ctx.arena.get_mut(dict_ref) {
            d.map.is_fixed = true;
        }
    }

    // MRO patch (D1): every builtin type built here carries an
    // empty `mro` in its `*_type_data` literal, so
    // `is_subclass_fast` fell through to `mro.contains(object)`
    // and `isinstance(1, object)` came out `False`. The Zig's
    // `mroOf` appends `object` for a parentless type — the ROM
    // registry (`rom.rs`) already patches its types to `[t,
    // object]`; patch the VM-built ones the same way (leaving
    // non-empty MROs — the exception types — alone; `object`
    // itself gets `[object]`).
    for t in [
        fun_builtin_var, int, bool_, cell, float, complex, none_type, str_, bytes, tuple, list,
        dict, set, frozenset, range, slice, array, bytearray, memoryview, map, filter, zip,
        enumerate, reversed, fun_bc, gen_wrap, gen_instance, closure, bound_meth, module, property,
        ellipsis, type_, super_, staticmethod, classmethod, code,
        dict_view, dict_view_it, iterator, range_it,
    ] {
        if let Obj::Type(d) = ctx.arena.get_mut(t) {
            if d.mro.is_empty() {
                d.mro = vec![t, object];
            }
        }
    }
    if let Obj::Type(d) = ctx.arena.get_mut(object) {
        if d.mro.is_empty() {
            d.mro = vec![object];
        }
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
        fileio,
        textio,
        attrtuple,
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
        dict_view,
        dict_view_it,
        iterator,
        range_it,
        socket: None,
        socketfile: None,
        deque: None,
        ordereddict: None,
        namedtuple_fn: None,
        re_type: None,
        match_type: None,
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
        reversed,
        code,
    };
    (types, vm_types, exc, empty_tuple, Value::Heap(ellipsis_singleton))
}
