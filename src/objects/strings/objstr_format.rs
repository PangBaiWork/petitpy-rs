//! `%`-format and `str.format` (Lane C8b) — `objstr.zig` lines
//! 2376–3050. The three `mpprint.c` helpers (`mp_print_strn`,
//! `mp_print_mp_int`, `mp_print_float`) live in `objstr_print.rs`;
//! C8c (the vendor `mpprint.c` port) unifies them when it lands.
//!
//! PF_FLAG constants are vendor `py/mpprint.h:31-40` (re-exported
//! from `objstr_print.rs`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    is_float, obj_get_float, obj_get_int, obj_get_int_maybe, obj_get_type, BuiltinTypes,
    ObjError,
};
use crate::objects::primitives::ops::is_exact_int as obj_is_int;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::int_from_float;
use crate::objects::objtype::instance::instance_unary_op;
use crate::objects::objtype::seams::with_call_hook_seams;
use crate::objects::slots::{CallHook, PrintKind, UnaryOp};
use crate::objects::strings::objstr_a::{
    is_bytes_value, is_str_value, new_str_via_qstr, str_data_pool, StrError,
};
use crate::objects::strings::objstr_b::{dict_get, new_str_type_from_vstr};
use crate::objects::strings::objstr_print::*;
use crate::objects::strings::vstr::{Vstr, VstrSink};
use crate::objects::value::{Obj, Value};

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

/// Run an instance's `__int__` through the hook (E2e).
/// `Ok(None)` = no `__int__` method (or no hook — the slot
/// path); `Err(PyRaise)` = the method raised (the exception
/// object is stashed on the hook's side channel —
/// `state.pending_exc` — and drains at the VM branch) or
/// returned a non-int (the vendor's bare `TypeError`, rebuilt
/// by `exc_from_pyerror` when nothing is stashed — probed).
fn call_int_maybe_hook(
    ctx: &mut CallCtx,
    arg: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Option<Value>, StrError> {
    let Some(hook) = hook else {
        return Ok(None);
    };
    with_call_hook_seams(hook, |seams| {
        instance_unary_op(ctx, UnaryOp::IntMaybe, arg, seams)
    })
    .map_err(|_| StrError::PyRaise)
}

/// `%d`/`%i`/`%u`/`%o`/`%x`/`%X` int conversion with an optional
/// VM hook for `__int__` dispatch (E2e): `int_value` covers
/// small ints / heap ints / bools; a user-class instance needs
/// its `__int__` method run through the hook (the slot's
/// `instance_unary_op_slot` uses stub seams and cannot run
/// FunBc — so `int(A())` and `"%d" % A()` both raised
/// "can't convert to int" with no hook). No hook (the slot
/// path) → `IntConversion`, as today.
fn modulo_int_value(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<IntValue, StrError> {
    if let Some(iv) = int_value(ctx, types, arg)? {
        return Ok(iv);
    }
    match call_int_maybe_hook(ctx, arg, hook)? {
        Some(v) => int_value(ctx, types, v)?.ok_or(StrError::Obj(ObjError::IntConversion)),
        None => Err(StrError::Obj(ObjError::IntConversion)),
    }
}

/// `*` width / precision int conversion with the same hook
/// (E2e — the vendor's width/precision parse calls
/// `mp_obj_get_int`, which dispatches `__int__`: `"%*d" %
/// (A(), 42)` prints `   42` on the reference).
fn modulo_width_int(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<i64, StrError> {
    match obj_get_int_maybe(ctx, types, arg).map_err(StrError::Obj)? {
        Some(n) => Ok(n),
        None => match call_int_maybe_hook(ctx, arg, hook)? {
            Some(v) => obj_get_int(ctx, types, v).map_err(StrError::Obj),
            None => Err(StrError::Obj(ObjError::IntConversion)),
        },
    }
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
    mut hook: Option<CallHook<'_, '_>>,
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
            let Some(v) = dict_get(ctx, d, k_obj) else {
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
                width = modulo_width_int(ctx, types, args[arg_i as usize], hook.as_deref_mut())?
                    as i32;
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
                    prec = modulo_width_int(ctx, types, args[arg_i as usize], hook.as_deref_mut())?
                        as i32;
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
                    let data = str_data_pool(ctx.arena, ctx.pool, arg).unwrap_or_default();
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
                let iv = modulo_int_value(ctx, types, a, hook.as_deref_mut())?;
                print_mp_int(out, &iv, 10, b'a', flags, fill, width, prec)?;
            }
            b'o' => {
                let mut f = flags;
                if alt != 0 {
                    f |= PF_SHOW_PREFIX | PF_SHOW_OCTAL_LETTER;
                }
                let iv = modulo_int_value(ctx, types, arg, hook.as_deref_mut())?;
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
                let iv = modulo_int_value(ctx, types, arg, hook.as_deref_mut())?;
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
/// The hook-less entry (the slot path — `binary_op_inner`):
/// `%d` on a user instance raises "can't convert to int" here,
/// as it did before E2e (the slot's stub seams cannot run a
/// FunBc `__int__`).
pub fn str_modulo_format(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    pattern: Value,
    rhs: Value,
) -> Result<Value, StrError> {
    str_modulo_format_hook(ctx, types, pattern, rhs, None)
}

/// The hook-carrying `%`-format entry (E2e): the VM's
/// `binary_op_ctx` passes its hook so the `%d`/`%o`/`%x` arms
/// dispatch a user instance's `__int__` through it
/// (`modulo_int_value`).
pub fn str_modulo_format_hook(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    pattern: Value,
    rhs: Value,
    hook: Option<CallHook<'_, '_>>,
) -> Result<Value, StrError> {
    let pat_data = str_data_pool(ctx.arena, ctx.pool, pattern)
        .ok_or(StrError::BadImplicitConversion)?;
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
    str_modulo_format_impl(ctx, types, &pat_data, is_bytes, &args, dict, &mut out, hook)?;
    let type_ = obj_get_type(ctx.arena, types, pattern).unwrap_or(types.str_);
    Ok(new_str_type_from_vstr(ctx, types, type_, &mut out))
}


// The `{}`-format engine lives in `objstr_format_brace.rs`
// (E2e split — this file was 868 lines, over the 800 cap);
// re-exported so existing call sites keep working.
pub use crate::objects::strings::objstr_format_brace::{str_format, str_format_builtin};
