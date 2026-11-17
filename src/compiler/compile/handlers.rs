//! Statement handlers — §11, §12, §20 of `compile.zig`.
//!
//! §11 generic dispatch helpers, §12 simple statements, §20
//! trivial input wrappers. The other sections live in sibling
//! modules and are re-exported here so the dispatch table keeps
//! one `handlers::` namespace: §13 import family
//! (`handlers_import`), §14–§16 global/nonlocal + control flow
//! (`handlers_control`), §17 try/with (`handlers_try`), §18
//! expression-statement (`handlers_expr_stmt`).

use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_leaf_arg, Pn,
    };
use crate::compiler::qstr::Qstr;

// ─────────────────────────────────────────────────────────────────────────
// §11 — Generic dispatch helpers. (Section numbers follow the plan.)
//
// `compile_generic_all_nodes` walks every child and recursively compiles
// it. Used as the safe default for dispatch slots without a dedicated
// handler.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_generic_all_nodes(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    for child in nodes {
        comp.compile_node(child);
    }
}

pub fn compile_generic_tuple(comp: &mut Compiler, pn: Pn) {
    // Tuple: compile all children, then BUILD_TUPLE(n).
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let n = nodes.len();
    for child in nodes {
        comp.compile_node(child);
    }
    comp.emit.build(n, MP_EMIT_BUILD_TUPLE, &mut comp.scopes);
}

// ─────────────────────────────────────────────────────────────────────────
// §12 — Simple statement handlers.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_pass_stmt(_comp: &mut Compiler, _pn: Pn) {
    // No-op. Vendor emits nothing; suite-block wrappers handle the rest.
}

pub fn compile_break_cont_stmt(comp: &mut Compiler, pn: Pn) {
    let is_break = comp.tree.struct_kind(pn) == grammar::RULE_break_stmt;
    let label = if is_break {
        comp.break_label
    } else {
        comp.continue_label
    };
    if label == super::INVALID_LABEL {
        comp.compile_syntax_error(pn, "'break'/'continue' outside loop");
        return;
    }
    let depth_delta = comp.cur_except_level - comp.break_continue_except_level;
    // unwind_jump handles the BREAK_FROM_FOR high bit encoded in `label`.
    comp.emit
        .unwind_jump(label, depth_delta as usize, &mut comp.scopes);
}

pub fn compile_return_stmt(comp: &mut Compiler, pn: Pn) {
    let sc = comp.scope_cur.expect("compile_return_stmt: no scope_cur");
    if !comp.scopes.scopes[sc].kind.is_func_like() {
        comp.compile_syntax_error(pn, "'return' outside function");
        return;
    }
    let arg = comp.tree.struct_nodes(pn)[0];
    if pn_is_null(arg) {
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    } else {
        comp.compile_node(arg);
    }
    comp.emit.return_value(&mut comp.scopes);
}

pub fn compile_yield_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor delegates to compile_yield_expr; we inline the simple form.
    compile_generic_all_nodes(comp, pn);
    comp.emit.pop_top(&mut comp.scopes);
}

pub fn compile_raise_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_raise_stmt (compile.c:1070-1086). The grammar shape
    // is: raise_stmt: opt_rule(raise_stmt_arg). raise_stmt_arg in turn
    // wraps (test, opt_rule(raise_stmt_from)).
    let arg = comp.tree.struct_nodes(pn)[0];
    if pn_is_null(arg) {
        comp.emit.raise_varargs(0, &mut comp.scopes);
        return;
    }
    if comp
        .tree
        .is_struct_kind(arg, grammar::RULE_raise_stmt_arg)
    {
        let arg_nodes: Vec<Pn> = comp.tree.struct_nodes(arg).to_vec();
        let a0 = arg_nodes[0];
        let a1 = arg_nodes[1];
        comp.compile_node(a0);
        if pn_is_null(a1) {
            comp.emit.raise_varargs(1, &mut comp.scopes);
        } else {
            // a1 is a raise_stmt_from struct (KW_FROM, test); generic
            // walk compiles the KW_FROM no-op then pushes the from-arg.
            comp.compile_node(a1);
            comp.emit.raise_varargs(2, &mut comp.scopes);
        }
    } else {
        comp.compile_node(arg);
        comp.emit.raise_varargs(1, &mut comp.scopes);
    }
}

