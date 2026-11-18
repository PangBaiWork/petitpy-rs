//! Object core — type-slot dispatch + the `mp_obj_*` helpers.
//!
//! Port of `src/objects/obj.zig` (835 lines, 0 test blocks; the Zig
//! port of vendor `py/obj.c`). `obj.zig` is `@cImport("py/obj.h",
//! "py/runtime.h")` + `extern fn` into vendor C + `mp_mirrors`; the C
//! declarations are NOT ported (no C left to mirror — design doc D1),
//! only their semantics are.
//!
//! What lives here: the slot-dispatch core every object lane builds
//! on (look the slot up on the value's type, call it, fall back
//! exactly as the original does) plus the `mp_obj_*` helpers for
//! int/float/complex extraction, len, truth, callable and id. The
//! rest of the `obj.zig` port lives in `obj_ops.rs` (equality,
//! printing, indexing, exceptions) and dispatches through here.
//!
//! Error model (the C5 `SetupError` precedent): the original raises
//! by building an exception object into `pyerr.pending_exc` and
//! returning `error.PyRaise`. `VmState.pending_exc` does not exist
//! until C13/C14 (design doc D7), so raises here are `ObjError`
//! values carrying exactly the data the original formats into the
//! exception — the exception type and the TERSE message literal. The
//! VM maps an `ObjError` to a `pending_exc` object when it lands.
//!
//! Slot outcomes: a slot that returns `Err(PyError::PyRaise)` is the
//! Zig `NULL` return — "not implemented" — so the caller falls
//! through to the next candidate exactly as the original's `NULL`
//! checks do. `Err(PyError::OutOfMemory)` is a real error and
//! propagates. (The Zig code distinguishes the two via the
//! `pending_exc` global; until `CallCtx` gains `pending_exc` — C13
//! grows it into the `Vm` — a slot's own raise is dropped together
//! with its "not implemented", so the control flow is the original's
//! but the raise data is not yet preserved. See `scripts/
//! objcore_result.txt`.)
//!
//! Deferred to other lanes (NOT silently dropped — see the result
//! file): the `list[str]` / `__class_getitem__` branch of
//! `mp_obj_subscr` (needs `runtime.typingCall`, C14/C15); the
//! `nativeBase` step of `mp_obj_get_float_maybe` and the
//! instance-call nuance of `mp_obj_is_callable` (need the instance
//! representation, C12 — no `Instance` variant exists in the frozen
//! `Obj` enum yet); dynamic-qstr byte/length resolution (the bytes
//! live in the VM's `RuntimeQstr` pool — static ids resolve through
//! the generated table here; `obj_print_exception` takes an explicit
//! resolver because traceback file names are dynamic in practice);
//! the standalone `mp_unary_op` runtime function (the MAYBE-only
//! dispatch used here is the private `call_unary` helper);
//! `mp_obj_malloc_helper` (subsumed by `CallCtx::alloc` — it is a
//! four-line allocate-and-tag-type wrapper, no Rust counterpart is
//! needed).

