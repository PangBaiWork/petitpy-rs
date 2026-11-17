//! Dispatch helpers — the value-stack, bytecode-decode,
//! unwind, and small-error-constructor pieces of the dispatch
//! loop (`vm.zig:92-99`, `vm.zig:287-330`, `vm.zig:449-498`),
//! shared by `vm_dispatch` and `vm_dispatch_exec`.
//!
//! The stack helpers operate on `frame.stack: Vec<Option<Value>>`
//! (`None` == `MP_OBJ_NULL`).

use crate::compiler::emit::consts::MP_OBJ_ITER_BUF_NSLOTS;
use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::vm::VmState;

/// What one `dispatch_loop` run returns (the Zig's `Ctx.outcome`).
pub enum DispatchOutcome {
    /// `unwindReturn` ran out of handlers: the frame returned
    /// normally; the value is the stack top.
    Normal(Value),
    /// `YIELD_VALUE` / `YIELD_FROM` (sub-generator yielded).
    Yield(Value),
    /// A raise: `exception_handler` decides catch-or-propagate.
    Raised(Value),
    /// Return the exception directly, bypassing the handler
    /// (unknown opcode → `NotImplementedError("opcode")`).
    RetExc(Value),
}

// ── value-stack helpers (`frame.stack: Vec<Option<Value>>`,
// `None` == `MP_OBJ_NULL`) ───────────────────────────────────────────

/// PUSH(v).
pub(crate) fn spush(frame: &mut CodeState, v: Value) {
    frame.stack.push(Some(v));
}

/// PUSH(MP_OBJ_NULL).
pub(crate) fn spush_null(frame: &mut CodeState) {
    frame.stack.push(None);
}

/// POP() — `None` when the slot held NULL or the stack is empty.
pub(crate) fn spop(frame: &mut CodeState) -> Option<Value> {
    frame.stack.pop().flatten()
}

/// TOP() — `None` when the top slot holds NULL or the stack is
/// empty.
pub(crate) fn stop(frame: &CodeState) -> Option<Value> {
    frame.stack.last().copied().flatten()
}

/// SET_TOP(v).
pub(crate) fn sset_top(frame: &mut CodeState, v: Value) {
    if let Some(t) = frame.stack.last_mut() {
        *t = Some(v);
    } else {
        frame.stack.push(Some(v));
    }
}

/// `sp[-depth]` — depth 0 is the top.
pub(crate) fn sget(frame: &CodeState, depth: usize) -> Option<Value> {
    let len = frame.stack.len();
    if depth < len {
        frame.stack[len - 1 - depth]
    } else {
        None
    }
}

/// `sp[-depth] = v`.
pub(crate) fn sset(frame: &mut CodeState, depth: usize, v: Value) {
    let len = frame.stack.len();
    if depth < len {
        frame.stack[len - 1 - depth] = Some(v);
    }
}

// ── bytecode decodes (a local `ip` cursor over the code
// store's `fun_data`; out-of-bounds reads yield 0, never a
// panic — malformed bytecode ends in the unknown-opcode arm) ──

pub(crate) fn bc_byte(state: &VmState, code_idx: usize, ip: usize) -> u8 {
    state
        .code_store
        .code(code_idx)
        .fun_data
        .get(ip)
        .copied()
        .unwrap_or(0)
}

/// `decodeUint` (`vm.zig:287-298`): a varint (7 bits/byte,
/// high bit = continue).
pub(crate) fn decode_uint(state: &VmState, code_idx: usize, ip: &mut usize) -> usize {
    let mut unum: usize = 0;
    loop {
        let b = bc_byte(state, code_idx, *ip);
        unum = (unum << 7) + (b & 0x7f) as usize;
        *ip += 1;
        if b & 0x80 == 0 {
            break;
        }
    }
    unum
}

