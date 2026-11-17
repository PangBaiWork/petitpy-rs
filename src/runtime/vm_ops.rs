//! The VM's operation helpers — the exception builders
//! (`exc_from_*` / `type_error`) and `mp_make_function_from_proto_fun`
//! / `mp_make_closure_from_proto_fun` (`emitglue.zig`), plus the
//! `&mut Vm` wrappers. The call dispatch
//! (`mp_call_function_n_kw` / `mp_call_method_n_kw` /
//! `mp_call_prepare_args_n_kw_var`, `runtime.zig:811-886`,
//! `:896-1048`) lives in `vm_call.rs`; the
//! name/global/attr/unary/binary/iter/unpack helpers live in
//! `vm_rt.rs` (`runtime.zig:288-408`, `:550-793`, `:1055-1301`).
//!
//! Raise convention: every helper here returns
//! `Result<Value, Value>` — `Err(exc)` carries the exception
//! OBJECT (built via `vm_raise::exception_from_pending` for a
//! recorded slot raise, or via `exc_from_typed` for a typed
//! error). `PyError::OutOfMemory` becomes a `MemoryError`
//! instance (the Zig raises `mp_obj_new_exception_msg` on the
//! OOM path in the same places).

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::objects::callables::objclosure::new_closure;
use crate::objects::callables::objfun::new_fun_bc;
use crate::objects::callables::objgenerator::GenError;
use crate::objects::objtype::objexcept::{new_exception, new_exception_args, new_exception_msg};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::PyError;
use crate::runtime::vm::{Vm, VmState};
use crate::runtime::vm_gen::{close_generator_ctx, resume_generator_ctx, resume_generator_value_ctx};
use crate::runtime::vm_raise::exception_from_pending;

/// Build the exception object for a typed error (`TypedRaise`:
/// the name/arg/message triple the `CallCtx` recording uses).
pub fn exc_from_typed<E: TypedRaise>(
    ctx: &mut CallCtx,
    state: &VmState,
    e: E,
) -> Value {
    let name = e.raise_name();
    let type_ = state.exc.by_name(name).unwrap_or(state.exc.type_error);
    if let Some(arg) = e.raise_arg() {
        new_exception_args(ctx, type_, &[arg])
    } else if let Some(msg) = e.raise_message(ctx.pool) {
        new_exception_msg(ctx, type_, &msg)
    } else {
        new_exception(ctx, type_)
    }
}

/// Build the exception object for a `PyError` returned by a slot
/// or helper: `PyRaise` drains the recorded raise (falling back
/// to a bare `TypeError` when nothing was recorded — the
/// "slot raised, data dropped" case); `OutOfMemory` becomes a
/// `MemoryError` instance.
pub fn exc_from_pyerror(ctx: &mut CallCtx, state: &VmState, e: PyError) -> Value {
    match e {
        PyError::PyRaise => exception_from_pending(ctx, state).unwrap_or_else(|| {
            new_exception(ctx, state.exc.type_error)
        }),
        PyError::OutOfMemory => {
            new_exception_msg(ctx, state.exc.memory_error, "out of memory")
        }
    }
}

/// Build the exception object for a slot-dispatch `ObjError`
/// (`obj_subscr` / `binary_op` / `unary_op` / `dict_store` /
/// `set_store` / `getiter` / `iternext`). The slot fns record
/// their real error in `ctx.pending_raise` and return `PyRaise`,
/// which `call_subscr` / `call_binary` / `call_unary` map to
/// `NotImplemented` — so the `ObjError` that reaches here is
/// often only the GENERIC fallback ("object isn't
/// subscriptable", "object doesn't support item assignment").
/// The Zig's `mpObjSubscrImpl` rule (`obj.zig:604-605`): a slot
/// failure WITH a pending exception propagates THAT error, and
/// only a failure with NO pending error falls through to the
/// generic raise. Drain the pending raise first; convert the
/// typed `ObjError` only when nothing is pending.
pub fn exc_from_obj_error<E: TypedRaise>(
    ctx: &mut CallCtx,
    state: &VmState,
    e: E,
) -> Value {
    if let Some(exc) = exception_from_pending(ctx, state) {
        return exc;
    }
    exc_from_typed(ctx, state, e)
}

