//! Operator/delimiter encoding tables, keyword table, and `TokenKind`.
//!
//! Port of the corresponding sections of `src/compiler/lexer.zig`
//! (the encoding tables are themselves a verbatim port of vendor
//! `py/lexer.c`).
//!
//! `TokenKind` variant names keep the Zig/vendor spellings verbatim
//! (e.g. `KW_FALSE`, `DEL_PAREN_OPEN`) so the port stays checkable
//! line-by-line against the Zig original — hence the `allow` below.
//! `TokenKind::name()` returns the Zig `@tagName` spelling, used by
//! the token dump in the tests.

#![allow(non_camel_case_types)]

/// MicroPython token kind. Variant ordinals match vendor
/// `mp_token_kind_t` for the nano + FSTRINGS + ASYNC_AWAIT config. If
/// the vendor enum is regenerated the fingerprint check below will
/// fail with a clear message.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    END = 0,
    INVALID = 1,
    DEDENT_MISMATCH = 2,
    LONELY_STRING_OPEN = 3,
    MALFORMED_FSTRING = 4,
    NEWLINE = 5,
    INDENT = 6,
    DEDENT = 7,
    NAME = 8,
    INTEGER = 9,
    FLOAT_OR_IMAG = 10,
    STRING = 11,
    BYTES = 12,
    ELLIPSIS = 13,
    KW_FALSE = 14,
    KW_NONE = 15,
    KW_TRUE = 16,
    KW___DEBUG__ = 17,
    KW_AND = 18,
    KW_AS = 19,
    KW_ASSERT = 20,
    KW_ASYNC = 21,
    KW_AWAIT = 22,
    KW_BREAK = 23,
    KW_CLASS = 24,
    KW_CONTINUE = 25,
    KW_DEF = 26,
    KW_DEL = 27,
    KW_ELIF = 28,
    KW_ELSE = 29,
    KW_EXCEPT = 30,
    KW_FINALLY = 31,
    KW_FOR = 32,
    KW_FROM = 33,
    KW_GLOBAL = 34,
    KW_IF = 35,
    KW_IMPORT = 36,
    KW_IN = 37,
    KW_IS = 38,
    KW_LAMBDA = 39,
    KW_NONLOCAL = 40,
    KW_NOT = 41,
    KW_OR = 42,
    KW_PASS = 43,
    KW_RAISE = 44,
    KW_RETURN = 45,
    KW_TRY = 46,
    KW_WHILE = 47,
    KW_WITH = 48,
    KW_YIELD = 49,
    OP_ASSIGN = 50,
    OP_TILDE = 51,
    OP_LESS = 52,
    OP_MORE = 53,
    OP_DBL_EQUAL = 54,
    OP_LESS_EQUAL = 55,
    OP_MORE_EQUAL = 56,
    OP_NOT_EQUAL = 57,
    OP_PIPE = 58,
    OP_CARET = 59,
    OP_AMPERSAND = 60,
    OP_DBL_LESS = 61,
    OP_DBL_MORE = 62,
    OP_PLUS = 63,
    OP_MINUS = 64,
    OP_STAR = 65,
    OP_AT = 66,
    OP_DBL_SLASH = 67,
    OP_SLASH = 68,
    OP_PERCENT = 69,
    OP_DBL_STAR = 70,
    DEL_PIPE_EQUAL = 71,
    DEL_CARET_EQUAL = 72,
    DEL_AMPERSAND_EQUAL = 73,
    DEL_DBL_LESS_EQUAL = 74,
    DEL_DBL_MORE_EQUAL = 75,
    DEL_PLUS_EQUAL = 76,
    DEL_MINUS_EQUAL = 77,
    DEL_STAR_EQUAL = 78,
    DEL_AT_EQUAL = 79,
    DEL_DBL_SLASH_EQUAL = 80,
    DEL_SLASH_EQUAL = 81,
    DEL_PERCENT_EQUAL = 82,
    DEL_DBL_STAR_EQUAL = 83,
    DEL_PAREN_OPEN = 84,
    DEL_PAREN_CLOSE = 85,
    DEL_BRACKET_OPEN = 86,
    DEL_BRACKET_CLOSE = 87,
    DEL_BRACE_OPEN = 88,
    DEL_BRACE_CLOSE = 89,
    DEL_COMMA = 90,
    DEL_COLON = 91,
    DEL_PERIOD = 92,
    DEL_SEMICOLON = 93,
    DEL_EQUAL = 94,
    DEL_MINUS_MORE = 95,
    NUMBER_OF = 96,
}

