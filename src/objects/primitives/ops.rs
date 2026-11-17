//! Primitives support — op enums, builtin-type refs, error type.
//!
//! The C6 lane's canonical types live in `crate::objects::slots`
//! (`UnaryOp` / `BinaryOp`) and `crate::objects::obj`
//! (`BuiltinTypes`, `ExcType`); C7's temporary mirrors of them were
//! deleted by the C7r rewire (the mirrors were verified verbatim
//! identical — same variant names, same discriminants, same fields).
//! Nothing here invents semantics: every value is traced to the
//! vendor header or the Zig source named in its doc.
//!
//! Error model (the C5 `SetupError` / C6 `ObjError` precedent): the
//! original raises by building an exception object into
//! `pyerr.pending_exc` and returning `error.PyRaise`. `VmState.pending_exc`
//! does not exist until C13/C14, so raises here are `PrimError`
//! values carrying exactly the data the original formats into the
//! exception — the exception type and the TERSE message literal
//! (ROM_LEVEL_MINIMUM reporting). The VM maps a `PrimError` to a
//! `pending_exc` object when it lands.
//!
//! Slot outcomes: a rich slot function returns `PrimResult` =
//! `Result<PrimOutcome, PrimError>` — `Ok(PrimOutcome::Value(v))` is
//! a handled op, `Ok(PrimOutcome::NotHandled)` is the Zig `NULL`
//! return ("not implemented", the caller falls through exactly as
//! the original's `NULL` checks do), `Err(e)` is a raise with its
//! data preserved. The thin adapters to the frozen slot signatures
//! (`slots.rs` `UnaryOpFn` / `BinaryOpFn`) live in `objint.rs` /
//! `objbool.rs` and read the type registry from `CallCtx.types`
//! (the C13 growth, done early by C7r); they convert with
//! `prim_to_pyresult`, which drops the raise data at the slot
//! boundary — the accepted C6 convention until C13 adds
//! `pending_exc`.

use crate::memory::gc::{GcArena, RootSet};
use crate::objects::obj::{BuiltinTypes, ExcType};
use crate::objects::value::{small_int_fits, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::vendor::mpz::BigInt;

// ── Op enums (verbatim mirrors of C6's `value.rs`) ─────────────────────

/// A raise signalled by a primitives function: exactly the data the
/// original formats into the exception object — the exception type
/// plus the TERSE message literal (ROM_LEVEL_MINIMUM reporting).
/// `message()` is `None` only for the bare `ValueError` (no message)
/// and for `OutOfMemory` (no exception object).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimError {
    /// `TypeError: can't convert to int`.
    IntConversion,
    /// `OverflowError: overflow converting long int to machine word`.
    OverflowWord,
    /// `ZeroDivisionError: divide by zero`.
    DivideByZero,
    /// `ValueError: negative shift count`.
    NegativeShift,
    /// `ValueError: invalid syntax for integer`.
    InvalidSyntaxForInt,
    /// `ValueError: int() arg 2 must be >= 2 and <= 36`.
    IntBaseRange,
    /// `ValueError: can't convert NaN to int`.
    NanToInt,
    /// `OverflowError: can't convert inf to int`.
    InfToInt,
    /// `OverflowError: buffer too small`.
    BufferTooSmall,
    /// Bare `ValueError` (no message) — `int.to_bytes` negative length.
    ValueErrorBare,
    /// `TypeError: unsupported type for operator`.
    UnsupportedOperator,
    /// `TypeError: pow() with 3 arguments requires integers`.
    Pow3RequiresInts,
    /// `ValueError: divide by zero` — `pow(x, y, 0)` raises
    /// ValueError, NOT ZeroDivisionError (verified on the binary).
    Pow3ModZero,
    /// `TypeError: object with buffer protocol required`.
    BufferProtocol,
    /// `TypeError: argument num/types mismatch`.
    ArgNumTypesMismatch,
    /// An allocation failed (carries `PyError::OutOfMemory` through).
    OutOfMemory,
}

impl PrimError {
    /// The exception type this error raises (`None` for
    /// `OutOfMemory`, which has no exception object).
    pub fn exc_type(self) -> Option<ExcType> {
        match self {
            PrimError::IntConversion
            | PrimError::UnsupportedOperator
            | PrimError::Pow3RequiresInts
            | PrimError::BufferProtocol
            | PrimError::ArgNumTypesMismatch => Some(ExcType::TypeError),
            PrimError::Pow3ModZero => Some(ExcType::ValueError),
            PrimError::InvalidSyntaxForInt
            | PrimError::NegativeShift
            | PrimError::IntBaseRange
            | PrimError::NanToInt
            | PrimError::ValueErrorBare => Some(ExcType::ValueError),
            PrimError::OverflowWord | PrimError::InfToInt | PrimError::BufferTooSmall => {
                Some(ExcType::OverflowError)
            }
            PrimError::DivideByZero => Some(ExcType::ZeroDivisionError),
            PrimError::OutOfMemory => None,
        }
    }

