//! Bind the D1 builtin functions into the builtins dict
//! (`modbuiltins.zig` — the table at `blk-built:1034-1290`).
//!
//! Split out of `vm.rs` (`build_builtins`) at D1: the plain
//! `BuiltinFn` / `BuiltinKwFn` binds, the VM-intercepted binds,
//! `NotImplemented`, `isinstance` / `issubclass`, `__name__`
//! and the `io` module registration. `vm.rs` keeps the type /
//! exception / `print` / `next` / `__build_class__` / `eval` /
//! `exec` / `compile` / `__import__` / `help` binds and the
//! builtins-dict assembly; this module returns the values it
//! created so `vm.rs` can store them in `VmState` and pin them.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_new_typed;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::objtype::subclass::{isinstance_fun_obj, issubclass_fun_obj};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, InstanceData, MapData, Obj, Value};
use crate::runtime::builtin_fns::{
    abs_builtin, bin_builtin, callable_builtin, chr_builtin, divmod_builtin, format_builtin,
    hash_builtin, hex_builtin, id_builtin, input_builtin, intercepted_stub_fn, len_builtin,
    oct_builtin, open_builtin, open_builtin_kw, ord_builtin, pow_builtin, pow_builtin_kw,
    repr_builtin, round_builtin, round_builtin_kw,
};
use crate::runtime::vm::VmState;

/// The values `bind_d1_builtins` created, for `VmState` + pins.
pub(crate) struct D1Bindings {
    pub not_implemented: Value,
    pub globals_builtin: Value,
    pub locals_builtin: Value,
    pub dir_builtin: Value,
    pub sorted_builtin: Value,
    pub min_builtin: Value,
    pub max_builtin: Value,
    pub getattr_builtin: Value,
    pub setattr_builtin: Value,
    pub hasattr_builtin: Value,
    pub delattr_builtin: Value,
    pub iter_builtin: Value,
    pub all_builtin: Value,
    pub any_builtin: Value,
    pub sum_builtin: Value,
    pub io_module: Value,
}

