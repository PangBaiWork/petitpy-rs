//! Decimal parsing helpers for float formatting: the `parsenum`
//! half of the vendor MicroPython float code — `py/parsenum.c`
//! (`mp_decimal_exp`, `mp_parse_float_internal` + `accept_digit`)
//! as linked in petitpy, where the two exported symbols resolve to
//! petitpy's ZIG implementations (`src/frontend/parsenum.zig`), not
//! the vendor C ones (see `parse_float_internal`).
//!
//! Since Lane C17 this module is also the canonical home of the
//! remaining `parsenum` entry points: `mp_parse_num_base`
//! (`src/frontend/parsenumbase.zig`), `mp_parse_num_integer`,
//! `mp_parse_num_float` / `mp_parse_num_decimal` /
//! `mp_parse_num_complex` (`src/frontend/parsenum.zig`). The
//! `int()` builtin (`objects::primitives::objint`) and the `.mpy`
//! const loader (`runtime::vm_raise::obj_from_entry`) both call
//! these; the parser's own integer helpers
//! (`compiler::parser::nodes::validate_int_literal` /
//! `parse_int_base0`) are NOT these entry points — they mirror the
//! Zig parser's `validateIntLiteral` / `std.fmt.parseInt` shapes
//! (see those docs), and stay where they are.

use crate::vendor::mpz::BigInt;

/// `MANTISSA_MAX` (`parsenum.c`): retain precision while not
/// overflowing the `u64` mantissa.
const MANTISSA_MAX: u64 = 0x1999_9999_9999_9998;
/// `MAX_EXACT_POWER_OF_5` for the double build (`parsenum.c`):
/// largest x so that `5^x` is exactly representable in an `f64`.
const MAX_EXACT_POWER_OF_5: i32 = 22;

/// `mp_decimal_exp` (`parsenum.c`, non-`EXACT` branch — the live one
/// for the `APPROX` build): compute `num * 10^dec_exp`.
///
/// First multiplies by `2^dec_exp` by adding to the binary exponent
/// field (an 11-bit unsigned bit-field, so the addition wraps modulo
/// 2048 — reproduced here with `& 0x7ff`), then multiplies or divides
/// by the corresponding power of 5. Note this is inexact for
/// subnormal `num`: setting the exponent field of a subnormal also
/// materialises the implicit leading 1. That inaccuracy is part of
/// the original's behaviour and is reproduced, not fixed.
pub(crate) fn decimal_exp(num: f64, dec_exp: i32) -> f64 {
    if dec_exp == 0 || num == 0.0 {
        return num;
    }
    let bits = num.to_bits();
    let exp = (bits >> 52) & 0x7ff;
    // C bit-field assignment: (exp + dec_exp) mod 2048.
    let new_exp = ((exp as i64 + dec_exp as i64) & 0x7ff) as u64;
    let bits = (bits & !(0x7ff << 52)) | (new_exp << 52);
    let mut res = f64::from_bits(bits);
    // Use positive exponents when they are more precise than negative.
    if dec_exp < 0 && dec_exp >= -MAX_EXACT_POWER_OF_5 {
        res /= 5f64.powf(-(dec_exp as f64));
    } else {
        res *= 5f64.powf(dec_exp as f64);
    }
    res
}

/// `accept_digit` (`parsenum.c`): ingest one more digit into the
/// parsed mantissa. `in_state`: 0 = integer part, 1 = fraction,
/// 2 = exponent (never passed here — the exponent path does not call
/// this helper).
fn accept_digit(mantissa: u64, dig: u32, exp_extra: &mut i32, in_state: i32) -> u64 {
    if mantissa < MANTISSA_MAX {
        // dec_val won't overflow so keep accumulating.
        if in_state == 1 {
            *exp_extra -= 1;
        }
        10u64 * mantissa + dig as u64
    } else {
        // dec_val might overflow and we anyway can't represent more
        // digits of precision, so ignore the digit and just adjust
        // the exponent.
        if in_state == 0 {
            *exp_extra += 1;
        }
        mantissa
    }
}

