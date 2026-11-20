//! `eval` / `exec` / `compile` execution + `.mpy` code
//! registration — split out of `vm.rs` (file-size rule).
//!
//! The `Vm`-level entries (`Vm::eval_exec`,
//! `Vm::register_mpy_code`) intern through `&mut self.pool`;
//! the dispatch-path twins (`Vm::eval_exec_ctx`,
//! `Vm::register_mpy_code_ctx`) resolve the qstr table through
//! the shared `CallCtx.pool` — `from_strn` interns through
//! `&self` since C15b, so genuinely-new names are interned on
//! both paths (the old read-only fallback failed them with
//! `OutOfMemory`).

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::compile::CompileError;
use crate::compiler::mpy::{self};
use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::new_fun_bc;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::persistentcode::LoadedCode;
use crate::runtime::qstr::resolve_qstr_table;
use crate::runtime::vm::{Vm, VmError, VmState};

impl Vm {
    /// The shared tail of `register_mpy_code` /
    /// `register_mpy_code_ctx`: convert + pin the const-object
    /// table, create the module object, patch its dict when
    /// asked, register the code. Takes the already-interned
    /// module name id and the already-resolved qstr table — both
    /// callers intern through the shared pool (`from_strn`
    /// takes `&self` since C15b).
    fn register_code_inner(
        ctx: &mut CallCtx,
        state: &mut VmState,
        module: mpy::Module,
        name_id: u32,
        qstr_table: Vec<u16>,
        module_dict: Option<GcRef>,
        register: bool,
    ) -> Result<(Value, usize), VmError> {
        let module_type = state.vm_types.module;
        // Load the code (`mp_raw_code_load_mem`, C15b): header
        // check + const-table conversion with pinning.
        let loaded = crate::runtime::persistentcode::raw_code_load_mem(
            ctx, state, module, qstr_table,
        )?;
        let LoadedCode {
            rc,
            qstr_table,
            const_table,
        } = loaded;
        let module_val = new_module(ctx, name_id, module_type);
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("new_module returns a heap module"),
        };
        if let Some(dict) = module_dict {
            if let Obj::Module(m) = ctx.arena.get_mut(module_ref) {
                m.dict = dict;
            }
        }
        let code_idx = state.code_store.register_module(
            rc,
            qstr_table,
            const_table,
            module_ref,
        );
        if register {
            // D4: the entry module is `__main__` — the original
            // never caches it in the loaded registry or in
            // `sys.modules` (its globals dict is `dict_main`,
            // initialised at `mp_init`); `import __main__`
            // resolves through `state.d4.main_module` instead
            // (set by `load_module_from_mpy`).
            let is_main = ctx.pool.qstr_str(name_id as u16) == b"__main__";
            if !is_main {
                state.modules.insert(name_id, module_ref);
                // Keep the live `sys.modules` dict in step (D2;
                // file modules only — see `create_and_register`).
                if let Some(mirror) = state.sys_modules_dict {
                    let _ = crate::objects::containers::objdict::dict_store(
                        ctx,
                        Value::Heap(mirror),
                        Value::Qstr(name_id),
                        module_val,
                    );
                }
            }
        }
        Ok((module_val, code_idx))
    }

    /// Register already-loaded code under an EXISTING module object
    /// (the import loader's `execMpyInModule`: the module was
    /// created and registered in `state.modules` by
    /// `try_load_component` BEFORE the code was loaded, so no new
    /// module is created and nothing is re-registered here).
    /// Returns the code index.
    pub(crate) fn register_loaded_into(
        state: &mut VmState,
        loaded: LoadedCode,
        module_val: Value,
    ) -> usize {
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("module value"),
        };
        let LoadedCode {
            rc,
            qstr_table,
            const_table,
        } = loaded;
        state
            .code_store
            .register_module(rc, qstr_table, const_table, module_ref)
    }

    /// Read `.mpy` bytes, resolve the qstr table (interning
    /// through `&mut pool`), and register the code. The
    /// `Vm`-level entry (module loads + `Vm::eval_exec`). When
    /// `module_dict` is `Some`, the fresh module's dict is
    /// REPLACED with it — the Rust form of `codeExecute`'s
    /// patch of `module_fun.context.module.globals` to the
    /// caller's globals (`builtinevex.zig:78-84`); the fresh
    /// dict (with its `__name__`) is orphaned, as the Zig's
    /// NULL globals slot is overwritten there. When `register`
    /// is true the module is also entered in `state.modules`
    /// (the import registry — module loads only; eval modules
    /// are transient, as in the Zig). Returns `(module_value,
    /// code_idx)`.
    pub(crate) fn register_mpy_code(
        &mut self,
        data: &[u8],
        name: &[u8],
        module_dict: Option<GcRef>,
        register: bool,
    ) -> Result<(Value, usize), VmError> {
        let module = mpy::read(data).map_err(VmError::Mpy)?;
        // (Intern the name BEFORE the split — the ctx carries
        // the pool shared, and `from_strn` interns through `&self`
        // since C15b, so either order works.)
        let name_id = self
            .pool
            .from_strn(name)
            .map_err(|_| VmError::OutOfMemory)?;
        let qstr_table =
            resolve_qstr_table(&self.pool, &module.qstrs).map_err(|_| VmError::OutOfMemory)?;
        let (mut ctx, state) = self.split();
        Self::register_code_inner(
            &mut ctx,
            state,
            module,
            name_id as u32,
            qstr_table,
            module_dict,
            register,
        )
    }

    /// The dispatch-path twin of `register_mpy_code`: same
    /// steps, resolving the qstr table through the shared
    /// `CallCtx.pool` (`from_strn` interns through `&self`
    /// since C15b — genuinely-new names are interned, as on the
    /// `Vm`-level path). Used by `eval_exec_ctx` (the
    /// `call_function_ctx` interception of `eval`/`exec`).
    pub(crate) fn register_mpy_code_ctx(
        ctx: &mut CallCtx,
        state: &mut VmState,
        data: &[u8],
        name: &[u8],
        module_dict: Option<GcRef>,
    ) -> Result<(Value, usize), VmError> {
        let module = mpy::read(data).map_err(VmError::Mpy)?;
        let name_id = ctx
            .pool
            .from_strn(name)
            .map_err(|_| VmError::OutOfMemory)? as u32;
        let qstr_table =
            resolve_qstr_table(ctx.pool, &module.qstrs).map_err(|_| VmError::OutOfMemory)?;
        Self::register_code_inner(ctx, state, module, name_id, qstr_table, module_dict, false)
    }

    /// The dispatch-path twin of `eval_exec` (the
    /// `call_function_ctx` interception of `eval`/`exec`):
    /// same steps — the qstr table resolves through the shared
    /// pool (`from_strn` interns through `&self` since C15b, so
    /// genuinely-new names in the eval'd source are interned,
    /// as on the `Vm`-level path) — and the body runs through
    /// `call_function_ctx` (no `&mut Vm` is available inside
    /// dispatch). The `Vm`-level `eval_exec` is the complete
    /// one; this one is the interception.
    pub(crate) fn eval_exec_ctx(
        ctx: &mut CallCtx,
        state: &mut VmState,
        source: &[u8],
        kind: InputKind,
        globals: GcRef,
        locals: GcRef,
    ) -> Result<Value, Value> {
        let source_str = match std::str::from_utf8(source) {
            Ok(s) => s,
            Err(_) => {
                return Err(Self::compile_error_value_ctx(
                    ctx,
                    state,
                    CompileError::SyntaxError,
                ));
            }
        };
        let mpy = match compile_to_mpy_named(source_str, kind, b"<string>") {
            Ok(m) => m,
            Err(ce) => return Err(Self::compile_error_value_ctx(ctx, state, ce)),
        };
        let (module_val, code_idx) =
            match Self::register_mpy_code_ctx(ctx, state, &mpy, b"<string>", Some(globals)) {
                Ok(v) => v,
                Err(VmError::OutOfMemory) => {
                    // Register-path allocation failure — NOT a
                    // compiler-reported OOM (that is OSError(12)
                    // via `raise_from_code`); the tree's
                    // convention for allocation failure.
                    return Err(new_exception_msg(
                        ctx,
                        state.exc.memory_error,
                        "out of memory",
                    ));
                }
                Err(e) => unreachable!("eval code registration cannot fail: {e:?}"),
            };
        // Pin the module object, as a module load does.
        ctx.roots.extra.insert(state.extra_base, module_val);
        state.extra_base += 1;
        // Pin the locals dict too when distinct from globals
        // (see `Vm::eval_exec` — it is reachable only through
        // `state.locals`, not a GC root).
        if locals != globals {
            ctx.roots.extra.insert(state.extra_base, Value::Heap(locals));
            state.extra_base += 1;
        }
        let old_globals = state.globals;
        let old_locals = state.locals;
        state.globals = globals;
        state.locals = Some(locals);
        let fun = Value::Heap(Self::call_module_fun_ctx(ctx, state, code_idx, module_val));
        let result = crate::runtime::vm_call::call_function_ctx(ctx, state, fun, 0, 0, &[]);
        state.globals = old_globals;
        state.locals = old_locals;
        match result {
            Ok(v) => Ok(if kind == InputKind::EvalInput {
                v
            } else {
                Value::None
            }),
            Err(exc) => Err(exc),
        }
    }

    /// `compile_error_value`'s ctx+state form (see the
    /// `Vm`-level one). Delegates to the bridge's
    /// `raise_from_code` (the canonical mapping).
    fn compile_error_value_ctx(
        ctx: &mut CallCtx,
        state: &VmState,
        e: CompileError,
    ) -> Value {
        let file = match ctx.pool.from_strn(b"<string>") {
            Ok(id) => u32::from(id),
            Err(_) => 0,
        };
        crate::runtime::compile_bridge::raise_from_code(ctx, state, &e, file)
    }

    /// `call_module_fun`'s ctx+state form: the module-body
    /// `FunBc` for a registered module code (context = the
    /// module, so its globals are the module dict).
    pub(crate) fn call_module_fun_ctx(
        ctx: &mut CallCtx,
        state: &VmState,
        code_idx: usize,
        module_val: Value,
    ) -> GcRef {
        let fun_bc_ty = state.vm_types.fun_bc;
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("module value"),
        };
        let name = static_qstr_id(b"<module>").unwrap_or(0);
        new_fun_bc(
            ctx,
            fun_bc_ty,
            name,
            0,
            code_idx,
            None,
            None,
            Some(module_ref),
        )
    }

    /// Run source bytes as `eval` or `exec` (the `Vm` half of
    /// `builtinevex.rs::eval_exec_helper` + `codeExecute`,
    /// `builtinevex.zig:64-84,117-149`): compile the source
    /// (`debug_mode` false, filename `"<string>"` — the Zig's
    /// `MP_QSTR__lt_string_gt_`), run the body with the
    /// caller's globals/locals, and return the body value for
    /// `eval` or `Value::None` for `exec`. `globals`/`locals`
    /// are the already-resolved dicts (the `i in 1..3` loop
    /// lives in `builtinevex.rs`). The caller's globals/locals
    /// are saved and restored around the call (the Zig's
    /// `defer` in `codeExecute`) on both the success and the
    /// raise path. Errors are the escaped exception object
    /// (including a compile failure, raised as a `SyntaxError`
    /// / `IndentationError` value). Non-UTF-8 source is a
    /// `SyntaxError("invalid syntax")` — the Rust compiler
    /// takes `&str`, and the Zig's lexer would fail the source
    /// at the first undecodable byte.
    pub fn eval_exec(
        &mut self,
        source: &[u8],
        kind: InputKind,
        globals: GcRef,
        locals: GcRef,
    ) -> Result<Value, Value> {
        self.eval_exec_named(source, kind, b"<string>", globals, locals)
    }

    /// `eval_exec` with a caller-chosen source name (F1: the
    /// embedding API's `ppy_exec` compiles with the host-given
    /// filename so tracebacks name it — `api_test.c`'s
    /// `format_exception` check expects `File "<test>"`;
    /// `ppy_eval` uses `"<string>"`). The module is registered
    /// under the same name (eval modules are transient —
    /// `register` is false — so the name only surfaces in
    /// tracebacks).
    pub fn eval_exec_named(
        &mut self,
        source: &[u8],
        kind: InputKind,
        name: &[u8],
        globals: GcRef,
        locals: GcRef,
    ) -> Result<Value, Value> {
        let source_str = match std::str::from_utf8(source) {
            Ok(s) => s,
            Err(_) => return Err(self.compile_error_value_named(CompileError::SyntaxError, name)),
        };
        let mpy = match compile_to_mpy_named(source_str, kind, name) {
            Ok(m) => m,
            Err(ce) => return Err(self.compile_error_value_named(ce, name)),
        };
        let (module_val, code_idx) =
            match self.register_mpy_code(&mpy, name, Some(globals), false) {
                Ok(v) => v,
                Err(VmError::OutOfMemory) => {
                    // Register-path allocation failure — NOT a
                    // compiler-reported OOM (that is OSError(12)
                    // via `raise_from_code`); the tree's
                    // convention for allocation failure.
                    let (mut ctx, state) = self.split();
                    return Err(new_exception_msg(
                        &mut ctx,
                        state.exc.memory_error,
                        "out of memory",
                    ));
                }
                Err(e) => unreachable!("eval code registration cannot fail: {e:?}"),
            };
        // Pin the module object, as a module load does (its dict
        // is the caller's globals, so it stays alive through
        // the caller; the pin keeps the module itself alive for
        // the body call). Pin the locals dict too when it is
        // distinct from globals: it is reachable ONLY through
        // `state.locals` (not a GC root — the Zig scans
        // `thread.dict_locals` as a root), and a GC during the
        // body call could otherwise collect it.
        self.pin(module_val);
        if locals != globals {
            self.pin(Value::Heap(locals));
        }
        let old_globals = self.state.globals;
        let old_locals = self.state.locals;
        self.state.globals = globals;
        self.state.locals = Some(locals);
        let fun = Value::Heap(self.call_module_fun(code_idx, module_val));
        let result = crate::runtime::vm_ops::call_function(self, fun, 0, 0, &[]);
        self.state.globals = old_globals;
        self.state.locals = old_locals;
        match result {
            Ok(v) => Ok(if kind == InputKind::EvalInput {
                v
            } else {
                Value::None
            }),
            Err(exc) => Err(exc),
        }
    }
}

