//! `mpprint.c` mirrors for the `%`-format / `str.format` paths
//! (Lane C8b): `mp_print_strn` (`mpprint.c:73-157`),
//! `mp_print_mp_int` (`mpprint.c:233-360`), `mp_print_float`
//! (`mpprint.c:361-392`), plus the `PF_FLAG` constants (vendor
//! `py/mpprint.h:31-40`). C8c (the vendor `mpprint.c` port) unifies
//! them when it lands; until then these are the single
//! implementation and C8c must keep their observable behaviour (the
//! probe records in `scripts/strings_b_result.txt` pin it).
//!
//! `PRINT_STR`/`PRINT_REPR`/`PRINT_RAW` are vendor `py/obj.h:581-585`
//! (`0`/`1`/`4`).

use std::cmp::Ordering;

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int_maybe, BuiltinTypes};
use crate::objects::strings::objstr_a::StrError;
use crate::objects::strings::vstr::Vstr;
use crate::objects::value::{Obj, Value};
use crate::vendor::formatfloat::format_float_into_buf;
use crate::vendor::mpz::BigInt;

// ── PF_FLAG constants (vendor `py/mpprint.h:31-40`) ───────────────────

pub const PF_LEFT_ADJUST: u32 = 0x001;
pub const PF_SHOW_SIGN: u32 = 0x002;
pub const PF_SPACE_SIGN: u32 = 0x004;
pub const PF_SHOW_PREFIX: u32 = 0x008;
pub const PF_PAD_AFTER_SIGN: u32 = 0x010;
pub const PF_CENTER_ADJUST: u32 = 0x020;
pub const PF_ADD_PERCENT: u32 = 0x040;
pub const PF_SHOW_OCTAL_LETTER: u32 = 0x080;
pub const PF_ALWAYS_DECIMAL: u32 = 0x100;
/// The grouping character (`','` or `'_'`) lives at bit 9
/// (`flags >> 9` is the separator byte, 0 when absent).
pub const PF_SEP_POS: u32 = 9;

// ── `mp_print_strn` (vendor `py/mpprint.c:73-157`) ────────────────────

/// 16 spaces (`pad_spaces`).
const PAD_SPACES: &[u8; 16] = b"                ";
/// `pad_common` (23 bytes): 16 `'0'`, then `'_'`, `'0'`, `'0'`,
/// `'0'`, `','`, `'0'`, `'0'` — arranged so the underscore window
/// (`+12`, 5 bytes: `0000_`) and the comma window (`+17`, 4 bytes:
/// `000,`) are slices of it.
const PAD_COMMON: &[u8; 23] = b"0000000000000000_000,00";

/// Emit `n` pad bytes by repeating the fixed `window` in chunks of
/// `window.len()` (the C prints the same window pointer each chunk
/// — it does NOT rotate).
fn emit_pad(v: &mut Vstr, window: &[u8], mut n: i32) -> Result<(), StrError> {
    while n > 0 {
        let p = n.min(window.len() as i32) as usize;
        v.add_strn(&window[..p])?;
        n -= p as i32;
    }
    Ok(())
}

/// `mp_print_strn` (`mpprint.c:73-157`): print `s` padded to
/// `width` with `fill`, honouring LEFT/CENTER. `width`/`len` are
/// `i32` (the C's `int`); a negative `pad` prints nothing.
pub fn print_strn(
    v: &mut Vstr,
    s: &[u8],
    flags: u32,
    fill: u8,
    width: i32,
) -> Result<(), StrError> {
    let len = s.len() as i32;
    let mut pad = width - len;
    let grouping = (flags >> PF_SEP_POS) as u8;
    // The grouped-zero window bump (`mpprint.c`): when the fill is
    // `'0'` with a grouping separator and `width` is a multiple of
    // the window size, one extra pad char is printed (the result
    // never starts with a grouping character). The bump feeds
    // `pad`, so it runs before the pad amounts are computed.
    if fill == b'0' && grouping != 0 {
        let size = if grouping == b'_' { 5i32 } else { 4i32 };
        if width % size == 0 {
            pad += 1;
        }
    }
    let (mut left_pad, mut right_pad) = (0i32, 0i32);
    if flags & PF_CENTER_ADJUST != 0 {
        left_pad = pad / 2;
        right_pad = pad - left_pad;
    } else if flags & PF_LEFT_ADJUST != 0 {
        right_pad = pad;
    } else {
        left_pad = pad;
    }
    if left_pad <= 0 && right_pad <= 0 {
        if !s.is_empty() {
            v.add_strn(s)?;
        }
        return Ok(());
    }
    // A pad WILL be printed, so `width > len >= 0` and the C's
    // `width % pad_size` is the non-negative one (a negative width
    // can never reach the window slice — the C's pointer arithmetic
    // there is UB for it, and the window is never dereferenced).
    let fill_buf = [fill];
    let window: &[u8] = if fill == 0 || fill == b' ' {
        PAD_SPACES
    } else if fill == b'0' && grouping == 0 {
        &PAD_COMMON[0..16]
    } else if fill == b'0' {
        let (base, size) = if grouping == b'_' {
            (12usize, 5i32)
        } else {
            (17usize, 4i32)
        };
        let mut w = width;
        if w % size == 0 {
            w += 1;
        }
        let off = base + (size as usize - 1 - (w % size) as usize);
        &PAD_COMMON[off..off + size as usize]
    } else {
        &fill_buf
    };
    if left_pad > 0 {
        emit_pad(v, window, left_pad)?;
    }
    if !s.is_empty() {
        v.add_strn(s)?;
    }
    if right_pad > 0 {
        emit_pad(v, window, right_pad)?;
    }
    Ok(())
}

