//! Method-slot contract — op codes + slot fn types (C6).
//!
//! Moved out of `value.rs` (2026-11-17, Lane C6) so `value.rs` stays
//! under the 800-line cap. The shapes are the frozen C0 interface
//! (design doc D5); only the module changed, nothing else.

use std::fmt;

use crate::memory::gc::CallCtx;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

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
/// `fn(&mut CallCtx, Value, n_args, n_kw, &[Value]) -> PyResult<Value>` —
/// call slot.
///
/// C10 amendment (2026-11-17): the C0 signature was
/// `fn(&mut CallCtx, Value, &[Value])`, which cannot express the Zig
/// call protocol — vendor `mp_call_function_t`
/// (`py/obj.h`): `call(self, n_args, n_kw, args)`, where `args` holds
/// `n_args` positionals followed by `2 * n_kw` `(name, value)` keyword
/// pairs. Without `n_kw` a slot cannot split the slice (kwargs would
/// be misread as positionals), so `fun_bc_call` — the first slot that
/// binds keyword arguments — could not be ported at all. No `call`
/// slot existed yet in the tree (only `call: None` literals), so
/// nothing breaks. Same process as C6's op-parameter amendment.
pub type CallFn =
    for<'a> fn(&mut CallCtx<'a>, Value, usize, usize, &[Value]) -> PyResult<Value>;
/// The VM's hook for running user code (E2b): one object carrying
/// BOTH operations a collection / call site needs from the VM, so
/// they share a single `&mut VmState` borrow. Two separate
/// closures (one to call, one to resume) cannot be built from the
/// same `&mut VmState` — E0499, the same wall E2's `vm_seams.rs`
/// `__getattr__` comment documents — so the hook is a trait object
/// with two methods instead.
///
/// `call` is E2's call hook: call `fun` as a function, through
/// the VM when the caller has one (a `FunBc` callee actually
/// executes). `None` = the ctx-only path: the stub seams'
/// `call_via_type_slot` (a `FunBc` callee raises `PyRaise`, the
/// pre-E2 behaviour). `resume` is the generator-resume hook
/// (E2b): step a generator once, in the `iternext` form —
/// `Ok(Some(v))` = yielded `v`, `Ok(None)` = exhausted,
/// `Err(PyError)` = the generator raised (the exception object
/// is stashed on the VM's `state.pending_exc` side channel, the
/// C14 factory contract, exactly like `call` — the caller drains
/// it). `call` and `resume` share the VM's `state.pending_exc`
/// drain (`drain_seam_raise` / `call_type_ctx`'s drain).
///
/// The hook is passed DOWN as a parameter, never stored in
/// `CallCtx` (an object over `&mut VmState` cannot outlive the
/// call that built it). Two lifetimes: `'r` is the reference
/// borrow (shortened on every reborrow, so a caller can pass the
/// hook down several times), `'h` is the object's own lifetime.
pub trait VmHook {
    /// Call `fun` as a function (the `CallFn` layout).
    fn call(
        &mut self,
        ctx: &mut CallCtx,
        fun: Value,
        n_args: usize,
        n_kw: usize,
        args: &[Value],
    ) -> PyResult<Value>;
    /// Resume `gen` one step (the `iternext` form).
    fn resume(&mut self, ctx: &mut CallCtx, gen: Value) -> Result<Option<Value>, PyError>;
}

/// The VM's user-code hook: `&mut dyn VmHook` — see `VmHook`.
pub type CallHook<'r, 'h> = &'r mut (dyn VmHook + 'h);

/// `fn(&mut CallCtx, Value, n_args, n_kw, &[Value]) -> PyResult<Value>` —
/// make_new slot (`mp_make_new_fun`, vendor `py/obj.h`): construct an
/// instance of `type` from `args` (positionals followed by `2 * n_kw`
/// `(name, value)` keyword pairs — the `CallFn` layout). C12 adds it;
/// builtin constructors are wired by their owning lanes via thin
/// adapters (their native signatures vary — see each `*_make_new`).
/// E2 adds the trailing `call` hook (`Some` on the VM's type-call
/// path, `None` on ctx-only paths — see `CallHook`).
pub type MakeNewFn = for<'a, 'r, 'h> fn(
    &mut CallCtx<'a>,
    GcRef,
    &[Value],
    usize,
    Option<CallHook<'r, 'h>>,
) -> PyResult<Value>;
/// `fn(&mut CallCtx, Value) -> PyResult<Value>` — getiter slot.
pub type GetIterFn = for<'a> fn(&mut CallCtx<'a>, Value) -> PyResult<Value>;
/// Built-in function body (fixed-arity and var-no-kw builtins).
/// Receives positional args only; arity is checked by the
/// `fun_builtin_*_call` adapters before the call.
pub type BuiltinFn = for<'a> fn(&mut CallCtx<'a>, &[Value]) -> PyResult<Value>;
/// Keyword-taking built-in body (`mp_obj_fun_builtin_var_t` with the
/// takes_kw sig bit set, C10): positionals plus the keyword pairs as
/// `(name qstr id, value)`. The `fun_builtin_var_call` adapter builds
/// the pairs from the call's `args` slice; a builtin with a `kw_func`
/// is never called through `func`.
pub type BuiltinKwFn =
    for<'a> fn(&mut CallCtx<'a>, &[Value], &[(u32, Value)]) -> PyResult<Value>;

