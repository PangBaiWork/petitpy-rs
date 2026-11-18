//! The `time` builtin module (`src/modules/modtime.zig`, 236 lines).
//!
//! Ported from the Zig (itself `clock_gettime(2)` / `nanosleep(2)`
//! wrappers): `time`, `monotonic`, `sleep`, `time_ns`,
//! `monotonic_ns`, `perf_counter` (the SAME `FunBuiltin` object as
//! `monotonic` — CPython says so, `modtime.zig` table), `sleep_ms`,
//! `ticks_ms`, `ticks_diff`.
//!
//! Faithful points (all probed on the reference binary 2026-11-18,
//! see `scripts/d4_result.txt` §2):
//! - `time()` / `monotonic()` are floats; `time_ns()` /
//!   `monotonic_ns()` / `ticks_ms()` are ints.
//! - `ticks_ms` is the monotonic clock in milliseconds (the full
//!   64-bit value — the Zig returns it un-wrapped, `modtime.zig`
//!   comment); `ticks_diff(a, b)` is `a - b`.
//! - `sleep(-1)` → `OSError(22,)` (`EINVAL`); `sleep("x")` →
//!   `TypeError("can't convert to float")`; `sleep(float("nan"))`
//!   → `OSError(22,)` (the Zig's `nanosleep` rejects the NaN
//!   timespec); `sleep_ms(-5)` → `None` (never negative);
//!   `sleep_ms("x")` → `TypeError("can't convert to int")`
//!   (a float is NOT convertible — `sleep_ms(1.5)` raises too).
//! - Divergences (the reference aborts, Rust does not): `sleep(inf)`
//!   aborts the reference (`@intFromFloat` panic); Rust sleeps
//!   `Duration::MAX` (effectively forever, same observable
//!   non-return).

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::obj::{obj_get_float_maybe, obj_get_int};
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::ops::PrimError;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

/// `EINVAL` (`modtime.zig`: `sleep` on a negative timeout).
const EINVAL: i32 = 22;

/// The monotonic epoch (`CLOCK_MONOTONIC`'s fixed origin —
/// the first call fixes it, matching `clock_gettime`'s
/// boot-relative clock).
static MONO_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Elapsed time since the monotonic epoch.
fn monotonic_elapsed() -> Duration {
    MONO_EPOCH.get_or_init(Instant::now).elapsed()
}

/// `mp_obj_get_float` (`objfloat.c`, the `modmath.rs`
/// `get_float` shape): int/bool/float → `f64`; anything else
/// is `TypeError("can't convert to float")`.
fn get_float(ctx: &mut CallCtx, v: Value) -> Result<f64, PyError> {
    match obj_get_float_maybe(ctx, ctx.types, v) {
        Some(f) => Ok(f),
        None => Err(ctx.record_typed(PrimError::FloatConversion)),
    }
}

/// `mp_obj_get_int` (`obj.zig`): int/bool → `i64` (a float
/// is NOT convertible); anything else is `TypeError("can't
/// convert to int")`.
fn get_int(ctx: &mut CallCtx, v: Value) -> Result<i64, PyError> {
    obj_get_int(ctx, ctx.types, v).map_err(|e| ctx.record_typed(e))
}

/// An int value (small int in-window, heap `Int` out of it —
/// `Value::int`, the `time_ns` shape: the value fits the
/// 62-bit window for realistic clocks, but the heap path
/// keeps it correct past it).
fn int_value(ctx: &mut CallCtx, n: i64) -> Value {
    Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, n)
}

/// `py_time` (`modtime.zig`): `CLOCK_REALTIME` seconds as a float.
fn time_time_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    Ok(new_float(ctx, ctx.types, d.as_secs_f64()))
}

/// `py_monotonic` (`modtime.zig`): `CLOCK_MONOTONIC` seconds
/// as a float.
fn time_monotonic_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(new_float(ctx, ctx.types, monotonic_elapsed().as_secs_f64()))
}