/// `decodeInt` — the `LOAD_CONST_SMALL_INT` signed decode
/// (`vm.zig:539-548`): bit 6 of the FIRST byte is the sign;
/// when set, the accumulator starts at all-ones so the
/// left-shifts sign-extend. The result is bit-cast to `i64`.
pub(crate) fn decode_int(state: &VmState, code_idx: usize, ip: &mut usize) -> i64 {
    let mut num: usize = 0;
    if bc_byte(state, code_idx, *ip) & 0x40 != 0 {
        num = num.wrapping_sub(1);
    }
    loop {
        let b = bc_byte(state, code_idx, *ip);
        num = (num << 7) | (b & 0x7f) as usize;
        *ip += 1;
        if b & 0x80 == 0 {
            break;
        }
    }
    num as i64
}

/// `decodeQstr` (`vm.zig:300-306`): a varint index into the
/// module's qstr table.
pub(crate) fn decode_qstr(state: &VmState, code_idx: usize, ip: &mut usize) -> u32 {
    let i = decode_uint(state, code_idx, ip);
    state
        .code_store
        .qstr_table(code_idx)
        .get(i)
        .copied()
        .unwrap_or(0) as u32
}

/// `decodeUlabel` (`vm.zig:307-317`): a 1- or 2-byte unsigned
/// label (the second byte carries the high 7 bits when the
/// first has its high bit set).
pub(crate) fn decode_ulabel(state: &VmState, code_idx: usize, ip: &mut usize) -> usize {
    let b0 = bc_byte(state, code_idx, *ip);
    if b0 & 0x80 != 0 {
        let v = ((b0 & 0x7f) as usize) | ((bc_byte(state, code_idx, *ip + 1) as usize) << 7);
        *ip += 2;
        v
    } else {
        *ip += 1;
        b0 as usize
    }
}

/// `decodeSlabel` (`vm.zig:318-326`): a 1- or 2-byte signed
/// label (bias 0x40 / 0x4000), held in a `usize` with wrapping
/// arithmetic (`jumpBy` adds with `+%`).
pub(crate) fn decode_slabel(state: &VmState, code_idx: usize, ip: &mut usize) -> usize {
    let b0 = bc_byte(state, code_idx, *ip);
    if b0 & 0x80 != 0 {
        let v = (((b0 & 0x7f) as usize) | ((bc_byte(state, code_idx, *ip + 1) as usize) << 7))
            .wrapping_sub(0x4000);
        *ip += 2;
        v
    } else {
        *ip += 1;
        (b0 as usize).wrapping_sub(0x40)
    }
}

/// `jumpBy` (`vm.zig:328-330`): `ip += off` (wrapping).
pub(crate) fn jump_by(ip: &mut usize, off: usize) {
    *ip = ip.wrapping_add(off);
}

// ── small error constructors (`vm.zig:92-99`) ───────────────────────

pub(crate) fn new_name_error_local(ctx: &mut CallCtx, state: &VmState) -> Value {
    new_exception_msg(
        ctx,
        state.exc.name_error,
        "local variable referenced before assignment",
    )
}

pub(crate) fn new_no_active_exc(ctx: &mut CallCtx, state: &VmState) -> Value {
    new_exception_msg(ctx, state.exc.runtime_error, "no active exception to reraise")
}

pub(crate) fn new_not_impl_opcode(ctx: &mut CallCtx, state: &VmState) -> Value {
    new_exception_msg(ctx, state.exc.not_implemented_error, "opcode")
}

// ── unwind helpers (`vm.zig:449-498`) ───────────────────────────────

/// `cancelActiveFinally` (`vm.zig:447-448`): drop the slots
/// below the top value, keeping the top value itself.
pub(crate) fn cancel_active_finally(frame: &mut CodeState) {
    let y = spop(frame).unwrap_or(Value::None);
    if matches!(stop(frame), Some(Value::SmallInt(_))) {
        spop(frame);
    }
    sset_top(frame, y);
}

/// `unwindReturn` / `unwindJump` outcome: keep dispatching at
/// the (rewritten) `ip`, or return from the frame.
pub(crate) enum Flow {
    Dispatch,
    Ret,
}

