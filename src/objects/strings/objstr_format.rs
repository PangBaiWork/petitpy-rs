//! `%`-format and `str.format` (Lane C8b) — `objstr.zig` lines
//! 2376–3050. The three `mpprint.c` helpers (`mp_print_strn`,
//! `mp_print_mp_int`, `mp_print_float`) live in `objstr_print.rs`;
//! C8c (the vendor `mpprint.c` port) unifies them when it lands.
//!
//! PF_FLAG constants are vendor `py/mpprint.h:31-40` (re-exported
//! from `objstr_print.rs`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    is_float, obj_get_float, obj_get_int, obj_get_type, BuiltinTypes, ObjError,
};
use crate::objects::primitives::ops::is_exact_int as obj_is_int;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::int_from_float;
use crate::objects::slots::PrintKind;
use crate::objects::strings::objstr_a::{
    find_static_qstr, is_bytes_value, is_str_value, new_str_via_qstr, slot_raise, str_data,
    StrError,
};
use crate::objects::strings::objstr_b::{dict_get, new_str_type_from_vstr};
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::strings::objstr_print::*;
use crate::objects::strings::vstr::{Vstr, VstrSink};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};

// ── `str_to_int` (objstr.zig:2376-2387) ───────────────────────────────

/// `str_to_int`: scan a run of ASCII digits from `data[pos..]`,
/// accumulating into `num` (resetting it to 0 when the first byte
/// IS a digit — `width`/`precision` start at -1 and the reset is
/// what makes `"05"` parse as 5, not -95). Returns the new
/// position. Wrapping arithmetic: the original aborts (rc=134,
/// "integer overflow") on i32 overflow (e.g. `"%9999999999d"`,
/// `objstr.zig:2381`) — Rust wraps instead (intentional
/// divergence, same class as C8a's).
pub fn str_to_int(data: &[u8], mut pos: usize, num: &mut i32) -> usize {
    if pos < data.len() && data[pos].is_ascii_digit() {
        *num = 0;
        while pos < data.len() && data[pos].is_ascii_digit() {
            *num = num
                .wrapping_mul(10)
                .wrapping_add((data[pos] - b'0') as i32);
            pos += 1;
        }
    }
    pos
}

// ── `isalignment` / `istype` (objstr.zig:2389-2408) ───────────────────

/// `isalignment`: `<`, `>`, `=`, `^`.
pub fn is_alignment(c: u8) -> bool {
    matches!(c, b'<' | b'>' | b'=' | b'^')
}

/// `istype`: `b c d e E f F g G n o s x X %` (note: NO `i`/`u` —
/// those are only `%`-format conversions).
pub fn is_type(c: u8) -> bool {
    matches!(
        c,
        b'b' | b'c'
            | b'd'
            | b'e'
            | b'E'
            | b'f'
            | b'F'
            | b'g'
            | b'G'
            | b'n'
            | b'o'
            | b's'
            | b'x'
            | b'X'
            | b'%'
    )
}

// ── `arg_looks_integer` / `arg_looks_numeric` / `arg_as_int` ──────────

/// `arg_looks_integer` (`objstr.zig:2403-2405`): bool or exact int.
/// (Bool is NOT an int subclass in petitpy — design doc D8 — but it
/// counts as integer-looking here, which is why
/// `"{:d}".format(True)` → `1` while `"{:s}".format(True)` →
/// `ValueError: bad format string`.)
///
/// NOTE: the Zig calls `objIsInt` (`mp_obj_is_int` — a small int OR
/// an exact heap int). That is C7's `primitives::ops::is_exact_int`,
/// NOT C6's `obj::is_exact_int` (which is heap-only, `isExactType`
/// semantics, and returns false for small ints — using it here made
/// every `"{:d}"` on a small int raise `BadFormatStr`).
pub fn arg_looks_integer(arena: &crate::memory::gc::GcArena, types: &BuiltinTypes, v: Value) -> bool {
    matches!(v, Value::Bool(_)) || obj_is_int(arena, types, v)
}