impl TokenKind {
    /// Convert a raw `u8` discriminant back to a kind. `None` for
    /// out-of-range values (96+). Mirrors Zig's `@enumFromInt` call
    /// sites, which only ever feed in-range values.
    pub fn from_u8(v: u8) -> Option<TokenKind> {
        Some(match v {
            0 => TokenKind::END,
            1 => TokenKind::INVALID,
            2 => TokenKind::DEDENT_MISMATCH,
            3 => TokenKind::LONELY_STRING_OPEN,
            4 => TokenKind::MALFORMED_FSTRING,
            5 => TokenKind::NEWLINE,
            6 => TokenKind::INDENT,
            7 => TokenKind::DEDENT,
            8 => TokenKind::NAME,
            9 => TokenKind::INTEGER,
            10 => TokenKind::FLOAT_OR_IMAG,
            11 => TokenKind::STRING,
            12 => TokenKind::BYTES,
            13 => TokenKind::ELLIPSIS,
            14 => TokenKind::KW_FALSE,
            15 => TokenKind::KW_NONE,
            16 => TokenKind::KW_TRUE,
            17 => TokenKind::KW___DEBUG__,
            18 => TokenKind::KW_AND,
            19 => TokenKind::KW_AS,
            20 => TokenKind::KW_ASSERT,
            21 => TokenKind::KW_ASYNC,
            22 => TokenKind::KW_AWAIT,
            23 => TokenKind::KW_BREAK,
            24 => TokenKind::KW_CLASS,
            25 => TokenKind::KW_CONTINUE,
            26 => TokenKind::KW_DEF,
            27 => TokenKind::KW_DEL,
            28 => TokenKind::KW_ELIF,
            29 => TokenKind::KW_ELSE,
            30 => TokenKind::KW_EXCEPT,
            31 => TokenKind::KW_FINALLY,
            32 => TokenKind::KW_FOR,
            33 => TokenKind::KW_FROM,
            34 => TokenKind::KW_GLOBAL,
            35 => TokenKind::KW_IF,
            36 => TokenKind::KW_IMPORT,
            37 => TokenKind::KW_IN,
            38 => TokenKind::KW_IS,
            39 => TokenKind::KW_LAMBDA,
            40 => TokenKind::KW_NONLOCAL,
            41 => TokenKind::KW_NOT,
            42 => TokenKind::KW_OR,
            43 => TokenKind::KW_PASS,
            44 => TokenKind::KW_RAISE,
            45 => TokenKind::KW_RETURN,
            46 => TokenKind::KW_TRY,
            47 => TokenKind::KW_WHILE,
            48 => TokenKind::KW_WITH,
            49 => TokenKind::KW_YIELD,
            50 => TokenKind::OP_ASSIGN,
            51 => TokenKind::OP_TILDE,
            52 => TokenKind::OP_LESS,
            53 => TokenKind::OP_MORE,
            54 => TokenKind::OP_DBL_EQUAL,
            55 => TokenKind::OP_LESS_EQUAL,
            56 => TokenKind::OP_MORE_EQUAL,
            57 => TokenKind::OP_NOT_EQUAL,
            58 => TokenKind::OP_PIPE,
            59 => TokenKind::OP_CARET,
            60 => TokenKind::OP_AMPERSAND,
            61 => TokenKind::OP_DBL_LESS,
            62 => TokenKind::OP_DBL_MORE,
            63 => TokenKind::OP_PLUS,
            64 => TokenKind::OP_MINUS,
            65 => TokenKind::OP_STAR,
            66 => TokenKind::OP_AT,
            67 => TokenKind::OP_DBL_SLASH,
            68 => TokenKind::OP_SLASH,
            69 => TokenKind::OP_PERCENT,
            70 => TokenKind::OP_DBL_STAR,
            71 => TokenKind::DEL_PIPE_EQUAL,
            72 => TokenKind::DEL_CARET_EQUAL,
            73 => TokenKind::DEL_AMPERSAND_EQUAL,
            74 => TokenKind::DEL_DBL_LESS_EQUAL,
            75 => TokenKind::DEL_DBL_MORE_EQUAL,
            76 => TokenKind::DEL_PLUS_EQUAL,
            77 => TokenKind::DEL_MINUS_EQUAL,
            78 => TokenKind::DEL_STAR_EQUAL,
            79 => TokenKind::DEL_AT_EQUAL,
            80 => TokenKind::DEL_DBL_SLASH_EQUAL,
            81 => TokenKind::DEL_SLASH_EQUAL,
            82 => TokenKind::DEL_PERCENT_EQUAL,
            83 => TokenKind::DEL_DBL_STAR_EQUAL,
            84 => TokenKind::DEL_PAREN_OPEN,
            85 => TokenKind::DEL_PAREN_CLOSE,
            86 => TokenKind::DEL_BRACKET_OPEN,
            87 => TokenKind::DEL_BRACKET_CLOSE,
            88 => TokenKind::DEL_BRACE_OPEN,
            89 => TokenKind::DEL_BRACE_CLOSE,
            90 => TokenKind::DEL_COMMA,
            91 => TokenKind::DEL_COLON,
            92 => TokenKind::DEL_PERIOD,
            93 => TokenKind::DEL_SEMICOLON,
            94 => TokenKind::DEL_EQUAL,
            95 => TokenKind::DEL_MINUS_MORE,
            _ => return None,
        })
    }

