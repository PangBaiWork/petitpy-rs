//! Error type for the objtype lane (`ObjError` pattern, `obj.rs`).
//!
//! Each variant is exactly the data one Zig raise helper formats
//! into the exception object: the exception type plus the TERSE
//! message literal (ROM_LEVEL_MINIMUM reporting). `MroConflict`
//! carries the base name qstr ids — the Zig formats them into the
//! message (`"Cannot create a consistent method resolution order
//! (MRO) for bases X, Y"`); `message()` returns the static prefix
//! and the test asserts the names from the payload.

use crate::memory::gc::TypedRaise;
use crate::objects::obj::ExcType;
use crate::objects::value::Value;
use crate::pyerr::PyError;
use crate::runtime::qstr::RuntimeQstr;

/// A raise signalled by an objtype helper.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ObjTypeError {
    /// `TypeError: type takes 1 or 3 arguments` (`type_make_new`).
    TypeTakesOneOrThree,
    /// `TypeError: can't create instance` (`type_call` on a type
    /// without a make_new slot).
    CantCreateInstance,
    /// `TypeError: type isn't an acceptable base type` (a base
    /// without a make_new slot).
    NotAcceptableBase,
    /// `NotImplementedError: multiple inheritance not supported`
    /// (`mp_obj_new_type`, nano-gated in the Zig — unreachable in
    /// the full config, kept for faithfulness).
    MultipleInheritance,
    /// `TypeError: multiple bases have instance lay-out conflict`.
    LayoutConflict,
    /// `TypeError: too many base classes` (C3, more than 15 bases).
    TooManyBases,
    /// `TypeError: Cannot create a consistent method resolution
    /// order (MRO) for bases ...` — payload is the base name qstr
    /// ids, in order.
    MroConflict(Vec<u32>),
    /// `TypeError: __init__() should return None`.
    InitMustReturnNone,
    /// `TypeError: object not callable` (`instance_call`).
    ObjectNotCallable,
    /// `TypeError: issubclass() arg 1 must be a class`.
    IssubclassArg1,
    /// `TypeError: issubclass() arg 2 must be a class or a tuple
    /// of classes`.
    IssubclassArg2,
    /// `TypeError: arg must be user-type` (`object.__new__` on a
    /// non-user type).
    MustBeUserType,
    /// Bare `TypeError` with no message (the Zig's
    /// `raiseTypeErrorNull` — e.g. `int.__new__` checks and
    /// `super()` validation).
    BareTypeError,
    /// The terse argument-count error (`mp_arg_check_num_sig`,
    /// polled): `TypeError: argument num/types mismatch`.
    ArgNumTypesMismatch,
    /// `TypeError: can't convert to str implicitly` — a non-str
    /// value where `mp_obj_str_get_qstr` is required (`type()` with
    /// a non-str name).
    NotStr,
    /// A callee raised (e.g. a `__set_name__` method): the exception
    /// object travels via `pending_exc` (the C6 convention — the
    /// raise data is dropped at the boundary until C13), so the
    /// only signal here is "a raise happened". Maps to
    /// `PyError::PyRaise`.
    Raised,
    /// An allocation failed.
    OutOfMemory,
}

impl ObjTypeError {
    /// The exception type this error raises (`None` for
    /// `OutOfMemory`, which has no exception object).
    pub fn exc_type(self) -> Option<ExcType> {
        match self {
            ObjTypeError::MultipleInheritance => Some(ExcType::NotImplementedError),
            ObjTypeError::OutOfMemory | ObjTypeError::Raised => None,
            _ => Some(ExcType::TypeError),
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory`).
    /// `MroConflict` returns the static prefix; the base names
    /// live in the payload.
    pub fn message(&self) -> Option<&'static str> {
        match self {
            ObjTypeError::TypeTakesOneOrThree => Some("type takes 1 or 3 arguments"),
            ObjTypeError::CantCreateInstance => Some("can't create instance"),
            ObjTypeError::NotAcceptableBase => Some("type isn't an acceptable base type"),
            ObjTypeError::MultipleInheritance => {
                Some("multiple inheritance not supported")
            }
            ObjTypeError::LayoutConflict => {
                Some("multiple bases have instance lay-out conflict")
            }
            ObjTypeError::TooManyBases => Some("too many base classes"),
            ObjTypeError::MroConflict(_) => Some(
                "Cannot create a consistent method resolution order (MRO) for bases",
            ),
            ObjTypeError::InitMustReturnNone => Some("__init__() should return None"),
            ObjTypeError::ObjectNotCallable => Some("object not callable"),
            ObjTypeError::IssubclassArg1 => Some("issubclass() arg 1 must be a class"),
            ObjTypeError::IssubclassArg2 => {
                Some("issubclass() arg 2 must be a class or a tuple of classes")
            }
            ObjTypeError::MustBeUserType => Some("arg must be user-type"),
            ObjTypeError::BareTypeError => None,
            ObjTypeError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            ObjTypeError::NotStr => Some("can't convert to str implicitly"),
            ObjTypeError::Raised => None,
            ObjTypeError::OutOfMemory => None,
        }
    }

    /// The plain `PyError` signal (`PyRaise` for a Python raise).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ObjTypeError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl TypedRaise for ObjTypeError {
    /// The exception name (`""` for `OutOfMemory` / `Raised` —
    /// `record_typed` records nothing for an empty name: an OOM
    /// has no exception object, and `Raised` means the object
    /// already travels on the `pending_exc` side channel).
    fn raise_name(&self) -> &'static str {
        match self.clone().exc_type() {
            Some(t) => t.name(),
            None => "",
        }
    }

    fn raise_arg(&self) -> Option<Value> {
        None
    }

    /// The TERSE message. `MroConflict` formats the base names
    /// into the message, exactly as the Zig does
    /// (`objtype.zig:1025-1030`: the prefix, then `" A"` for the
    /// first base and `", B"` for each later one).
    fn raise_message(&self, pool: &RuntimeQstr) -> Option<String> {
        match self {
            ObjTypeError::MroConflict(names) => {
                let mut msg = String::from(
                    "Cannot create a consistent method resolution order (MRO) for bases",
                );
                for (i, n) in names.iter().enumerate() {
                    if i > 0 {
                        msg.push(',');
                    }
                    msg.push(' ');
                    msg.push_str(&String::from_utf8_lossy(pool.qstr_str(*n as u16)));
                }
                Some(msg)
            }
            _ => self.message().map(|m| m.to_string()),
        }
    }

    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
    }
}

/// `mp_arg_check_num_sig` (`runtime/argcheck.zig:100`), polled-only:
/// returns `Some(ArgNumTypesMismatch)` when the count is out of
/// range, else `None` — the caller records it and keeps going (a
/// later error overwrites it; the recorded one surfaces only if
/// nothing else raises). Same model as C7's `tuple_make_new` sig
/// handling. `takes_kw` is the sig's kw bit.
pub fn arg_check_num_sig(
    n_args: usize,
    n_kw: usize,
    min: usize,
    max: usize,
    takes_kw: bool,
) -> Option<ObjTypeError> {
    if n_args < min || n_args > max || (!takes_kw && n_kw > 0) {
        Some(ObjTypeError::ArgNumTypesMismatch)
    } else {
        None
    }
}
