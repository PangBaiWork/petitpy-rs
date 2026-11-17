//! Ported unit tests for the lexer.
//!
//! The 11 `test` blocks in `src/compiler/lexer.zig`, ported verbatim
//! (names snake_cased), plus extra evidence tests of our own (not
//! part of the Zig bar): the TokenKind fingerprint, the keyword table
//! lookup, the operator table spot check, and a corpus smoke test.

use super::tables::{strcmp_slice, TokenKind, TOK_ENC_KIND, TOK_KW};
use super::{Lexer, LexError};

fn new_lex(src: &str) -> Lexer {
    Lexer::new_from_str(0, src.as_bytes(), true).expect("lexer construction")
}

fn kinds(src: &str) -> Vec<TokenKind> {
    let mut lex = new_lex(src);
    let mut out = vec![lex.tok_kind()];
    while lex.tok_kind() != TokenKind::END {
        lex.to_next().expect("lex step");
        out.push(lex.tok_kind());
    }
    out
}

/// Dump a whole token stream as "KIND:vstr\n" lines. Mirrors Zig's
/// `dumpStream` (used by the soft-keyword replay test).
fn dump_stream(src: &[u8], peek_at: Option<&[u8]>) -> Vec<u8> {
    let mut lex = Lexer::new_from_str(0, src, true).expect("lexer construction");
    let mut out = Vec::new();
    while lex.tok_kind() != TokenKind::END {
        if let Some(name) = peek_at {
            if lex.tok_kind() == TokenKind::NAME && lex.vstr() == name {
                let _ = lex.peek_line_ends_with_colon();
            }
        }
        out.extend_from_slice(lex.tok_kind().name().as_bytes());
        out.push(b':');
        out.extend_from_slice(lex.vstr());
        out.push(b'\n');
        lex.to_next().expect("lex step");
    }
    out
}

// ── Ported verbatim from the `test` blocks in lexer.zig ──────────────────

#[test]
fn smoke_empty_input_yields_newline_then_end() {
    let mut lex = new_lex("");
    assert_eq!(TokenKind::NEWLINE, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::END, lex.tok_kind());
}

#[test]
fn smoke_pass_tokenises_to_kw_pass_newline_end() {
    let mut lex = new_lex("pass\n");
    assert_eq!(TokenKind::KW_PASS, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::NEWLINE, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::END, lex.tok_kind());
}

#[test]
fn smoke_identifier_accumulates_into_vstr() {
    let lex = new_lex("hello\n");
    assert_eq!(TokenKind::NAME, lex.tok_kind());
    assert_eq!(b"hello", lex.vstr());
}

#[test]
fn smoke_integer_literal() {
    let lex = new_lex("42\n");
    assert_eq!(TokenKind::INTEGER, lex.tok_kind());
    assert_eq!(b"42", lex.vstr());
}

#[test]
fn smoke_simple_string_literal() {
    let lex = new_lex("'abc'\n");
    assert_eq!(TokenKind::STRING, lex.tok_kind());
    assert_eq!(b"abc", lex.vstr());
}

#[test]
fn smoke_indent_dedent() {
    let src = "def f():\n    return 1\n";
    let mut lex = new_lex(src);
    // def f():
    assert_eq!(TokenKind::KW_DEF, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::NAME, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEL_PAREN_OPEN, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEL_PAREN_CLOSE, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEL_COLON, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::NEWLINE, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::INDENT, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::KW_RETURN, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::INTEGER, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::NEWLINE, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEDENT, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::END, lex.tok_kind());
}

#[test]
fn smoke_debug_flips_on_debug_mode() {
    {
        let lex = Lexer::new_from_str(0, b"__debug__\n", true).unwrap();
        assert_eq!(TokenKind::KW_TRUE, lex.tok_kind());
    }
    {
        let lex = Lexer::new_from_str(0, b"__debug__\n", false).unwrap();
        assert_eq!(TokenKind::KW_FALSE, lex.tok_kind());
    }
}

