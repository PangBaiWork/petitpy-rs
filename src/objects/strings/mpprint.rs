//! `mpprint` — the print-output layer (Lane C8c).
//!
//! Port of `src/objects/strings/mpprint.zig` (511 lines, 0 test
//! blocks), itself a faithful port of vendor `py/mpprint.c`:
//! `mp_print_str` / `mp_print_strn` (the pad/fill/group state
//! machine), `mp_print_int` (uint→string base conversion),
//! `mp_print_mp_int` (objint-coupled), `mp_print_float`
//! (FLOAT=1 in both nano and full), plus the platform/stderr
//! printers `mp_plat_print` / `mp_stderr_print`.
//!
//! Division of labour with C8b's `objstr_print.rs` (DO NOT
//! duplicate it): C8b already ported the vendor C cores
//! (`print_strn` / `print_mp_int` / `print_float` + the `PF_FLAG`
//! constants) in Vstr-bound form. This module is their unification:
//! the three algorithms live here ONCE, as sink-generic cores
//! (`print_strn_core` / `print_mp_int_core` / `print_float_core`
//! over a `PrintSink`), `objstr_print.rs` keeps thin Vstr wrappers
//! with identical signatures, and the `mp_print_*` exports are thin
//! wrappers over the same cores with a `Print` sink. The C8b probe
//! records (`scripts/strings_b_result.txt`) pin the cores'
//! observable behaviour — a refactor, not a rewrite.
//!
//! What is NEW here (not in C8b): `mp_print_str`, `mp_print_int`,
//! `format_float_repr` (the Zig-only repr path, `prec == 99` —
//! shortest round-trip, Ryu digits via Rust's `{:e}`), the `Print`
//! sink abstraction, the `pad_spaces` / `pad_common` tables as the
//! single home (moved out of `objstr_print.rs`), and the two
//! platform printers. `mp_vprintf` / `mp_printf` live in
//! `va_ops.c` — NOT in scope (see the Zig file header).

use std::io::Write as _;

use crate::memory::gc::CallCtx;
use crate::objects::obj::{BuiltinTypes, ObjError};
use crate::objects::strings::objstr_a::StrError;
use crate::objects::strings::objstr_print::{
    int_value, IntValue, PF_ADD_PERCENT, PF_ALWAYS_DECIMAL, PF_CENTER_ADJUST,
    PF_LEFT_ADJUST, PF_PAD_AFTER_SIGN, PF_SEP_POS, PF_SHOW_OCTAL_LETTER,
    PF_SHOW_PREFIX, PF_SHOW_SIGN, PF_SPACE_SIGN,
};
use crate::objects::strings::vstr::Vstr;
use crate::objects::value::Value;
use crate::vendor::formatfloat::format_float_into_buf;

// Re-export the `PF_FLAG` constants so the `mpprint` module is the
// single import site for the whole print layer (they still live in
// `objstr_print.rs`, where C8b put them).
pub use crate::objects::strings::objstr_print::{
    PF_LEFT_ADJUST as PF_FLAG_LEFT_ADJUST, PF_SHOW_SIGN as PF_FLAG_SHOW_SIGN,
    PF_SPACE_SIGN as PF_FLAG_SPACE_SIGN, PF_SHOW_PREFIX as PF_FLAG_SHOW_PREFIX,
    PF_PAD_AFTER_SIGN as PF_FLAG_PAD_AFTER_SIGN,
    PF_CENTER_ADJUST as PF_FLAG_CENTER_ADJUST,
    PF_ADD_PERCENT as PF_FLAG_ADD_PERCENT,
    PF_SHOW_OCTAL_LETTER as PF_FLAG_SHOW_OCTAL_LETTER,
    PF_ALWAYS_DECIMAL as PF_FLAG_ALWAYS_DECIMAL,
    PF_SEP_POS as PF_FLAG_SEP_POS,
};

// ── `Print` — the `mp_print_t` sink ──────────────────────────────────

/// A byte sink for the `mp_print_*` family: the `print_strn`
/// callback of `mp_print_t` (`py/mpprint.h`), as a safe Rust trait
/// (no raw pointers, no unsafe). The C callback returns `void` and
/// cannot fail; the Rust version returns `Result<(), StrError>` so
/// a `Vstr` sink can still report its fixed-buffer / allocation
/// failure. Stdout/stderr sinks ignore I/O errors, exactly as the
/// C does (`output.zig` discards the `write(2)` return).
pub trait PrintSink {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), StrError>;
}

impl PrintSink for Vec<u8> {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), StrError> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