use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::slots::{BinaryOp, BufferFn, PrintFn, SubscrArg, UnaryOp};
use crate::objects::value::{GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// `mp_obj_type_t.flags` bits (vendor `py/obj.h:566-577`).
pub const FLAG_EQ_NOT_REFLEXIVE: u16 = 0x0004;
pub const FLAG_EQ_CHECKS_OTHER_TYPE: u16 = 0x0008;
pub const FLAG_EQ_HAS_NEQ_TEST: u16 = 0x0010;
/// `MP_TYPE_FLAG_HAS_SPECIAL_ACCESSORS` (vendor `py/obj.h:565-578`,
/// `mp_mirrors.zig:676`): the type's locals contain a property,
/// a descriptor, or `__setattr__`/`__delattr__` — instance
/// attribute access goes through the descriptor dispatch. C12 adds it.
pub const FLAG_HAS_SPECIAL_ACCESSORS: u16 = 0x0002;
/// `MP_TYPE_FLAG_ITER_IS_ITERNEXT` (vendor `py/obj.h:565-578`,
/// `mp_mirrors.zig:683`): the type's iter slot IS the `iternext`
/// function itself (polymorph iterators). C12 adds it.
pub const FLAG_ITER_IS_ITERNEXT: u16 = 0x0080;
/// `MP_TYPE_FLAG_ITER_IS_CUSTOM` (`mp_mirrors.zig:684`). C12 adds it.
pub const FLAG_ITER_IS_CUSTOM: u16 = 0x0100;
/// `MP_TYPE_FLAG_ITER_IS_STREAM` = ITERNEXT | CUSTOM
/// (`mp_mirrors.zig:685`). C12 adds it.
pub const FLAG_ITER_IS_STREAM: u16 = 0x0180;
/// `MP_TYPE_FLAG_BINDS_SELF` (vendor `py/obj.h:566-577`): the type
/// binds `self` when its methods are accessed — function and
/// closure types set it (C10).
pub const FLAG_BINDS_SELF: u16 = 0x0020;
/// `MP_TYPE_FLAG_BUILTIN_FUN` (vendor `py/obj.h:566-577`): a
/// built-in function type (C10).
pub const FLAG_BUILTIN_FUN: u16 = 0x0040;
pub const FLAG_INSTANCE_TYPE: u16 = 0x0200;
/// `MP_TYPE_FLAG_SUBSCR_ALLOWS_STACK_SLICE` (vendor
/// `py/obj.h:578`): the subscr slot accepts a slice object
/// allocated on the stack. bytearray and memoryview set it;
/// array does not (its slice path still works — the flag only
/// gates the VM's stack-slice fast path, C13/C14).
pub const FLAG_SUBSCR_ALLOWS_STACK_SLICE: u16 = 0x0400;

/// Buffer-protocol flags (vendor `py/obj.h:622-625`).
pub const BUFFER_READ: u32 = 1;
pub const BUFFER_WRITE: u32 = 2;
pub const BUFFER_RW: u32 = BUFFER_READ | BUFFER_WRITE;
pub const BUFFER_RAISE_IF_UNSUPPORTED: u32 = 4;

pub use crate::objects::builtin_types::BuiltinTypes;

/// The exception type an `ObjError` raises, by name — the type
/// objects do not exist yet; the VM resolves the name through the
/// builtins registry when `VmState.pending_exc` lands.
/// `ZeroDivisionError` added by C7r (the int lane's `PrimError`
/// needs it: `0 ** -1` and friends raise it — verified on the
/// reference binary).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExcType {
    TypeError,
    ValueError,
    IndexError,
    OverflowError,
    ZeroDivisionError,
    /// `NotImplementedError` added by C12 (objtype's `raiseMI`:
    /// "multiple inheritance not supported" — verified on the
    /// reference binary).
    NotImplementedError,
}

impl ExcType {
    pub fn name(self) -> &'static str {
        match self {
            ExcType::TypeError => "TypeError",
            ExcType::ValueError => "ValueError",
            ExcType::IndexError => "IndexError",
            ExcType::OverflowError => "OverflowError",
            ExcType::ZeroDivisionError => "ZeroDivisionError",
            ExcType::NotImplementedError => "NotImplementedError",
        }
    }
}

/// A raise signalled by an `obj.rs` helper: exactly the data Zig's
/// raise helpers (`raiseIntConversion` etc., `obj.zig:127-175`)
/// format into the exception object — the exception type plus the
/// TERSE message literal (ROM_LEVEL_MINIMUM reporting).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjError {
    /// `TypeError: can't convert to int`.
    IntConversion,
    /// `OverflowError: overflow converting long int to machine word`.
    Overflow,
    /// `TypeError: object has no len`.
    NoLen,
    /// `TypeError: expected tuple/list`.
    ExpectedTupleList,
    /// `ValueError: tuple/list has wrong length`.
    WrongLength,
    /// `TypeError: indices must be integers`.
    IndicesMustBeInts,
    /// `IndexError: index out of range`.
    IndexOutOfRange,
    /// `TypeError: object doesn't support item deletion`.
    NoItemDeletion,
    /// `TypeError: object isn't subscriptable`.
    NotSubscriptable,
    /// `TypeError: object doesn't support item assignment`.
    NoItemAssignment,
    /// `TypeError: object with buffer protocol required`.
    BufferProtocol,
    /// `TypeError: can't convert to float`.
    FloatConversion,
    /// `TypeError: can't convert to complex`.
    ComplexConversion,
    /// An allocation failed (carries `PyError::OutOfMemory` through).
    OutOfMemory,
}

