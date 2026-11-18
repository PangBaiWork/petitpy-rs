//! SystemExit classification + exit-code mapping
//! (`entry/systemexit.zig`, shared by both entry paths).
//!
//! `embed_exec.zig` (compile-then-exec, used by `petitpy`)
//! and `embed_run_mpy.zig` (load-a-`.mpy`, used by
//! `petitpy-vm`) both treat an uncaught SystemExit as a clean
//! exit rather than a traceback (CPython convention).

use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::subclass::is_subclass_fast;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::VmState;

/// `isSystemExit` (`systemexit.zig:18-25`): true when `exc`
/// is a SystemExit (or a subclass of one) — the exception's
/// type, checked with `mp_obj_is_subclass_fast` against
/// `mp_type_SystemExit`.
pub fn is_system_exit(ctx: &CallCtx, state: &VmState, exc: Value) -> bool {
    let Some(t) = obj_get_type(ctx.arena, ctx.types, exc) else {
        return false;
    };
    is_subclass_fast(ctx.arena, Value::Heap(t), Value::Heap(state.exc.system_exit))
}

/// `exitCode` (`systemexit.zig:27-40`): maps SystemExit args
/// to a Unix exit code, CPython-style:
///   no arg / None → 0
///   int           → that int (low 8 bits)
///   anything else → 1
///
/// Only SMALL ints count as ints (`mirrors.isSmallInt`) —
/// a heap int (`2**70`) or a bool (`True` is not a small int
/// in this port) falls through to 1 (all probed on the
/// reference binary). `v & 0xff` is two's-complement, so
/// `SystemExit(-1)` is 255.
pub fn exit_code(ctx: &CallCtx, exc: Value) -> i32 {
    // `mp_obj_exception_get_value`: the first arg, or None
    // when there are no args.
    let arg = match exc {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Exception(d) => d.args.first().copied().unwrap_or(Value::None),
            _ => Value::None,
        },
        _ => Value::None,
    };
    match arg {
        Value::None => 0,
        Value::SmallInt(v) => (v & 0xff) as i32,
        _ => 1,
    }
}
