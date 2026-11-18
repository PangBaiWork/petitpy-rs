//! The dispatch loop — the body of `dispatchBody` (`vm.zig:500-1180`).
//!
//! One iteration per opcode: fetch (polling `pending_raise`
//! first — the B2 polled-sentinel, so a raise left by a
//! bridge-wrapped call routes to the `PENDING` arm with `ip`
//! still at that opcode's byte), decode the operands with a
//! local `ip` cursor, execute, and loop back to fetch (which
//! writes `ip` into `frame.ip` — so a mid-opcode raise leaves
//! `frame.ip` at the opcode byte for `exception_handler`).
//!
//! Rooting: `sync_frame_roots` runs at the top of every
//! iteration, so the frame's stack/state are rooted before any
//! opcode that can allocate. Call arms never pop the operands
//! before the call — the args stay on `frame.stack` (rooted by
//! that sync) while the callee runs, and are truncated only
//! after it returns (the Zig leaves them on the C stack the
//! same way).
//!
//! The loop returns a `DispatchOutcome`; `execute_frame`
//! (`vm_raise.rs`) owns the outer exception loop. The stack /
//! decode / unwind helpers live in `vm_dispatch_util`; the
//! yield / builder / unpack / make / call / import arms live
//! in `vm_dispatch_exec` (both split out so no file exceeds
//! 800 lines).