/// `mp_parse_float_internal` — the PETITPY (Zig) version
/// (`src/frontend/parsenum.zig`), NOT the vendor C one.
///
/// The scan loop, `accept_digit`, `MANTISSA_MAX` and the exponent
/// bookkeeping are identical to vendor `parsenum.c`. The difference
/// is the final value computation: the Zig port computes the value
/// with libc `strtod` (correctly rounded) on the scanned text, and
/// only falls back to `mp_decimal_exp` (the approximate mantissa *
/// 10^exp) when strtod does not consume the whole text or the text
/// is too long (>= 128 non-`_` chars). The vendor C version always
/// uses `mp_decimal_exp`, which is off by an ulp at times (the Zig
/// comment cites `2.2250738585072014e-308`).
///
/// `mp_format_float` in the reference binary links against this Zig
/// symbol (the vendor `formatfloat.o` leaves `mp_parse_float_internal`
/// undefined), so the parse-back step of the dichotomic search uses
/// correctly-rounded values — and the search lands on different
/// (better) mantissas than the vendor C parse would. A Rust port
/// using only the vendor C parse fails ~110/3440 probe cases in the
/// last digit; with this version it matches.
///
/// Rust's `str::parse::<f64>` is correctly rounded, i.e. it agrees
/// with C `strtod` on every decimal string (both implement correct
/// rounding). The scanned text here contains only digits, `.`,
/// `e`/`E` with an optional sign, and `_` — all handled identically
/// by both parsers.
///
/// Thin wrapper over [`parse_float_internal_with_len`]: the Zig
/// returns a NULL pointer for a dangling exponent (`"1e+"`), which
/// the formatter never produces — the wrapper maps it to `0.0`,
/// exactly as the FMT port did.
pub(crate) fn parse_float_internal(s: &[u8]) -> f64 {
    parse_float_internal_with_len(s).map(|(_, v)| v).unwrap_or(0.0)
}

/// `mp_parse_float_internal` with the consumed length kept
/// (`parsenum.zig:328-360`): returns `(consumed, value)` — the
/// Zig's `[*c]const u8` return is the pointer just past the scan,
/// i.e. `consumed` bytes — or `None` when the scan ends on a
/// dangling exponent (`"1e+"`, `"1e-"`: the Zig returns NULL there).
/// `mp_parse_num_decimal` needs the length; `parse_float_internal`
/// (the formatter's entry) only needs the value.
pub(crate) fn parse_float_internal_with_len(s: &[u8]) -> Option<(usize, f64)> {
    // parse_dec_in_t: 0 = INTG, 1 = FRAC, 2 = EXP.
    let mut in_state: i32 = 0;
    let mut exp_neg = false;
    let mut mantissa: u64 = 0;
    let mut exp_val: i32 = 0;
    let mut exp_extra: i32 = 0;
    let mut trailing_zeros_intg: i32 = 0;
    let mut trailing_zeros_frac: i32 = 0;
    let mut idx: usize = 0;
    let top = s.len();
    while idx < top {
        let dig = s[idx];
        idx += 1;
        if dig.is_ascii_digit() {
            let d = (dig - b'0') as u32;
            if in_state == 2 {
                // Don't overflow exp_val when adding the next digit;
                // instead just truncate it and the resulting float
                // will still be correct, either inf or 0.0.
                if exp_val < (i32::MAX / 2 - 9) / 10 {
                    exp_val = 10 * exp_val + d as i32;
                }
            } else if d == 0 || mantissa >= MANTISSA_MAX {
                // Defer treatment of zeros in the fractional part.
                // Also, once we reach MANTISSA_MAX, treat every
                // additional digit as a trailing zero.
                if in_state == 0 {
                    trailing_zeros_intg += 1;
                } else {
                    trailing_zeros_frac += 1;
                }
            } else {
                // Time to un-defer any trailing zeros. Intg zeros first.
                while trailing_zeros_intg > 0 {
                    mantissa = accept_digit(mantissa, 0, &mut exp_extra, 0);
                    trailing_zeros_intg -= 1;
                }
                while trailing_zeros_frac > 0 {
                    mantissa = accept_digit(mantissa, 0, &mut exp_extra, 1);
                    trailing_zeros_frac -= 1;
                }
                mantissa = accept_digit(mantissa, d, &mut exp_extra, in_state);
            }
        } else if in_state == 0 && dig == b'.' {
            in_state = 1;
        } else if in_state != 2 && (dig | 0x20) == b'e' {
            in_state = 2;
            if idx < top {
                if s[idx] == b'+' {
                    idx += 1;
                } else if s[idx] == b'-' {
                    idx += 1;
                    exp_neg = true;
                }
            }
            if idx == top {
                // Zig returns NULL (dangling exponent); unreachable
                // from mp_format_float.
                return None;
            }
        } else if dig == b'_' {
            continue;
        } else {
            // Unknown character.
            idx -= 1;
            break;
        }
    }

    // Work out the exponent.
    if exp_neg {
        exp_val = -exp_val;
    }
    exp_val += exp_extra + trailing_zeros_intg;

    // The scan above decides what is a number; the value comes from
    // a correctly-rounded parse of the scanned text (Zig: libc
    // strtod; Rust: str::parse, also correctly rounded), falling
    // back to mp_decimal_exp for what strtod does not take whole
    // (or text too long for the 128-byte buffer).
    let scanned = &s[..idx];
    let v = exact_decimal(scanned).unwrap_or_else(|| decimal_exp(mantissa as f64, exp_val));
    Some((idx, v))
}

