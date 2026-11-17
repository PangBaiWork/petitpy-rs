//! String/bytes literal scanning.
//!
//! Port of the string-literal section of `src/compiler/lexer.zig`.
//! `STR_UNICODE` comes from the generated `build_options.rs` (see
//! `build.rs`): it selects how string-literal escapes are emitted —
//! `true` means `str` is a Unicode code-point sequence and escapes
//! are encoded as UTF-8 (CPython-compatible default; `"\xff"` becomes
//! the 2 bytes `c3 bf` for U+00FF); `false` means `str` is a byte
//! sequence (`"\xff"` becomes the single byte `ff`). Independent of
//! full/nano: configure via `PETITPY_STR_UNICODE=0` at build time.

use super::chars::{is_xdigit, xdigit_value};
use super::{LexError, LexResult, Lexer, Unichar, EOF};
use super::tables::TokenKind;
use crate::compiler::build_options::STR_UNICODE;

impl Lexer {
    pub(crate) fn get_hex(&mut self, mut num_digits: usize, result: &mut Unichar) -> bool {
        let mut num: Unichar = 0;
        while num_digits != 0 {
            num_digits -= 1;
            self.next_char();
            let ch = self.cur();
            if !is_xdigit(ch) {
                return false;
            }
            num = (num << 4) + xdigit_value(ch);
        }
        *result = num;
        true
    }

