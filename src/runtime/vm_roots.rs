//! Frame rooting helpers (split out of `vm.rs`, Lane D2 —
//! `vm.rs` was over the 800-line cap).
//!
//! The GC is a mark-sweep arena with an explicit `RootSet`;
//! a running frame's live values (its `state` slots, its
//! value stack, and the exc-stack `prev_exc`s) must be in
//! `ctx.roots.stack` whenever the arena can allocate, so the
//! dispatch syncs them at the top of every iteration.

use crate::memory::gc::CallCtx;
use crate::objects::value::Value;
use crate::runtime::bc::CodeState;

/// The live values of one frame, for rooting (stack + state +
/// `fun` + the exc-stack `prev_exc`s).
pub(crate) fn frame_roots_of(frame: &CodeState) -> Vec<Value> {
    let mut out = Vec::new();
    out.push(Value::Heap(frame.fun));
    for v in &frame.state {
        if let Some(v) = v {
            out.push(*v);
        }
    }
    for v in &frame.stack {
        if let Some(v) = v {
            out.push(*v);
        }
    }
    for b in &frame.exc_stack {
        if let Some(v) = b.prev_exc {
            out.push(v);
        }
    }
    out
}

/// Sync the current frame's live values into `ctx.roots.stack`
/// (the dispatch calls this at the top of every iteration — see
/// the module docs). `mark` is the `roots.stack` length on
/// `execute_frame` entry.
pub(crate) fn sync_frame_roots(ctx: &mut CallCtx, frame: &CodeState, mark: usize) {
    ctx.roots.stack.truncate(mark);
    ctx.roots.stack.extend(frame_roots_of(frame));
}
