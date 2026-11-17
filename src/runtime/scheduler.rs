//! Pending-exception scheduler (`runtime/scheduler.zig`, 110 lines).
//!
//! Under nano the live remainder is ~30 lines (file header): the
//! pending-exception slot, `mp_sched_exception` (set, no raise),
//! `mp_handle_pending_internal` (the ONLY raise path) and the
//! `mp_event_handle_*` thin wrappers. `SCHEDULER=0 + PY_THREAD=0`
//! blocks are dead; `VM_ABORT=0`, `KBD_EXCEPTION=0` blocks are
//! dead; `MICROPY_BEGIN/END_ATOMIC_SECTION` are no-ops (the Zig
//! keeps a dead `atomic_state` local — Rust has no equivalent).
//!
//! The slot is thread state: in the Zig it is a module-level `var`
//! (`pending_exception`, `scheduler.zig:33`); in Rust it is
//! `VmState.pending_sched: Option<Value>` (`None` = the Zig's
//! `MP_OBJ_NULL` = no pending exception). NO global static.
//!
//! Behaviour codes (`py/runtime.h:55-59`, C enum from 0):
//! `MP_HANDLE_PENDING_CALLBACKS_ONLY = 0`,
//! `MP_HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS = 1`,
//! `MP_HANDLE_PENDING_CALLBACKS_AND_CLEAR_EXCEPTIONS = 2`.

use crate::objects::value::Value;
use crate::runtime::vm::VmState;

/// `MP_HANDLE_PENDING_CALLBACKS_ONLY` (`runtime.h:56`).
pub const HANDLE_PENDING_CALLBACKS_ONLY: i32 = 0;
/// `MP_HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS` (`runtime.h:57`).
pub const HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS: i32 = 1;
/// `MP_HANDLE_PENDING_CALLBACKS_AND_CLEAR_EXCEPTIONS`
/// (`runtime.h:58`).
pub const HANDLE_PENDING_CALLBACKS_AND_CLEAR_EXCEPTIONS: i32 = 2;

/// `npy_sch_get_pending` (`scheduler.zig:36`): the pending
/// exception object, or `None` when the slot is empty.
pub fn sched_get_pending(state: &VmState) -> Option<Value> {
    state.pending_sched
}

/// `npy_sch_set_pending` (`scheduler.zig:40`).
pub fn sched_set_pending(state: &mut VmState, exc: Value) {
    state.pending_sched = Some(exc);
}

/// `npy_sch_clear_pending` (`scheduler.zig:44`).
pub fn sched_clear_pending(state: &mut VmState) {
    state.pending_sched = None;
}

/// `mp_sched_exception` (`scheduler.zig:48`): set the pending
/// exception (no raise). The `SCHEDULER=0 + PY_THREAD=0` sched-state
/// optimisation block is dead.
pub fn sched_exception(state: &mut VmState, exc: Value) {
    sched_set_pending(state, exc);
}

/// `mpHandlePendingInternalImpl` (`scheduler.zig:57`): check the
/// pending slot and optionally raise it. The ONLY raise path in the
/// file. The slot is cleared BEFORE the raise (the Zig clears it
/// inside the atomic section, then raises — `scheduler.zig:65-72`).
///
/// Returns `Err(Some(obj))` on raise (the exception OBJECT — the
/// caller raises it; the Zig sets `pyerr.pending_exc` and the VM's
/// dispatch loop injects it). The object does NOT also go into
/// `state.pending_exc`: the caller owns it from here (see
/// `pending_exc_check`).
///
/// * behavior `0` (`CALLBACKS_ONLY`): callbacks only — there are no
///   callbacks (`SCHEDULER=0`), so this is a no-op.
/// * behavior `1` (`CALLBACKS_AND_EXCEPTIONS`): raise the pending
///   exception.
/// * behavior `2` (`CALLBACKS_AND_CLEAR_EXCEPTIONS`): clear the
///   pending exception without raising.
/// * Any other behavior: the Zig's `handle_exceptions` is true
///   (behavior != 0) and `raise_exceptions` is false (behavior !=
///   1) — the slot is cleared without raising.
pub fn handle_pending(
    state: &mut VmState,
    behavior: i32,
) -> Result<(), Option<Value>> {
    let handle_exceptions = behavior != HANDLE_PENDING_CALLBACKS_ONLY;
    let raise_exceptions = behavior == HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS;
    if handle_exceptions {
        if let Some(obj) = sched_get_pending(state) {
            sched_clear_pending(state);
            if raise_exceptions {
                return Err(Some(obj));
            }
        }
    }
    Ok(())
}

