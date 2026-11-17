//! The dispatch arms for yields, builders, unpack, make, call,
//! and imports (`vm.zig:828-930`, `vm.zig:1067-1103`) — the second
//! half of `dispatchBody`, split out of `vm_dispatch` so no file
//! exceeds 800 lines. `dispatch_exec` returns `Some(outcome)`
//! when the arm returns from the frame, `None` when it falls
//! through to the next iteration.

use crate::compiler::emit::consts::{
    MP_BC_BUILD_LIST, MP_BC_BUILD_MAP, MP_BC_BUILD_SET, MP_BC_BUILD_SLICE, MP_BC_BUILD_TUPLE,
    MP_BC_CALL_FUNCTION, MP_BC_CALL_FUNCTION_VAR_KW, MP_BC_CALL_METHOD, MP_BC_CALL_METHOD_VAR_KW,
    MP_BC_END_FINALLY, MP_BC_IMPORT_FROM, MP_BC_IMPORT_NAME, MP_BC_IMPORT_STAR, MP_BC_MAKE_CLOSURE,
    MP_BC_MAKE_CLOSURE_DEFARGS, MP_BC_MAKE_FUNCTION, MP_BC_MAKE_FUNCTION_DEFARGS,
    MP_BC_POP_EXCEPT_JUMP, MP_BC_SETUP_EXCEPT, MP_BC_SETUP_FINALLY, MP_BC_SETUP_WITH,
    MP_BC_STORE_COMP, MP_BC_STORE_MAP, MP_BC_UNPACK_EX, MP_BC_UNPACK_SEQUENCE,
    MP_BC_WITH_CLEANUP, MP_BC_YIELD_FROM, MP_BC_YIELD_VALUE,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_new, dict_store};
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objlist_methods::list_append;
use crate::objects::containers::objset::{set_new, set_store};
use crate::objects::containers::objslice::slice_make_new;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::objtype::objexcept::exception_match;
use crate::objects::value::Value;
use crate::runtime::bc::{CodeState, ExcBlock};
use crate::runtime::vm::VmState;
use crate::runtime::vm_dispatch_util::{
    bc_byte, decode_qstr, decode_uint, decode_ulabel, jump_by, sget, spop, spush, spush_null,
    sset_top, stop, unwind_jump, unwind_return, DispatchOutcome, Flow,
};
use crate::runtime::vm_call::{
    call_function_ctx, call_method_ctx, call_method_var_kw_ctx,
};
use crate::runtime::vm_ops::{exc_from_obj_error, make_closure_ctx, make_function_ctx};
use crate::runtime::vm_rt::is_true_ctx;
use crate::runtime::vm_rt_name::load_method_ctx;
use crate::runtime::vm_raise::make_raise_obj;
use crate::runtime::vm_rt_iter::{
    import_all_ctx, import_from_ctx, import_name_ctx, prepare_var_kw, resume_vm_ctx,
    unpack_ex_ctx, unpack_sequence_ctx, ResumeOutcome,
};

