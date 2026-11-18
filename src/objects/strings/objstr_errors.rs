//! str / bytes error type (E2b split of `objstr_a.rs` — that
//! file was 806 lines, over the 800-line cap).
//!
//! `StrError` carries exactly the data the Zig formats into the
//! exception — the exception type plus the TERSE message literal
//! (see the `objstr_a.rs` header for the error model). Moved
//! verbatim; `objstr_a.rs` re-exports it, so existing imports
//! (`use crate::objects::strings::objstr_a::{StrError, ...}` and
//! `use ...::objstr_a::*`) keep working.

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::objects::obj::{BuiltinTypes, ObjError};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::PyError;
use crate::runtime::qstr::RuntimeQstr;

// ── errors ────────────────────────────────────────────────────────────

/// The exception type a `StrError` raises, by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrExc {
    TypeError,
    ValueError,
    IndexError,
    NotImplementedError,
    OverflowError,
    /// Added by the main agent at the C8a merge (C7r added this variant
    /// to C6's `ExcType`; the `StrError::Obj` mapping must cover it).
    ZeroDivisionError,
    /// Added by Lane C8b (format machinery raises `KeyError`).
    KeyError,
    /// Added by Lane C8b (`vstr` raises bare `RuntimeError` on a
    /// fixed buffer).
    RuntimeError,
}

impl StrExc {
    pub fn name(self) -> &'static str {
        match self {
            StrExc::TypeError => "TypeError",
            StrExc::ValueError => "ValueError",
            StrExc::IndexError => "IndexError",
            StrExc::NotImplementedError => "NotImplementedError",
            StrExc::OverflowError => "OverflowError",
            StrExc::ZeroDivisionError => "ZeroDivisionError",
            StrExc::KeyError => "KeyError",
            StrExc::RuntimeError => "RuntimeError",
        }
    }
}

/// A raise signalled by a str/bytes helper: exactly the data the Zig
/// inline exception constructors (`excBadImplicitConv` etc.,
/// `objstr.zig:70-121`) format into the exception object — the
/// exception type plus the TERSE message literal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrError {
    /// `TypeError: can't convert to str implicitly`.
    BadImplicitConversion,
    /// `ValueError: substring not found`.
    SubstrNotFound,
    /// `ValueError: bytes value out of range`.
    BytesValueOutOfRange,
    /// `TypeError: join expects a list of str/bytes objects consistent
    /// with self object`.
    JoinType,
    /// `ValueError: empty separator`.
    EmptySeparator,
    /// `ValueError: non-hex digit`.
    NonHexDigit,
    /// `NotImplementedError: rsplit(None,n)`.
    RsplitNone,
    /// `ValueError` with NO message (`excNegLen`).
    NegLen,
    /// `TypeError: wrong number of arguments`.
    WrongArgs,
    /// `ValueError: sep must be a single character`.
    FromHexSepLen,
    /// `TypeError: string indices must be integers`.
    StringIndicesMustBeInts,
    /// `IndexError: string index out of range`.
    StringIndexOutOfRange,
    /// `ValueError: slice step can't be zero`.
    SliceStepZero,
    /// `TypeError: object not iterable`.
    ObjectNotIterable,
    /// `TypeError: argument num/types mismatch` — the TERSE
    /// `mp_arg_error_terse_mismatch` (vendor `py/argcheck.c:141`),
    /// raised by `mp_arg_check_num_sig` and by the `KwPos` merge
    /// (unknown/duplicate keyword, missing required slot).
    ///
    /// NOTE (correction, 2026-11-17): this is NOT the same message
    /// as `WrongArgs`. The previous session's note conflated them;
    /// the reference binary distinguishes them — `str('a', foo=1)`
    /// and `str('a','b','c','d')` raise "argument num/types
    /// mismatch", while the explicit `raiseWrongArgs()` in
    /// `bytesMakeNewImpl` raises "wrong number of arguments"
    /// (`bytes('ab')`, `bytes(1, 2)`).
    ArgNumTypesMismatch,
    /// A C6 `ObjError` (int conversion, index out of range,
    /// buffer protocol, …) — same type/message, shared variant so
    /// callers of `obj.rs` helpers don't re-spell the message.
    Obj(ObjError),
    /// A C7 `PrimError` (float→int conversion in `arg_as_int`, …) —
    /// same type/message, shared variant so callers of the int lane
    /// don't re-spell the message. Added by Lane C8b.
    Prim(crate::objects::primitives::ops::PrimError),
    /// `TypeError: fillchar must be 1 character` (`str_center` /
    /// `strPadOneSide`, `objstr.zig:2285,2325`). Added by Lane C8b.
    FillcharMustBeOne,
    /// `ValueError: bad format string` (`excBadFormatStr`,
    /// `objstr.zig:97`). Added by Lane C8b.
    BadFormatStr,
    /// `TypeError: format needs a dict` (`excFormatNeedsDict`,
    /// `objstr.zig:100`). Added by Lane C8b.
    FormatNeedsDict,
    /// `TypeError: format string needs more arguments`
    /// (`excMoreArgs`, `objstr.zig:103`). Added by Lane C8b.
    MoreArgs,
    /// `TypeError: %c needs int or char` (`excPctC`,
    /// `objstr.zig:106`). Added by Lane C8b.
    PctC,
    /// `TypeError: integer needed` (`excIntegerNeeded`,
    /// `objstr.zig:109`). Added by Lane C8b.
    IntegerNeeded,
    /// `TypeError: format string didn't convert all arguments`
    /// (`excExtraArgs`, `objstr.zig:112`). Added by Lane C8b.
    ExtraArgs,
    /// `IndexError: tuple index out of range` (`excIndexOOR`,
    /// `objstr.zig:115`). Added by Lane C8b.
    TupleIndexOutOfRange,
    /// `KeyError: <key>` (`excKeyErr`, `objstr.zig:118-121`) — the
    /// missing key travels with the error (the pending cell only
    /// keeps type+message, so a slot path records `(KeyError, None)`
    /// there; the key itself is visible to direct callers of the
    /// method functions). Added by Lane C8b.
    KeyError(Value),
    /// `NotImplementedError: attributes not supported`
    /// (`excAttrsNotSupp`, `objstr.zig:122`). Added by Lane C8b.
    AttrsNotSupported,
    /// Bare `RuntimeError` (no message) — `vstr` extend on a fixed
    /// buffer (`vstr.zig`, `mp_obj_new_exception_msg(RuntimeError,
    /// null)`). Added by Lane C8b.
    RuntimeError,
    /// A slot raise whose exception object is already recorded
    /// elsewhere (the hook's `state.pending_exc` side channel,
    /// or `CallCtx.pending_raise`) — the `DictError::PyRaise`
    /// precedent. Carries no name/message, so `record_typed`
    /// records nothing and the stashed object drains instead.
    /// Added by E2b for the `iter_items` generator arm.
    PyRaise,
}

