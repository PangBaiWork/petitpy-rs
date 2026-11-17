//! Vendor constants for the bytecode emitter.
//!
//! Pure-Rust port of the constant blocks at the top of
//! `src/compiler/emit.zig` (itself hand-ported from vendor `py/bc0.h`,
//! `py/emit.h`, `py/runtime0.h`, `py/lexer.h`, `py/obj.h`). Values are
//! pinned to the same vendor sources so a future bytecode-format bump
//! fails loudly. The format itself is frozen by `MPY_VERSION = 6`.
//!
//! Two families live here:
//!   * `MP_BC_*` opcode bases / opcodes (the wire format).
//!   * `MP_EMIT_*` / `MP_UNARY_OP_*` / `MP_BINARY_OP_*` kind enums
//!     (the emitter's input vocabulary).
//!
//! Note the token ordinals: `MP_TOKEN_*` (9/10/11/12) follow vendor
//! `py/lexer.h` with FSTRINGS disabled — they are the emitter's own
//! constants and are NOT the lexer `TokenKind` discriminants (the
//! Rust lexer, like Zig's, numbers `ELLIPSIS` at 13). `compile.rs`
//! maps `TokenKind` variants onto these constants explicitly.

use crate::compiler::qstr::Qstr;

/// Scratch buffer size for size-only / suppressed writes.
/// `MP_ENCODE_UINT_MAX_BYTES` from vendor `py/bc.h`.
pub const DUMMY_DATA_SIZE: usize = 10;

// ── py/bc0.h opcode bases ────────────────────────────────────────────────
pub const MP_BC_BASE_QSTR_O: u8 = 0x10;
pub const MP_BC_BASE_VINT_E: u8 = 0x20;
pub const MP_BC_BASE_VINT_O: u8 = 0x30;
pub const MP_BC_BASE_JUMP_E: u8 = 0x40;
pub const MP_BC_BASE_BYTE_O: u8 = 0x50;
pub const MP_BC_BASE_BYTE_E: u8 = 0x60;

// ── const loads ─────────────────────────────────────────────────────────
pub const MP_BC_LOAD_CONST_FALSE: u8 = MP_BC_BASE_BYTE_O + 0x00;
pub const MP_BC_LOAD_CONST_NONE: u8 = MP_BC_BASE_BYTE_O + 0x01;
pub const MP_BC_LOAD_CONST_TRUE: u8 = MP_BC_BASE_BYTE_O + 0x02;
pub const MP_BC_LOAD_CONST_SMALL_INT: u8 = MP_BC_BASE_VINT_E + 0x02;
pub const MP_BC_LOAD_CONST_STRING: u8 = MP_BC_BASE_QSTR_O + 0x00;
pub const MP_BC_LOAD_CONST_OBJ: u8 = MP_BC_BASE_VINT_E + 0x03;
pub const MP_BC_LOAD_NULL: u8 = MP_BC_BASE_BYTE_O + 0x03;

// ── locals/names ────────────────────────────────────────────────────────
pub const MP_BC_LOAD_FAST_N: u8 = MP_BC_BASE_VINT_E + 0x04;
pub const MP_BC_LOAD_DEREF: u8 = MP_BC_BASE_VINT_E + 0x05;
pub const MP_BC_LOAD_NAME: u8 = MP_BC_BASE_QSTR_O + 0x01;
pub const MP_BC_LOAD_GLOBAL: u8 = MP_BC_BASE_QSTR_O + 0x02;
pub const MP_BC_LOAD_ATTR: u8 = MP_BC_BASE_QSTR_O + 0x03;
pub const MP_BC_LOAD_METHOD: u8 = MP_BC_BASE_QSTR_O + 0x04;
pub const MP_BC_LOAD_SUPER_METHOD: u8 = MP_BC_BASE_QSTR_O + 0x05;
pub const MP_BC_LOAD_BUILD_CLASS: u8 = MP_BC_BASE_BYTE_O + 0x04;
pub const MP_BC_LOAD_SUBSCR: u8 = MP_BC_BASE_BYTE_O + 0x05;

pub const MP_BC_STORE_FAST_N: u8 = MP_BC_BASE_VINT_E + 0x06;
pub const MP_BC_STORE_DEREF: u8 = MP_BC_BASE_VINT_E + 0x07;
pub const MP_BC_STORE_NAME: u8 = MP_BC_BASE_QSTR_O + 0x06;
pub const MP_BC_STORE_GLOBAL: u8 = MP_BC_BASE_QSTR_O + 0x07;
pub const MP_BC_STORE_ATTR: u8 = MP_BC_BASE_QSTR_O + 0x08;
pub const MP_BC_STORE_SUBSCR: u8 = MP_BC_BASE_BYTE_O + 0x06;

