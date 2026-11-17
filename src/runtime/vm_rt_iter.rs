//! Getiter / iternext / resume / unpack — the `runtime.zig:1055-1123`
//! and `:1277-1456` ports: `mp_getiter` / `mp_iternext` /
//! `mp_iternext_allow_raise` / `mp_resume` / `mp_unpack_sequence` /
//! `mp_unpack_ex`, the import stubs (C15's), the `CALL_FUNCTION`
//! operand marshalling, and the `&mut Vm` wrappers.
//! Split out of `vm_rt.rs` (file-size rule).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objgenerator::GenError;
use crate::objects::obj::{obj_get_type, ObjError};
use crate::objects::obj_ops::obj_equal;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::vm::{Vm, VmState};
use crate::runtime::vm_ops::{exc_from_obj_error, exc_from_pyerror, exc_from_typed, type_error};
use crate::runtime::vm_rt::{binary_op_ctx, is_true_ctx, pending_exc_or_type_error, unary_op_ctx};
use crate::runtime::vm_rt_name::{
    convert_member_lookup, delete_global_ctx, delete_name_ctx, load_attr_ctx, load_build_class_ctx,
    load_global_ctx, load_method_ctx, load_method_maybe_ctx, load_name_ctx, store_attr_ctx,
    store_global_ctx, store_name_ctx,
};

/// The CONTAINS fallback (`runtime.zig:760-773`): iterate the
/// lhs and compare each item with `obj_equal`. `pub(crate)` —
/// `binary_op_ctx` (`vm_rt.rs`) calls it.
pub(crate) fn contains_op_ctx(ctx: &mut CallCtx, state: &mut VmState, lhs: Value, rhs: Value) -> Result<Value, Value> {
    let it = getiter_vm_ctx(ctx, state, lhs)?;
    loop {
        match iternext_vm_ctx(ctx, state, it)? {
            Some(v) => {
                let types = ctx.types;
                match obj_equal(ctx, types, v, rhs) {
                    Ok(true) => return Ok(Value::Bool(true)),
                    Ok(false) => {}
                    Err(e) => return Err(exc_from_pyerror(ctx, state, e)),
                }
            }
            None => return Ok(Value::Bool(false)),
        }
    }
}

// ── getiter / iternext / resume ──────────────────────────────────────

/// `mp_getiter` (`runtime.zig:1277-1301`) via C11's `getiter`
/// (iterator identity, the getiter slot, the `__getitem__`
/// fallback, else `TypeError("object not iterable")`).
pub fn getiter_vm_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    value: Value,
) -> Result<Value, Value> {
    use crate::objects::iters::iter::getiter;
    getiter(ctx, value).map_err(|e| exc_from_obj_error(ctx, state, e))
}

/// Drain a raise left by an iterator seam: `state.pending_exc`
/// first (the `call` seam's stash), then the recorded
/// `pending_raise`, else a bare TypeError.
fn drain_seam_raise(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    if let Some(exc) = state.pending_exc.take() {
        return exc;
    }
    pending_exc_or_type_error(ctx, state)
}

/// Is `exc` a `StopIteration` instance (including subclasses)?
fn is_stop_iteration(arena: &crate::memory::gc::GcArena, state: &VmState, exc: Value) -> bool {
    use crate::objects::objtype::objexcept::is_exception_instance;
    is_exception_instance(arena, exc, state.exc.stop_iteration)
}

