//! Token-to-node conversion, rule completion, const-bool predicates
//! and tree dumps for the parser.
//!
//! Port of the corresponding section of `src/compiler/parser.zig`
//! (`makeNodeConstObject`, `validateIntLiteral`, `makeTokenPn`,
//! `pushResultToken`, `pushResultRule`, `parseNodeIsConstBool`,
//! `parseNodeExtractList`, `dumpPn` / `parseToDump`).

use super::{
    fold, pn_is_leaf, pn_is_null, pn_is_small_int, pn_is_struct, pn_is_token_kind,
    pn_leaf_arg, pn_leaf_kind, pn_leaf_small_int, pn_new_leaf, pn_new_small_int,
    ConstObjectKind, ConstPayload, ParseError, ParseResult, ParseTree, Parser, Pn, PN_ID,
    PN_STRING, PN_TOKEN,
};
use crate::compiler::bigint;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;

/// Maximum length of a string literal that gets interned as a
/// PN_STRING leaf (vendor `PARSE_INTERN_STRING_LEN`, parse.c).
/// Longer strings become const_object.long_string nodes unless they
/// happen to be interned already.
const PARSE_INTERN_STRING_LEN: usize = 10;

/// Allocate a RULE_const_object node carrying `payload` bytes.
/// Mirrors Zig `makeNodeConstObject` (nodes[0] = kind, nodes[1] =
/// payload len, nodes[2] = chunk pointer — always 0 here; the payload
/// lives in the arena entry, see the module docs).
pub(crate) fn make_node_const_object(
    p: &mut Parser,
    src_line: usize,
    kind: ConstObjectKind,
    payload: &[u8],
) -> ParseResult<Pn> {
    Ok(p.tree.alloc_const_object(
        src_line as u32,
        kind,
        ConstPayload::Bytes(payload.to_vec()),
    ))
}

/// Reject malformed integer literals at parse time, matching vendor's
/// mp_parse_num_base + mp_parse_num_integer semantics. Returns
/// `ParseError::SyntaxError` for:
///   * Leading-zero decimal (`01`, `010`). All-zero strings (`0`,
///     `00`) and zero-with-underscores (`0_0`) remain valid.
///   * Any character bigint.parse_literal can't consume (`123z`,
///     `1abc`, `0xq`). Underscore separators are allowed.
pub(crate) fn validate_int_literal(buf: &[u8]) -> ParseResult<()> {
    if buf.len() >= 2 && buf[0] == b'0' {
        let c2 = buf[1] | 0x20;
        if c2 != b'x' && c2 != b'o' && c2 != b'b' {
            // Decimal-with-leading-zero. Permitted only if every later
            // digit is also zero (or an underscore separator).
            let mut i: usize = 1;
            while i < buf.len() && (buf[i] == b'0' || buf[i] == b'_') {
                i += 1;
            }
            if i < buf.len() {
                return Err(ParseError::SyntaxError);
            }
        }
    }
    let text = std::str::from_utf8(buf).map_err(|_| ParseError::SyntaxError)?;
    match bigint::parse_literal(text) {
        Ok(_) => Ok(()),
        Err(bigint::BigIntError::OutOfMemory) => Err(ParseError::OutOfMemory),
        Err(_) => Err(ParseError::SyntaxError),
    }
}

/// Zig `std.fmt.parseInt(isize, buf, 0)` — base-0 auto-detect
/// (`0x` hex, `0o` octal, `0b` binary, else decimal). Returns None on
/// overflow, on a bad digit (including `_` — Zig's parseInt rejects
/// underscores, so `1_000` falls through to a const_object here just
/// as in the original), or on a non-UTF8 / empty digit run.
fn parse_int_base0(buf: &[u8]) -> Option<isize> {
    let s = std::str::from_utf8(buf).ok()?;
    let (digits, radix): (&str, u32) = if s.len() >= 2 && s.as_bytes()[0] == b'0' {
        match s.as_bytes()[1] | 0x20 {
            b'x' => (&s[2..], 16),
            b'o' => (&s[2..], 8),
            b'b' => (&s[2..], 2),
            _ => (s, 10),
        }
    } else {
        (s, 10)
    };
    isize::from_str_radix(digits, radix).ok()
}

