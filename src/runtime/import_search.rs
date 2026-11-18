//! Import search (`runtime/import_search.zig`, 463 lines).
//!
//! ADR-003 L1+L2: `import X` (and `import X.Y.Z`) search past the
//! registered-modules table. Chain (in order, per dotted
//! component): frozen-tiny → `sys.path` or parent-package
//! `__path__` filesystem (`import_search.zig:1-22`).
//!
//! `builtinimport.rs` walks the dotted module name one component at
//! a time, calling `try_load_component` for each. On hit the module
//! is created + registered in `sys.modules` under the full dotted
//! name BEFORE its code runs (so circular imports see the partial
//! module, and a failed load removes it again —
//! `remove_from_loaded`); a directory component stores its path in
//! `__path__` and executes `__init__.py` (if any); a `.py`/`.mpy`
//! file is read, compiled (if `.py`) → loaded → executed under the
//! module's globals. On miss, `try_load_component` returns
//! `Ok(None)` and the caller raises `ImportError`.
//!
//! Rust notes:
//! * File I/O is `std::fs` (the Zig uses libc `fopen`/`stat`);
//!   `PATH_MAX_LEN = 1024` is kept (`import_search.zig:115`).
//! * `sys.path` is `VmState.sys_path` (`Vec<String>` — the `sys`
//!   module that exposes it is D's `modsys`); `sys.modules` is
//!   `VmState.modules` (C13) — no second table.
//! * Names are interned by the CALLER once (the full dotted
//!   name — `import_impl` / the opcode path); component names
//!   are never interned inside this module: lookups compare
//!   name BYTES (`loaded_lookup_by_bytes`) and the module's
//!   `__name__` IS the full qstr. Interning works on both paths
//!   (`from_strn` takes `&self` since C15b — the shared
//!   `CallCtx.pool` interns for real, so an imported module's
//!   new names resolve; the old read-only fallback failed them
//!   with `MemoryError`).
//! * Errors are `Result<_, Value>` (the VM convention — the
//!   exception object itself), not `PyResult`: module-body
//!   failures already produce exception objects via
//!   `call_function_ctx`.

use crate::compiler::mpy;
use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::value::{GcRef, Obj, StrData, Value};
use crate::pyerr::PyError;
use crate::runtime::compile_bridge::{compile_source_to_mpy, raise_from_code};
use crate::runtime::persistentcode::{raw_code_load_mem, LoadedCode};
use crate::runtime::runtime::{dunder, RtError};
use crate::runtime::runtime_method::load_method_maybe;
use crate::runtime::vm::{Vm, VmError, VmState};
use crate::runtime::vm_ops::exc_from_pyerror;

/// `PATH_MAX_LEN` (`import_search.zig:115`).
pub const PATH_MAX_LEN: usize = 1024;

/// What a per-component filesystem search found
/// (`import_search.zig:245`).
#[derive(Clone, Debug, PartialEq)]
pub enum FoundKind {
    NotFound,
    FilePy,
    FileMpy,
    DirPackage,
}

/// A search hit: the kind plus the resolved path
/// (`import_search.zig:247-254` — the Zig borrows a caller-owned
/// buffer; Rust owns the `String`).
#[derive(Clone, Debug)]
pub struct Found {
    pub kind: FoundKind,
    pub path: String,
}

/// Map an `RtError` to its exception object (for the import fns,
/// which return `Result<_, Value>`). `Raised(v)` is the object
/// itself; `PyRaise` drains the recorded raise; the named arms
/// rebuild from `exc_name()`/`message()`; `StopIteration` carries
/// its arg.
pub(crate) fn rt_error_to_value(
    ctx: &mut CallCtx,
    state: &VmState,
    e: RtError,
) -> Value {
    match e {
        RtError::Raised(v) => v,
        RtError::PyRaise => exc_from_pyerror(ctx, state, PyError::PyRaise),
        RtError::OutOfMemory => {
            new_exception_msg(ctx, state.exc.memory_error, "out of memory")
        }
        RtError::StopIteration(arg) => {
            use crate::objects::objtype::objexcept::new_exception_args;
            match arg {
                Some(v) => new_exception_args(ctx, state.exc.stop_iteration, &[v]),
                None => crate::objects::objtype::objexcept::new_exception(
                    ctx,
                    state.exc.stop_iteration,
                ),
            }
        }
        _ => {
            let name = e.exc_name();
            let msg = e.message();
            let type_ = state.exc.by_name(name).unwrap_or(state.exc.type_error);
            new_exception_msg(ctx, type_, &msg)
        }
    }
}

