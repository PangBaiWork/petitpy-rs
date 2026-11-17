//! Method-slot contract — op codes + slot fn types (C6).
//!
//! Moved out of `value.rs` (2026-11-17, Lane C6) so `value.rs` stays
//! under the 800-line cap. The shapes are the frozen C0 interface
//! (design doc D5); only the module changed, nothing else.

use std::fmt;

use crate::memory::gc::CallCtx;
use crate::objects::value::Value;
use crate::pyerr::PyResult;

// ── Method-slot fn types (mirror `mp_obj_type_t` slots) ─────────────
//
// C6 note (2026-11-17): the C0 skeleton's `UnaryOpFn`/`BinaryOpFn`
// took no op argument, but the original's slots are
// `mp_unary_op(op, arg)` / `mp_binary_op(op, lhs, rhs)` — without the
// op the dispatch in `obj.rs` (`mp_obj_len` needs LEN, `mp_obj_is_true`
// needs BOOL, `mp_obj_equal` needs EQUAL/NOT_EQUAL) cannot be
// expressed at all. The op is therefore part of the slot contract
// now; no lane had implemented a slot yet (verified by grep — only
// the definitions and the `TypeData` fields existed), so nothing
// breaks. The op codes are the vendor `mp_unary_op_t` /
// `mp_binary_op_t` ordinals (`py/runtime0.h:72-88` / `:89-164`);
// `compiler::emit::consts` defines the same numbers for the emitter
// (NOT_IN/IS_NOT there are 35/36 — the emitter's private values,
// never emitted; see `BinaryOp::NotIn`).

/// Unary-op codes (`mp_unary_op_t`, vendor `py/runtime0.h:72-88`).
/// The first four may appear in bytecode; the rest are runtime-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum UnaryOp {
    Positive = 0,
    Negative = 1,
    Invert = 2,
    Not = 3,
    /// `__bool__`.
    Bool = 4,
    /// `__len__`.
    Len = 5,
    /// `__hash__`; must return a small int.
    Hash = 6,
    /// `__abs__`.
    Abs = 7,
    /// `__int__`; must return NULL, or an object satisfying `mp_obj_is_int()`.
    IntMaybe = 8,
    /// `__float__`.
    FloatMaybe = 9,
    /// `__complex__`.
    ComplexMaybe = 10,
    /// `sys.getsizeof()`.
    Sizeof = 11,
}

impl UnaryOp {
    const ALL: [UnaryOp; 12] = [
        UnaryOp::Positive,
        UnaryOp::Negative,
        UnaryOp::Invert,
        UnaryOp::Not,
        UnaryOp::Bool,
        UnaryOp::Len,
        UnaryOp::Hash,
        UnaryOp::Abs,
        UnaryOp::IntMaybe,
        UnaryOp::FloatMaybe,
        UnaryOp::ComplexMaybe,
        UnaryOp::Sizeof,
    ];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<UnaryOp> {
        Self::ALL.get(v as usize).copied()
    }
}

/// Binary-op codes (`mp_binary_op_t`, vendor `py/runtime0.h:89-164`).
/// The first 35 may appear in bytecode; DIVMOD/CONTAINS are
/// runtime-only; the REVERSE_* group follows; NOT_IN/IS_NOT are
/// synthesised by the emitter and never reach the runtime.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BinaryOp {
    Less = 0,
    More = 1,
    Equal = 2,
    LessEqual = 3,
    MoreEqual = 4,
    NotEqual = 5,
    In = 6,
    Is = 7,
    ExceptionMatch = 8,
    InplaceOr = 9,
    InplaceXor = 10,
    InplaceAnd = 11,
    InplaceLshift = 12,
    InplaceRshift = 13,
    InplaceAdd = 14,
    InplaceSubtract = 15,
    InplaceMultiply = 16,
    InplaceMatMultiply = 17,
    InplaceFloorDivide = 18,
    InplaceTrueDivide = 19,
    InplaceModulo = 20,
    InplacePower = 21,
    Or = 22,
    Xor = 23,
    And = 24,
    Lshift = 25,
    Rshift = 26,
    Add = 27,
    Subtract = 28,
    Multiply = 29,
    MatMultiply = 30,
    FloorDivide = 31,
    TrueDivide = 32,
    Modulo = 33,
    Power = 34,
    Divmod = 35,
    Contains = 36,
    ReverseOr = 37,
    ReverseXor = 38,
    ReverseAnd = 39,
    ReverseLshift = 40,
    ReverseRshift = 41,
    ReverseAdd = 42,
    ReverseSubtract = 43,
    ReverseMultiply = 44,
    ReverseMatMultiply = 45,
    ReverseFloorDivide = 46,
    ReverseTrueDivide = 47,
    ReverseModulo = 48,
    ReversePower = 49,
    NotIn = 50,
    IsNot = 51,
}

