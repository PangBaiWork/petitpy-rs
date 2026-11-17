//! str / bytes core — prelude + constructors + index/slice helpers +
//! iterators (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 1–2080, first half:
//! the prelude (exception constructors, small helpers), `find_subbytes`,
//! the `str_index_to_ptr` full arm, the slice helpers (`MpBoundSlice`,
//! `sliceCount`), and the str/bytes iterators. The method bodies live
//! in `objstr_methods_a.rs` / `objstr_methods_b.rs` and build on this
//! file — the shared prelude is imported, never copied (C8b imports
//! from here too).
//!
//! Error model: like C6's `ObjError`, raises here are `StrError`
//! values carrying exactly the data the Zig formats into the
//! exception — the exception type plus the TERSE message literal.
//! Method functions return `StrError` directly. Slot functions
//! (frozen `PyResult<Value>` signatures) record a specific raise
//! in the `PENDING_STR_ERROR` cell (`pyerr.pending_exc` stand-in,
//! see below) before returning `PyError::PyRaise`, and record
//! nothing when the original returns NULL — so "no pending error"
//! still means "not implemented" for C6's `call_*` wrappers, until
//! C13 teaches them to check the pending cell (design doc D7).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{
    obj_get_int, obj_get_int_maybe, obj_get_type, qstr_bytes, BuiltinTypes, ObjError,
    BUFFER_READ,
};
use crate::objects::obj_ops::{get_buffer, get_index};
use crate::objects::slots::BufferInfo;
use crate::objects::strings::unicode::{is_utf8_cont, utf8_next_char};
use crate::objects::value::{
    BytesData, GcRef, Obj, SliceData, StrData, Value,
};
use crate::pyerr::PyError;

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
            | StrError::ObjectNotIterable => Some(StrExc::TypeError),
            StrError::SubstrNotFound
            | StrError::BytesValueOutOfRange
            | StrError::EmptySeparator
            | StrError::NonHexDigit
            | StrError::NegLen
            | StrError::FromHexSepLen
            | StrError::SliceStepZero => Some(StrExc::ValueError),
            StrError::StringIndexOutOfRange => Some(StrExc::IndexError),
            StrError::RsplitNone => Some(StrExc::NotImplementedError),
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
            StrError::Obj(e) => e.message(),
        }
    }

    /// The plain `PyError` signal (`PyRaise` for a Python raise).
    pub fn to_pyerror(self) -> PyError {
        match self {
            StrError::Obj(e) => e.to_pyerror(),
            _ => PyError::PyRaise,
        }
    }
}

impl From<ObjError> for StrError {
    fn from(e: ObjError) -> StrError {
        StrError::Obj(e)
    }
}

// ── pending error (thread-local) ────────────────────────────────────────

// The pending str/bytes raise, as `(exception type, TERSE message)`.
//
// Stand-in for Zig's `pyerr.pending_exc` global until C13 lands
// `VmState.pending_exc` (design doc D7). C6's `call_*` wrappers
// collapse every slot `PyRaise` into `NotImplemented`, so a slot
// that raises a SPECIFIC error (e.g. `IndexError: string index out
// of range` from `str_subscr`) records it here before returning
// `PyRaise`; a slot that returns NULL in the original records
// nothing, so "no pending error" still means "not implemented".
// C13 must teach the wrappers to check this cell (or the
// `CallCtx` field that replaces it) before collapsing to
// `NotImplemented`. Tests read it via `take_pending_str_error`
// to assert the exact (type, message) a slot raises.
// (A `///` doc comment on the `thread_local!` block itself is an
// "unused doc comment" — the docs live on this comment instead.)
thread_local! {
    static PENDING_STR_ERROR: std::cell::Cell<Option<(StrExc, Option<&'static str>)>> =
        const { std::cell::Cell::new(None) };
}

/// Record `e` as the pending str/bytes raise and return its plain
/// `PyError` signal — the slot functions' raise path. (`OutOfMemory`
/// sets no pending error: it has no exception object.)
pub(crate) fn slot_raise(e: StrError) -> PyError {
    let sig = e.to_pyerror();
    if sig == PyError::PyRaise {
        PENDING_STR_ERROR.with(|c| {
            c.set(Some((
                e.exc_type().unwrap_or(StrExc::TypeError),
                e.message(),
            )))
        });
    }
    sig
}

/// Take (and clear) the pending str/bytes raise, if any.
pub fn take_pending_str_error() -> Option<(StrExc, Option<&'static str>)> {
    PENDING_STR_ERROR.with(|c| c.replace(None))
}

/// Clear the pending str/bytes raise (test fixtures call this before
/// each slot call so a stale raise cannot leak into the next test).
pub fn clear_pending_str_error() {
    PENDING_STR_ERROR.with(|c| c.set(None));
}

/// The result type for a method/slot on a value: the header type
/// when known, else the payload kind's builtin type (bootstrapping
/// fallback — real str/bytes values always carry a header type).
pub(crate) fn result_type(types: &BuiltinTypes, value_type: Option<GcRef>, is_str: bool) -> GcRef {
    value_type.unwrap_or(if is_str { types.str_ } else { types.bytes })
}

// ── data access ───────────────────────────────────────────────────────

/// The bytes of a str/bytes value (`npyStrObjStrDataLen` /
/// `strDataLen`, `objstr.zig:246-260,3145-3153`): a qstr immediate
/// reads the static table; a heap `Str`/`Bytes` payload reads its
/// bytes. `None` for anything else (the Zig reads the `MpObjStr`
/// layout unconditionally — UB on a non-str; Rust refuses instead).
pub fn str_data<'a>(arena: &'a GcArena, v: Value) -> Option<&'a [u8]> {
    match v {
        Value::Qstr(id) => qstr_bytes(id),
        Value::Heap(r) => match arena.get(r) {
            Obj::Str(s) => Some(&s.bytes),
            Obj::Bytes(b) => Some(&b.bytes),
            _ => None,
        },
        _ => None,
    }
}