#[test]
fn smoke_hex_escape_in_string_literal() {
    let lex = new_lex("'\\x41'\n");
    assert_eq!(TokenKind::STRING, lex.tok_kind());
    assert_eq!(b"A", lex.vstr());
}

#[test]
fn smoke_nested_brackets_suppress_newline() {
    let src = "(1,\n 2)\n";
    let mut lex = new_lex(src);
    // No NEWLINE between '1,' and '2' because nested_bracket_level > 0.
    assert_eq!(TokenKind::DEL_PAREN_OPEN, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::INTEGER, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEL_COMMA, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::INTEGER, lex.tok_kind());
    lex.to_next().unwrap();
    assert_eq!(TokenKind::DEL_PAREN_CLOSE, lex.tok_kind());
}

#[test]
fn soft_keywords_peeking_a_line_replays_it_byte_identically() {
    let cases: &[&str] = &[
        "match x:\n    case 1:\n        pass\n",
        // The ambiguity the peek exists to resolve: same prefix, and a
        // bracketed subject so NEWLINE suppression is in play too.
        "match (x):\n    case (1):\n        pass\n",
        "match(x)\n",
        // f-strings rewrite the input queue mid-line; scanning past one
        // and replaying must not desync that state machine.
        "match f'{a}{b!r:>{w}}':\n    case 1:\n        pass\n",
        // Unbalanced bracket: no NEWLINE until EOF, so the peek buffers
        // the rest of the file. A syntax error downstream either way.
        "match (x:\n    pass\n",
        // `match` and `case` as ordinary identifiers must be untouched.
        "match = 1\ncase = 2\nprint(match + case)\n",
    ];
    for src in cases {
        let plain = dump_stream(src.as_bytes(), None);
        for kw in ["match", "case"] {
            let peeked = dump_stream(src.as_bytes(), Some(kw.as_bytes()));
            assert_eq!(plain, peeked, "peek replay diverged for {src:?} at {kw}");
        }
    }
}

#[test]
fn soft_keywords_the_colon_verdict_separates_statement_from_call() {
    let cases: &[(&str, bool)] = &[
        ("match x:\n    pass\n", true),
        ("match (x):\n    pass\n", true),
        ("match [a, b]:\n    pass\n", true),
        // A dict subject: braces carry colons, but only the last token
        // before NEWLINE decides.
        ("match {1: 2}:\n    pass\n", true),
        ("match {1: 2}\n", false),
        ("match(x)\n", false),
        ("match = 1\n", false),
        ("match.group(0)\n", false),
        // Bracketed line continuation: one logical line, verdict is the
        // colon after the close paren.
        ("match (\n    x\n):\n    pass\n", true),
        // An annotation ends in a name, not a colon.
        ("match: int = 1\n", false),
    ];
    for (src, want) in cases {
        let mut lex = new_lex(src);
        assert_eq!(*want, lex.peek_line_ends_with_colon().unwrap(), "{src:?}");
        // The peek left the stream where it found it.
        assert_eq!(TokenKind::NAME, lex.tok_kind());
        assert_eq!(b"match", lex.vstr());
    }
}

// ── Extra evidence (own tests, not part of the Zig bar) ───────────────────

#[test]
fn token_kind_fingerprint_number_of_is_96() {
    // Mirrors the Zig `comptime` fingerprint block.
    assert_eq!(96u8, TokenKind::NUMBER_OF as u8);
    // from_u8 round-trips every discriminant; 96+ is out of range.
    for v in 0..96u8 {
        let k = TokenKind::from_u8(v).expect("in range");
        assert_eq!(v, k as u8);
        assert!(!k.name().is_empty());
    }
    assert_eq!(None, TokenKind::from_u8(96));
    assert_eq!(None, TokenKind::from_u8(255));
    // Spot-check the name table against the Zig @tagName spellings.
    assert_eq!("END", TokenKind::END.name());
    assert_eq!("KW_PASS", TokenKind::KW_PASS.name());
    assert_eq!("DEL_MINUS_MORE", TokenKind::DEL_MINUS_MORE.name());
    assert_eq!("NUMBER_OF", TokenKind::NUMBER_OF.name());
}