use crate::compiler::emit::consts::{
    MP_BC_BINARY_OP_MULTI, MP_BC_BINARY_OP_MULTI_NUM, MP_BC_BUILD_LIST, MP_BC_BUILD_MAP,
    MP_BC_BUILD_SET, MP_BC_BUILD_SLICE, MP_BC_BUILD_TUPLE, MP_BC_CALL_FUNCTION,
    MP_BC_CALL_FUNCTION_VAR_KW, MP_BC_CALL_METHOD, MP_BC_CALL_METHOD_VAR_KW, MP_BC_DELETE_DEREF,
    MP_BC_DELETE_FAST, MP_BC_DELETE_GLOBAL, MP_BC_DELETE_NAME, MP_BC_DUP_TOP, MP_BC_DUP_TOP_TWO,
    MP_BC_END_FINALLY, MP_BC_FOR_ITER, MP_BC_GET_ITER, MP_BC_GET_ITER_STACK, MP_BC_IMPORT_FROM,
    MP_BC_IMPORT_NAME, MP_BC_IMPORT_STAR, MP_BC_JUMP, MP_BC_JUMP_IF_FALSE_OR_POP,
    MP_BC_JUMP_IF_TRUE_OR_POP, MP_BC_LOAD_ATTR, MP_BC_LOAD_BUILD_CLASS, MP_BC_LOAD_CONST_FALSE,
    MP_BC_LOAD_CONST_NONE, MP_BC_LOAD_CONST_OBJ, MP_BC_LOAD_CONST_SMALL_INT,
    MP_BC_LOAD_CONST_SMALL_INT_MULTI, MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS,
    MP_BC_LOAD_CONST_SMALL_INT_MULTI_NUM, MP_BC_LOAD_CONST_STRING, MP_BC_LOAD_CONST_TRUE,
    MP_BC_LOAD_DEREF, MP_BC_LOAD_FAST_MULTI, MP_BC_LOAD_FAST_MULTI_NUM, MP_BC_LOAD_FAST_N,
    MP_BC_LOAD_GLOBAL, MP_BC_LOAD_METHOD, MP_BC_LOAD_NAME, MP_BC_LOAD_NULL, MP_BC_LOAD_SUBSCR,
    MP_BC_LOAD_SUPER_METHOD, MP_BC_MAKE_CLOSURE, MP_BC_MAKE_CLOSURE_DEFARGS, MP_BC_MAKE_FUNCTION,
    MP_BC_MAKE_FUNCTION_DEFARGS, MP_BC_POP_EXCEPT_JUMP, MP_BC_POP_JUMP_IF_FALSE,
    MP_BC_POP_JUMP_IF_TRUE, MP_BC_POP_TOP, MP_BC_RAISE_FROM, MP_BC_RAISE_LAST, MP_BC_RAISE_OBJ,
    MP_BC_RETURN_VALUE, MP_BC_ROT_THREE, MP_BC_ROT_TWO, MP_BC_SETUP_EXCEPT, MP_BC_SETUP_FINALLY,
    MP_BC_SETUP_WITH, MP_BC_STORE_ATTR, MP_BC_STORE_COMP, MP_BC_STORE_DEREF, MP_BC_STORE_FAST_MULTI,
    MP_BC_STORE_FAST_MULTI_NUM, MP_BC_STORE_FAST_N, MP_BC_STORE_GLOBAL, MP_BC_STORE_MAP,
    MP_BC_STORE_NAME, MP_BC_STORE_SUBSCR, MP_BC_UNARY_OP_MULTI, MP_BC_UNARY_OP_MULTI_NUM,
    MP_BC_UNPACK_EX, MP_BC_UNPACK_SEQUENCE, MP_BC_UNWIND_JUMP, MP_BC_WITH_CLEANUP,
    MP_BC_YIELD_FROM, MP_BC_YIELD_VALUE,
};
use crate::memory::gc::CallCtx;
use crate::objects::obj_ops::obj_subscr;
use crate::objects::objtype::objexcept::{
    exception_set_cause, is_exception_instance, is_exception_type, new_exception_msg,
};
use crate::objects::primitives::objcell::{cell_get, cell_set};
use crate::objects::slots::SubscrArg;
use crate::objects::value::{Obj, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::vm::{sync_frame_roots, VmState};
use crate::runtime::vm_dispatch_exec::dispatch_exec;
pub use crate::runtime::vm_dispatch_util::DispatchOutcome;
use crate::runtime::vm_dispatch_util::{
    bc_byte, binary_op_from_ordinal, decode_int, decode_qstr, decode_slabel, decode_uint,
    decode_ulabel, fast_local, jump_by, list_slot, list_slot_mut, new_name_error_local,
    new_no_active_exc, new_not_impl_opcode, scan_prev_exc, spop, spush, spush_null, sset, sget,
    stop, store_fast_local, unary_op_from_ordinal, unwind_jump, unwind_return, Flow,
};
use crate::runtime::vm_ops::{exc_from_obj_error, type_error};
use crate::runtime::vm_raise::{exception_from_pending, make_raise_obj};
use crate::runtime::vm_rt::{binary_op_ctx, is_true_ctx, unary_op_ctx};
use crate::runtime::vm_rt_iter::{getiter_vm_ctx, iternext_vm_ctx};
use crate::runtime::vm_rt_name::{
    delete_global_ctx, delete_name_ctx, load_attr_ctx, load_build_class_ctx, load_global_ctx,
    load_method_ctx, load_name_ctx, load_super_method_ctx, store_attr_ctx, store_global_ctx,
    store_name_ctx,
};

// ── the loop ─────────────────────────────────────────────────────────

/// Run the dispatch loop for one frame until it returns, yields,
/// or raises. `inject` is the injected exception (`gen.throw`);
/// it is raised immediately unless the frame sits on
/// `YIELD_FROM`. `mark` is the `roots.stack` length on
/// `execute_frame` entry (see `sync_frame_roots`).
pub(crate) fn dispatch_loop(
    ctx: &mut CallCtx,
    state: &mut VmState,
    frame: &mut CodeState,
    inject: &mut Option<Value>,
    mark: usize,
) -> DispatchOutcome {
    let code_idx = match ctx.arena.get(frame.fun) {
        Obj::FunBc(d) => d.code,
        _ => 0,
    };
    let mut ip = frame.ip;
    loop {
        // ── fetch ──
        // Poll the recorded raise first (the B2 polled
        // sentinel): a raise left by a bridge-wrapped call
        // routes to the `PENDING` arm with `ip` still at
        // that opcode's byte.
        if ctx.has_pending_raise() {
            if let Some(exc) = exception_from_pending(ctx, state) {
                return DispatchOutcome::Raised(exc);
            }
        }
        // Scheduler poll (Tier G, `vm.zig:110-114`): an
        // exception raised from an interrupt (`mp_sched_exception`
        // — C15b's `scheduler.rs`) is raised here, once per
        // iteration, after the recorded-raise poll.
        if let Some(exc) = crate::runtime::scheduler::pending_exc_check(state) {
            return DispatchOutcome::Raised(exc);
        }
        frame.ip = ip;
        sync_frame_roots(ctx, frame, mark);
        let op = bc_byte(state, code_idx, ip);
        ip += 1;
        // The injected exception (`gen.throw`) is raised at
        // once — unless the frame sits on `YIELD_FROM` (the
        // sub-generator gets it instead).
        if op != MP_BC_YIELD_FROM {
            if let Some(exc) = inject.take() {
                let exc = make_raise_obj(ctx, state, exc);
                return DispatchOutcome::Raised(exc);
            }
        }
        match op {
            // ── MULTI ranges (checked first; the named
            // opcodes all sit below 0x70) ──
            op if op >= MP_BC_LOAD_CONST_SMALL_INT_MULTI
                && op < MP_BC_LOAD_CONST_SMALL_INT_MULTI
                    + MP_BC_LOAD_CONST_SMALL_INT_MULTI_NUM as u8 =>
            {
                let n = op as i64 - MP_BC_LOAD_CONST_SMALL_INT_MULTI as i64
                    - MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS as i64;
                spush(frame, Value::SmallInt(n));
            }
            op if op >= MP_BC_LOAD_FAST_MULTI
                && op < MP_BC_LOAD_FAST_MULTI + MP_BC_LOAD_FAST_MULTI_NUM as u8 =>
            {
                let u = (op - MP_BC_LOAD_FAST_MULTI) as usize;
                match fast_local(frame, u) {
                    Some(v) => spush(frame, v),
                    None => {
                        return DispatchOutcome::Raised(new_name_error_local(ctx, state));
                    }
                }
            }
            op if op >= MP_BC_STORE_FAST_MULTI
                && op < MP_BC_STORE_FAST_MULTI + MP_BC_STORE_FAST_MULTI_NUM as u8 =>
            {
                let u = (op - MP_BC_STORE_FAST_MULTI) as usize;
                let v = spop(frame).unwrap_or(Value::None);
                store_fast_local(frame, u, v);
            }
            op if op >= MP_BC_UNARY_OP_MULTI
                && op < MP_BC_UNARY_OP_MULTI + MP_BC_UNARY_OP_MULTI_NUM as u8 =>
            {
                let ord = op - MP_BC_UNARY_OP_MULTI;
                let arg = spop(frame).unwrap_or(Value::None);
                let uop = match unary_op_from_ordinal(ord) {
                    Some(u) => u,
                    None => {
                        return DispatchOutcome::RetExc(new_not_impl_opcode(ctx, state));
                    }
                };
                match unary_op_ctx(ctx, state, uop, arg) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            op if op >= MP_BC_BINARY_OP_MULTI
                && op < MP_BC_BINARY_OP_MULTI + MP_BC_BINARY_OP_MULTI_NUM as u8 =>
            {
                let ord = op - MP_BC_BINARY_OP_MULTI;
                let rhs = spop(frame).unwrap_or(Value::None);
                let lhs = spop(frame).unwrap_or(Value::None);
                let bop = match binary_op_from_ordinal(ord) {
                    Some(b) => b,
                    None => {
                        return DispatchOutcome::RetExc(new_not_impl_opcode(ctx, state));
                    }
                };
                match binary_op_ctx(ctx, state, bop, lhs, rhs) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            // ── constants ──
            MP_BC_LOAD_CONST_FALSE => spush(frame, Value::Bool(false)),
            MP_BC_LOAD_CONST_TRUE => spush(frame, Value::Bool(true)),
            MP_BC_LOAD_CONST_NONE => spush(frame, Value::None),
            MP_BC_LOAD_NULL => spush_null(frame),
            MP_BC_LOAD_CONST_SMALL_INT => {
                let n = decode_int(state, code_idx, &mut ip);
                spush(frame, Value::SmallInt(n));
            }
            MP_BC_LOAD_CONST_STRING => {
                let q = decode_qstr(state, code_idx, &mut ip);
                spush(frame, Value::Qstr(q));
            }
            MP_BC_LOAD_CONST_OBJ => {
                let u = decode_uint(state, code_idx, &mut ip);
                let v = state
                    .code_store
                    .const_table(code_idx)
                    .get(u)
                    .copied()
                    .unwrap_or(Value::None);
                spush(frame, v);
            }
            // ── loads ──
            MP_BC_LOAD_FAST_N => {
                let u = decode_uint(state, code_idx, &mut ip);
                match fast_local(frame, u) {
                    Some(v) => spush(frame, v),
                    None => {
                        return DispatchOutcome::Raised(new_name_error_local(ctx, state));
                    }
                }
            }
            MP_BC_LOAD_DEREF => {
                let u = decode_uint(state, code_idx, &mut ip);
                let cell = fast_local(frame, u);
                let v = cell.and_then(|c| match c {
                    Value::Heap(r) => cell_get(ctx.arena, r),
                    _ => None,
                });
                match v {
                    Some(v) => spush(frame, v),
                    None => {
                        return DispatchOutcome::Raised(new_name_error_local(ctx, state));
                    }
                }
            }
            MP_BC_LOAD_NAME => {
                let q = decode_qstr(state, code_idx, &mut ip);
                match load_name_ctx(ctx, state, q) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_LOAD_GLOBAL => {
                let q = decode_qstr(state, code_idx, &mut ip);
                match load_global_ctx(ctx, state, q) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_LOAD_ATTR => {
                let q = decode_qstr(state, code_idx, &mut ip);
                let base = spop(frame).unwrap_or(Value::None);
                match load_attr_ctx(ctx, state, base, q) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_LOAD_METHOD => {
                let q = decode_qstr(state, code_idx, &mut ip);
                let obj = spop(frame).unwrap_or(Value::None);
                match load_method_ctx(ctx, state, obj, q) {
                    Ok((method, self_)) => {
                        spush(frame, method);
                        match self_ {
                            Some(s) => spush(frame, s),
                            None => spush_null(frame),
                        }
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_LOAD_SUPER_METHOD => {
                let q = decode_qstr(state, code_idx, &mut ip);
                let obj = spop(frame).unwrap_or(Value::None);
                let type_v = spop(frame).unwrap_or(Value::None);
                let type_ref = match type_v {
                    Value::Heap(r) => r,
                    _ => {
                        return DispatchOutcome::Raised(type_error(
                            ctx,
                            state,
                            "super() argument 1 must be type",
                        ));
                    }
                };
                match load_super_method_ctx(ctx, state, type_ref, obj, q) {
                    Ok((method, self_)) => {
                        spush(frame, method);
                        match self_ {
                            Some(s) => spush(frame, s),
                            None => spush_null(frame),
                        }
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_LOAD_BUILD_CLASS => match load_build_class_ctx(ctx, state) {
                Ok(v) => spush(frame, v),
                Err(e) => return DispatchOutcome::Raised(e),
            },
            MP_BC_LOAD_SUBSCR => {
                let index = spop(frame).unwrap_or(Value::None);
                let base = spop(frame).unwrap_or(Value::None);
                match list_slot(ctx, base, index) {
                    Some(v) => spush(frame, v),
                    None => match obj_subscr(ctx, ctx.types, base, index, SubscrArg::Get) {
                        Ok(v) => spush(frame, v),
                        Err(e) => {
                            return DispatchOutcome::Raised(exc_from_obj_error(ctx, state, e))
                        }
                    },
                }
            }
            // ── stores ──
            MP_BC_STORE_FAST_N => {
                let u = decode_uint(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                store_fast_local(frame, u, v);
            }
            MP_BC_STORE_DEREF => {
                let u = decode_uint(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                if let Some(Value::Heap(r)) = fast_local(frame, u) {
                    cell_set(ctx.arena, r, v);
                }
            }
            MP_BC_STORE_NAME => {
                let q = decode_qstr(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                if let Err(e) = store_name_ctx(ctx, state, q, v) {
                    return DispatchOutcome::Raised(e);
                }
            }
            MP_BC_STORE_GLOBAL => {
                let q = decode_qstr(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                if let Err(e) = store_global_ctx(ctx, state, q, v) {
                    return DispatchOutcome::Raised(e);
                }
            }
            MP_BC_STORE_ATTR => {
                // (`vm.zig:657-662`): `mp_store_attr(sp[0], q,
                // (sp - 1)[0])` — `sp[0]` is the TOP of the
                // stack, so the stack is `[value, base]` with
                // the BASE on top (the compiler pushes the value
                // first, as for `STORE_SUBSCR`'s `[value, base,
                // index]` — C13's note). Pop the base first.
                let q = decode_qstr(state, code_idx, &mut ip);
                let base = spop(frame).unwrap_or(Value::None);
                let v = spop(frame).unwrap_or(Value::None);
                if let Err(e) = store_attr_ctx(ctx, state, base, q, Some(v)) {
                    return DispatchOutcome::Raised(e);
                }
            }
            MP_BC_STORE_SUBSCR => {
                // (`vm.zig:663-674`): the stack is `[value,
                // base, index]` — index on TOP, value at the
                // bottom (the compiler pushes the value first).
                // `value == NULL` is `del x[i]` (DELETE_SUBSCR
                // = LOAD_NULL + STORE_SUBSCR). The `listSlot`
                // fast path writes in place when the base is
                // an exact list and the index is in range.
                let index = spop(frame).unwrap_or(Value::None);
                let base = spop(frame).unwrap_or(Value::None);
                let value = spop(frame);
                match value {
                    None => {
                        if let Err(e) =
                            obj_subscr(ctx, ctx.types, base, index, SubscrArg::Delete)
                        {
                            return DispatchOutcome::Raised(exc_from_obj_error(ctx, state, e));
                        }
                    }
                    Some(v) => {
                        if let Some(slot) = list_slot_mut(ctx, base, index) {
                            *slot = v;
                        } else if let Err(e) =
                            obj_subscr(ctx, ctx.types, base, index, SubscrArg::Set(v))
                        {
                            return DispatchOutcome::Raised(exc_from_obj_error(ctx, state, e));
                        }
                    }
                }
            }
            // ── deletes ──
            MP_BC_DELETE_FAST => {
                let u = decode_uint(state, code_idx, &mut ip);
                // Clear the slot to NULL (unbound); the Zig
                // writes `MP_OBJ_NULL` into the slot.
                if let Some(idx) = frame.n_state.checked_sub(1 + u) {
                    if let Some(slot) = frame.state.get_mut(idx) {
                        *slot = None;
                    }
                }
            }
            MP_BC_DELETE_DEREF => {
                let u = decode_uint(state, code_idx, &mut ip);
                if let Some(Value::Heap(r)) = fast_local(frame, u) {
                    if let Obj::Cell(c) = ctx.arena.get_mut(r) {
                        c.value = None;
                    }
                }
            }
            MP_BC_DELETE_NAME => {
                let q = decode_qstr(state, code_idx, &mut ip);
                delete_name_ctx(ctx, state, q);
            }
            MP_BC_DELETE_GLOBAL => {
                let q = decode_qstr(state, code_idx, &mut ip);
                delete_global_ctx(ctx, state, q);
            }
            // ── stack ops ──
            MP_BC_DUP_TOP => {
                let v = stop(frame);
                match v {
                    Some(v) => spush(frame, v),
                    None => spush_null(frame),
                }
            }
            MP_BC_DUP_TOP_TWO => {
                let a = sget(frame, 1);
                let b = stop(frame);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        spush(frame, a);
                        spush(frame, b);
                    }
                    _ => {
                        spush_null(frame);
                        spush_null(frame);
                    }
                }
            }
            MP_BC_POP_TOP => {
                spop(frame);
            }
            MP_BC_ROT_TWO => {
                let a = sget(frame, 0);
                let b = sget(frame, 1);
                sset(frame, 0, b.unwrap_or(Value::None));
                sset(frame, 1, a.unwrap_or(Value::None));
            }
            MP_BC_ROT_THREE => {
                let a = sget(frame, 0);
                let b = sget(frame, 1);
                let c = sget(frame, 2);
                sset(frame, 0, b.unwrap_or(Value::None));
                sset(frame, 1, c.unwrap_or(Value::None));
                sset(frame, 2, a.unwrap_or(Value::None));
            }
            // ── jumps ──
            MP_BC_UNWIND_JUMP => {
                let off = decode_slabel(state, code_idx, &mut ip);
                let dest = ip.wrapping_add(off);
                let count = bc_byte(state, code_idx, ip);
                ip += 1;
                spush(frame, Value::SmallInt(dest as i64));
                spush(frame, Value::SmallInt(count as i64));
                match unwind_jump(frame, &mut ip) {
                    Flow::Dispatch => {}
                    Flow::Ret => {
                        // `unwindJump` never returns `.ret` in
                        // the Zig (it always dispatches) — this
                        // arm is unreachable.
                        return DispatchOutcome::Normal(stop(frame).unwrap_or(Value::None));
                    }
                }
            }
            MP_BC_JUMP => {
                let off = decode_slabel(state, code_idx, &mut ip);
                jump_by(&mut ip, off);
            }
            MP_BC_POP_JUMP_IF_TRUE => {
                let off = decode_slabel(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                match is_true_ctx(ctx, state, v) {
                    Ok(true) => jump_by(&mut ip, off),
                    Ok(false) => {}
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_POP_JUMP_IF_FALSE => {
                let off = decode_slabel(state, code_idx, &mut ip);
                let v = spop(frame).unwrap_or(Value::None);
                match is_true_ctx(ctx, state, v) {
                    Ok(false) => jump_by(&mut ip, off),
                    Ok(true) => {}
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_JUMP_IF_TRUE_OR_POP => {
                // (`vm.zig:751-756`): the `*_OR_POP` jumps take an
                // UNSIGNED label (`decodeUlabel`), NOT the signed
                // one — the signed decode subtracts 0x40, so a
                // taken jump landed 64 bytes early and the next
                // fetch decoded garbage (`NotImplementedError`
                // "opcode"). Found by Lane D3 bisecting
                // `struct.pack` in an `and` chain; fixed at the
                // D3 merge (integration).
                let off = decode_ulabel(state, code_idx, &mut ip);
                let v = stop(frame).unwrap_or(Value::None);
                match is_true_ctx(ctx, state, v) {
                    Ok(true) => jump_by(&mut ip, off),
                    Ok(false) => {
                        spop(frame);
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_JUMP_IF_FALSE_OR_POP => {
                // Unsigned label too (`vm.zig:758-761`) — see the
                // `JUMP_IF_TRUE_OR_POP` arm above.
                let off = decode_ulabel(state, code_idx, &mut ip);
                let v = stop(frame).unwrap_or(Value::None);
                match is_true_ctx(ctx, state, v) {
                    Ok(false) => jump_by(&mut ip, off),
                    Ok(true) => {
                        spop(frame);
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_GET_ITER => {
                let v = spop(frame).unwrap_or(Value::None);
                match getiter_vm_ctx(ctx, state, v) {
                    Ok(it) => spush(frame, it),
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_GET_ITER_STACK => {
                let v = spop(frame).unwrap_or(Value::None);
                match getiter_vm_ctx(ctx, state, v) {
                    Ok(it) => {
                        spush_null(frame);
                        spush(frame, it);
                        spush_null(frame);
                        spush_null(frame);
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            MP_BC_FOR_ITER => {
                let off = decode_ulabel(state, code_idx, &mut ip);
                // The iterator buffer base is `len - 4`
                // (`GET_ITER_STACK` pushed `[None, it, None,
                // None]`); the iterator itself is at `len -
                // 3`. A `None` base means the iterator was
                // exhausted by a prior `FOR_ITER` in the same
                // buffer — use the iterator at `len - 3`.
                let len = frame.stack.len();
                let it = if len >= 4 && frame.stack[len - 4].is_none() {
                    frame.stack.get(len - 3).copied().flatten()
                } else {
                    frame.stack.get(len - 4).copied().flatten()
                };
                let it = match it {
                    Some(v) => v,
                    None => {
                        let n = frame.stack.len();
                        frame.stack.truncate(n.saturating_sub(4));
                        jump_by(&mut ip, off);
                        continue;
                    }
                };
                match iternext_vm_ctx(ctx, state, it) {
                    Ok(Some(v)) => spush(frame, v),
                    Ok(None) => {
                        let n = frame.stack.len();
                        frame.stack.truncate(n.saturating_sub(4));
                        jump_by(&mut ip, off);
                    }
                    Err(e) => return DispatchOutcome::Raised(e),
                }
            }
            // ── return / raise ──
            MP_BC_RETURN_VALUE => {
                // The return value stays on top (`unwindReturn`
                // reads it from `sp[0]`); `.ret` returns it
                // as the frame's normal outcome.
                match unwind_return(frame, &mut ip) {
                    Flow::Dispatch => {}
                    Flow::Ret => {
                        return DispatchOutcome::Normal(stop(frame).unwrap_or(Value::None));
                    }
                }
            }
            MP_BC_RAISE_LAST => match scan_prev_exc(frame) {
                Some(exc) => return DispatchOutcome::Raised(exc),
                None => return DispatchOutcome::Raised(new_no_active_exc(ctx, state)),
            },
            MP_BC_RAISE_OBJ => {
                let exc = spop(frame).unwrap_or(Value::None);
                let exc = make_raise_obj(ctx, state, exc);
                return DispatchOutcome::Raised(exc);
            }
            MP_BC_RAISE_FROM => {
                // (`vm.zig:1055-1070`): `raise exc from cause`.
                // A non-`None` cause must be an exception
                // instance or type (else `TypeError`); a class
                // cause is instantiated, `None` suppresses the
                // context.
                let cause = spop(frame).unwrap_or(Value::None);
                let exc = spop(frame).unwrap_or(Value::None);
                if cause != Value::None
                    && !is_exception_instance(ctx.arena, cause, state.exc.base_exception)
                    && !is_exception_type(ctx.arena, cause, state.exc.base_exception)
                {
                    return DispatchOutcome::Raised(new_exception_msg(
                        ctx,
                        state.exc.type_error,
                        "exception causes must derive from BaseException",
                    ));
                }
                let exc = make_raise_obj(ctx, state, exc);
                let cause = if cause == Value::None {
                    cause
                } else {
                    make_raise_obj(ctx, state, cause)
                };
                exception_set_cause(ctx, exc, cause);
                return DispatchOutcome::Raised(exc);
            }
            // ── yields / builders / unpack / make / call /
            // imports — the second half of `dispatchBody`
            // (`vm_dispatch_exec`) ──
            MP_BC_YIELD_VALUE
            | MP_BC_YIELD_FROM
            | MP_BC_SETUP_WITH
            | MP_BC_SETUP_EXCEPT
            | MP_BC_SETUP_FINALLY
            | MP_BC_POP_EXCEPT_JUMP
            | MP_BC_WITH_CLEANUP
            | MP_BC_END_FINALLY
            | MP_BC_BUILD_TUPLE
            | MP_BC_BUILD_LIST
            | MP_BC_BUILD_MAP
            | MP_BC_BUILD_SET
            | MP_BC_BUILD_SLICE
            | MP_BC_STORE_MAP
            | MP_BC_STORE_COMP
            | MP_BC_UNPACK_SEQUENCE
            | MP_BC_UNPACK_EX
            | MP_BC_MAKE_FUNCTION
            | MP_BC_MAKE_FUNCTION_DEFARGS
            | MP_BC_MAKE_CLOSURE
            | MP_BC_MAKE_CLOSURE_DEFARGS
            | MP_BC_CALL_FUNCTION
            | MP_BC_CALL_FUNCTION_VAR_KW
            | MP_BC_CALL_METHOD
            | MP_BC_CALL_METHOD_VAR_KW
            | MP_BC_IMPORT_NAME
            | MP_BC_IMPORT_FROM
            | MP_BC_IMPORT_STAR => {
                if let Some(out) =
                    dispatch_exec(ctx, state, frame, code_idx, &mut ip, op, inject)
                {
                    return out;
                }
            }
            // ── unknown opcode → `NotImplementedError`,
            // returned directly (bypasses the handler) ──
            _ => {
                return DispatchOutcome::RetExc(new_not_impl_opcode(ctx, state));
            }
        }
    }
}