    pub(crate) fn parse_string_literal_inner(
        &mut self,
        is_raw: bool,
        is_fstring: bool,
    ) -> LexResult<()> {
        let mut quote_char: u8 = b'\'';
        if self.is_char(b'"') {
            quote_char = b'"';
        }
        self.next_char();

        let num_quotes: usize;
        if self.is_char_and(quote_char, quote_char) {
            self.next_char();
            self.next_char();
            num_quotes = 3;
        } else {
            num_quotes = 1;
        }

        // FSTRINGS: prep the .format(... ) buffer if this is an f-string.
        if is_fstring {
            // Empty fstring_args means a fresh f-string; otherwise an
            // adjacent concatenated f-string just keeps appending to
            // the args buffer.
            if self.fstring_args.is_empty() {
                self.fstring_args.extend_from_slice(b".format(");
            }
        }

        let mut n_closing: usize = 0;
        while !self.is_end() && (num_quotes > 1 || !self.is_char(b'\n')) && n_closing < num_quotes {
            if self.is_char(quote_char) {
                n_closing += 1;
                self.vstr.push((self.cur() & 0xff) as u8);
            } else {
                n_closing = 0;

                // FSTRINGS: handle {...} placeholders.
                while is_fstring && self.is_char(b'{') {
                    self.next_char();
                    if self.is_char(b'{') {
                        // {{ → literal { (passes through to str.format).
                        self.vstr.push(b'{');
                        self.next_char();
                    } else {
                        // wrap each arg in (): f"{a,b}, {c}" →
                        // "{}".format((a,b),(c),)
                        self.fstring_args.push(b'(');
                        let start_i = self.fstring_args.len();
                        let mut nested: u32 = 0;
                        // Extract until top-level ':' or '}' or
                        // '!r'/'!s' conversion.
                        while !self.is_end()
                            && (nested != 0
                                || !(self.is_char_or(b':', b'}')
                                    || (self.is_char(b'!')
                                        && self.is_char_following_or(b'r', b's')
                                        && self
                                            .is_char_following_following_or(b':', b'}'))))
                        {
                            let ch: u8 = (self.cur() & 0xff) as u8;
                            if ch == b'[' || ch == b'{' {
                                nested = nested.wrapping_add(1);
                            } else if ch == b']' || ch == b'}' {
                                nested = nested.wrapping_sub(1);
                            }
                            self.fstring_args.push(ch);
                            self.next_char();
                        }
                        // f'{a=}' — inject "a=" before the '{' (in the
                        // literal), and strip the trailing '=' from the arg.
                        if self.fstring_args.len() > start_i
                            && self.fstring_args[self.fstring_args.len() - 1] == b'='
                        {
                            let head =
                                self.fstring_args[start_i..self.fstring_args.len()].to_vec();
                            self.vstr.extend_from_slice(&head);
                            self.fstring_args.truncate(self.fstring_args.len() - 1);
                        }
                        self.fstring_args.push(b')');
                        self.fstring_args.push(b',');
                    }
                    self.vstr.push(b'{');
                }

                if self.is_char(b'\\') {
                    self.next_char();
                    let mut ch = self.cur();
                    if is_raw {
                        self.vstr.push(b'\\');
                    } else {
                        // Byte values below are the escape letters;
                        // `ch` is a u32 (a byte, or EOF).
                        match ch {
                            0x0a => ch = EOF, // '\n': line continuation, emit nothing
                            0x5c | 0x27 | 0x22 => {} // '\\', '\'', '"': pass through
                            0x61 => ch = 0x07, // 'a'
                            0x62 => ch = 0x08, // 'b'
                            0x74 => ch = 0x09, // 't'
                            0x6e => ch = 0x0a, // 'n'
                            0x76 => ch = 0x0b, // 'v'
                            0x66 => ch = 0x0c, // 'f'
                            0x72 => ch = 0x0d, // 'r'
                            0x75 | 0x55 | 0x78 => {
                                // 'u', 'U', 'x'
                                if (ch == 0x75 || ch == 0x55)
                                    && self.tok_kind == TokenKind::BYTES
                                {
                                    // b'\u1234' stays literal: keep the
                                    // backslash; the 'u' is emitted below.
                                    self.vstr.push(b'\\');
                                } else {
                                    let mut num: Unichar = 0;
                                    let ndig: usize = if ch == 0x78 {
                                        2
                                    } else if ch == 0x75 {
                                        4
                                    } else {
                                        8
                                    };
                                    if !self.get_hex(ndig, &mut num) {
                                        self.tok_kind = TokenKind::INVALID;
                                    }
                                    ch = num;
                                }
                            }
                            0x4e => return Err(LexError::UnicodeNameEscape), // 'N'
                            _ => {
                                if ch >= 0x30 && ch <= 0x37 {
                                    // Octal escape: up to 3 digits.
                                    let mut digits: i32 = 3;
                                    let mut num: Unichar = ch - 0x30;
                                    loop {
                                        if !self.is_following_odigit() {
                                            break;
                                        }
                                        digits -= 1;
                                        if digits == 0 {
                                            break;
                                        }
                                        self.next_char();
                                        num = num * 8 + (self.cur() - 0x30);
                                    }
                                    ch = num;
                                } else {
                                    self.vstr.push(b'\\');
                                }
                            }
                        }
                    }
                    // Shared with the raw branch: the char after '\' is
                    // still emitted (raw keeps the '\' too). Matches
                    // lexer.c structure.
                    if ch != EOF {
                        if STR_UNICODE {
                            // STR_UNICODE: str keeps codepoint (utf-8
                            // encode); bytes keeps raw byte. Vendor
                            // lexer.c:496-509.
                            if ch < 0x110000 && self.tok_kind == TokenKind::STRING {
                                self.append_unichar(ch);
                            } else if ch < 0x100 && self.tok_kind == TokenKind::BYTES {
                                self.vstr.push(ch as u8);
                            } else {
                                self.tok_kind = TokenKind::INVALID;
                            }
                        } else if ch < 0x100 {
                            self.vstr.push(ch as u8);
                        } else {
                            self.tok_kind = TokenKind::INVALID;
                        }
                    }
                } else {
                    self.vstr.push((self.cur() & 0xff) as u8);
                }
            }
            self.next_char();
        }

        if n_closing < num_quotes {
            self.tok_kind = TokenKind::LONELY_STRING_OPEN;
        }

        // vstr_cut_tail_bytes equivalent — drop the closing quote chars
        // we accumulated while scanning for the terminator.
        if self.vstr.len() >= n_closing {
            self.vstr.truncate(self.vstr.len() - n_closing);
        } else {
            self.vstr.clear();
        }
        Ok(())
    }

    /// Encode `ch` as UTF-8 into the vstr. Matches vstr_add_char
    /// semantics (which calls utf8_get_char_len +
    /// utf8_get_char_encode).
    pub(crate) fn append_unichar(&mut self, ch: Unichar) {
        if ch < 0x80 {
            self.vstr.push(ch as u8);
        } else if ch < 0x800 {
            self.vstr.extend_from_slice(&[
                (0xC0 | (ch >> 6)) as u8,
                (0x80 | (ch & 0x3F)) as u8,
            ]);
        } else if ch < 0x10000 {
            self.vstr.extend_from_slice(&[
                (0xE0 | (ch >> 12)) as u8,
                (0x80 | ((ch >> 6) & 0x3F)) as u8,
                (0x80 | (ch & 0x3F)) as u8,
            ]);
        } else {
            self.vstr.extend_from_slice(&[
                (0xF0 | (ch >> 18)) as u8,
                (0x80 | ((ch >> 12) & 0x3F)) as u8,
                (0x80 | ((ch >> 6) & 0x3F)) as u8,
                (0x80 | (ch & 0x3F)) as u8,
            ]);
        }
    }
}
