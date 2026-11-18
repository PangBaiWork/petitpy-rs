//! `{}`-format (`str.format`) — split out of `objstr_format.rs`
//! by Lane E2e (that file was 868 lines, over the 800 cap).
//! The `%`-format engine (`str_modulo_format`,
//! `str_modulo_format_hook`) stays in `objstr_format.rs`; this
//! file owns the brace engine: `format_helper_into` +
//! `str_format` + `str_format_builtin` (`objstr.zig:2709-3049`).
//! Shared helpers (`str_to_int`, `is_alignment`, `is_type`,
//! `arg_looks_integer`, `arg_looks_numeric`) stay in
//! `objstr_format.rs` and are imported here.

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    obj_get_float, obj_get_int, obj_get_type, BuiltinTypes, ObjError,
};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::slots::PrintKind;
use crate::objects::strings::objstr_a::{
    new_str_via_qstr, slot_raise, str_data_pool, StrError,
};
use crate::objects::strings::objstr_b::new_str_type_from_vstr;
use crate::objects::strings::objstr_format::{
    arg_looks_integer, arg_looks_numeric, is_alignment, is_type, str_to_int,
};
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::strings::objstr_print::*;
use crate::objects::strings::vstr::{Vstr, VstrSink};
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};

// ── `str.format` ──────────────────────────────────────────────────────

/// Look a keyword up in `kwargs` (`npyStrKwargsLookup`,
/// `objstr.zig:339-342` — a map lookup by the field name's qstr).
/// The vendor interns the field name (`qstr_from_strn` CREATES
/// it) and compares ids; Rust compares the name BYTES through
/// the pool instead (the same equality, without interning the
/// field name). The old code required the field name to be a
/// STATIC qstr (`find_static_qstr`) — wrong for real kwargs:
/// a keyword name from source (`foo=`, `text=`, `align=`) is
/// interned by the compiler as a DYNAMIC qstr (not in the
/// static table), so `"{foo}".format(foo="bar")` raised
/// `KeyError: foo` (E2e).
fn kwargs_lookup(
    pool: &crate::runtime::qstr::RuntimeQstr,
    kwargs: &[KwArg],
    field_name: &[u8],
) -> Option<Value> {
    kwargs
        .iter()
        .find(|k| pool.qstr_str(k.name as u16) == field_name)
        .map(|k| k.value)
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
                let Some(v) = kwargs_lookup(ctx.pool, kwargs, &fname[..lookup_end]) else {
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
                    let data = str_data_pool(ctx.arena, ctx.pool, arg)
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
    let data = str_data_pool(ctx.arena, ctx.pool, self_)
        .ok_or(StrError::BadImplicitConversion)?;
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
    str_format(ctx, types, args, &[]).map_err(|e| slot_raise(ctx, e))
}
