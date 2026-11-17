//! Character predicates for the lexer.
//!
//! Port of the predicate section of `src/compiler/lexer.zig`.
//! ASCII-only predicates — vendor's `unichar_is*` are likewise
//! ASCII-only (see vendor/micropython_embed/py/unicode.c:118 comment).

use super::Lexer;
use super::Unichar;

// ── Free predicates on a single character ─────────────────────────────────

/// ASCII-only: `ch < 128 and (ch == ' ' or (ch >= 9 and ch <= 13))`.
pub(crate) fn is_space(ch: Unichar) -> bool {
    ch < 128 && (ch == b' ' as Unichar || (9..=13).contains(&ch))
}

pub(crate) fn is_alpha(ch: Unichar) -> bool {
    ch < 128 && ((b'A' as Unichar..=b'Z' as Unichar).contains(&ch) || (b'a' as Unichar..=b'z' as Unichar).contains(&ch))
}

pub(crate) fn is_digit(ch: Unichar) -> bool {
    (b'0' as Unichar..=b'9' as Unichar).contains(&ch)
}

pub(crate) fn is_xdigit(ch: Unichar) -> bool {
    is_digit(ch)
        || (b'A' as Unichar..=b'F' as Unichar).contains(&ch)
        || (b'a' as Unichar..=b'f' as Unichar).contains(&ch)
}

pub(crate) fn xdigit_value(ch: Unichar) -> Unichar {
    // Mirrors the Zig bit trick: clear the 0x20 bit (fold case) then
    // subtract ('A' - ('9' + 1)) == 7.
    let mut n: Unichar = ch - b'0' as Unichar;
    if n > 9 {
        n &= !(b'a' - b'A') as Unichar;
        n -= (b'A' - (b'9' + 1)) as Unichar;
    }
    n
}

// ── Lexer predicates on the 3-char lookahead ──────────────────────────────

impl Lexer {
    pub(crate) fn cur(&self) -> Unichar {
        self.chr0
    }

    pub(crate) fn is_end(&self) -> bool {
        self.chr0 == super::EOF
    }

    pub(crate) fn is_physical_newline(&self) -> bool {
        self.chr0 == b'\n' as Unichar
    }

    pub(crate) fn is_char(&self, ch: u8) -> bool {
        self.chr0 == ch as Unichar
    }

    pub(crate) fn is_char_or(&self, c1: u8, c2: u8) -> bool {
        self.chr0 == c1 as Unichar || self.chr0 == c2 as Unichar
    }

    pub(crate) fn is_char_or3(&self, c1: u8, c2: u8, c3: u8) -> bool {
        self.chr0 == c1 as Unichar || self.chr0 == c2 as Unichar || self.chr0 == c3 as Unichar
    }

    pub(crate) fn is_char_or4(&self, c1: u8, c2: u8, c3: u8, c4: u8) -> bool {
        self.chr0 == c1 as Unichar
            || self.chr0 == c2 as Unichar
            || self.chr0 == c3 as Unichar
            || self.chr0 == c4 as Unichar
    }

    pub(crate) fn is_char_following(&self, ch: u8) -> bool {
        self.chr1 == ch as Unichar
    }

    pub(crate) fn is_char_following_or(&self, c1: u8, c2: u8) -> bool {
        self.chr1 == c1 as Unichar || self.chr1 == c2 as Unichar
    }

    pub(crate) fn is_char_following_following_or(&self, c1: u8, c2: u8) -> bool {
        self.chr2 == c1 as Unichar || self.chr2 == c2 as Unichar
    }

    pub(crate) fn is_char_and(&self, c1: u8, c2: u8) -> bool {
        self.chr0 == c1 as Unichar && self.chr1 == c2 as Unichar
    }

    pub(crate) fn is_whitespace(&self) -> bool {
        is_space(self.chr0)
    }

    pub(crate) fn is_letter(&self) -> bool {
        is_alpha(self.chr0)
    }

    pub(crate) fn is_digit_p(&self) -> bool {
        is_digit(self.chr0)
    }

    pub(crate) fn is_following_digit(&self) -> bool {
        is_digit(self.chr1)
    }

    pub(crate) fn is_following_base_char(&self) -> bool {
        let c = self.chr1 | 0x20;
        c == b'b' as Unichar || c == b'o' as Unichar || c == b'x' as Unichar
    }

    pub(crate) fn is_following_odigit(&self) -> bool {
        self.chr1 >= b'0' as Unichar && self.chr1 <= b'7' as Unichar
    }

    pub(crate) fn is_head_of_identifier(&self) -> bool {
        self.is_letter() || self.chr0 == b'_' as Unichar || self.chr0 >= 0x80
    }

    pub(crate) fn is_tail_of_identifier(&self) -> bool {
        self.is_head_of_identifier() || self.is_digit_p()
    }

    pub(crate) fn is_string_or_bytes(&self) -> bool {
        // FSTRINGS: include 'f' and 'rf'/'fr' prefixes.
        self.is_char_or(b'\'', b'"')
            || (self.is_char_or4(b'r', b'u', b'b', b'f') && self.is_char_following_or(b'\'', b'"'))
            || ((self.is_char_and(b'r', b'b')
                || self.is_char_and(b'b', b'r')
                || self.is_char_and(b'r', b'f')
                || self.is_char_and(b'f', b'r'))
                && self.is_char_following_following_or(b'\'', b'"'))
    }

}