/// Build the parse node corresponding to the lexer's current token.
/// Called whenever the grammar consumes a TOK_* terminal. Mirrors
/// vendor push_result_token() in parse.c:436-499 (via the Zig
/// `makeTokenPn` helper).
pub(crate) fn make_token_pn(p: &mut Parser) -> ParseResult<Pn> {
    // Copy the token text up front: the branches below borrow p's
    // qstr context and tree mutably, and vstr borrows the lexer.
    let buf: Vec<u8> = p.lex.vstr().to_vec();
    let tk = p.lex.tok_kind();
    let src_line = p.lex.tok_line();

    if tk == TokenKind::NAME {
        let id = p.qstr_from_strn(&buf)?;
        return Ok(pn_new_leaf(PN_ID, id as usize));
    } else if tk == TokenKind::INTEGER {
        // Validate against vendor mp_parse_num_base +
        // mp_parse_num_integer. Two malformed shapes the small-int /
        // const-object paths below would silently accept otherwise:
        //   (a) trailing alphabetic ("123z", "1abc") — the bigint
        //       parse rejects it; surfaced as SyntaxError.
        //   (b) leading-zero decimal ("01", "010") — Python 3 /
        //       vendor reject; parseInt would accept silently.
        validate_int_literal(&buf)?;
        // Vendor: small ints fitting in MP_SMALL_INT (31 bits signed
        // in nano config) become MP_PARSE_NODE_SMALL_INT leaves. Only
        // overflow falls through to a const_object. The SMALL_INT
        // shape is what const-fold predicates look at — without this,
        // `while 0:` never short-circuits.
        if let Some(v) = parse_int_base0(&buf) {
            // SMALL_INT carries a 31-bit signed value (low bit is
            // tag). Range check: 30-bit signed in our Pn = usize
            // (after the pnNewSmallInt shift).
            if v >= fold::SMALL_INT_MIN_30 && v <= fold::SMALL_INT_MAX_30 {
                return Ok(pn_new_small_int(v));
            }
        }
        // Overflow / non-decimal big literal: keep as const_object.
        return make_node_const_object(p, src_line, ConstObjectKind::Integer, &buf);
    } else if tk == TokenKind::FLOAT_OR_IMAG {
        return make_node_const_object(p, src_line, ConstObjectKind::FloatOrImag, &buf);
    } else if tk == TokenKind::STRING {
        if buf.len() <= PARSE_INTERN_STRING_LEN {
            let qst = p.qstr_from_strn(&buf)?;
            return Ok(pn_new_leaf(PN_STRING, qst as usize));
        } else {
            // Long string: check whether it happens to be interned
            // already (some test corpora repeat long strings; vendor
            // does the same lookup before falling back to a const
            // node).
            let qst = p.qctx.find_strn(&buf);
            if qst != 0 {
                return Ok(pn_new_leaf(PN_STRING, qst as usize));
            }
            return make_node_const_object(p, src_line, ConstObjectKind::LongString, &buf);
        }
    } else if tk == TokenKind::BYTES {
        return make_node_const_object(p, src_line, ConstObjectKind::BytesLiteral, &buf);
    } else {
        // Generic terminal — keyword, operator, punctuation. Carry
        // the token-kind ordinal in the leaf arg so the compiler can
        // pattern-match on it later.
        return Ok(pn_new_leaf(PN_TOKEN, tk as usize));
    }
}

/// Push the parse node for the lexer's current token onto the result
/// stack. `rule_id` is ignored (COMP_CONST=0 — vendor uses it only
/// for const-folding).
pub(crate) fn push_result_token(p: &mut Parser, rule_id: u8) -> ParseResult<()> {
    let _ = rule_id; // COMP_CONST=0 — vendor uses this only for const-folding
    let pn = make_token_pn(p)?;
    p.push_result_node(pn);
    Ok(())
}

