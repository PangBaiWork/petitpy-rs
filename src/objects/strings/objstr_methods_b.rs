//! str / bytes methods, part 2 (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 581–640
//! (`mp_str_print_quoted` / `str_print`), 1130–1310 (case
//! conversion + predicates + `str_zfill`), 2000–2080
//! (`bytesHexImpl` / `bytesFromHexImpl`), and 3647–3760 (the
//! `mp_type_str` / `mp_type_bytes` ROMs).
//!
//! Method functions take `(ctx, types, args)` with `args[0]` =
//! self — except `bytes_fromhex`, whose `args[0]` is the class
//! (it is a classmethod in the original). The `*_builtin`
//! wrappers at the bottom adapt them to the frozen `BuiltinFn`
//! shape (`fn(&mut CallCtx, &[Value])`) by reading `types` from
//! the installed-types cell — C10/D1 drop them straight into
//! `FunBuiltinData.func`.
//!
//! The `TypeData` helpers build the str/bytes type objects with
//! the slots filled. They take the locals dict as a parameter:
//! filling it with the method table needs `FunBuiltinData`
//! allocation + the `n_args` encoding, both C10/D1-owned. The
//! membership (from the shared ROM table,
//! `objstr.zig:3520-3600`) is: str = find..encode (no hex,
//! fromhex, decode); bytes = hex, fromhex, decode, find..isalnum
//! (no encode). `base`/`mro` are C12's (object base + C3), and
//! `getiter` stays `None` until C12 builds the polymorph-iter
//! type the iterator constructors need.