/// Read an object's bytes through the buffer protocol
/// (`npyStrGetBufferRaise` / `mp_get_buffer` with `MP_BUFFER_READ`):
/// str/bytes payloads read via `str_data`; anything else probes
/// `get_buffer` — no buffer → `BufferProtocol` ("object with buffer
/// protocol required"). A buffer on a non-str/bytes object has no
/// safe reader yet (this lane is `unsafe`-free and the raw pointer
/// in `BufferInfo` can only be read with `unsafe` — C9 must add a
/// safe copy when array/bytearray land, see the result file), so it
/// reports the same error. Unreachable today: str/bytes are the
/// only buffer providers.
pub fn buffer_data(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    v: Value,
) -> Result<Vec<u8>, StrError> {
    if let Some(d) = str_data(ctx.arena, v) {
        return Ok(d.to_vec());
    }
    let mut info = BufferInfo::empty();
    match get_buffer(ctx, types, v, &mut info, BUFFER_READ) {
        // TODO(C9): `Ok(true)` means the object HAS a buffer but
        // there is no safe reader for it yet — see above.
        Ok(_) => Err(StrError::Obj(ObjError::BufferProtocol)),
        Err(e) => Err(StrError::Obj(e)),
    }
}

/// The items of a value, for the iterable paths that predate C11's
/// iterator protocol (`str.join`, `bytes(iterable)`): a list/tuple
/// yields its items; a str yields its 1-char strings; a bytes
/// yields its byte ints; a dict yields its keys; a set yields its
/// items (payload order — set order is unspecified); a range yields
/// its ints. Anything else cannot be iterated without C9's `list()`
/// + C11's iterator protocol, so it reports "object not iterable"
/// — a documented gap for values no lane can build yet (generators,
/// user iterables).
pub fn iter_items(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<Vec<Value>, StrError> {
    // First extract owned data (the arena borrow must end before
    // any allocation below — `new_str_via_qstr` / `Value::int`
    // take `&mut CallCtx`).
    enum JoinSource {
        Items(Vec<Value>),
        StrData(Vec<u8>, bool),
        Range(i64, i64, i64),
    }
    let src = match arg {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => JoinSource::Items(l.items.clone()),
            Obj::Tuple(t) => JoinSource::Items(t.clone()),
            Obj::Str(_) | Obj::Bytes(_) => JoinSource::StrData(
                str_data(ctx.arena, arg).unwrap_or(&[]).to_vec(),
                is_str_value(ctx.arena, arg),
            ),
            Obj::Dict(d) => JoinSource::Items(d.map.entries.iter().map(|(k, _)| *k).collect()),
            Obj::Set(s) => JoinSource::Items(s.items.clone()),
            Obj::Range(rg) => JoinSource::Range(rg.start, rg.stop, rg.step),
            _ => return Err(StrError::ObjectNotIterable),
        },
        Value::Qstr(id) => JoinSource::StrData(qstr_bytes(id).unwrap_or(&[]).to_vec(), true),
        _ => return Err(StrError::ObjectNotIterable),
    };
    match src {
        JoinSource::Items(v) => Ok(v),
        JoinSource::StrData(data, is_str) => {
            if is_str {
                let mut v = Vec::new();
                let mut i = 0;
                while i < data.len() {
                    let end = utf8_next_char(&data, i);
                    v.push(new_str_via_qstr(ctx, types, &data[i..end]));
                    i = end;
                }
                Ok(v)
            } else {
                Ok(data.iter().map(|b| Value::SmallInt(*b as i64)).collect())
            }
        }
        JoinSource::Range(start, stop, step) => {
            let mut v = Vec::new();
            let mut i = start;
            if step > 0 {
                while i < stop {
                    v.push(Value::int(ctx.arena, Some(types.int), ctx.roots, i));
                    i += step;
                }
            } else if step < 0 {
                while i > stop {
                    v.push(Value::int(ctx.arena, Some(types.int), ctx.roots, i));
                    i += step;
                }
            }
            Ok(v)
        }
    }
}