/// `arg_looks_numeric` (`objstr.zig:2409-2413`): integer-looking or
/// float (complex is `COMPLEX=0`, absent).
pub fn arg_looks_numeric(arena: &crate::memory::gc::GcArena, types: &BuiltinTypes, v: Value) -> bool {
    arg_looks_integer(arena, types, v) || is_float(arena, types, v)
}

/// `arg_as_int` (`objstr.zig:2415-2421`): float → `int(float)`
/// (`NaN` → `ValueError: can't convert NaN to int`, `Inf` →
/// `OverflowError: can't convert inf to int`, via C7's
/// `int_from_float`); anything else is returned unchanged. Only
/// the `%d`/`%i`/`%u` arm uses it — `%o`/`%x`/`%X` do NOT (a float
/// there raises `TypeError: can't convert to int`, probed).
pub fn format_arg_as_int(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
) -> Result<Value, StrError> {
    if is_float(ctx.arena, types, arg) {
        let f = obj_get_float(ctx, types, arg).map_err(StrError::Obj)?;
        return int_from_float(ctx.arena, types, ctx.roots, f).map_err(StrError::Prim);
    }
    Ok(arg)
}

// ── `%`-format ────────────────────────────────────────────────────────

/// `vspPrintHelperIntoTemp` (`objstr.zig`): print `arg` into a
/// temporary vstr (init 16), truncate the printed BYTES to `prec`
/// (`prec < 0` means "no truncation"), then `print_strn` the
/// result into `out` with a space fill. `kind` is `PRINT_STR` /
/// `PRINT_REPR` / `PRINT_RAW` (vendor `py/obj.h:581-585`).
fn print_value_into(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
    kind: PrintKind,
    flags: u32,
    width: i32,
    prec: i32,
    out: &mut Vstr,
) -> Result<(), StrError> {
    let mut tmp = Vstr::new(16);
    {
        let mut sink = VstrSink(&mut tmp);
        obj_print_helper(ctx, types, arg, kind, &mut sink);
    }
    let mut vlen = tmp.len as i32;
    let mut prec = prec;
    if prec < 0 {
        prec = vlen;
    }
    if vlen > prec {
        vlen = prec;
    }
    let data = tmp.as_bytes().to_vec();
    print_strn(out, &data[..vlen as usize], flags, b' ', width)
}

