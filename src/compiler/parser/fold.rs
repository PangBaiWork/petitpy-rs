//! Constant-folding helpers for the parser.
//!
//! Port of the folding section of `src/compiler/parser.zig`
//! (`pnIsConst`, `makeNodeConstTuple`, `foldSmallIntBinOp`,
//! `foldBinarySmallInt`, the big-int fallback, `foldLogicalConstants`,
//! `buildTupleFromStack`).

use super::{
    pn_is_leaf, pn_is_small_int, pn_leaf_arg, pn_leaf_kind, pn_leaf_small_int,
    pn_new_small_int, ConstObjectKind, ConstPayload, ParseError, ParseResult, ParseTree, Parser,
    Pn, PN_STRING, PN_TOKEN,
};
use crate::compiler::bigint;
use crate::compiler::build_options::IS_FULL;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;

/// Predicate: is `pn` a const-foldable parse node? Vendor
/// mp_parse_node_is_const (parse.c:373) accepts SMALL_INT, STRING leaf,
/// KW_TRUE/FALSE/NONE/ELLIPSIS tokens, RULE_const_object, and
/// RULE_atom_paren whose body is NULL (the empty tuple `()`).
pub(crate) fn pn_is_const(tree: &ParseTree, pn: Pn) -> bool {
    if pn_is_small_int(pn) {
        return true;
    }
    if pn_is_leaf(pn) {
        let lk = pn_leaf_kind(pn);
        if lk == PN_STRING {
            return true;
        }
        if lk == PN_TOKEN {
            let tk = pn_leaf_arg(pn) as u16;
            return tk == (TokenKind::KW_TRUE as u16)
                || tk == (TokenKind::KW_FALSE as u16)
                || tk == (TokenKind::KW_NONE as u16)
                || tk == (TokenKind::ELLIPSIS as u16);
        }
        return false;
    }
    if tree.is_struct_kind(pn, grammar::RULE_const_object) {
        return true;
    }
    if tree.is_struct_kind(pn, grammar::RULE_atom_paren) {
        // Possible empty tuple — nodes[0] is NULL.
        return super::pn_is_null(tree.struct_nodes(pn)[0]);
    }
    false
}

/// Allocate a chunk-backed RULE_const_object node carrying a tuple of
/// `num_args` already-const parse nodes (currently sitting at the top
/// of the result stack — caller pops them in reverse). Vendor
/// MICROPY_COMP_CONST_TUPLE counterpart (build_tuple_from_stack at
/// parse.c:915).
///
/// (Rust: the arena stores the tuple items in the node's payload;
/// the Zig layout's nodes[0]/[1]/[2] slots are [kind, len, 0].)
pub(crate) fn make_node_const_tuple(
    p: &mut Parser,
    src_line: usize,
    num_args: usize,
) -> ParseResult<Pn> {
    // popResult walks newest-first; collect then reverse so the
    // order matches source-level (leftmost element at index 0).
    let mut items: Vec<Pn> = Vec::with_capacity(num_args);
    for _ in 0..num_args {
        items.push(p.pop_result());
    }
    items.reverse();
    Ok(p.tree
        .alloc_const_object(src_line as u32, ConstObjectKind::Tuple, ConstPayload::Tuple(items)))
}

/// Small-int range gate. Mirrors the makeTokenPn promotion range so the
/// folded value can sit in a SMALL_INT leaf. Range matches the configured
/// MP_SMALL_INT_BITS — 63 on 64-bit hosts (MICROPY_OBJ_REPR_A), 31 on
/// 32-bit hosts (wasm32 etc., where SMALL_INT is 31 bits to fit in u32
/// with tag bits).
const SMALL_INT_BITS_LOCAL: u32 = if usize::BITS >= 64 { 63 } else { 31 };
const SMALL_INT_MAX_LOCAL: isize = (1isize << (SMALL_INT_BITS_LOCAL - 1)) - 1;
const SMALL_INT_MIN_LOCAL: isize = -(1isize << (SMALL_INT_BITS_LOCAL - 1));