/// `mp_iternext_allow_raise` (`runtime.zig:1304-1321`): the next
/// value of an iterator, or a raise (StopIteration propagates
/// with its object). Generator → the generator resume
/// (`raise_stop_iteration=true`); `Obj::Iterator` → C11's
/// `iternext` with the VM seams (exhaustion raises a bare
/// StopIteration); anything else → the `__next__` method path
/// (the locals-dict lookup directly — the `__next__` special
/// case in `load_method_maybe` returns the `next` builtin,
/// which would recurse here).
pub fn iternext_allow_raise_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    it: Value,
) -> Result<Value, Value> {
    use crate::objects::iters::iter::{iternext, CallOutcome, IterSeams};
    use crate::objects::obj_ops::obj_subscr;
    use crate::objects::slots::SubscrArg;
    if let Value::Heap(r) = it {
        if matches!(ctx.arena.get(r), Obj::Generator(_)) {
            return match crate::runtime::vm_gen::resume_generator_ctx(
                ctx, state, it, Value::None, None, true,
            ) {
                Ok(Some(v)) => Ok(v),
                Ok(None) => unreachable!("the send form never returns Ok(None)"),
                Err(e) => Err(crate::runtime::vm_ops::exc_from_gen_error(ctx, state, e)),
            };
        }
    }
    if let Value::Heap(r) = it {
        if matches!(ctx.arena.get(r), Obj::Iterator(_)) {
            // The seams: `call` routes user-function calls
            // through the VM (capturing `state`); `getitem`
            // uses the subscr slot directly (no state needed).
            let mut call_seam =
                |ctx2: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
                    match crate::runtime::vm_call::call_function_ctx(
                        ctx2, state, fun, args.len(), 0, args,
                    ) {
                        Ok(v) => CallOutcome::Value(v),
                        Err(exc) => {
                            if is_stop_iteration(ctx2.arena, state, exc) {
                                CallOutcome::Stop
                            } else {
                                state.pending_exc = Some(exc);
                                CallOutcome::Raise
                            }
                        }
                    }
                };
            let mut getitem_seam =
                |ctx2: &mut CallCtx, obj: Value, index: i64| -> CallOutcome {
                    let types = ctx2.types;
                    match obj_subscr(ctx2, types, obj, Value::SmallInt(index), SubscrArg::Get) {
                        Ok(v) => CallOutcome::Value(v),
                        Err(ObjError::IndexOutOfRange) => CallOutcome::Stop,
                        Err(e) => {
                            let _ = ctx2.record_typed(e);
                            CallOutcome::Raise
                        }
                    }
                };
            let mut seams = IterSeams {
                call: &mut call_seam,
                getitem: &mut getitem_seam,
            };
            return match iternext(ctx, it, &mut seams) {
                Ok(Some(v)) => Ok(v),
                Ok(None) => Err(crate::objects::objtype::objexcept::new_exception(
                    ctx,
                    state.exc.stop_iteration,
                )),
                Err(crate::objects::iters::iter::IterError::PyRaise) => {
                    Err(drain_seam_raise(ctx, state))
                }
                Err(e) => Err(exc_from_typed(ctx, state, e)),
            };
        }
    }
    // The `__next__` method path.
    let (method, self_) = load_next_method_ctx(ctx, state, it)?;
    crate::runtime::vm_call::call_method_ctx(ctx, state, method, self_, 0, 0, &[])
}

/// The locals-dict `__next__` (bypassing the hasIterNext
/// special case in `load_method_maybe` — that returns the `next`
/// builtin, which would recurse into `iternext_allow_raise`).
/// `TypeError("object not an iterator")` when absent.
fn load_next_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    it: Value,
) -> Result<(Value, Option<Value>), Value> {
    let next_q = static_qstr_id(b"__next__").unwrap_or(u32::MAX);
    let type_ = obj_get_type(ctx.arena, ctx.types, it);
    if let Some(t) = type_ {
        if let Obj::Type(d) = ctx.arena.get(t) {
            let dict = d.dict;
            use crate::objects::containers::objdict::dict_get_value;
            if let Ok(member) = dict_get_value(ctx, Value::Heap(dict), Value::Qstr(next_q)) {
                let (method, self_) = convert_member_lookup(ctx, state, it, t, member);
                if let Some(m) = method {
                    return Ok((m, self_));
                }
            }
        }
    }
    Err(type_error(ctx, state, "object not an iterator"))
}

/// `mp_iternext` (`runtime.zig:1326-1359`): the translating
/// form — exhaustion (and any StopIteration) is `Ok(None)`.
/// Pre-flight: a pending raise returns stop WITHOUT clearing
/// it (`runtime.zig:1359-1362`).
pub fn iternext_vm_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    it: Value,
) -> Result<Option<Value>, Value> {
    if ctx.has_pending_raise() || state.pending_exc.is_some() {
        return Ok(None);
    }
    match iternext_allow_raise_ctx(ctx, state, it) {
        Ok(v) => Ok(Some(v)),
        Err(exc) => {
            if is_stop_iteration(ctx.arena, state, exc) {
                Ok(None)
            } else {
                Err(exc)
            }
        }
    }
}

/// `mp_resume` (`runtime.zig:1363-1456`): resume an iterator /
/// generator for `YIELD_FROM`. `Yield(v)` = a value was
/// produced; `Normal(v)` = exhausted with return value `v`;
/// `Exception(exc)` = a raise.
pub enum ResumeOutcome {
    Yield(Value),
    Normal(Value),
    Exception(Value),
}

