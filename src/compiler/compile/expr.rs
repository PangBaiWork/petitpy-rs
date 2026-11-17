//! Expression compilers — §19, §21, §23, §24, §29 of `compile.zig`.
//!
//! §19 `c_if_cond` (boolean tests with label allocation order that is
//! byte-critical), §21 or/and/not/comparison/star/factor/power/test-if,
//! §23 walrus, §24 yield-expr, §29 `compile_term` + op constants (the
//! constants themselves live in `emit::consts`). §22 atom-expr +
//! trailers (`expr_atom`) and §26 const-object (`expr_const`) are
//! sibling modules, re-exported here so the dispatch table keeps
//! one `expr::` namespace.
use super::{Compiler, Pass};
use crate::compiler::emit::common::EmitCommon;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_id, pn_is_leaf, pn_is_null, pn_is_struct,
    pn_leaf_arg, pn_leaf_kind, Pn, PN_TOKEN,
};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{IdInfoKind, ScopeKind};


// ─────────────────────────────────────────────────────────────────────────
// §19 — c_if_cond + boolean tests. BYTE-CRITICAL label ordering.
// ─────────────────────────────────────────────────────────────────────────

/// Compile a condition expression: jump to `label` if the condition is
/// `if_true` (mirrors vendor c_if_cond). The key invariant is the label
/// allocation order inside the recursive and/or cases — these are
/// observable in the .mpy because labels are numbered in allocation
/// order.
pub fn c_if_cond(comp: &mut Compiler, pn: Pn, if_true: bool, label: u16) {
    // Constant short-circuit.
    if comp.tree.parse_node_is_const_true(pn) {
        if if_true {
            comp.emit.jump(label as usize, &mut comp.scopes);
        }
        return;
    }
    if comp.tree.parse_node_is_const_false(pn) {
        if !if_true {
            comp.emit.jump(label as usize, &mut comp.scopes);
        }
        return;
    }

    if pn_is_struct(pn) {
        let k = comp.tree.struct_kind(pn);
        if k == grammar::RULE_or_test {
            if if_true {
                // x or y, jump-if-true: jump on either.
                let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
                for child in nodes {
                    c_if_cond(comp, child, true, label);
                }
            } else {
                // jump-if-false: only if ALL fail; need a fall-through label.
                let lab_fall = comp.comp_next_label();
                let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
                let n = nodes.len();
                let mut i: usize = 0;
                while i + 1 < n {
                    c_if_cond(comp, nodes[i], true, lab_fall);
                    i += 1;
                }
                c_if_cond(comp, nodes[n - 1], false, label);
                comp.emit.label_assign(lab_fall as usize);
            }
            return;
        }
        if k == grammar::RULE_and_test {
            if if_true {
                let lab_fall = comp.comp_next_label();
                let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
                let n = nodes.len();
                let mut i: usize = 0;
                while i + 1 < n {
                    c_if_cond(comp, nodes[i], false, lab_fall);
                    i += 1;
                }
                c_if_cond(comp, nodes[n - 1], true, label);
                comp.emit.label_assign(lab_fall as usize);
            } else {
                let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
                for child in nodes {
                    c_if_cond(comp, child, false, label);
                }
            }
            return;
        }
        if k == grammar::RULE_not_test_2 {
            let child = comp.tree.struct_nodes(pn)[0];
            c_if_cond(comp, child, !if_true, label);
            return;
        }
    }

    // Default: emit the value, then conditional jump.
    comp.compile_node(pn);
    comp.emit.pop_jump_if(if_true, label as usize, &mut comp.scopes);
}

// ─────────────────────────────────────────────────────────────────────────
// §21 — Expression compilers.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_or_test(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_or_and_test (cond=true). Compile all N nodes; emit
    // jump_if_or_pop(true) after every non-last child so the first truthy
    // child short-circuits to l_end with itself on the stack.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let n = nodes.len();
    let l_end = comp.comp_next_label();
    let mut i: usize = 0;
    while i < n {
        comp.compile_node(nodes[i]);
        if i + 1 < n {
            comp.emit
                .jump_if_or_pop(true, l_end as usize, &mut comp.scopes);
        }
        i += 1;
    }
    comp.emit.label_assign(l_end as usize);
}