/// `unwindReturn` (`vm.zig:449-470`): walk the handler stack
/// for a `finally` whose handler is ahead of `ip`; enter it
/// with the return value + a `-1` sentinel, else pop blocks
/// until none remain (return normally — the value is the stack
/// top).
pub(crate) fn unwind_return(frame: &mut CodeState, ip: &mut usize) -> Flow {
    while frame.exc_sp_idx >= 1 {
        let (with_flag, handler, val_sp) = match frame.exc_stack.last() {
            Some(b) => (b.with_flag, b.handler, b.val_sp),
            None => break,
        };
        if with_flag {
            if handler >= *ip {
                let ret = stop(frame).unwrap_or(Value::None);
                frame.stack.truncate(val_sp);
                frame.stack.push(Some(ret));
                frame.stack.push(Some(Value::SmallInt(-1)));
                *ip = handler;
                return Flow::Dispatch;
            } else {
                cancel_active_finally(frame);
            }
        }
        frame.exc_stack.pop();
        frame.exc_sp_idx = frame.exc_stack.len();
    }
    Flow::Ret
}

/// `unwindJump` (`vm.zig:471-498`): pop the `(dest, count)`
/// pair the `UNWIND_JUMP` opcode pushed; the count's low 7
/// bits are the blocks to unwind, bit 7 says to pop the
/// iterator buffer. A `finally` ahead of `ip` is entered with
/// the decremented count as its sentinel; otherwise blocks
/// are popped until the count is spent, then `ip = dest`.
pub(crate) fn unwind_jump(frame: &mut CodeState, ip: &mut usize) -> Flow {
    let mut unum = match spop(frame) {
        Some(Value::SmallInt(n)) => n as usize,
        _ => 0,
    };
    while (unum & 0x7f) > 0 {
        unum -= 1;
        let (with_flag, handler) = match frame.exc_stack.last() {
            Some(b) => (b.with_flag, b.handler),
            None => (false, 0),
        };
        if frame.exc_sp_idx >= 1 && with_flag {
            if handler >= *ip {
                spush(frame, Value::SmallInt(unum as i64));
                *ip = handler;
                return Flow::Dispatch;
            } else {
                cancel_active_finally(frame);
            }
        }
        frame.exc_stack.pop();
        frame.exc_sp_idx = frame.exc_stack.len();
    }
    let dest = match spop(frame) {
        Some(Value::SmallInt(n)) => n as usize,
        _ => 0,
    };
    *ip = dest;
    if unum != 0 {
        let n = frame.stack.len();
        frame
            .stack
            .truncate(n.saturating_sub(MP_OBJ_ITER_BUF_NSLOTS));
    }
    Flow::Dispatch
}

/// `excSpScanPrevExc` (the `RAISE_LAST` scan): the most recent
/// `prev_exc` on the handler stack, top-down.
pub(crate) fn scan_prev_exc(frame: &CodeState) -> Option<Value> {
    frame.exc_stack.iter().rev().find_map(|b| b.prev_exc)
}

/// A local slot (`frame.state[n_state - 1 - u]`); `None` when
/// the slot is unbound (`MP_OBJ_NULL`).
pub(crate) fn fast_local(frame: &CodeState, u: usize) -> Option<Value> {
    let idx = frame.n_state.checked_sub(1 + u)?;
    frame.state.get(idx).copied().flatten()
}

pub(crate) fn store_fast_local(frame: &mut CodeState, u: usize, v: Value) {
    if let Some(idx) = frame.n_state.checked_sub(1 + u) {
        if let Some(slot) = frame.state.get_mut(idx) {
            *slot = Some(v);
        }
    }
}

