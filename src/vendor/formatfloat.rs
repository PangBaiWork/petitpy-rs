//! Float formatting: port of vendor MicroPython `py/formatfloat.c`
//! (569 lines) + `py/formatfloat.h` (36 lines), vendor MicroPython
//! `b14d129a1`, as compiled by petitpy (devel HEAD `df7de3a`).
//!
//! Configuration this port targets (petitpy `port/mpconfigport.h`,
//! full config, and `build.zig` C flags):
//! - `MICROPY_FLOAT_IMPL` is `DOUBLE`, so `mp_float_t` is `f64`,
//!   `mp_large_float_t` is `f64` (APPROX build: no larger float),
//!   `mp_large_float_uint_t` is `u64`, `MP_FLOAT_EXP_OFFSET` is 1023.
//! - `MICROPY_FLOAT_FORMAT_IMPL` is `APPROX`, so the live refinement
//!   path is the dichotomic search in the `#else` branch of
//!   `mp_format_float`. The `EXACT` retry block (`#if ... != APPROX`)
//!   and the `FLOAT` (non-double) helper variants are compiled out of
//!   the original and are NOT ported.
//! - The reference build compiles the C with `zig cc`, `-std=c99`,
//!   `-DNDEBUG`, `-DPETITPY_CONFIG_FULL=1` (Debug default `-O0`).
//!   `-DNDEBUG` means the C `assert`s are dormant there; the Rust port
//!   keeps the equivalent checks active (see `format_float_into_buf`).
//!
//! Only the formatting algorithm is ported. The two helpers it needs
//! are `mp_decimal_exp` and `mp_parse_float_internal`, ported in
//! [`super::parsenum`]. NOTE: in the reference binary these resolve
//! to petitpy's ZIG implementations (`src/frontend/parsenum.zig`),
//! not the vendor C ones — the vendor `formatfloat.o` leaves both
//! symbols undefined and the Zig exports satisfy them.
//! `mp_decimal_exp` is algorithm-identical in C and Zig;
//! `mp_parse_float_internal` is NOT: the Zig version computes the
//! final value with libc `strtod` (correctly rounded) and only falls
//! back to `mp_decimal_exp` when strtod does not consume the whole
//! text. This port follows the Zig version (see `parsenum`), because
//! the search in `mp_format_float` parses its own output back and
//! the correctly-rounded parse lands on different mantissas than the
//! vendor C parse. Porting only the vendor C parse fails ~110/3440
//! probe cases in the last digit.
//!
//! Callers (API boundary, read but not ported): Zig
//! `src/objects/strings/mpprint.zig` (`mp_print_float`, 36-byte buffer,
//! passes `buf.len - 3` = 33) and C `py/mpprint.c` (`mp_print_float`,
//! same 36/33), `py/objfloat.c` (repr: `'g'`, prec 99),
//! `py/objcomplex.c` (real/imag parts: `'g'`, prec 99), `py/objstr.c`
//! (`%` formatting and `format()`).
//!
//! Return value / truncation (from `formatfloat.h` and the C code):
//! `format_float_into_buf` returns the number of bytes written,
//! excluding the terminating NUL, including any sign character.
//! The buffer is NUL-terminated. The C code never writes past
//! `buf_size`: `max_digits` / `trail_zeros` are clamped to the buffer
//! and `%f` with a huge exponent switches to `%g` when it would not
//! fit. Callers must pass at least `MIN_BUF_SIZE` (29) bytes.
//!
//! Known inaccuracy, ported exactly: the vendor algorithm is only
//! approximate for subnormal doubles. `mp_decimal_exp` shifts the
//! binary exponent field directly, which is wrong for subnormals
//! (their exponent field is 0, so setting it also materialises the
//! implicit leading 1); the mantissa then overflows `u64` and the
//! C cast `(mp_large_float_uint_t)(mantissa_f + 0.5)` is undefined
//! behaviour in C. What the reference binary actually does is
//! determined by the instruction the compiler emits for that cast:
//! the shipped `formatfloat.o` (both copies in the reference build's
//! `.zig-cache`) contains `vcvttsd2usi`, which yields `u64::MAX` for
//! out-of-range inputs. `c_f64_to_u64` below reproduces exactly that
//! instruction's semantics. The gcc-compiled copy of the same C code
//! resolves the same UB differently (mantissa 0), so a gcc harness
//! is NOT a valid oracle here — the differential probe uses the
//! reference binary first and a `zig cc` harness second
//! (`harness_ziglike`: vendor C formatfloat + the Zig parsenum's
//! strtod parse-back, verified to agree with the binary on all 2940
//! binary-reachable cases). See `scripts/fmt_result.txt`.

