//! The `micropython` builtin module (`src/modules/modmicropython.zig`,
//! 122 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modmicropython.c`
//! + the Zig's own `heap_lock` / `heap_unlock`): `const`,
//! `opt_level`, `heap_lock`, `heap_unlock` — 5 entries
//! (`__name__` + 4).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `const(x)` is the identity function (an ordinary
//!   1-arg builtin — the optimiser folds it at compile time
//!   in CPython; here it is a runtime identity, like the
//!   Zig's `mp_const_obj`).
//! - `opt_level()` returns the compile-time optimisation level
//!   (default `0`, the `mp_optimise_value` global); `opt_level(n)`
//!   sets it and returns `None`. It is an intercepted builtin
//!   (needs `state.d4.optimise_value` — the D2 shape).
//! - `heap_lock()` locks the GC arena (`gc_lock`) and returns
//!   `None`; `heap_unlock()` unlocks (`gc_unlock`) and returns
//!   **always `0`** — the Zig returns
//!   `state.threadState().gc_lock_depth` (`mpstate.zig:150`),
//!   a field that is NEVER written anywhere (the real counter
//!   is the module-level `lock_depth` in `gc.zig:130`), so the
//!   return value is a constant 0 (probed: lock + unlock → 0,
//!   double lock + two unlocks → 0, 0).
//! - Divergence: an unmatched `heap_unlock()` ABORTS the
//!   reference (integer-overflow panic in `gc.zig:199` when
//!   `lock_depth` underflows); Rust's `GcArena::unlock`
//!   panics the same way (`checked_sub` + expect) — the
//!   same abort class, so no e2e test calls unlock-at-0.

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::obj::obj_get_int;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};

/// `py_const` (`modmicropython.zig`): the identity function.
fn micropython_const_builtin(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[0])
}

/// `py_heap_lock` (`modmicropython.zig`): `gc_lock`, return `None`.
fn micropython_heap_lock_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.arena.lock();
    Ok(Value::None)
}

/// `py_heap_unlock` (`modmicropython.zig`): `gc_unlock`,
/// return the (always 0) `threadState().gc_lock_depth` —
/// see the module docs.
fn micropython_heap_unlock_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.arena.unlock();
    Ok(Value::SmallInt(0))
}

/// The stub `func` for the intercepted `opt_level` builtin —
/// a bare raise, like D2's `fd_stub_fn` (the interception in
/// `call_function_ctx` handles the real calls).
fn opt_level_stub_fn(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// The arity check the intercepted body needs
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

/// `py_opt_level` (`modmicropython.zig`): `opt_level()` reads
/// `mp_optimise_value`; `opt_level(n)` sets it (an int —
/// anything else is `TypeError("can't convert to int")`) and
/// returns `None`. A negative level wraps (`as usize` — the
/// reference aborts on `@intCast(usize)`; the wrap
/// round-trips through the read path).
pub fn call_opt_level_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 0, 1)?;
    if n_args == 0 {
        return Ok(Value::SmallInt(state.d4.optimise_value as i64));
    }
    let level = match obj_get_int(ctx, ctx.types, args[0]) {
        Ok(n) => n,
        Err(e) => {
            let pe = ctx.record_typed(e);
            return Err(exc(ctx, state, pe));
        }
    };
    state.d4.optimise_value = level as usize;
    Ok(Value::None)
}

/// `build_micropython_module` (`modmicropython.zig` table):
/// 5 entries. Records the intercepted `opt_level` builtin
/// on `state.d4`.
pub fn build_micropython_module(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let (module, dict) = new_d4_module(ctx, "micropython", module_type);
    let const_fn = mk_fun(ctx, fun_builtin_var, "const", 1, 1, micropython_const_builtin, None);
    let opt_level = mk_fun(ctx, fun_builtin_var, "opt_level", 0, 1, opt_level_stub_fn, None);
    let heap_lock = mk_fun(ctx, fun_builtin_var, "heap_lock", 0, 0, micropython_heap_lock_builtin, None);
    let heap_unlock = mk_fun(ctx, fun_builtin_var, "heap_unlock", 0, 0, micropython_heap_unlock_builtin, None);
    state.d4.opt_level_builtin = opt_level;
    store(ctx, dict, "const", const_fn);
    store(ctx, dict, "opt_level", opt_level);
    store(ctx, dict, "heap_lock", heap_lock);
    store(ctx, dict, "heap_unlock", heap_unlock);
    fix_dict(ctx, dict);
    module
}
