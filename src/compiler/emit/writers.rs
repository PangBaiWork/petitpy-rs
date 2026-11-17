//! Buffer writers (free functions taking `&mut Emit`) — vendor
//! `emitbc.c` equivalents, plus the prelude bit-packing and the
//! driver helpers `compile_scope` / `to_mpy_raw_code`.
//!
//! Port of the writer section of `src/compiler/emit.zig`.
//!
//! ## Divergences
//!
//! * The Zig writers catch `useQstr` / `useConstObj` OOM and set
//!   `emit.overflow`; Rust allocation cannot fail gracefully, so the
//!   `use_*` calls are infallible here and those catch paths do not
//!   exist. Observable difference: none short of actual OOM.
//! * `to_mpy_raw_code` sets `kind = MP_CODE_BYTECODE` (2). Zig's
//!   version sets `.kind = 0`, which its own writer would reject
//!   (`kind < MP_CODE_BYTECODE`); the function is dead code in Zig
//!   (nothing calls it) so the `0` never reaches a writer. The Rust
//!   port uses the semantically correct kind.
//! * `compile_scope` returns an owned `Vec<u8>` copy of the finalized
//!   code (Zig returns a slice borrowed from `Emit`).

use super::const_obj::ConstObj;
use super::consts::*;
use super::emit::Emit;
use super::encoders::{encode_int, encode_uint};
use crate::compiler::mpy::{RawCode, MP_CODE_BYTECODE};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{Scope, ScopeArena, ScopeId, SCOPE_FLAG_ALL_SIG};

pub fn get_cur_to_write_code_info(emit: &mut Emit, num: usize) -> &mut [u8] {
    if (emit.pass as u8) < (Pass::Emit as u8) {
        emit.code_info_offset += num;
        return &mut emit.dummy_data[..num];
    }
    let start = emit.code_info_offset;
    emit.code_info_offset += num;
    &mut emit.code_base[start..start + num]
}

pub fn write_code_info_byte(emit: &mut Emit, b: u8) {
    let slot = get_cur_to_write_code_info(emit, 1);
    slot[0] = b;
}

pub fn write_code_info_uint(emit: &mut Emit, val: usize) {
    let mut buf = [0u8; DUMMY_DATA_SIZE];
    let enc_len = {
        let enc = encode_uint(&mut buf, val);
        enc.len()
    };
    let slot = get_cur_to_write_code_info(emit, enc_len);
    slot.copy_from_slice(&buf[..enc_len]);
}

pub fn write_code_info_qstr(emit: &mut Emit, q: Qstr) {
    let short_idx = emit.emit_common.use_qstr(q);
    write_code_info_uint(emit, short_idx as usize);
}

pub fn get_cur_to_write_bytecode(emit: &mut Emit, num: usize) -> &mut [u8] {
    if emit.suppress {
        return &mut emit.dummy_data[..num];
    }
    if (emit.pass as u8) < (Pass::Emit as u8) {
        emit.bytecode_offset += num;
        return &mut emit.dummy_data[..num];
    }
    let start = emit.code_info_size + emit.bytecode_offset;
    emit.bytecode_offset += num;
    &mut emit.code_base[start..start + num]
}

pub fn adjust_stack(emit: &mut Emit, delta: i32, scopes: &mut ScopeArena) {
    if emit.pass == Pass::Scope {
        return;
    }
    emit.stack_size += delta;
    let sc = emit.scope.expect("adjust_stack: Emit has no scope");
    if emit.stack_size > scopes.scopes[sc].stack_size as i32 {
        // Vendor narrowing: scope.stack_size is u16. We clamp during the
        // STACK_SIZE pass; downstream passes only read it.
        scopes.scopes[sc].stack_size = emit.stack_size as u16;
    }
}

pub fn write_bytecode_raw_byte(emit: &mut Emit, b: u8) {
    let slot = get_cur_to_write_bytecode(emit, 1);
    slot[0] = b;
}

pub fn write_bytecode_byte(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    scopes: &mut ScopeArena,
) {
    adjust_stack(emit, stack_adj, scopes);
    let slot = get_cur_to_write_bytecode(emit, 1);
    slot[0] = b;
}