pub const MP_BC_DELETE_FAST: u8 = MP_BC_BASE_VINT_E + 0x08;
pub const MP_BC_DELETE_DEREF: u8 = MP_BC_BASE_VINT_E + 0x09;
pub const MP_BC_DELETE_NAME: u8 = MP_BC_BASE_QSTR_O + 0x09;
pub const MP_BC_DELETE_GLOBAL: u8 = MP_BC_BASE_QSTR_O + 0x0a;

// ── stack ops ───────────────────────────────────────────────────────────
pub const MP_BC_DUP_TOP: u8 = MP_BC_BASE_BYTE_O + 0x07;
pub const MP_BC_DUP_TOP_TWO: u8 = MP_BC_BASE_BYTE_O + 0x08;
pub const MP_BC_POP_TOP: u8 = MP_BC_BASE_BYTE_O + 0x09;
pub const MP_BC_ROT_TWO: u8 = MP_BC_BASE_BYTE_O + 0x0a;
pub const MP_BC_ROT_THREE: u8 = MP_BC_BASE_BYTE_O + 0x0b;

// ── control flow ────────────────────────────────────────────────────────
pub const MP_BC_UNWIND_JUMP: u8 = MP_BC_BASE_JUMP_E + 0x00;
pub const MP_BC_JUMP: u8 = MP_BC_BASE_JUMP_E + 0x02;
pub const MP_BC_POP_JUMP_IF_TRUE: u8 = MP_BC_BASE_JUMP_E + 0x03;
pub const MP_BC_POP_JUMP_IF_FALSE: u8 = MP_BC_BASE_JUMP_E + 0x04;
pub const MP_BC_JUMP_IF_TRUE_OR_POP: u8 = MP_BC_BASE_JUMP_E + 0x05;
pub const MP_BC_JUMP_IF_FALSE_OR_POP: u8 = MP_BC_BASE_JUMP_E + 0x06;
pub const MP_BC_SETUP_WITH: u8 = MP_BC_BASE_JUMP_E + 0x07;
pub const MP_BC_SETUP_EXCEPT: u8 = MP_BC_BASE_JUMP_E + 0x08;
pub const MP_BC_SETUP_FINALLY: u8 = MP_BC_BASE_JUMP_E + 0x09;
pub const MP_BC_POP_EXCEPT_JUMP: u8 = MP_BC_BASE_JUMP_E + 0x0a;
pub const MP_BC_FOR_ITER: u8 = MP_BC_BASE_JUMP_E + 0x0b;
pub const MP_BC_WITH_CLEANUP: u8 = MP_BC_BASE_BYTE_O + 0x0c;
pub const MP_BC_END_FINALLY: u8 = MP_BC_BASE_BYTE_O + 0x0d;
pub const MP_BC_GET_ITER: u8 = MP_BC_BASE_BYTE_O + 0x0e;
pub const MP_BC_GET_ITER_STACK: u8 = MP_BC_BASE_BYTE_O + 0x0f;

// ── build / unpack / store_comp ─────────────────────────────────────────
pub const MP_BC_BUILD_TUPLE: u8 = MP_BC_BASE_VINT_E + 0x0a;
pub const MP_BC_BUILD_LIST: u8 = MP_BC_BASE_VINT_E + 0x0b;
pub const MP_BC_BUILD_MAP: u8 = MP_BC_BASE_VINT_E + 0x0c;
pub const MP_BC_STORE_MAP: u8 = MP_BC_BASE_BYTE_E + 0x02;
pub const MP_BC_BUILD_SET: u8 = MP_BC_BASE_VINT_E + 0x0d;
pub const MP_BC_BUILD_SLICE: u8 = MP_BC_BASE_VINT_E + 0x0e;
pub const MP_BC_STORE_COMP: u8 = MP_BC_BASE_VINT_E + 0x0f;
pub const MP_BC_UNPACK_SEQUENCE: u8 = MP_BC_BASE_VINT_O + 0x00;
pub const MP_BC_UNPACK_EX: u8 = MP_BC_BASE_VINT_O + 0x01;

// ── terminus ────────────────────────────────────────────────────────────
pub const MP_BC_RETURN_VALUE: u8 = MP_BC_BASE_BYTE_E + 0x03;
pub const MP_BC_RAISE_LAST: u8 = MP_BC_BASE_BYTE_E + 0x04;
pub const MP_BC_RAISE_OBJ: u8 = MP_BC_BASE_BYTE_E + 0x05;
pub const MP_BC_RAISE_FROM: u8 = MP_BC_BASE_BYTE_E + 0x06;
pub const MP_BC_YIELD_VALUE: u8 = MP_BC_BASE_BYTE_E + 0x07;
pub const MP_BC_YIELD_FROM: u8 = MP_BC_BASE_BYTE_E + 0x08;

