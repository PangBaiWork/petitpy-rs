//! Line reader for `input()` + the REPL (`support/readline.zig` —
//! Zig port of `support/readline_shim.c`).
//!
//! The Zig reads from stdio `FILE*` with `fgets`; the Rust core
//! takes the input as a `BufRead` seam and the prompt destination
//! as a `Write` seam, so both are testable without stdio.
//! `input()`'s builtin wiring is D1's; the REPL loop is Phase
//! E's. `npy_repl_continue` is NOT ported: it delegates to the
//! extern `mp_repl_continue_with_input` (the REPL's bracket /
//! compound-statement check, main.zig's repl logic — Phase E).

use std::io::{BufRead, Write};

/// `readline`'s EOF return (`CHAR_CTRL_D`, 0x04).
pub const CHAR_CTRL_D: i32 = 0x04;

/// One `fgets` chunk: at most `limit` bytes, stopping just after
/// a `\n`. Returns `(visible_bytes, ends_in_newline, chunk)`.
/// A read error or EOF yields an empty chunk.
///
/// C `fgets` loops over short reads (a pipe `read` may return
/// fewer bytes than asked for) until a newline, `limit` bytes,
/// or EOF — so this loops `fill_buf` the same way instead of
/// returning the first short fill as a complete chunk (the
/// pre-F2 bug: a 4000-byte REPL line split at a pipe boundary
/// was exec'd as two lines, prompt counts 9 vs the reference's 7).
/// A read error behaves like `fgets` returning NULL: the partial
/// line is discarded (`readline.zig` zeroes the buffer, too).
/// The Zig side measures the line with `strlen`, so everything
/// from the first NUL byte on is invisible — but the bytes up to
/// the `\n` (or the limit) were already consumed by the `fgets`,
/// exactly as in C, so the returned chunk is truncated at the
/// NUL while the consumption is not. A NUL-first line therefore
/// reads as length 0, which the REPL treats as EOF, like the
/// original (`npy_repl_read_line` returns `strlen` == 0).
fn fgets_chunk(reader: &mut impl BufRead, limit: usize) -> (usize, bool, Vec<u8>) {
    let mut raw: Vec<u8> = Vec::new();
    while raw.len() < limit {
        let (step, done) = {
            let available = match reader.fill_buf() {
                Ok(b) => b,
                Err(_) => return (0, false, Vec::new()),
            };
            if available.is_empty() {
                break; // EOF
            }
            let take = available.len().min(limit - raw.len());
            match available[..take].iter().position(|&b| b == b'\n') {
                Some(pos) => (available[..pos + 1].to_vec(), true),
                None => (available[..take].to_vec(), false),
            }
        };
        let n = step.len();
        raw.extend_from_slice(&step);
        reader.consume(n);
        if done {
            break;
        }
    }
    let visible = match raw.iter().position(|&b| b == 0) {
        Some(pos) => &raw[..pos],
        None => &raw[..],
    };
    let done = visible.last() == Some(&b'\n');
    (visible.len(), done, visible.to_vec())
}

/// `readline(line, prompt)` — write the prompt (if non-empty)
/// to `prompt_out` (stdout, flushed), then read 256-byte
/// `fgets` chunks until a chunk ends in `\n` (stripped) or
/// EOF. Bytes accumulate in `out` (including a partial line
/// when EOF follows it). Returns 0 on a complete line,
/// `CHAR_CTRL_D` on EOF (a read error behaves like EOF —
/// `fgets` returns null for both).
pub fn readline<R: BufRead, W: Write>(
    reader: &mut R,
    prompt_out: &mut W,
    prompt: Option<&[u8]>,
    out: &mut Vec<u8>,
) -> i32 {
    if let Some(p) = prompt {
        if !p.is_empty() {
            let _ = prompt_out.write_all(p);
            let _ = prompt_out.flush();
        }
    }
    loop {
        let (_, done, chunk) = fgets_chunk(reader, 255);
        if chunk.is_empty() {
            return CHAR_CTRL_D;
        }
        if done {
            out.extend_from_slice(&chunk[..chunk.len() - 1]);
            return 0;
        }
        out.extend_from_slice(&chunk);
    }
}

/// `npy_repl_read_line(line_buf, buf_max, prompt)` — the prompt
/// (if non-empty) goes to STDERR, not stdout (the REPL prompt
/// is a diagnostic), then one `fgets` of at most `buf_max - 1`
/// bytes. Returns the byte count INCLUDING a trailing `\n`
/// (the count is taken before the strip), with the `\n`
/// stripped in `out`; 0 on EOF (`out` left empty). The count
/// is the `strlen` length: bytes from the first NUL on are
/// invisible, so a NUL-first line returns 0 (EOF), like the
/// original.
pub fn repl_read_line<R: BufRead, W: Write>(
    reader: &mut R,
    prompt_out: &mut W,
    prompt: Option<&[u8]>,
    buf_max: usize,
    out: &mut Vec<u8>,
) -> usize {
    if let Some(p) = prompt {
        if !p.is_empty() {
            let _ = prompt_out.write_all(p);
            let _ = prompt_out.flush();
        }
    }
    out.clear();
    if buf_max <= 1 {
        return 0; // `fgets` with size <= 1 stores only the NUL
    }
    let (n, done, chunk) = fgets_chunk(reader, buf_max - 1);
    if n == 0 {
        return 0;
    }
    if done {
        out.extend_from_slice(&chunk[..chunk.len() - 1]);
    } else {
        out.extend_from_slice(&chunk);
    }
    n
}