/// The string bytes of a value: a qstr id from the pool, a heap
/// `Str`'s bytes, else `None` (the Zig's `mp_obj_str_get_str`
/// returning NULL — `runtime_import.rs`'s `str_bytes`).
pub(crate) fn value_str_bytes(ctx: &CallCtx, v: Value) -> Option<Vec<u8>> {
    match v {
        Value::Qstr(id) => Some(ctx.pool.qstr_str(id as u16).to_vec()),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => Some(s.bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// A module's globals dict ref (`Obj::Module(m).dict`).
fn module_dict(ctx: &CallCtx, module: Value) -> Option<GcRef> {
    match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Some(m.dict),
            _ => None,
        },
        _ => None,
    }
}

/// Allocate a heap `Str` from bytes (C8 construction).
fn new_str(ctx: &mut CallCtx, bytes: &[u8]) -> Value {
    Value::Heap(ctx.alloc(
        Some(ctx.types.str_),
        Obj::Str(StrData {
            bytes: bytes.to_vec(),
        }),
    ))
}

/// Build `"<dir>/<name>"` or `"<dir>/<name><ext>"`
/// (`import_search.zig:226-243`). Returns `None` if it does not
/// fit (`total + 1 > PATH_MAX_LEN`). The separator is added iff
/// `dir` is non-empty, so an empty `dir` (the `sys.path` `""`
/// entry = current directory) yields a bare relative path.
pub fn build_path(dir: &str, name: &str, ext: &str) -> Option<String> {
    let sep_needed = !dir.is_empty();
    let total = dir.len() + usize::from(sep_needed) + name.len() + ext.len();
    if total + 1 > PATH_MAX_LEN {
        return None;
    }
    let mut out = String::with_capacity(total);
    out.push_str(dir);
    if sep_needed {
        out.push('/');
    }
    out.push_str(name);
    out.push_str(ext);
    Some(out)
}

/// Read a file's full contents (`import_search.zig:192-219`).
/// `None` on open / size / read failure (treated as "not found"
/// by the callers — the Zig returns null the same way).
pub fn read_file_full(path: &str) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Search a single directory for `component` as a module file or
/// subpackage directory (`import_search.zig:258-282`). Tries `.py`
/// first (the vendor's `stat_file_py_or_mpy` precedence), then
/// `.mpy`, then a directory.
pub fn stat_component_in(root: &str, component: &str) -> Found {
    if let Some(p) = build_path(root, component, ".py") {
        if std::path::Path::new(&p).is_file() {
            return Found {
                kind: FoundKind::FilePy,
                path: p,
            };
        }
    }
    if let Some(p) = build_path(root, component, ".mpy") {
        if std::path::Path::new(&p).is_file() {
            return Found {
                kind: FoundKind::FileMpy,
                path: p,
            };
        }
    }
    if let Some(p) = build_path(root, component, "") {
        if std::path::Path::new(&p).is_dir() {
            return Found {
                kind: FoundKind::DirPackage,
                path: p,
            };
        }
    }
    Found {
        kind: FoundKind::NotFound,
        path: String::new(),
    }
}

/// Search every `sys.path` entry for `component`
/// (`import_search.zig:286-303`). An empty `sys.path` finds
/// nothing (the Zig returns not-found when `sys_mutable[0]` is
/// null; the Rust default is `[""]`, never empty).
///
/// The entries come from the `sys.path` list OBJECT
/// (`state.sys_path_list`, the `sys_mutable[0]` value — D2)
/// when it exists, so Python `sys.path.append(x)` is visible
/// to imports; otherwise from the `Vec<String>` (hand-built
/// `VmState` fixtures in the import tests). Non-str entries
/// are skipped (the C reads the list through the str slot,
/// which fails the same way).
pub fn search_top_level(ctx: &mut CallCtx, state: &VmState, component: &str) -> Found {
    let mut owned: Vec<String> = Vec::new();
    let entries: &[String] = match state.sys_path_list {
        Some(list_ref) => {
            if let Obj::List(l) = ctx.arena.get(list_ref) {
                for item in &l.items {
                    if let Some(b) = crate::objects::strings::objstr_a::str_data_pool(
                        ctx.arena, ctx.pool, *item,
                    ) {
                        owned.push(String::from_utf8_lossy(&b).into_owned());
                    }
                }
                &owned
            } else {
                &state.sys_path
            }
        }
        None => &state.sys_path,
    };
    for entry in entries {
        let found = stat_component_in(entry, component);
        if found.kind != FoundKind::NotFound {
            return found;
        }
    }
    Found {
        kind: FoundKind::NotFound,
        path: String::new(),
    }
}

/// Look a dotted name up in `sys.modules` BYTES — no interning
/// needed (the dispatch path cannot intern a genuinely-new prefix
/// name; the scan compares each registered module's `__name__`
/// bytes). Returns the module object, or `None`.
pub fn loaded_lookup_by_bytes(
    ctx: &CallCtx,
    state: &VmState,
    name: &[u8],
) -> Option<Value> {
    for module_ref in state.modules.values() {
        let module = Value::Heap(*module_ref);
        if let Value::Heap(r) = module {
            if let Obj::Module(m) = ctx.arena.get(r) {
                if ctx.pool.qstr_str(m.name as u16) == name {
                    return Some(module);
                }
            }
        }
    }
    None
}

/// `getattr(obj, attr, None)` for the importer's probes
/// (`import_search.zig:92-100`): an `AttributeError` from a
/// module `__getattr__` (PEP 562) means absent, not failure —
/// the pending raise is cleared and `(None, None)` returned.
/// Other exceptions propagate. Returns `(dest[0], dest[1])`:
/// the method/value plus its bound self (`None` = unbound —
/// the Zig's `dest[1] == MP_OBJ_NULL`).
pub fn load_attr_maybe(
    ctx: &mut CallCtx,
    state: &VmState,
    obj: Value,
    attr: u32,
) -> Result<(Option<Value>, Option<Value>), Value> {
    let next_obj = state.next_builtin;
    match load_method_maybe(ctx, obj, attr, next_obj) {
        Ok(Some(dest)) => Ok((Some(dest.func), dest.self_)),
        Ok(None) => Ok((None, None)),
        Err(e) => {
            // AttributeError → absent (clear the pending raise).
            // Anything else stays an error.
            if let RtError::PyRaise = e {
                if let Some(pr) = ctx.take_pending_raise() {
                    if pr.exc_name == "AttributeError" {
                        return Ok((None, None));
                    }
                    // Not AttributeError — put the raise back and
                    // drain it into the exception object.
                    *ctx.pending_raise = Some(pr);
                }
            }
            Err(rt_error_to_value(ctx, state, e))
        }
    }
}

/// Read the parent module's `__path__` attribute
/// (`import_search.zig:381-396`). Returns the path string on
/// hit, or `None` when the parent is not a package (no
/// `__path__`, a non-str value, an empty path — a frozen
/// package's empty `__path__` means "no filesystem" — or a
/// path over `PATH_MAX_LEN`).
pub fn parent_path(
    ctx: &mut CallCtx,
    state: &VmState,
    parent_obj: Value,
) -> Result<Option<String>, Value> {
    let path_q = dunder(b"__path__").expect("__path__ is a static qstr");
    let (dest, _) = load_attr_maybe(ctx, state, parent_obj, path_q)?;
    let Some(v) = dest else {
        return Ok(None);
    };
    let Some(bytes) = value_str_bytes(ctx, v) else {
        return Ok(None);
    };
    if bytes.is_empty() || bytes.len() > PATH_MAX_LEN {
        return Ok(None);
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok(Some(s)),
        Err(_) => Ok(None),
    }
}

/// Run already-loaded code under a module's globals
/// (`execSourceInModule` / `execMpyInModule` tail,
/// `import_search.zig:119-190`): register the code under the
/// (pre-created, pre-registered) module, build the module-body
/// function, swap globals/locals to the module dict, call it
/// with 0 args, restore. Errors are the escaped exception
/// object.
fn exec_loaded_in_module(
    ctx: &mut CallCtx,
    state: &mut VmState,
    loaded: LoadedCode,
    module_obj: Value,
) -> Result<(), Value> {
    let code_idx = Vm::register_loaded_into(state, loaded, module_obj);
    let dict = module_dict(ctx, module_obj).expect("module object");
    let old_globals = state.globals;
    let old_locals = state.locals;
    state.globals = dict;
    state.locals = Some(dict);
    let fun = Value::Heap(Vm::call_module_fun_ctx(ctx, state, code_idx, module_obj));
    let result = crate::runtime::vm_call::call_function_ctx(ctx, state, fun, 0, 0, &[]);
    state.globals = old_globals;
    state.locals = old_locals;
    result.map(|_| ())
}

/// Compile + load + execute Python source bytes under the given
/// module's globals dict (`import_search.zig:119-160`).
/// `source_name` is the module's dotted name (the Zig compiles
/// with `src_name = qstr_str(source_file_qstr)`).
pub fn exec_source_in_module(
    ctx: &mut CallCtx,
    state: &mut VmState,
    src: &[u8],
    source_name: &str,
    module_obj: Value,
) -> Result<(), Value> {
    let mpy = match compile_source_to_mpy(src, InputKind::FileInput, source_name) {
        Ok(m) => m,
        Err(ce) => {
            let file = module_name_qstr(ctx, module_obj);
            return Err(raise_from_code(ctx, state, &ce, file));
        }
    };
    exec_mpy_in_module(ctx, state, &mpy, module_obj)
}

/// Load already-compiled `.mpy` bytes under the given module's
/// globals (`import_search.zig:162-190`).
pub fn exec_mpy_in_module(
    ctx: &mut CallCtx,
    state: &mut VmState,
    mpy_bytes: &[u8],
    module_obj: Value,
) -> Result<(), Value> {
    let module = mpy::read(mpy_bytes).map_err(|_| {
        new_exception_msg(ctx, state.exc.value_error, "incompatible .mpy file")
    })?;
    let qstr_table = crate::runtime::qstr::resolve_qstr_table(ctx.pool, &module.qstrs)
        .map_err(|_| {
            new_exception_msg(ctx, state.exc.memory_error, "out of memory")
        })?;
    let loaded = raw_code_load_mem(ctx, state, module, qstr_table)
        .map_err(|e| vm_error_to_value(ctx, state, e))?;
    exec_loaded_in_module(ctx, state, loaded, module_obj)
}

/// The module object's `__name__` qstr id (its `ModuleData.name`
/// — the full dotted name), or 0 when the value is not a module.
fn module_name_qstr(ctx: &CallCtx, module: Value) -> u32 {
    match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.name,
            _ => 0,
        },
        _ => 0,
    }
}