// ── `mp_print_mp_int` (vendor `py/mpprint.c:233-360`) ─────────────────

/// An integer value for `print_mp_int`: a small int, or a heap big
/// int. (`mp_print_mp_int` takes the object and formats it via
/// `mp_obj_int_formatted` — Lane C's `BigInt::as_str`.)
pub enum IntValue {
    I64(i64),
    Big(BigInt),
}

impl IntValue {
    pub fn is_negative(&self) -> bool {
        match self {
            IntValue::I64(n) => *n < 0,
            IntValue::Big(b) => b.cmp_mpz(&BigInt::zero()) == Ordering::Less,
        }
    }

    pub fn as_str(&self, base: u32, prefix: &str, base_char: u8, comma: u8) -> String {
        match self {
            IntValue::I64(n) => BigInt::from_int(*n).as_str(base, prefix, base_char, comma),
            IntValue::Big(b) => b.as_str(base, prefix, base_char, comma),
        }
    }
}

/// The integer value of `v` for printing, or `None` when `v` is not
/// an integer. Heap big ints are checked FIRST: C6's
/// `obj_get_int_maybe` ERRORS (`Overflow`) on an out-of-range heap
/// int, but `%d` must print it (probe: `"%d" % (2**70)` → all 19
/// digits). A bool yields 0/1 (via `obj_get_int_maybe`, which
/// handles `Bool`); a float yields `None` — the C's `mp_obj_get_int`
/// raises `TypeError: can't convert to int` for floats (vendor
/// `py/obj.c:306-313`: no implicit float→int conversion), so
/// `"%o" % 3.0` raises (probed).
pub fn int_value(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    v: Value,
) -> Result<Option<IntValue>, StrError> {
    if let Value::Heap(r) = v {
        if let Obj::Int(b) = ctx.arena.get(r) {
            return Ok(Some(IntValue::Big(b.clone())));
        }
    }
    match obj_get_int_maybe(ctx, types, v).map_err(StrError::Obj)? {
        Some(n) => Ok(Some(IntValue::I64(n))),
        None => Ok(None),
    }
}

