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
use crate::objects::value::{DictData, FunBuiltinData, InstanceData, MapData, ModuleData, Obj, Value};
use crate::runtime::builtinevex::{compile_builtin_fn, eval_builtin_fn, exec_builtin_fn};
use crate::runtime::builtinimport::import_builtin_fn;
use crate::runtime::vm::{Vm, VmState};
use crate::runtime::vm_builtins::{
    build_class_builtin_fn, next_builtin_fn, print_builtin, print_builtin_nokw,
    repl_print_builtin,
};
use crate::runtime::builtin_fns::{
    abs_builtin, bin_builtin, callable_builtin, chr_builtin, divmod_builtin, format_builtin,
    hash_builtin, hex_builtin, id_builtin, input_builtin, intercepted_stub_fn, len_builtin,
    oct_builtin, open_builtin, open_builtin_kw, ord_builtin, pow_builtin, pow_builtin_kw,
    repr_builtin, round_builtin, round_builtin_kw,
};

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
    pub repr_builtin: Value,
    pub len_builtin: Value,
    pub hash_builtin: Value,
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
    let hash_val = mk_var(ctx, "hash", 1, 1, hash_builtin, pairs);
    let _ = mk_var(ctx, "hex", 1, 1, hex_builtin, pairs);
    let _ = mk_var(ctx, "id", 1, 1, id_builtin, pairs);
    let len_val = mk_var(ctx, "len", 1, 1, len_builtin, pairs);
    let _ = mk_var(ctx, "oct", 1, 1, oct_builtin, pairs);
    let _ = mk_var(ctx, "ord", 1, 1, ord_builtin, pairs);
    let repr_val = mk_var(ctx, "repr", 1, 1, repr_builtin, pairs);
    let _ = mk_var(ctx, "input", 0, 1, input_builtin, pairs);
    // `__repl_print__` (E1): the REPL echo builtin
    // (`modbuiltins.zig:355`, sig (1, 1)) — a plain
    // `BuiltinFn`, no interception; the compiler's REPL branch
    // resolves it on demand.
    let _ = mk_var(ctx, "__repl_print__", 1, 1, repl_print_builtin, pairs);
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
        repr_builtin: repr_val,
        len_builtin: len_val,
        hash_builtin: hash_val,
        io_module,
    }
}