/// Magic `prec` value asking for "optimal repr" behaviour
/// (`formatfloat.h`).
use super::parsenum::{decimal_exp, parse_float_internal};

pub const MP_FLOAT_REPR_PREC: i32 = 99;

/// Minimal value expected for `buf_size` (`formatfloat.c`).
pub const MIN_BUF_SIZE: usize = 29;

/// `MAX_MANTISSA_DIGITS` for the double build (`formatfloat.c`).
const MAX_MANTISSA_DIGITS: i32 = 19;
/// `SAFE_MANTISSA_DIGITS` for the double build (`formatfloat.c`).
const SAFE_MANTISSA_DIGITS: i32 = 16;
/// `FPMIN_BUF_SIZE` for the double build (`formatfloat.c`).
const FPMIN_BUF_SIZE: i32 = 7;

// Internal formatting flags (`formatfloat.c`).
const FMT_MODE_E: i32 = 0x01;
const FMT_MODE_G: i32 = 0x02;
const FMT_MODE_F: i32 = 0x04;
const FMT_E_CASE: i32 = 0x20;

/// `MP_FLOAT_EXP_OFFSET` for the double build (`misc.h`).
const MP_FLOAT_EXP_OFFSET: i32 = 1023;

/// C `double` -> `uint64_t` conversion as the reference build
/// performs it.
///
/// The C code does `(mp_large_float_uint_t)(mantissa_f + 0.5)`, which
/// is undefined behaviour when the value is out of `u64` range (only
/// reachable for subnormal inputs, see the module docs). The shipped
/// `formatfloat.o` lowers that cast to `vcvttsd2usi`, whose semantics
/// are: truncate toward zero in range; `u64::MAX` for NaN, negative
/// values, and values above `u64::MAX`. Rust's saturating `as u64`
/// matches that for non-negative finite values and differs only for
/// NaN/negative (Rust gives 0), so those two cases are handled
/// explicitly here to reproduce the instruction exactly.
///
/// `pub(crate)` so the unit tests can pin the semantics directly.
pub(crate) fn c_f64_to_u64(x: f64) -> u64 {
    if x.is_nan() || x < 0.0 {
        u64::MAX
    } else {
        x as u64
    }
}

/// `fp_expval` (`formatfloat.c`, double build): the unbiased binary
/// exponent of `x` (`exp` field minus 1023; -1023 for subnormals).
fn fp_expval(x: f64) -> i32 {
    (((x.to_bits() >> 52) & 0x7ff) as i32) - MP_FLOAT_EXP_OFFSET
}

/// `fp_isless1` (`formatfloat.c`, double build).
fn fp_isless1(x: f64) -> bool {
    x < 1.0
}

/// `fp_iszero` (`formatfloat.c`, double build).
fn fp_iszero(x: f64) -> bool {
    x == 0.0
}

/// `mp_prepend_zeros` (`formatfloat.c`): write `"0."` followed by
/// `cnt` zeros at `buf[s..]`; returns the new write position.
fn prepend_zeros(buf: &mut [u8], mut s: usize, cnt: i32) -> usize {
    buf[s] = b'0';
    s += 1;
    buf[s] = b'.';
    s += 1;
    let mut cnt = cnt;
    while cnt > 0 {
        buf[s] = b'0';
        s += 1;
        cnt -= 1;
    }
    s
}