// ── make / call ─────────────────────────────────────────────────────────
pub const MP_BC_MAKE_FUNCTION: u8 = MP_BC_BASE_VINT_O + 0x02;
pub const MP_BC_MAKE_FUNCTION_DEFARGS: u8 = MP_BC_BASE_VINT_O + 0x03;
pub const MP_BC_MAKE_CLOSURE: u8 = MP_BC_BASE_VINT_E + 0x00;
pub const MP_BC_MAKE_CLOSURE_DEFARGS: u8 = MP_BC_BASE_VINT_E + 0x01;
pub const MP_BC_CALL_FUNCTION: u8 = MP_BC_BASE_VINT_O + 0x04;
pub const MP_BC_CALL_FUNCTION_VAR_KW: u8 = MP_BC_BASE_VINT_O + 0x05;
pub const MP_BC_CALL_METHOD: u8 = MP_BC_BASE_VINT_O + 0x06;
pub const MP_BC_CALL_METHOD_VAR_KW: u8 = MP_BC_BASE_VINT_O + 0x07;

// ── imports ─────────────────────────────────────────────────────────────
pub const MP_BC_IMPORT_NAME: u8 = MP_BC_BASE_QSTR_O + 0x0b;
pub const MP_BC_IMPORT_FROM: u8 = MP_BC_BASE_QSTR_O + 0x0c;
pub const MP_BC_IMPORT_STAR: u8 = MP_BC_BASE_BYTE_E + 0x09;

// ── MULTI ranges ────────────────────────────────────────────────────────
pub const MP_BC_LOAD_CONST_SMALL_INT_MULTI: u8 = 0x70;
pub const MP_BC_LOAD_CONST_SMALL_INT_MULTI_NUM: isize = 64;
pub const MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS: isize = 16;
pub const MP_BC_LOAD_FAST_MULTI: u8 = 0xb0;
/// `MP_BC_LOAD_FAST_MULTI_NUM` (vendor `py/bc0.h:75`).
pub const MP_BC_LOAD_FAST_MULTI_NUM: isize = 16;
pub const MP_BC_STORE_FAST_MULTI: u8 = 0xc0;
/// `MP_BC_STORE_FAST_MULTI_NUM` (vendor `py/bc0.h:76`).
pub const MP_BC_STORE_FAST_MULTI_NUM: isize = 16;
pub const MP_BC_UNARY_OP_MULTI: u8 = 0xd0;
/// `MP_BC_UNARY_OP_MULTI_NUM` = `MP_UNARY_OP_NUM_BYTECODE`
/// (vendor `py/runtime0.h:60`: POSITIVE..NOT = 4).
pub const MP_BC_UNARY_OP_MULTI_NUM: isize = 4;
pub const MP_BC_BINARY_OP_MULTI: u8 = 0xd7;
/// `MP_BC_BINARY_OP_MULTI_NUM` = `MP_BINARY_OP_NUM_BYTECODE`
/// (vendor `py/runtime0.h:64`: LESS..POWER = 35).
pub const MP_BC_BINARY_OP_MULTI_NUM: isize = 35;

// ── py/emit.h kind enums ────────────────────────────────────────────────
/// Emitter pass. Discriminants match Zig's `Pass` (scope=1 .. emit=4);
/// ordering matters — several gates compare `pass >= Pass::CodeSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Pass {
    Scope = 1,
    StackSize = 2,
    CodeSize = 3,
    Emit = 4,
}

impl Default for Pass {
    fn default() -> Self {
        Pass::Scope
    }
}

/// `EmitCommon::id_op` direction (Zig: anonymous enum `{ load, store,
/// delete }` on `idOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdOp {
    Load,
    Store,
    Delete,
}

pub const MP_EMIT_IDOP_LOCAL_FAST: u8 = 0;
pub const MP_EMIT_IDOP_LOCAL_DEREF: u8 = 1;
pub const MP_EMIT_IDOP_GLOBAL_NAME: u8 = 0;
pub const MP_EMIT_IDOP_GLOBAL_GLOBAL: u8 = 1;

pub const MP_EMIT_IMPORT_NAME: u8 = 0;
pub const MP_EMIT_IMPORT_FROM: u8 = 1;
pub const MP_EMIT_IMPORT_STAR: u8 = 2;

pub const MP_EMIT_SUBSCR_LOAD: u8 = 0;
pub const MP_EMIT_SUBSCR_STORE: u8 = 1;
pub const MP_EMIT_SUBSCR_DELETE: u8 = 2;

pub const MP_EMIT_ATTR_LOAD: u8 = 0;
pub const MP_EMIT_ATTR_STORE: u8 = 1;
pub const MP_EMIT_ATTR_DELETE: u8 = 2;