/// Map a `VmError` to its exception object (shared with the
/// bridge — see `compile_bridge::vm_error_to_value`).
pub(crate) fn vm_error_to_value(
    ctx: &mut CallCtx,
    state: &VmState,
    e: VmError,
) -> Value {
    match e {
        VmError::Compile(ce) => raise_from_code(ctx, state, &ce, 0),
        VmError::Mpy(_) => {
            new_exception_msg(ctx, state.exc.value_error, "incompatible .mpy file")
        }
        VmError::OutOfMemory => {
            new_exception_msg(ctx, state.exc.memory_error, "out of memory")
        }
        VmError::Raised(v) => v,
    }
}

/// Load whatever `stat_component_in` / `search_top_level`
/// returned into `module_obj` (already registered in
/// `sys.modules` under `full_qstr`)
/// (`import_search.zig:307-379`).
fn load_found(
    ctx: &mut CallCtx,
    state: &mut VmState,
    found: &Found,
    full_qstr: u32,
    module_obj: Value,
) -> Result<(), Value> {
    match found.kind {
        FoundKind::NotFound => unreachable!("load_found on not-found"),
        FoundKind::FilePy => {
            let Some(src) = read_file_full(&found.path) else {
                // The Zig returns a bare `PyRaise` with NO
                // pending exception here (the file vanished
                // between stat and read — a TOCTOU wart). The
                // closest raisable thing is `ImportError`.
                return Err(new_exception_msg(
                    ctx,
                    state.exc.import_error,
                    "module not found",
                ));
            };
            let name = ctx.pool.qstr_str(full_qstr as u16).to_vec();
            let name = String::from_utf8_lossy(&name).into_owned();
            exec_source_in_module(ctx, state, &src, &name, module_obj)
        }
        FoundKind::FileMpy => {
            let Some(bytes) = read_file_full(&found.path) else {
                return Err(new_exception_msg(
                    ctx,
                    state.exc.import_error,
                    "module not found",
                ));
            };
            exec_mpy_in_module(ctx, state, &bytes, module_obj)
        }
        FoundKind::DirPackage => {
            let dir = found.path.clone();
            // Store `__path__` as a fresh string.
            let path_q = dunder(b"__path__").expect("__path__ is a static qstr");
            let path_str = new_str(ctx, dir.as_bytes());
            let dict = module_dict(ctx, module_obj).expect("module object");
            if let Err(e) = dict_store(
                ctx,
                Value::Heap(dict),
                Value::Qstr(path_q),
                path_str,
            ) {
                let err = ctx.record_typed(e);
                return Err(exc_from_pyerror(ctx, state, err));
            }
            // Try `__init__.py`, then `__init__.mpy`. Either is
            // optional — a namespace-style package with no
            // `__init__` is fine. NOTE the Zig's control flow:
            // a PRESENT-but-unreadable `__init__.py` returns
            // silently WITHOUT trying `__init__.mpy` (the
            // `return` sits inside the `pathIsFile` block).
            if let Some(init) = build_path(&dir, "__init__", ".py") {
                if std::path::Path::new(&init).is_file() {
                    if let Some(src) = read_file_full(&init) {
                        let name =
                            ctx.pool.qstr_str(full_qstr as u16).to_vec();
                        let name = String::from_utf8_lossy(&name).into_owned();
                        exec_source_in_module(ctx, state, &src, &name, module_obj)?;
                    }
                    return Ok(());
                }
            }
            if let Some(init) = build_path(&dir, "__init__", ".mpy") {
                if std::path::Path::new(&init).is_file() {
                    if let Some(bytes) = read_file_full(&init) {
                        exec_mpy_in_module(ctx, state, &bytes, module_obj)?;
                    }
                }
            }
            Ok(())
        }
    }
}

