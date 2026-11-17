//! Unit tests for the re1.5 port (`src/vendor/re15.rs`).
//!
//! The differential probe battery (`re15_probe.rs`, driven by
//! `scripts/re15_vectors.txt`) is the main conformance check against
//! the reference binary; these tests pin the per-opcode behaviour,
//! the boundary limits, and the error kinds directly.

use crate::vendor::re15::*;

fn compile(pat: &[u8]) -> ByteProg {
    let mut prog = ByteProg::new();
    compilecode(&mut prog, pat).unwrap();
    prog
}

fn compile_err(pat: &[u8]) -> CompileErrorKind {
    let mut prog = ByteProg::new();
    compilecode(&mut prog, pat).unwrap_err().kind
}

/// Run `pat` on `subj`; return the capture spans as
/// `(start, end)` per group (group 0 first), `-1` for a group that
/// did not participate, or `None` for no match.
fn run(pat: &[u8], subj: &[u8], anchored: bool) -> Option<Vec<(i64, i64)>> {
    let prog = compile(pat);
    let mut caps = vec![Sub::new(); prog.sub + 1];
    if recursiveloopprog(&prog, subj, &mut caps, anchored) {
        Some(
            caps.iter()
                .map(|s| match (s.start, s.end) {
                    (Some(a), Some(b)) => (a as i64, b as i64),
                    _ => (-1, -1),
                })
                .collect(),
        )
    } else {
        None
    }
}

fn search(pat: &[u8], subj: &[u8]) -> Option<Vec<(i64, i64)>> {
    run(pat, subj, false)
}

fn matches(pat: &[u8], subj: &[u8]) -> Option<Vec<(i64, i64)>> {
    run(pat, subj, true)
}

#[test]
fn opcode_char_and_any() {
    assert_eq!(matches(b"abc", b"abc"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"abc", b"abd"), None);
    assert_eq!(search(b"bc", b"abc"), Some(vec![(1, 3)]));
    assert_eq!(search(b"z", b"abc"), None);
    // Any matches every byte, including '\n' (no DOTALL concept).
    assert_eq!(matches(b".", b"\n"), Some(vec![(0, 1)]));
    assert_eq!(matches(b".", b"\x80"), Some(vec![(0, 1)]));
    assert_eq!(matches(b".", b""), None);
    // Escaped metacharacters are literals.
    assert_eq!(matches(b"a\\.c", b"a.c"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"a\\.c", b"abc"), None);
    assert_eq!(matches(b"\\\\", b"\\"), Some(vec![(0, 1)]));
    // \n is not special: backslash + 'n' is the literal 'n'.
    assert_eq!(matches(b"\\n", b"n"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"\\n", b"\n"), None);
}

#[test]
fn opcode_class() {
    assert_eq!(matches(b"[a-z]+", b"abc"), Some(vec![(0, 3)]));
    // Anchored (not full) match: the prefix "ab" matches, "1" is left.
    assert_eq!(matches(b"[a-z]+", b"ab1"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"[^a-z]+", b"123"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"[^a-z]", b"a"), None);
    // Multiple ranges in one class.
    assert_eq!(matches(b"[a-cu-z]", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"[a-cu-z]", b"z"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"[a-cu-z]", b"d"), None);
    assert_eq!(matches(b"[a-cu-z]", b"A"), None);
    assert_eq!(matches(b"[^a-cu-z]", b"d"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"[^a-cu-z]", b"a"), None);
    // '-' at the edges is literal (re1.py cases).
    assert_eq!(matches(b"[-a]+", b"-a"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"[a-]+", b"-a"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"[a\\-z]", b"-"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"[a\\-z]", b"f"), None);
    assert_eq!(matches(b"[a\\]z]", b"a"), Some(vec![(0, 1)]));
    // Escaped ']' inside a class.
    assert_eq!(matches(b"[.\\]]*", b".].]a"), Some(vec![(0, 4)]));
    // Empty class never matches; negated empty class always matches.
    assert_eq!(matches(b"[]", b"a"), None);
    assert_eq!(matches(b"[]a", b"a"), None);
    assert_eq!(matches(b"[^]", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"[^]x", b"ax"), Some(vec![(0, 2)]));
    // Ranges compare as *signed* chars (charclass.c): 0x80 is -128.
    assert_eq!(matches(b"[\x80-\xff]+", b"\x80\xff"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"[a-z]", b"\x80"), None);
    // A NUL byte in a pattern is end-of-string to the C (patterns are
    // NUL-terminated), so "[\x00-..." is just "[" — unterminated.
    assert_eq!(compile_err(b"[\x00-\x7f]"), CompileErrorKind::UnterminatedClass);
}

