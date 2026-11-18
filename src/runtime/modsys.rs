//! The `sys` builtin module (`src/modules/modsys.zig`, 517 lines).
//!
//! Mapping (Zig → Rust):
//! | Zig | Rust |
//! | --- | --- |
//! | `mp_module_sys_globals_table` (18 entries, full) | `build_sys_module`: the entries in the same order — `__name__` is already stored by `new_module`, so the builder stores the other 17 |
//! | `VERSION_STR` | `VERSION_STR` (verbatim) |
//! | `version_info` (3,4,0) | `new_tuple([3, 4, 0])` |
//! | `implementation` attrtuple (name, version, _machine, _mpy) | `attrtuple_new` over a fresh attrtuple type (pinned by the D2 step — the header type is not otherwise reachable) |
//! | `MPY_FILE_HEADER_INT` = `'M' \| ('P' << 8) \| (6 << 16)` | `MPY_FILE_HEADER_INT` = 413773 |
//! | `maxsize` (`mp_sys_maxsize_obj`, MPZ 2^63-1) | `Value::int(..., i64::MAX)` (heap Int — out of the small-int window) |
//! | `mp_sys_argv_obj` | the `argv` list object (default `[]`; Phase E's entry fills it in place via `VmState.sys_argv_list`) |
//! | `mp_loaded_modules_dict` | the live `sys.modules` dict (`VmState.sys_modules_dict`), kept in sync with `state.modules` at the three write sites (import_search `create_and_register` / `remove_from_loaded`, vm_eval `load_module`). Builtin modules are NEVER registered there — probed: after `import os`, `sys.modules` is still `{}` |
//! | `sys_mutable[0]` (`sys.path`) | the `sys.path` list object (`VmState.sys_path_list`) + the `ModuleData.delegation` slot ([path, ps1, ps2] → [list, unset, unset]); `sys.path.append(x)` mutates the object, so the import search reads it (see `search_top_level`) |
//! | `sys_mutable[1..2]` (`ps1`/`ps2`) | unset delegation slots — `sys.ps1` → AttributeError (probed) |
//! | `py_sys_exit` (0–1 args) | `sys_exit_builtin`: bare `SystemExit` / `SystemExit(arg)` via `record_raise` |
//! | `py_sys_getrecursionlimit` / `py_sys_setrecursionlimit` | 1000 / accept-any-int no-op returning `None` (the Zig ignores the argument entirely) |
//! | `mp_obj_str_intern_checked` | `sys_intern_builtin`: str/bytes/qstr → the interned qstr (so `intern("x") is "x"`); anything else → `TypeError("can't convert to str implicitly")` (probed) |
//! | `py_sys_exc_info` | `sys_exc_info_builtin`: ALWAYS `(None, None, None)` — the original never assigns `cur_exception` (no `cur_exception =` anywhere in `src/` or `vendor/`; `vm.zig:187` says `SYS_EXC_INFO=0 → no write`); probed at top level, inside `except`, and nested |
//! | `py_sys_print_exception` (1–2 args) | `sys_print_exception_builtin` is a STUB — intercepted by GcRef identity in `call_function_ctx` (it needs the VM's `sys_stdout` for its default target); the real body is `print_exception_ctx` here |
//!
//! The sys dict is FIXED (`is_fixed = true`, as the C's ROM
//! dict): `sys.foo = 42` and `del sys.nonexistent` raise
//! AttributeError (probed); only the delegation keys
//! (`path`/`ps1`/`ps2`) accept stores, and deletes are never
//! delegated (`del sys.path` → AttributeError, probed).
//! `'path' in sys.__dict__` is False (probed) — `path` lives
//! only in the delegation table, never in the dict.
//!
//! Divergences: (1) `sys.path = <new list>` rebinds the
//! delegation slot but the import search keeps reading the
//! OLD list object (`VmState.sys_path_list` is not re-pointed
//! — the store path has no `VmState` access; see the result
//! file). Mutating `sys.path` (`append`/`extend`) works
//! correctly — same object. No corpus/golden file rebinds
//! `sys.path`. (2) `sys.print_exception(e, <non-stream>)`
//! raises the vendor-C `OSError("stream operation not
//! supported")` via `get_stream_raise`; the reference ABORTS
//! (rc=134, the Zig's `mp_get_stream_raise` @panic).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objattrtuple::attrtuple_new;
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn};
use crate::objects::strings::objstr_a::{new_str, str_data_pool};
use crate::objects::value::{DictData, FunBuiltinData, GcRef, MapData, ModuleDelegation, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::support::stream::{get_stream_raise, stream_write_adaptor, MP_STREAM_OP_WRITE};

/// `VERSION_STR` (`modsys.zig:79`): the banner baked from
/// `mpversion.h` — verbatim.
pub const VERSION_STR: &str =
    "3.4.0; MicroPython (with v2.0 preview) v1.28.0-preview.80.gb14d129a16 on 2026-06-04";

/// `MACHINE_STR` (`modsys.zig:136`): `MICROPY_BANNER_MACHINE`,
/// hard-coded for the host build.
const MACHINE_STR: &str = "petitpy [GCC 4.2.1 (Apple Inc. build 5658) (LLVM build)] version";

/// `MPY_FILE_HEADER_INT` (`modsys.zig:145`, vendor
/// `persistentcode.h`): `'M' | ('P' << 8) | (6 << 16)`.
const MPY_FILE_HEADER_INT: i64 = ('M' as i64) | (('P' as i64) << 8) | (6i64 << 16);

/// `sys.platform` (`modsys.zig:149-154`): `macos => "darwin"`,
/// else the OS tag name. Rust's `std::env::consts::OS` is
/// `"linux"`/`"macos"`/`"windows"` — the same names the Zig
/// switch produces on those targets.
fn platform_str() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

// ── builtin fns ───────────────────────────────────────────────────────

/// `py_sys_exit` (`modsys.zig:197-208`, sig (0,1)): 0 args →
/// bare `SystemExit`; 1 arg → `SystemExit(arg)`.
pub fn sys_exit_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        ctx.record_raise("SystemExit", None);
    } else {
        ctx.record_raise_value("SystemExit", args[0]);
    }
    Err(PyError::PyRaise)
}