/// Load a frozen module under `full_qstr`
/// (`import_search.zig:398-411`). A frozen package gets an
/// empty `__path__` (marks it as a package for `from pkg
/// import sub`; `parent_path` reads it as "no filesystem", so
/// its submodules come only from the frozen table).
fn load_frozen(
    ctx: &mut CallCtx,
    state: &mut VmState,
    bytes: &[u8],
    full_qstr: u32,
) -> Result<Value, Value> {
    let module_obj = create_and_register(ctx, state, full_qstr);
    let result = (|| {
        let name = ctx.pool.qstr_str(full_qstr as u16).to_vec();
        let name = String::from_utf8_lossy(&name).into_owned();
        if state.frozen_modules.is_package(&name) {
            let path_q = dunder(b"__path__").expect("__path__ is a static qstr");
            let empty = new_str(ctx, b"");
            let dict = module_dict(ctx, module_obj).expect("module object");
            if let Err(e) = dict_store(
                ctx,
                Value::Heap(dict),
                Value::Qstr(path_q),
                empty,
            ) {
                let err = ctx.record_typed(e);
                return Err(exc_from_pyerror(ctx, state, err));
            }
        }
        exec_mpy_in_module(ctx, state, bytes, module_obj)
    })();
    match result {
        Ok(()) => Ok(module_obj),
        Err(e) => {
            remove_from_loaded(ctx, state, full_qstr);
            Err(e)
        }
    }
}

