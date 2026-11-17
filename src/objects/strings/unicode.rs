//! Unicode character classes + UTF-8 helpers (Lane C8a).
//!
//! Port of `src/support/unicode.zig` (the `unichar_*` ASCII table +
//! `utf8_*` walkers). The table is rebuilt from the SAME `FL_*` /
//! `AT_*` symbolic source as the vendor C array literal, exactly as
//! the Zig does — faithful by construction, no transcription risk.
//!
//! Scope note: only the functions `objstr.zig` (lines 1–2080) actually
//! calls are ported here (`isspace/isalpha/isdigit/isxdigit/isupper/
//! islower/tolower/toupper/xdigit_value`, `utf8_next_char/charlen/
//! ptr_to_index`). `utf8_get_char` has no caller in this lane's range
//! and is NOT ported (C8b's format code may add it if it needs it).

// Attribute flags (verbatim from `py/unicode.c` via `unicode.zig`).
const FL_PRINT: u8 = 0x01;
const FL_SPACE: u8 = 0x02;
const FL_DIGIT: u8 = 0x04;
const FL_ALPHA: u8 = 0x08;
const FL_UPPER: u8 = 0x10;
const FL_LOWER: u8 = 0x20;
const FL_XDIGIT: u8 = 0x40;

const AT_PR: u8 = FL_PRINT;
const AT_SP: u8 = FL_SPACE | FL_PRINT;
const AT_DI: u8 = FL_DIGIT | FL_PRINT | FL_XDIGIT;
const AT_UP: u8 = FL_UPPER | FL_ALPHA | FL_PRINT;
const AT_LO: u8 = FL_LOWER | FL_ALPHA | FL_PRINT;
const AT_UX: u8 = FL_UPPER | FL_ALPHA | FL_PRINT | FL_XDIGIT;
const AT_LX: u8 = FL_LOWER | FL_ALPHA | FL_PRINT | FL_XDIGIT;

/// Attribute table for ASCII characters — identical literal to the C
/// table (`unicode.zig` `attr[128]`).
const ATTR: [u8; 128] = [
    0, 0, 0, 0, 0, 0, 0, 0, //
    0, AT_SP, AT_SP, AT_SP, AT_SP, AT_SP, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, //
    AT_SP, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, //
    AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, //
    AT_DI, AT_DI, AT_DI, AT_DI, AT_DI, AT_DI, AT_DI, AT_DI, //
    AT_DI, AT_DI, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, //
    AT_PR, AT_UX, AT_UX, AT_UX, AT_UX, AT_UX, AT_UX, AT_UP, //
    AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, //
    AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, AT_UP, //
    AT_UP, AT_UP, AT_UP, AT_PR, AT_PR, AT_PR, AT_PR, AT_PR, //
    AT_PR, AT_LX, AT_LX, AT_LX, AT_LX, AT_LX, AT_LX, AT_LO, //
    AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, //
    AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, AT_LO, //
    AT_LO, AT_LO, AT_LO, AT_PR, AT_PR, AT_PR, AT_PR, 0,
];

/// `at(c)` — only reached when `c < 128` (the C `&&` guard).
fn at(c: u32) -> u8 {
    ATTR[c as usize]
}

/// `unichar_isspace` (`unicode.zig`): ASCII whitespace only.
pub fn unichar_isspace(c: u32) -> bool {
    c < 128 && (at(c) & FL_SPACE) != 0
}

/// `unichar_isalpha` (`unicode.zig`): ASCII letters only.
pub fn unichar_isalpha(c: u32) -> bool {
    c < 128 && (at(c) & FL_ALPHA) != 0
}

/// `unichar_isdigit` (`unicode.zig`): ASCII digits only.
pub fn unichar_isdigit(c: u32) -> bool {
    c < 128 && (at(c) & FL_DIGIT) != 0
}

/// `unichar_isxdigit` (`unicode.zig`): ASCII hex digits only.
pub fn unichar_isxdigit(c: u32) -> bool {
    c < 128 && (at(c) & FL_XDIGIT) != 0
}

/// `unichar_isupper` (`unicode.zig`): ASCII uppercase only.
pub fn unichar_isupper(c: u32) -> bool {
    c < 128 && (at(c) & FL_UPPER) != 0
}

/// `unichar_islower` (`unicode.zig`): ASCII lowercase only.
pub fn unichar_islower(c: u32) -> bool {
    c < 128 && (at(c) & FL_LOWER) != 0
}

/// `unichar_isident` (`unicode.zig:137-144`): ASCII identifier
/// character — alpha, digit, or `_`. This was the ONE `unicode.zig`
/// function missing from the C8a port (the C8a `unicode.rs` had
/// the other 10); added by Lane C16.
pub fn unichar_isident(c: u32) -> bool {
    c < 128 && ((at(c) & (FL_ALPHA | FL_DIGIT)) != 0 || c == '_' as u32)
}