pub fn compile_and_test(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_or_and_test (cond=false). Same shape as compile_or_test
    // but the short-circuit predicate is inverted: first falsy child wins.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let n = nodes.len();
    let l_end = comp.comp_next_label();
    let mut i: usize = 0;
    while i < n {
        comp.compile_node(nodes[i]);
        if i + 1 < n {
            comp.emit
                .jump_if_or_pop(false, l_end as usize, &mut comp.scopes);
        }
        i += 1;
    }
    comp.emit.label_assign(l_end as usize);
}

pub fn compile_not_test_2(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_not_test_2: compile the operand, then UNARY_OP NOT.
    let child = comp.tree.struct_nodes(pn)[0];
    comp.compile_node(child);
    comp.emit.unary_op(MP_UNARY_OP_NOT, &mut comp.scopes);
}

pub fn compile_binary_op(comp: &mut Compiler, pn: Pn) {
    // Unified handler for the six binary-op rules. The CompileRules table
    // wires all of expr / xor_expr / and_expr / shift_expr / arith_expr /
    // term to this entry point. We split on rule kind:
    //
    //   RULE_expr/xor_expr/and_expr  — vendor compile_binary_op: the op
    //       comes from the rule kind (OR + (kind - RULE_expr)). The grammar
    //       has no per-pair op token; children are pure list elements.
    //
    //   RULE_shift_expr/arith_expr/term — vendor compile_term: each pair
    //       carries its own op token; op = LSHIFT + (tok - OP_DBL_LESS).
    //
    // The two contiguous rule-id windows let us assert the layout.
    debug_assert_eq!(grammar::RULE_xor_expr, grammar::RULE_expr + 1);
    debug_assert_eq!(grammar::RULE_and_expr, grammar::RULE_expr + 2);
    debug_assert_eq!(grammar::RULE_shift_expr, grammar::RULE_expr + 3);
    debug_assert_eq!(grammar::RULE_arith_expr, grammar::RULE_expr + 4);
    debug_assert_eq!(grammar::RULE_term, grammar::RULE_expr + 5);
    let kind = comp.tree.struct_kind(pn);
    if kind <= grammar::RULE_and_expr {
        // compile_binary_op path — op encoded in rule kind.
        let op: u8 = MP_BINARY_OP_OR + (kind - grammar::RULE_expr);
        let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        let n = nodes.len();
        comp.compile_node(nodes[0]);
        let mut i: usize = 1;
        while i < n {
            comp.compile_node(nodes[i]);
            comp.emit.binary_op(op, &mut comp.scopes);
            i += 1;
        }
    } else {
        // compile_term path — per-pair op token.
        compile_term(comp, pn);
    }
}