impl ObjError {
    /// The exception type this error raises (`None` for
    /// `OutOfMemory`, which has no exception object).
    pub fn exc_type(self) -> Option<ExcType> {
        match self {
            ObjError::IntConversion
            | ObjError::NoLen
            | ObjError::ExpectedTupleList
            | ObjError::IndicesMustBeInts
            | ObjError::NoItemDeletion
            | ObjError::NotSubscriptable
            | ObjError::NoItemAssignment
            | ObjError::BufferProtocol
            | ObjError::FloatConversion
            | ObjError::ComplexConversion => Some(ExcType::TypeError),
            ObjError::WrongLength => Some(ExcType::ValueError),
            ObjError::IndexOutOfRange => Some(ExcType::IndexError),
            ObjError::Overflow => Some(ExcType::OverflowError),
            ObjError::OutOfMemory => None,
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory`).
    pub fn message(self) -> Option<&'static str> {
        match self {
            ObjError::IntConversion => Some("can't convert to int"),
            ObjError::Overflow => Some("overflow converting long int to machine word"),
            ObjError::NoLen => Some("object has no len"),
            ObjError::ExpectedTupleList => Some("expected tuple/list"),
            ObjError::WrongLength => Some("tuple/list has wrong length"),
            ObjError::IndicesMustBeInts => Some("indices must be integers"),
            ObjError::IndexOutOfRange => Some("index out of range"),
            ObjError::NoItemDeletion => Some("object doesn't support item deletion"),
            ObjError::NotSubscriptable => Some("object isn't subscriptable"),
            ObjError::NoItemAssignment => Some("object doesn't support item assignment"),
            ObjError::BufferProtocol => Some("object with buffer protocol required"),
            ObjError::FloatConversion => Some("can't convert to float"),
            ObjError::ComplexConversion => Some("can't convert to complex"),
            ObjError::OutOfMemory => None,
        }
    }

    /// The plain `PyError` signal (`PyRaise` for a Python raise).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ObjError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for ObjError {
    fn raise_name(&self) -> &'static str {
        self.exc_type().map(|t| t.name()).unwrap_or("")
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.message().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.to_pyerror()
    }
}

// ── qstr helpers (static table only) ──────────────────────────────────

/// Static qstr bytes for `id` (`QSTR_STRINGS`); `None` for dynamic
/// ids (>= N_TOTAL), whose bytes live in the VM's `RuntimeQstr` pool.
pub(crate) fn qstr_bytes(id: u32) -> Option<&'static [u8]> {
    crate::qstr_table::QSTR_STRINGS.get(id as usize).copied()
}

// ── Type helpers ──────────────────────────────────────────────────────

/// `mp_obj_get_type` (`obj.zig:518-546`): the heap header's type for
/// heap objects; the immediate table for immediates (small int → int,
/// qstr → str, `0b0110` → NoneType, `0b1110` → bool — the Zig
/// 16-entry table indexed by `o & 0xf`). `None` only while
/// bootstrapping (a header with no type yet — the Zig code returns a
/// NULL type pointer there).
pub fn obj_get_type(arena: &GcArena, types: &BuiltinTypes, value: Value) -> Option<GcRef> {
    match value {
        Value::SmallInt(_) => Some(types.int),
        Value::Bool(_) => Some(types.bool_),
        Value::None => Some(types.none_type),
        Value::Qstr(_) => Some(types.str_),
        Value::Heap(r) => arena.header(r).type_,
    }
}

/// Read a type's `TypeData` from the arena (`None` if the ref does
/// not name an `Obj::Type` — cannot happen for well-formed arenas;
/// the Zig code dereferences the type pointer unconditionally).
pub(crate) fn type_data<'a>(arena: &'a GcArena, t: GcRef) -> Option<&'a TypeData> {
    match arena.get(t) {
        Obj::Type(d) => Some(d),
        _ => None,
    }
}

/// `mp_obj_is_str` (macro, `obj.zig` isStr): qstr immediate OR
/// exact-type str.
pub fn is_str(arena: &GcArena, types: &BuiltinTypes, value: Value) -> bool {
    matches!(value, Value::Qstr(_))
        || matches!(value, Value::Heap(r) if arena.header(r).type_ == Some(types.str_))
}

/// Exact-type bytes (`obj.zig` isBytes).
pub fn is_bytes(arena: &GcArena, types: &BuiltinTypes, value: Value) -> bool {
    matches!(value, Value::Heap(r) if arena.header(r).type_ == Some(types.bytes))
}

/// `mp_obj_is_float` (macro, `obj.zig` isFloat): under REPR_A the
/// only float representation is heap-allocated → exact-type check.
pub fn is_float(arena: &GcArena, types: &BuiltinTypes, value: Value) -> bool {
    matches!(value, Value::Heap(r) if arena.header(r).type_ == Some(types.float))
}

/// Exact-type int (heap `Obj::Int` with the int type header).
pub fn is_exact_int(arena: &GcArena, types: &BuiltinTypes, value: Value) -> bool {
    matches!(value, Value::Heap(r) if arena.header(r).type_ == Some(types.int))
}

/// Exact-type complex.
fn is_exact_complex(arena: &GcArena, types: &BuiltinTypes, value: Value) -> bool {
    matches!(value, Value::Heap(r) if arena.header(r).type_ == Some(types.complex))
}

// ── Slot-call helpers ─────────────────────────────────────────────────

/// What a slot call produced. The Zig slots return `Obj` with
/// `MP_OBJ_NULL` meaning "not implemented"; the Rust slots return
/// `PyResult<Value>` where `Err(PyRaise)` is that NULL (see the
/// module docs) and `Err(OutOfMemory)` is a real error.
pub(crate) enum SlotOutcome {
    Value(Value),
    NotImplemented,
}

/// Call a type's unary-op slot (`mp_unary_op` MAYBE-only subset —
/// the full runtime `mp_unary_op` with its raise paths is C14/C15's;
/// see the module docs). `None` type or no slot → NotImplemented.
pub(crate) fn call_unary(
    ctx: &mut CallCtx,
    type_: Option<GcRef>,
    op: UnaryOp,
    arg: Value,
) -> Result<SlotOutcome, ObjError> {
    // The runtime `mp_unary_op`'s small-int fast path
    // (`runtime.zig:355-362`): INT_MAYBE on a small int returns the
    // small int itself, WITHOUT consulting the type's slot. Only
    // INT_MAYBE needs it here — LEN/BOOL go through the type's slot
    // directly in the Zig (`mp_obj_len_maybe`, `mp_obj_is_true`), and
    // FLOAT/COMPLEX_MAYBE never reach a small int (their callers
    // handle immediates first).
    if op == UnaryOp::IntMaybe {
        if let Value::SmallInt(_) = arg {
            return Ok(SlotOutcome::Value(arg));
        }
    }
    let slot = match type_ {
        Some(t) => match ctx.arena.get(t) {
            Obj::Type(d) => d.unary_op,
            _ => None,
        },
        None => None,
    };
    match slot {
        Some(f) => match f(ctx, op, arg) {
            Ok(v) => Ok(SlotOutcome::Value(v)),
            Err(PyError::OutOfMemory) => Err(ObjError::OutOfMemory),
            Err(PyError::PyRaise) => Ok(SlotOutcome::NotImplemented),
        },
        None => Ok(SlotOutcome::NotImplemented),
    }
}

/// Call a type's binary-op slot. Same outcome convention as
/// `call_unary`.
pub(crate) fn call_binary(
    ctx: &mut CallCtx,
    type_: Option<GcRef>,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> Result<SlotOutcome, ObjError> {
    let slot = match type_ {
        Some(t) => match ctx.arena.get(t) {
            Obj::Type(d) => d.binary_op,
            _ => None,
        },
        None => None,
    };
    match slot {
        Some(f) => match f(ctx, op, lhs, rhs) {
            Ok(v) => Ok(SlotOutcome::Value(v)),
            Err(PyError::OutOfMemory) => Err(ObjError::OutOfMemory),
            Err(PyError::PyRaise) => Ok(SlotOutcome::NotImplemented),
        },
        None => Ok(SlotOutcome::NotImplemented),
    }
}

/// Call a type's subscr slot. Same outcome convention.
pub(crate) fn call_subscr(
    ctx: &mut CallCtx,
    type_: Option<GcRef>,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> Result<SlotOutcome, ObjError> {
    let slot = match type_ {
        Some(t) => match ctx.arena.get(t) {
            Obj::Type(d) => d.subscr,
            _ => None,
        },
        None => None,
    };
    match slot {
        Some(f) => match f(ctx, base, index, arg) {
            Ok(v) => Ok(SlotOutcome::Value(v)),
            Err(PyError::OutOfMemory) => Err(ObjError::OutOfMemory),
            Err(PyError::PyRaise) => Ok(SlotOutcome::NotImplemented),
        },
        None => Ok(SlotOutcome::NotImplemented),
    }
}

/// Read a type's buffer slot out of the arena (`None` = no slot).
pub(crate) fn buffer_slot(arena: &GcArena, type_: Option<GcRef>) -> Option<BufferFn> {
    match type_ {
        Some(t) => match arena.get(t) {
            Obj::Type(d) => d.buffer,
            _ => None,
        },
        None => None,
    }
}

/// Read a type's print slot out of the arena (`None` = no slot).
pub(crate) fn print_slot(arena: &GcArena, type_: Option<GcRef>) -> Option<PrintFn> {
    match type_ {
        Some(t) => match arena.get(t) {
            Obj::Type(d) => d.print,
            _ => None,
        },
        None => None,
    }
}

// ── int extraction ────────────────────────────────────────────────────

/// `intGetChecked` (`obj.zig:231-241`): the machine-word value of an
/// int object. `Ok(Some(n))` = fits; `Ok(None)` = not an int at all
/// (a `__int__` that broke its contract — the original reads the
/// value out of non-int memory here, which is UB; the Rust port
/// treats it as "not an int" instead); `Err(Overflow)` = bignum
/// overflow (records the raise the caller must preserve).
fn int_checked(arena: &GcArena, o: Value) -> Result<Option<i64>, ObjError> {
    match o {
        Value::Bool(false) => Ok(Some(0)),
        Value::Bool(true) => Ok(Some(1)),
        Value::SmallInt(n) => Ok(Some(n)),
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => match z.as_int_checked() {
                Some(n) => Ok(Some(n)),
                None => Err(ObjError::Overflow),
            },
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// `mp_obj_get_int_maybe` (`obj.zig:197-228`, full config).
/// Tri-state because the overflow path records a raise the caller
/// must preserve (`mpObjGetIntImpl` keeps a pre-existing
/// `pending_exc`): `Ok(Some(n))` = got it, `Ok(None)` = not an int
/// (no error recorded), `Err(Overflow)` = bignum overflow.
/// The Zig `arg == NULL` / `pending_exc != 0` defensive branches
/// have no Rust counterpart (`Value` is non-nullable; errors travel
/// as `Result`s, not a global).
pub fn obj_get_int_maybe(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<Option<i64>, ObjError> {
    match arg {
        Value::Bool(false) => Ok(Some(0)),
        Value::Bool(true) => Ok(Some(1)),
        Value::SmallInt(n) => Ok(Some(n)),
        Value::Heap(_) if is_exact_int(ctx.arena, types, arg) => int_checked(ctx.arena, arg),
        _ => {
            // `mp_unary_op(MP_UNARY_OP_INT_MAYBE, arg)`; NULL means
            // "no __int__" (or a swallowed raise — see module docs).
            match call_unary(ctx, obj_get_type(ctx.arena, types, arg), UnaryOp::IntMaybe, arg)? {
                SlotOutcome::Value(v) => int_checked(ctx.arena, v),
                SlotOutcome::NotImplemented => Ok(None),
            }
        }
    }
}

/// `mp_obj_get_int` (`obj.zig:243-250`): `get_int_maybe` or
/// `TypeError: can't convert to int` — preserving a recorded
/// Overflow (the Zig `pending_exc == 0` check).
pub fn obj_get_int(ctx: &mut CallCtx, types: &BuiltinTypes, arg: Value) -> Result<i64, ObjError> {
    match obj_get_int_maybe(ctx, types, arg)? {
        Some(n) => Ok(n),
        None => Err(ObjError::IntConversion),
    }
}

/// `mp_obj_get_int_truncated` (`obj.zig:251-257`): int values are
/// truncated to the machine word (`mpz_hash` for bignums — vendor
/// `objint_mpz.c:420-428`, i.e. `BigInt::hash_mpz`); anything else
/// goes through `get_int`.
pub fn obj_get_int_truncated(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<i64, ObjError> {
    match arg {
        Value::SmallInt(n) => Ok(n),
        Value::Heap(r) if is_exact_int(ctx.arena, types, arg) => match ctx.arena.get(r) {
            Obj::Int(z) => Ok(z.hash_mpz()),
            _ => Ok(0),
        },
        _ => obj_get_int(ctx, types, arg),
    }
}

/// `mp_obj_get_uint` (`obj.zig`, full-only `longInt.getUintImpl`):
/// `mp_obj_int_get_uint_checked` — small int >= 0 → value; bignum →
/// `mpz_as_uint_checked`; anything else raises OverflowError with
/// the same message as the int path (vendor `objint_mpz.c:449-462`).
pub fn obj_get_uint(ctx: &mut CallCtx, types: &BuiltinTypes, arg: Value) -> Result<u64, ObjError> {
    let mut arg = arg;
    if !is_exact_int(ctx.arena, types, arg) {
        match call_unary(ctx, obj_get_type(ctx.arena, types, arg), UnaryOp::IntMaybe, arg)? {
            SlotOutcome::Value(v) => arg = v,
            SlotOutcome::NotImplemented => return Err(ObjError::IntConversion),
        }
    }
    match arg {
        Value::Bool(false) => Ok(0),
        Value::Bool(true) => Ok(1),
        Value::SmallInt(n) if n >= 0 => Ok(n as u64),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => z.as_uint_checked().ok_or(ObjError::Overflow),
            _ => Err(ObjError::Overflow),
        },
        _ => Err(ObjError::Overflow),
    }
}

/// `mp_obj_get_ll` (`obj.zig`, full-only `longInt.getLlImpl`):
/// small int → value; else `mp_obj_int_to_bytes_impl(arg,
/// big_endian=true, sizeof(c_longlong))` into an 8-byte buffer —
/// the return is IGNORED (overflow wraps silently, vendor
/// `objint_mpz.c:115-119` + `mpz_as_bytes` with `as_signed = neg`).
pub fn obj_get_ll(ctx: &mut CallCtx, types: &BuiltinTypes, arg: Value) -> Result<i64, ObjError> {
    let mut arg = arg;
    if !is_exact_int(ctx.arena, types, arg) {
        match call_unary(ctx, obj_get_type(ctx.arena, types, arg), UnaryOp::IntMaybe, arg)? {
            SlotOutcome::Value(v) => arg = v,
            SlotOutcome::NotImplemented => return Err(ObjError::IntConversion),
        }
    }
    match arg {
        Value::Bool(false) => Ok(0),
        Value::Bool(true) => Ok(1),
        Value::SmallInt(n) => Ok(n),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => {
                let mut buf = [0u8; 8];
                let _ = z.as_bytes(true, z.is_neg(), &mut buf);
                Ok(i64::from_be_bytes(buf))
            }
            _ => Err(ObjError::IntConversion),
        },
        _ => Err(ObjError::IntConversion),
    }
}

// ── float / complex extraction ────────────────────────────────────────

/// `mp_obj_get_float_maybe` (`obj.zig:261-281`, full config).
/// `None` = "not float-convertible" (the Zig `NULL` out-param is
/// untouched). The `nativeBase` step — for an instance of a Python
/// subclass of float, the value lives in the native base object
/// (`subobj[0]`, `opmethods.zig:27-33`) — is NOT implemented: no
/// instance representation exists in the frozen `Obj` enum yet, so
/// there is no `subobj[0]` to read (C12). Until then this returns
/// `None` for a float-subclass instance without its own
/// `__float__`; see the result file.
pub fn obj_get_float_maybe(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Option<f64> {
    match arg {
        Value::Bool(false) => Some(0.0),
        Value::Bool(true) => Some(1.0),
        Value::SmallInt(n) => Some(n as f64),
        _ => {
            if is_exact_int(ctx.arena, types, arg) {
                return match ctx.arena.get(arg.as_heap().unwrap()) {
                    Obj::Int(z) => Some(z.as_float()),
                    _ => None,
                };
            }
            if is_float(ctx.arena, types, arg) {
                return match ctx.arena.get(arg.as_heap().unwrap()) {
                    Obj::Float(f) => Some(*f),
                    _ => None,
                };
            }
            // `mp_unary_op(MP_UNARY_OP_FLOAT_MAYBE, arg)`; NULL →
            // fall through to the `nativeBase` step (deferred).
            if let Ok(SlotOutcome::Value(v)) = call_unary(
                ctx,
                obj_get_type(ctx.arena, types, arg),
                UnaryOp::FloatMaybe,
                arg,
            ) {
                if is_float(ctx.arena, types, v) {
                    if let Value::Heap(r) = v {
                        if let Obj::Float(f) = ctx.arena.get(r) {
                            return Some(*f);
                        }
                    }
                }
            }
            None
        }
    }
}

/// `mp_obj_get_float` (`obj.zig:283-284`): `get_float_maybe` or
/// `TypeError: can't convert to float` (unconditional — no
/// preserve-check, unlike the int path).
pub fn obj_get_float(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<f64, ObjError> {
    obj_get_float_maybe(ctx, types, arg).ok_or(ObjError::FloatConversion)
}

/// `mp_obj_get_complex_maybe` (`obj.zig:286-303`): any
/// float-convertible value is a complex with imag 0; else the exact
/// complex value; else the `__complex__` slot's result.
pub fn obj_get_complex_maybe(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Option<(f64, f64)> {
    if let Some(f) = obj_get_float_maybe(ctx, types, arg) {
        return Some((f, 0.0));
    }
    if is_exact_complex(ctx.arena, types, arg) {
        if let Value::Heap(r) = arg {
            if let Obj::Complex { re, im } = ctx.arena.get(r) {
                return Some((*re, *im));
            }
        }
    }
    if let Ok(SlotOutcome::Value(v)) = call_unary(
        ctx,
        obj_get_type(ctx.arena, types, arg),
        UnaryOp::ComplexMaybe,
        arg,
    ) {
        if is_exact_complex(ctx.arena, types, v) {
            if let Value::Heap(r) = v {
                if let Obj::Complex { re, im } = ctx.arena.get(r) {
                    return Some((*re, *im));
                }
            }
        }
    }
    None
}

/// `mp_obj_get_complex` (`obj.zig:305-308`): `get_complex_maybe` or
/// `TypeError: can't convert to complex`.
pub fn obj_get_complex(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<(f64, f64), ObjError> {
    obj_get_complex_maybe(ctx, types, arg).ok_or(ObjError::ComplexConversion)
}

// ── len ───────────────────────────────────────────────────────────────

/// `mp_obj_len_maybe` (`obj.zig:421-429`): the length, or `None`
/// ("no len" — the Zig `MP_OBJ_NULL`). Under `STR_UNICODE=1` (full)
/// only bytes take the fast path (`isStrOrBytesForLen`); strings go
/// through the type's unary-op LEN slot, as do all other objects.
pub fn obj_len_maybe(ctx: &mut CallCtx, types: &BuiltinTypes, value: Value) -> Option<Value> {
    if is_bytes(ctx.arena, types, value) {
        if let Value::Heap(r) = value {
            if let Obj::Bytes(b) = ctx.arena.get(r) {
                // Copy the length out first: `b` borrows the arena
                // and `Value::int` needs `&mut` it.
                let n = b.bytes.len() as i64;
                return Some(Value::int(ctx.arena, Some(types.int), ctx.roots, n));
            }
        }
        return None;
    }
    let t = obj_get_type(ctx.arena, types, value);
    match call_unary(ctx, t, UnaryOp::Len, value) {
        Ok(SlotOutcome::Value(v)) => Some(v),
        _ => None,
    }
}

/// `mp_obj_len` (`obj.zig:441-445`).
pub fn obj_len(ctx: &mut CallCtx, types: &BuiltinTypes, value: Value) -> Result<Value, ObjError> {
    obj_len_maybe(ctx, types, value).ok_or(ObjError::NoLen)
}


// ── truth / callable / id ─────────────────────────────────────────

/// `mp_obj_is_true` (`obj.zig:468-490`): immediates first, then the
/// type's unary-op BOOL slot, then `len_maybe`, else true. (The Zig
/// `arg == MP_OBJ_NULL` / `pending_exc` defensive branches have no
/// Rust counterpart — `Value` is non-nullable; errors travel as
/// `Result`s, not a global.)
pub fn obj_is_true(ctx: &mut CallCtx, types: &BuiltinTypes, arg: Value) -> PyResult<bool> {
    match arg {
        Value::Bool(b) => return Ok(b),
        Value::None => return Ok(false),
        Value::SmallInt(n) => return Ok(n != 0),
        _ => {}
    }
    let t = obj_get_type(ctx.arena, types, arg);
    match call_unary(ctx, t, UnaryOp::Bool, arg) {
        Ok(SlotOutcome::Value(r)) => return Ok(r == Value::Bool(true)),
        Ok(SlotOutcome::NotImplemented) => {}
        Err(e) => return Err(ctx.record_typed(e)),
    }
    if let Some(len) = obj_len_maybe(ctx, types, arg) {
        // The Zig code compares the len Obj to `newSmallInt(0)` by
        // identity, so a heap-int 0 (a LEN slot that broke its
        // contract) is "true" here too — reproduced exactly.
        return Ok(len != Value::SmallInt(0));
    }
    Ok(true)
}

/// `mp_obj_is_callable` (`obj.zig:492-501` + `obj.c:200-205`):
/// a call slot means callable — EXCEPT when the slot is the
/// instance-call slot, in which case the instance is callable
/// only if a `__call__` lookup hits (`objtype.c:872-875`,
/// `mp_obj_instance_is_callable`). The instance branch runs
/// through the stub seams (same as `instance_call_slot`): a
/// plain `__call__` method in the class dict needs no call.
pub fn obj_is_callable(ctx: &mut CallCtx, value: Value) -> bool {
    let types = ctx.types;
    let Some(t) = obj_get_type(ctx.arena, types, value) else {
        return false;
    };
    let call = match ctx.arena.get(t) {
        Obj::Type(d) => d.call,
        _ => return false,
    };
    let Some(f) = call else {
        return false;
    };
    if std::ptr::fn_addr_eq(f, crate::objects::objtype::instance::INSTANCE_CALL) {
        return crate::objects::objtype::seams::with_stub_seams(|seams| {
            crate::objects::objtype::instance::instance_is_callable(ctx, value, seams)
        });
    }
    true
}

/// Is `value` the `NotImplemented` singleton? (D1: an
/// `Obj::Instance` with the shared ellipsis type as header —
/// the ellipsis type is the only type with name qstr 0 that
/// can head an empty instance; the cell type also has name 0
/// but is an `Obj::Cell`, not an `Instance`, and no user code
/// can instantiate the ellipsis type.) Used by
/// `instance_binary_op` (E2c): a dunder returning
/// `NotImplemented` means "try the other fallbacks".
pub fn is_not_implemented(ctx: &CallCtx, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    let Obj::Instance(inst) = ctx.arena.get(r) else {
        return false;
    };
    if inst.members.used != 0 || !inst.subobj.is_empty() {
        return false;
    }
    let Some(t) = ctx.arena.header(r).type_ else {
        return false;
    };
    matches!(ctx.arena.get(t), Obj::Type(d) if d.name == 0)
}

/// `mp_obj_id` (`obj.zig:503-516`): the identity integer. For
/// immediates this is the tagged word itself (`id(5) == 11`,
/// `id(True) == 30`, `id(None) == 6`, `id(0) == 1`, `id(-1) == -1`
/// — verified on the reference binary); for heap objects it is the
/// arena slot index (design doc D3: addresses are reused after free
/// in C too). All cases fit in a small int, so this never allocates.
pub fn obj_id(value: Value) -> Value {
    match value {
        Value::SmallInt(n) => Value::SmallInt((n << 1) | 1),
        Value::Bool(false) => Value::SmallInt(14),
        Value::Bool(true) => Value::SmallInt(30),
        Value::None => Value::SmallInt(6),
        Value::Qstr(id) => Value::SmallInt(((id as i64) << 3) | 2),
        Value::Heap(r) => Value::SmallInt(r.0 as i64),
    }
}