pub fn resume_vm_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    self_: Value,
    send: Option<Value>,
    throw: Option<Value>,
) -> ResumeOutcome {
    use crate::objects::objtype::objexcept::{exception_get_value, exception_match};
    use crate::objects::objtype::objexcept::type_is_subclass_of;
    // Generator → the generator resume (`raise_stop_iteration`
    // so exhaustion is a `Normal` with the return value).
    // "No send" is `None` here — a real `None` send is
    // `Some(Value::None)` (the Zig distinguishes NULL from
    // `mp_const_none`).
    let send_v = send.unwrap_or(Value::None);
    if let Value::Heap(r) = self_ {
        if matches!(ctx.arena.get(r), Obj::Generator(_)) {
            return match crate::runtime::vm_gen::resume_generator_ctx(
                ctx, state, self_, send_v, throw, true,
            ) {
                Ok(Some(v)) => ResumeOutcome::Yield(v),
                Ok(None) => unreachable!("the send form never returns Ok(None)"),
                Err(GenError::StopIteration(stop)) => {
                    ResumeOutcome::Normal(stop.unwrap_or(Value::None))
                }
                Err(GenError::Raised(v)) => {
                    ResumeOutcome::Exception(v)
                }
                Err(e) => ResumeOutcome::Exception(
                    crate::runtime::vm_ops::exc_from_gen_error(ctx, state, e),
                ),
            };
        }
    }
    // The `send` method path — ONLY when the send value is a
    // real non-`None` value (`mp_resume`, `runtime.zig:1407`:
    // `send_value != 0` loads `send`; a `None` send — `Some(None)`
    // here, or no send at all — falls through to `__next__`
    // below, exactly as the Zig's `send_value == OBJ_NONE`
    // branches do). A list iterator has no `send`, so routing
    // a `None` send through it raised `AttributeError("no
    // attribute 'send'")` for every `yield from [1, 2]`.
    if let Some(sv) = send {
        if sv != Value::None {
            let send_q = static_qstr_id(b"send").unwrap_or(u32::MAX);
            match load_method_ctx(ctx, state, self_, send_q) {
                Ok((method, self2)) => {
                    match crate::runtime::vm_call::call_method_ctx(
                        ctx, state, method, self2, 1, 0, &[sv],
                    ) {
                        Ok(v) => return ResumeOutcome::Yield(v),
                        Err(exc) => {
                            if is_stop_iteration(ctx.arena, state, exc) {
                                return ResumeOutcome::Normal(exception_get_value(ctx.arena, exc));
                            }
                            return ResumeOutcome::Exception(exc);
                        }
                    }
                }
                Err(exc) => return ResumeOutcome::Exception(exc),
            }
        }
    }
    if let Some(throw_value) = throw {
        // GeneratorExit → `close`; else `throw`.
        let is_gen_exit = match obj_get_type(ctx.arena, ctx.types, throw_value) {
            Some(t) => type_is_subclass_of(ctx.arena, t, state.exc.generator_exit),
            None => false,
        };
        let meth_q = static_qstr_id(if is_gen_exit { b"close" } else { b"throw" })
            .unwrap_or(u32::MAX);
        if let Ok((method, self2)) = load_method_maybe_ctx(ctx, state, self_, meth_q) {
            if let Some(m) = method {
                let args: &[Value] = if is_gen_exit { &[] } else { &[throw_value] };
                let n_args = if is_gen_exit { 0 } else { 1 };
                return match crate::runtime::vm_call::call_method_ctx(
                    ctx, state, m, self2, n_args, 0, args,
                ) {
                    Ok(v) => {
                        if is_gen_exit {
                            ResumeOutcome::Normal(v)
                        } else {
                            ResumeOutcome::Yield(v)
                        }
                    }
                    Err(exc) => {
                        if !is_gen_exit && is_stop_iteration(ctx.arena, state, exc) {
                            ResumeOutcome::Normal(exception_get_value(ctx.arena, exc))
                        } else {
                            ResumeOutcome::Exception(exc)
                        }
                    }
                };
            }
        }
        if exception_match(
            ctx.arena,
            throw_value,
            Value::Heap(state.exc.stop_iteration),
        ) {
            return ResumeOutcome::Exception(
                crate::objects::objtype::objexcept::new_exception_msg(
                    ctx,
                    state.exc.runtime_error,
                    "generator raised StopIteration",
                ),
            );
        }
        return ResumeOutcome::Exception(crate::runtime::vm_raise::make_raise_obj(
            ctx, state, throw_value,
        ));
    }
    // send == None, no throw → the `__next__` path.
    match iternext_allow_raise_ctx(ctx, state, self_) {
        Ok(v) => ResumeOutcome::Yield(v),
        Err(exc) => {
            if is_stop_iteration(ctx.arena, state, exc) {
                ResumeOutcome::Normal(exception_get_value(ctx.arena, exc))
            } else {
                ResumeOutcome::Exception(exc)
            }
        }
    }
}