/// Pop `num_args` items off the result stack and assemble them into a
/// new PnStruct of kind `rule_id`. Handles the testlist_comp /
/// atom_paren / testlist_comp_3c special cases that thread parens and
/// trailing-comma tuples through cleanly. Mirrors vendor
/// push_result_rule in parse.c:501-560.
pub(crate) fn push_result_rule(
    p: &mut Parser,
    src_line: usize,
    rule_id: u8,
    num_args_in: usize,
) -> ParseResult<()> {
    let mut num_args = num_args_in;

    if rule_id == grammar::RULE_atom_paren {
        // Collapse parens around a single non-tuple expression — `(x)`
        // is just `x`. Keep them for empty tuples `()` and any tuple
        // literal whose body is RULE_testlist_comp.
        let pn = p.peek_result(0);
        if pn_is_null(pn) {
            // empty tuple — keep parens
        } else if p.tree.is_struct_kind(pn, grammar::RULE_testlist_comp) {
            // tuple — keep parens
        } else {
            return Ok(());
        }
    } else if rule_id == grammar::RULE_testlist_comp {
        // testlist_comp's job is dual-purpose: it's both `(a, b, c)`
        // and `(x for x in ...)`. Disambiguate by peeking at the
        // already-built child.
        let pn = p.peek_result(0);
        if pn_is_struct(pn) {
            let k = p.tree.struct_kind(pn);
            if k == grammar::RULE_testlist_comp_3b {
                let _ = p.pop_result();
                num_args -= 1;
            } else if k == grammar::RULE_testlist_comp_3c {
                let child = p.pop_result();
                // Re-kind the 3c node in place to testlist_comp; the
                // sentinel copy pushed by 3c is still on the stack
                // below, so the result stays (same Pn value).
                p.tree.set_struct_kind(child, rule_id);
                return Ok(());
            } else if k == grammar::RULE_comp_for {
                // generator expression — keep as-is
            } else {
                // tuple with 2 items — keep as-is
            }
        } else {
            // leaf child — tuple with 2 items
        }
    } else if rule_id == grammar::RULE_testlist_comp_3c {
        // Trailing comma adds a sentinel child; account for it.
        num_args += 1;
    }

    // Vendor MICROPY_COMP_CONST_TUPLE fold (parse.c:914-963
    // build_tuple). Try to collapse a tuple literal whose elements
    // are all const into a single RULE_const_object carrying a
    // folded tuple.
    if rule_id == grammar::RULE_testlist_comp {
        if p.peek_rule(0) == grammar::RULE_atom_paren {
            // `(a,)` tuple form. testlist_comp here always has all
            // children on the stack as flat elements.
            if fold::build_tuple_from_stack(p, src_line, num_args)? {
                return Ok(());
            }
        }
    } else if rule_id == grammar::RULE_testlist_comp_3c {
        // `(a, b)` form: testlist_comp wraps a testlist_comp_3b +
        // trailing testlist_comp_3c; if the outer rule is atom_paren
        // we can fold. Vendor discards two rules on success (the
        // testlist_comp and the testlist_comp_3b).
        if p.peek_rule(2) == grammar::RULE_atom_paren {
            if fold::build_tuple_from_stack(p, src_line, num_args)? {
                let top = p.rule_stack.len();
                p.rule_stack.truncate(top - 2);
                return Ok(());
            }
        }
    } else if rule_id == grammar::RULE_testlist_star_expr
        || rule_id == grammar::RULE_testlist
        || rule_id == grammar::RULE_subscriptlist
    {
        // Bare tuple contexts: `x = a, b`, `return a, b`,
        // `for x in a, b: pass`, `x[a, b]`.
        if fold::build_tuple_from_stack(p, src_line, num_args)? {
            return Ok(());
        }
    }

    // Vendor MICROPY_COMP_CONST_FOLDING fold_logical_constants
    // (parse.c:687). Handles or_test/and_test short-circuit folding
    // and not_test_2 inversion. Updates num_args via the in/out
    // pointer; if fully folded (only 1 operand left), the operand
    // stays on the stack with no wrapper.
    if fold::fold_logical_constants(p, rule_id, &mut num_args)? {
        return Ok(());
    }

    // Vendor MICROPY_COMP_CONST_FOLDING fold_constants (parse.c:744).
    // Minimal small-int port: covers factor_2 (unary +/-/~), the
    // bitwise binary rules expr/xor_expr/and_expr, and the per-token
    // shift_expr/arith_expr/term. Float / float-returning ops (/,
    // matrix @) skip; big-int results that don't fit SMALL_INT skip.
    // Power fold is deferred (results blow up quickly).
    if fold::fold_binary_small_int(p, src_line, rule_id, num_args)? {
        return Ok(());
    }
    if fold::try_fold_big_int_binary(p, src_line, rule_id, num_args)? {
        return Ok(());
    }
    if rule_id == grammar::RULE_factor_2 && num_args == 2 {
        let operand = p.peek_result(0);
        let op_tok = p.peek_result(1);
        if pn_is_small_int(operand)
            && pn_is_leaf(op_tok)
            && pn_leaf_kind(op_tok) == PN_TOKEN
        {
            let tk: u16 = pn_leaf_arg(op_tok) as u16;
            let op_plus: u16 = TokenKind::OP_PLUS as u16;
            let op_minus: u16 = TokenKind::OP_MINUS as u16;
            let op_tilde: u16 = TokenKind::OP_TILDE as u16;
            let v = pn_leaf_small_int(operand);
            let mut folded: Option<isize> = None;
            if tk == op_plus {
                folded = Some(v);
            } else if tk == op_minus {
                // v is a 30-bit small int (all SMALL_INT leaves are
                // range-gated at creation), so -v cannot overflow.
                folded = Some(-v);
            } else if tk == op_tilde {
                folded = Some(!v);
            }
            if let Some(fv) = folded {
                // Range gate matches the makeTokenPn SMALL_INT path.
                if fv >= fold::SMALL_INT_MIN_30 && fv <= fold::SMALL_INT_MAX_30 {
                    let _ = p.pop_result(); // operand
                    let _ = p.pop_result(); // op_tok
                    p.push_result_node(pn_new_small_int(fv));
                    return Ok(());
                }
            }
        }
    }
    if fold::try_fold_big_int_unary(p, src_line, rule_id, num_args)? {
        return Ok(());
    }

    // Struct assembly: pop num_args items (newest first) and reverse
    // to source order.
    let mut children: Vec<Pn> = Vec::with_capacity(num_args);
    for _ in 0..num_args {
        children.push(p.pop_result());
    }
    children.reverse();
    let pn = p.tree.alloc_struct(src_line as u32, rule_id, children);

    if rule_id == grammar::RULE_testlist_comp_3c {
        // testlist_comp_3c is the only rule that pushes its struct
        // node *twice*: once as its own result and once as the
        // trailing sentinel consumed by testlist_comp above.
        p.push_result_node(pn);
    }
    p.push_result_node(pn);
    Ok(())
}