/// `py_sys_getrecursionlimit` (`modsys.zig:214`, sig (0,0)):
/// fixed 1000 (the CPython default; no limit is enforced).
pub fn sys_getrecursionlimit_builtin(
    _ctx: &mut CallCtx,
    _args: &[Value],
) -> PyResult<Value> {
    Ok(Value::SmallInt(1000))
}

/// `py_sys_setrecursionlimit` (`modsys.zig:217`, sig (1,1)):
/// accepts any int, no-op (the Zig ignores the argument
/// entirely — even a non-int returns `None`).
pub fn sys_setrecursionlimit_builtin(
    _ctx: &mut CallCtx,
    _args: &[Value],
) -> PyResult<Value> {
    Ok(Value::None)
}

/// `mp_obj_str_intern_checked` (`modsys.zig` `FullOnly`,
/// sig (1,1)): intern a str/bytes/qstr into the runtime qstr
/// pool and return the qstr value — so `sys.intern("x") is
/// "x"` (probed). Anything else → `TypeError("can't convert
/// to str implicitly")` (probed: `intern(0)`, `intern(1.5)`).
pub fn sys_intern_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let bytes = match str_data_pool(ctx.arena, ctx.pool, args[0]) {
        Some(b) => b,
        None => {
            ctx.record_raise("TypeError", Some("can't convert to str implicitly"));
            return Err(PyError::PyRaise);
        }
    };
    let id = ctx.pool.from_strn(&bytes)?;
    Ok(Value::Qstr(u32::from(id)))
}

/// `py_sys_exc_info` (`modsys.zig` `FullOnly`, sig (0,0)):
/// ALWAYS `(None, None, None)`. The original never assigns
/// `cur_exception` — there is no `cur_exception =` anywhere
/// in `src/` or `vendor/`, and `vm.zig:187` documents
/// `SYS_EXC_INFO=0 → no MP_STATE_VM(cur_exception) write`.
/// Probed 2026-11-18: top level, inside `except`, nested.
pub fn sys_exc_info_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let none = Value::None;
    Ok(Value::Heap(new_tuple(
        ctx,
        ctx.types.tuple,
        &[none, none, none],
    )))
}

/// `mp_sys_print_exception_obj` (sig (1,2)) — STUB: the real
/// body is `print_exception_ctx` below, reached by GcRef
/// identity interception in `call_function_ctx` (the default
/// target is the VM's `sys_stdout`, which a plain `BuiltinFn`
/// cannot reach). Called directly only when the interception
/// is bypassed — a bare raise, like the other stubs.
pub fn sys_print_exception_builtin(
    ctx: &mut CallCtx,
    _args: &[Value],
) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// `py_sys_print_exception` (`modsys.zig:228-245`, sig (1,2)):
/// print the exception (cause chain + traceback + the
/// exception line, via `obj_print_exception`) to `sys.stdout`
/// by default, or to `args[1]` when given. The target stream
/// is resolved through `get_stream_raise` (WRITE) — a
/// non-stream second argument raises the vendor-C OSError
/// (the reference ABORTS there; see the module doc).
pub fn print_exception_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    // Sig (1, 2, no kwargs) — the interception bypasses
    // `call_fun_builtin`'s arity check, so it runs here
    // (the terse TypeError, as for every other builtin).
    if n_kw != 0 || n_args < 1 || n_args > 2 {
        ctx.record_raise("TypeError", Some("argument num/types mismatch"));
        return Err(PyError::PyRaise);
    }
    let exc = args[0];
    let target = args.get(1).copied().unwrap_or(state.sys_stdout);
    // `mp_get_stream_raise(args[1], MP_STREAM_OP_WRITE)`
    // validates the target FIRST (the Zig calls it before
    // building the print — a bad target fails before any
    // output is produced).
    let _ = get_stream_raise(ctx, target, MP_STREAM_OP_WRITE)?;
    let mut out = String::new();
    // `ctx.pool` is a `Copy` shared ref, so it feeds the
    // resolve closure without borrowing `ctx`
    // (`runtime_utils.rs: print_exc`).
    let pool = ctx.pool;
    let types = ctx.types;
    crate::objects::obj_ops::obj_print_exception(
        ctx,
        types,
        exc,
        &|id| pool.qstr_str(id as u16),
        &mut out,
    );
    stream_write_adaptor(ctx, target, out.as_bytes())?;
    Ok(Value::None)
}