#[test]
fn opcode_named_class() {
    for (pat, good, bad) in [
        (b"\\d".as_slice(), b"5".as_slice(), b"a".as_slice()),
        (b"\\D".as_slice(), b"a".as_slice(), b"5".as_slice()),
        (b"\\s".as_slice(), b" ".as_slice(), b"a".as_slice()),
        (b"\\S".as_slice(), b"a".as_slice(), b" ".as_slice()),
        (b"\\w".as_slice(), b"_".as_slice(), b"-".as_slice()),
        (b"\\W".as_slice(), b"-".as_slice(), b"_".as_slice()),
    ] {
        assert_eq!(matches(pat, good), Some(vec![(0, 1)]), "{:?}", pat);
        assert_eq!(matches(pat, bad), None, "{:?}", pat);
    }
    // Uppercase variants are the negated class.
    assert_eq!(matches(b"\\d+", b"123"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"\\D+", b"abc"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"\\s+", b" \t\r"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"\\S+", b"ab"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"\\w+", b"a1_"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"\\W+", b"-.="), Some(vec![(0, 3)]));
    // High bytes are not in any named class.
    assert_eq!(matches(b"\\d", b"\x80"), None);
    assert_eq!(matches(b"\\w", b"\x80"), None);
    assert_eq!(matches(b"\\D", b"\x80"), Some(vec![(0, 1)]));
    // Named class inside a set (re_namedclass.py).
    assert_eq!(matches(b"[\\d]+", b"123"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"[\\d]+", b"abc"), None);
    assert_eq!(matches(b"[^\\D]+", b"123"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"[^\\D]+", b"abc"), None);
    assert_eq!(matches(b"([\\s\\d]+)([\\W]+)", b"1  2-+="), Some(vec![(0, 7), (0, 4), (4, 7)]));
}

#[test]
fn opcode_anchors() {
    assert_eq!(matches(b"^a", b"a"), Some(vec![(0, 1)]));
    assert_eq!(search(b"^b", b"abc"), None);
    assert_eq!(search(b"c$", b"abc"), Some(vec![(2, 3)]));
    assert_eq!(search(b"b$", b"abc"), None);
    assert_eq!(matches(b"^ab$", b"ab"), Some(vec![(0, 2)]));
    assert_eq!(matches(b"^ab$", b"abc"), None);
    // ^ anchors to begin_line, not to the search start (Subject).
    let prog = compile(b"^o");
    let subj = Subject::new(b"dog", 0, 1, 3);
    let mut caps = vec![Sub::new(); prog.sub + 1];
    assert!(!recursiveloopprog_subject(&prog, &subj, &mut caps, true));
    let prog = compile(b"o");
    let mut caps = vec![Sub::new(); prog.sub + 1];
    assert!(recursiveloopprog_subject(&prog, &subj, &mut caps, true));
    assert_eq!(caps[0].start, Some(1));
    // $ anchors to end (endpos).
    let prog = compile(b"o$");
    let subj = Subject::new(b"dog", 0, 0, 2);
    let mut caps = vec![Sub::new(); prog.sub + 1];
    assert!(recursiveloopprog_subject(&prog, &subj, &mut caps, false));
    let subj = Subject::new(b"dog", 0, 0, 3);
    let mut caps = vec![Sub::new(); prog.sub + 1];
    assert!(!recursiveloopprog_subject(&prog, &subj, &mut caps, false));
}

