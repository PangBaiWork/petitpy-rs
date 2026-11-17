//! Compile bridge (`runtime/zig_compile_bridge.zig`, 329 lines).
//!
//! The Zig file is the architectural pivot of NX-3: it replaces the
//! `nlr_push_jump_callback` globals/locals swap with `defer`.
//! Five entry points; the Rust mapping is in
//! `scripts/import_result.txt` §1b. The C-ABI exports
//! (`npy_zig_compile_source` etc.) have no Rust counterpart — the
//! Rust forms are the plain functions below.
//!
//! Two C15a corrections live here (verified on the reference binary,
//! see the result file §5): `raise_from_code` is the canonical
//! compile-error mapping, and `vm_eval.rs::compile_error_value_ctx`
//! delegates to it — the old version mapped `IndentationError` to
//! `"unexpected indent"` (the original says `"invalid syntax"` for
//! BOTH syntax and indent errors) and `OutOfMemory` to
//! `MemoryError("out of memory")` (the Zig raises `OSError(12)`).

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::compile::CompileError;
use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::objtype::objexcept::{
    exception_add_traceback, new_exception_args, new_exception_msg,
};
use crate::objects::value::Value;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::exc_from_pyerror;

/// `MP_ENOMEM` (`zig_compile_bridge.zig:51-52`).
pub const MP_ENOMEM: i32 = 12;

/// Compile source bytes to `.mpy` (shared by `compile_and_load`,
/// `compile_to_code`, and the import loader's
/// `exec_source_in_module`). Non-UTF-8 source is a
/// `SyntaxError` (the Zig's lexer fails at the first
/// undecodable byte — the Rust compiler takes `&str`).
pub fn compile_source_to_mpy(
    src: &[u8],
    kind: InputKind,
    source_name: &str,
) -> Result<Vec<u8>, CompileError> {
    let src_str = std::str::from_utf8(src).map_err(|_| CompileError::SyntaxError)?;
    compile_to_mpy_named(src_str, kind, source_name)
}

/// `raiseFromCode` (`zig_compile_bridge.zig:108`): map a compiler
/// error to its Python exception object.
///
/// * `OutOfMemory` → `OSError(12)` (the Zig's dedicated
///   `mp_raise_OSError_polled(MP_ENOMEM)` path — pre-allocated, no
///   traceback; the message matches vendor under
///   `MEM_ERROR_MSG_TERSE`).
/// * `SyntaxError` / `IndentationError` → `SyntaxError` /
///   `IndentationError` with message `"invalid syntax"` (BOTH —
///   the Zig's match arm is `NPY_ZIG_SYNTAX, NPY_ZIG_INDENT =>
///   "invalid syntax"`) and a traceback frame attached
///   (`source_file`, line 0, block null — `MICROPY_ENABLE_SOURCE_LINE=0`
///   so the printer omits line numbers; without the frame
///   `mp_obj_print_exception` suppresses the `Traceback …`
///   prologue).
///
/// `source_file` is the source name's qstr id (0 = `MP_QSTRnull`
/// when the caller could not resolve one).
pub fn raise_from_code(
    ctx: &mut CallCtx,
    state: &VmState,
    err: &CompileError,
    source_file: u32,
) -> Value {
    match err {
        CompileError::OutOfMemory => {
            // `OSError(12,)` — args == (12,), as probed for
            // `eval(123)` (C15a).
            new_exception_args(ctx, state.exc.os_error, &[Value::SmallInt(12)])
        }
        CompileError::SyntaxError | CompileError::IndentationError => {
            let type_ = match err {
                CompileError::SyntaxError => state.exc.syntax_error,
                _ => state.exc.indentation_error,
            };
            let exc = new_exception_msg(ctx, type_, "invalid syntax");
            exception_add_traceback(ctx, exc, source_file, 0, 0);
            exc
        }
    }
}