/// `strModuloFormatImpl` (`objstr.zig:2467-2672`): walk the pattern
/// byte-by-byte; on `%` parse a conversion spec (optional dict key,
/// flags, width, precision, type char) and emit the formatted
/// argument into `out`. `args` is the positional tuple (or the
/// single-arg list); `dict` is `Some` only for the dict-rhs form.
/// `arg_i` counts consumed args (the dict-key form sets it to 1).
fn str_modulo_format_impl(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    pattern: &[u8],
    is_bytes: bool,
    args: &[Value],
    dict: Option<Value>,
    out: &mut Vstr,
) -> Result<(), StrError> {
    let n_args = args.len();
    let mut arg_i: i32 = 0;
    let mut p = 0usize;
    let top = pattern.len();
    while p < top {
        let mut arg: Option<Value> = None;
        if pattern[p] != b'%' {
            out.add_byte(pattern[p])?;
            p += 1;
            continue;
        }
        p += 1;
        if p >= top {
            return Err(StrError::BadFormatStr);
        }
        if pattern[p] == b'%' {
            out.add_byte(b'%')?;
            p += 1;
            continue;
        }
        // Dictionary value lookup `%(key)s`.
        if pattern[p] == b'(' {
            let Some(d) = dict else {
                return Err(StrError::FormatNeedsDict);
            };
            arg_i = 1; // consume the single dict argument
            p += 1;
            let key_start = p;
            while p < top && pattern[p] != b')' {
                p += 1;
            }
            if p >= top {
                return Err(StrError::BadFormatStr);
            }
            let key = &pattern[key_start..p];
            let k_obj = new_str_via_qstr(ctx, types, key);
            let Some(v) = dict_get(ctx, types, d, k_obj) else {
                return Err(StrError::KeyError(k_obj));
            };
            arg = Some(v);
            p += 1;
        }
        // Parse flags.
        let mut flags: u32 = 0;
        let mut fill: u8 = b' ';
        let mut alt: u32 = 0;
        while p < top {
            match pattern[p] {
                b'-' => flags |= PF_LEFT_ADJUST,
                b'+' => flags |= PF_SHOW_SIGN,
                b' ' => flags |= PF_SPACE_SIGN,
                b'#' => alt = PF_SHOW_PREFIX,
                b'0' => {
                    flags |= PF_PAD_AFTER_SIGN;
                    fill = b'0';
                }
                _ => break,
            }
            p += 1;
        }
        // Parse width.
        let mut width: i32 = 0;
        if p < top {
            if pattern[p] == b'*' {
                if arg_i as usize >= n_args {
                    return Err(StrError::MoreArgs);
                }
                width = obj_get_int(ctx, types, args[arg_i as usize])
                    .map_err(StrError::Obj)? as i32;
                arg_i += 1;
                p += 1;
            } else {
                p = str_to_int(pattern, p, &mut width);
            }
        }
        // Parse precision.
        let mut prec: i32 = -1;
        if p < top && pattern[p] == b'.' {
            p += 1;
            if p < top {
                if pattern[p] == b'*' {
                    if arg_i as usize >= n_args {
                        return Err(StrError::MoreArgs);
                    }
                    prec = obj_get_int(ctx, types, args[arg_i as usize])
                        .map_err(StrError::Obj)? as i32;
                    arg_i += 1;
                    p += 1;
                } else {
                    prec = 0;
                    p = str_to_int(pattern, p, &mut prec);
                }
            }
        }
        if p >= top {
            return Err(StrError::BadFormatStr);
        }
        // Tuple value lookup (when not a dict key).
        if arg.is_none() {
            if arg_i as usize >= n_args {
                return Err(StrError::MoreArgs);
            }
            arg = Some(args[arg_i as usize]);
            arg_i += 1;
        }
        let arg = arg.unwrap();
        // `nativeBase` (full builds) is the identity here: no user
        // classes / subclass instances exist in Rust yet (C12/C13);
        // documented, not silently dropped.
        let conv = pattern[p];
        match conv {
            b'c' => {
                if is_str_value(ctx.arena, arg) {
                    let data = str_data(ctx.arena, arg).unwrap_or(&[]);
                    if data.len() != 1 {
                        return Err(StrError::PctC);
                    }
                    print_strn(out, &data[..1], flags, b' ', width)?;
                } else if arg_looks_integer(ctx.arena, types, arg) {
                    let n = obj_get_int(ctx, types, arg).map_err(StrError::Obj)?;
                    // i64→i32 `@intCast` (panics in the original on
                    // overflow — `"%c" % (2**40)` aborts, rc=134),
                    // then truncate to one byte. Rust wraps instead.
                    let ch = (n as i32) as u8;
                    print_strn(out, &[ch], flags, b' ', width)?;
                } else {
                    return Err(StrError::IntegerNeeded);
                }
            }
            b'd' | b'i' | b'u' => {
                let a = format_arg_as_int(ctx, types, arg)?;
                let iv = int_value(ctx, types, a)?
                    .ok_or(StrError::Obj(ObjError::IntConversion))?;
                print_mp_int(out, &iv, 10, b'a', flags, fill, width, prec)?;
            }
            b'o' => {
                let mut f = flags;
                if alt != 0 {
                    f |= PF_SHOW_PREFIX | PF_SHOW_OCTAL_LETTER;
                }
                let iv = int_value(ctx, types, arg)?
                    .ok_or(StrError::Obj(ObjError::IntConversion))?;
                print_mp_int(out, &iv, 8, b'a', f, fill, width, prec)?;
            }
            b'r' | b's' => {
                let mut kind = if conv == b'r' {
                    PrintKind::Repr
                } else {
                    PrintKind::Str
                };
                if kind == PrintKind::Str
                    && is_bytes
                    && obj_get_type(ctx.arena, types, arg) == Some(types.bytes)
                {
                    // `b"%s" % b"1"` → the bytes arg prints undecorated.
                    kind = PrintKind::Raw;
                }
                print_value_into(ctx, types, arg, kind, flags, width, prec, out)?;
            }
            b'X' | b'x' => {
                let base_char = if conv == b'X' { b'A' } else { b'a' };
                let iv = int_value(ctx, types, arg)?
                    .ok_or(StrError::Obj(ObjError::IntConversion))?;
                print_mp_int(out, &iv, 16, base_char, flags | alt, fill, width, prec)?;
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                let f = obj_get_float(ctx, types, arg).map_err(StrError::Obj)?;
                print_float(out, f, conv, flags, fill, width, prec)?;
            }
            _ => return Err(StrError::BadFormatStr),
        }
        p += 1;
    }
    if dict.is_none() && arg_i as usize != n_args {
        return Err(StrError::ExtraArgs);
    }
    Ok(())
}

