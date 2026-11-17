//! Ported test blocks from `src/compiler/parser.zig` (8 blocks:
//! 1 SKIP, 4 smoke tests, 3 match tests with verbatim expected dumps).

use super::names::rule_name;
use super::{parse, pn_is_null, pn_is_struct, InputKind, ParseError, ParseTree, Pn};
use crate::compiler::lexer::Lexer;
use crate::compiler::qstr::{QstrContext, N_TOTAL};

/// Zig `dumpPn` — readable tree dump (rule short names, `leaf` /
/// `null` for non-structs).
fn dump_pn(tree: &ParseTree, pn: Pn, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    if !pn_is_struct(pn) {
        // Distinguishing the two matters: an absent opt_rule slot is
        // PN_NULL, and a test that called it "leaf" could not tell a
        // missing guard from a present one.
        out.push_str(if pn_is_null(pn) { "null\n" } else { "leaf\n" });
        return;
    }
    out.push_str(rule_name(tree.struct_kind(pn)));
    out.push('\n');
    for &child in tree.struct_nodes(pn) {
        dump_pn(tree, child, depth + 1, out);
    }
}

/// Zig `parseToDump` — parse `src` as file_input and dump the tree.
fn parse_to_dump(src: &[u8]) -> Result<String, ParseError> {
    let mut qctx = QstrContext::new();
    let mut lex = Lexer::new_from_str(0, src, true).map_err(ParseError::from)?;
    let tree = parse(&mut lex, &mut qctx, InputKind::FileInput)?;
    let mut out = String::new();
    dump_pn(&tree, tree.root, 0, &mut out);
    Ok(out)
}

fn parse_src(src: &[u8]) -> Result<ParseTree, ParseError> {
    let mut qctx = QstrContext::new();
    let mut lex = Lexer::new_from_str(0, src, true).map_err(ParseError::from)?;
    parse(&mut lex, &mut qctx, InputKind::FileInput)
}

/// Zig: `test "smoke: empty file_input parses to a single struct"`.
///
/// SKIPPED upstream — the empty-file expectation was stale (the
/// parser returns PN_NULL, not a struct, for empty input) and was
/// left to be re-derived with the empty-program handler in a later
/// phase. Ported as `#[ignore]` so the block is not lost.
#[test]
#[ignore]
fn smoke_empty_file_input_parses_to_single_struct() {
    let tree = parse_src(b"").expect("empty source parses");
    assert!(tree.root != 0, "root should be a struct node");
}

#[test]
fn smoke_pass_yields_file_input_wrapping_pass_stmt() {
    let tree = parse_src(b"pass\n").expect("pass parses");
    assert!(tree.root != 0, "root should be a struct node");
}

#[test]
fn smoke_x_eq_1_produces_struct_node() {
    let tree = parse_src(b"x = 1\n").expect("x = 1 parses");
    assert!(tree.root != 0, "root should be a struct node");
}

#[test]
fn smoke_syntax_error_on_trailing_equals() {
    let err = parse_src(b"x =\n").expect_err("trailing = must fail");
    assert_eq!(err, ParseError::SyntaxError);
}

#[test]
fn smoke_identifier_interning_works_through_parser() {
    let mut qctx = QstrContext::new();
    let mut lex = Lexer::new_from_str(0, b"foo\n", true).expect("lexer");
    let _tree = parse(&mut lex, &mut qctx, InputKind::FileInput).expect("foo parses");
    // After parse, `foo` must be interned in the dynamic pool.
    let id = qctx.find_strn(b"foo");
    assert!(id >= N_TOTAL, "foo id {id} should be dynamic (>= {N_TOTAL})");
}

#[test]
fn match_statement_parses_to_extension_rule_tree() {
    let dump = parse_to_dump(
        b"match x:\n    case 1:\n        pass\n    case _:\n        pass\n",
    )
    .expect("match statement parses");
    assert_eq!(
        dump,
        "match_stmt\n  leaf\n  leaf\n  match_stmt_case_list\n    match_stmt_case\n      leaf\n      leaf\n      null\n      pass_stmt\n    match_stmt_case\n      leaf\n      leaf\n      null\n      pass_stmt\n"
    );
}

#[test]
fn match_guards_as_bindings_and_open_sequences_reach_their_own_rules() {
    // No match_stmt_case_list here: a one_or_more rule with a single
    // element collapses to that element, as it does everywhere else in
    // this grammar. The two-case test above shows the list node.
    let dump = parse_to_dump(b"match p:\n    case P() as q, 2 if g:\n        pass\n")
        .expect("match with guard parses");
    assert_eq!(
        dump,
        "match_stmt\n  leaf\n  leaf\n  match_stmt_case\n    leaf\n    match_stmt_patterns\n      match_stmt_pattern\n        atom_expr_normal\n          leaf\n          trailer_paren\n            null\n        match_stmt_as\n          leaf\n      leaf\n    match_stmt_guard\n      leaf\n    pass_stmt\n"
    );
}

#[test]
fn match_as_plain_name_never_reaches_extension_rules() {
    // The whole point of the soft-keyword hook — each of these is
    // ordinary code that happens to use `match` as an identifier.
    let cases: &[&[u8]] = &[
        b"match = 1\n",
        b"match(x)\n",
        b"match.group(0)\n",
        b"match: int = 1\n",
        b"match = {1: 2}\n",
        b"print(match)\n",
    ];
    for src in cases {
        let dump = parse_to_dump(src).expect("plain-name match parses");
        assert!(
            !dump.contains("match_stmt"),
            "source {src:?} must not reach match_stmt, got:\n{dump}"
        );
    }
}
