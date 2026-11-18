//! Expression-statement — §18 of `compile.zig`.
//!
//! Bare expressions (pop result, or REPL print), PEP 526 class
//! annotations with the annotation unparser. Split out of
//! `handlers.rs` (file-size rule); re-exported from there.

use super::assign::{c_assign, AssignKind};
use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_id, pn_is_leaf, pn_is_null, pn_is_small_int, pn_is_struct, pn_is_token_kind,
    pn_leaf_arg, pn_leaf_kind, pn_leaf_small_int, ConstObjectKind, ConstPayload, ParseTree, Pn,
    PN_STRING, PN_TOKEN,
};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{IdInfoKind, ScopeKind};

// ─────────────────────────────────────────────────────────────────────────
// §18 — Expression-statement.
//
// Handles:
//   * bare expression (pop result, or REPL print)
//   * augassign (a += b)
//   * single assign (a = b)
//   * multi-target (a = b = c) via assign_list
// ─────────────────────────────────────────────────────────────────────────

// ── Class annotations (PEP 526) ──────────────────────────────────────
// Vendor compiles `x: T` in a class to nothing and drops T from `x: T = v`,
// so a class has no __annotations__ and @dataclass cannot see a field
// without a default, nor the order of its fields. Here a class body that
// annotates a name records it in `__annotations__`, in source order. The
// value is the annotation's text, as under `from __future__ import
// annotations` (PEP 563): evaluating it would raise NameError on forward
// references such as `next: Node` inside `class Node`. Annotations the
// unparser below does not cover record None.

pub(crate) fn annotations_qstr(comp: &mut Compiler) -> Qstr {
    comp.qstr_from_strn(b"__annotations__")
}

/// Whether `pn` annotates anything, outside nested defs, lambdas and classes.
/// Those, comprehensions (comp_for) and const objects are not descended
/// into: their slots hold scope pointers or raw payloads, not parse nodes.
pub(crate) fn has_annassign(tree: &ParseTree, pn: Pn) -> bool {
    if !pn_is_struct(pn) {
        return false;
    }
    let kind = tree.struct_kind(pn);
    match kind {
        grammar::RULE_annassign => return true,
        grammar::RULE_funcdef
        | grammar::RULE_async_funcdef
        | grammar::RULE_lambdef
        | grammar::RULE_lambdef_nocond
        | grammar::RULE_classdef
        | grammar::RULE_comp_for
        | grammar::RULE_const_object => return false,
        _ => {}
    }
    for &child in tree.struct_nodes(pn) {
        if has_annassign(tree, child) {
            return true;
        }
    }
    false
}

struct AnnText {
    buf: [u8; 256],
    len: usize,
}

impl AnnText {
    fn new() -> Self {
        AnnText {
            buf: [0u8; 256],
            len: 0,
        }
    }

