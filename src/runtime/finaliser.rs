//! GC finaliser drain (F6, ADR 019, `src/memory/gc.zig` `runFinalisers`).
//!
//! The arena's `collect()` lists unmarked finaliser-flagged objects in
//! `pending_finalisers` (flag cleared, marked live for that collection so
//! `__del__` still sees an intact object). The VM drains the list here,
//! after an explicit `gc.collect()` — see `call_gc_collect_ctx` in
//! `builtin_vm.rs`: `gc.collect` is intercepted by GcRef identity so the
//! drain runs with full VM state (the plain `gc_collect_builtin` only
//! sees a `CallCtx`, which cannot materialise a `__del__` raise for the
//! protected print).
//!
//! For each pending object, in slot order (gc.zig's FTB walk order):
//! load `__del__` (`mp_load_method_maybe`), call it with no args
//! through the normal call path, then free the object. A raise is
//! protected, exactly as gc.zig's `runFinalisers` (locked GC,
//! `pending_exc` saved and restored, the finaliser call neither sees
//! nor clears it): the recorded raise is saved around the drain, a
//! `__del__` raise is printed with the canonical
//! `obj_print_exception` (to fd 1, through the embedding sink
//! when installed — the protected-call shape, NOT the
//! uncaught-traceback fd-2 shape; see `print_raised` below)
//! and swallowed, never raised. A lookup
//! failure is skipped silently (the vendor's `dest[0] == 0` case;
//! its defer restores the saved pending exception, discarding any
//! lookup raise). Only native types allocate with the flag (files
//! from `open()`, sockets); user classes' `__del__` is never called,
//! as in the original.

use crate::memory::gc::CallCtx;
use crate::objects::obj_ops::obj_print_exception;
use crate::objects::value::Value;
use crate::runtime::import_search::rt_error_to_value;
use crate::runtime::runtime::{dunder, RtError};
use crate::runtime::runtime_call::call_method_n_kw;
use crate::runtime::runtime_method::load_method_maybe;
use crate::runtime::vm::VmState;
use crate::runtime::vm_rt_iter::drain_seam_raise;

/// Map a `RtError` from the `__del__` call onto the exception object
/// (the `builtin_vm.rs` `rt_to_value` convention: `PyRaise` drains the
/// seam stash, anything else goes through `rt_error_to_value`).
fn rt_to_value(ctx: &mut CallCtx, state: &mut VmState, e: RtError) -> Value {
    match e {
        RtError::PyRaise => drain_seam_raise(ctx, state),
        other => rt_error_to_value(ctx, state, other),
    }
}

/// Print a `__del__` raise with the canonical exception printer, to
/// fd 1 through the embedding sink when one is installed — the
/// original's protected call prints via `mp_obj_print_exception`
/// with `mp_plat_print` (`runtime_utils.zig:41-48`), which routes
/// to `output.write(1, ...)` (`vendor_stubs.zig:22-24`); the
/// uncaught-traceback shape (`mp_stderr_print`, fd 2) is a
/// different path and does NOT apply here. Without a sink, plain
/// `write(1, ...)` (stdout, flushed first so piped output stays
/// in order — the `print_traceback` ordering, destination
/// corrected).
fn print_raised(ctx: &mut CallCtx, exc: Value) {
    use std::io::Write as _;
    let mut out = String::new();
    let pool = ctx.pool;
    let types = ctx.types;
    obj_print_exception(ctx, types, exc, &|id| pool.qstr_str(id as u16), &mut out);
    let _ = std::io::stdout().flush();
    if crate::support::output::write_sink(1, out.as_bytes()) {
        return;
    }
    let _ = std::io::stdout().write_all(out.as_bytes());
}

/// Drain the arena's pending-finaliser list: run each object's
/// `__del__` (protected), then free it. No-op when the list is empty.
pub fn drain_pending_finalisers(ctx: &mut CallCtx, state: &mut VmState) {
    let pending = ctx.arena.take_pending_finalisers();
    if pending.is_empty() {
        return;
    }
    // gc.zig `runFinalisers`: a collection can start while an
    // exception is pending; the finaliser call must neither see nor
    // clear it — save it and restore it at the end. Anything the
    // `__del__` calls record is consumed by the print path (or
    // cleared below) and never leaks.
    let saved = ctx.take_pending_raise();
    // No collection from inside a finaliser (gc.zig bumps
    // `lock_depth`); blocks allocated by a finaliser are therefore
    // never collected mid-drain. A re-entrant `gc.collect()` from
    // inside `__del__` sees the lock and returns 0 with an empty
    // pending list.
    ctx.arena.lock();
    let del_q = dunder(b"__del__").expect("__del__ is a static qstr");
    for r in pending {
        let obj = Value::Heap(r);
        match load_method_maybe(ctx, obj, del_q, Value::None) {
            Ok(Some(dest)) => {
                if let Err(e) = call_method_n_kw(ctx, 0, 0, &dest, &[]) {
                    let exc = rt_to_value(ctx, state, e);
                    print_raised(ctx, exc);
                }
            }
            Ok(None) => {}
            // Lookup raise: the vendor skips silently (its defer
            // restores the saved pending exception); clear whatever
            // the lookup recorded so it cannot leak.
            Err(_) => {}
        }
        // A `__del__` call must not leave a recorded raise behind
        // (the print path consumes it, but be explicit — the saved
        // raise is restored below, nothing else may survive).
        let _ = ctx.take_pending_raise();
        ctx.arena.free(r);
    }
    ctx.arena.unlock();
    if let Some(pr) = saved {
        *ctx.pending_raise = Some(pr);
    }
}
