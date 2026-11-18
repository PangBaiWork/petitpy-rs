//! The `select` builtin module (`src/modules/modselect.zig`, 216 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modselect.c`):
//! only `select` — 2 entries (`__name__` + `select`).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `select(r, w, x[, timeout])` takes three lists of fd
//!   ints; fds `< 0` or `>= FD_SETSIZE` are silently skipped
//!   (the Zig's `fdSet` guard); `timeout` is `None` (block
//!   forever) or seconds (float or int); a negative timeout
//!   raises `OSError(22,)` (`EINVAL`); fewer than 3 args
//!   raises `TypeError("argument num/types mismatch")` (the
//!   sig is `(3, 4)` — the call machinery checks it).
//! - `select([], [], [], 0)` → `([], [], [])`.
//!
//! Implementation (pure `std` — there is NO `select(2)` /
//! `poll(2)` API in `std`):
//! - The empty-lists case is implemented exactly: with `nfds ==
//!   0`, `select(2)` returns after the timeout with three
//!   empty sets — so this port sleeps the timeout and returns
//!   three empty lists. (A `None` timeout sleeps effectively
//!   forever, as the reference blocks forever.)
//! - Any non-empty list raises `NotImplementedError`
//!   (`"select.select is not implemented (no std select API)"`,
//!   the D2 `os.kill` stub precedent). A listener-accept
//!   check would destructively consume pending connections
//!   and FIFO/stdin readiness is unknowable without the fd
//!   API — not attempted. The integration tests
//!   (`tests/test_integration_os.py:662+`) exercise exactly
//!   the empty-lists case plus real-fd cases; the real-fd
//!   cases are the documented gap.
//! - Divergences: a non-list argument (`select(1, [], [], 0)`)
//!   ABORTS the reference (`incorrect alignment` panic in
//!   `MpObjList.fromObj`, rc=134); Rust raises `TypeError`
//!   (`"select() argument must be a list"`). An infinite
//!   timeout aborts the reference (`@intFromFloat` panic);
//!   Rust sleeps `Duration::MAX`. A NaN timeout raises
//!   `OSError(EINVAL)` (same shape as `time.sleep`'s NaN).

use std::time::Duration;

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_float_maybe;
use crate::objects::primitives::ops::{is_list, PrimError};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

/// `EINVAL` (`modselect.zig`: negative timeout).
const EINVAL: i32 = 22;

/// The length of a list value (`None` when not a list —
/// the caller checks `is_list` first).
fn list_len(ctx: &CallCtx, v: Value) -> Option<usize> {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => Some(l.items.len()),
            _ => None,
        },
        _ => None,
    }
}

/// `py_select` (`modselect.zig`): `select(rlist, wlist,
/// xlist[, timeout])` — see the module docs for the exact
/// empty-lists case and the non-empty limitation.
fn select_select_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    for v in &args[..3] {
        if !is_list(ctx.arena, ctx.types, *v) {
            ctx.record_raise("TypeError", Some("select() argument must be a list"));
            return Err(PyError::PyRaise);
        }
    }
    let timeout = if args.len() > 3 { args[3] } else { Value::None };
    let secs = match timeout {
        Value::None => None,
        v => match obj_get_float_maybe(ctx, ctx.types, v) {
            Some(f) => Some(f),
            None => return Err(ctx.record_typed(PrimError::FloatConversion)),
        },
    };
    if let Some(f) = secs {
        if f < 0.0 || f.is_nan() {
            return Err(raise_os_error(ctx, EINVAL));
        }
    }
    if args[..3].iter().any(|v| list_len(ctx, *v).unwrap_or(0) > 0) {
        ctx.record_raise(
            "NotImplementedError",
            Some("select.select is not implemented (no std select API)"),
        );
        return Err(PyError::PyRaise);
    }
    // Empty lists: `select(2)` with `nfds == 0` returns after
    // the timeout with three empty sets — sleep the timeout
    // (`None` = forever, `Duration::MAX`).
    match secs {
        Some(f) if f.is_finite() => std::thread::sleep(Duration::from_secs_f64(f)),
        _ => std::thread::sleep(Duration::MAX),
    }
    // Three DISTINCT empty lists (one shared list would
    // alias mutations across the three result positions).
    let r = Value::Heap(new_list(ctx, ctx.types.list, &[]));
    let w = Value::Heap(new_list(ctx, ctx.types.list, &[]));
    let x = Value::Heap(new_list(ctx, ctx.types.list, &[]));
    let tuple = Value::Heap(new_tuple(ctx, ctx.types.tuple, &[r, w, x]));
    Ok(tuple)
}

/// `build_select_module` (`modselect.zig` table): 2 entries.
pub fn build_select_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let (module, dict) = new_d4_module(ctx, "select", module_type);
    let select = mk_fun(ctx, fun_builtin_var, "select", 3, 4, select_select_builtin, None);
    store(ctx, dict, "select", select);
    fix_dict(ctx, dict);
    Ok(module)
}