/// Create a module object + register it in `sys.modules` under
/// its full dotted name + pin it in `roots.extra` (the Zig's
/// `mp_obj_new_module` registers; the pin mirrors
/// `load_module_from_mpy` — the module must survive a GC
/// during its own body execution).
///
/// The live `sys.modules` dict (`state.sys_modules_dict`,
/// D2) is kept in step with `state.modules` — builtin
/// modules are never inserted (probed: after `import os`,
/// `sys.modules` is still `{}`). A failed mirror store is
/// ignored (the mirror is a view; the primary registration
/// already happened — and a store into a fresh plain dict
/// cannot fail short of OOM, which the module alloc would
/// have hit first).
fn create_and_register(
    ctx: &mut CallCtx,
    state: &mut VmState,
    full_qstr: u32,
) -> Value {
    let module_val = crate::objects::objtype::objmodule::new_module(
        ctx,
        full_qstr,
        state.vm_types.module,
    );
    if let Value::Heap(r) = module_val {
        state.modules.insert(full_qstr, r);
    }
    if let Some(mirror) = state.sys_modules_dict {
        let _ = crate::objects::containers::objdict::dict_store(
            ctx,
            Value::Heap(mirror),
            Value::Qstr(full_qstr),
            module_val,
        );
    }
    ctx.roots.extra.insert(state.extra_base, module_val);
    state.extra_base += 1;
    module_val
}

