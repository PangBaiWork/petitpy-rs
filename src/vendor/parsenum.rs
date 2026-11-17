//! Decimal parsing helpers for float formatting: the `parsenum`
//! half of the vendor MicroPython float code — `py/parsenum.c`
//! (`mp_decimal_exp`, `mp_parse_float_internal` + `accept_digit`)
//! as linked in petitpy, where the two exported symbols resolve to
//! petitpy's ZIG implementations (`src/frontend/parsenum.zig`), not
//! the vendor C ones (see `parse_float_internal`).
//!
//! Only what `formatfloat.rs` needs is ported: `mp_decimal_exp`,
//! `mp_parse_float_internal`, and the scan helpers they use. The
//! rest of `parsenum.c` (integer parsing, `mp_parse_float`) is
//! unrelated to formatting and is not ported here.

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
pub(crate) fn parse_float_internal(s: &[u8]) -> f64 {
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
                // C returns NULL; unreachable from mp_format_float.
                return 0.0;
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
    if let Some(v) = exact_decimal(scanned) {
        return v;
    }
    decimal_exp(mantissa as f64, exp_val)
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