    /// Zig `@tagName` spelling, e.g. `KW_PASS`. Used by the token
    /// dump in the tests (mirrors Zig's `dumpStream`).
    pub fn name(self) -> &'static str {
        match self {
            TokenKind::END => "END",
            TokenKind::INVALID => "INVALID",
            TokenKind::DEDENT_MISMATCH => "DEDENT_MISMATCH",
            TokenKind::LONELY_STRING_OPEN => "LONELY_STRING_OPEN",
            TokenKind::MALFORMED_FSTRING => "MALFORMED_FSTRING",
            TokenKind::NEWLINE => "NEWLINE",
            TokenKind::INDENT => "INDENT",
            TokenKind::DEDENT => "DEDENT",
            TokenKind::NAME => "NAME",
            TokenKind::INTEGER => "INTEGER",
            TokenKind::FLOAT_OR_IMAG => "FLOAT_OR_IMAG",
            TokenKind::STRING => "STRING",
            TokenKind::BYTES => "BYTES",
            TokenKind::ELLIPSIS => "ELLIPSIS",
            TokenKind::KW_FALSE => "KW_FALSE",
            TokenKind::KW_NONE => "KW_NONE",
            TokenKind::KW_TRUE => "KW_TRUE",
            TokenKind::KW___DEBUG__ => "KW___DEBUG__",
            TokenKind::KW_AND => "KW_AND",
            TokenKind::KW_AS => "KW_AS",
            TokenKind::KW_ASSERT => "KW_ASSERT",
            TokenKind::KW_ASYNC => "KW_ASYNC",
            TokenKind::KW_AWAIT => "KW_AWAIT",
            TokenKind::KW_BREAK => "KW_BREAK",
            TokenKind::KW_CLASS => "KW_CLASS",
            TokenKind::KW_CONTINUE => "KW_CONTINUE",
            TokenKind::KW_DEF => "KW_DEF",
            TokenKind::KW_DEL => "KW_DEL",
            TokenKind::KW_ELIF => "KW_ELIF",
            TokenKind::KW_ELSE => "KW_ELSE",
            TokenKind::KW_EXCEPT => "KW_EXCEPT",
            TokenKind::KW_FINALLY => "KW_FINALLY",
            TokenKind::KW_FOR => "KW_FOR",
            TokenKind::KW_FROM => "KW_FROM",
            TokenKind::KW_GLOBAL => "KW_GLOBAL",
            TokenKind::KW_IF => "KW_IF",
            TokenKind::KW_IMPORT => "KW_IMPORT",
            TokenKind::KW_IN => "KW_IN",
            TokenKind::KW_IS => "KW_IS",
            TokenKind::KW_LAMBDA => "KW_LAMBDA",
            TokenKind::KW_NONLOCAL => "KW_NONLOCAL",
            TokenKind::KW_NOT => "KW_NOT",
            TokenKind::KW_OR => "KW_OR",
            TokenKind::KW_PASS => "KW_PASS",
            TokenKind::KW_RAISE => "KW_RAISE",
            TokenKind::KW_RETURN => "KW_RETURN",
            TokenKind::KW_TRY => "KW_TRY",
            TokenKind::KW_WHILE => "KW_WHILE",
            TokenKind::KW_WITH => "KW_WITH",
            TokenKind::KW_YIELD => "KW_YIELD",
            TokenKind::OP_ASSIGN => "OP_ASSIGN",
            TokenKind::OP_TILDE => "OP_TILDE",
            TokenKind::OP_LESS => "OP_LESS",
            TokenKind::OP_MORE => "OP_MORE",
            TokenKind::OP_DBL_EQUAL => "OP_DBL_EQUAL",
            TokenKind::OP_LESS_EQUAL => "OP_LESS_EQUAL",
            TokenKind::OP_MORE_EQUAL => "OP_MORE_EQUAL",
            TokenKind::OP_NOT_EQUAL => "OP_NOT_EQUAL",
            TokenKind::OP_PIPE => "OP_PIPE",
            TokenKind::OP_CARET => "OP_CARET",
            TokenKind::OP_AMPERSAND => "OP_AMPERSAND",
            TokenKind::OP_DBL_LESS => "OP_DBL_LESS",
            TokenKind::OP_DBL_MORE => "OP_DBL_MORE",
            TokenKind::OP_PLUS => "OP_PLUS",
            TokenKind::OP_MINUS => "OP_MINUS",
            TokenKind::OP_STAR => "OP_STAR",
            TokenKind::OP_AT => "OP_AT",
            TokenKind::OP_DBL_SLASH => "OP_DBL_SLASH",
            TokenKind::OP_SLASH => "OP_SLASH",
            TokenKind::OP_PERCENT => "OP_PERCENT",
            TokenKind::OP_DBL_STAR => "OP_DBL_STAR",
            TokenKind::DEL_PIPE_EQUAL => "DEL_PIPE_EQUAL",
            TokenKind::DEL_CARET_EQUAL => "DEL_CARET_EQUAL",
            TokenKind::DEL_AMPERSAND_EQUAL => "DEL_AMPERSAND_EQUAL",
            TokenKind::DEL_DBL_LESS_EQUAL => "DEL_DBL_LESS_EQUAL",
            TokenKind::DEL_DBL_MORE_EQUAL => "DEL_DBL_MORE_EQUAL",
            TokenKind::DEL_PLUS_EQUAL => "DEL_PLUS_EQUAL",
            TokenKind::DEL_MINUS_EQUAL => "DEL_MINUS_EQUAL",
            TokenKind::DEL_STAR_EQUAL => "DEL_STAR_EQUAL",
            TokenKind::DEL_AT_EQUAL => "DEL_AT_EQUAL",
            TokenKind::DEL_DBL_SLASH_EQUAL => "DEL_DBL_SLASH_EQUAL",
            TokenKind::DEL_SLASH_EQUAL => "DEL_SLASH_EQUAL",
            TokenKind::DEL_PERCENT_EQUAL => "DEL_PERCENT_EQUAL",
            TokenKind::DEL_DBL_STAR_EQUAL => "DEL_DBL_STAR_EQUAL",
            TokenKind::DEL_PAREN_OPEN => "DEL_PAREN_OPEN",
            TokenKind::DEL_PAREN_CLOSE => "DEL_PAREN_CLOSE",
            TokenKind::DEL_BRACKET_OPEN => "DEL_BRACKET_OPEN",
            TokenKind::DEL_BRACKET_CLOSE => "DEL_BRACKET_CLOSE",
            TokenKind::DEL_BRACE_OPEN => "DEL_BRACE_OPEN",
            TokenKind::DEL_BRACE_CLOSE => "DEL_BRACE_CLOSE",
            TokenKind::DEL_COMMA => "DEL_COMMA",
            TokenKind::DEL_COLON => "DEL_COLON",
            TokenKind::DEL_PERIOD => "DEL_PERIOD",
            TokenKind::DEL_SEMICOLON => "DEL_SEMICOLON",
            TokenKind::DEL_EQUAL => "DEL_EQUAL",
            TokenKind::DEL_MINUS_MORE => "DEL_MINUS_MORE",
            TokenKind::NUMBER_OF => "NUMBER_OF",
        }
    }
}