/// `mp_format_mantissa` (`formatfloat.c`): convert the integer
/// `mantissa` to a string in `buf`, starting at absolute index `s`
/// (`base` is the absolute index of the start of the unsigned
/// representation, i.e. just past any sign character).
///
/// Returns the absolute end index (one past the last written byte,
/// not counting the terminating NUL, which is written at that index).
/// The caller asserts `end - base + 1 <= buf_size`.
fn format_mantissa(
    buf: &mut [u8],
    base: usize,
    mut s: usize,
    mut mantissa: u64,
    mantissa_cap: u64,
    mut num_digits: i32,
    max_exp_zeros: i32,
    mut trailing_zeros: i32,
    mut dec: i32,
    mut e: i32,
    fmt_flags: i32,
) -> usize {
    if mantissa != 0 {
        // If rounding/searching created an extra digit or removed
        // too many, fix mantissa first.
        if mantissa >= mantissa_cap {
            if fmt_flags & FMT_MODE_F != 0 {
                debug_assert!(e >= 0);
                num_digits += 1;
                dec += 1;
            } else {
                mantissa /= 10;
                e += 1;
            }
        }
    }

    // When 'g' format is used, replace small exponents by explicit zeros.
    if (fmt_flags & FMT_MODE_G) != 0 && e != 0 {
        if e >= 0 {
            // If 0 < e < max_exp_zeros, expand positive exponent
            // into trailing zeros.
            if e < max_exp_zeros {
                dec += e;
                if dec >= num_digits {
                    trailing_zeros = dec - (num_digits - 1);
                }
                e = 0;
            }
        } else {
            // If -4 <= e < 0, expand negative exponent without
            // losing significant digits.
            if e >= -4 {
                let mut cnt = 0;
                while e < 0 && mantissa % 10 == 0 {
                    mantissa /= 10;
                    cnt += 1;
                    e += 1;
                }
                num_digits -= cnt;
                s = prepend_zeros(buf, s, cnt - e - 1);
                dec = 255;
                e = 0;
            }
        }
    }

    // Convert the integer mantissa to string.
    let mut m = mantissa;
    for digit in (0..num_digits).rev() {
        let digit_ofs = if digit > dec { digit + 1 } else { digit };
        buf[s + digit_ofs as usize] = b'0' + (m % 10) as u8;
        m /= 10;
    }
    let mut dot = dec >= 255;
    if dec + 1 < num_digits {
        dot = true;
        s += 1;
        buf[s + dec as usize] = b'.';
    }
    // num_digits can go negative here (the 'g' small-exponent branch
    // above subtracts the stripped zero count); C pointer arithmetic
    // then moves `s` backwards, which `wrapping_add_signed` mirrors.
    s = s.wrapping_add_signed(num_digits as isize);

    // Append or remove trailing zeros, as required by format.
    if trailing_zeros != 0 {
        dec -= num_digits - 1;
        while trailing_zeros != 0 {
            trailing_zeros -= 1;
            if dec == 0 {
                buf[s] = b'.';
                s += 1;
                dot = true;
            }
            dec -= 1;
            buf[s] = b'0';
            s += 1;
        }
    }
    if (fmt_flags & FMT_MODE_G) != 0 {
        // 'g' format requires to remove trailing zeros after the
        // decimal point.
        if dot {
            while buf[s - 1] == b'0' {
                s -= 1;
            }
            if buf[s - 1] == b'.' {
                s -= 1;
            }
        }
    }

    // Append the exponent if needed.
    if (e != 0 || (fmt_flags & FMT_MODE_E) != 0) && (fmt_flags & FMT_MODE_F) == 0 {
        buf[s] = b'E' | (fmt_flags & FMT_E_CASE) as u8;
        s += 1;
        if e >= 0 {
            buf[s] = b'+';
            s += 1;
        } else {
            buf[s] = b'-';
            s += 1;
            e = -e;
        }
        if e >= 100 {
            buf[s] = b'0' + (e / 100) as u8;
            s += 1;
        }
        buf[s] = b'0' + ((e / 10) % 10) as u8;
        s += 1;
        buf[s] = b'0' + (e % 10) as u8;
        s += 1;
    }
    buf[s] = 0;
    s - base
}

