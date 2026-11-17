//! `eval` / `exec` / `compile` (`runtime/builtinevex.zig`).
//!
//! The three `MpObjFunBuiltinVar` ROM objects (sigs `(1, 3,
//! false)` / `(1, 3, false)` / `(3, 6, false)`,
//! `builtinevex.zig:207-225`) are built by `Vm::build_builtins`
//! and intercepted by GcRef identity in `call_function_ctx`
//! (the `next` pattern) — their `func`s are stubs, as `next`'s
//! is. The interception targets are `call_eval_exec_ctx` and
//! `call_compile_ctx` below.
//!
//! `eval`/`exec` semantics (`evalExecHelperImpl`,
//! `builtinevex.zig:117-149` + `codeExecute`, `:64-84`):
//! globals default to the current thread globals, locals to
//! the current thread locals; the `for i in 1..3 && i<n_args`
//! loop takes `args[1]` as globals (and locals too, `i==1`)
//! and `args[2]` as locals — each must be a dict or `None`
//! (else the bare `TypeError`, `raiseTypeErrorNull`); the
//! source goes through the buffer protocol (`getBufferRaise`);
//! the code-object fast path (`objIsCode`) is dead in Rust
//! (there is no `Obj::Code` yet — see `call_compile_ctx`).
//! The body runs with the caller's globals/locals
//! (`codeExecute`'s save/restore + its patch of
//! `module_fun.context.module.globals` to the caller's globals
//! — the Rust form is `Vm::eval_exec` / `Vm::eval_exec_ctx`,
//! which replace the eval module's dict).
//!
//! Non-buffer source (`eval(123)`, `eval([1])`, …): the Zig's
//! `getBufferRaise` IGNORES the `mp_get_buffer` failure (`_ =`,
//! `builtinevex.zig:33-35`) and `evalExecHelperImpl` calls
//! `npy_zig_eval_exec` with the UNINITIALIZED `bufinfo` —
//! `raiseBufferProtocol`'s `TypeError("object with buffer
//! protocol required")` (`obj.zig:179-182`) is recorded and
//! then OVERWRITTEN by the bridge's OOM path
//! (`zig_compile_bridge.zig:110`,
//! `mp_raise_OSError_polled(MP_ENOMEM)`). Probed on the
//! reference binary: `eval(123)`, `eval([1])`, `eval(None)`,
//! `eval({})`, `eval(1.5)` ALL raise `OSError(12,)` — stable
//! on this (Debug) binary, so the Rust port raises
//! `OSError(12)` (`MP_ENOMEM`) for a non-buffer source, NOT
//! the overwritten TypeError. `MP_ENOMEM = 12`
//! (`zig_compile_bridge.zig:51-52`).

use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::buffer_bytes;
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::strings::objstr_a::str_data_pool;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::{raise_bad_compile_mode, raise_os_error, raise_type_error_null};
use crate::runtime::vm::Vm;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};

/// `MP_ENOMEM` (`zig_compile_bridge.zig:51-52`): the errno a
/// non-buffer `eval`/`exec` source raises (see the module docs).
pub const MP_ENOMEM: i32 = 12;

/// `mp_builtin_eval` as a `BuiltinFn` — a stub, reachable only
/// on a direct call bypassing the `Vm` (the interception in
/// `call_function_ctx` handles the real calls, as `next`'s
/// stub does for `next`). A bare `Err(PyRaise)` with nothing
/// recorded, so the VM falls back to a bare `TypeError`.
pub(crate) fn eval_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `mp_builtin_exec` as a `BuiltinFn` — same stub shape as `eval`.
pub(crate) fn exec_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `mp_builtin_compile` as a `BuiltinFn` — same stub shape as `eval`.
pub(crate) fn compile_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// Is `value` an exact `dict` (`mirrors.isExactType(v,
/// &mp_type_dict)` — a dict SUBCLASS instance is not exact)?
fn is_exact_dict(ctx: &CallCtx, value: Value) -> bool {
    obj_get_type(ctx.arena, ctx.types, value) == Some(ctx.types.dict)
}