/// `str_modulo_format` (`objstr.zig:2673-2708`, the `%` operator):
/// tuple rhs → its items; dict rhs → `dict = rhs, args = [rhs]`;
/// anything else → `args = [rhs], dict = None`. The NULL-vs-None
/// sentinel is critical (the Zig checks `dict == MP_OBJ_NULL`).
pub fn str_modulo_format(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    pattern: Value,
    rhs: Value,
) -> Result<Value, StrError> {
    let pat_data = str_data(ctx.arena, pattern)
        .ok_or(StrError::BadImplicitConversion)?
        .to_vec();
    let is_bytes = is_bytes_value(ctx.arena, pattern);
    let (args, dict): (Vec<Value>, Option<Value>) = match rhs {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Tuple(items) => (items.clone(), None),
            Obj::Dict(_) => (vec![rhs], Some(rhs)),
            _ => (vec![rhs], None),
        },
        _ => (vec![rhs], None),
    };
    let mut out = Vstr::new(16);
    str_modulo_format_impl(ctx, types, &pat_data, is_bytes, &args, dict, &mut out)?;
    let type_ = obj_get_type(ctx.arena, types, pattern).unwrap_or(types.str_);
    Ok(new_str_type_from_vstr(ctx, types, type_, &mut out))
}

// ── `str.format` ──────────────────────────────────────────────────────

/// Look a keyword up in `kwargs` (`npyStrKwargsLookup`,
/// `objstr.zig:339-342` — a map lookup by the field name's qstr).
/// Kwarg names are qstr ids, so the field name must be a static
/// qstr to match (`find_static_qstr`); a non-static name can never
/// equal a static qstr id's bytes — it is a miss, `KeyError`.
fn kwargs_lookup(kwargs: &[KwArg], field_name: &[u8]) -> Option<Value> {
    let id = find_static_qstr(field_name)?;
    kwargs.iter().find(|k| k.name == id).map(|k| k.value)
}

/// A byte of the format spec, or 0 past the end (the Zig's
/// NUL-terminated spec buffer — `vspNulTerminated`).
fn spec_at(spec: &[u8], i: usize) -> u8 {
    spec.get(i).copied().unwrap_or(0)
}