pub fn compile_comparison(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_comparison: chained comparisons (a < b < c == ...).
    // For each operator pair we DUP/ROT to keep the next lhs available,
    // emit the binary_op, and short-circuit on false via jump_if_or_pop.
    // The trailing cleanup uses ROT/POP to drop the dangling value left
    // on the stack when the chain short-circuits.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let num_nodes = nodes.len();
    comp.compile_node(nodes[0]);
    let multi = num_nodes > 3;
    let mut l_fail: u16 = 0;
    if multi {
        l_fail = comp.comp_next_label();
    }
    let mut i: usize = 1;
    while i + 1 < num_nodes {
        comp.compile_node(nodes[i + 1]);
        if i + 2 < num_nodes {
            comp.emit.dup_top(&mut comp.scopes);
            comp.emit.rot_three(&mut comp.scopes);
        }
        let pn_op = nodes[i];
        if pn_is_leaf(pn_op) && pn_leaf_kind(pn_op) == PN_TOKEN {
            let tok: u8 = pn_leaf_arg(pn_op) as u8;
            let op: u8;
            if tok == TokenKind::KW_IN as u8 {
                op = MP_BINARY_OP_IN;
            } else {
                op = MP_BINARY_OP_LESS + (tok - TokenKind::OP_LESS as u8);
            }
            comp.emit.binary_op(op, &mut comp.scopes);
        } else {
            debug_assert!(pn_is_struct(pn_op));
            let k2 = comp.tree.struct_kind(pn_op);
            if k2 == grammar::RULE_comp_op_not_in {
                comp.emit
                    .binary_op(MP_BINARY_OP_NOT_IN, &mut comp.scopes);
            } else {
                debug_assert_eq!(k2, grammar::RULE_comp_op_is);
                let inner = comp.tree.struct_nodes(pn_op)[0];
                if pn_is_null(inner) {
                    comp.emit.binary_op(MP_BINARY_OP_IS, &mut comp.scopes);
                } else {
                    comp.emit
                        .binary_op(MP_BINARY_OP_IS_NOT, &mut comp.scopes);
                }
            }
        }
        if i + 2 < num_nodes {
            comp.emit
                .jump_if_or_pop(false, l_fail as usize, &mut comp.scopes);
        }
        i += 2;
    }
    if multi {
        let l_end = comp.comp_next_label();
        comp.emit.jump(l_end as usize, &mut comp.scopes);
        comp.emit.label_assign(l_fail as usize);
        comp.emit.adjust_stack_size(1, &mut comp.scopes);
        comp.emit.rot_two(&mut comp.scopes);
        comp.emit.pop_top(&mut comp.scopes);
        comp.emit.label_assign(l_end as usize);
    }
}

pub fn compile_star_expr(comp: &mut Compiler, pn: Pn) {
    // Inside a tuple assignment target — vendor flags the star; the
    // tuple-assign helper handles the actual unpack. In any other
    // context this is a syntax error.
    comp.compile_syntax_error(pn, "can't use starred expression here");
}

pub fn compile_factor_2(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_factor_2: unary +/-/~. nodes[0] is the op token,
    // nodes[1] is the operand. Token arithmetic mirrors mp_unary_op_t:
    // OP_TILDE→INVERT, OP_PLUS→POSITIVE, OP_MINUS→NEGATIVE.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    comp.compile_node(nodes[1]);
    let tok: u8 = pn_leaf_arg(nodes[0]) as u8;
    let op: u8;
    if tok == TokenKind::OP_TILDE as u8 {
        op = MP_UNARY_OP_INVERT;
    } else {
        debug_assert!(
            tok == TokenKind::OP_PLUS as u8 || tok == TokenKind::OP_MINUS as u8
        );
        op = MP_UNARY_OP_POSITIVE + (tok - TokenKind::OP_PLUS as u8);
    }
    comp.emit.unary_op(op, &mut comp.scopes);
}

pub fn compile_power(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_power: walk both children (base, exponent), emit POWER.
    super::handlers::compile_generic_all_nodes(comp, pn);
    comp.emit.binary_op(MP_BINARY_OP_POWER, &mut comp.scopes);
}

pub fn compile_test_if_expr(comp: &mut Compiler, pn: Pn) {
    // `a if c else b` — vendor compile.c:2121. The parse tree is
    //   test_if_expr: [success_value, test_if_else_struct]
    //   test_if_else: [condition, failure_value]
    // i.e. the condition + failure value live inside a nested struct,
    // not as flat siblings.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    debug_assert!(comp
        .tree
        .is_struct_kind(nodes[1], grammar::RULE_test_if_else));
    let else_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();

    let lab_else = comp.comp_next_label();
    let lab_end = comp.comp_next_label();
    c_if_cond(comp, else_nodes[0], false, lab_else); // condition
    comp.compile_node(nodes[0]); // success value
    comp.emit.jump(lab_end as usize, &mut comp.scopes);
    comp.emit.label_assign(lab_else as usize);
    // Vendor compile.c:2131 — both branches push one value; the
    // emitter saw a +1 stack delta when it compiled the success
    // branch, and at lab_else we're about to push again. Tell the
    // STACK_SIZE pass that the failure path starts one slot lower so
    // the recorded max-stack matches mpy-cross byte-for-byte.
    comp.emit.adjust_stack_size(-1, &mut comp.scopes);
    comp.compile_node(else_nodes[1]); // failure value
    comp.emit.label_assign(lab_end as usize);
}