/// Build the exception object for a `SetupError` (always a
/// `TypeError` with the TERSE message — `bc.rs:248`).
pub fn exc_from_setup(
    ctx: &mut CallCtx,
    state: &VmState,
    e: crate::runtime::bc::SetupError,
) -> Value {
    let msg = e.message(ctx.pool);
    new_exception_msg(ctx, state.exc.type_error, &msg)
}

/// Build the exception object for a `GenError` (`objgenerator.rs:
/// `exc_name()` / `message()` / `setup_message()`; `Raised`
/// carries the exception object itself; `StopIteration` builds
/// the real `StopIteration(value)` object).
pub fn exc_from_gen_error(ctx: &mut CallCtx, state: &VmState, e: GenError) -> Value {
    match e {
        GenError::Raised(v) => v,
        GenError::StopIteration(stop) => match stop {
            Some(v) => new_exception_args(ctx, state.exc.stop_iteration, &[v]),
            None => new_exception(ctx, state.exc.stop_iteration),
        },
        GenError::Setup(se) => exc_from_setup(ctx, state, se),
        GenError::PyRaise => exception_from_pending(ctx, state)
            .unwrap_or_else(|| new_exception(ctx, state.exc.type_error)),
        GenError::OutOfMemory => {
            new_exception_msg(ctx, state.exc.memory_error, "out of memory")
        }
        _ => {
            let name = e.exc_name();
            let type_ = state.exc.by_name(name).unwrap_or(state.exc.type_error);
            match e.message() {
                Some(m) => new_exception_msg(ctx, type_, m),
                None => new_exception(ctx, type_),
            }
        }
    }
}

/// Raise a `TypeError` with a message, as an exception object.
pub fn type_error(ctx: &mut CallCtx, state: &VmState, msg: &str) -> Value {
    new_exception_msg(ctx, state.exc.type_error, msg)
}

use crate::runtime::vm_call::{call_function_ctx, call_method_ctx};

// ── `make_function` / `make_closure` ────────────────────────────────

/// `mp_make_function_from_proto_fun` (`emitglue.zig:89-104`):
/// build a `FunBc` from a child `RawCode` (looked up in the code
/// store by (parent, child)), with the current module as context.
/// The name is the prelude's block name and `n_args` is the
/// C7 packing of (n_pos, n_pos + n_kwonly); if the child's
/// prelude scope flag has the generator bit (0x01), the object
/// is typed `gen_wrap` instead of `fun_bc` (same payload,
/// different header type — `emitglue.zig` retypes it after
/// creation). `def_pos`/`def_kw` are the two values popped by
/// `MAKE_FUNCTION_DEFARGS` (`[def_pos_tuple, def_kw_or_NULL]` —
/// the compiler emits `BUILD_TUPLE` + `LOAD_NULL` when there are
/// pos defaults but no kw defaults, `compile.zig:3551-3555`).
pub fn make_function_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    parent_code: usize,
    child: usize,
    def_pos: Option<Value>,
    def_kw: Option<Value>,
) -> Result<Value, Value> {
    use crate::objects::primitives::objint::fun_n_args;
    use crate::runtime::bc::{prelude_sig_decode, prelude_size_decode, MP_SCOPE_FLAG_GENERATOR};
    let code_idx = match state.code_store.child(parent_code, child) {
        Some(i) => i,
        None => {
            return Err(type_error(ctx, state, "invalid child code index"));
        }
    };
    let code = state.code_store.code(code_idx);
    let mut cursor: &[u8] = &code.fun_data;
    let sig = prelude_sig_decode(&mut cursor);
    let _size = prelude_size_decode(&mut cursor);
    let name = crate::runtime::bc::decode_uint(&mut cursor);
    let is_gen = sig.scope_flags & MP_SCOPE_FLAG_GENERATOR != 0;
    let type_ = if is_gen {
        state.vm_types.gen_wrap
    } else {
        state.vm_types.fun_bc
    };
    let n_args = fun_n_args(
        sig.n_pos_args as u16,
        (sig.n_pos_args + sig.n_kwonly_args) as u16,
    );
    let context = {
        let module = state.code_store.module(code_idx);
        Some(module)
    };
    let fun = new_fun_bc(
        ctx, type_, name as u32, n_args, code_idx, def_pos, def_kw, context,
    );
    Ok(Value::Heap(fun))
}