/// `Vm::build_builtins` — moved here from `vm.rs` at the
/// D2 merge integration (`vm.rs` was 864 lines). Binds the
/// type / exception / `print` / `next` / `__build_class__` /
/// `eval` / `exec` / `compile` / `__import__` / `help` names
/// and assembles the builtins dict; the D1 binds come from
/// `bind_d1_builtins` above.
impl Vm {
    pub(crate) fn build_builtins(&mut self) {
        // (Capture the VmTypes refs before the split — `split`
        // borrows `self`, so the fields are unreadable after it.
        // `VmTypes` is `Copy`, so this is a plain copy.)
        let vmt = self.state.vm_types;
        let (mut ctx, state) = self.split();
        let mut pairs: Vec<(Value, Value)> = Vec::new();
        let bind = |name: &str, v: Value, pairs: &mut Vec<(Value, Value)>| {
            let id = static_qstr_id(name.as_bytes())
                .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
            pairs.push((Value::Qstr(id), v));
        };
        let t = ctx.types;
        bind("int", Value::Heap(t.int), &mut pairs);
        bind("str", Value::Heap(t.str_), &mut pairs);
        bind("bytes", Value::Heap(t.bytes), &mut pairs);
        bind("bool", Value::Heap(t.bool_), &mut pairs);
        bind("float", Value::Heap(t.float), &mut pairs);
        bind("complex", Value::Heap(t.complex), &mut pairs);
        bind("list", Value::Heap(t.list), &mut pairs);
        bind("tuple", Value::Heap(t.tuple), &mut pairs);
        bind("dict", Value::Heap(t.dict), &mut pairs);
        bind("set", Value::Heap(t.set), &mut pairs);
        bind("frozenset", Value::Heap(t.frozenset), &mut pairs);
        bind("range", Value::Heap(t.range), &mut pairs);
        bind("object", Value::Heap(vmt.object), &mut pairs);
        bind("type", Value::Heap(t.type_), &mut pairs);
        bind("property", Value::Heap(vmt.property), &mut pairs);
        bind("bytearray", Value::Heap(vmt.bytearray), &mut pairs);
        bind("memoryview", Value::Heap(vmt.memoryview), &mut pairs);
        bind("slice", Value::Heap(vmt.slice), &mut pairs);
        bind("map", Value::Heap(vmt.map), &mut pairs);
        bind("filter", Value::Heap(vmt.filter), &mut pairs);
        bind("zip", Value::Heap(vmt.zip), &mut pairs);
        bind("enumerate", Value::Heap(vmt.enumerate), &mut pairs);
        bind("reversed", Value::Heap(vmt.reversed), &mut pairs);
        bind("Ellipsis", state.ellipsis, &mut pairs);
        bind("super", Value::Heap(t.super_), &mut pairs);
        bind("staticmethod", Value::Heap(t.staticmethod), &mut pairs);
        bind("classmethod", Value::Heap(t.classmethod), &mut pairs);
        for (name, r) in [
            ("BaseException", state.exc.base_exception),
            ("SystemExit", state.exc.system_exit),
            ("KeyboardInterrupt", state.exc.keyboard_interrupt),
            ("GeneratorExit", state.exc.generator_exit),
            ("Exception", state.exc.exception),
            ("StopAsyncIteration", state.exc.stop_async_iteration),
            ("StopIteration", state.exc.stop_iteration),
            ("ArithmeticError", state.exc.arithmetic_error),
            ("OverflowError", state.exc.overflow_error),
            ("ZeroDivisionError", state.exc.zero_division_error),
            ("AssertionError", state.exc.assertion_error),
            ("AttributeError", state.exc.attribute_error),
            ("EOFError", state.exc.eof_error),
            ("ImportError", state.exc.import_error),
            ("ModuleNotFoundError", state.exc.module_not_found_error),
            ("LookupError", state.exc.lookup_error),
            ("IndexError", state.exc.index_error),
            ("KeyError", state.exc.key_error),
            ("MemoryError", state.exc.memory_error),
            ("NameError", state.exc.name_error),
            ("OSError", state.exc.os_error),
            ("RuntimeError", state.exc.runtime_error),
            ("NotImplementedError", state.exc.not_implemented_error),
            ("SyntaxError", state.exc.syntax_error),
            ("IndentationError", state.exc.indentation_error),
            ("TypeError", state.exc.type_error),
            ("ValueError", state.exc.value_error),
            ("UnicodeError", state.exc.unicode_error),
        ] {
            bind(name, Value::Heap(r), &mut pairs);
        }
        // `print` (KwFn: `sep`/`end`/`file`/`flush`).
        let print_id = static_qstr_id(b"print").expect("print is a static qstr");
        let print_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: print_id,
                n_args: fun_n_args(0, 255),
                func: print_builtin_nokw,
                kw_func: Some(print_builtin),
            }),
        );
        let print_val = Value::Heap(print_fun);
        bind("print", print_val, &mut pairs);
        // `next` (1–2 args; intercepted by identity in
        // `call_function` — its `func` is a stub).
        let next_id = static_qstr_id(b"next").expect("next is a static qstr");
        let next_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: next_id,
                n_args: fun_n_args(1, 2),
                func: next_builtin_fn,
                kw_func: None,
            }),
        );
        let next_val = Value::Heap(next_fun);
        bind("next", next_val, &mut pairs);
        // `__build_class__` (intercepted by identity in
        // `call_function` — its `func` is a stub).
        let bc_id = static_qstr_id(b"__build_class__").expect("__build_class__ is a static qstr");
        let bc_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: bc_id,
                n_args: fun_n_args(2, 255),
                func: build_class_builtin_fn,
                kw_func: None,
            }),
        );
        let bc_val = Value::Heap(bc_fun);
        bind("__build_class__", bc_val, &mut pairs);
        // `eval` / `exec` (1–3 args) and `compile` (3–6 args):
        // `MpObjFunBuiltinVar` objects (`builtinevex.zig:207-225`,
        // sigs `(1, 3, false)` / `(1, 3, false)` / `(3, 6,
        // false)`), intercepted by identity in `call_function_ctx`
        // — their `func`s are stubs, as `next`'s is.
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
        let eval_val = mk_var(&mut ctx, "eval", 1, 3, eval_builtin_fn, &mut pairs);
        let exec_val = mk_var(&mut ctx, "exec", 1, 3, exec_builtin_fn, &mut pairs);
        let compile_val = mk_var(&mut ctx, "compile", 3, 6, compile_builtin_fn, &mut pairs);
        // `__import__` (1–5 args: `builtinimport.zig:179-184`,
        // `MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(..., 1, 5, ...)`),
        // intercepted by identity in `call_function_ctx` — its
        // `func` is a stub, as `eval`'s is. Added by C15b.
        let import_val =
            mk_var(&mut ctx, "__import__", 1, 5, import_builtin_fn, &mut pairs);
        // `help` (0–1 args: `MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN
        // (mp_builtin_help_obj, 0, 1, ...)`, `builtinhelp.c:189`),
        // intercepted by identity in `call_function_ctx` — its
        // `func` is a stub, as `eval`'s is. Added by C18b.
        let help_val = mk_var(
            &mut ctx,
            "help",
            0,
            1,
            crate::runtime::builtinhelp::help_builtin_fn,
            &mut pairs,
        );
        // The D1 binds (`vm_builtins_bind.rs`): plain builtins,
        // VM-intercepted builtins, `NotImplemented`,
        // `isinstance` / `issubclass`, `__name__`, the `io`
        // module registration.
        let d1 = crate::runtime::vm_builtins_bind::bind_d1_builtins(&mut ctx, state, &mut pairs);
        // The embedding host-call builtin (F1, ADR 022's
        // `hostCall`): ONE shared `FunBuiltin` that every
        // `ppy_define_function` bound method wraps (its `self`
        // is the host's index in the interpreter's `hosts`
        // list). Created here (not on first
        // `ppy_define_function`) so its GcRef is stable from
        // birth and `call_function_ctx` can intercept it by
        // identity — a `FunBuiltin` slot cannot see the host
        // table. Its `func` is the shared unreachable stub, as
        // `next`'s is; its name is a dynamic qstr (never shown
        // to Python — a bound method's `__name__` raises
        // `AttributeError` for a `FunBuiltin`).
        let host_call_id = ctx
            .pool
            .from_strn(b"__ppy_host_call__")
            .expect("__ppy_host_call__ interns");
        let host_call_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: u32::from(host_call_id),
                n_args: fun_n_args(1, 255),
                func: intercepted_stub_fn,
                kw_func: None,
            }),
        );
        let host_call_val = Value::Heap(host_call_fun);

        let builtins = ctx.alloc(
            None,
            Obj::Dict(DictData {
                map: MapData::from_pairs(pairs),
            }),
        );
        state.builtins = builtins;
        // Fix the builtins dict (the vendor's ROM table,
        // `is_fixed = 1` — the sys/os/errno precedent in
        // modsys.rs/modos.rs/moderrno.rs): a store into the
        // builtins module must land in the override dict, not
        // mutate the ROM table (`module_store_attr`'s fixed
        // branch). Then expose the builtins as an importable
        // module (E2i: `import builtins` — the module's dict
        // IS the builtins dict, so `builtins.abs` reads the
        // original while `abs` resolves through the override
        // dict, exactly as the reference binary does).
        if let Obj::Dict(d) = ctx.arena.get_mut(builtins) {
            d.map.is_fixed = true;
        }
        let builtins_id =
            static_qstr_id(b"builtins").expect("builtins is a static qstr");
        let builtins_module = ctx.alloc(
            Some(vmt.module),
            Obj::Module(ModuleData {
                name: builtins_id,
                dict: builtins,
                delegation: None,
            }),
        );
        let builtins_module_val = Value::Heap(builtins_module);
        state
            .builtin_modules
            .push((builtins_id, builtins_module_val));
        // No module loaded yet: globals default to builtins (a
        // harmless default — `load_module` sets the real globals).
        state.globals = builtins;
        state.next_builtin = next_val;
        state.build_class_builtin = bc_val;
        state.eval_builtin = eval_val;
        state.exec_builtin = exec_val;
        state.compile_builtin = compile_val;
        state.import_builtin = import_val;
        state.help_builtin = help_val;
        state.globals_builtin = d1.globals_builtin;
        state.locals_builtin = d1.locals_builtin;
        state.dir_builtin = d1.dir_builtin;
        state.sorted_builtin = d1.sorted_builtin;
        // E2b: the `str.join` / `bytes.join` method builtins
        // (two different `FunBuiltin` objects, one per type
        // dict — each dict is built from its own table, so the
        // `join` entry is allocated twice). Looked up by name
        // in the type dicts; `None` (impossible in practice —
        // both tables carry `join`) leaves the handle unset
        // and `join` keeps its old ctx-only behavior.
        let join_q = static_qstr_id(b"join").expect("join is a static qstr");
        for (type_, slot) in [
            (ctx.types.str_, &mut state.str_join_builtin),
            (ctx.types.bytes, &mut state.bytes_join_builtin),
        ] {
            let dict = match ctx.arena.get(type_) {
                Obj::Type(d) => d.dict,
                _ => continue,
            };
            if let Ok(v) = crate::objects::containers::objdict::dict_get_value(
                &mut ctx,
                Value::Heap(dict),
                Value::Qstr(join_q),
            ) {
                *slot = v;
            }
        }
        // E2c: the `list.sort` method builtin (one `FunBuiltin`
        // object in the list type dict). Looked up by name;
        // a miss (impossible in practice — the table carries
        // `sort`) leaves the handle unset and `sort` keeps its
        // old ctx-only behavior (a non-`None` key raises).
        let sort_q = static_qstr_id(b"sort").expect("sort is a static qstr");
        if let Obj::Type(d) = ctx.arena.get(ctx.types.list) {
            let list_dict = d.dict;
            if let Ok(v) = crate::objects::containers::objdict::dict_get_value(
                &mut ctx,
                Value::Heap(list_dict),
                Value::Qstr(sort_q),
            ) {
                state.list_sort_builtin = v;
            }
        }
        state.hash_builtin = d1.hash_builtin;
        // E2i: the `array.extend` / `bytearray.extend` method
        // builtins (two different `FunBuiltin` objects — the
        // array dict and the bytearray dict are built from
        // separate tables, same as `str.join` / `bytes.join`
        // above). Looked up by name; a miss leaves the handle
        // unset and `extend` keeps its old ctx-only behavior
        // (a generator arg raises "object not iterable").
        let extend_q = static_qstr_id(b"extend").expect("extend is a static qstr");
        // The array type lives in `vm_types` (built before
        // this bind runs); bytearray's dict is built the same
        // way — look both up through the builtin type refs.
        for (type_, slot) in [
            (ctx.types.array, &mut state.array_extend_builtin),
            (ctx.types.bytearray, &mut state.bytearray_extend_builtin),
        ] {
            let dict = match ctx.arena.get(type_) {
                Obj::Type(d) => d.dict,
                _ => continue,
            };
            if let Ok(v) = crate::objects::containers::objdict::dict_get_value(
                &mut ctx,
                Value::Heap(dict),
                Value::Qstr(extend_q),
            ) {
                *slot = v;
            }
        }
        state.min_builtin = d1.min_builtin;
        state.max_builtin = d1.max_builtin;
        state.getattr_builtin = d1.getattr_builtin;
        state.setattr_builtin = d1.setattr_builtin;
        state.hasattr_builtin = d1.hasattr_builtin;
        state.delattr_builtin = d1.delattr_builtin;
        state.iter_builtin = d1.iter_builtin;
        state.all_builtin = d1.all_builtin;
        state.any_builtin = d1.any_builtin;
        state.sum_builtin = d1.sum_builtin;
        state.repr_builtin = d1.repr_builtin;
        state.len_builtin = d1.len_builtin;
        state.print_builtin = print_val;
        state.host_call_builtin = host_call_val;
        state.not_implemented = d1.not_implemented;
        // Pin everything live: the types, the singletons, the
        // builtins dict and the builtin values.
        let mut pins: Vec<Value> = Vec::new();
        for r in [
            t.int,
            t.str_,
            t.bytes,
            t.bool_,
            t.none_type,
            t.float,
            t.complex,
            t.tuple,
            t.list,
            t.set,
            t.frozenset,
            t.range,
            t.dict,
            t.type_,
        ] {
            pins.push(Value::Heap(r));
        }
        for r in [
            vmt.fun_bc,
            vmt.gen_wrap,
            vmt.gen_instance,
            vmt.closure,
            vmt.bound_meth,
            vmt.module,
            vmt.object,
            vmt.ellipsis,
            vmt.slice,
            vmt.fun_builtin_var,
            vmt.array,
            vmt.bytearray,
            vmt.memoryview,
            vmt.cell,
            vmt.property,
        ] {
            pins.push(Value::Heap(r));
        }
        for r in state.exc.all() {
            pins.push(Value::Heap(r));
        }
        pins.push(state.empty_tuple);
        pins.push(state.ellipsis);
        pins.push(Value::Heap(builtins));
        pins.push(builtins_module_val);
        pins.push(print_val);
        pins.push(host_call_val);
        pins.push(next_val);
        pins.push(bc_val);
        pins.push(eval_val);
        pins.push(exec_val);
        pins.push(compile_val);
        pins.push(import_val);
        pins.push(d1.globals_builtin);
        pins.push(d1.locals_builtin);
        pins.push(d1.dir_builtin);
        pins.push(d1.sorted_builtin);
        pins.push(d1.min_builtin);
        pins.push(d1.max_builtin);
        pins.push(d1.getattr_builtin);
        pins.push(d1.setattr_builtin);
        pins.push(d1.hasattr_builtin);
        pins.push(d1.delattr_builtin);
        pins.push(d1.iter_builtin);
        pins.push(d1.all_builtin);
        pins.push(d1.any_builtin);
        pins.push(d1.sum_builtin);
        pins.push(d1.not_implemented);
        pins.push(d1.io_module);
        drop(ctx);
        for v in pins {
            self.pin(v);
        }
    }
}