/// `py_sleep` (`modtime.zig`): sleep `secs` seconds (float
/// or int); negative or NaN → `OSError(EINVAL)`; infinite
/// sleeps `Duration::MAX` (the reference aborts on inf —
/// see the module docs).
fn time_sleep_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let secs = get_float(ctx, args[0])?;
    if secs < 0.0 || secs.is_nan() {
        return Err(raise_os_error(ctx, EINVAL));
    }
    let dur = if secs.is_infinite() {
        Duration::MAX
    } else {
        Duration::from_secs_f64(secs)
    };
    std::thread::sleep(dur);
    Ok(Value::None)
}

/// `py_time_ns` (`modtime.zig`): `CLOCK_REALTIME` nanoseconds.
fn time_time_ns_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let ns = (d.as_secs() as i128) * 1_000_000_000 + d.subsec_nanos() as i128;
    Ok(int_value(ctx, ns as i64))
}

/// `py_monotonic_ns` (`modtime.zig`): `CLOCK_MONOTONIC`
/// nanoseconds.
fn time_monotonic_ns_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(int_value(ctx, monotonic_elapsed().as_nanos() as i64))
}

/// `py_sleep_ms` (`modtime.zig`): sleep `n` milliseconds;
/// never negative (`n <= 0` returns `None` immediately).
fn time_sleep_ms_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let n = get_int(ctx, args[0])?;
    if n <= 0 {
        return Ok(Value::None);
    }
    std::thread::sleep(Duration::from_millis(n as u64));
    Ok(Value::None)
}

/// `py_ticks_ms` (`modtime.zig`): monotonic milliseconds.
fn time_ticks_ms_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(int_value(ctx, monotonic_elapsed().as_millis() as i64))
}

/// `py_ticks_diff` (`modtime.zig`): `a - b` (wrapping — the
/// Zig's `a - b` panics on overflow in its Debug build; the
/// wrap is the only reachable behaviour for in-window ints).
fn time_ticks_diff_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let a = get_int(ctx, args[0])?;
    let b = get_int(ctx, args[1])?;
    Ok(int_value(ctx, a.wrapping_sub(b)))
}

/// `build_time_module` (`modtime.zig` table): 10 entries
/// (`__name__` + 9). `perf_counter` is the SAME `FunBuiltin`
/// object as `monotonic` (the Zig stores
/// `&mp_time_monotonic_obj` under both keys).
pub fn build_time_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let (module, dict) = new_d4_module(ctx, "time", module_type);
    // Bind each value before its `store` — `store(ctx, dict,
    // name, mk_fun(ctx, ...))` double-borrows `ctx` (the D2
    // `modos.rs` note).
    let time_fn = mk_fun(ctx, fun_builtin_var, "time", 0, 0, time_time_builtin, None);
    let monotonic = mk_fun(ctx, fun_builtin_var, "monotonic", 0, 0, time_monotonic_builtin, None);
    let sleep = mk_fun(ctx, fun_builtin_var, "sleep", 1, 1, time_sleep_builtin, None);
    let time_ns = mk_fun(ctx, fun_builtin_var, "time_ns", 0, 0, time_time_ns_builtin, None);
    let monotonic_ns = mk_fun(ctx, fun_builtin_var, "monotonic_ns", 0, 0, time_monotonic_ns_builtin, None);
    let sleep_ms = mk_fun(ctx, fun_builtin_var, "sleep_ms", 1, 1, time_sleep_ms_builtin, None);
    let ticks_ms = mk_fun(ctx, fun_builtin_var, "ticks_ms", 0, 0, time_ticks_ms_builtin, None);
    let ticks_diff = mk_fun(ctx, fun_builtin_var, "ticks_diff", 2, 2, time_ticks_diff_builtin, None);
    store(ctx, dict, "time", time_fn);
    store(ctx, dict, "monotonic", monotonic);
    store(ctx, dict, "sleep", sleep);
    store(ctx, dict, "time_ns", time_ns);
    store(ctx, dict, "monotonic_ns", monotonic_ns);
    store(ctx, dict, "perf_counter", monotonic);
    store(ctx, dict, "sleep_ms", sleep_ms);
    store(ctx, dict, "ticks_ms", ticks_ms);
    store(ctx, dict, "ticks_diff", ticks_diff);
    fix_dict(ctx, dict);
    Ok(module)
}
