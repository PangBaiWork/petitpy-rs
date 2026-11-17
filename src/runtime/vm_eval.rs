//! `eval` / `exec` / `compile` execution + `.mpy` code
//! registration — split out of `vm.rs` (file-size rule).
//!
//! The `Vm`-level entries (`Vm::eval_exec`,
//! `Vm::register_mpy_code`) intern through `&mut self.pool`;
//! the dispatch-path twins (`Vm::eval_exec_ctx`,
//! `Vm::register_mpy_code_ctx`) resolve the qstr table
//! read-only — see `resolve_qstr_table_readonly` for the gap
//! that leaves (a genuinely-new name fails with
//! `OutOfMemory` instead of being interned).

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::compile::CompileError;
use crate::compiler::mpy::{self, QstrEntry};
use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::new_fun_bc;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::vm::{resolve_qstr_table, Vm, VmError, VmState};

impl Vm {
    /// The shared tail of `register_mpy_code` /
    /// `register_mpy_code_ctx`: convert + pin the const-object
    /// table, create the module object, patch its dict when
    /// asked, register the code. Takes the already-interned
    /// module name id and the already-resolved qstr table — the
    /// two callers resolve them differently (the `Vm` one
    /// interns through `&mut pool`; the dispatch one looks up
    /// read-only, see `resolve_qstr_table_readonly`).
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
        // Convert the module's const-object table once
        // (`obj_from_entry`) and pin every value in `roots.extra`
        // (inserted at `extra_base`, so `rebuild_frame_roots`
        // keeps them) — `LOAD_CONST_OBJ` must push the SAME
        // object on every load, as the Zig's `obj_table` does.
        // Each value is pinned immediately after conversion so a
        // GC during the next conversion cannot collect it.
        let mut const_table: Vec<Value> = Vec::with_capacity(module.objs.len());
        for entry in &module.objs {
            let types = ctx.types;
            let v = crate::runtime::vm_raise::obj_from_entry(
                ctx, types, state.ellipsis, entry,
            )
            .map_err(|_| VmError::OutOfMemory)?;
            ctx.roots.extra.insert(state.extra_base, v);
            state.extra_base += 1;
            const_table.push(v);
        }
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
            module.rc.clone(),
            qstr_table,
            const_table,
            module_ref,
        );
        if register {
            state.modules.insert(name_id, module_ref);
        }
        Ok((module_val, code_idx))
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
        name: &str,
        module_dict: Option<GcRef>,
        register: bool,
    ) -> Result<(Value, usize), VmError> {
        let module = mpy::read(data).map_err(VmError::Mpy)?;
        // (Intern with `&mut self.pool` BEFORE the split — the
        // ctx carries the pool shared.)
        let name_id = self
            .pool
            .from_strn(name.as_bytes())
            .map_err(|_| VmError::OutOfMemory)?;
        let qstr_table =
            resolve_qstr_table(&mut self.pool, &module.qstrs).map_err(|_| VmError::OutOfMemory)?;
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
    /// steps, but the qstr table is resolved READ-ONLY (see
    /// `resolve_qstr_table_readonly` — a genuinely-new name
    /// fails with `OutOfMemory` because the shared
    /// `CallCtx.pool` cannot intern). Used by `eval_exec_ctx`
    /// (the `call_function_ctx` interception of `eval`/`exec`).
    pub(crate) fn register_mpy_code_ctx(
        ctx: &mut CallCtx,
        state: &mut VmState,
        data: &[u8],
        name: &str,
        module_dict: Option<GcRef>,
    ) -> Result<(Value, usize), VmError> {
        let module = mpy::read(data).map_err(VmError::Mpy)?;
        let name_id = resolve_one_qstr_readonly(ctx.pool, name.as_bytes())
            .map_err(|_| VmError::OutOfMemory)? as u32;
        let qstr_table =
            resolve_qstr_table_readonly(ctx.pool, &module.qstrs).map_err(|_| VmError::OutOfMemory)?;
        Self::register_code_inner(ctx, state, module, name_id, qstr_table, module_dict, false)
    }

    /// Build the exception object for a compile failure
    /// (`SyntaxError("invalid syntax")` /
    /// `IndentationError("unexpected indent")` /
    /// `MemoryError("out of memory")` — the `CompileError`
    /// `Display` texts). Used by `eval_exec`: a compile failure
    /// inside `eval`/`exec` raises the exception, it does not
    /// surface as a `VmError::Compile`.
    fn compile_error_value(&mut self, e: CompileError) -> Value {
        let (mut ctx, state) = self.split();
        Self::compile_error_value_ctx(&mut ctx, state, e)
    }

    /// The dispatch-path twin of `eval_exec` (the
    /// `call_function_ctx` interception of `eval`/`exec`):
    /// same steps, but the qstr table resolves read-only
    /// (`register_mpy_code_ctx` — a genuinely-new name in the
    /// eval'd source fails with `MemoryError`, see
    /// `resolve_qstr_table_readonly`), and the body runs through
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
        let mpy = match compile_to_mpy_named(source_str, kind, "<string>") {
            Ok(m) => m,
            Err(ce) => return Err(Self::compile_error_value_ctx(ctx, state, ce)),
        };
        let (module_val, code_idx) =
            match Self::register_mpy_code_ctx(ctx, state, &mpy, "<string>", Some(globals)) {
                Ok(v) => v,
                Err(VmError::OutOfMemory) => {
                    return Err(Self::compile_error_value_ctx(
                        ctx,
                        state,
                        CompileError::OutOfMemory,
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
    /// `Vm`-level one).
    fn compile_error_value_ctx(
        ctx: &mut CallCtx,
        state: &VmState,
        e: CompileError,
    ) -> Value {
        match e {
            CompileError::SyntaxError => {
                new_exception_msg(ctx, state.exc.syntax_error, "invalid syntax")
            }
            CompileError::IndentationError => new_exception_msg(
                ctx,
                state.exc.indentation_error,
                "unexpected indent",
            ),
            CompileError::OutOfMemory => {
                new_exception_msg(ctx, state.exc.memory_error, "out of memory")
            }
        }
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
        let source_str = match std::str::from_utf8(source) {
            Ok(s) => s,
            Err(_) => return Err(self.compile_error_value(CompileError::SyntaxError)),
        };
        let mpy = match compile_to_mpy_named(source_str, kind, "<string>") {
            Ok(m) => m,
            Err(ce) => return Err(self.compile_error_value(ce)),
        };
        let (module_val, code_idx) =
            match self.register_mpy_code(&mpy, "<string>", Some(globals), false) {
                Ok(v) => v,
                Err(VmError::OutOfMemory) => {
                    return Err(self.compile_error_value(CompileError::OutOfMemory));
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

/// Resolve one `New` qstr entry read-only: the id when the name
/// is already in the pool, `Err(OutOfMemory)` when it is not
/// (see `resolve_qstr_table_readonly`).
fn resolve_one_qstr_readonly(pool: &RuntimeQstr, bytes: &[u8]) -> PyResult<u16> {
    let id = pool.find_strn(bytes);
    if id == 0 {
        return Err(PyError::OutOfMemory);
    }
    Ok(id)
}

/// `resolve_qstr_table`'s read-only twin, for the dispatch path
/// (`eval`/`exec` called from running code): static entries pass
/// through; `New` bytes resolve through `pool.find_strn` — the
/// name must ALREADY be in the pool (static, or interned by an
/// earlier module load). A genuinely-new name cannot be interned
/// through the shared `CallCtx.pool` (it is `&`, not `&mut` —
/// the C0 design, so the dispatch loop never mutates the pool),
/// so the resolve fails with `OutOfMemory`: the interning
/// subsystem's own error (`from_strn`'s only failure), raised
/// LOUDLY rather than silently mis-resolving the name to `""`
/// (qstr 0). Consequences, verified against the corpus tests:
/// `eval("1+2")`, `eval('x')` with `x` an outer variable, and
/// `exec("def foo(): ...")` with `foo` an outer variable all
/// work (their names are static or interned by the outer
/// module's load); `exec("def bar(): ...")` where `bar` never
/// appeared as an identifier fails with `MemoryError` — the
/// precise gap. The `Vm`-level `eval_exec` (which holds `&mut
/// pool`) interns correctly. Closing this gap for dispatch-path
/// `eval` needs a `CallCtx.pool` mutability decision — see the
/// result file. (`New` entries longer than 255 bytes fail the
/// same way: `find_strn` returns 0 for them, `qstr.rs:121-123`.)
pub fn resolve_qstr_table_readonly(
    pool: &RuntimeQstr,
    qstrs: &[QstrEntry],
) -> PyResult<Vec<u16>> {
    let mut table = Vec::with_capacity(qstrs.len());
    for q in qstrs {
        match q {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => table.push(resolve_one_qstr_readonly(pool, bytes)?),
        }
    }
    Ok(table)
}