/// Remove a partially-registered module from `sys.modules`
/// (`import_search.zig:456-459`, the Zig's `unload_module`).
/// Used on not-found and on partial-init failure. The live
/// `sys.modules` dict is kept in step (D2).
pub fn remove_from_loaded(
    ctx: &mut CallCtx,
    state: &mut VmState,
    modname_qstr: u32,
) {
    state.modules.remove(&modname_qstr);
    if let Some(mirror) = state.sys_modules_dict {
        let _ = crate::objects::containers::objdict::dict_delete(
            ctx,
            Value::Heap(mirror),
            Value::Qstr(modname_qstr),
        );
    }
}

/// Public entry — called once per dotted component from
/// `builtinimport.rs` (`import_search.zig:418-454`).
///
/// `full_qstr` — the absolute dotted name up to this depth
/// (`"foo.bar"`); `level_slice` — just the current component
/// (`"bar"`); `parent` — the parent module object, or `None`
/// for the top-level component.
///
/// Returns the loaded module object, or `Ok(None)` when no
/// source for the component was found anywhere. `PyError`
/// propagates real load failures (as the exception object).
pub fn try_load_component(
    ctx: &mut CallCtx,
    state: &mut VmState,
    full_qstr: u32,
    level_slice: &[u8],
    parent: Option<Value>,
) -> Result<Option<Value>, Value> {
    let mut found = Found {
        kind: FoundKind::NotFound,
        path: String::new(),
    };
    if parent.is_none() {
        // Top-level: frozen-tiny first (by LEVEL name), then
        // sys.path.
        if let Ok(level_str) = std::str::from_utf8(level_slice) {
            if let Some(bytes) = state.frozen_modules.lookup(level_str) {
                let bytes = bytes.to_vec();
                return Ok(Some(load_frozen(ctx, state, &bytes, full_qstr)?));
            }
        }
        if let Ok(component) = std::str::from_utf8(level_slice) {
            found = search_top_level(ctx, state, component);
        }
    } else {
        // Submodule: frozen (by FULL dotted name) first, then
        // the parent's `__path__`.
        let full_name = ctx.pool.qstr_str(full_qstr as u16).to_vec();
        if let Ok(full_str) = std::str::from_utf8(&full_name) {
            if let Some(bytes) = state.frozen_modules.lookup(full_str) {
                let bytes = bytes.to_vec();
                return Ok(Some(load_frozen(ctx, state, &bytes, full_qstr)?));
            }
        }
        let parent_obj = parent.expect("parent is Some here");
        if let Some(parent_dir) = parent_path(ctx, state, parent_obj)? {
            if let Ok(component) = std::str::from_utf8(level_slice) {
                found = stat_component_in(&parent_dir, component);
            }
        }
    }
    if found.kind == FoundKind::NotFound {
        return Ok(None);
    }
    // Create + register the module under its full dotted name.
    let module_obj = create_and_register(ctx, state, full_qstr);
    if let Err(e) = load_found(ctx, state, &found, full_qstr, module_obj) {
        remove_from_loaded(ctx, state, full_qstr);
        return Err(e);
    }
    Ok(Some(module_obj))
}