pub const MP_EMIT_SETUP_BLOCK_WITH: u8 = 0;
pub const MP_EMIT_SETUP_BLOCK_EXCEPT: u8 = 1;
pub const MP_EMIT_SETUP_BLOCK_FINALLY: u8 = 2;

pub const MP_EMIT_BUILD_TUPLE: u8 = 0;
pub const MP_EMIT_BUILD_LIST: u8 = 1;
pub const MP_EMIT_BUILD_MAP: u8 = 2;
pub const MP_EMIT_BUILD_SET: u8 = 3;
pub const MP_EMIT_BUILD_SLICE: u8 = 4;

pub const MP_EMIT_YIELD_VALUE: u8 = 0;
pub const MP_EMIT_YIELD_FROM: u8 = 1;

pub const MP_EMIT_STAR_FLAG_SINGLE: u8 = 0x01;
pub const MP_EMIT_STAR_FLAG_DOUBLE: u8 = 0x02;
pub const MP_EMIT_BREAK_FROM_FOR: usize = 0x8000;

// ── py/runtime0.h unary / binary op ordinals (used in MULTI bytecode) ───
pub const MP_UNARY_OP_POSITIVE: u8 = 0;
pub const MP_UNARY_OP_NEGATIVE: u8 = 1;
pub const MP_UNARY_OP_INVERT: u8 = 2;
pub const MP_UNARY_OP_NOT: u8 = 3;

pub const MP_BINARY_OP_LESS: u8 = 0;
pub const MP_BINARY_OP_MORE: u8 = 1;
pub const MP_BINARY_OP_EQUAL: u8 = 2; // vendor py/runtime0.h:97
pub const MP_BINARY_OP_MORE_EQUAL: u8 = 4; // vendor py/runtime0.h:99
pub const MP_BINARY_OP_IN: u8 = 6;
pub const MP_BINARY_OP_IS: u8 = 7;
pub const MP_BINARY_OP_EXCEPTION_MATCH: u8 = 8;
/// Vendor py/runtime0.h:106 — first of the 13 INPLACE_* binary ops.
/// (compile.zig §29.)
pub const MP_BINARY_OP_INPLACE_OR: u8 = 9;
/// Vendor py/runtime0.h: MORE = 1, INPLACE_ADD = 14. (compile.zig §29.)
pub const MP_BINARY_OP_INPLACE_ADD: u8 = 14;
/// Vendor py/runtime0.h:121 — first of the 13 normal arithmetic ops.
/// (compile.zig §29.)
pub const MP_BINARY_OP_OR: u8 = 22;
/// Vendor py/runtime0.h:124 — LSHIFT op id; base for the per-token
/// dispatch. (compile.zig §29.)
pub const MP_BINARY_OP_LSHIFT: u8 = 25;
/// Vendor py/runtime0.h:133 — POWER op id used by compilePower.
/// (compile.zig §29.)
pub const MP_BINARY_OP_POWER: u8 = 34;
// NOT_IN and IS_NOT are synthesised by the emitter; their ordinals exceed
// MP_BINARY_OP_NUM_BYTECODE. We only need their values for the
// binary_op dispatcher's `op == NOT_IN / IS_NOT` check; the actual
// bytecode goes via IN/IS + UNARY_NOT.
pub const MP_BINARY_OP_NOT_IN: u8 = 35; // vendor py/runtime0.h:163
pub const MP_BINARY_OP_IS_NOT: u8 = 36; // vendor py/runtime0.h:164

// ── py/lexer.h token ordinals (only the kw ones used here) ──────────────
// Pinned by mp_token_kind_t order: FALSE/NONE/TRUE follow ELLIPSIS.
// Vendor lexer.h:61-65 (with FSTRINGS disabled in nano config → no shift).
pub const MP_TOKEN_ELLIPSIS: u8 = 9;
pub const MP_TOKEN_KW_FALSE: u8 = 10;
pub const MP_TOKEN_KW_NONE: u8 = 11;
pub const MP_TOKEN_KW_TRUE: u8 = 12;

// ── well-known qstr ids (zero-based QDEF0 indices in the embedded table)
pub const MP_QSTR__STAR_: Qstr = 5; // "*"
pub const MP_QSTR_BASE_EXCEPTION: Qstr = 33; // "BaseException"

// ── obj.h: iter-buf size on stack (4 slots on 32- and 64-bit). ──────────
pub const MP_OBJ_ITER_BUF_NSLOTS: usize = 4;

/// Bit-width of the tagged small-int payload. (compile.zig §29:
/// `MP_SMALL_INT_BITS = @bitSizeOf(isize) - 1`.)
pub const MP_SMALL_INT_BITS: usize = usize::BITS as usize - 1;
