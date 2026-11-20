//! Lane C16 tests: support utilities (smallint, errno, output,
//! readline, qstr_hash verification, unicode full-table).
//!
//! Stream-protocol tests live in `stream_tests.rs` (they need
//! the arena fixture + fake backends).

use super::errno::errno;
use super::output::{write as output_write, OutputSink};
use super::readline::{repl_read_line, readline, CHAR_CTRL_D};
use super::smallint::{small_int_floor_divide, small_int_modulo};
use crate::objects::strings::unicode::{
    unichar_isalnum, unichar_isalpha, unichar_isdigit, unichar_isident, unichar_islower,
    unichar_isspace, unichar_isupper, unichar_isxdigit, unichar_tolower, unichar_toupper,
    unichar_xdigit_value,
};
use crate::qstr_table::qstr_compute_hash;
use std::io::Cursor;

#[test]
fn smallint_matches_python_semantics() {
    // Floored division / modulo (Zig `mp_small_int_floor_divide` /
    // `mp_small_int_modulo`; probed values from the C14 lane).
    assert_eq!(small_int_floor_divide(8, -3), -3);
    assert_eq!(small_int_floor_divide(-8, 3), -3);
    assert_eq!(small_int_floor_divide(8, 3), 2);
    assert_eq!(small_int_floor_divide(-8, -3), 2);
    assert_eq!(small_int_floor_divide(7, -3), -3);
    assert_eq!(small_int_floor_divide(0, -5), 0);
    assert_eq!(small_int_floor_divide(-1, 1), -1);
    assert_eq!(small_int_floor_divide(4611686018427387904, -1), -4611686018427387904);
    assert_eq!(small_int_modulo(8, -3), -1);
    assert_eq!(small_int_modulo(-8, 3), 1);
    assert_eq!(small_int_modulo(8, 3), 2);
    assert_eq!(small_int_modulo(-8, -3), -2);
    assert_eq!(small_int_modulo(7, -3), -2);
    assert_eq!(small_int_modulo(0, 7), 0);
    // Cross-check against the floored definition on a grid.
    for a in -20..=20 {
        for b in [-7, -3, -1, 1, 3, 7] {
            let q = small_int_floor_divide(a, b);
            let r = small_int_modulo(a, b);
            assert_eq!(a, q * b + r, "divmod identity {a} {b}");
            assert!(r == 0 || (r < 0) == (b < 0), "remainder sign {a} {b}");
        }
    }
}

#[test]
fn errno_reads_last_os_error() {
    // A failed open sets ENOENT (2); `errno()` reads it
    // back immediately (no intervening libc call in between).
    // The missing file must sit in an EXISTING directory:
    // a path whose parent is also missing reports
    // "path not found" (3) on Windows instead of ENOENT.
    let missing = std::env::temp_dir().join("petitpy-errno-definitely-not-a-real-file-xyz");
    let _ = std::fs::File::open(&missing);
    assert_eq!(errno(), 2);
    // A successful call does not clear errno, but a fresh failure
    // overwrites it: EACCES (13) vs ENOENT (2) differ, so open a
    // directory as a file for writing... instead check the error
    // object itself carries the same code.
    let e = std::fs::File::open(&missing).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(2));
    assert_eq!(errno(), 2);
}

struct CaptureSink {
    pub calls: Vec<(i32, Vec<u8>)>,
}

impl OutputSink for CaptureSink {
    fn write(&mut self, fd: i32, data: &[u8]) {
        self.calls.push((fd, data.to_vec()));
    }
}

#[test]
fn output_sink_routing() {
    let mut sink = CaptureSink { calls: Vec::new() };
    output_write(1, b"hi", Some(&mut sink));
    output_write(2, b"err", Some(&mut sink));
    assert_eq!(sink.calls, vec![(1, b"hi".to_vec()), (2, b"err".to_vec())]);
    // No sink: fd 1/2 go to stdio (unobservable here — just must
    // not panic), other fds are ignored.
    output_write(1, b"", None);
    output_write(2, b"", None);
    output_write(99, b"ignored", None);
}

#[test]
fn readline_lines_eof_and_empty() {
    let mut out = Vec::new();
    let mut prompt_out = Vec::new();
    // Prompt is written (stdout) and flushed; line returned without \n.
    let mut r = Cursor::new(b"hello\nworld".to_vec());
    assert_eq!(
        readline(&mut r, &mut prompt_out, Some(b">>> "), &mut out),
        0
    );
    assert_eq!(prompt_out, b">>> ");
    assert_eq!(out, b"hello");
    // No trailing newline: partial line accumulates, EOF reported.
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), CHAR_CTRL_D);
    assert_eq!(out, b"world");
    // Immediate EOF.
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), CHAR_CTRL_D);
    assert!(out.is_empty());
    // Empty line is a complete line (status 0, no bytes).
    let mut r = Cursor::new(b"\n\nx\n".to_vec());
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), 0);
    assert!(out.is_empty());
    // Long line: fgets chunking (255-byte reads) is unobservable
    // except in the accumulated bytes.
    let long = vec![b'a'; 600];
    let mut data = long.clone();
    data.push(b'\n');
    let mut r = Cursor::new(data);
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), 0);
    assert_eq!(out, long);
    // Empty prompt writes nothing.
    prompt_out.clear();
    let mut r = Cursor::new(b"q\n".to_vec());
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, Some(b""), &mut out), 0);
    assert!(prompt_out.is_empty());
}

