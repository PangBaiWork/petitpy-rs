//! Raise helpers (`runtime/raise.zig`).
//!
//! The Zig file is a bridge between the vendor-C noreturn raise
//! ABI and petitpy's polled-sentinel model (`pyerr.pending_exc`):
//! every helper builds the exception, stashes it in
//! `pending_exc`, and — in the Zig — either longjmps to a
//! trampoline handler (`hasActiveHandler()`, native + full) or
//! prints "Traceback (most recent call last):" + the exception
//! and exits 1 (`longlongRaiseDie`, `raise.zig:133-153`).
//!
//! Rust propagation decision (see the result file): the Rust
//! port takes ONLY the propagation path — record the raise in
//! `ctx.pending_raise` (first record wins) and return
//! `Err(PyError::PyRaise)`. The Zig's print+exit exists only
//! because vendor C cannot propagate; the longjmp branch IS
//! propagation, and in Rust there is always a handler — the VM —
//! whose top level prints uncaught exceptions and exits 1. The
//! observable outcome for an uncaught raise is therefore
//! identical, and `try`/`except` can now catch raises that the
//! Zig's print+exit path would have killed the process with
//! (correct Python semantics; the Zig's own trampoline exists
//! precisely to restore that).
//!
//! All six helpers of the second comptime block (`raise.zig:
//! 198-210`) are exported on BOTH arms (full included) — the
//! file's own comment corrects the "nano-only" reading: vendor
//! MPZ (`objint_mpz.c` divide-by-zero / negative shift / buffer
//! too small), `objint.c` `int_to_bytes`, and `modre.c` call them
//! under full. The `nano_raise_active`-gated section (`:124+`)
//! is dead under full and is NOT ported.

use crate::memory::gc::CallCtx;
use crate::objects::value::Value;
use crate::pyerr::PyError;

/// `raiseTypeErrorNull` (`builtinevex.zig:101`): a bare
/// `TypeError` with no message or argument — the Rust form of
/// the Zig's `raiseTypeErrorNull` / the vendor's
/// `mp_raise_TypeError(NULL)`. Reuse this instead of building a
/// second bare-TypeError path (several lanes already have local
/// `PendingRaise::bare("TypeError")` forms; new code uses this).
pub fn raise_type_error_null(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("TypeError", None);
    PyError::PyRaise
}

/// `mp_raise_OSError_polled` (`raise.zig:57-59`): set the
/// pending exception to `OSError(errno)` and return — the only
/// polled raiser in the file. Called by
/// `zig_compile_bridge.zig:110` on the OOM path (`MP_ENOMEM`).
/// The exception carries the errno as its single argument
/// (`newExcArg1`); the VM's `exception_from_pending` rebuilds
/// it as `OSError(errno)`, whose print branch renders
/// `"[Errno 2] ENOENT: x"`.
pub fn raise_os_error(ctx: &mut CallCtx, errno: i32) -> PyError {
    ctx.record_raise_value("OSError", Value::SmallInt(i64::from(errno)));
    PyError::PyRaise
}

/// `m_malloc_fail` (`raise.zig:83-90`): `MemoryError("memory
/// allocation failed")`. `num_bytes` is kept in the Zig
/// signature for ABI back-compat but is unused — the Rust form
/// takes no size. Polled (sets the sentinel and returns); the
/// allocator returns the libc-null result and callers propagate.
pub fn malloc_fail(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("MemoryError", Some("memory allocation failed"));
    PyError::PyRaise
}

/// `mp_raise_msg` / `nano_mp_raise_msg` (`raise.zig:157-160`):
/// raise an exception of type `exc_name` with the message `msg`
/// (`None` → the bare exception, as the Zig's `msg == NULL`
/// branch). `exc_name` is the Python type name as
/// `exception_from_pending` resolves it (`by_name`).
pub fn raise_msg(ctx: &mut CallCtx, exc_name: &'static str, msg: Option<&str>) -> PyError {
    ctx.record_raise(exc_name, msg);
    PyError::PyRaise
}

/// `mp_raise_msg_varg` (`raise.zig:162-166`): raise with the
/// LITERAL format string — varg formatting is deferred in the
/// Zig ("varg formatting deferred — raise with the literal
/// format string"), so the Rust form takes the literal `fmt`
/// and does not format it either. Callers must not pass a
/// `%`-format string expecting substitution.
pub fn raise_msg_varg(ctx: &mut CallCtx, exc_name: &'static str, fmt: &str) -> PyError {
    raise_msg(ctx, exc_name, Some(fmt))
}

/// `mp_raise_ValueError` (`raise.zig:172-175`): `ValueError`
/// with `msg` (`None` → bare).
pub fn raise_value_error(ctx: &mut CallCtx, msg: Option<&str>) -> PyError {
    raise_msg(ctx, "ValueError", msg)
}

/// `mp_raise_TypeError` (`raise.zig:67-71`): `TypeError` with
/// `msg` (`None` → bare).
pub fn raise_type_error(ctx: &mut CallCtx, msg: Option<&str>) -> PyError {
    raise_msg(ctx, "TypeError", msg)
}

/// `mp_raise_NotImplementedError` (`raise.zig:177-180`):
/// `NotImplementedError` with `msg` (`None` → bare).
pub fn raise_not_implemented(ctx: &mut CallCtx, msg: Option<&str>) -> PyError {
    raise_msg(ctx, "NotImplementedError", msg)
}

/// `mp_raise_TypeError_int_conversion` (`raise.zig:186-189`):
/// `TypeError("can't convert to int")`. The `arg` is ignored —
/// the Zig's comment says the arg's type is not formatted into
/// the message (terse reporting), so the Rust form takes no
/// argument at all. Called by vendor `objint.c` when `int()`
/// receives a non-coercible object.
pub fn raise_type_error_int_conversion(ctx: &mut CallCtx) -> PyError {
    raise_msg(ctx, "TypeError", Some("can't convert to int"))
}

/// `mp_raise_type_arg` (`raise.zig:195-197`): raise an
/// exception of the given type with a single argument
/// (`newExcArg1`). Used by `modre.c` when regex compile/match
/// fails. `exc_name` is resolved by `exception_from_pending`
/// (`by_name`); `arg` becomes the exception's single arg.
pub fn raise_type_arg(ctx: &mut CallCtx, exc_name: &'static str, arg: Value) -> PyError {
    ctx.record_raise_value(exc_name, arg);
    PyError::PyRaise
}

/// `raiseBadCompileMode` (`builtinevex.zig:106`): `ValueError("bad
/// compile mode")` — the mode argument to `compile()` was not
/// `single` / `exec` / `eval`. Lives here (not in
/// `builtinevex.rs`) because `raise.zig` owns the raise helpers;
/// `builtinevex.rs` calls it.
pub fn raise_bad_compile_mode(ctx: &mut CallCtx) -> PyError {
    raise_value_error(ctx, Some("bad compile mode"))
}
