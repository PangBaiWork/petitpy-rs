//! Try/except/finally + with — §17 of `compile.zig`.
//!
//! Split out of `handlers.rs` (file-size rule); re-exported
//! from there.

use super::assign::{c_assign, AssignKind};
use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_null, pn_is_struct, pn_leaf_arg, Pn,
    };
use crate::compiler::qstr::Qstr;

// ─────────────────────────────────────────────────────────────────────────
// §17 — Try/except/finally + with.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_try_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_try_stmt (compile.c:1718). The try statement has
    // three possible shapes:
    //   try: body
    //   finally: ...                       — try_stmt_finally
    //   try: body
    //   except A: ...                      — bare try-except (no else/finally)
    //   try: body                          — try_stmt_except_and_more
    //   except A: ...
    //   [else: ...] [finally: ...]
    //
    // pns->nodes[0] is the body; pns->nodes[1] discriminates the shape.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    debug_assert!(pn_is_struct(nodes[1]));
    let pns2_kind = comp.tree.struct_kind(nodes[1]);

    if pns2_kind == grammar::RULE_try_stmt_finally {
        // just try-finally
        let finally_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();
        compile_try_finally(
            comp,
            nodes[0],
            0,
            &[],
            crate::compiler::parser::PN_NULL,
            finally_nodes[0],
        );
        return;
    }

    if pns2_kind == grammar::RULE_try_stmt_except_and_more {
        // try-except with optional else and/or finally
        let pns2_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();
        let list = comp
            .tree
            .extract_list(pns2_nodes[0], grammar::RULE_try_stmt_except_list);
        let pn_else = pns2_nodes[1];
        let pn_finally_wrap = pns2_nodes[2];
        if pn_is_null(pn_finally_wrap) {
            // no finally clause
            compile_try_except(comp, nodes[0], list.len(), &list, pn_else);
        } else {
            // finally wraps the try-except-else
            let pn_finally = comp.tree.struct_nodes(pn_finally_wrap)[0];
            compile_try_finally(comp, nodes[0], list.len(), &list, pn_else, pn_finally);
        }
        return;
    }

    // just try-except (no else, no finally)
    let list = comp
        .tree
        .extract_list(nodes[1], grammar::RULE_try_stmt_except_list);
    compile_try_except(
        comp,
        nodes[0],
        list.len(),
        &list,
        crate::compiler::parser::PN_NULL,
    );
}

pub fn compile_with_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_with_stmt (compile.c:1772). The with-statement's
    // pre-bits (a, a as b, c as d, ...) come in as a list rooted at
    // pns->nodes[0]; the body is pns->nodes[1]. Each pre-bit is either a
    // bare expr or an as-binding (PN_with_item). The bytecode is built
    // recursively in a nested fashion so SETUP_WITH / WITH_CLEANUP /
    // END_FINALLY nest from outer to inner.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let pre_list = extract_with_list(comp, nodes[0]);
    let list_kind = if comp
        .tree
        .is_struct_kind(pre_list, grammar::RULE_with_paren_list)
    {
        grammar::RULE_with_paren_list
    } else {
        grammar::RULE_with_stmt_list
    };
    let list = comp.tree.extract_list(pre_list, list_kind);
    debug_assert!(!list.is_empty());
    compile_with_stmt_helper(comp, list.len(), &list, nodes[1]);
}

/// PEP 617 `with (a, b, …):` wraps the item list in a with_body_paren
/// struct — unwrap it so downstream sees the raw list node.
fn extract_with_list(comp: &Compiler, pn: Pn) -> Pn {
    if comp
        .tree
        .is_struct_kind(pn, grammar::RULE_with_body_paren)
    {
        return comp.tree.struct_nodes(pn)[0];
    }
    pn
}