#[test]
fn repl_read_line_semantics() {
    let mut out = Vec::new();
    let mut prompt_out = Vec::new();
    // Prompt goes to STDERR (the caller passes the stderr sink);
    // count INCLUDES the trailing \n, which is stripped in `out`.
    let mut r = Cursor::new(b"abc\ndef".to_vec());
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, Some(b"... "), 256, &mut out),
        4
    );
    assert_eq!(prompt_out, b"... ");
    assert_eq!(out, b"abc");
    // No trailing newline: count is the bytes read.
    out.clear();
    assert_eq!(repl_read_line(&mut r, &mut prompt_out, None, 256, &mut out), 3);
    assert_eq!(out, b"def");
    // EOF: 0, empty.
    out.clear();
    assert_eq!(repl_read_line(&mut r, &mut prompt_out, None, 256, &mut out), 0);
    assert!(out.is_empty());
    // buf_max caps the read (fgets size): 6 bytes max → 5 bytes.
    let mut r = Cursor::new(b"0123456789\n".to_vec());
    out.clear();
    assert_eq!(repl_read_line(&mut r, &mut prompt_out, None, 6, &mut out), 5);
    assert_eq!(out, b"01234");
    // ...and the next call continues where the last stopped.
    out.clear();
    assert_eq!(repl_read_line(&mut r, &mut prompt_out, None, 6, &mut out), 5);
    assert_eq!(out, b"56789");
    out.clear();
    assert_eq!(repl_read_line(&mut r, &mut prompt_out, None, 6, &mut out), 1);
    assert!(out.is_empty()); // the \n itself: stripped, count 1
}

/// A `BufRead` that hands out at most `chunk` bytes per
/// `fill_buf`, modelling a pipe whose writes arrive in pieces.
/// C `fgets` loops over such short reads; the pre-F2
/// `fgets_chunk` returned the first short fill as a chunk.
struct ShortFill<'a> {
    data: &'a [u8],
    pos: usize,
    chunk: usize,
}

impl<'a> std::io::Read for ShortFill<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf
            .len()
            .min(self.chunk)
            .min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl<'a> std::io::BufRead for ShortFill<'a> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        let end = (self.pos + self.chunk).min(self.data.len());
        Ok(&self.data[self.pos..end])
    }

    fn consume(&mut self, amt: usize) {
        self.pos += amt;
    }
}

#[test]
fn f2_fgets_loops_over_short_fills() {
    // A 4000-byte line arriving in 7-byte pipe pieces is ONE
    // `fgets` chunk (4095 limit), not 572 lines — the F2 probe
    // (`total20000`: reference 7 prompts, Rust 9 before the fix).
    let mut data = vec![b'y'; 3999];
    data.push(b'\n');
    data.extend_from_slice(b"print(1)\n");
    let mut r = ShortFill {
        data: &data,
        pos: 0,
        chunk: 7,
    };
    let mut prompt_out = Vec::new();
    let mut out = Vec::new();
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, None, 4096, &mut out),
        4000
    );
    assert_eq!(out.len(), 3999);
    assert!(out.iter().all(|&b| b == b'y'));
    out.clear();
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, None, 4096, &mut out),
        9
    );
    assert_eq!(out, b"print(1)");
    // The same short-fill stream through `readline` (input()):
    // 600-byte line in 11-byte pieces accumulates whole.
    let long = vec![b'a'; 600];
    let mut data = long.clone();
    data.push(b'\n');
    let mut r = ShortFill {
        data: &data,
        pos: 0,
        chunk: 11,
    };
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), 0);
    assert_eq!(out, long);
}

#[test]
fn f2_repl_line_truncates_at_nul() {
    // The Zig side measures the line with `strlen`: `print(1)`
    // then NUL then junk — the reference prints 1 and the junk
    // (already consumed by the `fgets`) never becomes a line.
    let mut r = Cursor::new(b"print(1)\x00junk\nprint(2)\n".to_vec());
    let mut prompt_out = Vec::new();
    let mut out = Vec::new();
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, None, 4096, &mut out),
        8
    );
    assert_eq!(out, b"print(1)");
    out.clear();
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, None, 4096, &mut out),
        9
    );
    assert_eq!(out, b"print(2)");
    // A NUL-first line reads as length 0 (EOF), like the original.
    let mut r = Cursor::new(b"\x00\nprint(2)\n".to_vec());
    out.clear();
    assert_eq!(
        repl_read_line(&mut r, &mut prompt_out, None, 4096, &mut out),
        0
    );
    assert!(out.is_empty());
    // `input()` sees the same `strlen` rule: the line is "ab"
    // and the status is EOF (the Zig `readline` adds the 2 bytes
    // then hits EOF on the next `fgets`).
    let mut r = Cursor::new(b"ab\x00cd\n".to_vec());
    out.clear();
    assert_eq!(readline(&mut r, &mut prompt_out, None, &mut out), CHAR_CTRL_D);
    assert_eq!(out, b"ab");
}

