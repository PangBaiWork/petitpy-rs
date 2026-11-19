//! Protected calls + overflow predicates
//! (`runtime/runtime_utils.zig`, Zig port of vendor
//! `py/runtime_utils.c`).
//!
//! `mp_call_function_{1,2}_protected`: call, and on a polled
//! raise *print* the exception and return NULL — NO re-raise
//! (the same shape as `objgetitemiter`). The only live caller
//! in the original is the GC finaliser drain (`gc.zig:346`:
//! `mp_load_method_maybe(obj, __del__)` then
//! `mp_call_function_1_protected`); **the Rust VM does not
//! drain finalisers yet** (see the `GcArena` docs / the result
//! file), so these have no production caller today — they are
//! ported for the drain site, not for a caller that exists.
//!
//! `mp_mul_{ll,mp_int_t}_overflow`: pure predicates, no nlr.
//! `MICROPY_USE_GCC_MUL_OVERFLOW_INTRINSIC=0` (probed), so both
//! are compiled. C integer `/` is `@divTrunc` (Rust's `/` on
//! `i64` truncates the same way); all divisors are guarded
//! (`>0`, or `x != 0 &&`). **Header-vs-code discrepancy**: the
//! file header claims "the C source NEVER writes `*res`", but
//! the code writes `res[0] = x *% y` on the non-overflow path
//! in BOTH fns — code wins, so the Rust form returns the
//! product (`Some`) exactly when it does not overflow.
//!
//! `mp_call_function_{1,2}` are C14's call fns
//! (`runtime_call.rs:70-77`) — called directly, no shim. No
//! `dyn FnMut` call seam is needed: C14's fns take only
//! `&mut CallCtx`, and testability comes from the sink-taking
//! core fns below instead (the production wrappers collect
//! into a `String` and write it to stdout, ignoring errors —
//! the tree pattern, `obj_ops.rs:254`).

use std::io::Write as _;

use crate::memory::gc::CallCtx;
use crate::objects::obj_ops::obj_print_exception;
use crate::objects::objtype::objexcept::{new_exception, new_exception_args, new_exception_msg};
use crate::objects::value::Value;
use crate::pyerr::PyError;
use crate::runtime::runtime::RtError;
use crate::runtime::runtime_call::{call_function_1, call_function_2};
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::exc_from_pyerror;

/// Build the exception object for an `RtError` that arrived
/// with NOTHING pending (`runtime_utils`'s own converter —
/// `RtError` has no `TypedRaise` impl, and its `to_pyerror`
/// drops the name/message by design). Mirrors the arms of
/// `exc_from_gen_error` (`vm_ops.rs`) for the shapes an
/// `RtError` can carry: named message errors via
/// `by_name`/`new_exception_msg`, `Raised` is the object
/// itself, `StopIteration` keeps the bare-vs-arg distinction,
/// `OutOfMemory` is `MemoryError("out of memory")`, and a bare
/// `PyRaise` with nothing pending falls back to a bare
/// `TypeError` (the same fallback `exc_from_pyerror` uses).
fn rt_error_to_exc(ctx: &mut CallCtx, state: &VmState, e: RtError) -> Value {
    match e {
        RtError::Raised(v) => v,
        RtError::StopIteration(arg) => match arg {
            Some(v) => new_exception_args(ctx, state.exc.stop_iteration, &[v]),
            None => new_exception(ctx, state.exc.stop_iteration),
        },
        RtError::OutOfMemory => exc_from_pyerror(ctx, state, PyError::OutOfMemory),
        RtError::PyRaise => exc_from_pyerror(ctx, state, PyError::PyRaise),
        e => {
            let name = e.exc_name();
            let type_ = state.exc.by_name(name).unwrap_or(state.exc.type_error);
            new_exception_msg(ctx, type_, &e.message())
        }
    }
}

/// Print one exception into `sink` (`obj_print_exception`:
/// the "Traceback (most recent call last):" header, the cause
/// chain, the exception, a newline). `ctx.pool` is a `Copy`
/// shared ref, so it feeds the resolve closure without
/// borrowing `ctx`.
fn print_exc(ctx: &mut CallCtx, exc: Value, sink: &mut dyn crate::objects::slots::ByteSink) {
    let pool = ctx.pool;
    let types = ctx.types;
    obj_print_exception(ctx, types, exc, &|id| pool.qstr_str(id as u16), sink);
}

