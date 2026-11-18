//! str / bytes core — prelude + constructors + index helpers +
//! iterators (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 1–2080, first half:
//! the prelude (exception constructors, small helpers), `find_subbytes`,
//! the `str_index_to_ptr` full arm, and the str/bytes iterators.
//! (The slice helpers — `MpBoundSlice`, `sliceCount` — live in
//! `objstr_slice.rs`.) The method bodies live
//! in `objstr_methods_a.rs` / `objstr_methods_b.rs` and build on this
//! file — the shared prelude is imported, never copied (C8b imports
//! from here too).
//!
//! Error model: like C6's `ObjError`, raises here are `StrError`
//! values carrying exactly the data the Zig formats into the
//! exception — the exception type plus the TERSE message literal.
//! The type itself lives in `objstr_errors.rs` (E2b split) and is
//! re-exported below; the model text is kept here because the slot
//! functions below are its main users. Method functions return
//! (frozen `PyResult<Value>` signatures) record a specific raise
//! in the `CallCtx` pending raise (`PendingRaise`, the
//! `pyerr.pending_exc` stand-in, see below) before returning
//! `PyError::PyRaise`, and record nothing when the original
//! returns NULL — so "no pending raise" still means "not
//! implemented" for C6's `call_*` wrappers, until C13 teaches
//! them to check the pending raise (design doc D7).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::containers::map_set::set_items;
use crate::objects::containers::objdict::DictError;
use crate::objects::obj::{
    obj_get_int_maybe, obj_get_type, qstr_bytes, BuiltinTypes, ObjError, BUFFER_READ,
};
use crate::objects::obj_ops::{get_buffer, get_index};
use crate::objects::slots::{BufferInfo, CallHook};
use crate::objects::strings::unicode::{is_utf8_cont, utf8_next_char};
use crate::objects::value::{BytesData, GcRef, Obj, StrData, Value};

// The error type lives in `objstr_errors.rs` (E2b split —
// `objstr_a.rs` was 806 lines, over the 800-line cap);
// re-exported here so existing imports keep working.
pub use crate::objects::strings::objstr_errors::{StrError, StrExc};
pub(crate) use crate::objects::strings::objstr_errors::{result_type, slot_raise};

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