#[test]
fn keyword_table_lookup() {
    // Every keyword lexes to its kind (KW_FALSE + table index) —
    // except `__debug__`, which the lexer flips to KW_TRUE when
    // debug_mode is on (as the Zig original does).
    for (i, kw) in TOK_KW.iter().enumerate() {
        let src = format!("{kw}\n");
        let lex = new_lex(&src);
        let want = if *kw == "__debug__" {
            TokenKind::KW_TRUE
        } else {
            TokenKind::from_u8(TokenKind::KW_FALSE as u8 + i as u8).unwrap()
        };
        assert_eq!(want, lex.tok_kind(), "keyword {kw}");
        assert_eq!(kw.as_bytes(), lex.vstr());
    }
    // async/await are unconditional keywords (ASYNC_AWAIT on).
    assert_eq!(TokenKind::KW_ASYNC, new_lex("async\n").tok_kind());
    assert_eq!(TokenKind::KW_AWAIT, new_lex("await\n").tok_kind());
    // Non-keywords stay NAME — including the soft keywords.
    for name in ["match", "case", "print", "matchx", "x1", "_foo"] {
        let src = format!("{name}\n");
        let lex = new_lex(&src);
        assert_eq!(TokenKind::NAME, lex.tok_kind(), "{name}");
    }
    // The table is sorted by C strcmp, which the lexer's early break
    // relies on.
    for w in TOK_KW.windows(2) {
        assert!(strcmp_slice(w[0].as_bytes(), w[1].as_bytes()) < 0, "{w:?}");
    }
    assert_eq!(36, TOK_KW.len());
}

#[test]
fn operator_table_spot_check() {
    // Every encoded operator/delimiter lexes to its kind. This walks
    // the whole TOK_ENC table through the real scanner.
    let cases: &[(&str, TokenKind)] = &[
        ("=", TokenKind::DEL_EQUAL),
        ("==", TokenKind::OP_DBL_EQUAL),
        ("!=", TokenKind::OP_NOT_EQUAL),
        ("<", TokenKind::OP_LESS),
        ("<=", TokenKind::OP_LESS_EQUAL),
        ("<<", TokenKind::OP_DBL_LESS),
        ("<<=", TokenKind::DEL_DBL_LESS_EQUAL),
        (">", TokenKind::OP_MORE),
        (">=", TokenKind::OP_MORE_EQUAL),
        (">>", TokenKind::OP_DBL_MORE),
        (">>=", TokenKind::DEL_DBL_MORE_EQUAL),
        ("+", TokenKind::OP_PLUS),
        ("+=", TokenKind::DEL_PLUS_EQUAL),
        ("-", TokenKind::OP_MINUS),
        ("-=", TokenKind::DEL_MINUS_EQUAL),
        ("->", TokenKind::DEL_MINUS_MORE),
        ("*", TokenKind::OP_STAR),
        ("*=", TokenKind::DEL_STAR_EQUAL),
        ("**", TokenKind::OP_DBL_STAR),
        ("**=", TokenKind::DEL_DBL_STAR_EQUAL),
        ("/", TokenKind::OP_SLASH),
        ("/=", TokenKind::DEL_SLASH_EQUAL),
        ("//", TokenKind::OP_DBL_SLASH),
        ("//=", TokenKind::DEL_DBL_SLASH_EQUAL),
        ("%", TokenKind::OP_PERCENT),
        ("%=", TokenKind::DEL_PERCENT_EQUAL),
        ("&", TokenKind::OP_AMPERSAND),
        ("&=", TokenKind::DEL_AMPERSAND_EQUAL),
        ("|", TokenKind::OP_PIPE),
        ("|=", TokenKind::DEL_PIPE_EQUAL),
        ("^", TokenKind::OP_CARET),
        ("^=", TokenKind::DEL_CARET_EQUAL),
        ("~", TokenKind::OP_TILDE),
        ("@", TokenKind::OP_AT),
        ("@=", TokenKind::DEL_AT_EQUAL),
        ("(", TokenKind::DEL_PAREN_OPEN),
        (")", TokenKind::DEL_PAREN_CLOSE),
        ("[", TokenKind::DEL_BRACKET_OPEN),
        ("]", TokenKind::DEL_BRACKET_CLOSE),
        ("{", TokenKind::DEL_BRACE_OPEN),
        ("}", TokenKind::DEL_BRACE_CLOSE),
        (",", TokenKind::DEL_COMMA),
        (":", TokenKind::DEL_COLON),
        (";", TokenKind::DEL_SEMICOLON),
        (".", TokenKind::DEL_PERIOD),
        ("...", TokenKind::ELLIPSIS),
        (":=", TokenKind::OP_ASSIGN),
        ("!", TokenKind::INVALID),
    ];
    assert_eq!(48, cases.len());
    for (op, want) in cases {
        let src = format!("{op} x\n");
        let lex = new_lex(&src);
        assert_eq!(*want, lex.tok_kind(), "operator {op:?}");
    }
    // The kind table has one entry per encoding-table entry except
    // the two special-cased first chars ('!' and '.').
    assert_eq!(44, TOK_ENC_KIND.len());
}