/// The source bytes of an `eval`/`exec` argument, through the
/// buffer protocol (`getBufferRaise`,
/// `builtinevex.zig:33-35` + `mp_get_buffer`,
/// `obj.zig:637-660`): `str`/`bytes` read their bytes, `array`/
/// `bytearray`/`memoryview` read through the safe `buffer_bytes`
/// (`binary.rs:572` — the safe-Rust half of the protocol), a
/// qstr immediate reads the pool. Anything else raises
/// `OSError(MP_ENOMEM)` — see the module docs for why the
/// error is `OSError(12)` and not the buffer-protocol
/// `TypeError`.
fn source_bytes(ctx: &mut CallCtx, value: Value) -> PyResult<Vec<u8>> {
    if let Value::Qstr(_) = value {
        if let Some(bytes) = str_data_pool(ctx.arena, ctx.pool, value) {
            return Ok(bytes);
        }
    }
    if let Some((bytes, _)) = buffer_bytes(ctx, value) {
        return Ok(bytes);
    }
    Err(raise_os_error(ctx, MP_ENOMEM))
}

/// `evalExecHelperImpl` (`builtinevex.zig:117-149`): resolve
/// globals/locals from `args` (the `i in 1..3` loop — `args[1]`
/// is globals and, when given, locals too; `args[2]` is
/// locals; each must be a dict or `None`), read the source
/// bytes, and run the body. `args[0]` is the source;
/// `args[1..]` are the optional globals/locals. Returns the
/// body value (`eval`) or `Value::None` (`exec`) — the
/// `InputKind` selects which, in `Vm::eval_exec_ctx`.
fn eval_exec_helper(
    ctx: &mut CallCtx,
    state: &mut VmState,
    args: &[Value],
    kind: InputKind,
) -> Result<Value, Value> {
    let mut globals = state.globals;
    let mut locals = state.locals_dict();
    let mut i = 1usize;
    while i < 3 && i < args.len() {
        if args[i] != Value::None {
            if !is_exact_dict(ctx, args[i]) {
                let e = raise_type_error_null(ctx);
                return Err(exc_from_pyerror(ctx, state, e));
            }
            let Value::Heap(r) = args[i] else {
                unreachable!("an exact dict is a heap value");
            };
            locals = r;
            if i == 1 {
                globals = r;
            }
        }
        i += 1;
    }
    let source = match source_bytes(ctx, args[0]) {
        Ok(b) => b,
        Err(e) => return Err(exc_from_pyerror(ctx, state, e)),
    };
    Vm::eval_exec_ctx(ctx, state, &source, kind, globals, locals)
}

/// The `eval`/`exec` interception (`call_function_ctx`): the
/// sig is `(1, 3, false)` — 1–3 positional args, no kwargs
/// (probed: `eval()` and `eval("1", {}, {}, 1)` both raise
/// the terse mismatch). The arity check is here, not in the
/// `FunBuiltin` slot — the interception happens BEFORE the
/// slot call, as `call_next_ctx` does for `next`.
pub fn call_eval_exec_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    kind: InputKind,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 0 || n_args > 3 || n_kw != 0 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    eval_exec_helper(ctx, state, args, kind)
}

/// Is `value` a `str` for `mp_obj_str_get_qstr`
/// (`objstr.zig:3297-3308`): a qstr immediate, or an exact
/// `str` heap object. `bytes` is NOT — the Zig calls
/// `bad_implicit_conversion` for it (polled: records
/// `TypeError("can't convert to str implicitly")` and returns
/// `MP_QSTRnull`).
fn is_str_for_qstr(ctx: &CallCtx, value: Value) -> bool {
    match value {
        Value::Qstr(_) => true,
        Value::Heap(r) => {
            matches!(ctx.arena.get(r), crate::objects::value::Obj::Str(_))
        }
        _ => false,
    }
}