/// Pool-aware `str_data`: resolves a `Qstr` through the
/// `RuntimeQstr` pool (static AND dynamic ids — `qstr_str`
/// covers both), heap `Str`/`Bytes` through the arena. The
/// arena-only `str_data` above returns `None` for dynamic qstr
/// ids (>= `N_TOTAL`), whose bytes live only in the pool — so
/// slot fns that run under the VM (which HAVE the pool via
/// `CallCtx`) must use this one, or string literals from the
/// module (`"hello"[1]`, `"a" + "b"`, `"%d" % x`) read as
/// "not implemented" / "isn't subscriptable". Returns an owned
/// copy (the pool and arena borrows have different lifetimes).
/// E2e: a bytearray also reads here (its raw bytes — the
/// vendor's shared str/bytes/bytearray method table); int
/// arrays and memoryviews still return `None`.
pub fn str_data_pool(
    arena: &GcArena,
    pool: &crate::runtime::qstr::RuntimeQstr,
    v: Value,
) -> Option<Vec<u8>> {
    match v {
        Value::Qstr(id) => Some(pool.qstr_str(id as u16).to_vec()),
        Value::Heap(r) => match arena.get(r) {
            Obj::Str(s) => Some(s.bytes.clone()),
            Obj::Bytes(b) => Some(b.bytes.clone()),
            // E2e: a bytearray carries str/bytes semantics in the
            // vendor's shared method table
            // (`array_bytearray_str_bytes_locals_table`,
            // `objstr.zig:3533-3614`), so the str methods read
            // its raw bytes. Only the bytearray typecode (1):
            // int arrays and memoryviews keep returning `None`
            // here (their methods are append/extend / hex only).
            Obj::Array(a)
                if a.typecode
                    == crate::objects::containers::binary::BYTEARRAY_TYPECODE =>
            {
                crate::objects::containers::binary::array_raw_bytes(arena, r)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Read an object's bytes through the buffer protocol
/// (`npyStrGetBufferRaise` / `mp_get_buffer` with `MP_BUFFER_READ`):
/// str/bytes payloads read via `str_data`; anything else probes
/// `get_buffer` — no buffer → `BufferProtocol` ("object with buffer
/// protocol required"). A buffer on a non-str/bytes object reads
/// through the safe `buffer_bytes` copy (E2e: array/bytearray/
/// memoryview are buffer providers since C13b, and the vendor's
/// str slot reads any buffer rhs for a bytes lhs).
pub fn buffer_data(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    v: Value,
) -> Result<Vec<u8>, StrError> {
    // E2: the arena-only `str_data` read returned `None` for
    // dynamic-qstr literals (every string literal in a module), so
    // `bytes.fromhex('00:11:22', ':')` fell through to the
    // buffer-protocol error. The pool-aware read covers statics,
    // dynamics, and heap str/bytes alike (D5's `str_data_pool`
    // fix class).
    if let Some(d) = str_data_pool(ctx.arena, ctx.pool, v) {
        return Ok(d);
    }
    // E2e (item 1): array/bytearray/memoryview buffers read
    // through the safe `buffer_bytes` copy
    // (`containers::binary` — handles `Obj::Array` incl. view
    // chains, `Obj::Bytes`, `Obj::Str`, Qstr). The vendor's str
    // slot reads ANY buffer rhs for a bytes lhs via
    // `npyStrGetBufferRead` (`objstr.zig:842-932`); the old code
    // stopped at `get_buffer` `Ok(true)` because the raw pointer
    // in `BufferInfo` can only be read with `unsafe` — the
    // `buffer_bytes` copy is the safe reader C9 anticipated.
    // (Float arrays: the reference cannot even construct
    // `array('f', [1.0])` — its extend calls `mp_obj_get_int`
    // on the float item — and `bytes(array('f', []))` panics it
    // on a null items pointer; Rust returns the raw bytes /
    // `b''` instead. Documented in `scripts/e2e_result.txt` §4.)
    if let Some((bytes, _)) = crate::objects::containers::binary::buffer_bytes(ctx, v) {
        return Ok(bytes);
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
    mut call: Option<CallHook<'_, '_>>,
) -> Result<Vec<Value>, StrError> {
    // A generator (E2b): drain it through the hook's `resume`
    // (the `VmHook` pair — `bytes(gen())` resumes it step by
    // step). Without a hook (the ctx-only path — `str.join` and
    // the `.extend` method) there is no VM to resume it: the old
    // `TypeError("object not iterable")`. A raise inside the
    // generator is `StrError::PyRaise` (no name/message — the
    // exception object is already stashed on the hook's side
    // channel, so `record_typed` records nothing).
    if let Value::Heap(r) = arg {
        if matches!(ctx.arena.get(r), Obj::Generator(_)) {
            let Some(hook) = call.as_deref_mut() else {
                return Err(StrError::ObjectNotIterable);
            };
            let mut out = Vec::new();
            loop {
                match hook.resume(ctx, arg) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => break,
                    Err(_) => return Err(StrError::PyRaise),
                }
            }
            return Ok(out);
        }
    }
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
                // Pool-aware for uniformity with the `Qstr` arm
                // below (heap `Str`/`Bytes` read identically
                // through either — their bytes live in the arena).
                str_data_pool(ctx.arena, ctx.pool, arg).unwrap_or_default(),
                is_str_value(ctx.arena, arg),
            ),
            Obj::Dict(d) => JoinSource::Items(d.map.entries.iter().filter_map(|e| e.key).collect()),
            Obj::Set(s) => JoinSource::Items(set_items(&s.table)),
            Obj::Range(rg) => JoinSource::Range(rg.start, rg.stop, rg.step),
            // E2e (item 1): a C11 iterator (`iter([128, 255])`)
            // drains through the general collector (its
            // `Protocol` arm runs `iternext` with the hook's
            // seams, or the builtin-live stubs without one) —
            // `bytes(iter([128, 255]))` is `b'\x80\xff'` on the
            // reference. The error maps by kind: a raise inside
            // the iterator is `PyRaise` (already stashed on the
            // side channel), anything else is "not iterable".
            Obj::Iterator(_) => {
                return crate::objects::containers::objdict_collect::collect_iterable(
                    ctx, arg, call,
                )
                .map_err(|e| match e {
                    DictError::PyRaise => StrError::PyRaise,
                    DictError::OutOfMemory => StrError::Obj(ObjError::OutOfMemory),
                    _ => StrError::ObjectNotIterable,
                });
            }
            _ => return Err(StrError::ObjectNotIterable),
        },
        // Pool-aware (E2): `qstr_bytes` is static-only, so a
        // dynamic-qstr literal (`''.join('abc')`) read as empty.
        Value::Qstr(_) => JoinSource::StrData(
            str_data_pool(ctx.arena, ctx.pool, arg).unwrap_or_default(),
            true,
        ),
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
/// must equal `self_type`, with bytes ↔ bytearray normalised to
/// each other first (the Zig normalises both sides to `bytes`).
/// Mismatch raises "can't convert to str implicitly".
pub fn str_check_arg_type(
    arena: &GcArena,
    types: &BuiltinTypes,
    self_type: Option<GcRef>,
    arg: Value,
) -> Result<(), StrError> {
    // E2e: bytes ↔ bytearray are normalised to each other
    // before the comparison (vendor `objstr.zig:664-680`,
    // `str_check_arg_type` — "bytes and bytearray have the
    // same byte-semantics"), so `bytearray(b"ab").find(b"a")`
    // and `b"ab".find(bytearray(b"a"))` both pass. The old
    // comment claimed no bytearray type existed in Rust (C9);
    // it has existed since C13b.
    let mut arg_t = obj_get_type(arena, types, arg);
    let mut self_t = self_type;
    if arg_t == Some(types.bytearray) {
        arg_t = Some(types.bytes);
    }
    if self_t == Some(types.bytearray) {
        self_t = Some(types.bytes);
    }
    if arg_t != self_t {
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
    } else if type_ == types.bytearray {
        // E2e: the vendor's `mp_obj_new_str_type_from_vstr`
        // builds a REAL bytearray for the bytearray type
        // (`objstr.zig:3245-3247` — `mp_obj_new_bytearray_by_ref`),
        // not a `Bytes` payload with a bytearray header (a
        // `Bytes` payload would break `array_print` /
        // `append` / mutation on the result).
        Value::Heap(ctx.alloc(
            Some(type_),
            Obj::Array(crate::objects::value::ArrayData {
                typecode: crate::objects::containers::binary::BYTEARRAY_TYPECODE,
                free: 0,
                len: data.len(),
                data: data.to_vec(),
                source: None,
            }),
        ))
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
/// `mp_const_empty_bytes_obj` (so `b'' is b''` and `bytes() is
/// b''`); Rust returns `types.empty_bytes` (C12a — the singleton
/// landed). Slicing still allocates fresh (`b'ab'[:0] is b''` is
/// False on the reference binary), so only this function uses the
/// singleton — `new_bytes` is untouched.
pub fn new_empty_bytes(_ctx: &mut CallCtx, types: &BuiltinTypes) -> Value {
    Value::Heap(types.empty_bytes)
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

// The slice helpers live in `objstr_slice.rs` (split out to keep
// this file under 800 lines); re-exported so existing imports
// (`use objstr_a::*`) keep working.
pub use crate::objects::strings::objstr_slice::{
    fast_slice_indexes, slice_count, slice_indices, BoundSlice,
};