/// `mp_print_mp_int` (`mpprint.c:233-360`): `base` is 2/8/10/16
/// only. `width`/`prec` are `i32` (the C's `int`); a negative
/// width/prec is inert (the C's comparisons make it so).
pub fn print_mp_int(
    v: &mut Vstr,
    x: &IntValue,
    base: u32,
    base_char: u8,
    flags_in: u32,
    fill_in: u8,
    width_in: i32,
    prec_in: i32,
) -> Result<(), StrError> {
    let mut flags = flags_in;
    let mut fill = fill_in;
    let mut width = width_in;
    let mut prec = prec_in;
    if flags & (PF_LEFT_ADJUST | PF_CENTER_ADJUST) == 0 && fill == b'0' {
        if prec > width {
            width = prec;
        }
        prec = 0;
    }
    // Prefix: sign char (`+`/space, only for non-negative values)
    // then `0b`/`0o`/`0x` (the octal letter is gated on
    // `PF_SHOW_OCTAL_LETTER`).
    let mut prefix = String::new();
    if !x.is_negative() {
        if flags & PF_SHOW_SIGN != 0 {
            prefix.push('+');
        } else if flags & PF_SPACE_SIGN != 0 {
            prefix.push(' ');
        }
    }
    if flags & PF_SHOW_PREFIX != 0 {
        match base {
            2 => {
                prefix.push('0');
                prefix.push((base_char + b'b' - b'a') as char);
            }
            8 => {
                prefix.push('0');
                if flags & PF_SHOW_OCTAL_LETTER != 0 {
                    prefix.push((base_char + b'o' - b'a') as char);
                }
            }
            16 => {
                prefix.push('0');
                prefix.push((base_char + b'x' - b'a') as char);
            }
            _ => {}
        }
    }
    let prefix_len = prefix.len() as i32;
    let comma = (flags >> PF_SEP_POS) as u8;
    if prec > 1 {
        flags |= PF_PAD_AFTER_SIGN;
    }
    let mut sign: u8 = 0;
    let digits: String;
    if flags & PF_PAD_AFTER_SIGN != 0 {
        // Format without a prefix; a leading `-` is stripped and
        // printed separately (after the sign/prefix, before the pad).
        let s = x.as_str(base, "", base_char, comma);
        match s.strip_prefix('-') {
            Some(rest) => {
                sign = b'-';
                digits = rest.to_string();
            }
            None => digits = s,
        }
    } else {
        digits = x.as_str(base, &prefix, base_char, comma);
    }
    let fmt_size = digits.len() as i32;
    let mut spaces_before = 0i32;
    let mut spaces_after = 0i32;
    if prec > 1 {
        let mut prec_width = fmt_size;
        if prec_width < prec {
            prec_width = prec;
        }
        if flags & PF_PAD_AFTER_SIGN != 0 {
            if sign != 0 {
                prec_width += 1;
            }
            prec_width += prefix_len;
        }
        if prec_width < width {
            if flags & PF_LEFT_ADJUST != 0 {
                spaces_after = width - prec_width;
            } else {
                spaces_before = width - prec_width;
            }
        }
        fill = b'0';
        flags &= !PF_LEFT_ADJUST;
    }
    if spaces_before > 0 {
        print_strn(v, &[], 0, b' ', spaces_before)?;
    }
    if flags & PF_PAD_AFTER_SIGN != 0 {
        if sign != 0 {
            print_strn(v, &[sign], 0, 0, 1)?;
            width -= 1;
        }
        if prefix_len > 0 {
            print_strn(v, prefix.as_bytes(), 0, 0, 1)?;
            width -= prefix_len;
        }
    }
    if prec > 1 {
        width = prec;
    }
    print_strn(v, digits.as_bytes(), flags, fill, width)?;
    if spaces_after > 0 {
        print_strn(v, &[], 0, b' ', spaces_after)?;
    }
    Ok(())
}

// ── `mp_print_float` (vendor `py/mpprint.c:361-392`) ──────────────────

/// `mp_print_float` (`mpprint.c:361-392`): `fmt` is one of
/// `eEfgG`; `prec` is the C's `int` (-1 = default 6). The buffer is
/// 33 bytes, matching the C's `buf[36]` with `sizeof(buf) - 3`.
pub fn print_float(
    v: &mut Vstr,
    f: f64,
    fmt: u8,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<(), StrError> {
    let mut buf = [0u8; 33];
    let sign: u8 = if flags & PF_SHOW_SIGN != 0 {
        b'+'
    } else if flags & PF_SPACE_SIGN != 0 {
        b' '
    } else {
        0
    };
    let len = format_float_into_buf(f, fmt, prec, sign, &mut buf);
    let mut owned: Vec<u8> = buf[..len as usize].to_vec();
    if flags & PF_ALWAYS_DECIMAL != 0
        && !owned.iter().any(|&b| b == b'.' || b == b'e' || b == b'n')
    {
        owned.push(b'.');
        owned.push(b'0');
    }
    if flags & PF_ADD_PERCENT != 0 {
        owned.push(b'%');
    }
    let mut width = width;
    let mut start = 0usize;
    // `buf[0] < '0'` — the first byte is a sign character (space,
    // `+` or `-`; all < `'0'`).
    if flags & PF_PAD_AFTER_SIGN != 0 && !owned.is_empty() && owned[0] < b'0' {
        print_strn(v, &owned[0..1], 0, 0, 1)?;
        width -= 1;
        start = 1;
    }
    print_strn(v, &owned[start..], flags, fill, width)?;
    Ok(())
}

/// `vspPrintFloatPct` (`objstr.zig`): `f * 100` formatted as `'f'`
/// (the `%` itself comes from `mp_print_float`'s `ADD_PERCENT`
/// handling — the caller ORs `PF_ADD_PERCENT` into `flags`).
pub fn print_float_pct(
    v: &mut Vstr,
    f: f64,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<(), StrError> {
    print_float(v, f * 100.0, b'f', flags, fill, width, prec)
}