// Sanity: keep the variant count fingerprinted. A vendor enum drift
// adding/removing keywords would change this. (Mirrors the Zig
// `comptime` block.)
const _: () = assert!(
    TokenKind::NUMBER_OF as u8 == 96,
    "TokenKind ordinal layout drift — re-pin against vendor py/lexer.h"
);

// ── Operator/delimiter encoding tables (verbatim port) ────────────────────
//
// The Zig source builds this string by concatenation:
//   "()[]{},;~" ++ ":e=" ++ "<e=c<e=" ++ ">e=c>e=" ++ "*e=c*e=" ++
//   "+e=" ++ "-e=e>" ++ "&e=" ++ "|e=" ++ "/e=c/e=" ++ "%e=" ++
//   "^e=" ++ "@e=" ++ "=e=" ++ "!."

/// Encoded operator/delimiter scan table. A `'e'` marker means "the
/// preceding operator char may be followed by `=`"; a `'c'` marker
/// means "the preceding operator char may be followed by the next
/// char in the table". The lexer walk in `Lexer::scan_next` consumes
/// one `TOK_ENC_KIND` entry per operator char, skipping each
/// marker+char pair as part of the preceding entry.
pub(crate) const TOK_ENC: &[u8] =
    b"()[]{},;~:e=<e=c<e=>e=c>e=*e=c*e=+e=-e=e>&e=|e=/e=c/e=%e=^e=@e==e=!.";