/// `mp_make_closure_from_proto_fun` (`emitglue.zig:110-122`):
/// `make_function` plus the closed-over cells. `nco` packs
/// bit 8 (0x100) = def_args present, bits 0..7 = n_closed; the
/// stack layout is `[def_pos, def_kw, cell0, …]` when def_args
/// are present, `[cell0, …]` otherwise (`closed_offset =
/// (nco >> 7) & 2` — 2 iff bit 8 is set). `cells` are the
/// `Cell` objects themselves (the Zig's `mp_obj_new_closure`
/// takes the cell objects, not their contents).
pub fn make_closure_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    parent_code: usize,
    child: usize,
    nco: usize,
    stack: &[Option<Value>],
) -> Result<Value, Value> {
    let has_defargs = nco & 0x100 != 0;
    let n_closed = nco & 0xff;
    let cells_start = if has_defargs { 2 } else { 0 };
    let mut cells: Vec<GcRef> = Vec::with_capacity(n_closed);
    for i in 0..n_closed {
        match stack.get(cells_start + i).copied().flatten() {
            Some(Value::Heap(r)) => cells.push(r),
            _ => {
                return Err(type_error(
                    ctx,
                    state,
                    "closure cell is not a cell object",
                ));
            }
        }
    }
    let (def_pos, def_kw) = if has_defargs {
        (
            stack.first().copied().flatten(),
            stack.get(1).copied().flatten(),
        )
    } else {
        (None, None)
    };
    let fun = make_function_ctx(ctx, state, parent_code, child, def_pos, def_kw)?;
    let closure_type = state.vm_types.closure;
    let closure = new_closure(ctx, closure_type, fun, cells);
    Ok(Value::Heap(closure))
}

// ── `&mut Vm` wrappers ───────────────────────────────────────────────
// The `*_ctx` forms above take the split `(ctx, state)` — the
// dispatch and the iterator seams hold the split parts, not the
// `Vm`. These wrappers split a `&mut Vm` for the callers that
// hold the whole VM (module loading, the `next`/`print`
// builtins' tests).

/// `mp_call_function_n_kw` for a `&mut Vm` holder.
pub fn call_function(
    vm: &mut Vm,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let (mut ctx, state) = vm.split();
    call_function_ctx(&mut ctx, state, fun, n_args, n_kw, args)
}

/// `mp_call_method_n_kw` for a `&mut Vm` holder.
pub fn call_method(
    vm: &mut Vm,
    method: Value,
    self_: Option<Value>,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let (mut ctx, state) = vm.split();
    call_method_ctx(&mut ctx, state, method, self_, n_args, n_kw, args)
}

/// Generator resume for a `&mut Vm` holder.
pub fn resume_generator(
    vm: &mut Vm,
    gen: Value,
    send: Value,
    throw: Option<Value>,
    raise_stop_iteration: bool,
) -> Result<Option<Value>, GenError> {
    let (mut ctx, state) = vm.split();
    resume_generator_ctx(&mut ctx, state, gen, send, throw, raise_stop_iteration)
}

/// The `send`/`throw` form of a generator resume for a
/// `&mut Vm` holder.
pub fn resume_generator_value(
    vm: &mut Vm,
    gen: Value,
    send: Value,
    throw: Option<Value>,
) -> Result<Value, Value> {
    let (mut ctx, state) = vm.split();
    resume_generator_value_ctx(&mut ctx, state, gen, send, throw)
}

/// `gen.close()` for a `&mut Vm` holder.
pub fn close_generator(
    vm: &mut Vm,
    gen: Value,
    generator_exit: Value,
) -> Result<Value, GenError> {
    let (mut ctx, state) = vm.split();
    close_generator_ctx(&mut ctx, state, gen, generator_exit)
}

/// `mp_make_function_from_proto_fun` for a `&mut Vm` holder.
pub fn make_function(
    vm: &mut Vm,
    parent_code: usize,
    child: usize,
    def_pos: Option<Value>,
    def_kw: Option<Value>,
) -> Result<Value, Value> {
    let (mut ctx, state) = vm.split();
    make_function_ctx(&mut ctx, state, parent_code, child, def_pos, def_kw)
}

/// `mp_make_closure_from_proto_fun` for a `&mut Vm` holder.
pub fn make_closure(
    vm: &mut Vm,
    parent_code: usize,
    child: usize,
    nco: usize,
    stack: &[Option<Value>],
) -> Result<Value, Value> {
    let (mut ctx, state) = vm.split();
    make_closure_ctx(&mut ctx, state, parent_code, child, nco, stack)
}
