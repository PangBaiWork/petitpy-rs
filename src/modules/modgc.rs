//! The `gc` builtin module (`src/modules/modgc.zig`, 182 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modgc.c`):
//! `collect`, `disable`, `enable`, `isenabled`, `mem_free`,
//! `mem_alloc` — 7 entries (`__name__` + 6). There is NO
//! `threshold` entry: the Zig gates it on
//! `MICROPY_GC_ALLOC_THRESHOLD`, which is
//! `ROM_LEVEL_AT_LEAST_CORE_FEATURES`, but the port sets
//! `MICROPY_CONFIG_ROM_LEVEL` to `MINIMUM`
//! (`port/mpconfigport.h:35`) — so the reference binary has
//! no `threshold` attribute (probed 2026-11-18:
//! `AttributeError: no attribute 'threshold'`; the
//! `modgc.zig` header comment claiming "full → 1" is wrong
//! for the actual build config).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `gc.collect()` ALWAYS returns `0` (the bumping
//!   collector never sets `gc_collected`); the collection
//!   itself still runs (and is skipped while the GC is
//!   locked — `GcArena::collect`).
//! - `disable()` / `enable()` poke the auto-collect flag
//!   (`gc_auto_collect_enabled`, default on — C4); there is
//!   NO threshold rule (C4 proved it).
//! - `mem_alloc()` / `mem_free()` read `gc.info().used` /
//!   `.free` (the arena's live bytes).

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::PyResult;

/// `py_gc_collect` (`modgc.zig`): run the collector, return 0.
fn gc_collect_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx.arena.collect(ctx.roots);
    Ok(Value::SmallInt(0))
}

/// `py_gc_disable` (`modgc.zig`): clear `gc_auto_collect_enabled`.
fn gc_disable_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.arena.set_auto_collect_enabled(false);
    Ok(Value::None)
}

/// `py_gc_enable` (`modgc.zig`): set `gc_auto_collect_enabled`.
fn gc_enable_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.arena.set_auto_collect_enabled(true);
    Ok(Value::None)
}

/// `py_gc_isenabled` (`modgc.zig`).
fn gc_isenabled_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::Bool(ctx.arena.is_auto_collect_enabled()))
}

/// `py_gc_mem_free` (`modgc.zig`): `gc_info().free`.
fn gc_mem_free_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(ctx.arena.info().free as i64))
}

/// `py_gc_mem_alloc` (`modgc.zig`): `gc_info().used`.
fn gc_mem_alloc_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(ctx.arena.info().used as i64))
}

/// `build_gc_module` (`modgc.zig` table, full build): 7
/// entries — NO `threshold` (see the module docs). Returns
/// `(module, collect_fn)`: the `collect` `FunBuiltin` is
/// captured by `VmState.gc_collect_builtin` so `gc.collect()`
/// can be intercepted (F6 — the finaliser drain needs full VM
/// state; see `call_gc_collect_ctx`).
pub fn build_gc_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, Value)> {
    let (module, dict) = new_d4_module(ctx, "gc", module_type);
    let collect = mk_fun(ctx, fun_builtin_var, "collect", 0, 0, gc_collect_builtin, None);
    let disable = mk_fun(ctx, fun_builtin_var, "disable", 0, 0, gc_disable_builtin, None);
    let enable = mk_fun(ctx, fun_builtin_var, "enable", 0, 0, gc_enable_builtin, None);
    let isenabled = mk_fun(ctx, fun_builtin_var, "isenabled", 0, 0, gc_isenabled_builtin, None);
    let mem_free = mk_fun(ctx, fun_builtin_var, "mem_free", 0, 0, gc_mem_free_builtin, None);
    let mem_alloc = mk_fun(ctx, fun_builtin_var, "mem_alloc", 0, 0, gc_mem_alloc_builtin, None);
    store(ctx, dict, "collect", collect);
    store(ctx, dict, "disable", disable);
    store(ctx, dict, "enable", enable);
    store(ctx, dict, "isenabled", isenabled);
    store(ctx, dict, "mem_free", mem_free);
    store(ctx, dict, "mem_alloc", mem_alloc);
    fix_dict(ctx, dict);
    Ok((module, collect))
}