#[test]
fn quantifiers_greedy_and_lazy() {
    assert_eq!(matches(b"a?", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a?", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a*", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a*", b"aaa"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"a+", b""), None);
    assert_eq!(matches(b"a+", b"aaa"), Some(vec![(0, 3)]));
    // Lazy: prefer the shorter match.
    assert_eq!(matches(b"a+?", b"aaa"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a*?", b"aaa"), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a??", b"aaa"), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a+?", b"ab"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a*?", b"ab"), Some(vec![(0, 0)]));
    // Lazy group still finds the rest.
    assert_eq!(matches(b"a+?b", b"aab"), Some(vec![(0, 3)]));
    assert_eq!(matches(b".*?", b"abc"), Some(vec![(0, 0)]));
    assert_eq!(matches(b".*?b", b"abc"), Some(vec![(0, 2)]));
    // Quantifiers on classes and groups.
    assert_eq!(matches(b"[0-9]+", b"123"), Some(vec![(0, 3)]));
    assert_eq!(matches(b"(ab)+", b"abab"), Some(vec![(0, 4), (2, 4)]));
    assert_eq!(matches(b"(ab)+?", b"abab"), Some(vec![(0, 2), (0, 2)]));
    // Backtracking: first group gives back one 'a'.
    assert_eq!(matches(b"(a+)(a+)", b"aaa"), Some(vec![(0, 3), (0, 2), (2, 3)]));
    assert_eq!(matches(b"(a|ab)(c|bcd)", b"abcd"), Some(vec![(0, 4), (0, 1), (1, 4)]));
    // NOTE: `(a*)*` is deliberately NOT tested here: the group can
    // match empty forever, so both engines recurse without bound —
    // the C aborts via mp_cstack_check (verified: reference binary
    // rc=134 on `(a*)*` vs "aaa") and the Rust port overflows the
    // thread stack. See the result file.
}

#[test]
fn alternation() {
    assert_eq!(matches(b"a|b", b"b"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a|b|c", b"c"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a|b|c", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a|", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"|a", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"^a|b$", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"^a|b$", b"b"), Some(vec![(0, 1)]));
    assert_eq!(search(b"^b|b$", b"abc"), None);
    assert_eq!(matches(b"(a|b)*", b"abba"), Some(vec![(0, 4), (3, 4)]));
    assert_eq!(matches(b"a(b|c)d", b"acd"), Some(vec![(0, 3), (1, 2)]));
    // Long alternation needs int16 offsets (patched engine).
    let mut long = vec![b'x'; 150];
    long.extend_from_slice(b"|y");
    assert_eq!(search(&long, b"y"), Some(vec![(0, 1)]));
    assert_eq!(search(&long, &[b'x'; 150]), Some(vec![(0, 150)]));
}

#[test]
fn groups() {
    // Nested groups (re_group.py).
    assert_eq!(
        matches(b"(([0-9]*)([a-z]*)[0-9]*)", b"1234hello567"),
        Some(vec![(0, 12), (0, 12), (0, 4), (4, 9)])
    );
    assert_eq!(
        matches(b"([0-9]*)(([a-z]*)([0-9]*))", b"1234hello567"),
        Some(vec![(0, 12), (0, 4), (4, 12), (4, 9), (9, 12)])
    );
    // Optional group that matches / doesn't match.
    assert_eq!(matches(b"(a)?b(c)", b"abc"), Some(vec![(0, 3), (0, 1), (2, 3)]));
    assert_eq!(matches(b"(a)?b(c)", b"bc"), Some(vec![(0, 2), (-1, -1), (1, 2)]));
    // Non-capturing groups don't allocate slots.
    assert_eq!(matches(b"(?:a)(b)", b"ab"), Some(vec![(0, 2), (1, 2)]));
    assert_eq!(matches(b"(?:ab)+", b"abab"), Some(vec![(0, 4)]));
    let prog = compile(b"(?:a)(b)");
    assert_eq!(prog.sub, 1);
    let prog = compile(b"(a)(?:b)(c)");
    assert_eq!(prog.sub, 2);
    // Unmatched groups report (-1, -1) for span (modre.c: -1, -1).
    assert_eq!(matches(b"(a)|(b)", b"b"), Some(vec![(0, 1), (-1, -1), (0, 1)]));
}

#[test]
fn group_count_boundary() {
    // 63 groups: 2*63 = 126 and 2*63+1 = 127 fit in int8.
    let pat63 = "(a)".repeat(63);
    assert!(compilecode(&mut ByteProg::new(), pat63.as_bytes()).is_ok());
    // 64 groups: 2*64 = 128 overflows the int8 Save slot.
    assert_eq!(compile_err("(a)".repeat(64).as_bytes()), CompileErrorKind::SlotOverflow);
    assert_eq!(compile_err("(a)".repeat(256).as_bytes()), CompileErrorKind::SlotOverflow);
}

#[test]
fn class_count_boundary() {
    // 127 pairs fit in the int8 count byte; 128 do not.
    assert!(compilecode(&mut ByteProg::new(), format!("[{}]", "a".repeat(127)).as_bytes()).is_ok());
    assert_eq!(
        compile_err(format!("[{}]", "a".repeat(128)).as_bytes()),
        CompileErrorKind::SlotOverflow
    );
    assert_eq!(
        compile_err(format!("[{}]", "a".repeat(256)).as_bytes()),
        CompileErrorKind::SlotOverflow
    );
}

#[test]
fn jump_offset_boundary() {
    // int16 offsets: a 16000-'a' group under '?' compiles, a
    // 17000-'a' group overflows (offset ~34000 > 32767).
    let ok = "(".to_string() + &"a".repeat(16000) + ")?";
    assert!(compilecode(&mut ByteProg::new(), ok.as_bytes()).is_ok());
    let big = "(".to_string() + &"a".repeat(17000) + ")?";
    assert_eq!(compile_err(big.as_bytes()), CompileErrorKind::OffsetOverflow);
    // Long alternation compiles (vanilla int8 would fail at ~127).
    let long = "b".repeat(5000) + "|a";
    assert!(compilecode(&mut ByteProg::new(), long.as_bytes()).is_ok());
}

#[test]
fn empty_pattern_and_subject() {
    assert_eq!(matches(b"", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"", b"abc"), Some(vec![(0, 0)]));
    assert_eq!(search(b"", b"abc"), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a*", b""), Some(vec![(0, 0)]));
    assert_eq!(matches(b"a+", b""), None);
    assert_eq!(matches(b".*", b""), Some(vec![(0, 0)]));
    assert_eq!(search(b"x*", b"foo"), Some(vec![(0, 0)]));
    // Empty group matches empty.
    assert_eq!(matches(b"()", b""), Some(vec![(0, 0), (0, 0)]));
    assert_eq!(matches(b"(?:)", b"abc"), Some(vec![(0, 0)]));
}

#[test]
fn nul_byte_truncates_pattern() {
    // The C pattern is a NUL-terminated string: "a\x00b" compiles
    // exactly like "a".
    let p1 = compile(b"a\x00b");
    let p2 = compile(b"a");
    assert_eq!(p1, p2);
    assert_eq!(matches(b"a\x00b", b"a"), Some(vec![(0, 1)]));
    assert_eq!(matches(b"a\x00b", b"ab"), Some(vec![(0, 1)]));
}

#[test]
fn invalid_patterns() {
    for (pat, kind) in [
        (b"?".as_slice(), CompileErrorKind::NothingToRepeat),
        (b"*".as_slice(), CompileErrorKind::NothingToRepeat),
        (b"+".as_slice(), CompileErrorKind::NothingToRepeat),
        (b")".as_slice(), CompileErrorKind::UnexpectedCloseParen),
        (b"a)".as_slice(), CompileErrorKind::UnexpectedCloseParen),
        (b"(".as_slice(), CompileErrorKind::UnmatchedParen),
        (b"([".as_slice(), CompileErrorKind::UnterminatedClass),
        (b"([)".as_slice(), CompileErrorKind::UnterminatedClass),
        (b"[a".as_slice(), CompileErrorKind::UnterminatedClass),
        (b"[a\\".as_slice(), CompileErrorKind::UnterminatedClass),
        (b"a\\".as_slice(), CompileErrorKind::TrailingBackslash),
        (b"(a".as_slice(), CompileErrorKind::UnmatchedParen),
        (b"(?".as_slice(), CompileErrorKind::NothingToRepeat),
        (b"(?:".as_slice(), CompileErrorKind::UnmatchedParen),
        (b"(a))".as_slice(), CompileErrorKind::UnexpectedCloseParen),
        (b"a**".as_slice(), CompileErrorKind::NothingToRepeat),
    ] {
        assert_eq!(compile_err(pat), kind, "pattern {:?}", String::from_utf8_lossy(pat));
    }
    // sizecode agrees with compilecode, except the C quirk: sizecode
    // never checks the trailing ')', so "a)" has a size but fails
    // to compile (modre.c raises "error in regex" either way).
    assert_eq!(sizecode(b"a)"), Some(14));
    assert_eq!(sizecode(b"("), None);
    assert_eq!(sizecode(b"?"), None);
    assert_eq!(sizecode(b"a"), Some(14));
}

#[test]
fn sizecode_and_prog_fields() {
    // "a": prefix 7 + Save0 2 + Char 2 + Save1 2 + Match 1 = 14.
    assert_eq!(sizecode(b"a"), Some(14));
    assert_eq!(sizecode(b""), Some(12));
    assert_eq!(sizecode(b"ab"), Some(16));
    let prog = compile(b"(a)(b)");
    assert_eq!(prog.bytelen, prog.insts.len());
    assert_eq!(prog.sub, 2);
    // insts: prefix 3 + Save0 1 + (Save+Char+Save) 3 + (Save+Char+Save) 3 + Save1 1 + Match 1.
    assert_eq!(prog.len, 3 + 1 + 3 + 3 + 1 + 1);
    assert_eq!(prog.bytelen, 7 + 2 + 6 + 6 + 2 + 1);
}

#[test]
fn dumpcode_exact() {
    // Hand-computed from the patched compilecode.c + dumpcode.c.
    let prog = compile(b"a");
    assert_eq!(
        dumpcode(&prog),
        concat!(
            " 0: rsplit 7 (4)\n",
            " 3: any\n",
            " 4: jmp 0 (-7)\n",
            " 7: save 0\n",
            " 9: char a\n",
            "11: save 1\n",
            "13: match\n",
            "Bytes: 14, insts: 7\n",
        )
    );
    let prog = compile(b"^a|b[0-9]\\w$");
    assert_eq!(
        dumpcode(&prog),
        concat!(
            " 0: rsplit 7 (4)\n",
            " 3: any\n",
            " 4: jmp 0 (-7)\n",
            " 7: save 0\n",
            " 9: split 18 (6)\n",
            "12: assert bol\n",
            "13: char a\n",
            "15: jmp 27 (9)\n",
            "18: char b\n",
            "20: class 1 0x30-0x39\n",
            "24: namedclass w\n",
            "26: assert eol\n",
            "27: save 1\n",
            "29: match\n",
            "Bytes: 30, insts: 14\n",
        )
    );
    // Class dump with a high byte: C prints the sign-extended %02x.
    let prog = compile(b"[\x80-\xff]");
    assert!(dumpcode(&prog).contains("0xffffff80-0xffffffff"));
}