// ─────────────────────────────────────────────────────────────────────────
// §29 — compile_term.
// ─────────────────────────────────────────────────────────────────────────

/// Vendor compile_term (compile.c:2276-2285). Used by shift_expr,
/// arith_expr, and term rules.
pub fn compile_term(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let num_nodes = nodes.len();
    comp.compile_node(nodes[0]);
    let mut i: usize = 1;
    while i + 1 < num_nodes {
        comp.compile_node(nodes[i + 1]);
        let tok: u8 = pn_leaf_arg(nodes[i]) as u8;
        let op: u8 = MP_BINARY_OP_LSHIFT + (tok - TokenKind::OP_DBL_LESS as u8);
        comp.emit.binary_op(op, &mut comp.scopes);
        i += 2;
    }
}

// ─────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────
// §23 — Walrus (PEP 572).
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_namedexpr(comp: &mut Compiler, pn: Pn) {
    // `a := expr` — vendor compile.c:2184 delegates straight to
    // compile_namedexpr_helper, which carries the PEP-572 close-over
    // logic for the comprehension case.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    compile_namedexpr_helper(comp, nodes[0], nodes[1]);
}

// Vendor compile_namedexpr_helper (compile.c:2152). PEP-572: `(x := e)`
// inside a comprehension binds `x` in the comprehension's PARENT scope,
// not in the comprehension itself.
pub fn compile_namedexpr_helper(comp: &mut Compiler, pn_name: Pn, pn_expr: Pn) {
    if !pn_is_id(pn_name) {
        comp.compile_syntax_error(pn_name, "can't assign to expression");
        return;
    }
    comp.compile_node(pn_expr);
    comp.emit.dup_top(&mut comp.scopes);

    let target = pn_leaf_arg(pn_name) as Qstr;

    // PEP-572 close-over runs in the SCOPE pass only. The other passes
    // just see the resulting id_info and route the store through it.
    if comp.pass == Pass::Scope {
        let sc = comp.scope_cur.expect("namedexpr_helper: no scope_cur");
        if comp.scopes.scopes[sc].kind.is_comp_like() {
            let parent = comp.scopes.scopes[sc]
                .parent
                .expect("namedexpr_helper: comp scope has no parent");
            let id_parent_idx =
                EmitCommon::get_id_for_modification(&mut comp.scopes, parent, target);
            let parent_kind = comp.scopes.scopes[parent].id_info[id_parent_idx].kind;
            if parent_kind == IdInfoKind::GlobalExplicit {
                comp.scopes
                    .find_or_add(sc, target, IdInfoKind::GlobalExplicit);
            } else {
                let id_local_idx =
                    comp.scopes
                        .find_or_add(sc, target, IdInfoKind::Undecided);
                // is_global == "the comprehension is defined at module scope" —
                // its parent's parent is null. In that case PEP-572 promotes the
                // target to a module-level global; otherwise it's a nonlocal
                // (closed over via the cell/free chain).
                let is_global = comp.scopes.scopes[parent].parent.is_none();
                if !is_global
                    && comp.scopes.scopes[sc].id_info[id_local_idx].kind
                        == IdInfoKind::GlobalImplicit
                {
                    comp.scopes.scopes[sc].id_info[id_local_idx].kind =
                        IdInfoKind::Undecided;
                }
                if is_global {
                    // Vendor compile_declare_global (compile.c:1252-1255):
                    // a walrus target that's already bound to something else
                    // in this scope (typically the comprehension's own
                    // for-target, PEP-572 rebind error) is a SyntaxError.
                    let k = comp.scopes.scopes[sc].id_info[id_local_idx].kind;
                    if k != IdInfoKind::Undecided && k != IdInfoKind::GlobalExplicit {
                        comp.compile_syntax_error(
                            pn_name,
                            "identifier redefined as global",
                        );
                        return;
                    }
                    comp.scopes.scopes[sc].id_info[id_local_idx].kind =
                        IdInfoKind::GlobalExplicit;
                } else {
                    // Vendor compile_declare_nonlocal (compile.c:1265-1274):
                    // only an UNDECIDED slot can be promoted to nonlocal;
                    // anything already bound (e.g. for-target) is the
                    // PEP-572 rebind error.
                    let k = comp.scopes.scopes[sc].id_info[id_local_idx].kind;
                    if k != IdInfoKind::Undecided && k != IdInfoKind::Free {
                        comp.compile_syntax_error(
                            pn_name,
                            "identifier redefined as nonlocal",
                        );
                        return;
                    }
                    if k == IdInfoKind::Undecided {
                        comp.scopes.scopes[sc].id_info[id_local_idx].kind =
                            IdInfoKind::GlobalImplicit;
                        comp.scopes.check_to_close_over(sc, id_local_idx);
                        if comp.scopes.scopes[sc].id_info[id_local_idx].kind
                            == IdInfoKind::GlobalImplicit
                        {
                            comp.compile_syntax_error(
                                pn_name,
                                "no binding for nonlocal found",
                            );
                            return;
                        }
                    }
                }
            }
        }
    }

    comp.compile_store_id(target);
}

