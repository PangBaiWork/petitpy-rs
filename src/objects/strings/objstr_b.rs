//! `objstr` part 2 (Lane C8b) — `objstr.zig` lines 2081–3864.
//!
//! `str_splitlines`, `str_partition` / `str_rpartition`, `str_count`,
//! `str_center` / `str_ljust` / `str_rjust`, `bytes_decode` /
//! `str_encode`, the formal constructors (`mp_obj_new_str_copy`,
//! `mp_obj_new_str_from_cstr`, `mp_obj_new_str_type_from_vstr`,
//! `mp_obj_new_str_from_vstr`, `mp_obj_new_bytes_from_vstr`,
//! `mp_obj_str_equal`, `mp_obj_str_get_qstr` / `get_str` /
//! `get_data`, `mp_obj_str_intern` / `intern_checked`), the
//! `dict_get` helper used by the `%`-format / `str.format`
//! machinery, and the shared method tables
//! (`array_bytearray_str_bytes_locals_table`, `objstr.zig:3364`).
//!
//! The C8a prelude (`objstr_a.rs`) already implements the formal
//! definitions of `new_str` / `new_str_of_type` /
//! `new_str_via_qstr` / `new_bytes` — this file does NOT re-define
//! them; it only adds the constructors the prelude did not cover.
//! `mp_obj_str_get_buffer` (`objstr.zig:2083-2090`) was already
//! ported by C8a (`str_get_buffer`, `objstr_methods_c.rs:391`) and
//! is skipped here.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{obj_get_int, obj_get_type, obj_is_true, BuiltinTypes, ObjError};
use crate::objects::primitives::ops::PrimError;
use crate::objects::slots::BuiltinFn;
use crate::objects::strings::objstr_a::{
    find_static_qstr, is_bytes_value, is_str_value, new_empty_bytes, new_empty_str, new_str,
    new_str_of_type, new_str_via_qstr, slot_raise, static_qstr_id, str_check_arg_type, str_data,
    str_data_pool, str_index_to_ptr, StrError,
};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::strings::objstr_methods_c::{bytes_make_new, str_make_new};
use crate::objects::strings::unicode::{utf8_charlen, utf8_next_char};
use crate::objects::strings::vstr::Vstr;
use crate::objects::value::{BytesData, GcRef, ListData, Obj, StrData, Value};
use crate::pyerr::{PyError, PyResult};

// ── shared helper ─────────────────────────────────────────────────────

/// Map a bare `PyError` signal into a `StrError` (the real exception
/// object is dropped at the slot boundary until C13 adds
/// `VmState.pending_exc` — design doc D7 convention).
/// `OutOfMemory` maps exactly; `PyRaise` is unreachable for today's
/// value kinds (truthiness only raises via a `__bool__` hook, which
/// no Rust type has yet) and maps to a bare `ValueError` placeholder.
pub(crate) fn pyerr_to_str(e: PyError) -> StrError {
    match e {
        PyError::OutOfMemory => StrError::Obj(ObjError::OutOfMemory),
        PyError::PyRaise => StrError::Prim(PrimError::ValueErrorBare),
    }
}

// ── `str_splitlines` (objstr.zig:2091-2134) ───────────────────────────

/// `npyStrSplitlinesParseKeepends` (`objstr.zig:246-253`): the
/// keepends flag is `args[1]` when present, else the `keepends`
/// kwarg from the kw map, else `False`. The kw-map fallback needs
/// C10/C13 (kwargs are pre-merged positionally today), so only the
/// positional form is live here.
fn splitlines_keepends(ctx: &mut CallCtx, types: &BuiltinTypes, args: &[Value]) -> Result<bool, StrError> {
    if args.len() > 1 {
        return obj_is_true(ctx, types, args[1]).map_err(pyerr_to_str);
    }
    Ok(false)
}