impl Vm {
    /// Replace the module search path (`sys.path` — the `sys`
    /// module that exposes it is D's `modsys`; the default is
    /// `[""]`, the current directory — `runtime.zig:276-279`).
    /// Added by C15b for the import tests (and D1's `modsys`).
    /// G2 (P7a): paths are raw bytes — `sys.path` entries
    /// come from argv / the environment / a script's dirname,
    /// all of which may be non-UTF-8 (the reference keeps the
    /// raw bytes; `new_str` is byte-based).
    pub fn set_sys_path(&mut self, paths: Vec<Vec<u8>>) {
        let (mut ctx, state) = self.split();
        let types = ctx.types;
        let items: Vec<Value> = paths
            .iter()
            .map(|p| crate::objects::strings::objstr_a::new_str(&mut ctx, types, p))
            .collect();
        state.sys_path = paths;
        if let Some(list_ref) = state.sys_path_list {
            if let Obj::List(l) = ctx.arena.get_mut(list_ref) {
                l.items = items;
            }
        }
    }

    /// Replace `sys.argv` (the `sys` module's argv list
    /// object). Added by E1 for the entry point (`wireSysArgv`,
    /// `main.zig`): `['-c', ARG...]` or `[script, ARG...]`.
    /// Like `set_sys_path`, syncs the list object in place.
    ///
    /// G2 (P7a): args are raw bytes (see `set_sys_path`).
    pub fn set_sys_argv(&mut self, args: Vec<Vec<u8>>) {
        let (mut ctx, state) = self.split();
        let types = ctx.types;
        let items: Vec<Value> = args
            .iter()
            .map(|a| crate::objects::strings::objstr_a::new_str(&mut ctx, types, a))
            .collect();
        if let Some(list_ref) = state.sys_argv_list {
            if let Obj::List(l) = ctx.arena.get_mut(list_ref) {
                l.items = items;
            }
        }
    }