/// `exactDecimal` (`parsenum.zig`): correctly-rounded parse of
/// `text` (with `_` separators stripped), or `None` when the text
/// is too long for the 128-byte buffer or the parse does not consume
/// it whole. (The `wasi` early-return in Zig does not apply: this
/// port targets the native build.)
fn exact_decimal(text: &[u8]) -> Option<f64> {
    // Strip '_' separators, as the Zig version does into its buffer.
    let mut stripped = Vec::with_capacity(text.len());
    for &c in text {
        if c == b'_' {
            continue;
        }
        // Zig returns null once n + 1 >= 128 while copying, i.e.
        // for >= 128 non-'_' characters.
        if stripped.len() + 1 >= 128 {
            return None;
        }
        stripped.push(c);
    }
    // Rust's float parse is correctly rounded (== libc strtod here)
    // and rejects trailing garbage, matching strtod's end-pointer
    // check. An empty string parses to nothing in Rust but strtod
    // returns 0.0 without consuming; the formatter never produces
    // an empty scan, so treat it as 0.0 to match strtod.
    if stripped.is_empty() {
        return Some(0.0);
    }
    let s = core::str::from_utf8(&stripped).ok()?;
    s.parse::<f64>().ok()
}

// ── Integer entry points (Lane C17) ──────────────────────────────────

/// ASCII whitespace per `unichar_isspace` (vendor `py/unicode.c:119-121`):
/// `c < 128 && FL_SPACE` — exactly `0x09..=0x0d` and `0x20`.
/// The canonical predicate is `objects::strings::unicode::unichar_isspace`
/// (C16); this one-line copy keeps `vendor/` free of `objects/`
/// imports — `parsenum_tests` asserts the two agree on all 256
/// byte values, so they cannot drift silently. `pub(crate)` only
/// for that agreement test.
pub(crate) fn is_space(b: u8) -> bool {
    matches!(b, 0x09..=0x0d | 0x20)
}

/// `mp_parse_num_base` (`src/frontend/parsenumbase.zig:17-52`, the
/// Zig port of `py/parsenumbase.c`): strip an optional `0x` / `0o` /
/// `0b` prefix (case-insensitive) and settle `*base`; returns the
/// prefix length to skip. Base 0 with a leading `0` and no prefix
/// letter becomes base 1 (provokes `ValueError` unless every digit
/// is zero — the Zig's `mp_obj_new_int_from_str_len` re-parse then
/// sees a digit `>= base` and stops immediately); base 0 with no
/// prefix becomes 10. A prefix letter only counts when it matches
/// the requested base.
///
/// The `goto no_prefix` shape is preserved: the `len <= 1` and
/// `c != '0'` paths fall through to the `*base == 0 → 10` tail,
/// while the `c == '0'` path skips the tail (`break :done`).
pub(crate) fn parse_num_base(s: &[u8], base: &mut i32) -> usize {
    if s.len() <= 1 {
        if *base == 0 {
            *base = 10;
        }
        return 0;
    }
    if s[0] == b'0' {
        let c = s[1] | 0x20;
        let b = *base;
        if c == b'x' && (b == 0 || b == 16) {
            *base = 16;
            2
        } else if c == b'o' && (b == 0 || b == 8) {
            *base = 8;
            2
        } else if c == b'b' && (b == 0 || b == 2) {
            *base = 2;
            2
        } else {
            if b == 0 {
                *base = 1;
            }
            0
        }
    } else {
        if *base == 0 {
            *base = 10;
        }
        0
    }
}

/// `mp_parse_num_integer` failure (`parsenum.zig:130-138`):
/// `BadBase` is `ValueError("int() arg 2 must be >= 2 and <= 36")`,
/// `InvalidSyntax` is `ValueError("invalid syntax for integer")`.
/// (With a non-null `lex` the Zig converts the latter to a
/// `SyntaxError` + traceback — the lexer never calls this entry,
/// so the Rust form carries the `ValueError` shape only; see
/// `parse_num_integer`.) The message literals live on
/// `PrimError::message` (`objects::primitives::ops`) — the
/// `objint::parse_int_text` wrapper maps these variants there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseNumIntError {
    BadBase,
    InvalidSyntax,
}