/// The mode qstr of a `compile()` mode argument, as the Zig's
/// `mp_obj_str_get_qstr` + comparison would see it: a qstr
/// immediate compares by id against the static ids; a heap
/// `str` compares by bytes (the Zig interns it, and interning
/// a static string returns the static id — a dynamic id for
/// `"single"`/`"exec"`/`"eval"` can never exist, since
/// `from_strn` finds the static entry first). Returns the
/// `InputKind` on a match, `None` otherwise.
fn compile_mode_kind(ctx: &CallCtx, value: Value) -> Option<InputKind> {
    let single = static_qstr_id(b"single");
    let exec = static_qstr_id(b"exec");
    let eval = static_qstr_id(b"eval");
    match value {
        Value::Qstr(id) => {
            if Some(id) == single {
                Some(InputKind::SingleInput)
            } else if Some(id) == exec {
                Some(InputKind::FileInput)
            } else if Some(id) == eval {
                Some(InputKind::EvalInput)
            } else {
                None
            }
        }
        Value::Heap(r) => match ctx.arena.get(r) {
            crate::objects::value::Obj::Str(s) => match s.bytes.as_slice() {
                b"single" => Some(InputKind::SingleInput),
                b"exec" => Some(InputKind::FileInput),
                b"eval" => Some(InputKind::EvalInput),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `compile_glue.impl` (`builtinevex.zig:177-196`) up to the
/// code-object creation: validate the source (str/bytes via
/// `mp_obj_str_get_data` — a non-str/bytes source raises the
/// `TypeError` the Zig's polled `str_get_str` records before
/// the bridge's null-cast abort, see below), the filename (a
/// `str` for `mp_obj_str_get_qstr` — else `TypeError("can't
/// convert to str implicitly")`, probed), and the mode
/// (`single`/`exec`/`eval` — else `ValueError("bad compile
/// mode")`, probed — the mode check OVERWRITES a pending
/// filename error, since the Zig's `pending_exc` is a plain
/// assignment: `compile("1", 5, 5)` raises the `ValueError`,
/// probed).
///
/// BLOCKED on the missing `Obj::Code`: after validation this
/// raises `NotImplementedError` instead of returning a code
/// object. The gap is precise — it needs an `Obj::Code`
/// variant + the code type in ROM + `codeExecute` over code
/// objects (owner: C15b / the entry work). A `FunBc`-as-code
/// interim was considered and REJECTED: it would misrepresent
/// `type(c)` (`'function'` vs `'code'`) — a silent divergence
/// the rules forbid. (Note the original itself cannot report
/// `type(code)`: the code type pointer is null there, so
/// `type(compile(...))` ABORTS the reference binary rc=134 —
/// probed. `repr(c)` is `<>`, `hash(c)` works, `eval(c)` /
/// `exec(c)` work there; all of that stays blocked here on
/// `Obj::Code`.)
///
/// Original-abort divergences (the reference binary aborts
/// rc=134; Rust raises instead): a non-str/bytes `compile()`
/// source (`compile(5, "f", "exec")` — the Zig passes the
/// NULL `str_get_data` result to `@ptrCast` in
/// `zig_compile_bridge.zig:322`). Rust raises the
/// `TypeError("can't convert to str implicitly")` the Zig's
/// polled str read records before the abort.
fn compile_glue(
    ctx: &mut CallCtx,
    state: &mut VmState,
    args: &[Value],
) -> Result<Value, Value> {
    // Source: `mp_obj_str_get_data` — str or bytes.
    if str_data_pool(ctx.arena, ctx.pool, args[0]).is_none() {
        return Err(type_error(ctx, state, "can't convert to str implicitly"));
    }
    // Filename: `mp_obj_str_get_qstr` — a str, else the
    // implicit-conversion TypeError (recorded, but possibly
    // overwritten by the mode check below).
    let filename_ok = is_str_for_qstr(ctx, args[1]);
    // Mode: `mp_obj_str_get_qstr` + match — a non-match (or a
    // non-str mode, whose qstr read returns null) is the bad
    // mode error, which OVERWRITES the filename error (the
    // Zig's `pending_exc` is a plain assignment).
    let kind = compile_mode_kind(ctx, args[2]);
    match kind {
        Some(_) => {
            if !filename_ok {
                return Err(type_error(ctx, state, "can't convert to str implicitly"));
            }
        }
        None => {
            let e = raise_bad_compile_mode(ctx);
            return Err(exc_from_pyerror(ctx, state, e));
        }
    }
    // Validated — now COMPILE (C15b: the bridge's `compile_to_code`
    // compiles first, so a syntax error beats the missing-code
    // error, as on the original — then raises NotImplementedError
    // because there is no `Obj::Code` yet).
    let kind = kind.expect("mode checked above");
    let src = str_data_pool(ctx.arena, ctx.pool, args[0]).expect("source checked above");
    let filename_bytes =
        str_data_pool(ctx.arena, ctx.pool, args[1]).expect("filename checked above");
    let filename = String::from_utf8_lossy(&filename_bytes).into_owned();
    crate::runtime::compile_bridge::compile_to_code(ctx, state, &src, kind, &filename)
}

/// The `compile` interception (`call_function_ctx`): the sig
/// is `(3, 6, false)` — 3–6 positional args, no kwargs
/// (`compile("1", "f", "eval", 0)` succeeds on the reference
/// binary — the extra args are ignored by `compile_glue.impl`,
/// probed). The arity check is here, as for `eval`/`exec`.
pub fn call_compile_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args < 3 || n_args > 6 || n_kw != 0 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    compile_glue(ctx, state, args)
}
