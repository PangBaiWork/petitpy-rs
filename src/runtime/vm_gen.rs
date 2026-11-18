//! Generator resume — the `gen_resume` seam (`runtime.zig:1363-1456`
//! via `objgenerator.zig:257-314`): take a suspended generator frame
//! out of `VmState.frames`, run it through the VM runner, and put it
//! back iff it yields. Split out of `vm_ops.rs` (file-size rule).
//!
//! `resume_generator_ctx` is the `iternext` form (exhaustion is
//! `Ok(None)`); `resume_generator_value_ctx` is the `send`/`throw`
//! form (exhaustion raises a real `StopIteration(value)` object);
//! `close_generator_ctx` is `gen.close()`.

use crate::memory::gc::CallCtx;
use crate::objects::callables::objgenerator::{gen_instance_close, gen_resume_and_raise, GenError};
use crate::objects::objtype::objexcept::{new_exception, new_exception_args};
use crate::objects::value::{Obj, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::runner::RunOutcome;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::exc_from_gen_error;
use crate::runtime::vm_raise::execute_frame;

// ── Generator resume ─────────────────────────────────────────────────

/// Resume a generator through `gen_resume_and_raise`
/// (`objgenerator.zig:257-276`): take the frame out of the map
/// (so the runner closure can re-borrow the state), run it,
/// put it back iff it yields. `raise_stop_iteration` selects
/// the `send`/`throw` form (exhaustion raises
/// `StopIteration(value)`) vs the `iternext` form (exhaustion
/// is `Ok(None)`).
pub fn resume_generator_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    gen: Value,
    send: Value,
    throw: Option<Value>,
    raise_stop_iteration: bool,
) -> Result<Option<Value>, GenError> {
    let (result, _yielded) = run_gen_frame_ctx(ctx, state, gen, |ctx, frame, runner| {
        gen_resume_and_raise(ctx, gen, frame, runner, send, throw, raise_stop_iteration)
    });
    result
}

/// The `send`/`throw` form of a generator resume as a `Value`:
/// exhaustion raises a real `StopIteration(value)` object; a
/// raise inside becomes `Err(exc)`; other `GenError`s become
/// their typed exceptions.
pub fn resume_generator_value_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    gen: Value,
    send: Value,
    throw: Option<Value>,
) -> Result<Value, Value> {
    match resume_generator_ctx(ctx, state, gen, send, throw, true) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => unreachable!("the send form never returns Ok(None)"),
        Err(GenError::StopIteration(stop)) => {
            Err(match stop {
                Some(v) => new_exception_args(ctx, state.exc.stop_iteration, &[v]),
                None => new_exception(ctx, state.exc.stop_iteration),
            })
        }
        Err(GenError::Raised(v)) => Err(v),
        Err(e) => {
            Err(exc_from_gen_error(ctx, state, e))
        }
    }
}

/// `gen.close()` (`gen_instance_close`,
/// `objgenerator.zig:295-314`): resume with `GeneratorExit`
/// injected; a yield is `RuntimeError("generator ignored
/// GeneratorExit")`; a raise is swallowed iff it is a
/// `GeneratorExit`; normal is a successful close.
pub fn close_generator_ctx(ctx: &mut CallCtx, state: &mut VmState, gen: Value, generator_exit: Value) -> Result<Value, GenError> {
    let (result, _) = run_gen_frame_ctx(ctx, state, gen, |ctx, frame, runner| {
        gen_instance_close(ctx, gen, frame, runner, generator_exit)
    });
    result
}

/// The shared frame plumbing for the resume/close paths: take
/// the frame out of `state.frames` (so the runner closure can
/// re-borrow the state), root it, run `f`, put it back and
/// re-root iff it yielded. Returns `(result, yielded)`.
fn run_gen_frame_ctx<R>(
    ctx: &mut CallCtx,
    state: &mut VmState,
    gen: Value,
    f: impl FnOnce(
        &mut CallCtx,
        &mut CodeState,
        &mut dyn FnMut(&mut CallCtx, &mut CodeState, Value, Option<Value>) -> RunOutcome,
    ) -> R,
) -> (R, bool)
where
    R: GenYielded,
{
    let gen_ref = match gen {
        Value::Heap(r) => r,
        _ => {
            // A non-generator value cannot be resumed — the
            // callers only pass generator instances, so this is
            // a caller-contract violation; report it as a
            // `GenError::PyRaise` (the VM maps it to a bare
            // TypeError).
            return (R::py_raise(), false);
        }
    };
    let frame = state.frames.remove(&gen_ref);
    let mut frame = match frame {
        Some(f) => f,
        None => {
            // No suspended frame: Closed (fast path — the dummy
            // frame is never used) or Running (re-entrant —
            // `gen_resume` reports `AlreadyExecuting`).
            let fun = match ctx.arena.get(gen_ref) {
                Obj::Generator(d) => d.fun,
                _ => return (R::py_raise(), false),
            };
            CodeState {
                fun,
                ip: 0,
                state: Vec::new(),
                n_state: 0,
                exc_sp_idx: 0,
                stack: Vec::new(),
                exc_stack: Vec::new(),
            }
        }
    };
    // Root the suspended frame's values across the resume (the
    // GC only runs inside `alloc`, using these roots).
    let mark = ctx.roots.stack.len();
    crate::runtime::vm_roots::sync_frame_roots(ctx, &frame, mark);
    let mut runner =
        |ctx: &mut CallCtx, frame: &mut CodeState, send: Value, throw: Option<Value>| {
            execute_frame(ctx, state, frame, send, throw)
        };
    let result = f(ctx, &mut frame, &mut runner);
    ctx.roots.stack.truncate(mark);
    // A yield suspends the frame: store it and re-root it (its
    // values changed during the run — the old `roots.extra`
    // entries are stale).
    let yielded = result.yielded();
    if yielded {
        state.frames.insert(gen_ref, frame);
        let extra_base = state.extra_base;
        ctx.roots.extra.truncate(extra_base);
        for fr in state.frames.values() {
            for v in crate::runtime::vm_roots::frame_roots_of(fr) {
                ctx.roots.extra.push(v);
            }
        }
    }
    (result, yielded)
}

/// Whether a generator result suspended (yielded) — the frame
/// must be kept. `gen_resume_and_raise`'s `Ok(Some(_))` is a
/// yield; `gen_instance_close` never yields (a yield is an
/// error there, so its `Ok` never suspends).
trait GenYielded {
    fn yielded(&self) -> bool;
    fn py_raise() -> Self;
}

impl GenYielded for Result<Option<Value>, GenError> {
    fn yielded(&self) -> bool {
        matches!(self, Ok(Some(_)))
    }
    fn py_raise() -> Self {
        Err(GenError::PyRaise)
    }
}

impl GenYielded for Result<Value, GenError> {
    fn yielded(&self) -> bool {
        false
    }
    fn py_raise() -> Self {
        Err(GenError::PyRaise)
    }
}