impl BinaryOp {
    const ALL: [BinaryOp; 52] = [
        BinaryOp::Less,
        BinaryOp::More,
        BinaryOp::Equal,
        BinaryOp::LessEqual,
        BinaryOp::MoreEqual,
        BinaryOp::NotEqual,
        BinaryOp::In,
        BinaryOp::Is,
        BinaryOp::ExceptionMatch,
        BinaryOp::InplaceOr,
        BinaryOp::InplaceXor,
        BinaryOp::InplaceAnd,
        BinaryOp::InplaceLshift,
        BinaryOp::InplaceRshift,
        BinaryOp::InplaceAdd,
        BinaryOp::InplaceSubtract,
        BinaryOp::InplaceMultiply,
        BinaryOp::InplaceMatMultiply,
        BinaryOp::InplaceFloorDivide,
        BinaryOp::InplaceTrueDivide,
        BinaryOp::InplaceModulo,
        BinaryOp::InplacePower,
        BinaryOp::Or,
        BinaryOp::Xor,
        BinaryOp::And,
        BinaryOp::Lshift,
        BinaryOp::Rshift,
        BinaryOp::Add,
        BinaryOp::Subtract,
        BinaryOp::Multiply,
        BinaryOp::MatMultiply,
        BinaryOp::FloorDivide,
        BinaryOp::TrueDivide,
        BinaryOp::Modulo,
        BinaryOp::Power,
        BinaryOp::Divmod,
        BinaryOp::Contains,
        BinaryOp::ReverseOr,
        BinaryOp::ReverseXor,
        BinaryOp::ReverseAnd,
        BinaryOp::ReverseLshift,
        BinaryOp::ReverseRshift,
        BinaryOp::ReverseAdd,
        BinaryOp::ReverseSubtract,
        BinaryOp::ReverseMultiply,
        BinaryOp::ReverseMatMultiply,
        BinaryOp::ReverseFloorDivide,
        BinaryOp::ReverseTrueDivide,
        BinaryOp::ReverseModulo,
        BinaryOp::ReversePower,
        BinaryOp::NotIn,
        BinaryOp::IsNot,
    ];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<BinaryOp> {
        Self::ALL.get(v as usize).copied()
    }
}

/// `fn(&mut CallCtx, UnaryOp, Value) -> PyResult<Value>` — unary-op slot
/// (`mp_unary_op` in vendor `py/runtime.h`).
pub type UnaryOpFn = for<'a> fn(&mut CallCtx<'a>, UnaryOp, Value) -> PyResult<Value>;
/// `fn(&mut CallCtx, BinaryOp, Value, Value) -> PyResult<Value>` — binary-op slot.
pub type BinaryOpFn = for<'a> fn(&mut CallCtx<'a>, BinaryOp, Value, Value) -> PyResult<Value>;
/// `fn(&mut CallCtx, Value, &[Value]) -> PyResult<Value>` — call slot.
pub type CallFn = for<'a> fn(&mut CallCtx<'a>, Value, &[Value]) -> PyResult<Value>;
/// `fn(&mut CallCtx, Value) -> PyResult<Value>` — getiter slot.
pub type GetIterFn = for<'a> fn(&mut CallCtx<'a>, Value) -> PyResult<Value>;
/// Built-in function body.
pub type BuiltinFn = for<'a> fn(&mut CallCtx<'a>, &[Value]) -> PyResult<Value>;

/// Subscript argument: the Zig `mp_obj_subscr(base, index, value)`
/// encodes get/set/del in one `Obj` (`MP_OBJ_NULL` = delete,
/// `OBJ_SENTINEL` = get). Rust spells the three states out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubscrArg {
    /// `value == OBJ_SENTINEL`: `base[index]`.
    Get,
    /// `base[index] = value`.
    Set(Value),
    /// `del base[index]` (`value == MP_OBJ_NULL`).
    Delete,
}

/// `fn(&mut CallCtx, Value, Value, SubscrArg) -> PyResult<Value>` —
/// subscr slot (`mp_subscr` in vendor `py/obj.h`).
pub type SubscrFn = for<'a> fn(&mut CallCtx<'a>, Value, Value, SubscrArg) -> PyResult<Value>;

/// Buffer info (`mp_buffer_info_t`, vendor `py/buffer.h`): the memory
/// a buffer slot hands out. The data pointer is raw because the buffer
/// protocol is inherently about raw memory; reading through it is
/// `unsafe` and belongs to the lane that fills it (C9: bytes/array).
/// Storing the pointer is safe code.
#[derive(Clone, Copy, Debug)]
pub struct BufferInfo {
    pub data: *const u8,
    pub len: usize,
    /// Array typecode (`b`, `B`, …); 0 = plain bytes.
    pub typecode: u8,
}

impl BufferInfo {
    pub fn empty() -> BufferInfo {
        BufferInfo {
            data: std::ptr::null(),
            len: 0,
            typecode: 0,
        }
    }
}

/// `fn(&mut CallCtx, Value, &mut BufferInfo, u32) -> PyResult<bool>` —
/// buffer slot (`mp_buffer_p_t.get_buffer`). The `u32` is the flags
/// word with `MP_BUFFER_RAISE_IF_UNSUPPORTED` already masked off
/// (`flags & MP_BUFFER_RW` — see `obj::get_buffer`); `Ok(true)` means
/// the info was filled, `Ok(false)` means "not supported".
pub type BufferFn = for<'a> fn(&mut CallCtx<'a>, Value, &mut BufferInfo, u32) -> PyResult<bool>;

/// Print kinds (`mp_print_kind_t`, vendor `py/obj.h:580-586`).
/// `PRINT_EXC_SUBCLASS` (0x80) is a flag OR'd into the kind by the
/// exception printer (C12); it is not a kind of its own here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PrintKind {
    Str = 0,
    Repr = 1,
    Exc = 2,
    Json = 3,
    Raw = 4,
}

/// `fn(&mut CallCtx, Value, PrintKind, &mut dyn Write)` — print slot
/// (`mp_print_t` in vendor `py/mpprint.h`). Returns `()` like the
/// original (void): sink errors are ignored, as the original ignores
/// `mp_printf`'s return.
pub type PrintFn = for<'a> fn(&mut CallCtx<'a>, Value, PrintKind, &mut dyn fmt::Write);