// ── Const-bool predicates ───────────────────────────────────────────────
//
// Vendor parse_node_is_const_false / _true (parse.c). The Zig original
// is a free function on the raw Pn (it dereferences the struct
// pointer); here it is a ParseTree method because struct access goes
// through the arena.

impl ParseTree {
    pub(crate) fn parse_node_is_const_bool(&self, pn: Pn, value: bool) -> bool {
        let kw_kind: u16 = if value {
            TokenKind::KW_TRUE as u16
        } else {
            TokenKind::KW_FALSE as u16
        };
        if pn_is_token_kind(pn, kw_kind) {
            return true;
        }
        if pn_is_small_int(pn) {
            return (pn_leaf_small_int(pn) != 0) == value;
        }
        // ELLIPSIS is always truthy.
        if pn_is_leaf(pn) && pn_leaf_kind(pn) == PN_TOKEN {
            let t: u16 = pn_leaf_arg(pn) as u16;
            if t == (TokenKind::ELLIPSIS as u16) {
                return value;
            }
        }
        // Empty atom_paren — vendor parse_node_is_const_false: `()`
        // is falsy. Otherwise atom_paren isn't a const literal.
        if self.is_struct_kind(pn, grammar::RULE_atom_paren) {
            if pn_is_null(self.struct_nodes(pn)[0]) {
                return !value;
            }
            return false;
        }
        // String literal interned as STRING leaf — non-empty literal
        // is truthy (the empty-string qstr is MP_QSTR_ id 1; vendor
        // treats it as falsy for and/or short-circuit).
        if pn_is_leaf(pn) && pn_leaf_kind(pn) == PN_STRING {
            let q: usize = pn_leaf_arg(pn);
            return value == (q != 1);
        }
        // Const_object: discriminate by kind. Integer 0 → falsy;
        // tuple empty → falsy; string/bytes empty → falsy; everything
        // else truthy.
        if self.is_struct_kind(pn, grammar::RULE_const_object) {
            let nodes = self.struct_nodes(pn);
            let k = match ConstObjectKind::from_u32(nodes[0] as u32) {
                Some(k) => k,
                None => return false,
            };
            let len: usize = nodes[1];
            match k {
                ConstObjectKind::Integer => {
                    // Examine the decimal/oct/hex text; vendor parses
                    // to mp_int and compares to 0. For our purposes,
                    // "0", "0x0", "0o0", "0b0" mean falsy; any
                    // literal whose chars aren't all 0/x/o/b/_ is
                    // truthy.
                    if len == 0 {
                        return !value;
                    }
                    match self.const_payload(pn) {
                        Some(ConstPayload::Bytes(bytes)) => {
                            let mut truthy = false;
                            for &c in bytes.iter() {
                                match c {
                                    b'0' | b'x' | b'X' | b'o' | b'O' | b'b' | b'B' | b'_'
                                    | b'+' | b'-' => {}
                                    _ => {
                                        truthy = true;
                                        break;
                                    }
                                }
                            }
                            value == truthy
                        }
                        _ => !value,
                    }
                }
                ConstObjectKind::LongString | ConstObjectKind::BytesLiteral => {
                    value == (len > 0)
                }
                // Any float literal is "truthy" for fold purposes
                // (matches vendor).
                ConstObjectKind::FloatOrImag => value,
                ConstObjectKind::Tuple => value == (len > 0),
            }
        } else {
            false
        }
    }
}