/// Run one yield/builder/unpack/make/call/import opcode. Returns
/// `Some(outcome)` when the arm returns from the frame (yield or
/// raise), `None` when it falls through to the next iteration.
pub(crate) fn dispatch_exec(
    ctx: &mut CallCtx,
    state: &mut VmState,
    frame: &mut CodeState,
    code_idx: usize,
    ip: &mut usize,
    op: u8,
    inject: &mut Option<Value>,
) -> Option<DispatchOutcome> {
    match op {
            MP_BC_SETUP_WITH => {
                // (`vm.zig:983-1002`): the manager is on top.
                // `__exit__` overwrites its slot (method,
                // self), `__enter__` is loaded two slots up
                // and called; if `__enter__` raised, the exc
                // block is NOT pushed (the raise propagates
                // to the outer handler). On success the stack
                // is `[.., exit_meth, exit_self, ret]` and
                // `val_sp` is the depth after `exit_meth` /
                // `exit_self` were written.
                let mgr = spop(frame).unwrap_or(Value::None);
                let exit_q = static_qstr_id(b"__exit__").unwrap_or(u32::MAX);
                let enter_q = static_qstr_id(b"__enter__").unwrap_or(u32::MAX);
                let (exit_meth, exit_self) = match load_method_ctx(ctx, state, mgr, exit_q) {
                    Ok(pair) => pair,
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                };
                let (enter_meth, enter_self) =
                    match load_method_ctx(ctx, state, mgr, enter_q) {
                        Ok(pair) => pair,
                        Err(e) => return Some(DispatchOutcome::Raised(e)),
                    };
                spush(frame, exit_meth);
                match exit_self {
                    Some(s) => spush(frame, s),
                    None => spush_null(frame),
                }
                let ret = match call_method_ctx(ctx, state, enter_meth, enter_self, 0, 0, &[]) {
                    Ok(v) => v,
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                };
                let off = decode_ulabel(state, code_idx, ip);
                let val_sp = frame.stack.len();
                frame.push_exc_block(ExcBlock {
                    handler: (*ip).wrapping_add(off),
                    val_sp,
                    with_flag: true,
                    prev_exc: None,
                });
                spush(frame, ret);
            }
            MP_BC_SETUP_EXCEPT => {
                let off = decode_ulabel(state, code_idx, ip);
                let val_sp = frame.stack.len();
                frame.push_exc_block(ExcBlock {
                    handler: (*ip).wrapping_add(off),
                    val_sp,
                    with_flag: false,
                    prev_exc: None,
                });
            }
            MP_BC_SETUP_FINALLY => {
                let off = decode_ulabel(state, code_idx, ip);
                let val_sp = frame.stack.len();
                frame.push_exc_block(ExcBlock {
                    handler: (*ip).wrapping_add(off),
                    val_sp,
                    with_flag: true,
                    prev_exc: None,
                });
            }
            MP_BC_POP_EXCEPT_JUMP => {
                let off = decode_ulabel(state, code_idx, ip);
                // Pop the exception block, restore the previous
                // exception, and jump.
                let _ = frame.exc_stack.pop();
                frame.exc_sp_idx = frame.exc_stack.len();
                jump_by(ip, off);
            }
            MP_BC_WITH_CLEANUP => {
                // (`vm.zig:1004-1034`): the stack is `[..,
                // exit_meth, exit_self, top]` where `top` is
                // `None` (normal exit), a small int (a
                // return/break/continue sentinel, with the
                // return value below it), or an exception.
                // `__exit__` is called with three args in
                // all three cases; the stack effects are:
                // normal → `[.., None]` (the return value is
                // ignored); sentinel → `[.., data, cause]`;
                // exception → `[.., None]` when swallowed,
                // `[.., exc]` when re-raised.
                let top = stop(frame).unwrap_or(Value::None);
                match top {
                    Value::None => {
                        let exit_self = sget(frame, 1);
                        let exit_meth = sget(frame, 2).unwrap_or(Value::None);
                        let args = [Value::None, Value::None, Value::None];
                        match call_method_ctx(ctx, state, exit_meth, exit_self, 3, 0, &args) {
                            Ok(_) => {
                                let n = frame.stack.len();
                                frame.stack.truncate(n.saturating_sub(3));
                                spush(frame, Value::None);
                            }
                            Err(e) => return Some(DispatchOutcome::Raised(e)),
                        }
                    }
                    Value::SmallInt(_) => {
                        let cause = spop(frame).unwrap_or(Value::None);
                        let data = spop(frame).unwrap_or(Value::None);
                        let exit_self = spop(frame);
                        let exit_meth = spop(frame).unwrap_or(Value::None);
                        let args = [Value::None, Value::None, Value::None];
                        match call_method_ctx(ctx, state, exit_meth, exit_self, 3, 0, &args) {
                            Ok(_) => {
                                spush(frame, data);
                                spush(frame, cause);
                            }
                            Err(e) => return Some(DispatchOutcome::Raised(e)),
                        }
                    }
                    exc => {
                        let exit_self = sget(frame, 1);
                        let exit_meth = sget(frame, 2).unwrap_or(Value::None);
                        let exc_type = obj_get_type(ctx.arena, ctx.types, exc)
                            .map(Value::Heap)
                            .unwrap_or(Value::None);
                        let args = [exc_type, exc, Value::None];
                        match call_method_ctx(ctx, state, exit_meth, exit_self, 3, 0, &args) {
                            Ok(v) => match is_true_ctx(ctx, state, v) {
                                Ok(true) => {
                                    // Swallowed: `[.., None]`.
                                    let n = frame.stack.len();
                                    frame.stack.truncate(n.saturating_sub(3));
                                    spush(frame, Value::None);
                                }
                                Ok(false) => {
                                    // Re-raised: `[.., exc]`.
                                    let n = frame.stack.len();
                                    frame.stack.truncate(n.saturating_sub(3));
                                    spush(frame, exc);
                                }
                                Err(e) => return Some(DispatchOutcome::Raised(e)),
                            },
                            Err(e) => return Some(DispatchOutcome::Raised(e)),
                        }
                    }
                }
            }
            MP_BC_END_FINALLY => {
                // Pop the exc block, then dispatch on the
                // top: `None` → pop and continue; a small
                // int → `< 0` means `unwindReturn`, `>= 0`
                // means push it back and `unwindJump`; an
                // exception → raise it.
                let _ = frame.exc_stack.pop();
                frame.exc_sp_idx = frame.exc_stack.len();
                match stop(frame) {
                    None => {
                        spop(frame);
                    }
                    Some(Value::None) => {
                        spop(frame);
                    }
                    Some(Value::SmallInt(cause)) => {
                        spop(frame);
                        if cause < 0 {
                            match unwind_return(frame, ip) {
                                Flow::Dispatch => {}
                                Flow::Ret => {
                                    return Some(DispatchOutcome::Normal(
                                        stop(frame).unwrap_or(Value::None),
                                    ));
                                }
                            }
                        } else {
                            spush(frame, Value::SmallInt(cause));
                            match unwind_jump(frame, ip) {
                                Flow::Dispatch => {}
                                Flow::Ret => {
                                    return Some(DispatchOutcome::Normal(
                                        stop(frame).unwrap_or(Value::None),
                                    ));
                                }
                            }
                        }
                    }
                    Some(exc) => {
                        return Some(DispatchOutcome::Raised(exc));
                    }
                }
            }
            // ── yield ──
            MP_BC_YIELD_VALUE => {
                // (`vm.zig:1067-1071`): `ip` is already past
                // the opcode; the value stays on top
                // (`execute_frame` overwrites it with the
                // send value on resume).
                frame.ip = *ip;
                return Some(DispatchOutcome::Yield(stop(frame).unwrap_or(Value::None)));
            }
            MP_BC_YIELD_FROM => {
                // (`vm.zig:1072-1103`): pop the send value;
                // the sub-generator is on top. An injected
                // exception replaces the send.
                let send_value = spop(frame).unwrap_or(Value::None);
                let sub = stop(frame).unwrap_or(Value::None);
                let throw_arg = inject.take();
                let send_arg = if throw_arg.is_some() {
                    None
                } else {
                    Some(send_value)
                };
                match resume_vm_ctx(ctx, state, sub, send_arg, throw_arg) {
                    ResumeOutcome::Yield(ret_value) => {
                        // Rewind `ip` to the `YIELD_FROM`
                        // opcode byte, push the yielded value,
                        // and yield it up.
                        *ip -= 1;
                        spush(frame, ret_value);
                        frame.ip = *ip;
                        return Some(DispatchOutcome::Yield(ret_value));
                    }
                    ResumeOutcome::Normal(ret_value) => {
                        // The sub-generator returned: replace
                        // it with its return value. A thrown
                        // `GeneratorExit` is re-raised here.
                        sset_top(frame, ret_value);
                        if let Some(exc) = throw_arg {
                            if exception_match(
                                ctx.arena,
                                exc,
                                Value::Heap(state.exc.generator_exit),
                            ) {
                                let exc = make_raise_obj(ctx, state, exc);
                                return Some(DispatchOutcome::Raised(exc));
                            }
                        }
                    }
                    ResumeOutcome::Exception(exc) => {
                        // Pop the exhausted sub-generator.
                        spop(frame);
                        return Some(DispatchOutcome::Raised(exc));
                    }
                }
            }
            // ── builders ──
            MP_BC_BUILD_TUPLE => {
                let u = decode_uint(state, code_idx, ip);
                let n = frame.stack.len();
                let start = n.saturating_sub(u);
                let items: Vec<Value> = frame
                    .stack
                    .split_off(start)
                    .into_iter()
                    .map(|v| v.unwrap_or(Value::None))
                    .collect();
                let t = new_tuple(ctx, ctx.types.tuple, &items);
                spush(frame, Value::Heap(t));
            }
            MP_BC_BUILD_LIST => {
                let u = decode_uint(state, code_idx, ip);
                let n = frame.stack.len();
                let start = n.saturating_sub(u);
                let items: Vec<Value> = frame
                    .stack
                    .split_off(start)
                    .into_iter()
                    .map(|v| v.unwrap_or(Value::None))
                    .collect();
                let l = new_list(ctx, ctx.types.list, &items);
                spush(frame, Value::Heap(l));
            }
            MP_BC_BUILD_MAP => {
                let u = decode_uint(state, code_idx, ip);
                let d = dict_new(ctx, u);
                spush(frame, d);
            }
            MP_BC_BUILD_SET => {
                let u = decode_uint(state, code_idx, ip);
                let n = frame.stack.len();
                let start = n.saturating_sub(u);
                let items: Vec<Value> = frame
                    .stack
                    .split_off(start)
                    .into_iter()
                    .map(|v| v.unwrap_or(Value::None))
                    .collect();
                match set_new(ctx, ctx.types.set, &items) {
                    Ok(v) => spush(frame, v),
                    Err(e) => {
                        return Some(DispatchOutcome::Raised(exc_from_obj_error(
                            ctx, state, e,
                        )))
                    }
                }
            }
            MP_BC_BUILD_SLICE => {
                // (`vm.zig:828-833`): a 1-byte arg selects
                // 2-arg (start, stop) or 3-arg (start, stop,
                // step); any other value behaves as 2-arg.
                let n_args = bc_byte(state, code_idx, *ip);
                *ip += 1;
                let mut step = Value::None;
                if n_args == 3 {
                    step = spop(frame).unwrap_or(Value::None);
                }
                let stop_v = spop(frame).unwrap_or(Value::None);
                let start = spop(frame).unwrap_or(Value::None);
                let args = if n_args == 3 {
                    vec![start, stop_v, step]
                } else {
                    vec![start, stop_v]
                };
                match slice_make_new(ctx, state.vm_types.slice, &args, 0) {
                    Ok(v) => spush(frame, v),
                    Err(e) => {
                        return Some(DispatchOutcome::Raised(exc_from_obj_error(
                            ctx, state, e,
                        )))
                    }
                }
            }
            MP_BC_STORE_MAP => {
                // (`vm.zig:834-837`): `[.., dict, value,
                // key]` — the key is on top, the value below
                // it; the dict stays.
                let key = spop(frame).unwrap_or(Value::None);
                let value = spop(frame).unwrap_or(Value::None);
                let dict = stop(frame).unwrap_or(Value::None);
                if let Err(e) = dict_store(ctx, dict, key, value) {
                    return Some(DispatchOutcome::Raised(exc_from_obj_error(
                        ctx, state, e,
                    )));
                }
            }
            MP_BC_STORE_COMP => {
                // (`vm.zig:1110-1133`): `u` packs the tag
                // (low 2 bits) and the depth of the target
                // (`u >> 2`). Tag 0 = list-append (top =
                // item), tag 1 = dict-store (top = key,
                // second = value), tag 2 = set-store (top =
                // item — the real set path, not the old nano
                // bug).
                let u = decode_uint(state, code_idx, ip);
                let tag = u & 3;
                let depth = u >> 2;
                let obj = sget(frame, depth).unwrap_or(Value::None);
                match tag {
                    0 => {
                        let item = spop(frame).unwrap_or(Value::None);
                        if let Err(e) = list_append(ctx, obj, item) {
                            return Some(DispatchOutcome::Raised(exc_from_obj_error(
                                ctx, state, e,
                            )));
                        }
                    }
                    1 => {
                        let key = spop(frame).unwrap_or(Value::None);
                        let value = spop(frame).unwrap_or(Value::None);
                        if let Err(e) = dict_store(ctx, obj, key, value) {
                            return Some(DispatchOutcome::Raised(exc_from_obj_error(
                                ctx, state, e,
                            )));
                        }
                    }
                    _ => {
                        let item = spop(frame).unwrap_or(Value::None);
                        let mut pending = None;
                        if let Err(e) = set_store(ctx, obj, item, &mut pending) {
                            return Some(DispatchOutcome::Raised(exc_from_obj_error(
                                ctx, state, e,
                            )));
                        }
                        if let Some(e) = pending {
                            return Some(DispatchOutcome::Raised(exc_from_obj_error(
                                ctx, state, e,
                            )));
                        }
                    }
                }
            }
            // ── unpack ──
            MP_BC_UNPACK_SEQUENCE => {
                let u = decode_uint(state, code_idx, ip);
                let seq = spop(frame).unwrap_or(Value::None);
                match unpack_sequence_ctx(ctx, state, seq, u) {
                    Ok(items) => {
                        for v in items {
                            spush(frame, v);
                        }
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_UNPACK_EX => {
                let u = decode_uint(state, code_idx, ip);
                let seq = spop(frame).unwrap_or(Value::None);
                match unpack_ex_ctx(ctx, state, seq, u) {
                    Ok(items) => {
                        for v in items {
                            spush(frame, v);
                        }
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            // ── make function / closure ──
            MP_BC_MAKE_FUNCTION => {
                let child = decode_uint(state, code_idx, ip);
                match make_function_ctx(ctx, state, code_idx, child, None, None) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_MAKE_FUNCTION_DEFARGS => {
                let child = decode_uint(state, code_idx, ip);
                let def_kw = spop(frame);
                let def_pos = spop(frame);
                match make_function_ctx(ctx, state, code_idx, child, def_pos, def_kw) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_MAKE_CLOSURE => {
                let child = decode_uint(state, code_idx, ip);
                let nco = bc_byte(state, code_idx, *ip) as usize;
                *ip += 1;
                let start = frame.stack.len().saturating_sub(nco);
                match make_closure_ctx(ctx, state, code_idx, child, nco, &frame.stack[start..]) {
                    Ok(v) => {
                        frame.stack.truncate(start);
                        spush(frame, v);
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_MAKE_CLOSURE_DEFARGS => {
                let child = decode_uint(state, code_idx, ip);
                let nco = bc_byte(state, code_idx, *ip) as usize;
                *ip += 1;
                let total = 2 + nco;
                let start = frame.stack.len().saturating_sub(total);
                match make_closure_ctx(
                    ctx,
                    state,
                    code_idx,
                    child,
                    0x100 | nco,
                    &frame.stack[start..],
                ) {
                    Ok(v) => {
                        frame.stack.truncate(start);
                        spush(frame, v);
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            // ── calls ──
            MP_BC_CALL_FUNCTION => {
                // (`vm.zig:885-889`): `u` packs `n_args`
                // (low byte) and `n_kw` (next byte); the stack
                // is `[.., fun, args..., (key, value) x
                // n_kw]`.
                let u = decode_uint(state, code_idx, ip);
                let n_args = u & 0xff;
                let n_kw = (u >> 8) & 0xff;
                let (args, kwargs) = prepare_var_kw(frame, n_args, n_kw);
                let fun = spop(frame).unwrap_or(Value::None);
                let mut flat = args;
                for (k, v) in kwargs {
                    flat.push(Value::Qstr(k));
                    flat.push(v);
                }
                match call_function_ctx(ctx, state, fun, n_args, n_kw, &flat) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_CALL_METHOD => {
                // (`vm.zig:897-901`): the stack is `[..,
                // method, self, args..., (key, value) x
                // n_kw]`.
                let u = decode_uint(state, code_idx, ip);
                let n_args = u & 0xff;
                let n_kw = (u >> 8) & 0xff;
                let (args, kwargs) = prepare_var_kw(frame, n_args, n_kw);
                let self_ = spop(frame);
                let method = spop(frame).unwrap_or(Value::None);
                let mut flat = args;
                for (k, v) in kwargs {
                    flat.push(Value::Qstr(k));
                    flat.push(v);
                }
                match call_method_ctx(ctx, state, method, self_, n_args, n_kw, &flat) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_CALL_FUNCTION_VAR_KW => {
                // (`vm.zig:891-895`): the stack is `[.., fun,
                // args..., (key, value) x n_kw, star_mask]`
                // — the raw slice goes to
                // `call_method_var_kw_ctx` (`have_self =
                // false`).
                let u = decode_uint(state, code_idx, ip);
                let n_args = u & 0xff;
                let n_kw = (u >> 8) & 0xff;
                let fun_idx = frame
                    .stack
                    .len()
                    .saturating_sub(n_args + 2 * n_kw + 2);
                match call_method_var_kw_ctx(ctx, state, false, u, &frame.stack[fun_idx..]) {
                    Ok(v) => {
                        frame.stack.truncate(fun_idx);
                        spush(frame, v);
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_CALL_METHOD_VAR_KW => {
                // (`vm.zig:903-908`): the stack is `[..,
                // method, self, args..., (key, value) x n_kw,
                // star_mask]` (`have_self = true`).
                let u = decode_uint(state, code_idx, ip);
                let n_args = u & 0xff;
                let n_kw = (u >> 8) & 0xff;
                let meth_idx = frame
                    .stack
                    .len()
                    .saturating_sub(n_args + 2 * n_kw + 3);
                match call_method_var_kw_ctx(ctx, state, true, u, &frame.stack[meth_idx..]) {
                    Ok(v) => {
                        frame.stack.truncate(meth_idx);
                        spush(frame, v);
                    }
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            // ── imports (C15's — the stubs raise) ──
            MP_BC_IMPORT_NAME => {
                let q = decode_qstr(state, code_idx, ip);
                let from_list = spop(frame).unwrap_or(Value::None);
                // Level was discarded (`_level`) while the import
                // system was a stub — C15b wires it through to
                // `import_name_ctx` (a non-zero level raises
                // `NotImplementedError("relative import")`).
                let level = spop(frame).unwrap_or(Value::None);
                match import_name_ctx(ctx, state, q, from_list, level) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_IMPORT_FROM => {
                let q = decode_qstr(state, code_idx, ip);
                let module = stop(frame).unwrap_or(Value::None);
                match import_from_ctx(ctx, state, module, q) {
                    Ok(v) => spush(frame, v),
                    Err(e) => return Some(DispatchOutcome::Raised(e)),
                }
            }
            MP_BC_IMPORT_STAR => {
                let module = spop(frame).unwrap_or(Value::None);
                if let Err(e) = import_all_ctx(ctx, state, module) {
                    return Some(DispatchOutcome::Raised(e));
                }
            }
            // Any other opcode is handled by the loop in
            // `vm_dispatch` (this fn is only called for the
            // opcodes above) — fall through.
            _ => {}
    }
    None
}