// ── module builder ────────────────────────────────────────────────────

/// Allocate a `FunBuiltin` for a sys entry (the
/// `locals_dict_from_entries` construction, `vm_types.rs:98`).
fn mk_fun(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    name: &str,
    min: u16,
    max: u16,
    func: BuiltinFn,
    kw_func: Option<BuiltinKwFn>,
) -> Value {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("sys member name is not a static qstr: {}", name));
    Value::Heap(ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(min, max),
            func,
            kw_func,
        }),
    ))
}

/// Store one entry in the sys dict (panics only when the
/// name is not a static qstr — a build-time fact).
fn store(ctx: &mut CallCtx, dict: Value, name: &str, value: Value) {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("sys member name is not a static qstr: {}", name));
    dict_store(ctx, dict, Value::Qstr(id), value)
        .map_err(|e| ctx.record_typed(e))
        .expect("sys dict store cannot fail on a fresh dict");
}

/// Build the `sys` module object (`modsys.zig:418-488`,
/// full config — 18 entries, in the table's order).
/// `stdin`/`stdout`/`stderr` are the stdio objects from
/// `build_stdio_objects`; `sys_path` is the initial search
/// path (the `Vec<String>` the VM was built with).
///
/// Writes `state.sys_path_list` / `sys_modules_dict` /
/// `sys_argv_list` / `sys_stdin` / `sys_stdout` /
/// `sys_stderr` / `sys_print_exception_builtin`, and returns
/// the module. `sys.implementation` uses the SHARED
/// attrtuple type (`ctx.types.attrtuple`, built once in
/// `build_vm_types` — the same type `os.stat` results use;
/// probed: `type(os.stat('.')) is type(sys.implementation)`).
pub fn build_sys_module(
    ctx: &mut CallCtx,
    state: &mut VmState,
    stdin: Value,
    stdout: Value,
    stderr: Value,
) -> Value {
    let types: &BuiltinTypes = ctx.types;
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let sys_id = static_qstr_id(b"sys").expect("sys is a static qstr");
    let module = new_module(ctx, sys_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Value::Heap(m.dict),
            _ => unreachable!("new_module allocates a module"),
        },
        _ => unreachable!("new_module allocates a heap object"),
    };

    // `sys.path` list object (`sys_mutable[0]`): built from
    // the VM's `sys_path` — Python `sys.path.append(x)`
    // mutates THIS object, and the import search reads it.
    let path_items: Vec<Value> = state
        .sys_path
        .iter()
        .map(|p| new_str(ctx, types, p.as_bytes()))
        .collect();
    let path_list = new_list(ctx, types.list, &path_items);
    state.sys_path_list = Some(path_list);

    // `sys.argv` list object (default `[]`; Phase E fills it).
    let argv_list = new_list(ctx, types.list, &[]);
    state.sys_argv_list = Some(argv_list);

    // `sys.modules` live dict (file modules only — the sync
    // sites in import_search/vm_eval keep it in step with
    // `state.modules`; builtin modules are never inserted).
    let modules_dict = ctx.alloc(
        Some(types.dict),
        Obj::Dict(DictData { map: MapData::new() }),
    );
    state.sys_modules_dict = Some(modules_dict);

    // `sys.version_info` (3, 4, 0).
    let version_info = Value::Heap(new_tuple(
        ctx,
        types.tuple,
        &[Value::SmallInt(3), Value::SmallInt(4), Value::SmallInt(0)],
    ));
    // `sys.implementation.version` (1, 28, 0, "preview").
    let preview = static_qstr_id(b"preview").expect("preview is a static qstr");
    let impl_version = Value::Heap(new_tuple(
        ctx,
        types.tuple,
        &[
            Value::SmallInt(1),
            Value::SmallInt(28),
            Value::SmallInt(0),
            Value::Qstr(preview),
        ],
    ));
    // `sys.implementation` attrtuple (fields: name, version,
    // _machine, _mpy — `impl_fields`, modsys.zig:193).
    let impl_fields = [
        static_qstr_id(b"name").expect("name is a static qstr"),
        static_qstr_id(b"version").expect("version is a static qstr"),
        static_qstr_id(b"_machine").expect("_machine is a static qstr"),
        static_qstr_id(b"_mpy").expect("_mpy is a static qstr"),
    ];
    let micropython = static_qstr_id(b"micropython").expect("micropython is a static qstr");
    let machine = new_str(ctx, types, MACHINE_STR.as_bytes());
    let implementation = attrtuple_new(
        ctx,
        types.attrtuple,
        impl_fields.to_vec(),
        vec![
            Value::Qstr(micropython),
            impl_version,
            machine,
            Value::SmallInt(MPY_FILE_HEADER_INT),
        ],
    );

    // The `sys` fns (sigs from the `*_obj` definitions).
    let intern = mk_fun(ctx, fun_builtin_var, "intern", 1, 1, sys_intern_builtin, None);
    let exit = mk_fun(ctx, fun_builtin_var, "exit", 0, 1, sys_exit_builtin, None);
    let exc_info = mk_fun(
        ctx,
        fun_builtin_var,
        "exc_info",
        0,
        0,
        sys_exc_info_builtin,
        None,
    );
    let print_exception = mk_fun(
        ctx,
        fun_builtin_var,
        "print_exception",
        1,
        2,
        sys_print_exception_builtin,
        None,
    );
    let getrecursionlimit = mk_fun(
        ctx,
        fun_builtin_var,
        "getrecursionlimit",
        0,
        0,
        sys_getrecursionlimit_builtin,
        None,
    );
    let setrecursionlimit = mk_fun(
        ctx,
        fun_builtin_var,
        "setrecursionlimit",
        1,
        1,
        sys_setrecursionlimit_builtin,
        None,
    );
    state.sys_print_exception_builtin = print_exception;

    // The 17 entries after `__name__` (which `new_module`
    // already stored), in the table's order. (Values that need
    // `ctx` are bound first — `store(ctx, ...)` cannot take a
    // second `&mut ctx` argument in the same call.)
    let version = new_str(ctx, types, VERSION_STR.as_bytes());
    let platform = new_str(ctx, types, platform_str().as_bytes());
    let byteorder = new_str(ctx, types, b"little");
    let maxsize = Value::int(ctx.arena, Some(types.int), ctx.roots, i64::MAX);
    store(ctx, dict, "argv", Value::Heap(argv_list));
    store(ctx, dict, "version", version);
    store(ctx, dict, "version_info", version_info);
    store(ctx, dict, "implementation", implementation);
    store(ctx, dict, "platform", platform);
    store(ctx, dict, "byteorder", byteorder);
    store(ctx, dict, "maxsize", maxsize);
    store(ctx, dict, "intern", intern);
    store(ctx, dict, "exit", exit);
    store(ctx, dict, "stdin", stdin);
    store(ctx, dict, "stdout", stdout);
    store(ctx, dict, "stderr", stderr);
    store(ctx, dict, "modules", Value::Heap(modules_dict));
    store(ctx, dict, "exc_info", exc_info);
    store(ctx, dict, "print_exception", print_exception);
    store(ctx, dict, "getrecursionlimit", getrecursionlimit);
    store(ctx, dict, "setrecursionlimit", setrecursionlimit);

    // Fix the dict (the C's ROM dict, `is_fixed = 1`) and
    // install the delegation ([path, ps1, ps2] — `ps1`/`ps2`
    // unset, so they raise AttributeError). `path` is NOT in
    // the dict (`'path' in sys.__dict__` is False, probed).
    if let Value::Heap(r) = dict {
        if let Obj::Dict(d) = ctx.arena.get_mut(r) {
            d.map.is_fixed = true;
        }
    }
    let path_id = static_qstr_id(b"path").expect("path is a static qstr");
    let ps1_id = static_qstr_id(b"ps1").expect("ps1 is a static qstr");
    let ps2_id = static_qstr_id(b"ps2").expect("ps2 is a static qstr");
    if let Value::Heap(r) = module {
        if let Obj::Module(m) = ctx.arena.get_mut(r) {
            m.delegation = Some(ModuleDelegation {
                keys: vec![path_id, ps1_id, ps2_id],
                values: vec![Some(Value::Heap(path_list)), None, None],
            });
        }
    }
    state.sys_stdin = stdin;
    state.sys_stdout = stdout;
    state.sys_stderr = stderr;
    module
}
