//! The unified VM runner seam (Lane C13).
//!
//! C10a (`objfun.rs`) and C10b (`objgenerator.rs`) each grew their
//! own local runner seam — the VM executes bytecode, and the
//! callables need a way to call back into it without depending on
//! the VM (which would be a cycle: `runtime::vm` depends on
//! `objects::callables`, and the callables cannot depend on
//! `runtime::vm`). C13 unifies them here, in `runtime`, where both
//! sides can see it:
//!
//! - `RunOutcome` is the C10b shape (`Normal` / `Yield` /
//!   `Exception`): the C10a `NeedsVm` variant is dropped — the real
//!   VM never returns it (it was the "the slot has no runner"
//!   sentinel; the VM always has one).
//! - `VmRunner` is the C10b 4-argument shape: the frame, the value
//!   to deliver as the pending `yield` (or `None` for a fresh call),
//!   and the exception to inject (`None` for a plain resume). The
//!   C10a 2-argument runners call it with `send = None`,
//!   `throw = None`.
//!
//! The `Vm` implements this runner as a closure over its state
//! (see `vm.rs`); `fun_bc_call` / `bound_meth_call` /
//! `closure_call` / `gen_resume` all take it. The C10a
//! `RunOutcome` in `objfun.rs` and the C10b one in
//! `objgenerator.rs` are deleted — both modules import from here.

use crate::memory::gc::CallCtx;
use crate::objects::value::Value;
use crate::runtime::bc::CodeState;

/// What one `execute` step returns (`vm.zig:1191-1226`'s
/// `MP_VM_RETURN_*`): the function returned normally with this
/// value, yielded this value (suspended), or raised this exception
/// object.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// `MP_VM_RETURN_NORMAL` — `statePtr()[0]` after the run.
    Normal(Value),
    /// `MP_VM_RETURN_YIELD` — the yielded value; the frame is
    /// suspended at the `YIELD_VALUE` / `YIELD_FROM` opcode.
    Yield(Value),
    /// `MP_VM_RETURN_EXCEPTION` — the raised exception object.
    Exception(Value),
}

/// The runner the VM hands to callables: execute `frame` (already
/// set up), delivering `send` as the pending `yield`'s value
/// (`Value::None` for a fresh call — the Zig's `sp[0] =
/// send_value` overwrite, which only fires when the frame stack
/// is non-empty) and injecting `throw` (`Some(exc)` raises it at
/// the resume point; `None` for a plain resume). The runner owns
/// the value stack and the globals save/restore (see `vm.rs`).
///
/// Two lifetimes: `'a` is the `CallCtx` the runner accepts (the
/// caller's borrow — long in the VM's execute loop, short in a
/// test's per-call context); `'b` is the runner closure itself
/// (its captures — usually just the call, so callers leave it
/// elided). They must be distinct: tying the runner's own
/// lifetime to the ctx's would force a test runner capturing a
/// short-lived value to outlive the (longer-lived) context.
pub type VmRunner<'a, 'b> =
    dyn FnMut(&mut CallCtx<'a>, &mut CodeState, Value, Option<Value>) -> RunOutcome + 'b;