pub(crate) const TOK_ENC_KIND: &[TokenKind; 44] = &[
    TokenKind::DEL_PAREN_OPEN,
    TokenKind::DEL_PAREN_CLOSE,
    TokenKind::DEL_BRACKET_OPEN,
    TokenKind::DEL_BRACKET_CLOSE,
    TokenKind::DEL_BRACE_OPEN,
    TokenKind::DEL_BRACE_CLOSE,
    TokenKind::DEL_COMMA,
    TokenKind::DEL_SEMICOLON,
    TokenKind::OP_TILDE,
    TokenKind::DEL_COLON,
    TokenKind::OP_ASSIGN,
    TokenKind::OP_LESS,
    TokenKind::OP_LESS_EQUAL,
    TokenKind::OP_DBL_LESS,
    TokenKind::DEL_DBL_LESS_EQUAL,
    TokenKind::OP_MORE,
    TokenKind::OP_MORE_EQUAL,
    TokenKind::OP_DBL_MORE,
    TokenKind::DEL_DBL_MORE_EQUAL,
    TokenKind::OP_STAR,
    TokenKind::DEL_STAR_EQUAL,
    TokenKind::OP_DBL_STAR,
    TokenKind::DEL_DBL_STAR_EQUAL,
    TokenKind::OP_PLUS,
    TokenKind::DEL_PLUS_EQUAL,
    TokenKind::OP_MINUS,
    TokenKind::DEL_MINUS_EQUAL,
    TokenKind::DEL_MINUS_MORE,
    TokenKind::OP_AMPERSAND,
    TokenKind::DEL_AMPERSAND_EQUAL,
    TokenKind::OP_PIPE,
    TokenKind::DEL_PIPE_EQUAL,
    TokenKind::OP_SLASH,
    TokenKind::DEL_SLASH_EQUAL,
    TokenKind::OP_DBL_SLASH,
    TokenKind::DEL_DBL_SLASH_EQUAL,
    TokenKind::OP_PERCENT,
    TokenKind::DEL_PERCENT_EQUAL,
    TokenKind::OP_CARET,
    TokenKind::DEL_CARET_EQUAL,
    TokenKind::OP_AT,
    TokenKind::DEL_AT_EQUAL,
    TokenKind::DEL_EQUAL,
    TokenKind::OP_DBL_EQUAL,
];

/// Byte at `ti` in the encoding table, or 0 (the NUL terminator the
/// Zig string carries) past the end. The scan loop relies on the 0
/// sentinel to report INVALID for unrecognised characters.
pub(crate) fn tok_enc_at(ti: usize) -> u8 {
    TOK_ENC.get(ti).copied().unwrap_or(0)
}

// ── Keyword table ─────────────────────────────────────────────────────────
//
// Same order as the enum from KW_FALSE; sorted by C strcmp. ASYNC_AWAIT
// is on (port/mpconfigport.h: MICROPY_PY_ASYNC_AWAIT=1) so "async" and
// "await" are unconditional entries between "assert" and "break".

pub(crate) const TOK_KW: &[&str; 36] = &[
    "False", "None", "True", "__debug__", "and", "as", "assert", "async", "await", "break",
    "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
    "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
    "return", "try", "while", "with", "yield",
];

/// C `strcmp` semantics on NUL-terminated bytes: compare byte by
/// byte; the first difference decides by (a - b); a NUL byte (end of
/// either slice) compares as 0. Both inputs are treated as
/// NUL-terminated — the Zig side appends a NUL to the vstr copy and
/// the keywords are `[:0]` strings; here the end of each slice plays
/// the NUL's role, which is exactly equivalent.
pub(crate) fn strcmp_slice(a: &[u8], b: &[u8]) -> i32 {
    let mut i = 0;
    loop {
        let ca = a.get(i).copied().unwrap_or(0);
        let cb = b.get(i).copied().unwrap_or(0);
        if ca != cb {
            return ca as i32 - cb as i32;
        }
        if ca == 0 {
            return 0;
        }
        i += 1;
    }
}