/// Whether `v` holds str bytes by payload (qstr immediate or heap
/// `Str`). Payload truth, not a header-type check — the header type
/// is a type-identity question (`str_check_arg_type`), this one is
/// "what bytes does this value carry".
pub fn is_str_value(arena: &GcArena, v: Value) -> bool {
    match v {
        Value::Qstr(_) => true,
        Value::Heap(r) => matches!(arena.get(r), Obj::Str(_)),
        _ => false,
    }
}

/// Whether `v` holds bytes by payload (heap `Bytes`).
pub fn is_bytes_value(arena: &GcArena, v: Value) -> bool {
    matches!(v, Value::Heap(r) if matches!(arena.get(r), Obj::Bytes(_)))
}

/// Whether `v` is exactly of the str type (header type == `str_`),
/// qstr immediates included — the `objIsStr` half of
/// `str_check_arg_type`'s callers.
pub fn is_str_type(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    obj_get_type(arena, types, v) == Some(types.str_)
}

/// Whether `v` is exactly of the bytes type (header type == `bytes`).
pub fn is_bytes_type(arena: &GcArena, types: &BuiltinTypes, v: Value) -> bool {
    obj_get_type(arena, types, v) == Some(types.bytes)
}

/// `str_check_arg_type` (`objstr.zig:664-680`): the argument's type
/// must equal `self_type` (bytes ↔ bytearray are normalised to each
/// other in the Zig; no bytearray type exists in Rust yet — C9 — so
/// the normalisation is a no-op today and is recorded here, not
/// silently dropped). Mismatch raises "can't convert to str
/// implicitly".
pub fn str_check_arg_type(
    arena: &GcArena,
    types: &BuiltinTypes,
    self_type: Option<GcRef>,
    arg: Value,
) -> Result<(), StrError> {
    // No bytearray type exists yet (C9); the Zig's bytes↔bytearray
    // normalisation has nothing to normalise.
    let arg_t = obj_get_type(arena, types, arg);
    if arg_t != self_type {
        return Err(StrError::BadImplicitConversion);
    }
    Ok(())
}

// ── qstr lookup / constructors ────────────────────────────────────────

/// Static qstr id for `name` (`None` if the name is not a static
/// qstr). Used for type names (`"str"`, `"bytes"`) — not for string
/// content (that is `find_static_qstr`, which skips id 0).
pub fn static_qstr_id(name: &[u8]) -> Option<u32> {
    crate::qstr_table::QSTR_STRINGS
        .iter()
        .position(|s| *s == name)
        .map(|i| i as u32)
}

/// `qstr_find_strn` (`objstr.zig:3068`, vendor): the static qstr id
/// holding `data`, or `None` when absent. The scan starts at id 1:
/// id 0 is `MP_QSTRnull` (the "not found" sentinel) and also holds
/// `""`, while id 1 is `MP_QSTR_` (the empty-string qstr) — so
/// `find_static_qstr(b"")` is `Some(1)`, never `Some(0)`.
pub fn find_static_qstr(data: &[u8]) -> Option<u32> {
    crate::qstr_table::QSTR_STRINGS
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, s)| **s == data)
        .map(|(i, _)| i as u32)
}