/// The shared outcome of both protected calls
/// (`runtime_utils.zig:38-49`): the Zig checks
/// `pyerr.pending_exc` AFTER the call, whatever the call
/// returned — a value returned with a raise pending is still
/// printed and dropped. With a raise pending, the pending
/// exception is printed (drained by `exc_from_pyerror`); a
/// bare `Err` with nothing pending is built from the `RtError`
/// and printed. Either way the result is `None` (the Zig's
/// `MP_OBJ_NULL`).
fn protected_outcome(
    ctx: &mut CallCtx,
    state: &VmState,
    result: Result<Value, RtError>,
    sink: &mut dyn crate::objects::slots::ByteSink,
) -> Option<Value> {
    if ctx.has_pending_raise() {
        let exc = exc_from_pyerror(ctx, state, PyError::PyRaise);
        print_exc(ctx, exc, sink);
        return None;
    }
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            let exc = rt_error_to_exc(ctx, state, e);
            print_exc(ctx, exc, sink);
            None
        }
    }
}

/// `mp_call_function_1_protected` (`runtime_utils.zig:40-45`,
/// testable core): call `fun(arg)`; on a raise print the
/// exception into `sink` and return `None`.
pub(crate) fn call_function_1_protected_sink(
    ctx: &mut CallCtx,
    state: &VmState,
    fun: Value,
    arg: Value,
    sink: &mut dyn crate::objects::slots::ByteSink,
) -> Option<Value> {
    let result = call_function_1(ctx, fun, arg);
    protected_outcome(ctx, state, result, sink)
}

/// `mp_call_function_2_protected` (`runtime_utils.zig:47-52`,
/// testable core): call `fun(arg1, arg2)`; on a raise print
/// the exception into `sink` and return `None`.
pub(crate) fn call_function_2_protected_sink(
    ctx: &mut CallCtx,
    state: &VmState,
    fun: Value,
    arg1: Value,
    arg2: Value,
    sink: &mut dyn crate::objects::slots::ByteSink,
) -> Option<Value> {
    let result = call_function_2(ctx, fun, arg1, arg2);
    protected_outcome(ctx, state, result, sink)
}

/// `mp_call_function_1_protected` — the production entry: the
/// printed exception goes to stdout (the Zig's
/// `mp_plat_print`), write errors ignored.
pub fn call_function_1_protected(
    ctx: &mut CallCtx,
    state: &VmState,
    fun: Value,
    arg: Value,
) -> Option<Value> {
    let mut buf = String::new();
    let result = call_function_1_protected_sink(ctx, state, fun, arg, &mut buf);
    if !buf.is_empty() {
        let _ = std::io::stdout().write_all(buf.as_bytes());
    }
    result
}

/// `mp_call_function_2_protected` — the production entry.
pub fn call_function_2_protected(
    ctx: &mut CallCtx,
    state: &VmState,
    fun: Value,
    arg1: Value,
    arg2: Value,
) -> Option<Value> {
    let mut buf = String::new();
    let result = call_function_2_protected_sink(ctx, state, fun, arg1, arg2, &mut buf);
    if !buf.is_empty() {
        let _ = std::io::stdout().write_all(buf.as_bytes());
    }
    result
}

// ── Overflow predicates ──────────────────────────────────────────────

/// `mp_mul_ll_overflow` (`runtime_utils.zig:54-72`): `Some(x*y)`
/// when the `c_longlong` product fits, `None` when it
/// overflows. On x86-64 `c_longlong` is `i64`; the predicate is
/// the Zig's verbatim (`LMAX`/`LMIN` = `i64::MAX`/`i64::MIN`,
/// `@divTrunc` = Rust's truncating `/`).
pub fn mul_ll_overflow(x: i64, y: i64) -> Option<i64> {
    if mul_overflows(x, y) {
        None
    } else {
        Some(x.wrapping_mul(y))
    }
}

/// `mp_mul_mp_int_t_overflow` (`runtime_utils.zig:74-90`): the
/// same predicate for `mp_int_t` (`isize`, `i64` on x86-64) —
/// kept as a separate fn mirroring the Zig's two shapes, even
/// though the two coincide on this platform.
pub fn mul_mp_int_t_overflow(x: i64, y: i64) -> Option<i64> {
    if mul_overflows(x, y) {
        None
    } else {
        Some(x.wrapping_mul(y))
    }
}

/// The shared predicate body (`runtime_utils.zig:59-70` /
/// `:79-88`): every division's divisor is guarded (`y > 0`,
/// `x > 0`, or `x != 0`), so no division can trap — in
/// particular `i64::MIN / -1` never occurs (`x > 0` in the
/// branch that divides `LMIN` by `x`, `y > 0` in the branch
/// that divides `LMIN` by `y`).
fn mul_overflows(x: i64, y: i64) -> bool {
    if x > 0 {
        // x is positive.
        if y > 0 {
            // x and y are positive.
            x > i64::MAX / y
        } else {
            // x positive, y nonpositive.
            y < i64::MIN / x
        }
    } else {
        // x is nonpositive.
        if y > 0 {
            // x nonpositive, y positive.
            x < i64::MIN / y
        } else {
            // x and y nonpositive.
            x != 0 && y < i64::MAX / x
        }
    }
}