impl StrError {
    /// The exception type this error raises (`None` for
    /// `OutOfMemory`, which has no exception object).
    pub fn exc_type(self) -> Option<StrExc> {
        match self {
            StrError::BadImplicitConversion
            | StrError::JoinType
            | StrError::WrongArgs
            | StrError::ArgNumTypesMismatch
            | StrError::StringIndicesMustBeInts
            | StrError::ObjectNotIterable
            | StrError::FillcharMustBeOne
            | StrError::FormatNeedsDict
            | StrError::MoreArgs
            | StrError::PctC
            | StrError::IntegerNeeded
            | StrError::ExtraArgs => Some(StrExc::TypeError),
            StrError::SubstrNotFound
            | StrError::BytesValueOutOfRange
            | StrError::EmptySeparator
            | StrError::NonHexDigit
            | StrError::NegLen
            | StrError::FromHexSepLen
            | StrError::SliceStepZero
            | StrError::BadFormatStr => Some(StrExc::ValueError),
            StrError::StringIndexOutOfRange | StrError::TupleIndexOutOfRange => {
                Some(StrExc::IndexError)
            }
            StrError::RsplitNone | StrError::AttrsNotSupported => {
                Some(StrExc::NotImplementedError)
            }
            StrError::KeyError(_) => Some(StrExc::KeyError),
            StrError::RuntimeError => Some(StrExc::RuntimeError),
            // A stashed raise: no name, so `record_typed`
            // records nothing (the stashed object drains).
            StrError::PyRaise => None,
            StrError::Obj(e) => match e.exc_type() {
                Some(crate::objects::obj::ExcType::TypeError) => Some(StrExc::TypeError),
                Some(crate::objects::obj::ExcType::ValueError) => Some(StrExc::ValueError),
                Some(crate::objects::obj::ExcType::IndexError) => Some(StrExc::IndexError),
                Some(crate::objects::obj::ExcType::OverflowError) => {
                    Some(StrExc::OverflowError)
                }
                Some(crate::objects::obj::ExcType::ZeroDivisionError) => {
                    Some(StrExc::ZeroDivisionError)
                }
                Some(crate::objects::obj::ExcType::NotImplementedError) => {
                    Some(StrExc::NotImplementedError)
                }
                None => None,
            },
            StrError::Prim(e) => match e.exc_type() {
                Some(crate::objects::obj::ExcType::TypeError) => Some(StrExc::TypeError),
                Some(crate::objects::obj::ExcType::ValueError) => Some(StrExc::ValueError),
                Some(crate::objects::obj::ExcType::IndexError) => Some(StrExc::IndexError),
                Some(crate::objects::obj::ExcType::OverflowError) => {
                    Some(StrExc::OverflowError)
                }
                Some(crate::objects::obj::ExcType::ZeroDivisionError) => {
                    Some(StrExc::ZeroDivisionError)
                }
                Some(crate::objects::obj::ExcType::NotImplementedError) => {
                    Some(StrExc::NotImplementedError)
                }
                None => None,
            },
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` and for
    /// `NegLen`, which carries no message).
    pub fn message(self) -> Option<&'static str> {
        match self {
            StrError::BadImplicitConversion => Some("can't convert to str implicitly"),
            StrError::SubstrNotFound => Some("substring not found"),
            StrError::BytesValueOutOfRange => Some("bytes value out of range"),
            StrError::JoinType => Some(
                "join expects a list of str/bytes objects consistent with self object",
            ),
            StrError::EmptySeparator => Some("empty separator"),
            StrError::NonHexDigit => Some("non-hex digit"),
            StrError::RsplitNone => Some("rsplit(None,n)"),
            StrError::NegLen => None,
            StrError::WrongArgs => Some("wrong number of arguments"),
            StrError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            StrError::FromHexSepLen => Some("sep must be a single character"),
            StrError::StringIndicesMustBeInts => Some("string indices must be integers"),
            StrError::StringIndexOutOfRange => Some("string index out of range"),
            StrError::SliceStepZero => Some("slice step can't be zero"),
            StrError::ObjectNotIterable => Some("object not iterable"),
            StrError::FillcharMustBeOne => Some("fillchar must be 1 character"),
            StrError::BadFormatStr => Some("bad format string"),
            StrError::FormatNeedsDict => Some("format needs a dict"),
            StrError::MoreArgs => Some("format string needs more arguments"),
            StrError::PctC => Some("%c needs int or char"),
            StrError::IntegerNeeded => Some("integer needed"),
            StrError::ExtraArgs => Some("format string didn't convert all arguments"),
            StrError::TupleIndexOutOfRange => Some("tuple index out of range"),
            // The KeyError message is the missing key's repr — dynamic,
            // so there is no static literal (the key travels on the
            // `KeyError(Value)` variant itself).
            StrError::KeyError(_) => None,
            StrError::AttrsNotSupported => Some("attributes not supported"),
            StrError::RuntimeError => None,
            StrError::PyRaise => None,
            StrError::Prim(e) => e.message(),
            StrError::Obj(e) => e.message(),
        }
    }

    /// The plain `PyError` signal (`PyRaise` for a Python raise).
    pub fn to_pyerror(self) -> PyError {
        match self {
            StrError::Obj(e) => e.to_pyerror(),
            StrError::Prim(e) => e.to_pyerror(),
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, arg/message). A `KeyError` records its key
/// as the value argument (the old `PENDING_STR_ERROR` cell
/// dropped it).
impl TypedRaise for StrError {
    fn raise_name(&self) -> &'static str {
        self.exc_type().map(|t| t.name()).unwrap_or("")
    }
    fn raise_arg(&self) -> Option<Value> {
        match self {
            StrError::KeyError(v) => Some(*v),
            _ => None,
        }
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.message().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.to_pyerror()
    }
}

impl From<ObjError> for StrError {
    fn from(e: ObjError) -> StrError {
        StrError::Obj(e)
    }
}

// ── pending error (recorded on the `CallCtx`) ──────────────────────

// The pending str/bytes raise, recorded on the `CallCtx`
// (`PendingRaise`, `memory::gc`) — C13's replacement for C8a's
// `PENDING_STR_ERROR` thread-local cell (same data, now on the
// context: the C7r/C9a explicit-parameter precedent — shared
// state rides on the context, not on globals). C6's `call_*`
// wrappers collapse every slot `PyRaise` into `NotImplemented`,
// so a slot that raises a SPECIFIC error (e.g. `IndexError:
// string index out of range` from `str_subscr`) records it via
// `slot_raise` before returning `PyRaise`; a slot that returns
// NULL in the original records nothing, so "no pending raise"
// still means "not implemented". The VM drains the recording
// after the slot returns and builds the exception object into
// `VmState.pending_exc`. Tests read it via the fixture's
// `pending_raise` field to assert the exact (type, message) a
// slot raises.

/// Record `e` as the pending str/bytes raise and return its plain
/// `PyError` signal — the slot functions' raise path. This is the
/// `TypedRaise` recording shared by every error type (see
/// `CallCtx::record_typed`); the name is kept so the existing call
/// sites read the same.
pub(crate) fn slot_raise(ctx: &mut CallCtx, e: StrError) -> PyError {
    ctx.record_typed(e)
}

/// The result type for a method/slot on a value: the header type
/// when known, else the payload kind's builtin type (bootstrapping
/// fallback — real str/bytes values always carry a header type).
pub(crate) fn result_type(types: &BuiltinTypes, value_type: Option<GcRef>, is_str: bool) -> GcRef {
    value_type.unwrap_or(if is_str { types.str_ } else { types.bytes })
}