/// `unichar_isalnum` (`unicode.zig`): ASCII alpha-or-digit.
/// (`objstr.zig` defines its own `unichar_isalnum_` as
/// `isalpha || isdigit` — the same predicate; this one is used.)
pub fn unichar_isalnum(c: u32) -> bool {
    c < 128 && ((at(c) & (FL_ALPHA | FL_DIGIT)) != 0)
}

/// `unichar_tolower` (`unicode.zig`): ASCII-only; anything else
/// (including every non-ASCII byte) passes through unchanged — this
/// is what makes byte-wise case conversion safe for UTF-8.
pub fn unichar_tolower(c: u32) -> u32 {
    if unichar_isupper(c) {
        c.wrapping_add(0x20)
    } else {
        c
    }
}

/// `unichar_toupper` (`unicode.zig`): ASCII-only, see `tolower`.
pub fn unichar_toupper(c: u32) -> u32 {
    if unichar_islower(c) {
        c.wrapping_sub(0x20)
    } else {
        c
    }
}

/// `unichar_xdigit_value` (`unicode.zig`): `c` is assumed to be a
/// hex digit (`n = c - '0'`; if `n > 9`, fold case and subtract 7).
pub fn unichar_xdigit_value(c: u32) -> u32 {
    let mut n: u32 = c.wrapping_sub(b'0' as u32);
    if n > 9 {
        n &= !(b'a' - b'A') as u32; // ~0x20
        n = n.wrapping_sub((b'A' - (b'9' + 1)) as u32); // 7
    }
    n
}

/// UTF-8 continuation byte (`unicode.zig` `isCont`).
pub fn is_utf8_cont(b: u8) -> bool {
    (b & 0xC0) == 0x80
}

/// `utf8_next_char` (`unicode.zig`): byte offset just past the
/// character starting at `i` (one byte + following continuation
/// bytes). Safe on truncated/invalid input: stops at the slice end.
pub fn utf8_next_char(data: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < data.len() && is_utf8_cont(data[j]) {
        j += 1;
    }
    j
}

/// `utf8_charlen` (`unicode.zig`): number of code points in `data`
/// (counts non-continuation bytes).
pub fn utf8_charlen(data: &[u8]) -> usize {
    data.iter().filter(|b| !is_utf8_cont(**b)).count()
}

/// `utf8_ptr_to_index` (`unicode.zig`): code-point index of the byte
/// offset `ptr` in `data` (counts non-continuation bytes before it).
pub fn utf8_ptr_to_index(data: &[u8], ptr: usize) -> usize {
    data[..ptr.min(data.len())]
        .iter()
        .filter(|b| !is_utf8_cont(**b))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unichar_table_spot_checks() {
        // Verbatim from the vendor table via unicode.zig.
        assert!(unichar_isspace(b' ' as u32));
        assert!(unichar_isspace(b'\t' as u32));
        assert!(unichar_isspace(0x0b));
        assert!(!unichar_isspace(0x85)); // non-ASCII byte: false
        assert!(!unichar_isspace(b'a' as u32));
        assert!(unichar_isalpha(b'Z' as u32));
        assert!(!unichar_isalpha(b'5' as u32));
        assert!(!unichar_isalpha(0xC3)); // non-ASCII byte: false
        assert!(unichar_isdigit(b'0' as u32));
        assert!(unichar_isxdigit(b'f' as u32));
        assert!(unichar_isxdigit(b'F' as u32));
        assert!(!unichar_isxdigit(b'g' as u32));
        assert!(unichar_isupper(b'A' as u32));
        assert!(unichar_islower(b'z' as u32));
        assert!(unichar_isalnum(b'7' as u32));
        assert!(!unichar_isalnum(b' ' as u32));
        assert_eq!(unichar_tolower(b'A' as u32), b'a' as u32);
        assert_eq!(unichar_tolower(0xC3), 0xC3); // non-ASCII: identity
        assert_eq!(unichar_toupper(b'a' as u32), b'A' as u32);
        assert_eq!(unichar_toupper(0xE9), 0xE9); // non-ASCII: identity
        assert_eq!(unichar_xdigit_value(b'0' as u32), 0);
        assert_eq!(unichar_xdigit_value(b'9' as u32), 9);
        assert_eq!(unichar_xdigit_value(b'a' as u32), 10);
        assert_eq!(unichar_xdigit_value(b'F' as u32), 15);
    }

    #[test]
    fn utf8_walkers() {
        // "héllo": h(1B) é(2B) l l o — 5 code points, 6 bytes.
        let s = "héllo".as_bytes();
        assert_eq!(utf8_charlen(s), 5);
        assert_eq!(utf8_next_char(s, 0), 1);
        assert_eq!(utf8_next_char(s, 1), 3); // é is 2 bytes
        assert_eq!(utf8_next_char(s, 3), 4);
        assert_eq!(utf8_ptr_to_index(s, 0), 0);
        assert_eq!(utf8_ptr_to_index(s, 1), 1);
        assert_eq!(utf8_ptr_to_index(s, 3), 2);
        assert_eq!(utf8_ptr_to_index(s, 6), 5);
        assert_eq!(utf8_charlen(b""), 0);
        assert_eq!(utf8_charlen(b"abc"), 3);
    }
}