/// One method-table entry (C13): a type's method name, its
/// `BuiltinFn` body, the positional arity range counting `self`,
/// and the optional `BuiltinKwFn` for keyword-accepting methods
/// (`list.sort`'s `key=`/`reverse=`). The VM builds each type's
/// locals dict from these tables (the Zig's `mp_obj_*_methods`
/// tables carry the same (name, fn, min, max) shape); the sigs
/// here are the ones the builtin adapters already enforce (see
/// their doc comments), so the VM's `call_fun_builtin` sig check
/// records the terse TypeError before the body runs.
pub struct MethodEntry {
    pub name: &'static str,
    pub func: BuiltinFn,
    pub min: u16,
    pub max: u16,
    pub kw_func: Option<BuiltinKwFn>,
}
/// Load-only attribute access (`mp_obj_type_t.attr`, C10).
/// `Ok(Some(v))` = the attribute was found (Zig: `dest[0] = v`);
/// `Ok(None)` = not found (Zig: `dest` left `NULL`); `Err` = the slot
/// raised. Store/delete (`dest[0] != NULL` in the Zig — the attr fns
/// return early, "not handled") is the caller's problem: the
/// reference raises `AttributeError` for stores (`f.__name__ = "x"`,
/// probed 2026-11-17). C12 wires this into `TypeData` (no `attr`
/// field exists yet) together with `mp_load_method`.
pub type AttrFn = for<'a> fn(&mut CallCtx<'a>, Value, u32) -> PyResult<Option<Value>>;

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

// ── Stream protocol (`mp_stream_p_t`, vendor `py/stream.h`) ─────────────
// Ported by Lane C16 (`support/stream.zig`). The vtable lives on
// `TypeData.protocol` (added by this lane); the three fn pointers
// are `Option` because a backend may implement only part of the
// protocol (`get_stream_raise` checks the requested subset).

/// `MpStreamSeek` (`mp_stream_seek`'s in/out struct, vendor
/// `py/stream.h`): the seek target in, the new absolute offset
/// out. The Zig passes `&seek_s` as the ioctl arg; Rust passes
/// `IoctlArg::Seek(&mut SeekState)` (no raw pointer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekState {
    pub offset: i64,
    pub whence: i32,
    /// Filled by the backend with the new absolute offset.
    pub result: i64,
}

impl SeekState {
    pub fn new(offset: i64, whence: i32) -> SeekState {
        SeekState {
            offset,
            whence,
            result: 0,
        }
    }
}

/// The `ioctl` argument: the Zig's `usize arg` is either 0, a
/// pointer to `MpStreamSeek`, or an arbitrary caller value
/// (`streamIoctl` passes a buffer pointer or a truncated int).
/// Rust spells the three shapes out instead of smuggling a
/// pointer through a `usize`.
#[derive(Debug)]
pub enum IoctlArg<'a> {
    /// `arg == 0` (`MP_STREAM_FLUSH`, `MP_STREAM_CLOSE`).
    None,
    /// `MP_STREAM_SEEK`: `&seek_s` (see `SeekState`).
    Seek(&'a mut SeekState),
    /// `streamIoctl`'s arbitrary value: a buffer pointer
    /// (as an address) or a truncated int.
    Raw(usize),
}

/// `fn(&mut CallCtx, Value, &mut [u8]) -> PyResult<usize>` — the
/// `read` slot (`mp_stream_p_t.read`). Returns the bytes read
/// (`0` = EOF / nothing available); `Err` = the Zig's
/// `MP_STREAM_ERROR` + `errcode` — the backend records the
/// `OSError` itself (via `CallCtx::record_*`) before returning
/// `Err(PyError::PyRaise)`. The Zig's non-blocking path
/// (`isNonblockingError`) is hard-coded `false` under MINIMUM
/// ROM level, so there is no "would block" outcome to carry.
pub type StreamReadFn = for<'a> fn(&mut CallCtx<'a>, Value, &mut [u8]) -> PyResult<usize>;

/// `fn(&mut CallCtx, Value, &[u8]) -> PyResult<usize>` — the
/// `write` slot (`mp_stream_p_t.write`). Returns the bytes
/// written; `Err` as for `StreamReadFn`.
pub type StreamWriteFn = for<'a> fn(&mut CallCtx<'a>, Value, &[u8]) -> PyResult<usize>;

/// `fn(&mut CallCtx, Value, u32, IoctlArg) -> PyResult<usize>` —
/// the `ioctl` slot (`mp_stream_p_t.ioctl`). The `u32` is the
/// request (`MP_STREAM_FLUSH` / `MP_STREAM_SEEK` /
/// `MP_STREAM_CLOSE` / a caller value); returns the request's
/// result (`0` for flush/close, the new offset is carried in
/// `SeekState.result` for seek); `Err` as for `StreamReadFn`.
pub type StreamIoctlFn =
    for<'a> fn(&mut CallCtx<'a>, Value, u32, IoctlArg<'_>) -> PyResult<usize>;

/// The stream protocol vtable (`mp_stream_p_t`, vendor
/// `py/stream.h:43`): read / write / ioctl plus the text flag
/// (`is_text`: a text stream's `read` returns `str`, a binary
/// stream's returns `bytes`). `Copy` so `get_stream_raise`
/// can hand it out by value without arena-borrow conflicts.
#[derive(Clone, Copy, Debug)]
pub struct StreamP {
    pub read: Option<StreamReadFn>,
    pub write: Option<StreamWriteFn>,
    pub ioctl: Option<StreamIoctlFn>,
    pub is_text: bool,
}

impl StreamP {
    pub fn new(
        read: Option<StreamReadFn>,
        write: Option<StreamWriteFn>,
        ioctl: Option<StreamIoctlFn>,
        is_text: bool,
    ) -> StreamP {
        StreamP {
            read,
            write,
            ioctl,
            is_text,
        }
    }
}