/// `mp_parse_num_integer` (`parsenum.zig:212-270`, `mpParseNumIntegerImpl`):
/// the exact integer denoted by `s` in `base` (0 = auto-detect).
/// Skips leading whitespace and an optional sign, strips the base
/// prefix via [`parse_num_base`], then accumulates digits; `_` is
/// skipped ANYWHERE (including leading/trailing/doubled —
/// `int("_")` is 0: the `_` advances past `str_val_start`, so the
/// no-digits check does not fire); a digit `>= base` ends the digit
/// run without error; trailing whitespace is skipped and anything
/// else left is `InvalidSyntax`.
///
/// The Zig accumulates into an `isize` with a per-step overflow
/// check and re-parses via `mp_obj_new_int_from_str_len` (mpz) on
/// overflow; accumulating straight into a `BigInt` gives the
/// identical value (the C7 `parse_int_text` justification, moved
/// here unchanged with the function).
pub(crate) fn parse_num_integer(s: &[u8], base_in: i32) -> Result<BigInt, ParseNumIntError> {
    let mut base = base_in;
    if (base != 0 && base < 2) || base > 36 {
        return Err(ParseNumIntError::BadBase);
    }
    let top = s.len();
    let mut i = 0;
    while i < top && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < top {
        if s[i] == b'+' {
            i += 1;
        } else if s[i] == b'-' {
            i += 1;
            neg = true;
        }
    }
    i += parse_num_base(&s[i..], &mut base);
    let val_start = i;
    let mut z = BigInt::zero();
    let bbase = BigInt::from_int(base as i64);
    while i < top {
        let mut dig = s[i];
        if dig.is_ascii_digit() {
            dig -= b'0';
        } else if dig == b'_' {
            i += 1;
            continue;
        } else {
            dig |= 0x20;
            if dig.is_ascii_lowercase() {
                dig -= b'a' - 10;
            } else {
                break;
            }
        }
        if dig as i32 >= base {
            break;
        }
        z.mul_assign(&bbase);
        z.add_assign(&BigInt::from_int(dig as i64));
        i += 1;
    }
    if i == val_start {
        return Err(ParseNumIntError::InvalidSyntax);
    }
    while i < top && is_space(s[i]) {
        i += 1;
    }
    if i != top {
        return Err(ParseNumIntError::InvalidSyntax);
    }
    if neg {
        z = -z;
    }
    Ok(z)
}

// ── Decimal entry points (Lane C17) ──────────────────────────────────

/// `mp_parse_num_decimal` result (`parsenum.zig: parseDecimalImpl`).
/// Under the full config `complex_enabled` is true, so the `j`
/// suffix and the two-pass `force_complex` paths are live.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DecimalNum {
    Float(f64),
    Complex { re: f64, im: f64 },
}

/// `mp_parse_num_decimal` failure: `ValueError("invalid syntax for
/// number")` (`parsenum.zig: invalidNumberExc`). (With a non-null
/// `lex` the Zig converts it to a `SyntaxError` — the `.mpy`
/// loader and `float()` pass `lex = NULL`, so the Rust form
/// carries the `ValueError` shape only.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseNumError {
    InvalidNumber,
}

impl ParseNumError {
    /// The TERSE message literal the original raises.
    pub(crate) fn message(&self) -> &'static str {
        "invalid syntax for number"
    }
}