impl PrintSink for Vstr {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), StrError> {
        self.add_strn(bytes)
    }
}

/// `Print` — a `mp_print_t` (`py/mpprint.h: { data, print_strn }`)
/// as a Rust handle: the sink is a `PrintSink` trait object. The
/// five `mp_print_*` functions take `&mut Print` as their first
/// argument, mirroring the Zig's `Print` (`?*const anyopaque`)
/// parameter.
pub struct Print<'a> {
    sink: &'a mut dyn PrintSink,
}

impl<'a> Print<'a> {
    pub fn new(sink: &'a mut dyn PrintSink) -> Self {
        Print { sink }
    }

    /// Emit raw bytes through the sink (the former `npy_pr_emit`
    /// helper — `mpprint.zig:53`, inlined there).
    pub fn emit(&mut self, bytes: &[u8]) -> Result<(), StrError> {
        self.sink.emit(bytes)
    }

    /// The underlying sink, for the shared cores below.
    pub fn sink(&mut self) -> &mut dyn PrintSink {
        self.sink
    }
}

// ── Pad tables (`mpprint.zig:39-48`, byte-identical to mpprint.c) ────

/// 16 spaces (`pad_spaces`).
pub(crate) const PAD_SPACES: &[u8; 16] = b"                ";
/// `pad_common` (23 bytes): 16 `'0'`, then `'_'`, `'0'`, `'0'`,
/// `'0'`, `','`, `'0'`, `'0'` — arranged so the underscore window
/// (`+12`, 5 bytes: `0000_`) and the comma window (`+17`, 4 bytes:
/// `000,`) are slices of it.
pub(crate) const PAD_COMMON: &[u8; 23] = b"0000000000000000_000,00";

/// Emit `n` pad bytes by repeating the fixed `window` in chunks of
/// `window.len()` (the C prints the same window pointer each chunk
/// — it does NOT rotate).
fn emit_pad(
    sink: &mut dyn PrintSink,
    window: &[u8],
    mut n: i32,
) -> Result<(), StrError> {
    while n > 0 {
        let p = n.min(window.len() as i32) as usize;
        sink.emit(&window[..p])?;
        n -= p as i32;
    }
    Ok(())
}

// ── Shared cores (the single implementation) ─────────────────────────

/// `mp_print_strn` (`mpprint.c:73-157` / `mpprint.zig:71-141`):
/// print `s` padded to `width` with `fill`, honouring
/// LEFT/CENTER. Returns the total characters printed (the C's
/// `int` return, which C8b's Vstr wrapper discards).
/// `width`/`len` are `i32` (the C's `int`); a negative `pad`
/// prints nothing.
///
/// The early return (both pads `<= 0`) is C8b's: it keeps the
/// window computation — whose `width % pad_size` is the C's
/// pointer arithmetic, UB for a negative width — out of reach
/// when no pad will print.
pub(crate) fn print_strn_core(
    sink: &mut dyn PrintSink,
    s: &[u8],
    flags: u32,
    fill: u8,
    width: i32,
) -> Result<i32, StrError> {
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
    let mut total = 0i32;
    if left_pad <= 0 && right_pad <= 0 {
        if !s.is_empty() {
            sink.emit(s)?;
            total += len;
        }
        return Ok(total);
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
        total += left_pad;
        emit_pad(sink, window, left_pad)?;
    }
    if !s.is_empty() {
        sink.emit(s)?;
        total += len;
    }
    if right_pad > 0 {
        total += right_pad;
        emit_pad(sink, window, right_pad)?;
    }
    Ok(total)
}

/// `mp_print_mp_int` (`mpprint.c:233-360` / `mpprint.zig:228-353`):
/// `base` is 2/8/10/16 only (the Zig `std.debug.assert`s it).
/// `width`/`prec` are `i32` (the C's `int`); a negative
/// width/prec is inert (the C's comparisons make it so).
/// Returns the total characters printed.
pub(crate) fn print_mp_int_core(
    sink: &mut dyn PrintSink,
    x: &IntValue,
    base: u32,
    base_char: u8,
    flags_in: u32,
    fill_in: u8,
    width_in: i32,
    prec_in: i32,
) -> Result<i32, StrError> {
    debug_assert!(matches!(base, 2 | 8 | 10 | 16));
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
    let mut len = 0i32;
    if spaces_before > 0 {
        len += print_strn_core(sink, &[], 0, b' ', spaces_before)?;
    }
    if flags & PF_PAD_AFTER_SIGN != 0 {
        if sign != 0 {
            len += print_strn_core(sink, &[sign], 0, 0, 1)?;
            width -= 1;
        }
        if prefix_len > 0 {
            len += print_strn_core(sink, prefix.as_bytes(), 0, 0, 1)?;
            width -= prefix_len;
        }
    }
    if prec > 1 {
        width = prec;
    }
    len += print_strn_core(sink, digits.as_bytes(), flags, fill, width)?;
    if spaces_after > 0 {
        len += print_strn_core(sink, &[], 0, b' ', spaces_after)?;
    }
    Ok(len)
}