pub fn compile_del_stmt(comp: &mut Compiler, pn: Pn) {
    let first = comp.tree.struct_nodes(pn)[0];
    comp.apply_to_single_or_list(first, grammar::RULE_exprlist, c_del_atom);
}

/// Compile one atom in a `del` target list. Vendor `c_del_stmt` body.
fn c_del_atom(comp: &mut Compiler, pn: Pn) {
    if pn_is_id(pn) {
        comp.compile_delete_id(pn_leaf_arg(pn) as Qstr);
        return;
    }
    if comp
        .tree
        .is_struct_kind(pn, grammar::RULE_atom_expr_normal)
    {
        // a.b or a[b] target — compile the chain except the last
        // trailer, then emit DELETE_ATTR / DELETE_SUBSCR for it.
        // The trailer chain may be a single trailer directly OR an
        // atom_expr_trailers struct wrapping multiple — see
        // c_assign_atom_expr for the matching shape handling.
        let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        debug_assert_eq!(nodes.len(), 2);
        comp.compile_node(nodes[0]);

        let mut last_trailer: Pn = nodes[1];
        if comp
            .tree
            .is_struct_kind(nodes[1], grammar::RULE_atom_expr_trailers)
        {
            let trail_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();
            let trail_n = trail_nodes.len();
            let mut i: usize = 0;
            while i + 1 < trail_n {
                comp.compile_node(trail_nodes[i]);
                i += 1;
            }
            last_trailer = trail_nodes[trail_n - 1];
        }

        if comp
            .tree
            .is_struct_kind(last_trailer, grammar::RULE_trailer_period)
        {
            let lpns: Vec<Pn> = comp.tree.struct_nodes(last_trailer).to_vec();
            let q = pn_leaf_arg(lpns[0]) as Qstr;
            comp.emit.attr(q, MP_EMIT_ATTR_DELETE, &mut comp.scopes);
        } else if comp
            .tree
            .is_struct_kind(last_trailer, grammar::RULE_trailer_bracket)
        {
            let lpns: Vec<Pn> = comp.tree.struct_nodes(last_trailer).to_vec();
            comp.compile_node(lpns[0]);
            comp.emit.subscr(MP_EMIT_SUBSCR_DELETE, &mut comp.scopes);
        } else {
            comp.compile_syntax_error(pn, "can't delete expression");
        }
        return;
    }
    if comp.tree.is_struct_kind(pn, grammar::RULE_atom_paren)
        || comp.tree.is_struct_kind(pn, grammar::RULE_atom_bracket)
        || comp.tree.is_struct_kind(pn, grammar::RULE_testlist_comp)
    {
        // Tuple / list target — recurse on each child.
        let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        for child in nodes {
            c_del_atom(comp, child);
        }
        return;
    }
    comp.compile_syntax_error(pn, "can't delete this expression");
}


pub use super::handlers_control::{compile_assert_stmt, compile_for_stmt, compile_global_nonlocal_stmt, compile_if_stmt, compile_while_stmt};
pub use super::handlers_expr_stmt::compile_expr_stmt;
pub(crate) use super::handlers_expr_stmt::{annotations_qstr, has_annassign};
pub use super::handlers_import::{compile_dotted_as_name, compile_import_from, compile_import_name};
pub use super::handlers_try::{compile_try_stmt, compile_with_stmt};

// ─────────────────────────────────────────────────────────────────────────
// §20 — Trivial input wrappers (full funcdef/classdef live in funcdef.rs).
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_suite_block_stmts(comp: &mut Compiler, pn: Pn) {
    compile_generic_all_nodes(comp, pn);
}

pub fn compile_file_input(comp: &mut Compiler, pn: Pn) {
    compile_generic_all_nodes(comp, pn);
}

pub fn compile_eval_input(comp: &mut Compiler, pn: Pn) {
    // Top-level expression: compile + RETURN_VALUE.
    let child = comp.tree.struct_nodes(pn)[0];
    comp.compile_node(child);
    comp.emit.return_value(&mut comp.scopes);
}

pub fn compile_single_input(comp: &mut Compiler, pn: Pn) {
    compile_generic_all_nodes(comp, pn);
}