/// `mp_format_float` (`formatfloat.c`): format `f` into `buf`.
///
/// `fmt` is one of `f`/`F`, `e`/`E`, `g`/`G` (any other byte behaves
/// like `e`/`E`); `prec` is the precision, or negative for the
/// default of 6, or `MP_FLOAT_REPR_PREC` (99) for the optimal-repr
/// behaviour; `sign` is the sign character to prepend for
/// non-negative values (`b'+'`, `b' '`, or 0 for none) — a negative
/// value always gets `'-'`, and NaN never gets a sign flip.
///
/// Returns the number of bytes written, excluding the terminating
/// NUL, including any sign character. The buffer is NUL-terminated.
///
/// Panics if `buf.len() < MIN_BUF_SIZE` (the C code asserts the same;
/// the reference build compiles with `-DNDEBUG` so the C assert is
/// dormant there, but the invariant is part of the C source and the
/// algorithm relies on it).
pub fn format_float_into_buf(f: f64, fmt: u8, prec: i32, sign: u8, buf: &mut [u8]) -> i32 {
    assert!(
        buf.len() >= MIN_BUF_SIZE,
        "mp_format_float requires buf_size >= MIN_BUF_SIZE (29)"
    );
    // buf_size here is the remaining size, as in C (it is decremented
    // when a sign character is written).
    let mut buf_size = buf.len() as i32;

    // Handle sign.
    let mut f = f;
    let mut sign = sign;
    if f.is_sign_negative() && !f.is_nan() {
        f = -f;
        sign = b'-';
    }
    let mut base = 0usize;
    if sign != 0 {
        buf[0] = sign;
        base = 1;
        buf_size -= 1;
    }

    // Handle inf/nan.
    let uc = fmt & 0x20;
    if f.is_infinite() {
        buf[base] = b'I' ^ uc;
        buf[base + 1] = b'N' ^ uc;
        buf[base + 2] = b'F' ^ uc;
        buf[base + 3] = 0;
        return (base + 3) as i32;
    } else if f.is_nan() {
        buf[base] = b'N' ^ uc;
        buf[base + 1] = b'A' ^ uc;
        buf[base + 2] = b'N' ^ uc;
        buf[base + 3] = 0;
        return (base + 3) as i32;
    }

    // Decode format character.
    let mut fmt_flags = uc as i32; // setup FMT_E_CASE, clear all other bits
    let lofmt = fmt | 0x20; // fmt in lowercase
    if lofmt == b'f' {
        fmt_flags |= FMT_MODE_F;
    } else if lofmt == b'g' {
        fmt_flags |= FMT_MODE_G;
    } else {
        fmt_flags |= FMT_MODE_E;
    }

    // When precision is unspecified, default to 6.
    let mut prec = prec;
    if prec < 0 {
        prec = 6;
    }
    // Use high precision for `repr`, but switch to exponent mode
    // after 16 digits in any case to match CPython behaviour.
    let mut max_exp_zeros = if prec < buf_size - 3 { prec } else { buf_size - 3 };
    if prec == MP_FLOAT_REPR_PREC {
        prec = MAX_MANTISSA_DIGITS;
        max_exp_zeros = 16;
    }

    // Precompute the exact decimal exponent of f, such that
    // abs(e) is a lower bound on abs(power of 10 exponent).
    let mut e = 0i32;
    if !fp_iszero(f) {
        // Approximate power of 10 exponent from binary exponent.
        e = (fp_expval(f) as f64 * 0.3010299956639812) as i32;
        let positive_exp = !fp_isless1(f);
        let mut u_base = decimal_exp(1.0, e + positive_exp as i32);
        while (f >= u_base) == positive_exp {
            e += if positive_exp { 1 } else { -1 };
            u_base = decimal_exp(1.0, e + positive_exp as i32);
        }
    }

    // For 'e' format, prec is # digits after the decimal.
    // For 'f' format, prec is # digits after the decimal.
    // For 'g' format, prec is the max number of significant digits.
    //
    // For 'e' & 'g' format, there will be a single digit before the decimal.
    // For 'f' format, zeros must be expanded instead of using an exponent.
    // Make sure there is enough room in the buffer for them, or switch
    // to format 'g'.
    if (fmt_flags & FMT_MODE_F) != 0 && e > 0 {
        let req_size = e + prec + 2;
        if req_size > buf_size {
            fmt_flags ^= FMT_MODE_F;
            fmt_flags |= FMT_MODE_G;
            prec += 1;
        }
    }

    // To work independently of the format, we precompute:
    // - the max number of significant digits to produce
    // - the number of leading zeros to prepend (mode f only)
    // - the number of trailing zeros to append
    let mut max_digits = prec;
    let mut lead_zeros = 0i32;
    let mut trail_zeros = 0i32;
    if (fmt_flags & FMT_MODE_F) != 0 {
        if max_digits > buf_size - 3 {
            // Cannot satisfy requested number of decimals given
            // buf_size, sorry.
            max_digits = buf_size - 3;
        }
        if e < 0 {
            if max_digits > 2 && e < -2 {
                // Insert explicit leading zeros.
                lead_zeros = (if -e < max_digits { -e } else { max_digits }) - 2;
                max_digits -= lead_zeros;
            } else {
                max_digits += 1;
            }
        } else {
            max_digits += e + 1;
        }
    } else {
        if (fmt_flags & FMT_MODE_G) == 0 || max_digits == 0 {
            max_digits += 1;
        }
    }
    if max_digits > MAX_MANTISSA_DIGITS {
        // Use trailing zeros to avoid overflowing the mantissa.
        trail_zeros = max_digits - MAX_MANTISSA_DIGITS;
        max_digits = MAX_MANTISSA_DIGITS;
    }
    let overhead = if (fmt_flags & FMT_MODE_F) != 0 {
        3
    } else {
        FPMIN_BUF_SIZE + 1
    };
    if trail_zeros > buf_size - max_digits - overhead {
        // Cannot satisfy requested number of decimals given buf_size,
        // sorry.
        trail_zeros = buf_size - max_digits - overhead;
    }

    // When the caller asks for more precision than available for sure,
    // look for a shorter (rounded) representation first, and only dig
    // into more digits if there is no short representation.
    let mut num_digits = if SAFE_MANTISSA_DIGITS < max_digits {
        SAFE_MANTISSA_DIGITS
    } else {
        max_digits
    };
    let end: usize;
    'try_again: loop {
        let mut s = base;
        let extra_zeros = trail_zeros + (max_digits - num_digits);
        let mut decexp: i32;
        let mut dec: i32 = 0;

        if fp_iszero(f) {
            // No need for scaling 0.0.
            decexp = 0;
        } else if (fmt_flags & FMT_MODE_F) != 0 {
            decexp = num_digits - 1;
            if e < 0 {
                // Negative exponent: we keep a single leading zero in
                // the mantissa, as using more would waste precious
                // digits needed for accuracy.
                if lead_zeros > 0 {
                    // We are using leading zeros.
                    s = prepend_zeros(buf, s, lead_zeros);
                    decexp += lead_zeros + 1;
                    dec = 255; // no decimal dot
                } else {
                    // Small negative exponent, work directly on the
                    // mantissa.
                    dec = 0;
                }
            } else {
                // Positive exponent: we will add trailing zeros
                // separately.
                decexp -= e;
                dec = e;
            }
        } else {
            decexp = num_digits - e - 1;
        }

        // At this point,
        // - buf points to beginning of output buffer for the unsigned
        //   representation
        // - num_digits == the number of mantissa digits to add
        // - (dec + 1) == the number of digits to print before adding
        //   a decimal point
        // - decexp == the power of 10 exponent to apply to f to get
        //   the decimal mantissa
        // - e == the power of 10 exponent to append ('e' or 'g' format)
        let mut mantissa_cap: u64 = 10;
        for _ in 1..num_digits {
            mantissa_cap *= 10;
        }

        // Build the decimal mantissa into a large uint.
        let mut mantissa: u64 = 1;
        if num_digits > SAFE_MANTISSA_DIGITS && decexp > 1 {
            // If we don't have large floats, use integer multiply to
            // produce the last digits. (mp_large_float_t == mp_float_t
            // in the APPROX build, so this branch is always live.)
            if num_digits > SAFE_MANTISSA_DIGITS + 1 && decexp > 2 {
                mantissa = 100;
                decexp -= 2;
            } else {
                mantissa = 10;
                decexp -= 1;
            }
        }
        let mantissa_f = decimal_exp(f, decexp);
        mantissa = mantissa.wrapping_mul(c_f64_to_u64(mantissa_f + 0.5));

        // Finally convert the decimal mantissa to a floating-point
        // string, according to formatting rules.
        let mut reprlen = format_mantissa(
            buf,
            base,
            s,
            mantissa,
            mantissa_cap,
            num_digits,
            max_exp_zeros,
            extra_zeros,
            dec,
            e,
            fmt_flags,
        );
        assert!(reprlen + 1 <= buf_size as usize);

        // APPROX refinement: the initial decimal mantissa might not
        // be completely accurate due to the previous floating point
        // operations. The best way to verify this is to parse the
        // resulting number and compare against the original.
        let mut check = parse_float_internal(&buf[base..base + reprlen]);
        let mut diff = check - f;
        if diff != 0.0 {
            // In order to get the best possible representation, we
            // will perform a dichotomic search for a reversible
            // representation. This will also provide optimal rounding
            // on the fly.
            let mut best_diff = diff;
            let mut best_mantissa = mantissa;
            let mut err_range: u32 = 1;
            if num_digits > SAFE_MANTISSA_DIGITS {
                err_range <<= 3 * (num_digits - SAFE_MANTISSA_DIGITS) as u32;
            }
            let mut maxruns = 3 + 3 * (MAX_MANTISSA_DIGITS - SAFE_MANTISSA_DIGITS);
            while maxruns > 0 {
                maxruns -= 1;
                // Update mantissa according to dichotomic search.
                if diff.is_sign_negative() {
                    mantissa = mantissa.wrapping_add(err_range as u64);
                } else {
                    // Mantissa is expected to always have more
                    // significant digits than err_range.
                    assert!(mantissa >= err_range as u64);
                    mantissa -= err_range as u64;
                }
                // Retry conversion.
                reprlen = format_mantissa(
                    buf,
                    base,
                    s,
                    mantissa,
                    mantissa_cap,
                    num_digits,
                    max_exp_zeros,
                    extra_zeros,
                    dec,
                    e,
                    fmt_flags,
                );
                assert!(reprlen + 1 <= buf_size as usize);
                check = parse_float_internal(&buf[base..base + reprlen]);
                diff = check - f;
                if diff == 0.0 {
                    // We have a perfect match.
                    break;
                }
                // Keep track of our best estimate.
                let delta = diff.abs() - best_diff.abs();
                if delta.is_sign_negative() || (delta == 0.0 && mantissa % 10 == 0) {
                    best_diff = diff;
                    best_mantissa = mantissa;
                }
                // String repr is not perfect: continue a dichotomic
                // improvement.
                if err_range > 1 {
                    err_range >>= 1;
                } else {
                    // We have tried all possible mantissa, without
                    // finding a reversible repr. Check if we have an
                    // alternate precision to try.
                    if num_digits < max_digits {
                        num_digits += 1;
                        continue 'try_again;
                    }
                    // Otherwise, keep the closest one, which is either
                    // the first one or the last one.
                    if mantissa != best_mantissa {
                        // We had a better guess earlier.
                        reprlen = format_mantissa(
                            buf,
                            base,
                            s,
                            best_mantissa,
                            mantissa_cap,
                            num_digits,
                            max_exp_zeros,
                            extra_zeros,
                            dec,
                            e,
                            fmt_flags,
                        );
                    }
                    break;
                }
            }
        }

        end = base + reprlen;
        break 'try_again;
    }

    end as i32
}

/// Convenience wrapper: format `f` with an unbounded (1024-byte)
/// buffer and return the result as a `String`.
///
/// The formatting algorithm is exactly the same as
/// [`format_float_into_buf`]; only the buffer is larger, so the
/// buffer-size clamping that the C algorithm applies (see the module
/// docs) only kicks in for outputs longer than 1024 bytes.
pub fn format_float(f: f64, fmt: u8, prec: i32, sign: u8) -> String {
    let mut buf = [0u8; 1024];
    let len = format_float_into_buf(f, fmt, prec, sign, &mut buf);
    // The output is ASCII only (digits, '.', 'e'/'E', '+', '-', 'I',
    // 'N', 'F' in either case).
    String::from_utf8(buf[..len as usize].to_vec()).expect("format_float output is ASCII")
}