/// `mp_print_float` (`mpprint.c:361-392` / `mpprint.zig:443-501`):
/// `fmt` is one of `eEfgG`; `prec` is the C's `int` (-1 = default
/// 6). The buffer is 33 bytes, matching the C's `buf[36]` with
/// `sizeof(buf) - 3`. Returns the total characters printed.
///
/// The `prec == 99` branch is the Zig-only repr path
/// (`format_float_repr` below): shortest round-trip digits. No
/// live caller passes 99 yet (the float print slot lands with
/// C18) — the branch is ported verbatim from `mpprint.zig`.
pub(crate) fn print_float_core(
    sink: &mut dyn PrintSink,
    f: f64,
    fmt: u8,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<i32, StrError> {
    let mut buf = [0u8; 33];
    let sign: u8 = if flags & PF_SHOW_SIGN != 0 {
        b'+'
    } else if flags & PF_SPACE_SIGN != 0 {
        b' '
    } else {
        0
    };
    let len = if prec == MP_FLOAT_REPR_PREC && fmt == b'g' && f.is_finite() {
        format_float_repr(f, &mut buf, sign) as i32
    } else {
        format_float_into_buf(f, fmt, prec, sign, &mut buf)
    };
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
    let mut chrs = 0i32;
    // `buf[0] < '0'` — the first byte is a sign character (space,
    // `+` or `-`; all < `'0'`).
    if flags & PF_PAD_AFTER_SIGN != 0 && !owned.is_empty() && owned[0] < b'0' {
        chrs += print_strn_core(sink, &owned[0..1], 0, 0, 1)?;
        width -= 1;
        start = 1;
    }
    chrs += print_strn_core(sink, &owned[start..], flags, fill, width)?;
    Ok(chrs)
}

// ── `format_float_repr` — the Zig-only repr path ─────────────────────

/// repr(float) as CPython prints it: the shortest digits that
/// round-trip, positional when the exponent is in `[-4, 16)`,
/// scientific otherwise (`mpprint.zig`, `formatFloatRepr`).
/// `prec` value asking for "optimal repr" (`formatfloat.h`).
pub const MP_FLOAT_REPR_PREC: i32 = 99;

/// `formatFloatRepr` (`mpprint.zig`): shortest round-trip digits
/// for a FINITE `f` into `buf` (NUL-terminated, like the Zig),
/// returning the length without the NUL. The caller must pass a
/// buffer of at least 33 bytes (the `mp_print_float` buffer); the
/// longest output is ~23 bytes.
///
/// The digit string comes from Rust's `{:e}` (shortest round-trip,
/// same digits Zig's Ryu `{e}` produces — verified by probe:
/// `2.5→2.5e0`, `0.0→0e0`, `1e16→1e16`, `5e-324→5e-324`,
/// `123456789.0→1.23456789e8`). The exponent is re-emitted
/// normalised (`e±NN`, two digits minimum), so any spelling
/// difference in the source exponent cannot leak through.
pub fn format_float_repr(f: f64, buf: &mut [u8], sign: u8) -> usize {
    debug_assert!(f.is_finite(), "format_float_repr takes finite values only");
    let sci = format!("{:e}", f.abs());
    let e_at = sci.find('e').expect("LowerExp always contains 'e'");
    let exp: i32 = sci[e_at + 1..].parse().expect("LowerExp exponent parses");
    let mut digits = [0u8; 24];
    let mut nd = 0usize;
    for &c in sci[..e_at].as_bytes() {
        if c != b'.' {
            digits[nd] = c;
            nd += 1;
        }
    }
    let mut n = 0usize;
    if f.is_sign_negative() {
        buf[n] = b'-';
        n += 1;
    } else if sign != 0 {
        buf[n] = sign;
        n += 1;
    }
    if exp < -4 || exp >= 16 {
        buf[n] = digits[0];
        n += 1;
        if nd > 1 {
            buf[n] = b'.';
            buf[n + 1..n + nd].copy_from_slice(&digits[1..nd]);
            n += nd;
        }
        let e_sign: u8 = if exp < 0 { b'-' } else { b'+' };
        let e_str = format!("e{}{:02}", e_sign as char, exp.unsigned_abs());
        buf[n..n + e_str.len()].copy_from_slice(e_str.as_bytes());
        n += e_str.len();
    } else if exp < 0 {
        let zeros = (-exp - 1) as usize;
        buf[n..n + 2].copy_from_slice(b"0.");
        for i in 0..zeros {
            buf[n + 2 + i] = b'0';
        }
        n += 2 + zeros;
        buf[n..n + nd].copy_from_slice(&digits[..nd]);
        n += nd;
    } else {
        let int_len = (exp + 1) as usize;
        if nd <= int_len {
            buf[n..n + nd].copy_from_slice(&digits[..nd]);
            for i in nd..int_len {
                buf[n + i] = b'0';
            }
            n += int_len;
        } else {
            buf[n..n + int_len].copy_from_slice(&digits[..int_len]);
            buf[n + int_len] = b'.';
            buf[n + int_len + 1..n + nd + 1].copy_from_slice(&digits[int_len..nd]);
            n += nd + 1;
        }
    }
    buf[n] = 0;
    n
}

// ── The five exported functions (`mpprint.zig:65-501`) ───────────────

/// `mp_print_str` (`mpprint.zig:65-69`): emit a byte string,
/// returning its length. (The Zig takes a NUL-terminated `char *`
/// and measures it with `strlen`; the Rust caller passes the
/// slice directly.)
pub fn mp_print_str(print: &mut Print, s: &[u8]) -> Result<i32, StrError> {
    if !s.is_empty() {
        print.emit(s)?;
    }
    Ok(s.len() as i32)
}

/// `mp_print_strn` (`mpprint.zig:71-141`): the pad/fill/group
/// state machine over a `Print` sink. Returns the total
/// characters printed.
pub fn mp_print_strn(
    print: &mut Print,
    s: &[u8],
    flags: u32,
    fill: u8,
    width: i32,
) -> Result<i32, StrError> {
    print_strn_core(print.sink(), s, flags, fill, width)
}

/// `mp_print_int` (`mpprint.zig:143-190`): `usize` → string in
/// `base`, with sign handling. `INT_BUF_SIZE` is
/// `sizeof(isize) * 4` = 32, so a value needing more digits is
/// TRUNCATED to its low 32 digits (the vendor C loop breaks when
/// the buffer fills: `while (b > buf && x != 0)` — a base-2
/// `usize::MAX` prints its low 32 bits). Ported verbatim.
/// `x = -x` on `mp_uint_t` is a wrapping negate (`0 -% x`).
/// `SUPPORT_INT_BASE_PREFIX` is 0 in this build — the prefix
/// branches are dead and omitted, exactly as the Zig omits them.
pub fn mp_print_int(
    print: &mut Print,
    x_in: usize,
    sgn: i32,
    base: i32,
    base_char: i32,
    flags: u32,
    fill: u8,
    width_in: i32,
) -> Result<i32, StrError> {
    let mut x = x_in;
    let mut width = width_in;
    let mut sign: u8 = 0;
    if sgn != 0 {
        if (x as isize) < 0 {
            sign = b'-';
            x = x.wrapping_neg();
        } else if flags & PF_SHOW_SIGN != 0 {
            sign = b'+';
        } else if flags & PF_SPACE_SIGN != 0 {
            sign = b' ';
        }
    }
    const INT_BUF_SIZE: usize = std::mem::size_of::<isize>() * 4;
    let mut buf = [0u8; INT_BUF_SIZE];
    let mut b = INT_BUF_SIZE;
    // The Zig's `@intCast(base)`: a negative base panics there;
    // `try_into` panics here too.
    let ubase: usize = base
        .try_into()
        .expect("mp_print_int: base must be non-negative");
    if x == 0 {
        b -= 1;
        buf[b] = b'0';
    } else {
        loop {
            let mut ch = x % ubase;
            x /= ubase;
            if ch >= 10 {
                ch += (base_char as usize) - 10;
            } else {
                ch += b'0' as usize;
            }
            b -= 1;
            buf[b] = ch as u8;
            if !(b > 0 && x != 0) {
                break;
            }
        }
    }
    let mut len = 0i32;
    if flags & PF_PAD_AFTER_SIGN != 0 {
        if sign != 0 {
            len += mp_print_strn(print, &[sign], flags, fill, 1)?;
            width -= 1;
        }
    } else if sign != 0 && b > 0 {
        b -= 1;
        buf[b] = sign;
    }
    len += mp_print_strn(print, &buf[b..], flags, fill, width)?;
    Ok(len)
}

/// `mp_print_mp_int` (`mpprint.zig:228-353`): the int-format
/// beast, over a `Print` sink. The Zig takes the object directly
/// and converts non-ints with `mp_obj_get_int`; the Rust version
/// takes the same `Value` plus the `CallCtx`/`BuiltinTypes` the
/// conversion needs (`int_value`, C8b's helper — heap big ints
/// first, then the `IntMaybe` slot). A value with no int
/// conversion raises `TypeError: can't convert to int`
/// (`ObjError::IntConversion`, the Zig's `mp_obj_get_int` raise).
pub fn mp_print_mp_int(
    print: &mut Print,
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    x: Value,
    base: u32,
    base_char: u8,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<i32, StrError> {
    let iv = match int_value(ctx, types, x)? {
        Some(iv) => iv,
        // `int_value` returned `None` exactly when
        // `obj_get_int_maybe` found no conversion — the same call
        // `obj_get_int` would raise `IntConversion` for.
        None => return Err(StrError::Obj(ObjError::IntConversion)),
    };
    print_mp_int_core(print.sink(), &iv, base, base_char, flags, fill, width, prec)
}

/// `mp_print_float` (`mpprint.zig:443-501`): float formatting over
/// a `Print` sink, including the `prec == 99` repr dispatch
/// (shortest round-trip via `format_float_repr`). Returns the
/// total characters printed.
pub fn mp_print_float(
    print: &mut Print,
    f: f64,
    fmt: u8,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<i32, StrError> {
    print_float_core(print.sink(), f, fmt, flags, fill, width, prec)
}

// ── Platform/stderr printers (`mpprint.zig:356-391`) ─────────────────

/// `mp_print_t` (`py/mpprint.h`): `{ data, print_strn }`. Both
/// built-in printers have `data = null` (the Zig consts), so the
/// data field is omitted — the `print_strn` fn pointer is the
/// whole value. `PrintSink` is implemented so a `PrintT` can back
/// a `Print`.
#[derive(Clone, Copy)]
pub struct PrintT {
    pub print_strn: fn(&[u8]),
}

impl PrintSink for PrintT {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), StrError> {
        (self.print_strn)(bytes);
        Ok(())
    }
}

/// `mp_plat_print` routes to the platform's stdout
/// (`mp_hal_stdout_tx_strn_cooked`). That stub is a plain
/// `write(1, ...)` (`vendor_stubs.zig:22-24` → `output.zig:16-19`,
/// which discards the `write` return) — so this is a plain
/// stdout write, I/O errors ignored, exactly as the C does.
/// NOTE: the full build's `print()` goes through
/// `mp_sys_stdout_print` (`sys.stdout`, D-lane territory), not
/// this const; this const is the nano/embedding stdout printer.
pub static MP_PLAT_PRINT: PrintT = PrintT {
    print_strn: plat_print_strn,
};

fn plat_print_strn(bytes: &[u8]) {
    let _ = std::io::stdout().write_all(bytes);
}

/// `mp_stderr_print` routes to fd 2 (`write(2, ...)`), used by
/// the full build's `MICROPY_ERROR_PRINTER` so warnings +
/// tracebacks don't interleave with stdout. Faithful to the Zig
/// comment: with `write(2)` the system call is unbuffered, so no
/// flush is required (unlike the vendor unix `fwrite`+`fflush`).
pub static MP_STDERR_PRINT: PrintT = PrintT {
    print_strn: stderr_print_strn,
};

fn stderr_print_strn(bytes: &[u8]) {
    let _ = std::io::stderr().write_all(bytes);
}

/// `mp_plat_print` that consults the embedding host's sink
/// (F1, ADR 022): with a sink installed on this thread the
/// bytes go to the host's write callback as fd 1; otherwise
/// plain stdout (the old behavior). Used by `print()`'s
/// no-file path (`print_impl_with`); `__repl_print__` keeps
/// `MP_PLAT_PRINT` (the REPL never runs under the embedding
/// API, and its echo is a host-stream write, not program
/// output).
pub static MP_PLAT_PRINT_SINK_AWARE: PrintT = PrintT {
    print_strn: plat_print_sink_aware,
};

fn plat_print_sink_aware(bytes: &[u8]) {
    if !crate::support::output::write_sink(1, bytes) {
        plat_print_strn(bytes);
    }
}