// ── unpack ──────────────────────────────────────────────────────────

/// `mp_unpack_sequence` (`runtime.zig:1055-1079`): exactly `num`
/// items from `seq`, else `ValueError("wrong number of values
/// to unpack")` — the SAME message for too-short and too-long
/// (terse, `runtime.zig:232`). The returned Vec is in
/// stack-push order: the items are stored REVERSED, so the
/// first element ends up on top (the dispatch pushes them in
/// order and the first `STORE_*` pops the first element).
/// The Zig's tuple/list fast path is skipped — the iterator
/// path is observably identical for exact tuples/lists (no
/// user code runs for their iterators).
/// The single unpack error (`runtime.zig:232`): `ValueError("wrong
/// number of values to unpack")` — the SAME message for
/// too-short and too-long (terse).
fn unpack_err(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.value_error,
        "wrong number of values to unpack",
    )
}

pub fn unpack_sequence_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    seq: Value,
    num: usize,
) -> Result<Vec<Value>, Value> {
    let it = getiter_vm_ctx(ctx, state, seq)?;
    let mut items = Vec::with_capacity(num);
    for _ in 0..num {
        match iternext_vm_ctx(ctx, state, it)? {
            Some(v) => items.push(v),
            None => return Err(unpack_err(ctx, state)),
        }
    }
    if iternext_vm_ctx(ctx, state, it)?.is_some() {
        return Err(unpack_err(ctx, state));
    }
    items.reverse();
    Ok(items)
}

/// `mp_unpack_ex` (`runtime.zig:1081-1121`): `num` packs
/// (num_left, num_right) — `num_left = num & 0xff`,
/// `num_right = (num >> 8) & 0xff`. The middle items collect
/// into a real list (the starred target). Stack-push order:
/// the right items reversed, the starred list, the left items
/// reversed. Same `ValueError` on too few items.
pub fn unpack_ex_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    seq: Value,
    num: usize,
) -> Result<Vec<Value>, Value> {
    use crate::objects::containers::objlist::new_list;
    let num_left = num & 0xff;
    let num_right = (num >> 8) & 0xff;
    let it = getiter_vm_ctx(ctx, state, seq)?;
    let mut all = Vec::new();
    loop {
        match iternext_vm_ctx(ctx, state, it)? {
            Some(v) => all.push(v),
            None => break,
        }
    }
    if all.len() < num_left + num_right {
        return Err(unpack_err(ctx, state));
    }
    let right_start = all.len() - num_right;
    let star = new_list(ctx, ctx.types.list, &all[num_left..right_start]);
    let mut out = Vec::with_capacity(num_left + num_right + 1);
    for v in all[right_start..].iter().rev() {
        out.push(*v);
    }
    out.push(Value::Heap(star));
    for v in all[..num_left].iter().rev() {
        out.push(*v);
    }
    Ok(out)
}

// ── import stubs (C15 owns the import system) ───────────────────────

/// `mp_import_name` — C15's. Until then every import raises
/// `ImportError` (documented stub, not a silent pass).
pub fn import_name_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    _qst: u32,
    _from_list: Value,
) -> Result<Value, Value> {
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.import_error,
        "import system not implemented (C15)",
    ))
}

/// `mp_import_from` — C15's.
pub fn import_from_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    _module: Value,
    _qst: u32,
) -> Result<Value, Value> {
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.import_error,
        "import system not implemented (C15)",
    ))
}

/// `mp_import_all` — C15's.
pub fn import_all_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    _module: Value,
) -> Result<(), Value> {
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.import_error,
        "import system not implemented (C15)",
    ))
}

// ── call-operand marshalling ────────────────────────────────────────

/// Pop a `CALL_FUNCTION`'s operands off the frame stack:
/// `n_args` positional values followed by `n_kw` (key, value)
/// pairs (the compiler pushes the kwargs as alternating
/// key/value — `compile.zig:3551`). Returns (args, kwargs) in
/// source order.
pub fn prepare_var_kw(
    frame: &mut CodeState,
    n_args: usize,
    n_kw: usize,
) -> (Vec<Value>, Vec<(u32, Value)>) {
    let mut kwargs = Vec::with_capacity(n_kw);
    for _ in 0..n_kw {
        let v = frame.stack.pop().flatten().unwrap_or(Value::None);
        let k = frame.stack.pop().flatten().unwrap_or(Value::None);
        if let Value::Qstr(q) = k {
            kwargs.push((q, v));
        }
    }
    kwargs.reverse();
    let mut args = Vec::with_capacity(n_args);
    for _ in 0..n_args {
        args.push(frame.stack.pop().flatten().unwrap_or(Value::None));
    }
    args.reverse();
    (args, kwargs)
}