#[test]
fn string_escapes_and_numbers() {
    // A grab-bag of literal forms the smoke tests don't cover,
    // asserting the exact vstr the Zig scanner produces.
    let cases: &[(&str, TokenKind, &[u8])] = &[
        ("'\\n'\n", TokenKind::STRING, b"\n"),
        ("'\\t\\r\\\\\\'\\\"'\n", TokenKind::STRING, b"\t\r\\'\""),
        ("'\\a\\b\\v\\f'\n", TokenKind::STRING, b"\x07\x08\x0b\x0c"),
        ("'\\x41\\x4a'\n", TokenKind::STRING, b"AJ"),
        ("'\\u00e9'\n", TokenKind::STRING, "é".as_bytes()), // U+00E9 as UTF-8
        ("'\\101'\n", TokenKind::STRING, b"A"),              // octal
        ("'\\0'\n", TokenKind::STRING, b"\x00"),
        ("r'\\n'\n", TokenKind::STRING, b"\\n"),
        ("b'abc'\n", TokenKind::BYTES, b"abc"),
        ("b'\\x41'\n", TokenKind::BYTES, b"A"),
        ("b'\\u1234'\n", TokenKind::BYTES, b"\\u1234"), // stays literal for bytes
        ("'''a\nb'''\n", TokenKind::STRING, b"a\nb"),
        ("\"\"\"x\"\"\"\n", TokenKind::STRING, b"x"),
        ("'a' 'b'\n", TokenKind::STRING, b"ab"), // adjacent concatenation
        ("'a' b'b'\n", TokenKind::STRING, b"a"), // kind mismatch stops concat
        ("0x1F\n", TokenKind::INTEGER, b"0x1F"),
        ("0b101\n", TokenKind::INTEGER, b"0b101"),
        ("0o17\n", TokenKind::INTEGER, b"0o17"),
        ("1_000\n", TokenKind::INTEGER, b"1000"),
        ("1.5\n", TokenKind::FLOAT_OR_IMAG, b"1.5"),
        (".5\n", TokenKind::FLOAT_OR_IMAG, b".5"),
        ("1e5\n", TokenKind::FLOAT_OR_IMAG, b"1e5"),
        ("1E-5\n", TokenKind::FLOAT_OR_IMAG, b"1e-5"),
        ("2j\n", TokenKind::FLOAT_OR_IMAG, b"2j"),
        ("1_000.5e-2\n", TokenKind::FLOAT_OR_IMAG, b"1000.5e-2"),
    ];
    for (src, want_kind, want_vstr) in cases {
        let lex = new_lex(src);
        assert_eq!(*want_kind, lex.tok_kind(), "{src:?}");
        assert_eq!(*want_vstr, lex.vstr(), "{src:?}");
    }
    // \N{...} is a lexer error (UnicodeNameEscape). It surfaces
    // during construction: the constructor primes the first token.
    let res = Lexer::new_from_str(0, b"'\\N{LATIN SMALL LETTER A}'\n", true);
    assert_eq!(Err(LexError::UnicodeNameEscape), res.map(|_| ()));
    // An unterminated string is LONELY_STRING_OPEN, not a panic.
    let lex = new_lex("'abc\n");
    assert_eq!(TokenKind::LONELY_STRING_OPEN, lex.tok_kind());
    // A dedent mismatch is a token, not a panic.
    let kinds = kinds("if True:\n    pass\n  pass\n");
    assert!(kinds.contains(&TokenKind::DEDENT_MISMATCH), "{kinds:?}");
}