    /// The TERSE message literal (`None` for the bare `ValueError`
    /// and for `OutOfMemory`).
    pub fn message(self) -> Option<&'static str> {
        match self {
            PrimError::IntConversion => Some("can't convert to int"),
            PrimError::OverflowWord => Some("overflow converting long int to machine word"),
            PrimError::DivideByZero => Some("divide by zero"),
            PrimError::NegativeShift => Some("negative shift count"),
            PrimError::InvalidSyntaxForInt => Some("invalid syntax for integer"),
            PrimError::IntBaseRange => Some("int() arg 2 must be >= 2 and <= 36"),
            PrimError::NanToInt => Some("can't convert NaN to int"),
            PrimError::InfToInt => Some("can't convert inf to int"),
            PrimError::BufferTooSmall => Some("buffer too small"),
            PrimError::ValueErrorBare => None,
            PrimError::UnsupportedOperator => Some("unsupported type for operator"),
            PrimError::Pow3RequiresInts => Some("pow() with 3 arguments requires integers"),
            PrimError::Pow3ModZero => Some("divide by zero"),
            PrimError::BufferProtocol => Some("object with buffer protocol required"),
            PrimError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            PrimError::OutOfMemory => None,
        }
    }

    /// The plain `PyError` signal (`PyRaise` for a Python raise).
    pub fn to_pyerror(self) -> PyError {
        match self {
            PrimError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

/// What a rich slot function produced: a value, or "not handled"
/// (the Zig `NULL` return). Mirrors C6's `SlotOutcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimOutcome {
    Value(Value),
    NotHandled,
}

/// The rich slot-function result: a value, "not handled", or a raise
/// with its exception data preserved.
pub type PrimResult = Result<PrimOutcome, PrimError>;

/// Resolves a qstr id to its bytes. Static ids come from the
/// generated table; dynamic ids (>= `N_TOTAL`) live in the VM's
/// `RuntimeQstr` pool — the caller supplies the resolver (the C6
/// explicit-resolver precedent: `obj_print_exception` takes one
/// because dynamic names cannot be resolved here). Returns owned
/// bytes so the caller need not borrow the pool.
pub type QstrResolver<'a> = &'a dyn Fn(u32) -> Option<Vec<u8>>;

// ── qstr helpers ────────────────────────────────────────────────────────

/// Static qstr id for `bytes` (`None` if the string is not in the
/// static table). Linear scan of the generated table — bootstrap-only.
pub fn static_qstr_id(bytes: &[u8]) -> Option<u32> {
    crate::compiler::qstr::QSTR_STRINGS
        .iter()
        .position(|s| *s == bytes)
        .map(|i| i as u32)
}

// ── Value helpers ───────────────────────────────────────────────────────

/// The heap `BigInt` of an exact heap int, else `None`.
pub fn heap_int<'a>(arena: &'a GcArena, v: Value) -> Option<&'a BigInt> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => Some(z),
            _ => None,
        },
        _ => None,
    }
}

/// `mp_obj_is_int` (`py/obj.h:986`): a small int OR an exact int
/// type. Bool is NOT an int here (verified on the reference binary:
/// `isinstance(True, int)` is False).
pub fn is_exact_int(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::SmallInt(_))
        || matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.int))
}

/// Exact-type float (REPR_A: only heap-allocated).
pub fn is_float(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.float))
}

/// Exact-type complex.
pub fn is_complex(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.complex))
}

/// `mp_obj_is_str` (`obj.zig` isStr): qstr immediate OR exact-type str.
pub fn is_str(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Qstr(_))
        || matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.str_))
}

/// Exact-type bytes (`obj.zig` isBytes).
pub fn is_bytes(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.bytes))
}

/// Exact-type tuple.
pub fn is_tuple(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.tuple))
}

/// Exact-type list.
pub fn is_list(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Heap(r) if arena.header(r).type_ == Some(types.list))
}

