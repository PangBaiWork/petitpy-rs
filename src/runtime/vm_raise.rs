//! The raise/execute path — `mp_make_raise_obj`,
//! `exception_from_pending`, `execute_frame`,
//! `exception_handler` + the traceback decode, and the
//! `.mpy` const-table converter `obj_from_entry` (C13).
//! Split out of `vm.rs` (file-size cap).

use crate::compiler::emit::consts::{
    MP_BC_END_FINALLY, MP_BC_FOR_ITER, MP_BC_RAISE_LAST, MP_BC_YIELD_FROM,
};
use crate::compiler::mpy::{ObjEntry, RawCode};
use crate::memory::gc::CallCtx;
use crate::objects::obj::BuiltinTypes;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::{
    exception_add_traceback, exception_get_value, is_exception_instance, is_exception_type,
    new_exception, new_exception_args, new_exception_msg, type_is_subclass_of,
};
use crate::objects::primitives::objint::parse_int_text;
use crate::objects::primitives::ops::new_int_from_bigint;
use crate::objects::value::{BytesData, Obj, StrData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::bc::{decode_uint, prelude_sig_decode, prelude_size_decode, CodeState};
use crate::runtime::runner::RunOutcome;
use crate::runtime::vm::VmState;

/// Convert one `.mpy` const-table entry to a `Value` (the Zig
/// loader's `mpy` → `Obj` mapping, `mpy.zig` + `py/persistentcode.c`).
/// `Int` text is decimal; `Float`/`Complex` text parses with
/// `mp_parse_num_float` (`vendor::parsenum::parse_num_float` —
/// the FMT finding applies: the Zig uses libc `strtod`, correctly
/// rounded — Rust `str::parse` matches — but the entry point is
/// the full decimal parser, so `"1e+"` / `"1j"` raise `ValueError`
/// instead of silently becoming `0.0`). A `Tuple`
/// converts recursively. `Ellipsis` is the VM's singleton
/// (passed in — the ctx cannot reach `VmState`).
pub fn obj_from_entry(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    ellipsis: Value,
    entry: &ObjEntry,
) -> PyResult<Value> {
    match entry {
        ObjEntry::None => Ok(Value::None),
        ObjEntry::False => Ok(Value::Bool(false)),
        ObjEntry::True => Ok(Value::Bool(true)),
        ObjEntry::Ellipsis => Ok(ellipsis),
        ObjEntry::Str(bytes) => Ok(Value::Heap(ctx.alloc(
            Some(types.str_),
            Obj::Str(StrData {
                bytes: bytes.clone(),
            }),
        ))),
        ObjEntry::Bytes(bytes) => Ok(Value::Heap(ctx.alloc(
            Some(types.bytes),
            Obj::Bytes(BytesData {
                bytes: bytes.clone(),
            }),
        ))),
        ObjEntry::Int(text) => {
            let s = String::from_utf8_lossy(text);
            let z = parse_int_text(s.as_bytes(), 10)
                .map_err(|e| ctx.record_typed(e))?;
            Ok(new_int_from_bigint(ctx.arena, types, ctx.roots, &z))
        }
        ObjEntry::Float(text) => {
            // The Zig loader's `load_obj` calls `mp_parse_num_float`
            // (`allow_imag = false`) here — NOT a plain `str::parse`:
            // `"1e+"` / `"1_0.5"` / `"1j"` are ValueError / 10.5 /
            // ValueError respectively (probed on the reference binary
            // via hand-built .mpy files, C17), while `str::parse`
            // would give 0.0 for all three.
            let s = String::from_utf8_lossy(text);
            let f = match crate::vendor::parsenum::parse_num_float(s.as_bytes(), false) {
                Ok(crate::vendor::parsenum::DecimalNum::Float(f)) => f,
                Ok(crate::vendor::parsenum::DecimalNum::Complex { .. }) => {
                    unreachable!("parse_num_float with allow_imag=false never returns Complex")
                }
                Err(e) => {
                    ctx.record_raise("ValueError", Some(e.message()));
                    return Err(PyError::PyRaise);
                }
            };
            Ok(Value::Heap(ctx.alloc(Some(types.float), Obj::Float(f))))
        }
        ObjEntry::Complex(text) => {
            // Same loader path with `allow_imag = true`
            // (`persistentcode.zig: load_obj`). The text is the
            // vendor `PRINT_REPR` of the complex (`"(1+2j)"` when
            // real != 0, `"2j"` otherwise — `objcomplex.c:
            // complex_print`), and the parenthesised form does NOT
            // parse: the reference binary raises `ValueError("invalid
            // syntax for number")` for it (probed, C17). Only a bare
            // `"Nj"` text loads. (The petitpy compiler never emits a
            // COMPLEX entry — `1j` / `3+4j` become FLOAT `"1j"` /
            // `"4j"` consts — so this arm is reachable only from
            // foreign .mpy files.)
            let s = String::from_utf8_lossy(text);
            let (re, im) = match crate::vendor::parsenum::parse_num_float(s.as_bytes(), true) {
                Ok(crate::vendor::parsenum::DecimalNum::Float(f)) => (f, 0.0),
                Ok(crate::vendor::parsenum::DecimalNum::Complex { re, im }) => (re, im),
                Err(e) => {
                    ctx.record_raise("ValueError", Some(e.message()));
                    return Err(PyError::PyRaise);
                }
            };
            Ok(Value::Heap(ctx.alloc(Some(types.complex), Obj::Complex { re, im })))
        }
        ObjEntry::Tuple(items) => {
            let values: Vec<Value> = items
                .iter()
                .map(|e| obj_from_entry(ctx, types, ellipsis, e))
                .collect::<PyResult<Vec<Value>>>()?;
            Ok(Value::Heap(crate::objects::containers::objtuple::new_tuple(
                ctx, types.tuple, &values,
            )))
        }
    }
}

// ── Raise construction ───────────────────────────────────────────────

/// `mp_make_raise_obj` (`runtime.zig:1456`): an exception instance
/// is itself; an exception type is instantiated with no args; anything
/// else raises `TypeError("exceptions must derive from
/// BaseException")` (built directly — it cannot fail).
pub fn make_raise_obj(ctx: &mut CallCtx, state: &VmState, exc: Value) -> Value {
    if is_exception_instance(ctx.arena, exc, state.exc.base_exception) {
        exc
    } else if is_exception_type(ctx.arena, exc, state.exc.base_exception) {
        let type_ = match exc {
            Value::Heap(r) => r,
            _ => unreachable!("an exception type is a heap object"),
        };
        new_exception(ctx, type_)
    } else {
        new_exception_msg(
            ctx,
            state.exc.type_error,
            "exceptions must derive from BaseException",
        )
    }
}

/// Drain a recorded slot raise (`CallCtx.pending_raise`) into the
/// exception object the VM raises (`VmState.pending_exc`). `None`
/// when nothing was recorded. Unknown names fall back to
/// `TypeError` (defensive — every `TypedRaise` impl uses a real
/// exception name).
pub fn exception_from_pending(ctx: &mut CallCtx, state: &VmState) -> Option<Value> {
    let pr = ctx.take_pending_raise()?;
    let type_ = state
        .exc
        .by_name(pr.exc_name)
        .unwrap_or(state.exc.type_error);
    if let Some(arg) = pr.arg {
        Some(new_exception_args(ctx, type_, &[arg]))
    } else if let Some(msg) = pr.message {
        Some(new_exception_msg(ctx, type_, &msg))
    } else {
        Some(new_exception(ctx, type_))
    }
}

// ── The execute entry ────────────────────────────────────────────────

/// `mp_execute_bytecode` (`vm.zig:1191-1226`): the outer exception
/// loop. `send` is the resume value (overwrites the stack top —
/// the Zig's `code_state.sp[0] = send_value` in
/// `mp_obj_gen_resume`; fresh frames have an empty stack).
/// `throw` is the injected exception (`gen.throw`); it is raised
/// immediately unless the frame sits on `YIELD_FROM` (which
/// handles injection itself — see `dispatch_loop`).
pub fn execute_frame(
    ctx: &mut CallCtx,
    state: &mut VmState,
    frame: &mut CodeState,
    send: Value,
    throw: Option<Value>,
) -> RunOutcome {
    if !frame.stack.is_empty() {
        if let Some(top) = frame.stack.last_mut() {
            *top = Some(send);
        }
    }
    let mark = ctx.roots.stack.len();
    let mut inject = throw;
    let outcome = loop {
        match crate::runtime::vm_dispatch::dispatch_loop(ctx, state, frame, &mut inject, mark) {
            crate::runtime::vm_dispatch::DispatchOutcome::Normal(v) => {
                break RunOutcome::Normal(v)
            }
            crate::runtime::vm_dispatch::DispatchOutcome::Yield(v) => {
                break RunOutcome::Yield(v)
            }
            crate::runtime::vm_dispatch::DispatchOutcome::Raised(exc) => {
                if exception_handler(ctx, state, frame, exc) {
                    continue;
                }
                break RunOutcome::Exception(exc);
            }
            crate::runtime::vm_dispatch::DispatchOutcome::RetExc(exc) => {
                break RunOutcome::Exception(exc)
            }
        }
    };
    ctx.roots.stack.truncate(mark);
    outcome
}

/// `exceptionHandler` (`vm.zig:192-260`): the StopIteration
/// fast-path, traceback annotation, then catch-or-propagate.
/// Returns `true` when the exception was caught (re-enter the
/// dispatch at the handler); `false` when it propagates (the
/// exception is stashed in `state[0]`, as the Zig does).
pub fn exception_handler(
    ctx: &mut CallCtx,
    state: &mut VmState,
    frame: &mut CodeState,
    exc: Value,
) -> bool {
    let code_idx = match ctx.arena.get(frame.fun) {
        Obj::FunBc(d) => d.code,
        _ => unreachable!("a frame's fun is a FunBc"),
    };
    let code = state.code_store.code(code_idx);
    let fun_data = &code.fun_data;
    let cur_op = fun_data[frame.ip];
    // StopIteration fast-path: the exc's type must be a
    // StopIteration subclass.
    let is_stop = match obj_get_type(ctx.arena, ctx.types, exc) {
        Some(t) => type_is_subclass_of(ctx.arena, t, state.exc.stop_iteration),
        None => false,
    };
    if is_stop {
        if cur_op == MP_BC_FOR_ITER {
            let mut p = frame.ip + 1;
            let ulab = decode_ulabel(fun_data, &mut p);
            frame.ip = p + ulab;
            let n = frame.stack.len();
            frame.stack.truncate(n.saturating_sub(4));
            return true;
        } else if cur_op == MP_BC_YIELD_FROM {
            if let Some(top) = frame.stack.last_mut() {
                *top = Some(exception_get_value(ctx.arena, exc));
            }
            frame.ip += 1;
            return true;
        }
    }
    // Traceback annotation (skip for GeneratorExit, END_FINALLY,
    // RAISE_LAST). The Zig skips only the GeneratorExit SINGLETON
    // (pointer identity); Rust mints a fresh instance per close
    // (C10b), so any GeneratorExit instance skips — a documented
    // divergence until C12a's ROM singleton lands.
    let is_gen_exit = match obj_get_type(ctx.arena, ctx.types, exc) {
        Some(t) => type_is_subclass_of(ctx.arena, t, state.exc.generator_exit),
        None => false,
    };
    if !is_gen_exit && cur_op != MP_BC_END_FINALLY && cur_op != MP_BC_RAISE_LAST {
        let qstr_table = state.code_store.qstr_table(code_idx);
        annotate_traceback(ctx, frame, code, qstr_table, exc);
    }
    // Pop exception blocks whose handlers are already past ip.
    while frame.exc_sp_idx >= 1 {
        let handler = frame.exc_stack.last().map(|b| b.handler).unwrap_or(0);
        if handler <= frame.ip {
            frame.pop_exc_block();
        } else {
            break;
        }
    }
    if frame.exc_sp_idx >= 1 {
        let (handler, val_sp) = {
            let esp = frame.exc_stack.last_mut().unwrap();
            esp.prev_exc = Some(exc);
            (esp.handler, esp.val_sp)
        };
        frame.ip = handler;
        frame.stack.truncate(val_sp);
        frame.stack.push(Some(exc));
        true
    } else {
        if !frame.state.is_empty() {
            frame.state[0] = Some(exc);
        }
        false
    }
}

/// The traceback-annotation half of `exception_handler`: decode the
/// prelude (sig + size), read the block name + arg names out of the
/// source-info section, and append `(file, line, block)`.
fn annotate_traceback(
    ctx: &mut CallCtx,
    frame: &CodeState,
    code: &RawCode,
    qstr_table: &[u16],
    exc: Value,
) {
    let fun_data = &code.fun_data;
    let mut cursor: &[u8] = fun_data;
    let sig = prelude_sig_decode(&mut cursor);
    let size = prelude_size_decode(&mut cursor);
    let info_start = fun_data.len() - cursor.len();
    let line_info_top = info_start + size.n_info;
    let bytecode_start = info_start + size.n_info + size.n_cell;
    let mut p: &[u8] = &fun_data[info_start..];
    let block_name_idx = decode_uint(&mut p);
    let arg_count = 1 + sig.n_pos_args as usize + sig.n_kwonly_args as usize;
    for _ in 0..arg_count.saturating_sub(1) {
        let _ = decode_uint(&mut p);
    }
    let line_info_start = fun_data.len() - p.len();
    let block_name = qstr_table.get(block_name_idx).copied().unwrap_or(0);
    let source_file = qstr_table.first().copied().unwrap_or(0);
    let bc = frame.ip.saturating_sub(bytecode_start);
    let line = bytecode_get_source_line(&fun_data[line_info_start..line_info_top], bc);
    exception_add_traceback(ctx, exc, source_file as u32, line as u32, block_name as u32);
}

/// `mp_bytecode_get_source_line` (`vm.zig:122-140`, vendor
/// `py/bc.h:317-348`): walk the lineinfo varints until the
/// bytecode offset is covered.
pub fn bytecode_get_source_line(line_info: &[u8], bc_in: usize) -> usize {
    let mut i = 0;
    let mut bc_offset = bc_in;
    let mut source_line: usize = 1;
    while i < line_info.len() {
        let z = line_info[i];
        let (bc_inc, line_inc) = if z & 0x80 == 0 {
            ((z & 0x1f) as usize, (z >> 5) as usize)
        } else {
            let next = if i + 1 < line_info.len() {
                line_info[i + 1] as usize
            } else {
                0
            };
            (
                (z & 0xf) as usize,
                (((z as usize) << 4 & 0x700) | next),
            )
        };
        let adv = if z & 0x80 == 0 { 1 } else { 2 };
        if bc_offset >= bc_inc {
            bc_offset -= bc_inc;
            source_line += line_inc;
        } else {
            break;
        }
        i += adv;
    }
    source_line
}

/// `decodeUlabel` (`vm.zig:307-316`): a 1- or 2-byte unsigned
/// label (the second byte carries the high 7 bits when the first
/// has its high bit set).
pub fn decode_ulabel(fun_data: &[u8], p: &mut usize) -> usize {
    let b0 = fun_data[*p];
    if b0 & 0x80 != 0 {
        let v = ((b0 & 0x7f) as usize) | ((fun_data[*p + 1] as usize) << 7);
        *p += 2;
        v
    } else {
        *p += 1;
        b0 as usize
    }
}

/// `decodeSlabel` (`vm.zig:317-326`): a 1- or 2-byte signed label
/// (bias 0x40 / 0x4000). Returned as a `usize` wrapping offset
/// (the Zig holds it in a `usize` and adds with wrapping
/// arithmetic — `jumpBy` uses `+%`).
pub fn decode_slabel(fun_data: &[u8], p: &mut usize) -> usize {
    let b0 = fun_data[*p];
    if b0 & 0x80 != 0 {
        let v = (((b0 & 0x7f) as usize) | ((fun_data[*p + 1] as usize) << 7)).wrapping_sub(0x4000);
        *p += 2;
        v
    } else {
        *p += 1;
        (b0 as usize).wrapping_sub(0x40)
    }
}