/// `mp_obj_new_str` (`objstr.zig:3153-3159`): static-qstr interning —
/// a static qstr when the content matches one, else a fresh heap
/// str. (The data is copied: the Zig shares the source pointer for
/// `makeNewDefaultBytes`, but strings are immutable, so sharing is
/// unobservable.)
pub fn new_str(ctx: &mut CallCtx, types: &BuiltinTypes, data: &[u8]) -> Value {
    if let Some(id) = find_static_qstr(data) {
        return Value::Qstr(id);
    }
    Value::Heap(ctx.alloc(
        Some(types.str_),
        Obj::Str(StrData {
            bytes: data.to_vec(),
        }),
    ))
}

/// `mp_obj_new_str_of_type` (`objstr.zig:3169-3177`): `str` type →
/// `new_str` (static interning); any other type today is `bytes` →
/// fresh bytes, never interned. (`bytearray` lands with C9.)
pub fn new_str_of_type(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    data: &[u8],
) -> Value {
    if type_ == types.str_ {
        new_str(ctx, types, data)
    } else {
        Value::Heap(ctx.alloc(
            Some(type_),
            Obj::Bytes(BytesData {
                bytes: data.to_vec(),
            }),
        ))
    }
}

/// `mp_obj_new_str_via_qstr` (`objstr.zig:3096-3098`):
/// `qstr_from_strn` — interning that CREATES a dynamic qstr when
/// the content is new. Without the VM's `RuntimeQstr` pool (C13)
/// only static ids can be returned; non-static content becomes a
/// fresh heap str instead. Observable difference until C13:
/// `s[0] is s[0]` for a 1-char string that is not a static qstr
/// (both sides are distinct heap objects here, one dynamic qstr
/// there).
pub fn new_str_via_qstr(ctx: &mut CallCtx, types: &BuiltinTypes, data: &[u8]) -> Value {
    if let Some(id) = find_static_qstr(data) {
        return Value::Qstr(id);
    }
    Value::Heap(ctx.alloc(
        Some(types.str_),
        Obj::Str(StrData {
            bytes: data.to_vec(),
        }),
    ))
}

/// `mp_obj_new_bytes` (`objstr.zig:3100-3102`): fresh bytes, never
/// interned.
pub fn new_bytes(ctx: &mut CallCtx, types: &BuiltinTypes, data: &[u8]) -> Value {
    Value::Heap(ctx.alloc(
        Some(types.bytes),
        Obj::Bytes(BytesData {
            bytes: data.to_vec(),
        }),
    ))
}

/// The empty str (`newQstrObj(mp.MP_QSTR_)`).
pub fn new_empty_str() -> Value {
    Value::Qstr(1)
}

/// The empty bytes object. The original returns the ROM singleton
/// `mp_const_empty_bytes_obj` (so `b'' is b''`); Rust allocates a
/// fresh empty `BytesData` per call until a shared ROM object exists
/// (C12) — identity differs, content does not.
pub fn new_empty_bytes(ctx: &mut CallCtx, types: &BuiltinTypes) -> Value {
    new_bytes(ctx, types, &[])
}

// ── find_subbytes ─────────────────────────────────────────────────────

/// `find_subbytes` (`objstr.zig:514-533`): like `strstr` with
/// explicit lengths (NUL bytes allowed). `direction > 0` = forward
/// (first occurrence), `< 0` = backward (last occurrence). Returns
/// the byte offset, or `None`.
pub fn find_subbytes(haystack: &[u8], needle: &[u8], direction: i32) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    if direction > 0 {
        let end = haystack.len() - needle.len();
        let mut idx = 0;
        loop {
            if haystack[idx..idx + needle.len()] == needle[..] {
                return Some(idx);
            }
            if idx == end {
                break;
            }
            idx += 1;
        }
    } else {
        let mut idx = haystack.len() - needle.len();
        loop {
            if haystack[idx..idx + needle.len()] == needle[..] {
                return Some(idx);
            }
            if idx == 0 {
                break;
            }
            idx -= 1;
        }
    }
    None
}

// ── str_index_to_ptr (full arm) ───────────────────────────────────────