/// Bind every D1 name into `pairs` (the builtins-dict entries)
/// and return the created values. `ctx`/`state` are the split
/// VM parts from `Vm::build_builtins`.
pub(crate) fn bind_d1_builtins(
    ctx: &mut CallCtx,
    state: &mut VmState,
    pairs: &mut Vec<(Value, Value)>,
) -> D1Bindings {
    let vmt = state.vm_types;
    let t = ctx.types;
    let bind = |name: &str, v: Value, pairs: &mut Vec<(Value, Value)>| {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
        pairs.push((Value::Qstr(id), v));
    };
    // `mk_var` / `mk_kw` mirror `build_builtins`' locals (the
    // `eval`/`exec`/`compile` ones stay there): allocate a
    // `FunBuiltin` and bind it by name.
    let mk_var = |ctx: &mut CallCtx,
                  name: &str,
                  min: u16,
                  max: u16,
                  func: crate::objects::slots::BuiltinFn,
                  pairs: &mut Vec<(Value, Value)>|
     -> Value {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
        let f = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: None,
            }),
        );
        let v = Value::Heap(f);
        bind(name, v, pairs);
        v
    };
    let mk_kw = |ctx: &mut CallCtx,
                 name: &str,
                 min: u16,
                 max: u16,
                 func: crate::objects::slots::BuiltinFn,
                 kw_func: crate::objects::slots::BuiltinKwFn,
                 pairs: &mut Vec<(Value, Value)>|
     -> Value {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
        let f = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: Some(kw_func),
            }),
        );
        let v = Value::Heap(f);
        bind(name, v, pairs);
        v
    };
    // `NotImplemented` (D1): an `Obj::Instance` with the shared
    // ellipsis type as header (see the `not_implemented` field
    // docs) — bound here, like `Ellipsis`.
    let not_implemented = Value::Heap(ctx.alloc(
        Some(vmt.ellipsis),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj: Vec::new(),
        }),
    ));
    bind("NotImplemented", not_implemented, pairs);
    // `isinstance` / `issubclass` (D1): the `Rom`-built
    // `FunBuiltin` objects (`objtype.zig:577`) — the same
    // constructors `Rom` uses, each over its own fresh empty
    // dict, bound here by name only (the brief's grounding:
    // "D1 binds the names only").
    let isinstance_val = {
        let d = match dict_new_typed(ctx, t.dict, 0) {
            Value::Heap(r) => r,
            _ => unreachable!("dict_new_typed always returns a heap dict"),
        };
        ctx.roots.push(Value::Heap(d));
        isinstance_fun_obj(ctx, d)
    };
    let issubclass_val = {
        let d = match dict_new_typed(ctx, t.dict, 0) {
            Value::Heap(r) => r,
            _ => unreachable!("dict_new_typed always returns a heap dict"),
        };
        ctx.roots.push(Value::Heap(d));
        issubclass_fun_obj(ctx, d)
    };
    bind("isinstance", isinstance_val, pairs);
    bind("issubclass", issubclass_val, pairs);
    bind(
        "__name__",
        Value::Qstr(static_qstr_id(b"builtins").expect("builtins is a static qstr")),
        pairs,
    );
    // Plain builtins (D1 — `builtin_fns.rs`): fixed-arity
    // `BuiltinFn`s, plus the `BuiltinKwFn`s for `pow` / `round` /
    // `open` (the `kwVar` trio).
    let _ = mk_var(ctx, "abs", 1, 1, abs_builtin, pairs);
    let _ = mk_var(ctx, "bin", 1, 1, bin_builtin, pairs);
    let _ = mk_var(ctx, "callable", 1, 1, callable_builtin, pairs);
    let _ = mk_var(ctx, "chr", 1, 1, chr_builtin, pairs);
    let _ = mk_var(ctx, "divmod", 2, 2, divmod_builtin, pairs);
    let _ = mk_var(ctx, "format", 1, 2, format_builtin, pairs);
    let _ = mk_var(ctx, "hash", 1, 1, hash_builtin, pairs);
    let _ = mk_var(ctx, "hex", 1, 1, hex_builtin, pairs);
    let _ = mk_var(ctx, "id", 1, 1, id_builtin, pairs);
    let _ = mk_var(ctx, "len", 1, 1, len_builtin, pairs);
    let _ = mk_var(ctx, "oct", 1, 1, oct_builtin, pairs);
    let _ = mk_var(ctx, "ord", 1, 1, ord_builtin, pairs);
    let _ = mk_var(ctx, "repr", 1, 1, repr_builtin, pairs);
    let _ = mk_var(ctx, "input", 0, 1, input_builtin, pairs);
    let _ = mk_kw(ctx, "pow", 2, 3, pow_builtin, pow_builtin_kw, pairs);
    let _ = mk_kw(ctx, "round", 1, 2, round_builtin, round_builtin_kw, pairs);
    let open_val = mk_kw(ctx, "open", 1, 255, open_builtin, open_builtin_kw, pairs);
    // VM-intercepted builtins (D1 — `builtin_vm.rs`): their
    // `func`s are the shared stub; `call_function_ctx` routes
    // them by GcRef identity. The sigs mirror the Zig's table
    // (`modbuiltins.zig:890-927` + `kwFun`).
    let globals_val = mk_var(ctx, "globals", 0, 0, intercepted_stub_fn, pairs);
    let locals_val = mk_var(ctx, "locals", 0, 0, intercepted_stub_fn, pairs);
    let dir_val = mk_var(ctx, "dir", 0, 1, intercepted_stub_fn, pairs);
    let sorted_val = mk_var(ctx, "sorted", 1, 1, intercepted_stub_fn, pairs);
    let min_val = mk_var(ctx, "min", 1, 255, intercepted_stub_fn, pairs);
    let max_val = mk_var(ctx, "max", 1, 255, intercepted_stub_fn, pairs);
    let getattr_val = mk_var(ctx, "getattr", 2, 3, intercepted_stub_fn, pairs);
    let setattr_val = mk_var(ctx, "setattr", 3, 3, intercepted_stub_fn, pairs);
    let hasattr_val = mk_var(ctx, "hasattr", 2, 2, intercepted_stub_fn, pairs);
    let delattr_val = mk_var(ctx, "delattr", 2, 2, intercepted_stub_fn, pairs);
    let iter_val = mk_var(ctx, "iter", 1, 1, intercepted_stub_fn, pairs);
    let all_val = mk_var(ctx, "all", 1, 1, intercepted_stub_fn, pairs);
    let any_val = mk_var(ctx, "any", 1, 1, intercepted_stub_fn, pairs);
    let sum_val = mk_var(ctx, "sum", 1, 2, intercepted_stub_fn, pairs);
    // `io` (C18b's module — `import io`): registered in the
    // builtin-module table. The `open` member is D1's
    // `open_builtin` seam (a `NotImplementedError` stub until D2
    // fills the body); the registration itself is D1's.
    // `build_io_module` only fails on OOM (fresh dict stores) —
    // fatal at startup, as the other `expect`s above are for
    // programmer error.
    let io_module = build_io_module(ctx, vmt.fun_builtin_var, vmt.module, open_val)
        .unwrap_or_else(|_| panic!("out of memory building the io module"));
    let io_id = static_qstr_id(b"io").expect("io is a static qstr");
    state.builtin_modules.push((io_id, io_module));
    D1Bindings {
        not_implemented,
        globals_builtin: globals_val,
        locals_builtin: locals_val,
        dir_builtin: dir_val,
        sorted_builtin: sorted_val,
        min_builtin: min_val,
        max_builtin: max_val,
        getattr_builtin: getattr_val,
        setattr_builtin: setattr_val,
        hasattr_builtin: hasattr_val,
        delattr_builtin: delattr_val,
        iter_builtin: iter_val,
        all_builtin: all_val,
        any_builtin: any_val,
        sum_builtin: sum_val,
        io_module,
    }
}