use std::fmt;

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int, obj_get_type, BuiltinTypes};
use crate::objects::slots::PrintKind;
use crate::objects::strings::objstr_a::*;
use crate::objects::strings::objstr_methods_c as mc;
use crate::objects::strings::unicode::{
    unichar_isalnum, unichar_isalpha, unichar_isdigit, unichar_islower, unichar_isspace,
    unichar_isupper, unichar_isxdigit, unichar_tolower, unichar_toupper, unichar_xdigit_value,
};
use crate::objects::value::{GcRef, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

// ── case conversion ───────────────────────────────────────────────────

/// `strCaseconv` (`objstr.zig:1126-1140`): map every byte through
/// `f` (`unichar_tolower` / `unichar_toupper` — byte-wise, ASCII
/// only; non-ASCII bytes pass through unchanged). The result has
/// self's type.
fn str_caseconv(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    self_value: Value,
    f: fn(u32) -> u32,
) -> Result<Value, StrError> {
    let data = str_data(ctx.arena, self_value).ok_or(StrError::BadImplicitConversion)?;
    let data = data.to_vec();
    let out: Vec<u8> = data.iter().map(|b| f(*b as u32) as u8).collect();
    let self_type = obj_get_type(ctx.arena, types, self_value);
    let is_str = is_str_value(ctx.arena, self_value);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

/// `str_lower` (`objstr.zig:1142-1144`).
pub fn str_lower(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_caseconv(ctx, types, args[0], unichar_tolower)
}

/// `str_upper` (`objstr.zig:1145-1147`).
pub fn str_upper(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_caseconv(ctx, types, args[0], unichar_toupper)
}

/// `str_title` (`objstr.zig:1150-1167`): capitalize each word —
/// a word boundary is any non-alpha char.
pub fn str_title(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let data = str_data(ctx.arena, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let data = data.to_vec();
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut prev_is_alpha = false;
    for &c in &data {
        let is_alpha = unichar_isalpha(c as u32);
        if is_alpha && !prev_is_alpha {
            out.push(unichar_toupper(c as u32) as u8);
        } else if is_alpha {
            out.push(unichar_tolower(c as u32) as u8);
        } else {
            out.push(c);
        }
        prev_is_alpha = is_alpha;
    }
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let is_str = is_str_value(ctx.arena, args[0]);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

/// `str_capitalize` (`objstr.zig:1170-1181`): first char upper,
/// the rest lower.
pub fn str_capitalize(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let data = str_data(ctx.arena, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let data = data.to_vec();
    let out: Vec<u8> = data
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if i == 0 {
                unichar_toupper(c as u32) as u8
            } else {
                unichar_tolower(c as u32) as u8
            }
        })
        .collect();
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let is_str = is_str_value(ctx.arena, args[0]);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

/// `str_zfill` (`objstr.zig:1184-1208`): pad with zeros on the
/// left to reach `width`; a leading `+`/`-` stays leftmost and the
/// padding goes after it. `width <= len` returns self itself.
/// A negative width crashes the original (`@intCast` of a negative
/// `isize` to `usize`); Rust returns self (CPython's behavior), a
/// deliberate divergence.
pub fn str_zfill(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let width = obj_get_int(ctx, types, args[1])?;
    let data = str_data(ctx.arena, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let data = data.to_vec();
    if width <= data.len() as i64 {
        return Ok(args[0]);
    }
    let width = width as usize;
    let pad_count = width - data.len();
    let mut out: Vec<u8> = Vec::with_capacity(width);
    let mut sign_chars = 0;
    if !data.is_empty() && (data[0] == b'+' || data[0] == b'-') {
        out.push(data[0]);
        sign_chars = 1;
    }
    out.resize(sign_chars + pad_count, b'0');
    out.extend_from_slice(&data[sign_chars..]);
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let is_str = is_str_value(ctx.arena, args[0]);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

// ── predicates ────────────────────────────────────────────────────────

/// `strUniIstype` (`objstr.zig:1228-1256`): the shared predicate
/// core. Empty → `False`. `is_case_check` (isupper/islower) checks
/// only alpha chars and requires at least one; otherwise every
/// char must satisfy `f`.
fn str_uni_istype(data: &[u8], f: fn(u32) -> bool, is_case_check: bool) -> bool {
    if data.is_empty() {
        return false;
    }
    if !is_case_check {
        return data.iter().all(|&b| f(b as u32));
    }
    let mut contains_alpha = false;
    for &b in data {
        if unichar_isalpha(b as u32) {
            contains_alpha = true;
            if !f(b as u32) {
                return false;
            }
        }
    }
    contains_alpha
}

macro_rules! str_predicate {
    ($name:ident, $f:ident, $case:expr) => {
        pub fn $name(
            ctx: &mut CallCtx,
            types: &BuiltinTypes,
            args: &[Value],
        ) -> Result<Value, StrError> {
            let _ = types;
            let data =
                str_data(ctx.arena, args[0]).ok_or(StrError::BadImplicitConversion)?;
            Ok(Value::Bool(str_uni_istype(data, $f, $case)))
        }
    };
}

str_predicate!(str_isspace, unichar_isspace, false);
str_predicate!(str_isalpha, unichar_isalpha, false);
str_predicate!(str_isdigit, unichar_isdigit, false);
str_predicate!(str_isupper, unichar_isupper, true);
str_predicate!(str_islower, unichar_islower, true);
str_predicate!(str_isalnum, unichar_isalnum, false);

// ── hex / fromhex ─────────────────────────────────────────────────────

fn hex_digit(n: u8) -> u8 {
    if n > 9 {
        n + b'a' - 10
    } else {
        n + b'0'
    }
}

/// `bytesHexImpl` (`objstr.zig:1975-2007`) as the `bytes.hex`
/// method (`bytes_hex_as_str`, `objstr.zig:2009-2011`): the result
/// is a str — EXCEPT the empty case, where the Zig's early return
/// ignores `type_in` and yields the empty BYTES singleton
/// (`b''.hex()` is `b''`, probed on the reference binary). `sep`
/// is a single byte taken from the sep arg's data (an empty sep
/// reads the NUL terminator, i.e. sep byte 0 — probed); a non-str
/// sep raises "can't convert to str implicitly" where the original
/// crashes reading a small int as a pointer.
pub fn bytes_hex(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let data = buffer_data(ctx, types, args[0])?;
    if data.is_empty() {
        return Ok(new_empty_bytes(ctx, types));
    }
    let mut out_len = data.len() * 2;
    let mut sep_byte: u8 = 0;
    let mut has_sep = false;
    if args.len() > 1 {
        out_len += data.len() - 1;
        let sep_data = str_data(ctx.arena, args[1]).ok_or(StrError::BadImplicitConversion)?;
        sep_byte = sep_data.first().copied().unwrap_or(0);
        has_sep = true;
    }
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    for (i, &b) in data.iter().enumerate() {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0xf));
        if has_sep && i + 1 != data.len() {
            out.push(sep_byte);
        }
    }
    Ok(new_str_of_type(ctx, types, types.str_, &out))
}

/// `bytesFromHexImpl` (`objstr.zig:2013-2061`) as the
/// `bytes.fromhex` classmethod (`mp_obj_bytes_fromhex_var`):
/// `args = [cls, data, sep?]`. Whitespace is skipped only when
/// looking for a pair start (and the sep byte likewise); a
/// non-hex digit, or an odd trailing digit, raises `ValueError:
/// non-hex digit`; a multi-char sep raises `ValueError: sep must
/// be a single character`. The result has the class's type.
pub fn bytes_fromhex(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let cls = match args[0] {
        Value::Heap(r) if matches!(ctx.arena.get(r), crate::objects::value::Obj::Type(_)) => r,
        _ => types.bytes,
    };
    let data = buffer_data(ctx, types, args[1])?;
    let mut sep_byte: Option<u8> = None;
    if args.len() >= 3 {
        let sep_data = buffer_data(ctx, types, args[2])?;
        if sep_data.len() != 1 {
            return Err(StrError::FromHexSepLen);
        }
        sep_byte = Some(sep_data[0]);
    }
    let mut out: Vec<u8> = Vec::with_capacity(data.len() / 2);
    let mut i = 0;
    while i < data.len() {
        let ch1 = data[i] as u32;
        i += 1;
        if unichar_isspace(ch1) {
            continue;
        }
        if sep_byte == Some(ch1 as u8) {
            continue;
        }
        if i >= data.len() || !unichar_isxdigit(ch1) {
            return Err(StrError::NonHexDigit);
        }
        let ch2 = data[i] as u32;
        i += 1;
        if !unichar_isxdigit(ch2) {
            return Err(StrError::NonHexDigit);
        }
        out.push(((unichar_xdigit_value(ch1) << 4) | unichar_xdigit_value(ch2)) as u8);
    }
    Ok(new_str_of_type(ctx, types, cls, &out))
}

// ── print ─────────────────────────────────────────────────────────────

/// `mp_str_print_quoted` (`objstr.zig:581-617`): quote `'` unless
/// the data has a single quote and no double quote (then `"`).
/// Escapes: the quote char → `\"`, backslash → `\\`, printable
/// (0x20–0x7E except 0x7F; str also 0x80–0xFF raw, bytes only
/// <0x80) raw, `\n`/`\r`/`\t` escapes, else `\xHH` (lowercase).
/// Non-ASCII runs of a str are flushed as whole UTF-8 runs (the
/// sink is text — a lone byte >= 0x80 must never be written as a
/// Latin-1 char).
pub fn str_print_quoted(sink: &mut dyn fmt::Write, data: &[u8], is_bytes: bool) {
    let quote = if data.contains(&b'\'') && !data.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let _ = sink.write_char(quote as char);
    let mut run: Vec<u8> = Vec::new();
    let flush = |sink: &mut dyn fmt::Write, run: &mut Vec<u8>| {
        if !run.is_empty() {
            let _ = sink.write_str(&String::from_utf8_lossy(run));
            run.clear();
        }
    };
    for &ch in data {
        if ch == quote {
            flush(sink, &mut run);
            let _ = sink.write_str("\\");
            let _ = sink.write_char(quote as char);
        } else if ch == b'\\' {
            flush(sink, &mut run);
            let _ = sink.write_str("\\\\");
        } else if ch >= 0x20 && ch != 0x7f && (!is_bytes || ch < 0x80) {
            run.push(ch);
        } else if ch == b'\n' {
            flush(sink, &mut run);
            let _ = sink.write_str("\\n");
        } else if ch == b'\r' {
            flush(sink, &mut run);
            let _ = sink.write_str("\\r");
        } else if ch == b'\t' {
            flush(sink, &mut run);
            let _ = sink.write_str("\\t");
        } else {
            flush(sink, &mut run);
            let _ = write!(sink, "\\x{:02x}", ch);
        }
    }
    flush(sink, &mut run);
    let _ = sink.write_char(quote as char);
}

/// `str_print` (`objstr.zig:621-631`): the print slot shared by
/// str and bytes. `PRINT_RAW`, or `PRINT_STR` on a str, prints the
/// data as-is; otherwise (repr, or str-kind on bytes) bytes get
/// the `b` prefix plus the quoted form. (The raw branch writes
/// through the text sink — non-UTF-8 bytes degrade to U+FFFD there,
/// a known limitation of C6's text-sink design; the quoted path
/// is always ASCII-safe.)
pub fn str_print(ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn fmt::Write) {
    let data = str_data(ctx.arena, value).unwrap_or(&[]);
    let is_bytes = is_bytes_value(ctx.arena, value);
    if kind == PrintKind::Raw || (kind == PrintKind::Str && !is_bytes) {
        let _ = sink.write_str(&String::from_utf8_lossy(data));
    } else {
        if is_bytes {
            let _ = sink.write_str("b");
        }
        str_print_quoted(sink, data, is_bytes);
    }
}

// ── TypeData helpers ──────────────────────────────────────────────────

/// `mp_type_str` (full, `STR_UNICODE=1` — `objstr.zig:3647-3683`):
/// flags `0x0000`, slots make_new/print/unary_op/binary_op/
/// subscr/iter/buffer/locals_dict. `dict` is the caller-owned
/// locals dict (C10/D1 fill it — see the module docs); `base`/
/// `mro` are C12's. There is no `make_new` field on the frozen
/// `TypeData` — `str_make_new` (`objstr_methods_c.rs`) is called
/// by C12's type-call path directly.
pub fn str_type_data(name: u32, dict: GcRef) -> TypeData {
    TypeData {
        name,
        flags: 0,
        base: None,
        mro: vec![],
        dict,
        unary_op: Some(mc::str_unary_op),
        binary_op: Some(mc::str_binary_op),
        call: None,
        getiter: None, // TODO(C12): polymorph-iter type
        subscr: Some(mc::str_subscr),
        buffer: Some(mc::str_get_buffer),
        print: Some(str_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}

/// `mp_type_bytes` (`objstr.zig:3708-3736`): flags `0x0000`,
/// slots make_new/print/binary_op/subscr/iter/buffer/locals_dict
/// — NO unary_op slot (`len(bytes)` is served by C6's byte
/// fast path).
pub fn bytes_type_data(name: u32, dict: GcRef) -> TypeData {
    TypeData {
        name,
        flags: 0,
        base: None,
        mro: vec![],
        dict,
        unary_op: None,
        binary_op: Some(mc::str_binary_op),
        call: None,
        getiter: None, // TODO(C12): polymorph-iter type
        subscr: Some(mc::bytes_subscr),
        buffer: Some(mc::str_get_buffer),
        print: Some(str_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}

// ── BuiltinFn wrappers ────────────────────────────────────────────────

/// Adapt a `(ctx, types, args)` method function to the frozen
/// `BuiltinFn` shape (`fn(&mut CallCtx, &[Value])`): `types` comes
/// from the installed-types cell (the VM installs it at startup;
/// C10/D1 drop these wrappers straight into `FunBuiltinData.func`).
/// A specific `StrError` is recorded in the pending-error cell
/// before returning `PyRaise`; an empty `args` (no self —
/// impossible after the VM's sig check) is plain `PyRaise` with no
/// pending error.
macro_rules! builtin_method {
    ($name:ident, $f:path) => {
        pub fn $name(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
            if args.is_empty() {
                return Err(PyError::PyRaise);
            }
            let types = ctx.types;
            $f(ctx, types, args).map_err(slot_raise)
        }
    };
}

builtin_method!(str_find_builtin, crate::objects::strings::objstr_methods_a::str_find);
builtin_method!(str_rfind_builtin, crate::objects::strings::objstr_methods_a::str_rfind);
builtin_method!(str_index_builtin, crate::objects::strings::objstr_methods_a::str_index);
builtin_method!(str_rindex_builtin, crate::objects::strings::objstr_methods_a::str_rindex);
builtin_method!(
    str_startswith_builtin,
    crate::objects::strings::objstr_methods_a::str_startswith
);
builtin_method!(
    str_endswith_builtin,
    crate::objects::strings::objstr_methods_a::str_endswith
);
builtin_method!(str_replace_builtin, crate::objects::strings::objstr_methods_a::str_replace);
builtin_method!(str_join_builtin, crate::objects::strings::objstr_methods_a::str_join);
builtin_method!(str_split_builtin, crate::objects::strings::objstr_methods_a::str_split);
builtin_method!(str_rsplit_builtin, crate::objects::strings::objstr_methods_a::str_rsplit);
builtin_method!(str_strip_builtin, crate::objects::strings::objstr_methods_a::str_strip);
builtin_method!(str_lstrip_builtin, crate::objects::strings::objstr_methods_a::str_lstrip);
builtin_method!(str_rstrip_builtin, crate::objects::strings::objstr_methods_a::str_rstrip);
builtin_method!(str_lower_builtin, str_lower);
builtin_method!(str_upper_builtin, str_upper);
builtin_method!(str_title_builtin, str_title);
builtin_method!(str_capitalize_builtin, str_capitalize);
builtin_method!(str_zfill_builtin, str_zfill);
builtin_method!(str_isspace_builtin, str_isspace);
builtin_method!(str_isalpha_builtin, str_isalpha);
builtin_method!(str_isdigit_builtin, str_isdigit);
builtin_method!(str_isupper_builtin, str_isupper);
builtin_method!(str_islower_builtin, str_islower);
builtin_method!(str_isalnum_builtin, str_isalnum);
builtin_method!(bytes_hex_builtin, bytes_hex);
builtin_method!(bytes_fromhex_builtin, bytes_fromhex);
