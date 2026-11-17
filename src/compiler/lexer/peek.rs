//! Soft-keyword lookahead (`match` / `case`).
//!
//! Port of the SOFT KEYWORDS section of `src/compiler/lexer.zig`.
//!
//! `match` and `case` are soft keywords: valid identifiers everywhere,
//! statement introducers only in one position. They are deliberately NOT
//! in TOK_KW and get no TokenKind of their own — the lexer emits NAME
//! for them exactly as vendor py/lexer.c does, so the token stream stays
//! byte-identical for every input and TokenKind::NUMBER_OF stays pinned
//! at 96.
//!
//! Disambiguation is the parser's job, but it needs more than the one
//! token of lookahead the parser has: `match (x):` is a match statement
//! and `match(x)` is a call, and only the trailing ':' separates them.
//! The reader is a byte-at-a-time interface with no seek, so the
//! scan-ahead happens at the token level — `peek_line_ends_with_colon`
//! buffers a logical line and rewinds, and `to_next` replays the buffer
//! before it resumes scanning.

use super::{LexResult, Lexer};
use super::tables::TokenKind;

/// One buffered token: everything `to_next` publishes to the parser.
/// vstr bytes live in `Lexer::peek_vstr` at `[off..off+len]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedTok {
    kind: TokenKind,
    line: usize,
    column: usize,
    vstr_off: usize,
    vstr_len: usize,
}

impl Lexer {
    pub(crate) fn peek_drain_if_done(&mut self) {
        if self.peek_idx == self.peek_toks.len() {
            self.peek_toks.clear();
            self.peek_vstr.clear();
            self.peek_idx = 0;
        }
    }

    /// Append the token the lexer is currently sitting on to the buffer.
    pub(crate) fn peek_append_current(&mut self) {
        let off = self.peek_vstr.len();
        self.peek_vstr.extend_from_slice(&self.vstr);
        self.peek_toks.push(SavedTok {
            kind: self.tok_kind,
            line: self.tok_line,
            column: self.tok_column,
            vstr_off: off,
            vstr_len: self.vstr.len(),
        });
    }

    /// Publish a buffered token as the current one.
    pub(crate) fn peek_restore(&mut self, t: SavedTok) {
        self.tok_kind = t.kind;
        self.tok_line = t.line;
        self.tok_column = t.column;
        self.vstr.clear();
        self.vstr
            .extend_from_slice(&self.peek_vstr[t.vstr_off..t.vstr_off + t.vstr_len]);
    }

    /// Scan to the end of the current logical line without consuming
    /// it, and report whether the last token before the terminator
    /// was ':'.
    ///
    /// The caller's stream position is unchanged: every token scanned
    /// is buffered and replayed by `to_next`, starting with the one
    /// the caller was sitting on.
    ///
    /// NEWLINE is only emitted at nested_bracket_level == 0
    /// (skip_whitespace), so bracket depth needs no tracking here — a
    /// bracketed line arrives as one logical line for free.
    pub fn peek_line_ends_with_colon(&mut self) -> LexResult<bool> {
        debug_assert!(self.peek_toks.is_empty());

        self.peek_append_current();
        let mut prev = self.tok_kind;
        while self.tok_kind != TokenKind::NEWLINE && self.tok_kind != TokenKind::END {
            prev = self.tok_kind;
            self.scan_next()?;
            self.peek_append_current();
        }

        let first = self.peek_toks[0];
        self.peek_restore(first);
        self.peek_idx = 1;
        self.peek_drain_if_done();
        Ok(prev == TokenKind::DEL_COLON)
    }

    /// Advance to the next token. Serves the soft-keyword lookahead
    /// buffer first when one is live; otherwise scans real input.
    pub fn to_next(&mut self) -> LexResult<()> {
        if self.peek_idx < self.peek_toks.len() {
            let t = self.peek_toks[self.peek_idx];
            self.peek_idx += 1;
            self.peek_restore(t);
            self.peek_drain_if_done();
            return Ok(());
        }
        self.scan_next()
    }
}