pub fn write_bytecode_byte_int(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    val: isize,
    scopes: &mut ScopeArena,
) {
    write_bytecode_byte(emit, stack_adj, b, scopes);
    let mut buf = [0u8; DUMMY_DATA_SIZE];
    let enc_len = {
        let enc = encode_int(&mut buf, val);
        enc.len()
    };
    let slot = get_cur_to_write_bytecode(emit, enc_len);
    slot.copy_from_slice(&buf[..enc_len]);
}

pub fn write_bytecode_uint_raw(emit: &mut Emit, val: usize) {
    let mut buf = [0u8; DUMMY_DATA_SIZE];
    let enc_len = {
        let enc = encode_uint(&mut buf, val);
        enc.len()
    };
    let slot = get_cur_to_write_bytecode(emit, enc_len);
    slot.copy_from_slice(&buf[..enc_len]);
}

pub fn write_bytecode_byte_uint(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    val: usize,
    scopes: &mut ScopeArena,
) {
    write_bytecode_byte(emit, stack_adj, b, scopes);
    write_bytecode_uint_raw(emit, val);
}

pub fn write_bytecode_byte_qstr(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    q: Qstr,
    scopes: &mut ScopeArena,
) {
    let short_idx = emit.emit_common.use_qstr(q);
    write_bytecode_byte_uint(emit, stack_adj, b, short_idx as usize, scopes);
}

pub fn write_bytecode_byte_obj(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    obj: ConstObj,
    scopes: &mut ScopeArena,
) {
    let idx = emit.emit_common.use_const_obj(obj);
    write_bytecode_byte_uint(emit, stack_adj, b, idx as usize, scopes);
}

pub fn write_bytecode_byte_child(
    emit: &mut Emit,
    stack_adj: i32,
    b: u8,
    child: ScopeId,
    scopes: &mut ScopeArena,
) {
    let idx = emit.emit_common.alloc_const_child(child);
    write_bytecode_byte_uint(emit, stack_adj, b, idx as usize, scopes);
}

pub fn write_bytecode_byte_label(
    emit: &mut Emit,
    stack_adj: i32,
    b1: u8,
    label: usize,
    scopes: &mut ScopeArena,
) {
    adjust_stack(emit, stack_adj, scopes);
    if emit.suppress {
        return;
    }

    let is_signed = b1 <= MP_BC_POP_JUMP_IF_FALSE;
    let mut jump_encoding_size: isize = 1;
    let mut bytecode_offset: isize = 0;

    if emit.pass >= Pass::CodeSize {
        bytecode_offset =
            emit.label_offsets[label] as isize - emit.bytecode_offset as isize - 2;
        if (is_signed && (-64..=63).contains(&bytecode_offset))
            || (!is_signed && (0..=127).contains(&bytecode_offset))
        {
            jump_encoding_size = 0;
        }
        bytecode_offset -= jump_encoding_size;
    }

    let total: usize = (2 + jump_encoding_size) as usize;
    // Decide the payload bytes (and the overflow flag) before borrowing
    // the output slot — the borrow checker forbids touching `emit`
    // while the slot is alive.
    let (second, third): (u8, Option<u8>);
    if jump_encoding_size == 0 {
        if is_signed {
            bytecode_offset += 0x40;
        }
        second = (bytecode_offset & 0xff) as u8;
        third = None;
    } else {
        if is_signed {
            bytecode_offset += 0x4000;
        }
        if emit.pass == Pass::Emit && !(0..=0x7fff).contains(&bytecode_offset) {
            emit.overflow = true;
        }
        second = (0x80 | ((bytecode_offset as usize) & 0x7f)) as u8;
        third = Some(((bytecode_offset as usize) >> 7 & 0xff) as u8);
    }

    let cp = get_cur_to_write_bytecode(emit, total);
    cp[0] = b1;
    cp[1] = second;
    if let Some(b2) = third {
        cp[2] = b2;
    }
}