// Vendor compile_try_except (compile.c:1592).
fn compile_try_except(
    comp: &mut Compiler,
    pn_body: Pn,
    n_except: usize,
    pn_excepts: &[Pn],
    pn_else: Pn,
) {
    let l1 = comp.comp_next_label();
    let success_label = comp.comp_next_label();

    comp.compile_increase_except_level(l1, MP_EMIT_SETUP_BLOCK_EXCEPT);

    comp.compile_node(pn_body);
    comp.emit
        .pop_except_jump(success_label as usize, &mut comp.scopes);

    comp.emit.label_assign(l1 as usize);
    comp.emit.start_except_handler(&mut comp.scopes);

    let l2 = comp.comp_next_label();

    let mut i: usize = 0;
    while i < n_except {
        debug_assert!(comp
            .tree
            .is_struct_kind(pn_excepts[i], grammar::RULE_try_stmt_except));
        let pns_except: Vec<Pn> = comp.tree.struct_nodes(pn_excepts[i]).to_vec();

        let mut qstr_exception_local: Qstr = 0;
        let end_finally_label = comp.comp_next_label();

        if pn_is_null(pns_except[0]) {
            if i + 1 != n_except {
                comp.compile_syntax_error(pn_excepts[i], "default 'except' must be last");
                comp.compile_decrease_except_level();
                return;
            }
        } else {
            let mut pns_exception_expr = pns_except[0];
            if pn_is_struct(pns_exception_expr) {
                let kind = comp.tree.struct_kind(pns_exception_expr);
                if kind == grammar::RULE_try_stmt_as_name {
                    let pns3: Vec<Pn> = comp.tree.struct_nodes(pns_exception_expr).to_vec();
                    pns_exception_expr = pns3[0];
                    qstr_exception_local = pn_leaf_arg(pns3[1]) as Qstr;
                }
            }
            comp.emit.dup_top(&mut comp.scopes);
            comp.compile_node(pns_exception_expr);
            comp.emit
                .binary_op(MP_BINARY_OP_EXCEPTION_MATCH, &mut comp.scopes);
            comp.emit
                .pop_jump_if(false, end_finally_label as usize, &mut comp.scopes);
        }

        if qstr_exception_local == 0 {
            comp.emit.pop_top(&mut comp.scopes);
        } else {
            comp.compile_store_id(qstr_exception_local);
        }

        let mut l3: u16 = 0;
        if qstr_exception_local != 0 {
            l3 = comp.comp_next_label();
            comp.compile_increase_except_level(l3, MP_EMIT_SETUP_BLOCK_FINALLY);
        }
        comp.compile_node(pns_except[1]);
        if qstr_exception_local != 0 {
            comp.emit
                .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
            comp.emit.label_assign(l3 as usize);
            comp.emit.adjust_stack_size(1, &mut comp.scopes);
            comp.emit
                .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
            comp.compile_store_id(qstr_exception_local);
            comp.compile_delete_id(qstr_exception_local);
            comp.emit.adjust_stack_size(-1, &mut comp.scopes);
            comp.compile_decrease_except_level();
        }

        comp.emit
            .pop_except_jump(l2 as usize, &mut comp.scopes);
        comp.emit.label_assign(end_finally_label as usize);
        comp.emit.adjust_stack_size(1, &mut comp.scopes);
        i += 1;
    }

    comp.compile_decrease_except_level();
    comp.emit.end_except_handler(&mut comp.scopes);

    comp.emit.label_assign(success_label as usize);
    if !pn_is_null(pn_else) {
        comp.compile_node(pn_else);
    }
    comp.emit.label_assign(l2 as usize);
}

// Vendor compile_try_finally (compile.c:1689).
fn compile_try_finally(
    comp: &mut Compiler,
    pn_body: Pn,
    n_except: usize,
    pn_except: &[Pn],
    pn_else: Pn,
    pn_finally: Pn,
) {
    let l_finally_block = comp.comp_next_label();

    comp.compile_increase_except_level(l_finally_block, MP_EMIT_SETUP_BLOCK_FINALLY);

    if n_except == 0 {
        debug_assert!(pn_is_null(pn_else));
        comp.emit.adjust_stack_size(3, &mut comp.scopes);
        comp.compile_node(pn_body);
        comp.emit.adjust_stack_size(-3, &mut comp.scopes);
    } else {
        compile_try_except(comp, pn_body, n_except, pn_except, pn_else);
    }

    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);

    comp.emit.label_assign(l_finally_block as usize);
    comp.emit.adjust_stack_size(1, &mut comp.scopes);
    comp.compile_node(pn_finally);
    comp.emit.adjust_stack_size(-1, &mut comp.scopes);

    comp.compile_decrease_except_level();
}

// Vendor compile_with_stmt_helper (compile.c:1745).
fn compile_with_stmt_helper(comp: &mut Compiler, n: usize, nodes: &[Pn], body: Pn) {
    if n == 0 {
        comp.compile_node(body);
        return;
    }
    let l_end = comp.comp_next_label();
    if comp
        .tree
        .is_struct_kind(nodes[0], grammar::RULE_with_item)
    {
        let pns: Vec<Pn> = comp.tree.struct_nodes(nodes[0]).to_vec();
        comp.compile_node(pns[0]);
        comp.compile_increase_except_level(l_end, MP_EMIT_SETUP_BLOCK_WITH);
        c_assign(comp, pns[1], AssignKind::Store);
    } else {
        comp.compile_node(nodes[0]);
        comp.compile_increase_except_level(l_end, MP_EMIT_SETUP_BLOCK_WITH);
        comp.emit.pop_top(&mut comp.scopes);
    }
    compile_with_stmt_helper(comp, n - 1, &nodes[1..], body);
    // Vendor compile.c:1766-1768 emits only with_cleanup +
    // decrease_except_level. END_FINALLY is handled by the outer
    // compile_try_finally context, NOT here. (Earlier scaffold added
    // an extra end_finally which doubled the unwind.)
    comp.emit.with_cleanup(l_end as usize, &mut comp.scopes);
    comp.compile_decrease_except_level();
}