pub(crate) fn fits_small_int(v: isize) -> bool {
    v >= SMALL_INT_MIN_LOCAL && v <= SMALL_INT_MAX_LOCAL
}

/// The parser's 30-bit SMALL_INT gate (used by makeTokenPn and the
/// factor_2 / pushFoldedIntResult paths — the Pn tag leaves 30 bits
/// after the shift on a 32-bit... no: the gate is what the Zig code
/// writes literally in those two places).
pub(crate) const SMALL_INT_MIN_30: isize = -(1 << 30);
pub(crate) const SMALL_INT_MAX_30: isize = (1 << 30) - 1;

/// Vendor binary_op_maybe (parse.c, used by fold_constants). Returns
/// the folded value or None if (a) the op isn't supported here or
/// (b) the result doesn't fit SMALL_INT.
fn fold_small_int_bin_op(op_tok: u16, a: isize, b: isize) -> Option<isize> {
    let r: isize = if op_tok == (TokenKind::OP_PLUS as u16) {
        a.checked_add(b)?
    } else if op_tok == (TokenKind::OP_MINUS as u16) {
        a.checked_sub(b)?
    } else if op_tok == (TokenKind::OP_STAR as u16) {
        a.checked_mul(b)?
    } else if op_tok == (TokenKind::OP_DBL_SLASH as u16) {
        if b == 0 {
            return None;
        }
        // Python floor-division semantics (rounds toward -inf).
        let q = a / b;
        let r = a % b;
        if r != 0 && (r < 0) != (b < 0) {
            q - 1
        } else {
            q
        }
    } else if op_tok == (TokenKind::OP_PERCENT as u16) {
        if b == 0 {
            return None;
        }
        // Python modulo: sign follows divisor.
        let r = a % b;
        if r != 0 && (r < 0) != (b < 0) {
            r + b
        } else {
            r
        }
    } else if op_tok == (TokenKind::OP_DBL_LESS as u16) {
        if b < 0 || b as usize >= usize::BITS as usize {
            return None;
        }
        // Manual overflow guard — verify by shifting back.
        let shift = b as u32;
        let shifted = a.checked_shl(shift)?;
        if (shifted >> shift) != a {
            return None;
        }
        shifted
    } else if op_tok == (TokenKind::OP_DBL_MORE as u16) {
        if b < 0 || b as usize >= usize::BITS as usize {
            return None;
        }
        a >> (b as u32)
    } else {
        return None;
    };
    if !fits_small_int(r) {
        return None;
    }
    Some(r)
}