/// `mp_parse_num_decimal` (`parsenum.zig:395-487`, `parseDecimalImpl`):
/// parse a decimal number, optionally with an imaginary part.
/// `allow_imag` enables the `j` suffix (`"j"` alone means `1j`);
/// `force_complex` re-parses a second part after the first, so
/// `"3+4j"` becomes `Complex { re: 3, im: 4 }` (and `"3+"` with no
/// imaginary follow-up is an error). `inf` / `infinity` / `nan`
/// are recognised case-insensitively; the value itself comes from
/// [`parse_float_internal_with_len`] (Zig: libc `strtod`,
/// correctly rounded).
///
/// Full config only: the `complex_enabled = false` (nano) arms —
/// the `j` branch and the two-pass `force_complex` branch
/// comptime-eliding — are dead here and not represented.
pub(crate) fn parse_num_decimal(
    s: &[u8],
    allow_imag: bool,
    force_complex: bool,
) -> Result<DecimalNum, ParseNumError> {
    const STATE_START: u32 = 0;
    const STATE_HAVE_REAL: u32 = 1;
    const STATE_HAVE_IMAG: u32 = 2;
    let top = s.len();
    let mut i = 0;
    // Declared without an initial value, as in the Zig (`dec_val`
    // is assigned by every branch below before any read).
    let mut dec_val: f64;
    let mut dec_real = 0.0f64;
    let mut state: u32 = STATE_START;
    loop {
        let mut dec_neg = false;
        while i < top && is_space(s[i]) {
            i += 1;
        }
        if i < top {
            if s[i] == b'+' {
                i += 1;
            } else if s[i] == b'-' {
                i += 1;
                dec_neg = true;
            }
        }
        let str_val_start = i;
        // inf / infinity / nan / fall through to float-internal.
        // The `str + 2 < top` guard needs at least 3 bytes left.
        if i + 2 < top
            && (s[i] | 0x20) == b'i'
            && (s[i + 1] | 0x20) == b'n'
            && (s[i + 2] | 0x20) == b'f'
        {
            i += 3;
            dec_val = f64::INFINITY;
            // "inity" needs 5 more bytes (`str + 4 < top`).
            if i + 4 < top
                && (s[i] | 0x20) == b'i'
                && (s[i + 1] | 0x20) == b'n'
                && (s[i + 2] | 0x20) == b'i'
                && (s[i + 3] | 0x20) == b't'
                && (s[i + 4] | 0x20) == b'y'
            {
                i += 5;
            }
        } else if i + 2 < top
            && (s[i] | 0x20) == b'n'
            && (s[i + 1] | 0x20) == b'a'
            && (s[i + 2] | 0x20) == b'n'
        {
            i += 3;
            dec_val = f64::NAN;
        } else {
            match parse_float_internal_with_len(&s[i..]) {
                Some((consumed, v)) => {
                    dec_val = v;
                    i += consumed;
                }
                // Dangling exponent ("1e+") — the Zig's NULL return.
                None => return Err(ParseNumError::InvalidNumber),
            }
        }
        // 'j' suffix → mark imaginary.
        if allow_imag && i < top && (s[i] | 0x20) == b'j' {
            if i == str_val_start {
                dec_val = 1.0; // "j" → "1j"
            }
            i += 1;
            state |= STATE_HAVE_IMAG;
        }
        if dec_neg {
            dec_val = -dec_val;
        }
        if i == str_val_start {
            return Err(ParseNumError::InvalidNumber);
        }
        while i < top && is_space(s[i]) {
            i += 1;
        }
        if i != top {
            if force_complex && state == STATE_START {
                // "3+4j" two-pass: captured real, parse imag. (The
                // Zig also resets `dec_val = 0` here, but the next
                // pass reassigns it — inf/nan/float-internal — before
                // any read, so the reset is unobservable; omitted.)
                dec_real = dec_val;
                state |= STATE_HAVE_REAL;
                continue;
            }
            return Err(ParseNumError::InvalidNumber);
        }
        break;
    }
    if state == STATE_HAVE_REAL {
        // "3+" with no imag follow-up.
        return Err(ParseNumError::InvalidNumber);
    }
    if state != STATE_START {
        return Ok(DecimalNum::Complex { re: dec_real, im: dec_val });
    }
    if force_complex {
        return Ok(DecimalNum::Complex { re: dec_val, im: 0.0 });
    }
    Ok(DecimalNum::Float(dec_val))
}

/// `mp_parse_num_float` — vendor `parsenum.h:47-49`: the
/// static-inline wrapper is `mp_parse_num_decimal(..., allow_imag,
/// false, lex)`. Under full, the Zig export of this name is an
/// unused symbol (callers see the inline); the `.mpy` loader calls
/// it with `allow_imag = (obj_type == COMPLEX)`.
pub(crate) fn parse_num_float(s: &[u8], allow_imag: bool) -> Result<DecimalNum, ParseNumError> {
    parse_num_decimal(s, allow_imag, false)
}

/// `mp_parse_num_complex` — vendor `parsenum.h:51-53`:
/// `mp_parse_num_decimal(..., true, true, lex)`. The `complex(str)`
/// builtin's entry point (C18 owns the float/complex types; this
/// is ready for it). No in-tree caller yet — the `.mpy` loader
/// deliberately calls `parse_num_float` (force_complex=false).
#[allow(dead_code)]
pub(crate) fn parse_num_complex(s: &[u8]) -> Result<DecimalNum, ParseNumError> {
    parse_num_decimal(s, true, true)
}