    fn add(&mut self, text: &[u8]) -> bool {
        if self.len + text.len() > self.buf.len() {
            return false;
        }
        self.buf[self.len..self.len + text.len()].copy_from_slice(text);
        self.len += text.len();
        true
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// A string annotation (a forward reference) as repr would quote it.
fn add_quoted(out: &mut AnnText, text: &[u8]) -> bool {
    let quote: &[u8] = if text.contains(&b'\'') { b"\"" } else { b"'" };
    out.add(quote) && out.add(text) && out.add(quote)
}

/// The source text of an annotation, for the forms annotations use; false
/// for anything else (a call, an operator other than `|`, too long).
fn unparse_annotation(comp: &mut Compiler, pn: Pn, out: &mut AnnText) -> bool {
    if pn_is_null(pn) {
        return false;
    }
    if pn_is_id(pn) {
        let q = pn_leaf_arg(pn) as Qstr;
        let s: &[u8] = comp.qctx.qstr_str(q);
        return out.add(s);
    }
    if pn_is_small_int(pn) {
        let digits = pn_leaf_small_int(pn).to_string();
        return out.add(digits.as_bytes());
    }
    if pn_is_leaf(pn) {
        if pn_leaf_kind(pn) == PN_STRING {
            let q = pn_leaf_arg(pn) as Qstr;
            let s: &[u8] = comp.qctx.qstr_str(q);
            return add_quoted(out, s);
        }
        for (tok, text) in [
            (TokenKind::KW_NONE, "None"),
            (TokenKind::KW_TRUE, "True"),
            (TokenKind::KW_FALSE, "False"),
        ] {
            if pn_is_token_kind(pn, tok as u16) {
                return out.add(text.as_bytes());
            }
        }
        return false;
    }
    let kind = comp.tree.struct_kind(pn);
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    match kind {
        grammar::RULE_atom_expr_normal => {
            if !unparse_annotation(comp, nodes[0], out) {
                return false;
            }
            let trailers: Vec<Pn> = if comp
                .tree
                .is_struct_kind(nodes[1], grammar::RULE_atom_expr_trailers)
            {
                comp.tree.struct_nodes(nodes[1]).to_vec()
            } else {
                vec![nodes[1]]
            };
            for t in trailers {
                if comp
                    .tree
                    .is_struct_kind(t, grammar::RULE_trailer_period)
                {
                    let tn = comp.tree.struct_nodes(t)[0];
                    if !out.add(b".") || !unparse_annotation(comp, tn, out) {
                        return false;
                    }
                } else if comp
                    .tree
                    .is_struct_kind(t, grammar::RULE_trailer_bracket)
                {
                    let tn = comp.tree.struct_nodes(t)[0];
                    if !out.add(b"[")
                        || !unparse_annotation(comp, tn, out)
                        || !out.add(b"]")
                    {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            true
        }
        grammar::RULE_subscriptlist | grammar::RULE_testlist_comp | grammar::RULE_expr => {
            let sep: &[u8] = if kind == grammar::RULE_expr {
                b" | "
            } else {
                b", "
            };
            for (i, child) in nodes.iter().enumerate() {
                if i > 0 && !out.add(sep) {
                    return false;
                }
                if !unparse_annotation(comp, *child, out) {
                    return false;
                }
            }
            true
        }
        grammar::RULE_const_object => {
            // A string that is not a valid identifier is a const object.
            let kind_u32: u32 = nodes[0] as u32;
            if ConstObjectKind::from_u32(kind_u32) != Some(ConstObjectKind::LongString) {
                return false;
            }
            let text: &[u8] = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Bytes(b)) => b,
                _ => &[],
            };
            add_quoted(out, text)
        }
        grammar::RULE_atom_bracket => {
            if !out.add(b"[") {
                return false;
            }
            if !nodes.is_empty()
                && !pn_is_null(nodes[0])
                && !unparse_annotation(comp, nodes[0], out)
            {
                return false;
            }
            out.add(b"]")
        }
        _ => false,
    }
}

/// `__annotations__[name] = '<annotation text>'` (or None).
fn record_annotation(comp: &mut Compiler, name: Qstr, annotation: Pn) {
    let mut text = AnnText::new();
    if unparse_annotation(comp, annotation, &mut text) {
        let q = comp.qstr_from_strn(text.as_bytes());
        comp.emit.load_const_str(q, &mut comp.scopes);
    } else {
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    }
    let ann_q = annotations_qstr(comp);
    comp.compile_load_id(ann_q);
    comp.emit.load_const_str(name, &mut comp.scopes);
    comp.emit.subscr(MP_EMIT_SUBSCR_STORE, &mut comp.scopes);
}

pub fn compile_expr_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor: py/compile.c:2006 compile_expr_stmt.
    //
    // expr_stmt has two children: nodes[0] = LHS testlist_star_expr,
    // nodes[1] = optional RHS.  The RHS, after the parser's and_ident/or
    // collapses, is one of:
    //   * NULL                            -> bare expression statement
    //   * struct PN_annassign             -> "x: T"  or  "x: T = rhs"
    //   * struct PN_expr_stmt_augassign   -> "x += rhs" etc.
    //   * struct PN_expr_stmt_assign_list -> "a = b = c" (>= 2 targets)
    //   * anything else (struct or leaf)  -> plain "x = rhs"
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let mut pn_rhs: Pn = nodes[1];

    if pn_is_null(pn_rhs) {
        let sc_kind = comp.scopes.scopes[comp.scope_cur.expect("expr_stmt: no scope_cur")].kind;
        if comp.is_repl && sc_kind == ScopeKind::Module {
            // REPL top-level bare expression: evaluate then print.
            // The echo callee resolves ON DEMAND as
            // `__repl_print__` (`compReplPrintQstr`,
            // `compile.zig:2716-2718`) — NOT via the `wk` table:
            // `WK_NAMES[16]` is the literal `"<repl_print>"`,
            // which can never resolve, so a REPL `1+1` raised
            // `NameError` (E1 regression — see
            // `repl_single_input_resolves_repl_print`).
            let repl_q = comp.qstr_from_strn(b"__repl_print__");
            comp.compile_load_id(repl_q);
            comp.compile_node(nodes[0]);
            comp.emit.call_function(1, 0, 0, &mut comp.scopes);
            comp.emit.pop_top(&mut comp.scopes);
        } else {
            // Non-REPL bare expression: drop the result.
            // Skip the emit entirely for a lonely constant literal
            // (vendor optimisation: leaf-but-not-id, or PN_const_object).
            let n0 = nodes[0];
            let is_lonely_const = (pn_is_leaf(n0) && !pn_is_id(n0))
                || comp
                    .tree
                    .is_struct_kind(n0, grammar::RULE_const_object);
            if !is_lonely_const {
                comp.compile_node(n0);
                comp.emit.pop_top(&mut comp.scopes);
            }
        }
        return;
    }

    if pn_is_struct(pn_rhs) {
        let kind = comp.tree.struct_kind(pn_rhs);
        let sub_nodes: Vec<Pn> = comp.tree.struct_nodes(pn_rhs).to_vec();

        if kind == grammar::RULE_annassign {
            // annassign children: [annotation, opt(rhs)]
            let sc_kind =
                comp.scopes.scopes[comp.scope_cur.expect("expr_stmt: no scope_cur")].kind;
            if sc_kind == ScopeKind::Class && pn_is_id(nodes[0]) {
                if !pn_is_null(sub_nodes[1]) {
                    comp.compile_node(sub_nodes[1]);
                    c_assign(comp, nodes[0], AssignKind::Store);
                }
                record_annotation(comp, pn_leaf_arg(nodes[0]) as Qstr, sub_nodes[0]);
                return;
            }
            if pn_is_null(sub_nodes[1]) {
                // Bare annotation "x: T" — inside a function this declares
                // x as a local; elsewhere it's a no-op at runtime.
                if sc_kind == ScopeKind::Function && pn_is_id(nodes[0]) {
                    let lhs_q = pn_leaf_arg(nodes[0]) as Qstr;
                    let sc = comp.scope_cur.expect("expr_stmt: no scope_cur");
                    comp.scopes.find_or_add(sc, lhs_q, IdInfoKind::Local);
                }
                return;
            }
            // "x: T = rhs" — drop the annotation, then plain-assign rhs.
            pn_rhs = sub_nodes[1];
            // fall through to the plain_assign tail below.
        } else if kind == grammar::RULE_expr_stmt_augassign {
            // augassign children: [op_token, rhs]
            c_assign(comp, nodes[0], AssignKind::AugLoad);
            comp.compile_node(sub_nodes[1]);
            debug_assert!(
                pn_is_leaf(sub_nodes[0]) && pn_leaf_kind(sub_nodes[0]) == PN_TOKEN
            );
            let tok: u32 = pn_leaf_arg(sub_nodes[0]) as u32;
            // Vendor: MP_BINARY_OP_INPLACE_OR + (tok - MP_TOKEN_DEL_PIPE_EQUAL).
            // Both enum ranges are 13 contiguous entries in lock-step order.
            let op: u8 =
                (MP_BINARY_OP_INPLACE_OR as u32 + (tok - TokenKind::DEL_PIPE_EQUAL as u32)) as u8;
            comp.emit.binary_op(op, &mut comp.scopes);
            c_assign(comp, nodes[0], AssignKind::AugStore);
            return;
        } else if kind == grammar::RULE_expr_stmt_assign_list {
            // Chained assignment "a = b = ... = rhs".  The list contains the
            // intermediate targets (left-to-right) plus the final rhs at
            // the tail.  Vendor: compile rhs, then store left-most first,
            // DUP_TOP between each pair.
            let list_n = sub_nodes.len();
            let rhs_idx: usize = list_n - 1;
            comp.compile_node(sub_nodes[rhs_idx]);
            if rhs_idx > 0 {
                comp.emit.dup_top(&mut comp.scopes);
            }
            c_assign(comp, nodes[0], AssignKind::Store);
            let mut i: usize = 0;
            while i < rhs_idx {
                if i + 1 < rhs_idx {
                    comp.emit.dup_top(&mut comp.scopes);
                }
                c_assign(comp, sub_nodes[i], AssignKind::Store);
                i += 1;
            }
            return;
        } else if kind == grammar::RULE_testlist_star_expr
            && comp
                .tree
                .is_struct_kind(nodes[0], grammar::RULE_testlist_star_expr)
        {
            // Vendor MICROPY_COMP_{DOUBLE,TRIPLE}_TUPLE_ASSIGN (compile.c:2069):
            // "a, b = c, d" / "a, b, c = d, e, f" via ROT_TWO/ROT_THREE,
            // skipping the BUILD_TUPLE + UNPACK_SEQUENCE heap allocation.
            let lhs: Vec<Pn> = comp.tree.struct_nodes(nodes[0]).to_vec();
            let n = lhs.len();
            let no_star = !lhs
                .iter()
                .any(|t| comp.tree.is_struct_kind(*t, grammar::RULE_star_expr));
            if (n == 2 || n == 3) && n == sub_nodes.len() && no_star {
                for r in &sub_nodes[..n] {
                    comp.compile_node(*r);
                }
                if n == 3 {
                    comp.emit.rot_three(&mut comp.scopes);
                }
                comp.emit.rot_two(&mut comp.scopes);
                for t in &lhs[..n] {
                    c_assign(comp, *t, AssignKind::Store);
                }
                return;
            }
        }
        // Any other struct kind: fall through to plain_assign.
    }

    // plain_assign — vendor's labelled tail.  Reached from:
    //   * the "else" branch above (pn_rhs is a leaf such as an id, int,
    //     or a struct of some non-special kind), or
    //   * the annassign "x: T = rhs" path with pn_rhs rebound to the rhs.
    comp.compile_node(pn_rhs);
    c_assign(comp, nodes[0], AssignKind::Store);
}