/// Constant-folder for the binary rules that vendor's fold_constants
/// handles. Returns true on successful fold (stack now has 1 small_int
/// where there were num_args items); false otherwise (stack unchanged).
pub(crate) fn fold_binary_small_int(
    p: &mut Parser,
    src_line: usize,
    rule_id: u8,
    num_args: usize,
) -> ParseResult<bool> {
    // === Bitwise rules: expr | xor_expr | and_expr — operator is
    // determined by rule_id, all stack items are operands.
    if rule_id == grammar::RULE_expr
        || rule_id == grammar::RULE_xor_expr
        || rule_id == grammar::RULE_and_expr
    {
        if num_args < 2 {
            return Ok(false);
        }
        // All operands must be small_int.
        for i in 0..num_args {
            if !pn_is_small_int(p.peek_result(i)) {
                return Ok(false);
            }
        }
        // Walk leftmost → rightmost (peek(num_args-1) is leftmost).
        let mut acc: isize = pn_leaf_small_int(p.peek_result(num_args - 1));
        let mut j: isize = num_args as isize - 2;
        while j >= 0 {
            let v = pn_leaf_small_int(p.peek_result(j as usize));
            let r: isize = if rule_id == grammar::RULE_expr {
                acc | v
            } else if rule_id == grammar::RULE_xor_expr {
                acc ^ v
            } else {
                acc & v
            };
            if !fits_small_int(r) {
                return Ok(false);
            }
            acc = r;
            j -= 1;
        }
        // Pop all and push the folded leaf.
        for _ in 0..num_args {
            let _ = p.pop_result();
        }
        p.push_result_node(pn_new_small_int(acc));
        let _ = src_line;
        return Ok(true);
    }

    // === Per-token rules: shift_expr | arith_expr | term — odd
    // num_args; positions alternate operand, op_token, operand, ...
    if rule_id == grammar::RULE_shift_expr
        || rule_id == grammar::RULE_arith_expr
        || rule_id == grammar::RULE_term
    {
        if num_args < 3 || (num_args & 1) == 0 {
            return Ok(false);
        }
        // Check all operand slots are small_int (operands are at even
        // depths-from-top: peek(0), peek(2), peek(4) etc.).
        let mut i: usize = 0;
        while i < num_args {
            if !pn_is_small_int(p.peek_result(i)) {
                return Ok(false);
            }
            i += 2;
        }
        // Walk: acc = leftmost (peek(num_args-1)); for each op token
        // at odd depth, fold (acc op operand_at_lower_depth).
        let mut acc: isize = pn_leaf_small_int(p.peek_result(num_args - 1));
        let mut j: isize = num_args as isize - 2;
        while j >= 1 {
            let op_pn = p.peek_result(j as usize);
            if !pn_is_leaf(op_pn) || pn_leaf_kind(op_pn) != PN_TOKEN {
                return Ok(false);
            }
            let tok = pn_leaf_arg(op_pn) as u16;
            let rhs_pn = p.peek_result((j - 1) as usize);
            if !pn_is_small_int(rhs_pn) {
                return Ok(false);
            }
            let rhs = pn_leaf_small_int(rhs_pn);
            let r = match fold_small_int_bin_op(tok, acc, rhs) {
                Some(v) => v,
                None => return Ok(false),
            };
            acc = r;
            j -= 2;
        }
        for _ in 0..num_args {
            let _ = p.pop_result();
        }
        p.push_result_node(pn_new_small_int(acc));
        let _ = src_line;
        return Ok(true);
    }

    Ok(false)
}

// ── Big-int fold fallback ─────────────────────────────────────────────
//
// Vendor's `fold_constants` (parse.c:744) uses mp_obj_int arithmetic
// without distinguishing small/big — any integer operand folds when
// the op is in the supported set. Our small-int fold (above) bails on
// any overflow OR on a const_object.integer operand, so a literal like
// `(1 << 65)` or `~0x3fffffff` never folds.
//
// The bigint fallback handles both cases: parses any integer operand
// (small_int leaf or const_object.integer node) via bigint.parse_literal,
// runs the op via the BigInt in `bigint.rs`, and pushes the result
// either as a SMALL_INT leaf (if it fits the 30-bit gate) or as a
// const_object.integer node carrying canonical decimal text — matching
// vendor's `make_node_const_object_optimised`.
//
// Power (`**`) is deferred to a later commit (needs bit-length bounds
// to keep parse-time memory in check).