/// `str_splitlines_impl` (`objstr.zig:2091-2134`): split on `\n` and
/// `\r` ONLY (the Zig's `"\n\r"` boundary set — `\v`, `\f`,
/// `\x1c-\x1e`, `\x85`, U+2028 are NOT boundaries, unlike CPython).
/// `""` → `[]`, `"\n"` → `['']`.
pub fn str_splitlines(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_ = args[0];
    // Pool-aware (E2 — the D5 bug-2 class): arena-only `str_data`
    // returns `None` for dynamic-qstr literals (every string
    // literal in a module), so the bytes were read as empty.
    // Copy the bytes up front: the loop below allocates through
    // `ctx` (C8a's owned-copy pattern).
    let data = str_data_pool(ctx.arena, ctx.pool, self_).unwrap_or_default();
    let self_type = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    let keepends = splitlines_keepends(ctx, types, args)?;
    let mut items: Vec<Value> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < data.len() {
        let b = data[i];
        if b == b'\n' || b == b'\r' {
            let mut end = i;
            if keepends {
                end += 1;
                if b == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
                    end += 1;
                }
            }
            items.push(new_str_of_type(ctx, types, self_type, &data[start..end]));
            // Skip the line ending (a lone `\r` or `\n`, or `\r\n`
            // as one ending).
            i += 1;
            if b == b'\r' && i < data.len() && data[i] == b'\n' {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    // The trailing segment (empty when the string ends with a line
    // ending — `start == len` then, and nothing is appended).
    if start < data.len() {
        items.push(new_str_of_type(ctx, types, self_type, &data[start..]));
    }
    Ok(Value::Heap(ctx.alloc(
        Some(types.list),
        Obj::List(ListData { items }),
    )))
}

// ── `str_partition` / `str_rpartition` (objstr.zig:2135-2196) ─────────

/// `partitionerImpl` (`objstr.zig:2135-2189`): `direction > 0` =
/// `partition` (first occurrence), `< 0` = `rpartition` (last).
/// The separator arg is used AS-IS for the middle element (the
/// Zig's `arg` after `str_check_arg_type` — the bytes↔bytearray
/// copy is dead in Rust, no bytearray type exists yet).
pub fn partitioner_impl(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    self_: Value,
    arg: Value,
    direction: i32,
) -> Result<Value, StrError> {
    let self_type = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    str_check_arg_type(ctx.arena, types, Some(self_type), arg)?;
    let is_str = is_str_value(ctx.arena, self_);
    // Owned copies (see the `to_vec` note in `str_splitlines`).
    let data = str_data_pool(ctx.arena, ctx.pool, self_)
        .ok_or(StrError::BadImplicitConversion)?;
    let sep = str_data_pool(ctx.arena, ctx.pool, arg)
        .ok_or(StrError::BadImplicitConversion)?;
    if sep.is_empty() {
        return Err(StrError::EmptySeparator);
    }
    let empty = if is_str {
        new_empty_str()
    } else {
        new_empty_bytes(ctx, types)
    };
    let mut result = [empty, empty, empty];
    if direction > 0 {
        result[0] = self_;
    } else {
        result[2] = self_;
    }
    if let Some(pos) = crate::objects::strings::objstr_a::find_subbytes(&data, &sep, direction) {
        result[0] = new_str_of_type(ctx, types, self_type, &data[..pos]);
        // E2e: the vendor wraps a copy of the separator in
        // self's type when the arg's type differs (bytes sep on
        // a bytearray self → a bytearray sep;
        // `objstr.zig:2172-2176`, the BYTEARRAY comment).
        result[1] = if obj_get_type(ctx.arena, types, arg) != Some(self_type) {
            new_str_of_type(ctx, types, self_type, &sep)
        } else {
            arg
        };
        result[2] = new_str_of_type(ctx, types, self_type, &data[pos + sep.len()..]);
    }
    Ok(Value::Heap(ctx.alloc(
        Some(types.tuple),
        Obj::Tuple(result.to_vec()),
    )))
}

/// `str_partition` (`objstr.zig:2189-2191`).
pub fn str_partition(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    partitioner_impl(ctx, types, args[0], args[1], 1)
}

/// `str_rpartition` (`objstr.zig:2192-2196`).
pub fn str_rpartition(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    partitioner_impl(ctx, types, args[0], args[1], -1)
}

// ── `str_count` (objstr.zig:2217-2264) ────────────────────────────────

/// `str_count` (`objstr.zig:2217-2264`): non-overlapping count of
/// `sub` in `self[start:end]`. Empty needle counts
/// `utf8_charlen(slice) + 1` (code points for str, non-continuation
/// bytes for bytes — both go through `utf8_charlen` under full).
/// NOTE (intentional divergence): the Zig's `end - start` pointer
/// subtraction underflows (panic "integer overflow") when the empty
/// needle is counted with `start > end` (`"abc".count("", 5, 1)`
/// aborts the reference binary, rc=134); Rust saturates to 0
/// instead — same class as C8a's documented divergences.
pub fn str_count(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_ = args[0];
    // Owned copies (see the `to_vec` note in `str_splitlines`).
    let data = str_data_pool(ctx.arena, ctx.pool, self_).unwrap_or_default();
    let is_bytes = is_bytes_value(ctx.arena, self_);
    let sub = match args.get(1) {
        Some(v) => *v,
        None => return Err(StrError::WrongArgs),
    };
    let sub_data =
        str_data_pool(ctx.arena, ctx.pool, sub).ok_or(StrError::BadImplicitConversion)?;
    let start = match args.get(2) {
        Some(v) if *v != Value::None => str_index_to_ptr(ctx, types, &data, is_bytes, *v, true)?,
        _ => 0,
    };
    let end = match args.get(3) {
        Some(v) if *v != Value::None => str_index_to_ptr(ctx, types, &data, is_bytes, *v, true)?,
        _ => data.len(),
    };
    // `end - start` saturates (see the divergence note above).
    let slice_len = end.saturating_sub(start);
    let count = if sub_data.is_empty() {
        // `utf8Charlen(start, slice_len) + 1` — the Zig reads the
        // slice by pointer; `utf8_charlen` on the slice is the same
        // walk.
        utf8_charlen(&data[start..start + slice_len]) + 1
    } else {
        let mut n = 0usize;
        let mut ptr = start;
        let slice_end = start + slice_len;
        while ptr + sub_data.len() <= slice_end {
            if &data[ptr..ptr + sub_data.len()] == sub_data {
                n += 1;
                ptr += sub_data.len();
            } else if is_bytes {
                ptr += 1;
            } else {
                ptr = utf8_next_char(&data, ptr);
            }
        }
        n
    };
    Ok(Value::int(ctx.arena, Some(types.int), ctx.roots, count as i64))
}

// ── `str_center` / `str_ljust` / `str_rjust` (objstr.zig:2265-2347) ───

/// `strPadOneSide` (`objstr.zig:2306-2336`): pad one side of `data`
/// to `width` CHARACTERS with `fill` (a 1-BYTE fillchar; the caller
/// checks the length). The pad length is `width - n_chars`; the
/// buffer is `str_len + pad` bytes.
fn pad_one_side(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    data: &[u8],
    width: usize,
    fill: u8,
    left: bool,
) -> Value {
    let n_chars = utf8_charlen(&data);
    if width <= n_chars {
        return new_str_of_type(ctx, types, type_, data);
    }
    let pad = width - n_chars;
    let mut v = Vstr::init_len(data.len() + pad);
    // `vstr_init_len` sets `len = data.len() + pad` already; fill
    // the whole buffer, then copy the data over the right side.
    let bytes = v.as_bytes_mut_for_init();
    for b in bytes.iter_mut() {
        *b = fill;
    }
    if left {
        bytes[..data.len()].copy_from_slice(&data);
    } else {
        bytes[pad..pad + data.len()].copy_from_slice(&data);
    }
    new_str_type_from_vstr(ctx, types, type_, &mut v)
}

/// `str_center` (`objstr.zig:2265-2305`): the extra pad goes
/// RIGHT when the split is odd (`left = (width - chars) / 2`,
/// integer division, floor). A negative width aborts the original
/// (`@intCast(mp_obj_get_int)` i64→usize panics, objstr.zig:2270);
/// Rust returns `self` instead (intentional divergence).
pub fn str_center(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_ = args[0];
    // Owned copy (see the `to_vec` note in `str_splitlines`).
    let data = str_data_pool(ctx.arena, ctx.pool, self_).unwrap_or_default();
    let type_ = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    let width = obj_get_int(ctx, types, args[1]).map_err(StrError::Obj)?;
    if width < 0 {
        return Ok(self_);
    }
    let fill = pad_fillchar(ctx, args.get(2))?;
    let n_chars = utf8_charlen(&data);
    let width = width as usize;
    if width <= n_chars {
        return Ok(self_);
    }
    let left = (width - n_chars) / 2;
    let mut v = Vstr::init_len(data.len() + (width - n_chars));
    let bytes = v.as_bytes_mut_for_init();
    for b in bytes.iter_mut() {
        *b = fill;
    }
    bytes[left..left + data.len()].copy_from_slice(&data);
    Ok(new_str_type_from_vstr(ctx, types, type_, &mut v))
}

/// The fillchar argument (`args[2]` when present, else `' '`):
/// must be exactly 1 BYTE (`strPadOneSide` checks `data.len() !=
/// 1` — `objstr.zig:2285,2325`). A non-str/bytes fillchar is a
/// `BadImplicitConversion` in Rust (the Zig reads the object layout
/// unconditionally — garbage/UB there; probed below in the result
/// file).
fn pad_fillchar(ctx: &mut CallCtx, arg: Option<&Value>) -> Result<u8, StrError> {
    match arg {
        None => Ok(b' '),
        Some(v) => {
            let d = str_data_pool(ctx.arena, ctx.pool, *v).ok_or(StrError::BadImplicitConversion)?;
            if d.len() != 1 {
                return Err(StrError::FillcharMustBeOne);
            }
            Ok(d[0])
        }
    }
}

/// `str_ljust` (`objstr.zig:2337-2339`).
pub fn str_ljust(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_ = args[0];
    // Owned copy (see the `to_vec` note in `str_splitlines`).
    let data = str_data_pool(ctx.arena, ctx.pool, self_).unwrap_or_default();
    let type_ = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    let width = obj_get_int(ctx, types, args[1]).map_err(StrError::Obj)?;
    if width < 0 {
        return Ok(self_);
    }
    let fill = pad_fillchar(ctx, args.get(2))?;
    Ok(pad_one_side(ctx, types, type_, &data, width as usize, fill, true))
}

/// `str_rjust` (`objstr.zig:2340-2342`).
pub fn str_rjust(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_ = args[0];
    // Owned copy (see the `to_vec` note in `str_splitlines`).
    let data = str_data_pool(ctx.arena, ctx.pool, self_).unwrap_or_default();
    let type_ = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    let width = obj_get_int(ctx, types, args[1]).map_err(StrError::Obj)?;
    if width < 0 {
        return Ok(self_);
    }
    let fill = pad_fillchar(ctx, args.get(2))?;
    Ok(pad_one_side(ctx, types, type_, &data, width as usize, fill, false))
}

// ── `bytes_decode` / `str_encode` (objstr.zig:2350-2364) ──────────────

/// `bytes_decode` (`objstr.zig:2350-2356`): `str(self, encoding,
/// errors)` — the encoding/errors args are parsed but NEVER
/// validated or used (C8a's `str_make_new` copies the bytes raw;
/// the reference binary returns raw bytes for `b"\xff".decode()`
/// and for `b"ab".decode("utf-16")` alike).
pub fn bytes_decode(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    if args.len() == 1 {
        let utf8 = Value::Qstr(static_qstr_id(b"utf-8").unwrap_or(0));
        str_make_new(ctx, types, types.str_, &[args[0], utf8], &[])
    } else {
        str_make_new(ctx, types, types.str_, args, &[])
    }
}

/// `str_encode` (`objstr.zig:2357-2364`): `bytes(self, encoding,
/// errors)` — same "encoding is ignored" behaviour as decode.
pub fn str_encode(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    if args.len() == 1 {
        let utf8 = Value::Qstr(static_qstr_id(b"utf-8").unwrap_or(0));
        bytes_make_new(ctx, types, &[args[0], utf8], &[], None)
    } else {
        bytes_make_new(ctx, types, args, &[], None)
    }
}

// ── constructors (objstr.zig:3082-3274) ───────────────────────────────

/// `mp_obj_new_str_copy` (`objstr.zig:3082-3094`): always a fresh
/// heap copy of the given type (no interning). `str` → `Obj::Str`,
/// anything else today is `bytes` (bytearray lands with C9).
pub fn new_str_copy(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    data: &[u8],
) -> Value {
    if type_ == types.str_ {
        Value::Heap(ctx.alloc(
            Some(type_),
            Obj::Str(StrData {
                bytes: data.to_vec(),
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

/// `mp_obj_new_str_from_cstr` (`objstr.zig:3163-3167`): from a
/// NUL-terminated C string — the slice is truncated at the first
/// NUL byte (the Zig counts to the NUL).
pub fn new_str_from_cstr(ctx: &mut CallCtx, types: &BuiltinTypes, data: &[u8]) -> Value {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    new_str(ctx, types, &data[..end])
}

/// `mp_obj_new_str_type_from_vstr` (`objstr.zig:3221-3255`): take
/// the vstr's bytes (with static-qstr interning for `str`), and
/// leave the vstr emptied (`buf` freed, `alloc` 0 — `len` is left
/// untouched, exactly as in C). The Zig reuses the buffer when
/// `v_len + 1 == v_alloc` and reallocs otherwise; the Rust copy is
/// unobservable.
pub fn new_str_type_from_vstr(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    vstr: &mut Vstr,
) -> Value {
    let data = vstr.as_bytes().to_vec();
    if type_ == types.str_ {
        if let Some(id) = find_static_qstr(&data) {
            vstr.clear();
            vstr.alloc = 0;
            return Value::Qstr(id);
        }
    }
    let out = new_str_of_type(ctx, types, type_, &data);
    vstr.clear();
    vstr.alloc = 0;
    out
}

/// `mp_obj_new_str_from_vstr` (`objstr.zig:3256-3271`).
pub fn new_str_from_vstr(ctx: &mut CallCtx, types: &BuiltinTypes, vstr: &mut Vstr) -> Value {
    new_str_type_from_vstr(ctx, types, types.str_, vstr)
}

/// `mp_obj_new_bytes_from_vstr` (`objstr.zig:3272-3274`).
pub fn new_bytes_from_vstr(ctx: &mut CallCtx, types: &BuiltinTypes, vstr: &mut Vstr) -> Value {
    new_str_type_from_vstr(ctx, types, types.bytes, vstr)
}

/// `mp_obj_str_equal` (`objstr.zig:3276-3296`): qstr fast path,
/// then hash-then-memcmp. Note the Zig does NOT distinguish str
/// from bytes here — `mp_obj_str_equal("a", b"a")` is true. Rust:
/// compare the byte contents (str or bytes payloads).
/// Pool-aware (C13b): `str_data` returns `None` for dynamic
/// qstr ids, so a heap `Str` never equalled a dynamic `Qstr`
/// with the same bytes — `dict([('k', 9)])['k']` raised
/// `KeyError` (`str_data_pool`'s doc).
pub fn str_equal(arena: &GcArena, pool: &RuntimeQstr, s1: Value, s2: Value) -> bool {
    if let (Value::Qstr(a), Value::Qstr(b)) = (s1, s2) {
        return a == b;
    }
    match (
        str_data_pool(arena, pool, s1),
        str_data_pool(arena, pool, s2),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `mp_obj_str_get_qstr` (`objstr.zig:3297-3311`): the qstr id of a
/// str, creating a dynamic qstr for heap content that has none.
/// Without the VM's `RuntimeQstr` pool (C13) only static ids can
/// be returned — `None` for non-static heap content (C13 gap) and
/// for non-str values.
pub fn get_qstr(arena: &GcArena, types: &BuiltinTypes, v: Value) -> Option<u32> {
    match v {
        Value::Qstr(id) => Some(id),
        _ if is_str_value(arena, v) => {
            let data = str_data(arena, v).unwrap_or(&[]);
            find_static_qstr(data)
        }
        _ => {
            let _ = types;
            None
        }
    }
}

/// `mp_obj_str_get_str` (`objstr.zig:3312-3320`): the byte content
/// of a str (or `None` — the Zig sets `pending_exc` and returns
/// `NULL` for a non-str; C8a's `str_data` does not record the
/// pending error, and neither does this wrapper — C13).
pub fn get_str<'a>(arena: &'a GcArena, v: Value) -> Option<&'a [u8]> {
    str_data(arena, v)
}

/// `mp_obj_str_get_data` (`objstr.zig:3321-3347`): like `get_str`
/// but also reports the length (the Rust slice carries it). Same
/// pending-error note as `get_str`.
pub fn get_data<'a>(arena: &'a GcArena, v: Value) -> Option<&'a [u8]> {
    str_data(arena, v)
}

/// `mp_obj_str_intern` (`objstr.zig:3179-3183`): `qstr_from_strn`
/// on the content (dynamic interning; static id when possible).
pub fn intern(ctx: &mut CallCtx, types: &BuiltinTypes, v: Value) -> Value {
    let data = str_data_pool(ctx.arena, ctx.pool, v).unwrap_or_default();
    new_str_via_qstr(ctx, types, &data)
}

/// `mp_obj_str_intern_checked` (`objstr.zig:3184-3197`): `NULL`
/// when the value is not a str (returned here as `Qstr(0)`,
/// `MP_QSTRnull`).
pub fn intern_checked(ctx: &mut CallCtx, types: &BuiltinTypes, v: Value) -> Value {
    match str_data_pool(ctx.arena, ctx.pool, v) {
        None => Value::Qstr(0),
        Some(data) => new_str_via_qstr(ctx, types, &data),
    }
}

/// `mp_obj_str_set_data` (`objstr.zig:3198-3205`, the C8a prelude
/// did not export it): mutate a heap `Str`/`Bytes` payload's bytes
/// in place. A non-str/bytes object is left untouched (the Zig
/// takes a `*MpObjStr` and would read garbage there).
pub fn set_str_data(ctx: &mut CallCtx, r: GcRef, data: &[u8]) {
    match ctx.arena.get_mut(r) {
        Obj::Str(s) => s.bytes = data.to_vec(),
        Obj::Bytes(b) => b.bytes = data.to_vec(),
        _ => {}
    }
}

// ── `dict_get` helper (objstr.zig:246-400, `npyStrDictGet`) ────────────

/// `npyStrDictGet` (`objstr.zig:368-377`): `dict.get(key)` — the
/// `%`-format / `str.format` keyword lookup. Re-pointed at C9's
/// real hash-table lookup at the C8b merge (the linear scan over
/// the stub `MapData` is gone); a miss — or an unhashable key —
/// returns `None`, matching the old scan's "not equal" fallback.
/// A non-dict value (the NULL sentinel the format machinery may
/// pass) also returns `None` — `dict_get_value` panics on those,
/// so the guard stays here.
pub fn dict_get(ctx: &mut CallCtx, dict: Value, key: Value) -> Option<Value> {
    let is_dict = matches!(dict, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Dict(_)));
    if !is_dict {
        return None;
    }
    crate::objects::containers::objdict::dict_get_value(ctx, dict, key).ok()
}

// ── method tables (objstr.zig:3364-3616) ──────────────────────────────

/// `str` locals (`objstr.zig:3536-3616`): the shared table's
/// entries 5..end — 32 entries, in Zig order.
pub static STR_METHODS: &[(&str, BuiltinFn)] = &[
    ("find", crate::objects::strings::objstr_methods_b::str_find_builtin),
    ("rfind", crate::objects::strings::objstr_methods_b::str_rfind_builtin),
    ("index", crate::objects::strings::objstr_methods_b::str_index_builtin),
    ("rindex", crate::objects::strings::objstr_methods_b::str_rindex_builtin),
    ("join", crate::objects::strings::objstr_methods_b::str_join_builtin),
    ("split", crate::objects::strings::objstr_methods_b::str_split_builtin),
    ("splitlines", str_splitlines_builtin),
    ("rsplit", crate::objects::strings::objstr_methods_b::str_rsplit_builtin),
    ("startswith", crate::objects::strings::objstr_methods_b::str_startswith_builtin),
    ("endswith", crate::objects::strings::objstr_methods_b::str_endswith_builtin),
    ("strip", crate::objects::strings::objstr_methods_b::str_strip_builtin),
    ("lstrip", crate::objects::strings::objstr_methods_b::str_lstrip_builtin),
    ("rstrip", crate::objects::strings::objstr_methods_b::str_rstrip_builtin),
    ("format", crate::objects::strings::objstr_format::str_format_builtin),
    ("replace", crate::objects::strings::objstr_methods_b::str_replace_builtin),
    ("count", str_count_builtin),
    ("partition", str_partition_builtin),
    ("rpartition", str_rpartition_builtin),
    ("center", str_center_builtin),
    ("ljust", str_ljust_builtin),
    ("rjust", str_rjust_builtin),
    ("lower", crate::objects::strings::objstr_methods_b::str_lower_builtin),
    ("upper", crate::objects::strings::objstr_methods_b::str_upper_builtin),
    ("capitalize", crate::objects::strings::objstr_methods_b::str_capitalize_builtin),
    ("title", crate::objects::strings::objstr_methods_b::str_title_builtin),
    ("zfill", crate::objects::strings::objstr_methods_b::str_zfill_builtin),
    ("isspace", crate::objects::strings::objstr_methods_b::str_isspace_builtin),
    ("isalpha", crate::objects::strings::objstr_methods_b::str_isalpha_builtin),
    ("isdigit", crate::objects::strings::objstr_methods_b::str_isdigit_builtin),
    ("isupper", crate::objects::strings::objstr_methods_b::str_isupper_builtin),
    ("islower", crate::objects::strings::objstr_methods_b::str_islower_builtin),
    ("isalnum", crate::objects::strings::objstr_methods_b::str_isalnum_builtin),
    ("encode", str_encode_builtin),
];

/// `bytes` locals: the shared table's entries 2..(len−1) — 34
/// entries, in Zig order (`hex`, `fromhex`, `decode`, then the same
/// string methods as `str` except `format`'s place is kept, and
/// without `encode`).
pub static BYTES_METHODS: &[(&str, BuiltinFn)] = &[
    ("hex", crate::objects::strings::objstr_methods_b::bytes_hex_builtin),
    ("fromhex", crate::objects::strings::objstr_methods_b::bytes_fromhex_builtin),
    ("decode", bytes_decode_builtin),
    ("find", crate::objects::strings::objstr_methods_b::str_find_builtin),
    ("rfind", crate::objects::strings::objstr_methods_b::str_rfind_builtin),
    ("index", crate::objects::strings::objstr_methods_b::str_index_builtin),
    ("rindex", crate::objects::strings::objstr_methods_b::str_rindex_builtin),
    ("join", crate::objects::strings::objstr_methods_b::str_join_builtin),
    ("split", crate::objects::strings::objstr_methods_b::str_split_builtin),
    ("splitlines", str_splitlines_builtin),
    ("rsplit", crate::objects::strings::objstr_methods_b::str_rsplit_builtin),
    ("startswith", crate::objects::strings::objstr_methods_b::str_startswith_builtin),
    ("endswith", crate::objects::strings::objstr_methods_b::str_endswith_builtin),
    ("strip", crate::objects::strings::objstr_methods_b::str_strip_builtin),
    ("lstrip", crate::objects::strings::objstr_methods_b::str_lstrip_builtin),
    ("rstrip", crate::objects::strings::objstr_methods_b::str_rstrip_builtin),
    ("format", crate::objects::strings::objstr_format::str_format_builtin),
    ("replace", crate::objects::strings::objstr_methods_b::str_replace_builtin),
    ("count", str_count_builtin),
    ("partition", str_partition_builtin),
    ("rpartition", str_rpartition_builtin),
    ("center", str_center_builtin),
    ("ljust", str_ljust_builtin),
    ("rjust", str_rjust_builtin),
    ("lower", crate::objects::strings::objstr_methods_b::str_lower_builtin),
    ("upper", crate::objects::strings::objstr_methods_b::str_upper_builtin),
    ("capitalize", crate::objects::strings::objstr_methods_b::str_capitalize_builtin),
    ("title", crate::objects::strings::objstr_methods_b::str_title_builtin),
    ("zfill", crate::objects::strings::objstr_methods_b::str_zfill_builtin),
    ("isspace", crate::objects::strings::objstr_methods_b::str_isspace_builtin),
    ("isalpha", crate::objects::strings::objstr_methods_b::str_isalpha_builtin),
    ("isdigit", crate::objects::strings::objstr_methods_b::str_isdigit_builtin),
    ("isupper", crate::objects::strings::objstr_methods_b::str_isupper_builtin),
    ("islower", crate::objects::strings::objstr_methods_b::str_islower_builtin),
    ("isalnum", crate::objects::strings::objstr_methods_b::str_isalnum_builtin),
];

/// Look a method up in the `str` table (name → slot function).
pub fn lookup_str_method(name: &str) -> Option<BuiltinFn> {
    STR_METHODS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// Look a method up in the `bytes` table.
pub fn lookup_bytes_method(name: &str) -> Option<BuiltinFn> {
    BYTES_METHODS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

// ── `BuiltinFn` wrappers (C8a's `builtin_method!` macro is scoped to
//    `objstr_methods_b.rs`, so these are written out explicitly in
//    the same shape: empty args → `PyRaise`; types from `ctx.types`;
//    `StrError` → the `CallCtx` pending raise via `slot_raise`).

/// `str.splitlines` slot wrapper.
pub fn str_splitlines_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_splitlines(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.splitlines` as a `BuiltinKwFn` (E2): the `keepends`
/// kwarg passes through (`npyStrSplitlinesParseKeepends`,
/// objstr.zig:375-392 — a positional arg wins over the kwarg,
/// unknown kwargs are ignored, default `False`).
pub fn str_splitlines_kw_wrapper(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let types = ctx.types;
    let keepends_id =
        static_qstr_id(b"keepends").expect("keepends is a static qstr");
    let mut merged: Vec<Value> = args.to_vec();
    if args.len() < 2 {
        if let Some((_, v)) = kwargs.iter().find(|(k, _)| *k == keepends_id) {
            merged.push(*v);
        }
    }
    str_splitlines(ctx, types, &merged).map_err(|e| slot_raise(ctx, e))
}

/// `str.partition` slot wrapper.
pub fn str_partition_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // E2c sweep: reads `args[1]` unguarded — the old
    // `args.is_empty()` guard let a short call panic (SIGABRT).
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_partition(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.rpartition` slot wrapper.
pub fn str_rpartition_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // E2c sweep: reads `args[1]` unguarded — the old
    // `args.is_empty()` guard let a short call panic (SIGABRT).
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_rpartition(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.count` slot wrapper.
pub fn str_count_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_count(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.center` slot wrapper.
pub fn str_center_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // E2c sweep: reads `args[1]` unguarded — the old
    // `args.is_empty()` guard let a short call panic (SIGABRT).
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_center(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.ljust` slot wrapper.
pub fn str_ljust_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // E2c sweep: reads `args[1]` unguarded — the old
    // `args.is_empty()` guard let a short call panic (SIGABRT).
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_ljust(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.rjust` slot wrapper.
pub fn str_rjust_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // E2c sweep: reads `args[1]` unguarded — the old
    // `args.is_empty()` guard let a short call panic (SIGABRT).
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_rjust(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `bytes.decode` slot wrapper.
pub fn bytes_decode_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    bytes_decode(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}

/// `str.encode` slot wrapper.
pub fn str_encode_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_encode(ctx, types, args).map_err(|e| slot_raise(ctx, e))
}