// ── Tree dumps ──────────────────────────────────────────────────────────
//
// `ParseTree::dump_wire` — the rich wire format for the differential
// harness (examples/parse_dump.rs + the scratch Zig dump program in
// scripts/check_parser_zigdiff.py): one line per node, preorder,
// 2-space indent per depth:
//   rule <id> <line>
//   small_int <v>
//   id <hex>          (qstr id, lowercase hex)
//   string <hex>      (qstr id, lowercase hex)
//   token <ord>
//   null
//   const_object <kind> <len> <hex>   (bytes payload; for a
//     folded tuple: `const_object 5 <n>` then the n children
//     dumped at depth+1)
//
// (The Zig test helpers' short dump — rule short names, `leaf` /
// `null` — lives in tests.rs, as in the original where dumpPn /
// parseToDump sit in the test section.)

fn write_hex(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
}

fn dump_wire_pn(tree: &ParseTree, pn: Pn, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    if pn_is_null(pn) {
        out.push_str("null\n");
        return;
    }
    if pn_is_small_int(pn) {
        out.push_str(&format!("small_int {}\n", pn_leaf_small_int(pn)));
        return;
    }
    if pn_is_leaf(pn) {
        match pn_leaf_kind(pn) {
            PN_ID => out.push_str(&format!("id {:x}\n", pn_leaf_arg(pn))),
            PN_STRING => out.push_str(&format!("string {:x}\n", pn_leaf_arg(pn))),
            PN_TOKEN => out.push_str(&format!("token {}\n", pn_leaf_arg(pn))),
            _ => out.push_str(&format!("leaf {:x} {:x}\n", pn_leaf_kind(pn), pn_leaf_arg(pn))),
        }
        return;
    }
    // Struct node.
    if tree.is_struct_kind(pn, grammar::RULE_const_object) {
        let nodes = tree.struct_nodes(pn);
        let kind = nodes[0];
        let len = nodes[1];
        match tree.const_payload(pn) {
            Some(ConstPayload::Bytes(bytes)) => {
                out.push_str(&format!("const_object {kind} {len} "));
                write_hex(out, bytes);
                out.push('\n');
            }
            Some(ConstPayload::Tuple(items)) => {
                out.push_str(&format!("const_object {kind} {len}\n"));
                for &child in items {
                    dump_wire_pn(tree, child, depth + 1, out);
                }
            }
            None => {
                out.push_str(&format!("const_object {kind} {len}\n"));
            }
        }
        return;
    }
    out.push_str(&format!(
        "rule {} {}\n",
        tree.struct_kind(pn),
        tree.struct_source_line(pn)
    ));
    for &child in tree.struct_nodes(pn) {
        dump_wire_pn(tree, child, depth + 1, out);
    }
}

impl ParseTree {
    /// Rich wire-format dump of the whole tree (root at depth 0),
    /// for the differential harness.
    pub fn dump_wire(&self) -> String {
        let mut out = String::new();
        dump_wire_pn(self, self.root, 0, &mut out);
        out
    }
}