/// `formatHelperInto` (`objstr.zig:2709-3039`): the `{}`-format
/// walker. `args[0]` is the format string itself; `arg_i` is
/// shared with nested specs (a nested `{...}` inside a format spec
/// consumes args). `kwargs` is the keyword map (empty until
/// C10/C13 pass real kwargs through).
fn format_helper_into(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    s: &[u8],
    arg_i: &mut i32,
    args: &[Value],
    kwargs: &[KwArg],
    out: &mut Vstr,
) -> Result<(), StrError> {
    let mut p = 0usize;
    let top = s.len();
    while p < top {
        // Handle `}` — `}}` → literal `}`, otherwise raise.
        if s[p] == b'}' {
            p += 1;
            if p < top && s[p] == b'}' {
                out.add_byte(b'}')?;
                p += 1;
                continue;
            }
            return Err(StrError::BadFormatStr);
        }
        if s[p] != b'{' {
            out.add_byte(s[p])?;
            p += 1;
            continue;
        }
        p += 1;
        if p < top && s[p] == b'{' {
            out.add_byte(b'{')?;
            p += 1;
            continue;
        }
        // Parse: [field_name] ["!" conv] [":" format_spec] "}".
        let mut field_name: Option<(usize, usize)> = None;
        let mut conversion: u8 = 0;
        let mut format_spec: Option<(usize, usize)> = None;
        if p < top && s[p] != b'}' && s[p] != b'!' && s[p] != b':' {
            let start = p;
            while p < top && s[p] != b'}' && s[p] != b'!' && s[p] != b':' {
                p += 1;
            }
            field_name = Some((start, p));
        }
        if p < top && s[p] == b'!' {
            p += 1;
            if p < top && (s[p] == b'r' || s[p] == b's') {
                conversion = s[p];
                p += 1;
            } else {
                return Err(StrError::BadFormatStr);
            }
        }
        if p < top && s[p] == b':' {
            p += 1;
            if p < top && s[p] != b'}' {
                let spec_start = p;
                let mut nest: i32 = 1;
                while p < top {
                    if s[p] == b'{' {
                        nest += 1;
                    } else if s[p] == b'}' {
                        nest -= 1;
                        if nest == 0 {
                            break;
                        }
                    }
                    p += 1;
                }
                format_spec = Some((spec_start, p));
            }
        }
        if p >= top {
            return Err(StrError::BadFormatStr);
        }
        if s[p] != b'}' {
            return Err(StrError::BadFormatStr);
        }
        // Resolve the argument.
        let mut arg: Value;
        if let Some((fstart, fend)) = field_name {
            let fname = &s[fstart..fend];
            if !fname.is_empty() && fname[0].is_ascii_digit() {
                // Manual index: mixing manual after auto raises.
                if *arg_i > 0 {
                    return Err(StrError::BadFormatStr);
                }
                let mut index: i32 = 0;
                let q = str_to_int(fname, 0, &mut index);
                if index as usize >= args.len() - 1 {
                    return Err(StrError::TupleIndexOutOfRange);
                }
                arg = args[index as usize + 1];
                *arg_i = -1;
                if q < fname.len() {
                    // `.` / `[` remainder (or any other junk).
                    return Err(StrError::AttrsNotSupported);
                }
            } else {
                // Keyword: scan to `.` / `[`, look up the name.
                let mut lookup_end = 0usize;
                while lookup_end < fname.len()
                    && fname[lookup_end] != b'.'
                    && fname[lookup_end] != b'['
                {
                    lookup_end += 1;
                }
                let field_q = new_str_via_qstr(ctx, types, &fname[..lookup_end]);
                let Some(v) = kwargs_lookup(kwargs, &fname[..lookup_end]) else {
                    return Err(StrError::KeyError(field_q));
                };
                arg = v;
                if lookup_end < fname.len() {
                    return Err(StrError::AttrsNotSupported);
                }
            }
        } else {
            // Automatic numbering: mixing auto after manual raises.
            if *arg_i < 0 {
                return Err(StrError::BadFormatStr);
            }
            if *arg_i as usize >= args.len() - 1 {
                return Err(StrError::TupleIndexOutOfRange);
            }
            arg = args[*arg_i as usize + 1];
            *arg_i += 1;
        }
        // Default conversion: `'s'` only when there is NO spec AND
        // no explicit conversion.
        if format_spec.is_none() && conversion == 0 {
            conversion = b's';
        }
        if conversion != 0 {
            let kind = if conversion == b's' {
                PrintKind::Str
            } else {
                PrintKind::Repr
            };
            let mut tmp = Vstr::new(16);
            {
                let mut sink = VstrSink(&mut tmp);
                obj_print_helper(ctx, types, arg, kind, &mut sink);
            }
            arg = new_str_type_from_vstr(ctx, types, types.str_, &mut tmp);
        }
        let mut fill: u8 = 0;
        let mut alignc: u8 = 0;
        let mut width: i32 = -1;
        let mut precision: i32 = -1;
        let mut conv_type: u8 = 0;
        let mut flags: u32 = 0;
        if let Some((spec_start, spec_end)) = format_spec {
            // Recurse into the spec (it may itself contain `{...}`
            // fields, sharing `arg_i`).
            let mut inner = Vstr::new(16);
            format_helper_into(
                ctx,
                types,
                &s[spec_start..spec_end],
                arg_i,
                args,
                kwargs,
                &mut inner,
            )?;
            let spec = inner.as_bytes().to_vec();
            // `__format__` / `nativeBase` (full builds): no user
            // classes or methods exist in Rust yet (C12/C13/C14) —
            // identity, documented.
            let mut sp = 0usize;
            // [[fill]align]
            if is_alignment(spec_at(&spec, sp)) {
                alignc = spec_at(&spec, sp);
                sp += 1;
            } else if spec_at(&spec, sp) != 0 && is_alignment(spec_at(&spec, sp + 1)) {
                fill = spec_at(&spec, sp);
                alignc = spec_at(&spec, sp + 1);
                sp += 2;
            }
            // [sign]
            if spec_at(&spec, sp) == b'+'
                || spec_at(&spec, sp) == b'-'
                || spec_at(&spec, sp) == b' '
            {
                if spec_at(&spec, sp) == b'+' {
                    flags |= PF_SHOW_SIGN;
                } else if spec_at(&spec, sp) == b' ' {
                    flags |= PF_SPACE_SIGN;
                }
                sp += 1;
            }
            // [#]
            if spec_at(&spec, sp) == b'#' {
                flags |= PF_SHOW_PREFIX;
                sp += 1;
            }
            // [0] — NOT consumed here; `str_to_int` eats it as part
            // of the width.
            if spec_at(&spec, sp) == b'0' {
                if alignc == 0 && arg_looks_numeric(ctx.arena, types, arg) {
                    alignc = b'=';
                }
                if fill == 0 {
                    fill = b'0';
                }
            }
            // [width]
            sp = str_to_int(&spec, sp, &mut width);
            // [,_]
            if spec_at(&spec, sp) == b',' || spec_at(&spec, sp) == b'_' {
                flags |= (spec_at(&spec, sp) as u32) << PF_SEP_POS;
                sp += 1;
            }
            // [.precision]
            if spec_at(&spec, sp) == b'.' {
                sp += 1;
                sp = str_to_int(&spec, sp, &mut precision);
            }
            // [type]
            if is_type(spec_at(&spec, sp)) {
                conv_type = spec_at(&spec, sp);
                sp += 1;
            }
            if spec_at(&spec, sp) != 0 {
                return Err(StrError::BadFormatStr);
            }
        }
        if alignc == 0 {
            alignc = if arg_looks_numeric(ctx.arena, types, arg) {
                b'>'
            } else {
                b'<'
            };
        }
        if fill == 0 {
            fill = b' ';
        }
        if flags & (PF_SHOW_SIGN | PF_SPACE_SIGN) != 0 {
            if conv_type == b's' || conv_type == b'c' {
                return Err(StrError::BadFormatStr);
            }
        }
        match alignc {
            b'<' => flags |= PF_LEFT_ADJUST,
            b'=' => flags |= PF_PAD_AFTER_SIGN,
            b'^' => flags |= PF_CENTER_ADJUST,
            _ => {}
        }
        // Integer branch (bool counts — `arg_looks_integer`).
        if arg_looks_integer(ctx.arena, types, arg) {
            match conv_type {
                b'b' => {
                    let iv = int_value(ctx, types, arg)?
                        .ok_or(StrError::Obj(ObjError::IntConversion))?;
                    print_mp_int(out, &iv, 2, b'a', flags, fill, width, 0)?;
                    p += 1;
                    continue;
                }
                b'c' => {
                    let n = obj_get_int(ctx, types, arg).map_err(StrError::Obj)?;
                    let ch = (n as i32) as u8;
                    print_strn(out, &[ch], flags, fill, width)?;
                    p += 1;
                    continue;
                }
                0 | b'n' | b'd' => {
                    let iv = int_value(ctx, types, arg)?
                        .ok_or(StrError::Obj(ObjError::IntConversion))?;
                    print_mp_int(out, &iv, 10, b'a', flags, fill, width, 0)?;
                    p += 1;
                    continue;
                }
                b'o' => {
                    let mut f = flags;
                    if f & PF_SHOW_PREFIX != 0 {
                        f |= PF_SHOW_OCTAL_LETTER;
                    }
                    let iv = int_value(ctx, types, arg)?
                        .ok_or(StrError::Obj(ObjError::IntConversion))?;
                    print_mp_int(out, &iv, 8, b'a', f, fill, width, 0)?;
                    p += 1;
                    continue;
                }
                b'X' | b'x' => {
                    let base_char = if conv_type == b'X' { b'A' } else { b'a' };
                    let iv = int_value(ctx, types, arg)?
                        .ok_or(StrError::Obj(ObjError::IntConversion))?;
                    print_mp_int(out, &iv, 16, base_char, flags, fill, width, 0)?;
                    p += 1;
                    continue;
                }
                b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'%' => {
                    // Fall through to the numeric branch.
                }
                _ => return Err(StrError::BadFormatStr),
            }
        }
        // Numeric branch (int or float).
        if arg_looks_numeric(ctx.arena, types, arg) {
            let mut ct = conv_type;
            if ct == 0 {
                ct = b'g';
            }
            if ct == b'n' {
                ct = b'g';
            }
            match ct {
                b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                    let f = obj_get_float(ctx, types, arg).map_err(StrError::Obj)?;
                    print_float(out, f, ct, flags, fill, width, precision)?;
                    p += 1;
                    continue;
                }
                b'%' => {
                    let f = obj_get_float(ctx, types, arg).map_err(StrError::Obj)?;
                    let pct_flags = flags | PF_ADD_PERCENT;
                    print_float_pct(out, f, pct_flags, fill, width, precision)?;
                    p += 1;
                    continue;
                }
                _ => return Err(StrError::BadFormatStr),
            }
        } else {
            if alignc == b'=' {
                return Err(StrError::BadFormatStr);
            }
            match conv_type {
                0 | b's' => {
                    let data = str_data(ctx.arena, arg)
                        .ok_or(StrError::BadImplicitConversion)?;
                    let mut slen = data.len() as i32;
                    let mut prec = precision;
                    if prec < 0 {
                        prec = slen;
                    }
                    if slen > prec {
                        slen = prec;
                    }
                    print_strn(out, &data[..slen as usize], flags, fill, width)?;
                }
                _ => return Err(StrError::BadFormatStr),
            }
        }
        p += 1;
    }
    Ok(())
}

/// `mpObjStrFormatImpl` (`objstr.zig:3040-3049`): `str.format(...)`.
/// `args[0]` is the format string; `kwargs` is the keyword map
/// (empty until C10/C13 pass real kwargs through). The result takes
/// the format string's type (bytes in, bytes out).
pub fn str_format(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
    kwargs: &[KwArg],
) -> Result<Value, StrError> {
    let self_ = args[0];
    let data = str_data(ctx.arena, self_)
        .ok_or(StrError::BadImplicitConversion)?
        .to_vec();
    let mut arg_i: i32 = 0;
    let mut out = Vstr::new(16);
    format_helper_into(ctx, types, &data, &mut arg_i, args, kwargs, &mut out)?;
    let type_ = obj_get_type(ctx.arena, types, self_).unwrap_or(types.str_);
    Ok(new_str_type_from_vstr(ctx, types, type_, &mut out))
}

/// `str.format` slot wrapper (kwargs are empty until C10/C13 pass
/// them through — the core `str_format` takes them explicitly).
pub fn str_format_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let types = ctx.types;
    str_format(ctx, types, args, &[]).map_err(slot_raise)
}