    /// Replace the frozen-module table (`frozen_modules.zig`).
    /// Added by C15b for the import tests (and D1's registration
    /// of the frozen `petitpy-lib` bundle).
    pub fn set_frozen_modules(
        &mut self,
        table: crate::runtime::frozen_modules::FrozenTable,
    ) {
        self.state.frozen_modules = table;
    }
}

impl Vm {
    /// Register a `.mpy` module WITHOUT running it: the
    /// register + pin + `__main__` + globals half of
    /// `load_module_from_mpy`. The entry point's
    /// `run_main_source` uses it to set `__main__.__file__` +
    /// `sys.path[0]` between registration and the run.
    /// Returns `(module_value, code_idx, globals_dict)`.
    pub(crate) fn register_main_module(
        &mut self,
        data: &[u8],
        name: &str,
    ) -> Result<(Value, usize, GcRef), VmError> {
        let (module_val, code_idx) = self.register_mpy_code(data, name.as_bytes(), None, true)?;
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("new_module returns a heap module"),
        };
        // The module's globals dict is its `dict` — the frame's
        // globals while the body runs.
        let globals = match self.arena.get(module_ref) {
            Obj::Module(m) => m.dict,
            _ => unreachable!("module object"),
        };
        // Pin the module object (its dict is traced through it).
        self.pin(module_val);
        // D4: the entry module is `__main__` — record it so
        // `import __main__` resolves (the original's
        // `dict_main`, never cached in `sys.modules`).
        if name == "__main__" {
            self.state.d4.main_module = Some(module_ref);
        }
        self.state.globals = globals;
        Ok((module_val, code_idx, globals))
    }

    /// Run an already-registered module's body as a 0-arg
    /// function call: the run half of `load_module_from_mpy`.
    /// Returns the module value, or the raised exception.
    pub(crate) fn run_registered_module(
        &mut self,
        module_val: Value,
        code_idx: usize,
    ) -> Result<Value, VmError> {
        let fun = Value::Heap(self.call_module_fun(code_idx, module_val));
        let result = crate::runtime::vm_ops::call_function(self, fun, 0, 0, &[]);
        match result {
            Ok(_) => Ok(module_val),
            Err(exc) => Err(VmError::Raised(exc)),
        }
    }

    /// Load a module from `.mpy` bytes (the `petitpy-vm` entry
    /// point): read, register, run the module body.
    pub fn load_module_from_mpy(&mut self, data: &[u8], name: &str) -> Result<Value, VmError> {
        let (module_val, code_idx, _globals) = self.register_main_module(data, name)?;
        self.run_registered_module(module_val, code_idx)
    }

    /// Like `compile_error_value` but with the caller's source
    /// name in the traceback frame (`run_main_source`'s
    /// `"<stdin>"` — the eval path's `"<string>"` is wrong
    /// there; see `raise_from_code`). `pub(crate)` for the
    /// REPL line path (`entry::embed_exec`).
    pub(crate) fn compile_error_value_named(
        &mut self,
        e: CompileError,
        source_name: &[u8],
    ) -> Value {
        let file = self
            .pool
            .from_strn(source_name)
            .map(u32::from)
            .unwrap_or(0);
        let (mut ctx, state) = self.split();
        crate::runtime::compile_bridge::raise_from_code(&mut ctx, state, &e, file)
    }

    /// `wireScriptFileAndPath` (`main.zig`): set
    /// `__main__.__file__` = the script path in the module's
    /// dict, and replace `sys.path[0]` with `dirname(script)`.
    /// Runs after registration, BEFORE the body runs.
    ///
    /// G2 (P7a): `script` is raw bytes, not `&str` — a
    /// non-UTF-8 script name must reach `__file__` and
    /// `sys.path[0]` byte-exact (the reference prints the raw
    /// bytes; `new_str` is byte-based so nothing is lost).
    fn wire_script_file_and_path(&mut self, module_val: Value, script: &[u8]) {
        {
            let (mut ctx, _state) = self.split();
            if let Value::Heap(module_ref) = module_val {
                if let Obj::Module(m) = ctx.arena.get(module_ref) {
                    let dict = Value::Heap(m.dict);
                    let file_id = ctx
                        .pool
                        .from_strn(b"__file__")
                        .map(u32::from)
                        .unwrap_or(0);
                    let types = ctx.types;
                    let file_val = crate::objects::strings::objstr_a::new_str(
                        &mut ctx,
                        types,
                        script,
                    );
                    let _ = crate::objects::containers::objdict::dict_store(
                        &mut ctx,
                        dict,
                        Value::Qstr(file_id),
                        file_val,
                    );
                }
            }
        }
        // `sys.path[0] := dirname(script)` — the placeholder
        // empty-qstr entry gets overwritten in place (the Zig's
        // `std.fs.path.dirname` returns "" for a bare filename).
        // Byte-exact: `Path::parent` on the raw `OsStr`, no
        // lossy conversion (a non-UTF-8 directory must land in
        // `sys.path[0]` as-is, as on the reference) — on
        // Windows the path is the lossy UTF-8 rendering (see
        // `support::os_str`).
        let dir: Vec<u8> = std::path::Path::new(
            &crate::support::os_str::os_string_from_bytes(script),
        )
        .parent()
        .map(|p| crate::support::os_str::os_str_as_bytes(p.as_os_str()))
        .unwrap_or_default();
        let mut paths = self.state.sys_path.clone();
        if !paths.is_empty() {
            paths[0] = dir;
        }
        self.set_sys_path(paths);
    }

    /// Run source as `__main__` (the entry point's `runSource`,
    /// `main.zig`): compile with `source_name` (the entry
    /// point's `"<stdin>"` for scripts and `-c` alike —
    /// `main.zig`'s `wireScriptFileAndPath` comment:
    /// "Compile-side source_file stays as <stdin>"), register
    /// the module as `__main__` WITHOUT recording `__main__`
    /// as the source name, set `__main__.__file__` +
    /// `sys.path[0]` BEFORE the body runs when `file` is
    /// given, then run the body. Returns the module value, or
    /// the raised exception object.
    ///
    /// G2 (P7a): `file` is raw bytes — a non-UTF-8 script
    /// name must reach `__file__` byte-exact.
    pub fn run_main_source(
        &mut self,
        src: &[u8],
        source_name: &[u8],
        file: Option<&[u8]>,
    ) -> Result<Value, Value> {
        let src_str = match std::str::from_utf8(src) {
            Ok(s) => s,
            Err(_) => {
                return Err(self.compile_error_value_named(
                    CompileError::SyntaxError,
                    source_name,
                ))
            }
        };
        let mpy = match compile_to_mpy_named(src_str, InputKind::FileInput, source_name) {
            Ok(m) => m,
            Err(ce) => return Err(self.compile_error_value_named(ce, source_name)),
        };
        let (module_val, code_idx, _globals) = match self.register_main_module(&mpy, "__main__") {
            Ok(v) => v,
            Err(e) => {
                let (mut ctx, state) = self.split();
                return Err(crate::runtime::compile_bridge::vm_error_to_value(
                    &mut ctx, state, e,
                ));
            }
        };
        if let Some(path) = file {
            self.wire_script_file_and_path(module_val, path);
        }
        self.run_registered_module(module_val, code_idx)
            .map_err(|e| match e {
                VmError::Raised(exc) => exc,
                _ => unreachable!("running a registered module only raises"),
            })
    }
}