/// `pendingExcCheck` (`vm.zig:110-114`, Tier G): the dispatch
/// loop's per-iteration poll — if the scheduler pending slot is
/// non-null, handle it with behavior 1
/// (`CALLBACKS_AND_EXCEPTIONS`). Returns the exception object to
/// raise, or `None` when nothing is pending. (C13's dispatch loop
/// did not port this poll; the wiring lives in `vm_dispatch.rs`.)
pub fn pending_exc_check(state: &mut VmState) -> Option<Value> {
    if sched_get_pending(state).is_some() {
        match handle_pending(state, HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS) {
            Ok(()) => None,
            Err(obj) => obj,
        }
    } else {
        None
    }
}

/// `mp_event_handle_nowait` (`scheduler.zig:99`): under nano this
/// is a single `mp_handle_pending(true)` — i.e. behavior 1.
pub fn event_handle_nowait(state: &mut VmState) -> Result<(), Option<Value>> {
    handle_pending(state, HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS)
}

/// `mp_event_wait_indefinite` (`scheduler.zig:105`): `nowait()`
/// plus `MICROPY_INTERNAL_WFE(-1)` which is `(void)0`.
pub fn event_wait_indefinite(state: &mut VmState) -> Result<(), Option<Value>> {
    event_handle_nowait(state)
}

/// `mp_event_wait_ms` (`scheduler.zig:109`): same as nowait
/// (the wait duration is ignored — WFE is a no-op).
pub fn event_wait_ms(state: &mut VmState, _ms: usize) -> Result<(), Option<Value>> {
    event_handle_nowait(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::vm::Vm;

    fn exc_value() -> Value {
        // Any heap value stands in for an exception object in
        // these slot tests (the scheduler never inspects it).
        Value::SmallInt(7)
    }

    #[test]
    fn pending_slot_set_get_clear() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        assert_eq!(sched_get_pending(state), None);
        sched_exception(state, exc_value());
        assert_eq!(sched_get_pending(state), Some(exc_value()));
        sched_clear_pending(state);
        assert_eq!(sched_get_pending(state), None);
    }

    #[test]
    fn handle_pending_callbacks_only_is_noop() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        sched_exception(state, exc_value());
        assert!(handle_pending(state, HANDLE_PENDING_CALLBACKS_ONLY).is_ok());
        // Still pending: behavior 0 neither raises nor clears.
        assert_eq!(sched_get_pending(state), Some(exc_value()));
    }

    #[test]
    fn handle_pending_raises_and_clears() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        sched_exception(state, exc_value());
        let r = handle_pending(state, HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS);
        assert_eq!(r, Err(Some(exc_value())));
        // Cleared before the raise (Zig: clear inside the atomic
        // section, then raise).
        assert_eq!(sched_get_pending(state), None);
    }

    #[test]
    fn pending_exc_check_polls_and_raises() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        assert_eq!(pending_exc_check(state), None);
        sched_exception(state, exc_value());
        assert_eq!(pending_exc_check(state), Some(exc_value()));
        // Consumed: a second poll is quiet.
        assert_eq!(pending_exc_check(state), None);
    }

    #[test]
    fn handle_pending_clear_without_raise() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        sched_exception(state, exc_value());
        assert!(handle_pending(
            state,
            HANDLE_PENDING_CALLBACKS_AND_CLEAR_EXCEPTIONS
        )
        .is_ok());
        assert_eq!(sched_get_pending(state), None);
        assert_eq!(state.pending_exc, None);
    }

    #[test]
    fn handle_pending_empty_slot_is_noop() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        assert!(handle_pending(state, HANDLE_PENDING_CALLBACKS_AND_EXCEPTIONS).is_ok());
        assert_eq!(state.pending_exc, None);
    }

    #[test]
    fn event_wrappers_raise_like_nowait() {
        let mut vm = Vm::new();
        let (_, state) = vm.split();
        sched_exception(state, exc_value());
        assert_eq!(
            event_handle_nowait(state),
            Err(Some(exc_value()))
        );
        sched_exception(state, exc_value());
        assert_eq!(
            event_wait_indefinite(state),
            Err(Some(exc_value()))
        );
        sched_exception(state, exc_value());
        assert_eq!(event_wait_ms(state, 500), Err(Some(exc_value())));
    }
}
