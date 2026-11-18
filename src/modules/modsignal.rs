//! The `signal` builtin module (`src/modules/modsignal.zig`, 168 lines).
//!
//! Ported from the Zig (the MVP shape): `signal`, `getsignal`
//! plus the `SIG*` constants and `SIG_DFL` / `SIG_IGN` — 14
//! entries (`__name__` + 2 fns + 11 constants).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - Constants are the Linux `signal.h` values (`SIGTERM` 15,
//!   `SIGUSR1` 10, `SIGUSR2` 12 — the Zig takes them from the
//!   running platform's `cImport`; macOS differs for USR1/USR2).
//! - `SIG_DFL` / `SIG_IGN` are ints `0` / `1` (CPython's
//!   representation, not libc pointers).
//! - `signal(signum, handler)` records `handler` in the
//!   per-interpreter table (`state.d4.signal_handlers`,
//!   `NSIG = 64`) and returns the previous handler — `0`
//!   (`SIG_DFL`) when none was set; out-of-range signums
//!   return `0` without storing.
//! - `getsignal(signum)` returns the stored handler or `0`.
//! - NO libc `sigaction(2)` call is made and the handler is
//!   NEVER dispatched (the Zig's MVP limitation, documented
//!   in its header): delivery needs a bytecode-loop safepoint
//!   dispatcher (deferred in the original too). The process
//!   behaves per libc defaults.
//!
//! The two fns need VM state (the handler table), so they are
//! intercepted by GcRef identity in `vm_call_intercept.rs`
//! (the D2 `os` fd-builtin pattern); their `func`s here are
//! stubs. The builder takes the D2 shape `(ctx, state)`.

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::obj::obj_get_int;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::d4state::NSIG;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};

/// The signal constants (`modsignal.zig` table order):
/// `SIGHUP SIGINT SIGQUIT SIGKILL SIGUSR1 SIGUSR2 SIGPIPE
/// SIGTERM SIGCHLD`, then `SIG_DFL` / `SIG_IGN`.
const SIGNALS: &[(&str, i64)] = &[
    ("SIGHUP", 1),
    ("SIGINT", 2),
    ("SIGQUIT", 3),
    ("SIGKILL", 9),
    ("SIGUSR1", 10),
    ("SIGUSR2", 12),
    ("SIGPIPE", 13),
    ("SIGTERM", 15),
    ("SIGCHLD", 17),
    ("SIG_DFL", 0),
    ("SIG_IGN", 1),
];

/// The stub `func` for an intercepted signal builtin — a
/// bare raise, like D2's `fd_stub_fn` (the interception in
/// `call_function_ctx` handles the real calls).
fn signal_stub_fn(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// The arity check the intercepted bodies need
/// (`call_function_ctx` bypasses the normal dispatch, so the
/// sig is checked here — the D2 `modos_fd.rs` shape, same
/// message the dispatcher raises).
fn check_arity(
    ctx: &mut CallCtx,
    state: &VmState,
    n_args: usize,
    n_kw: usize,
    min: usize,
    max: usize,
) -> Result<(), Value> {
    if n_kw != 0 || n_args < min || n_args > max {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    Ok(())
}

/// Map a `PyError` into the `Value` exception the
/// interception returns (the D2 `modos_fd.rs` `exc` shape).
fn exc(ctx: &mut CallCtx, state: &VmState, e: PyError) -> Value {
    exc_from_pyerror(ctx, state, e)
}

/// The signum argument as a table index (`modsignal.zig`):
/// `mp_obj_get_int` first (a non-int is `TypeError("can't
/// convert to int")`), then the range check — out-of-range
/// (`< 0` or `>= NSIG`) returns `0` (`SIG_DFL`) without
/// storing or reading.
fn signum_index(ctx: &mut CallCtx, state: &VmState, v: Value) -> Result<Option<usize>, Value> {
    let signum = match obj_get_int(ctx, ctx.types, v) {
        Ok(n) => n,
        Err(e) => {
            let pe = ctx.record_typed(e);
            return Err(exc(ctx, state, pe));
        }
    };
    if signum < 0 || signum as usize >= NSIG {
        return Ok(None);
    }
    Ok(Some(signum as usize))
}

/// `py_signal` (`modsignal.zig`): store `handler` for
/// `signum`, return the previous handler (`0` when none was
/// set). The handler is written into the reserved root slot
/// for its signum (`state.d4.signal_base + signum`) so it
/// survives GC — the table itself is outside the arena.
pub fn call_signal_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 2, 2)?;
    let Some(idx) = signum_index(ctx, state, args[0])? else {
        return Ok(Value::SmallInt(0));
    };
    let prev = state.d4.signal_handlers[idx];
    state.d4.signal_handlers[idx] = args[1];
    ctx.roots.extra[state.d4.signal_base + idx] = args[1];
    match prev {
        Value::None => Ok(Value::SmallInt(0)),
        v => Ok(v),
    }
}

/// `py_getsignal` (`modsignal.zig`): return the stored
/// handler for `signum`, or `0` when none was set.
pub fn call_getsignal_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 1, 1)?;
    let Some(idx) = signum_index(ctx, state, args[0])? else {
        return Ok(Value::SmallInt(0));
    };
    match state.d4.signal_handlers[idx] {
        Value::None => Ok(Value::SmallInt(0)),
        v => Ok(v),
    }
}

/// `build_signal_module` (`modsignal.zig` table): 14 entries.
/// Records the two intercepted builtins on `state.d4`.
pub fn build_signal_module(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let (module, dict) = new_d4_module(ctx, "signal", module_type);
    let signal = mk_fun(ctx, fun_builtin_var, "signal", 2, 2, signal_stub_fn, None);
    let getsignal = mk_fun(ctx, fun_builtin_var, "getsignal", 1, 1, signal_stub_fn, None);
    state.d4.signal_builtin = signal;
    state.d4.getsignal_builtin = getsignal;
    store(ctx, dict, "signal", signal);
    store(ctx, dict, "getsignal", getsignal);
    for (name, num) in SIGNALS {
        store(ctx, dict, name, Value::SmallInt(*num));
    }
    fix_dict(ctx, dict);
    module
}