/// `str_index_to_ptr` (`objstr.zig:971-1035`, the full arm exported
/// as `str_index_to_ptr` under `STR_UNICODE=1`): resolve `index`
/// against `data` and return the BYTE offset. Bytes index by byte
/// (via `mp_get_index` — C6's `get_index`, messages "index out of
/// range" / "indices must be integers"); str indexes by code point
/// (UTF-8 walk, messages "string index out of range" / "string
/// indices must be integers"). `is_slice` clamps instead of raising.
pub fn str_index_to_ptr(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    data: &[u8],
    is_bytes: bool,
    index: Value,
    is_slice: bool,
) -> Result<usize, StrError> {
    if is_bytes {
        return Ok(get_index(ctx, types, data.len(), index, is_slice)?);
    }
    // str (UTF-8): walk code points.
    let mut i: i64 = match index {
        Value::SmallInt(n) => n,
        _ => match obj_get_int_maybe(ctx, types, index) {
            Ok(Some(n)) => n,
            _ => return Err(StrError::StringIndicesMustBeInts),
        },
    };
    let len = data.len();
    if i < 0 {
        // Vendor: `for (s = top - 1; i; --s)` with `++i` on a
        // non-continuation byte. `s` is a signed byte index so it
        // can run below the start and still be detected.
        let mut s: i64 = len as i64 - 1;
        while i != 0 {
            if s < 0 {
                if is_slice {
                    return Ok(0);
                }
                return Err(StrError::StringIndexOutOfRange);
            }
            if !is_utf8_cont(data[s as usize]) {
                i += 1;
            }
            s -= 1;
        }
        Ok((s + 1) as usize)
    } else {
        let mut s: usize = 0;
        loop {
            if s >= len {
                if is_slice {
                    return Ok(len);
                }
                return Err(StrError::StringIndexOutOfRange);
            }
            if i == 0 {
                return Ok(s);
            }
            i -= 1;
            s = utf8_next_char(data, s);
        }
    }
}

// ── slice helpers ─────────────────────────────────────────────────────

/// `mp_bound_slice_t` (vendor `py/obj.h`): resolved slice bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundSlice {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

/// `mp_obj_slice_indices` (vendor `py/objslice.c:129-187`): resolve
/// a slice against a sequence of `length`, resolving missing
/// components, negatives and off-the-end values. `step == 0` raises
/// `ValueError: slice step can't be zero`; a non-int component
/// raises `TypeError: can't convert to int` (via `obj_get_int`).
pub fn slice_indices(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    slice: &SliceData,
    length: i64,
) -> Result<BoundSlice, StrError> {
    let step: i64 = match slice.step {
        None => 1,
        Some(v) => {
            let n = obj_get_int(ctx, types, v)?;
            if n == 0 {
                return Err(StrError::SliceStepZero);
            }
            n
        }
    };
    let mut bound = |v: Option<Value>| -> Result<Option<i64>, StrError> {
        match v {
            None => Ok(None),
            Some(v) => Ok(Some(obj_get_int(ctx, types, v)?)),
        }
    };
    if step > 0 {
        let start = match bound(slice.start)? {
            None => 0,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        let stop = match bound(slice.stop)? {
            None => length,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(0, length)
            }
        };
        Ok(BoundSlice { start, stop, step })
    } else {
        let start = match bound(slice.start)? {
            None => length - 1,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        let stop = match bound(slice.stop)? {
            None => -1,
            Some(mut s) => {
                if s < 0 {
                    s += length;
                }
                s.clamp(-1, length - 1)
            }
        };
        Ok(BoundSlice { start, stop, step })
    }
}

/// `mp_seq_get_fast_slice_indexes` (vendor `py/sequence.c:48-64`):
/// `slice_indices` plus the fast-path adjustments (negative step:
/// `stop += 1`; positive step with `start > stop`: `stop = start`;
/// negative step with `start < stop`: `stop = start + 1`). Returns
/// the bounds and whether the step is 1 (the fast path).
pub fn fast_slice_indexes(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    slice: &SliceData,
    length: i64,
) -> Result<(BoundSlice, bool), StrError> {
    let mut b = slice_indices(ctx, types, slice, length)?;
    if b.step < 0 {
        b.stop += 1;
    }
    if b.step > 0 && b.start > b.stop {
        b.stop = b.start;
    } else if b.step < 0 && b.start < b.stop {
        b.stop = b.start + 1;
    }
    let fast = b.step == 1;
    Ok((b, fast))
}

/// `sliceCount` (`objstr.zig:427-433`): how many items a bound
/// slice selects. A signed walk, so negative-step slices work.
pub fn slice_count(b: &BoundSlice) -> usize {
    let step = b.step;
    if step > 0 && b.stop > b.start {
        ((b.stop - b.start + step - 1) / step) as usize
    } else if step < 0 && b.start > b.stop {
        ((b.start - b.stop + (-step) - 1) / (-step)) as usize
    } else {
        0
    }
}