/// The bytes of a str/bytes value: qstr via the resolver, heap
/// `Obj::Str` / `Obj::Bytes` payloads directly. `None` for anything
/// else (the caller decides what the original does with a NULL —
/// `int(x, base)` parses empty and raises `InvalidSyntaxForInt`,
/// `int(x)` raises `IntConversion`).
pub fn str_bytes_of(
    arena: &GcArena,
    _types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    v: Value,
) -> Option<Vec<u8>> {
    match v {
        Value::Qstr(id) => resolver(id),
        Value::Heap(r) => match arena.get(r) {
            Obj::Str(s) => Some(s.bytes.clone()),
            Obj::Bytes(b) => Some(b.bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Allocate an int value: small int in the 62-bit window, heap
/// `Obj::Int` outside it (mirrors `objint.c`'s small/mpz split).
pub fn new_int_from_i64(
    arena: &mut GcArena,
    types: &BuiltinTypes,
    roots: &RootSet,
    n: i64,
) -> Value {
    Value::int(arena, Some(types.int), roots, n)
}

/// Allocate an int value from a `BigInt`, typed by `type_` (the
/// `from_bytes` classmethod result is typed by its `cls`, which may
/// be an int subclass — not necessarily `types.int`).
pub fn new_int_from_bigint_typed(
    arena: &mut GcArena,
    type_: GcRef,
    roots: &RootSet,
    z: &BigInt,
) -> Value {
    if let Some(n) = z.as_int_checked() {
        if small_int_fits(n) {
            return Value::SmallInt(n);
        }
    }
    Value::Heap(arena.alloc(Some(type_), Obj::Int(z.clone()), roots))
}

/// Allocate an int value from a `BigInt`: small-ifies when it fits
/// the 62-bit window, else a heap `Obj::Int`.
pub fn new_int_from_bigint(
    arena: &mut GcArena,
    types: &BuiltinTypes,
    roots: &RootSet,
    z: &BigInt,
) -> Value {
    new_int_from_bigint_typed(arena, types.int, roots, z)
}

/// `mp_obj_new_int_from_float` (vendor `py/objint.c:143`, exact):
/// NaN → `ValueError`, Inf → `OverflowError`, else the exact
/// truncated value (small-ified when it fits).
pub fn int_from_float(
    arena: &mut GcArena,
    types: &BuiltinTypes,
    roots: &RootSet,
    f: f64,
) -> Result<Value, PrimError> {
    if f.is_nan() {
        return Err(PrimError::NanToInt);
    }
    if f.is_infinite() {
        return Err(PrimError::InfToInt);
    }
    Ok(new_int_from_bigint(arena, types, roots, &BigInt::from_float(f)))
}

/// `mp_obj_int_get_checked`: a small int, or a heap int that fits in
/// a machine word — else `OverflowError`. Bool counts as 0/1
/// (`mp_unary_op` INT_MAYBE on bool returns the small int).
pub fn arg_as_int(arena: &GcArena, types: &BuiltinTypes, v: Value) -> Result<i64, PrimError> {
    match v {
        Value::Bool(b) => Ok(if b { 1 } else { 0 }),
        Value::SmallInt(n) => Ok(n),
        Value::Heap(r) => {
            if arena.header(r).type_ == Some(types.int) {
                match arena.get(r) {
                    Obj::Int(z) => z.as_int_checked().ok_or(PrimError::OverflowWord),
                    _ => Err(PrimError::IntConversion),
                }
            } else {
                Err(PrimError::IntConversion)
            }
        }
        _ => Err(PrimError::IntConversion),
    }
}

/// `mp_obj_int_get_truncated`: a small int, or a heap int truncated
/// to its hash value (used for shift counts only after the checked
/// path rejects big values — kept for completeness).
pub fn arg_as_int_truncated(arena: &GcArena, types: &BuiltinTypes, v: Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(if b { 1 } else { 0 }),
        Value::SmallInt(n) => Some(n),
        Value::Heap(r) => {
            if arena.header(r).type_ == Some(types.int) {
                match arena.get(r) {
                    Obj::Int(z) => Some(z.hash_mpz()),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The 62-bit small-int window, re-exported for the int lane.
pub use crate::objects::value::{SMALL_INT_MAX, SMALL_INT_MIN};

/// Convert a `PrimError` into the frozen `PyResult` signal, dropping
/// the exception data (the C6 slot-boundary convention — the data is
/// preserved only on the rich `PrimResult` path).
pub fn prim_to_pyresult<T>(r: PrimResult) -> PyResult<T>
where
    T: From<Value>,
{
    match r {
        Ok(PrimOutcome::Value(v)) => Ok(T::from(v)),
        Ok(PrimOutcome::NotHandled) => Err(PyError::PyRaise),
        Err(e) => Err(e.to_pyerror()),
    }
}