// ── `&mut Vm` wrappers ──────────────────────────────────────────────

pub fn is_true(vm: &mut Vm, value: Value) -> Result<bool, Value> {
    let (mut ctx, mut state) = vm.split();
    is_true_ctx(&mut ctx, &mut state, value)
}

pub fn unary_op(vm: &mut Vm, op: UnaryOp, value: Value) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    unary_op_ctx(&mut ctx, &mut state, op, value)
}

pub fn binary_op(vm: &mut Vm, op: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    binary_op_ctx(&mut ctx, &mut state, op, lhs, rhs)
}

pub fn load_name(vm: &mut Vm, qst: u32) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    load_name_ctx(&mut ctx, &mut state, qst)
}

pub fn load_global(vm: &mut Vm, qst: u32) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    load_global_ctx(&mut ctx, &mut state, qst)
}

pub fn store_name(vm: &mut Vm, qst: u32, value: Value) -> Result<(), Value> {
    let (mut ctx, mut state) = vm.split();
    store_name_ctx(&mut ctx, &mut state, qst, value)
}

pub fn store_global(vm: &mut Vm, qst: u32, value: Value) -> Result<(), Value> {
    let (mut ctx, mut state) = vm.split();
    store_global_ctx(&mut ctx, &mut state, qst, value)
}

pub fn delete_name(vm: &mut Vm, qst: u32) {
    let (mut ctx, mut state) = vm.split();
    delete_name_ctx(&mut ctx, &mut state, qst)
}

pub fn delete_global(vm: &mut Vm, qst: u32) {
    let (mut ctx, mut state) = vm.split();
    delete_global_ctx(&mut ctx, &mut state, qst)
}

pub fn load_build_class(vm: &mut Vm) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    load_build_class_ctx(&mut ctx, &mut state)
}

pub fn load_method(vm: &mut Vm, obj: Value, attr: u32) -> Result<(Value, Option<Value>), Value> {
    let (mut ctx, mut state) = vm.split();
    load_method_ctx(&mut ctx, &mut state, obj, attr)
}

pub fn load_attr(vm: &mut Vm, base: Value, attr: u32) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    load_attr_ctx(&mut ctx, &mut state, base, attr)
}

pub fn store_attr(vm: &mut Vm, base: Value, attr: u32, value: Value) -> Result<(), Value> {
    let (mut ctx, mut state) = vm.split();
    store_attr_ctx(&mut ctx, &mut state, base, attr, value)
}

pub fn getiter_vm(vm: &mut Vm, value: Value) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    getiter_vm_ctx(&mut ctx, &mut state, value)
}

pub fn iternext_vm(vm: &mut Vm, it: Value) -> Result<Option<Value>, Value> {
    let (mut ctx, mut state) = vm.split();
    iternext_vm_ctx(&mut ctx, &mut state, it)
}

pub fn iternext_allow_raise(vm: &mut Vm, it: Value) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    iternext_allow_raise_ctx(&mut ctx, &mut state, it)
}

pub fn resume_vm(
    vm: &mut Vm,
    self_: Value,
    send: Option<Value>,
    throw: Option<Value>,
) -> ResumeOutcome {
    let (mut ctx, mut state) = vm.split();
    resume_vm_ctx(&mut ctx, &mut state, self_, send, throw)
}

pub fn unpack_sequence(vm: &mut Vm, seq: Value, num: usize) -> Result<Vec<Value>, Value> {
    let (mut ctx, mut state) = vm.split();
    unpack_sequence_ctx(&mut ctx, &mut state, seq, num)
}

pub fn unpack_ex(vm: &mut Vm, seq: Value, num: usize) -> Result<Vec<Value>, Value> {
    let (mut ctx, mut state) = vm.split();
    unpack_ex_ctx(&mut ctx, &mut state, seq, num)
}

pub fn import_name(vm: &mut Vm, qst: u32, from_list: Value) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    import_name_ctx(&mut ctx, &mut state, qst, from_list)
}

pub fn import_from(vm: &mut Vm, module: Value, qst: u32) -> Result<Value, Value> {
    let (mut ctx, mut state) = vm.split();
    import_from_ctx(&mut ctx, &mut state, module, qst)
}

pub fn import_all(vm: &mut Vm, module: Value) -> Result<(), Value> {
    let (mut ctx, mut state) = vm.split();
    import_all_ctx(&mut ctx, &mut state, module)
}