/// Extract an integer parse node's text representation into a fresh
/// owned String. Returns None for non-integer nodes (caller uses the
/// None to bail the fold).
fn pn_integer_text(tree: &ParseTree, pn: Pn) -> Option<String> {
    if pn_is_small_int(pn) {
        let v = pn_leaf_small_int(pn);
        return Some(format!("{v}"));
    }
    if pn_is_leaf(pn) {
        return None;
    }
    if !tree.is_struct_kind(pn, grammar::RULE_const_object) {
        return None;
    }
    let nodes = tree.struct_nodes(pn);
    let kind = ConstObjectKind::from_u32(nodes[0] as u32)?;
    if kind != ConstObjectKind::Integer {
        return None;
    }
    let len = nodes[1];
    if len == 0 {
        return None;
    }
    match tree.const_payload(pn) {
        Some(ConstPayload::Bytes(bytes)) => std::str::from_utf8(bytes).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn token_to_bin_op(tok: u16) -> Option<bigint::BinOp> {
    if tok == (TokenKind::OP_PLUS as u16) {
        Some(bigint::BinOp::Add)
    } else if tok == (TokenKind::OP_MINUS as u16) {
        Some(bigint::BinOp::Sub)
    } else if tok == (TokenKind::OP_STAR as u16) {
        Some(bigint::BinOp::Mul)
    } else if tok == (TokenKind::OP_DBL_SLASH as u16) {
        Some(bigint::BinOp::Floordiv)
    } else if tok == (TokenKind::OP_PERCENT as u16) {
        Some(bigint::BinOp::Mod)
    } else if tok == (TokenKind::OP_DBL_LESS as u16) {
        Some(bigint::BinOp::Shl)
    } else if tok == (TokenKind::OP_DBL_MORE as u16) {
        Some(bigint::BinOp::Shr)
    } else {
        None
    }
}

fn token_to_un_op(tok: u16) -> Option<bigint::UnOp> {
    if tok == (TokenKind::OP_PLUS as u16) {
        Some(bigint::UnOp::Pos)
    } else if tok == (TokenKind::OP_MINUS as u16) {
        Some(bigint::UnOp::Neg)
    } else if tok == (TokenKind::OP_TILDE as u16) {
        Some(bigint::UnOp::Invert)
    } else {
        None
    }
}

/// Push the bigint fold result back onto the stack. If the decimal
/// text fits SMALL_INT, emit a small_int leaf; otherwise a
/// const_object.integer node carrying the decimal text (vendor's
/// `make_node_const_object_optimised` shape).
///
/// NOTE: the Zig comment here says "30-bit SMALL_INT" but the code
/// calls `fitsSmallInt` — the 62-bit window (SMALL_INT_BITS_LOCAL =
/// 63 on 64-bit hosts). The code is what runs: a folded `-10^18`
/// becomes a small_int leaf even though the bare literal `10^18`
/// stays a const_object (makeTokenPn's 30-bit gate). Port the code,
/// not the comment.
fn push_folded_int_result(
    p: &mut Parser,
    src_line: usize,
    decimal: &str,
) -> ParseResult<()> {
    if let Ok(v) = decimal.parse::<isize>() {
        if fits_small_int(v) {
            p.push_result_node(pn_new_small_int(v));
            return Ok(());
        }
    }
    let pn = super::nodes::make_node_const_object(p, src_line, ConstObjectKind::Integer, decimal.as_bytes())?;
    p.push_result_node(pn);
    Ok(())
}

/// Big-int fallback for the same rule set as fold_binary_small_int.
/// Called after the small-int path returns false; only fires when
/// every operand is an integer (small or big) and every per-token
/// op is in our supported set.
pub(crate) fn try_fold_big_int_binary(
    p: &mut Parser,
    src_line: usize,
    rule_id: u8,
    num_args: usize,
) -> ParseResult<bool> {
    // Under nano (LONGINT_IMPL_NONE), don't fold to int_big: the
    // runtime can't hold it and the .mpy load would fail. Returning
    // false makes the parser emit the source expression verbatim so
    // the VM raises "small int overflow" at exec time, matching the
    // vendor LONGINT_NONE behaviour.
    if !IS_FULL {
        return Ok(false);
    }
    // "bitwise-style" rules: operator is implied by the rule_id, every
    // stack slot is an operand. Vendor parse.c:749-777 groups RULE_power
    // here too — `a ** b ** c` would unroll right-to-left through the
    // same num_args-walk (in practice it's always exactly 2 args).
    let is_bitwise = rule_id == grammar::RULE_expr
        || rule_id == grammar::RULE_xor_expr
        || rule_id == grammar::RULE_and_expr
        || rule_id == grammar::RULE_power;
    let is_pertoken = rule_id == grammar::RULE_shift_expr
        || rule_id == grammar::RULE_arith_expr
        || rule_id == grammar::RULE_term;
    if !is_bitwise && !is_pertoken {
        return Ok(false);
    }
    if is_bitwise && num_args < 2 {
        return Ok(false);
    }
    if is_pertoken && (num_args < 3 || (num_args & 1) == 0) {
        return Ok(false);
    }

    let mut acc = match pn_integer_text(&p.tree, p.peek_result(num_args - 1)) {
        Some(t) => t,
        None => return Ok(false),
    };

    if is_bitwise {
        let op: bigint::BinOp = if rule_id == grammar::RULE_expr {
            bigint::BinOp::Bor
        } else if rule_id == grammar::RULE_xor_expr {
            bigint::BinOp::Bxor
        } else if rule_id == grammar::RULE_and_expr {
            bigint::BinOp::Band
        } else {
            bigint::BinOp::Pow // RULE_power
        };
        let mut j: isize = num_args as isize - 2;
        while j >= 0 {
            let rhs = match pn_integer_text(&p.tree, p.peek_result(j as usize)) {
                Some(t) => t,
                None => return Ok(false),
            };
            let res = match bigint::fold_binary(op, &acc, &rhs) {
                Ok(v) => v,
                Err(bigint::BigIntError::OutOfMemory) => return Err(ParseError::OutOfMemory),
                Err(_) => return Ok(false),
            };
            acc = match res {
                Some(v) => v,
                None => return Ok(false),
            };
            j -= 1;
        }
    } else {
        let mut j: isize = num_args as isize - 2;
        while j >= 1 {
            let op_pn = p.peek_result(j as usize);
            if !pn_is_leaf(op_pn) || pn_leaf_kind(op_pn) != PN_TOKEN {
                return Ok(false);
            }
            let tok = pn_leaf_arg(op_pn) as u16;
            let op = match token_to_bin_op(tok) {
                Some(o) => o,
                None => return Ok(false),
            };
            let rhs = match pn_integer_text(&p.tree, p.peek_result((j - 1) as usize)) {
                Some(t) => t,
                None => return Ok(false),
            };
            let res = match bigint::fold_binary(op, &acc, &rhs) {
                Ok(v) => v,
                Err(bigint::BigIntError::OutOfMemory) => return Err(ParseError::OutOfMemory),
                Err(_) => return Ok(false),
            };
            acc = match res {
                Some(v) => v,
                None => return Ok(false),
            };
            j -= 2;
        }
    }

    for _ in 0..num_args {
        let _ = p.pop_result();
    }
    push_folded_int_result(p, src_line, &acc)?;
    Ok(true)
}

/// Big-int fallback for factor_2 unary (~, +, -). Mirrors the inline
/// small-int factor_2 fold; fires when the small path declined.
pub(crate) fn try_fold_big_int_unary(
    p: &mut Parser,
    src_line: usize,
    rule_id: u8,
    num_args: usize,
) -> ParseResult<bool> {
    // Same nano gate as the binary path — see try_fold_big_int_binary.
    if !IS_FULL {
        return Ok(false);
    }
    if rule_id != grammar::RULE_factor_2 || num_args != 2 {
        return Ok(false);
    }
    let operand = p.peek_result(0);
    let op_pn = p.peek_result(1);
    if !pn_is_leaf(op_pn) || pn_leaf_kind(op_pn) != PN_TOKEN {
        return Ok(false);
    }
    let tok = pn_leaf_arg(op_pn) as u16;
    let op = match token_to_un_op(tok) {
        Some(o) => o,
        None => return Ok(false),
    };

    let text = match pn_integer_text(&p.tree, operand) {
        Some(t) => t,
        None => return Ok(false),
    };
    let result = match bigint::fold_unary(op, &text) {
        Ok(v) => v,
        Err(bigint::BigIntError::OutOfMemory) => return Err(ParseError::OutOfMemory),
        Err(_) => return Ok(false),
    };

    let _ = p.pop_result(); // operand
    let _ = p.pop_result(); // op_tok
    push_folded_int_result(p, src_line, &result)?;
    Ok(true)
}

/// Vendor fold_logical_constants (parse.c:687).
///
/// For RULE_or_test / RULE_and_test: walk operands top-to-bottom (so
/// rightmost first), short-circuiting on a hit (const_true for OR,
/// const_false for AND) and DROPPING operands that const-fold to the
/// loser-side of the short-circuit. Returns true if the rule is now
/// "fully folded" (only one operand left → pushResultRule's caller
/// should treat that single operand as the result).
///
/// For RULE_not_test_2: if the operand is const_true → push KW_FALSE;
/// const_false → push KW_TRUE. Otherwise no-op.
///
/// On a partial fold (e.g. `x or False` → `x`) this returns true so
/// the operand stays bare on the stack with no or_test wrapper. The
/// caller updates num_args via the in/out pointer.
pub(crate) fn fold_logical_constants(
    p: &mut Parser,
    rule_id: u8,
    num_args_io: &mut usize,
) -> ParseResult<bool> {
    if rule_id == grammar::RULE_or_test || rule_id == grammar::RULE_and_test {
        let is_or = rule_id == grammar::RULE_or_test;
        let mut copy_to: usize = *num_args_io;
        let mut i: usize = copy_to;
        while i > 0 {
            i -= 1;
            let pn = p.peek_result(i);
            // copy_to-1 is the dest slot from the top; we move pn there.
            let top = p.result_stack.len();
            p.result_stack[top - copy_to] = pn;
            if i == 0 {
                break; // always keep leftmost
            }
            if is_or {
                if p.tree.parse_node_is_const_true(pn) {
                    break;
                } else if !p.tree.parse_node_is_const_false(pn) {
                    copy_to -= 1;
                }
            } else {
                if p.tree.parse_node_is_const_false(pn) {
                    break;
                } else if !p.tree.parse_node_is_const_true(pn) {
                    copy_to -= 1;
                }
            }
        }
        copy_to -= 1; // operand count to pop

        for _ in 0..copy_to {
            let _ = p.pop_result();
        }
        *num_args_io -= copy_to;
        return Ok(*num_args_io == 1);
    }
    if rule_id == grammar::RULE_not_test_2 && *num_args_io == 1 {
        let pn = p.peek_result(0);
        if p.tree.parse_node_is_const_false(pn) {
            let _ = p.pop_result();
            p.push_result_node(super::pn_new_leaf(
                super::PN_TOKEN,
                TokenKind::KW_TRUE as usize,
            ));
            return Ok(true);
        }
        if p.tree.parse_node_is_const_true(pn) {
            let _ = p.pop_result();
            p.push_result_node(super::pn_new_leaf(
                super::PN_TOKEN,
                TokenKind::KW_FALSE as usize,
            ));
            return Ok(true);
        }
    }
    Ok(false)
}

/// Vendor build_tuple_from_stack (parse.c:915): peek top `num_args`
/// stack entries; if all are pn_is_const, pop and pack them into a const
/// tuple node and push back. Returns true on fold success (the tuple
/// node replaces the elements on the stack); false otherwise (stack
/// unchanged).
pub(crate) fn build_tuple_from_stack(
    p: &mut Parser,
    src_line: usize,
    num_args: usize,
) -> ParseResult<bool> {
    let mut i: usize = num_args;
    while i > 0 {
        i -= 1;
        if !pn_is_const(&p.tree, p.peek_result(i)) {
            return Ok(false);
        }
    }
    let pn = make_node_const_tuple(p, src_line, num_args)?;
    p.push_result_node(pn);
    Ok(true)
}