pub fn call_helper(
    emit: &mut Emit,
    stack_adj_in: i32,
    bytecode_base: u8,
    n_positional: usize,
    n_keyword: usize,
    star_flags: u8,
    scopes: &mut ScopeArena,
) {
    let mut stack_adj = stack_adj_in;
    if star_flags != 0 {
        stack_adj -= n_positional as i32 + 2 * n_keyword as i32 + 1;
        write_bytecode_byte_uint(
            emit,
            stack_adj,
            bytecode_base + 1,
            (n_keyword << 8) | n_positional,
            scopes,
        );
    } else {
        stack_adj -= n_positional as i32 + 2 * n_keyword as i32;
        write_bytecode_byte_uint(
            emit,
            stack_adj,
            bytecode_base,
            (n_keyword << 8) | n_positional,
            scopes,
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Prelude bit-packing — verbatim port of py/bc.h
// MP_BC_PRELUDE_SIG_ENCODE and MP_BC_PRELUDE_SIZE_ENCODE.
// ─────────────────────────────────────────────────────────────────────────

pub fn prelude_sig_encode(emit: &mut Emit, s_in: usize, e_in: usize, scope: &Scope) {
    let mut f: usize = (scope.scope_flags & SCOPE_FLAG_ALL_SIG) as usize;
    let mut a: usize = scope.num_pos_args as usize;
    let mut k: usize = scope.num_kwonly_args as usize;
    let mut d: usize = scope.num_def_pos_args as usize;
    let mut s = s_in - 1;
    let mut e = e_in;
    let mut z: u8 = (((s & 0xf) << 3) | ((e & 1) << 2) | (a & 3)) as u8;
    s >>= 4;
    e >>= 1;
    a >>= 2;
    while (s | e | f | a | k | d) != 0 {
        write_code_info_byte(emit, 0x80 | z);
        z = (((f & 1) << 6)
            | ((s & 3) << 4)
            | ((k & 1) << 3)
            | ((a & 1) << 2)
            | ((e & 1) << 1)
            | (d & 1)) as u8;
        s >>= 2;
        e >>= 1;
        f >>= 1;
        a >>= 1;
        k >>= 1;
        d >>= 1;
    }
    write_code_info_byte(emit, z);
}

pub fn prelude_size_encode(emit: &mut Emit, i_in: usize, cc_in: usize) {
    let mut i = i_in;
    let mut cc = cc_in;
    loop {
        let mut z: u8 = (((i & 0x3f) << 1) | (cc & 1)) as u8;
        cc >>= 1;
        i >>= 6;
        if (cc | i) != 0 {
            z |= 0x80;
        }
        write_code_info_byte(emit, z);
        if (cc | i) == 0 {
            break;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Driver helpers — run the 3-pass loop, package the result for the writer.
// ─────────────────────────────────────────────────────────────────────────

/// Run SCOPE → STACK_SIZE → CODE_SIZE → EMIT (re-loop EMIT if sizes
/// shifted). `emit_body` is the per-scope walker that issues the
/// `Emit.*` opcode calls. Returns the finalized code (owned copy of
/// the `[code_info | bytecode]` buffer, sliced to the used length)
/// and the number of children allocated.
///
/// (Zig returns the code as a slice borrowed from `Emit`; Rust
/// returns an owned copy. The compile driver does not use this helper
/// — its body needs the full `Compiler`, not just the `Emit` — it is
/// ported because it is part of `emit.zig`'s public surface.)
pub fn compile_scope(
    emit: &mut Emit,
    scope: ScopeId,
    scopes: &mut ScopeArena,
    mut emit_body: impl FnMut(&mut Emit, ScopeId, &mut ScopeArena),
) -> (Vec<u8>, u16) {
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, scopes);
        emit_body(emit, scope, &mut *scopes);
        let _ = emit.end_pass(scopes);
    }
    // EMIT may need re-running if sizes shifted.
    loop {
        emit.emit_common.start_pass(Pass::Emit);
        emit.start_pass(Pass::Emit, scope, scopes);
        emit_body(emit, scope, &mut *scopes);
        if emit.end_pass(scopes) {
            break;
        }
    }
    // Vendor emitbc.c:404-411 — slice to the actual used length so the
    // .mpy never carries a phantom trailing 0x00.
    let used: usize = emit.code_info_size + emit.bytecode_size;
    (
        emit.code_base[..used].to_vec(),
        emit.emit_common.ct_cur_child,
    )
}

/// Wrap an `Emit`'s finalized state into an `mpy::RawCode`. `children`
/// are shallow — populate them recursively from outside. Kind is
/// `MP_CODE_BYTECODE` (see the module docs for why Zig's `0` is not
/// reproduced here).
pub fn to_mpy_raw_code(code: Vec<u8>, children: Vec<RawCode>) -> RawCode {
    RawCode {
        kind: MP_CODE_BYTECODE,
        fun_data: code,
        children,
    }
}