/// `listSlot` (`vm.zig:376-383`): `list[small_int]` for an
/// exact list and an in-range index. `None` falls back to
/// `obj_subscr` (subclasses, slices, the IndexError).
pub(crate) fn list_slot(ctx: &CallCtx, base: Value, index: Value) -> Option<Value> {
    let Value::SmallInt(mut i) = index else {
        return None;
    };
    let Value::Heap(r) = base else {
        return None;
    };
    if obj_get_type(ctx.arena, ctx.types, base) != Some(ctx.types.list) {
        return None;
    }
    let Obj::List(d) = ctx.arena.get(r) else {
        return None;
    };
    if i < 0 {
        i += d.items.len() as i64;
    }
    if i < 0 || i as usize >= d.items.len() {
        return None;
    }
    Some(d.items[i as usize])
}

/// The mutable `STORE_SUBSCR` fast path (`vm.zig:665-668`):
/// exact list + in-range small-int index (negative wraps) →
/// a `&mut` slot into the list's items; `None` → fall back
/// to `obj_subscr`. Mirrors `listSlot` (the read half above).
pub(crate) fn list_slot_mut<'a>(
    ctx: &'a mut CallCtx,
    base: Value,
    index: Value,
) -> Option<&'a mut Value> {
    let Value::SmallInt(mut i) = index else {
        return None;
    };
    let Value::Heap(r) = base else {
        return None;
    };
    if ctx.arena.header(r).type_ != Some(ctx.types.list) {
        return None;
    }
    let len = match ctx.arena.get(r) {
        Obj::List(d) => d.items.len(),
        _ => return None,
    };
    if i < 0 {
        i += len as i64;
    }
    if i < 0 || i as usize >= len {
        return None;
    }
    match ctx.arena.get_mut(r) {
        Obj::List(d) => Some(&mut d.items[i as usize]),
        _ => None,
    }
}

/// Convert a `UNARY_OP_MULTI` ordinal (0..4) to its `UnaryOp`.
pub(crate) fn unary_op_from_ordinal(op: u8) -> Option<UnaryOp> {
    match op {
        0 => Some(UnaryOp::Positive),
        1 => Some(UnaryOp::Negative),
        2 => Some(UnaryOp::Invert),
        3 => Some(UnaryOp::Not),
        _ => None,
    }
}

/// Convert a `BINARY_OP_MULTI` ordinal (0..35) to its
/// `BinaryOp` (the ordinals match the `BinaryOp`
/// discriminants, `slots.rs`).
pub(crate) fn binary_op_from_ordinal(op: u8) -> Option<BinaryOp> {
    Some(match op {
        0 => BinaryOp::Less,
        1 => BinaryOp::More,
        2 => BinaryOp::Equal,
        3 => BinaryOp::LessEqual,
        4 => BinaryOp::MoreEqual,
        5 => BinaryOp::NotEqual,
        6 => BinaryOp::In,
        7 => BinaryOp::Is,
        8 => BinaryOp::ExceptionMatch,
        9 => BinaryOp::InplaceOr,
        10 => BinaryOp::InplaceXor,
        11 => BinaryOp::InplaceAnd,
        12 => BinaryOp::InplaceLshift,
        13 => BinaryOp::InplaceRshift,
        14 => BinaryOp::InplaceAdd,
        15 => BinaryOp::InplaceSubtract,
        16 => BinaryOp::InplaceMultiply,
        17 => BinaryOp::InplaceMatMultiply,
        18 => BinaryOp::InplaceFloorDivide,
        19 => BinaryOp::InplaceTrueDivide,
        20 => BinaryOp::InplaceModulo,
        21 => BinaryOp::InplacePower,
        22 => BinaryOp::Or,
        23 => BinaryOp::Xor,
        24 => BinaryOp::And,
        25 => BinaryOp::Lshift,
        26 => BinaryOp::Rshift,
        27 => BinaryOp::Add,
        28 => BinaryOp::Subtract,
        29 => BinaryOp::Multiply,
        30 => BinaryOp::MatMultiply,
        31 => BinaryOp::FloorDivide,
        32 => BinaryOp::TrueDivide,
        33 => BinaryOp::Modulo,
        34 => BinaryOp::Power,
        _ => return None,
    })
}