// ─────────────────────────────────────────────────────────────────────────
// §24 — Yield-expr.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_yield_expr(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_yield_expr (compile.c:2748-2766). Three cases:
    //   - yield outside FUNCTION/LAMBDA → SyntaxError.
    //   - `yield` (bare) → LOAD_CONST_NONE, YIELD_VALUE.
    //   - `yield from X` → compile X, GET_ITER, LOAD_CONST_NONE, YIELD_FROM.
    //   - `yield X` → compile X, YIELD_VALUE.
    let sc = match comp.scope_cur {
        Some(sc) => sc,
        None => return,
    };
    let kind = comp.scopes.scopes[sc].kind;
    if kind != ScopeKind::Function && kind != ScopeKind::Lambda {
        comp.compile_syntax_error(pn, "'yield' outside function");
        return;
    }
    let arg = comp.tree.struct_nodes(pn)[0];
    if pn_is_null(arg) {
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
        comp.emit
            .yield_op(MP_EMIT_YIELD_VALUE, &mut comp.scopes);
    } else if comp
        .tree
        .is_struct_kind(arg, grammar::RULE_yield_arg_from)
    {
        // yield_arg_from: KW_FROM, test (vendor grammar.h:372). The
        // parser drops literal keyword tokens during AND-reduction —
        // the resulting struct has just one child, the test
        // expression at nodes[0] (matches vendor compile.c:2758-2759).
        let inner = comp.tree.struct_nodes(arg)[0];
        comp.compile_node(inner);
        comp.emit.get_iter(false, &mut comp.scopes);
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
        comp.emit
            .yield_op(MP_EMIT_YIELD_FROM, &mut comp.scopes);
    } else {
        comp.compile_node(arg);
        comp.emit
            .yield_op(MP_EMIT_YIELD_VALUE, &mut comp.scopes);
    }
}


pub use super::expr_atom::{compile_atom_brace, compile_atom_bracket, compile_atom_expr_normal, compile_atom_paren, compile_dictorsetmaker_dbl_star, compile_dictorsetmaker_item, compile_subscript, compile_subscriptlist, compile_testlist, compile_testlist_comp, compile_trailer_bracket, compile_trailer_paren, compile_trailer_period};
pub use super::expr_const::{compile_const_object, pn_to_const_obj};