#[test]
fn qstr_hash_matches_zig_formula() {
    // djb2 (`hash * 33 ^ byte`), 16-bit mask, zero bumped to 1
    // (`qstr_hash.zig` / vendor `py/qstr.c:qstr_compute_hash`).
    // The Rust fn is C1's `qstr_compute_hash`; this test pins the
    // formula independently (hand-computed values).
    assert_eq!(qstr_compute_hash(b""), 5381); // 5381 & 0xffff, non-zero
    assert_eq!(qstr_compute_hash(b"a"), 46532); // ((5381*33)^97) & 0xffff
    // Masking: a long input's raw hash exceeds 16 bits.
    let raw = b"micropython".iter().fold(5381u32, |h, &b| {
        (h.wrapping_shl(5).wrapping_add(h)) ^ b as u32
    });
    assert!(raw > 0xffff);
    assert_eq!(qstr_compute_hash(b"micropython"), (raw & 0xffff) as u16);
    // Zero bump: find a short input whose raw hash is 0 mod 2**16
    // and check the result is 1, not 0. Two bytes cannot do it
    // (the needed second byte would have to be >= 256), so solve
    // for the third byte: after [a, b] the hash is h2, and the
    // third byte c = (h2 * 33) & 0xffff must itself be < 256.
    let mut found: Option<Vec<u8>> = None;
    'outer: for a in 0..=255u8 {
        for b in 0..=255u8 {
            let h2 = [a, b].iter().fold(5381u32, |h, &x| {
                (h.wrapping_shl(5).wrapping_add(h)) ^ x as u32
            }) & 0xffff;
            let c = (h2.wrapping_mul(33)) & 0xffff;
            if c < 256 {
                found = Some(vec![a, b, c as u8]);
                break 'outer;
            }
        }
    }
    let input = found.expect("a 3-byte input hashing to 0 exists");
    assert_eq!(qstr_compute_hash(&input), 1);
}

/// Full-table对照：the 11 ASCII `unichar_*` predicates against
/// the vendor table rebuilt from the `FL_*`/`AT_*` symbols
/// (`unicode.zig` `attr[128]`, verified byte-identical to the
/// Rust `ATTR` by the lane's grounding script).
#[test]
fn unicode_full_ascii_table() {
    // Reference table, built from the same symbolic source.
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
    let attr: [u8; 128] = [
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
    for c in 0..128u32 {
        let a = attr[c as usize];
        assert_eq!(unichar_isspace(c), (a & FL_SPACE) != 0, "isspace {c}");
        assert_eq!(unichar_isalpha(c), (a & FL_ALPHA) != 0, "isalpha {c}");
        assert_eq!(unichar_isdigit(c), (a & FL_DIGIT) != 0, "isdigit {c}");
        assert_eq!(unichar_isxdigit(c), (a & FL_XDIGIT) != 0, "isxdigit {c}");
        assert_eq!(
            unichar_isident(c),
            ((a & (FL_ALPHA | FL_DIGIT)) != 0) || c == '_' as u32,
            "isident {c}"
        );
        assert_eq!(
            unichar_isalnum(c),
            (a & (FL_ALPHA | FL_DIGIT)) != 0,
            "isalnum {c}"
        );
        assert_eq!(unichar_isupper(c), (a & FL_UPPER) != 0, "isupper {c}");
        assert_eq!(unichar_islower(c), (a & FL_LOWER) != 0, "islower {c}");
        // tolower/toupper: ASCII case fold, everything else identity.
        let lower = if (a & FL_UPPER) != 0 { c + 0x20 } else { c };
        let upper = if (a & FL_LOWER) != 0 { c - 0x20 } else { c };
        assert_eq!(unichar_tolower(c), lower, "tolower {c}");
        assert_eq!(unichar_toupper(c), upper, "toupper {c}");
    }
    // Non-ASCII: every predicate is false, case folds are identity.
    for c in [128u32, 0xC3, 0xE9, 0x100, u32::MAX] {
        assert!(!unichar_isspace(c));
        assert!(!unichar_isalpha(c));
        assert!(!unichar_isdigit(c));
        assert!(!unichar_isxdigit(c));
        assert!(!unichar_isident(c));
        assert!(!unichar_isalnum(c));
        assert!(!unichar_isupper(c));
        assert!(!unichar_islower(c));
        assert_eq!(unichar_tolower(c), c);
        assert_eq!(unichar_toupper(c), c);
    }
    // xdigit_value (assumes a hex digit).
    for (c, v) in [
        (b'0', 0),
        (b'9', 9),
        (b'a', 10),
        (b'f', 15),
        (b'A', 10),
        (b'F', 15),
    ] {
        assert_eq!(unichar_xdigit_value(c as u32), v);
    }
}