/// `compileAndLoad` (`zig_compile_bridge.zig:133`): compile source
/// to `.mpy`, load it, and return the module function WITHOUT
/// executing it (the `npy_zig_compile_source` shape — the caller
/// invokes the result with 0 args to run the module body).
///
/// The module context's globals are the CURRENT globals
/// (`ctx.module.globals = mp_globals_get()` in the Zig); the
/// fresh module object is transient (not registered in
/// `state.modules`, as in `eval_exec_ctx`). A compile failure
/// raises via `raise_from_code` (source name resolved read-only;
/// 0 when it cannot be resolved).
pub fn compile_and_load(
    ctx: &mut CallCtx,
    state: &mut VmState,
    src: &[u8],
    kind: InputKind,
    source_name: &str,
) -> Result<Value, Value> {
    let mpy = match compile_source_to_mpy(src, kind, source_name) {
        Ok(m) => m,
        Err(ce) => {
            let file = resolve_source_file(ctx, source_name);
            return Err(raise_from_code(ctx, state, &ce, file));
        }
    };
    let globals = state.globals;
    let (module_val, code_idx) =
        match crate::runtime::vm::Vm::register_mpy_code_ctx(
            ctx, state, &mpy, source_name, Some(globals),
        ) {
            Ok(v) => v,
            Err(e) => return Err(vm_error_to_value(ctx, state, e)),
        };
    Ok(Value::Heap(crate::runtime::vm::Vm::call_module_fun_ctx(
        ctx, state, code_idx, module_val,
    )))
}

/// `npy_zig_compile_to_code`'s FULL arm (`zig_compile_bridge.zig:316`,
/// `compile_to_code_glue.impl`): compile + load, then wrap the
/// module function as a `code` object — except there is NO
/// `Obj::Code` in Rust yet (Phase E/C18), so after the load the
/// function raises `NotImplementedError` (no message, as in
/// C15a's `compile_glue`).
///
/// Compiling FIRST matters: a syntax error beats the missing-code
/// error (the original raises `SyntaxError` for
/// `compile("1+", "f", "eval")`; C15a's `compile_glue` validated
/// args but never compiled, so it raised `NotImplementedError`
/// instead — now fixed). The loaded module is transient (its
/// globals are NULL in the Zig — `compile()` binds no module's
/// globals; the code object is executed later under whatever
/// globals the caller chooses — so `module_dict` is `None` here).
pub fn compile_to_code(
    ctx: &mut CallCtx,
    state: &mut VmState,
    src: &[u8],
    kind: InputKind,
    source_name: &str,
) -> Result<Value, Value> {
    let mpy = match compile_source_to_mpy(src, kind, source_name) {
        Ok(m) => m,
        Err(ce) => {
            let file = resolve_source_file(ctx, source_name);
            return Err(raise_from_code(ctx, state, &ce, file));
        }
    };
    // Load (validates the .mpy, incl. the header check) and drop
    // the result — the code object it would wrap does not exist.
    if let Err(e) = crate::runtime::vm::Vm::register_mpy_code_ctx(
        ctx, state, &mpy, source_name, None,
    ) {
        return Err(vm_error_to_value(ctx, state, e));
    }
    let e = crate::runtime::raise::raise_not_implemented(ctx, None);
    Err(exc_from_pyerror(ctx, state, e))
}

/// Map a `VmError` to its exception object (for the bridge fns
/// above, whose callers raise `Value`s, not `VmError`s).
/// * `Compile(e)` → `raise_from_code` (source file unknown → 0).
/// * `Mpy(_)` → `ValueError("incompatible .mpy file")` (the Zig's
///   `raiseIncompatibleMpy` — every structural load failure is
///   the same raise in the original).
/// * `OutOfMemory` → `MemoryError("out of memory")` (the existing
///   convention — `exc_from_pyerror`'s `OutOfMemory` arm).
/// * `Raised(v)` → the object itself.
fn vm_error_to_value(
    ctx: &mut CallCtx,
    state: &VmState,
    e: crate::runtime::vm::VmError,
) -> Value {
    use crate::runtime::vm::VmError;
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

/// Resolve a source name to its qstr id (the dispatch path
/// interns for real since C15b — `from_strn` takes `&self`).
/// `0` (`MP_QSTRnull`) when the name cannot be interned
/// (over-long, or an allocation failure).
fn resolve_source_file(ctx: &CallCtx, name: &str) -> u32 {
    ctx.pool
        .from_strn(name.as_bytes())
        .map(u32::from)
        .unwrap_or(0)
}