#[test]
fn fstring_rewrite_emits_format_call() {
    // f"{a}" lexes as STRING "{}", then the injected .format((a,),)
    // token stream. This is the f-string apparatus the corpus
    // exercises heavily.
    let lex = new_lex("f'{a}'\n");
    assert_eq!(TokenKind::STRING, lex.tok_kind());
    assert_eq!(b"{}", lex.vstr());
    let rest = kinds("f'{a}'\n");
    assert_eq!(
        vec![
            TokenKind::STRING,
            TokenKind::DEL_PERIOD,
            TokenKind::NAME,
            TokenKind::DEL_PAREN_OPEN,
            TokenKind::DEL_PAREN_OPEN,
            TokenKind::NAME,
            TokenKind::DEL_PAREN_CLOSE,
            TokenKind::DEL_COMMA,
            TokenKind::DEL_PAREN_CLOSE,
            TokenKind::NEWLINE,
            TokenKind::END,
        ],
        rest
    );
    // {{ }} is a literal brace pair in the format string; {a=}
    // injects "a=" before the placeholder.
    assert_eq!(b"{{}}", new_lex("f'{{}}'\n").vstr());
    let lex = new_lex("f'{a=}'\n");
    assert_eq!(b"a={}", lex.vstr());
}

/// Lex every file in the corpus without panicking. The verdict
/// comparison against the original binary lives in
/// `scripts/check_lexer_corpus.py`; this test only proves the lexer
/// never panics or hangs on the corpus and reports the clean/error
/// split. Skipped (pass) when the corpus dir is absent.
#[test]
fn corpus_basics_lexes_without_panic() {
    use std::fs;
    use std::path::Path;

    let dir = Path::new("../petitpy-orig/corpus/basics");
    if !dir.is_dir() {
        eprintln!("corpus_basics_lexes_without_panic: corpus dir not found, skipping");
        return;
    }
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "py"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .py files under corpus/basics");

    let mut clean = 0usize;
    let mut error = 0usize;
    let mut error_files: Vec<String> = Vec::new();
    for path in &files {
        let src = fs::read(path).unwrap();
        match lex_all_clean(&src) {
            Ok(()) => clean += 1,
            Err(()) => {
                error += 1;
                error_files.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    eprintln!(
        "corpus_basics_lexes_without_panic: {clean} lex-clean, {error} lex-error of {}",
        files.len()
    );
    if !error_files.is_empty() {
        eprintln!("lex-error files: {}", error_files.join(", "));
    }
}

/// Lex a whole source to END. `Err` if the lexer reports an error
/// (`LexError`) or emits any error-kind token (INVALID,
/// DEDENT_MISMATCH, LONELY_STRING_OPEN, MALFORMED_FSTRING).
fn lex_all_clean(src: &[u8]) -> Result<(), ()> {
    let mut lex = Lexer::new_from_str(0, src, true).map_err(|_| ())?;
    loop {
        match lex.tok_kind() {
            TokenKind::END => return Ok(()),
            TokenKind::INVALID
            | TokenKind::DEDENT_MISMATCH
            | TokenKind::LONELY_STRING_OPEN
            | TokenKind::MALFORMED_FSTRING => return Err(()),
            _ => {}
        }
        lex.to_next().map_err(|_| ())?;
    }
}
