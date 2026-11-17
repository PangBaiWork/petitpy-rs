//! Global/nonlocal + control flow — §14–§16 of `compile.zig`.
//!
//! §14 `global`/`nonlocal` declarations, §15 assert/if/while,
//! §16 for-stmt with the optimised-range fast path. Split out
//! of `handlers.rs` (file-size rule); re-exported from there.

use super::assign::{c_assign, AssignKind};
use super::expr::c_if_cond;
use super::wk_idx;
use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_small_int, pn_is_struct, pn_leaf_arg, pn_leaf_small_int, Pn,
    };
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{IdInfoKind, ScopeKind, SCOPE_FLAG_REFGLOBALS};

// §14 — Global / nonlocal.
// ─────────────────────────────────────────────────────────────────────────

fn compile_declare_global(comp: &mut Compiler, pn: Pn, q: Qstr) {
    let sc = comp.scope_cur.expect("compile_declare_global: no scope_cur");
    let idx = comp
        .scopes
        .find_or_add(sc, q, IdInfoKind::GlobalExplicit);
    let kind = comp.scopes.scopes[sc].id_info[idx].kind;
    if kind != IdInfoKind::GlobalExplicit
        && kind != IdInfoKind::Undecided
        && kind != IdInfoKind::GlobalImplicit
        && kind != IdInfoKind::GlobalImplicitAssigned
    {
        comp.compile_syntax_error(pn, "identifier redefined as global");
        return;
    }
    comp.scopes.scopes[sc].id_info[idx].kind = IdInfoKind::GlobalExplicit;
    comp.scopes.scopes[sc].scope_flags |= SCOPE_FLAG_REFGLOBALS;
}

fn compile_declare_nonlocal(comp: &mut Compiler, pn: Pn, q: Qstr) {
    let sc = comp.scope_cur.expect("compile_declare_nonlocal: no scope_cur");
    // Vendor compile.c:1289 only rejects `nonlocal` at module scope.
    // Classes are valid (the binding refers to the enclosing function's
    // variable and propagates via the cell/free machinery as usual).
    if comp.scopes.scopes[sc].kind == ScopeKind::Module {
        comp.compile_syntax_error(pn, "can't declare nonlocal in outer code");
        return;
    }
    // Three-way branch from vendor compile.c:1265-1275. The seed kind
    // must be .undecided (NOT .free) so we can detect a brand-new entry
    // vs. a re-declaration of something already classified by an earlier
    // pass / statement.
    let idx = comp.scopes.find_or_add(sc, q, IdInfoKind::Undecided);
    let kind = comp.scopes.scopes[sc].id_info[idx].kind;
    if kind == IdInfoKind::Undecided {
        // First sighting: tentatively .global_implicit, then ask the
        // scope chain for a binding. check_to_close_over writes .free
        // iff an ancestor function has `q` as .local / .cell / .free;
        // on miss it leaves .global_implicit alone, which is our error
        // sentinel.
        comp.scopes.scopes[sc].id_info[idx].kind = IdInfoKind::GlobalImplicit;
        comp.scopes.check_to_close_over(sc, idx);
        if comp.scopes.scopes[sc].id_info[idx].kind == IdInfoKind::GlobalImplicit {
            comp.compile_syntax_error(pn, "no binding for nonlocal found");
        }
    } else if kind != IdInfoKind::Free {
        // Already classified as .local (e.g. parameter, prior write),
        // .cell, .global_explicit, or .global_implicit-from-earlier-
        // statement — none of these can be re-declared nonlocal.
        comp.compile_syntax_error(pn, "identifier redefined as nonlocal");
    }
    // id.kind == .free → idempotent re-declaration, no error.
}

pub fn compile_global_nonlocal_stmt(comp: &mut Compiler, pn: Pn) {
    let is_global = comp.tree.struct_kind(pn) == grammar::RULE_global_stmt;
    let name_list = comp.tree.struct_nodes(pn)[0];
    let list = comp.tree.extract_list(name_list, grammar::RULE_name_list);
    for name in list {
        if !pn_is_id(name) {
            comp.compile_syntax_error(name, "expected identifier");
            continue;
        }
        let q = pn_leaf_arg(name) as Qstr;
        if is_global {
            compile_declare_global(comp, name, q);
        } else {
            compile_declare_nonlocal(comp, name, q);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// §15 — Control-flow statements.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_assert_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor py/compile.c:1304-1320 compile_assert_stmt.
    // (Nano omits the MP_STATE_VM(mp_optimise_value) early-return — we
    // always emit assertions; nano has no -O optimise flag.)
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let lab_end = comp.comp_next_label();
    c_if_cond(comp, nodes[0], true, lab_end);
    // Vendor compile_assert_stmt (compile.c:1312) emits LOAD_GLOBAL for
    // AssertionError — kind GLOBAL_GLOBAL, not GLOBAL_NAME (LOAD_NAME).
    let ae = comp.wk(wk_idx::ASSERTION_ERROR);
    comp.emit
        .load_global(ae, MP_EMIT_IDOP_GLOBAL_GLOBAL, &mut comp.scopes);
    let extra = nodes[1];
    if !pn_is_null(extra) {
        // Optional assertion message — pass as single arg.
        comp.compile_node(extra);
        comp.emit.call_function(1, 0, 0, &mut comp.scopes);
    }
    comp.emit.raise_varargs(1, &mut comp.scopes);
    comp.emit.label_assign(lab_end as usize);
}

pub fn compile_if_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor: compile.c:1322-1395 compile_if_stmt.
    // if_stmt: condition, body, elif_list, else_block.
    //
    // BYTE-CRITICAL label ordering:
    //   1. l_end is allocated FIRST (always).
    //   2. l_fail for the main `if` is allocated only when the condition
    //      is not a const-false (`if False:` skips the body entirely).
    //   3. Same per-elif logic for elif fail labels.
    //   4. The `jump l_end` over elif/else is suppressed when both are
    //      empty (otherwise we emit an unused unconditional jump).
    //   5. When the condition is const-true we short-circuit past every
    //      remaining elif/else (vendor `goto done`).
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let lab_end = comp.comp_next_label();

    let mut done = false;

    if !comp.tree.parse_node_is_const_false(nodes[0]) {
        let lab_fail = comp.comp_next_label();
        c_if_cond(comp, nodes[0], false, lab_fail); // if condition

        comp.compile_node(nodes[1]); // if block

        if comp.tree.parse_node_is_const_true(nodes[0]) {
            // `if True:` body: skip everything else.
            done = true;
        } else {
            // Only emit the jump-over if there's actually elif/else to skip.
            if !(pn_is_null(nodes[2]) && pn_is_null(nodes[3])) {
                comp.emit.jump(lab_end as usize, &mut comp.scopes);
            }
            comp.emit.label_assign(lab_fail as usize);
        }
    }

    if !done {
        // elif list.
        let elist = comp
            .tree
            .extract_list(nodes[2], grammar::RULE_if_stmt_elif_list);
        for elif_pn in elist {
            let elif_nodes: Vec<Pn> = comp.tree.struct_nodes(elif_pn).to_vec();

            if comp.tree.parse_node_is_const_false(elif_nodes[0]) {
                continue;
            }

            let lab_fail = comp.comp_next_label();
            c_if_cond(comp, elif_nodes[0], false, lab_fail); // elif condition
            comp.compile_node(elif_nodes[1]); // elif block

            if comp.tree.parse_node_is_const_true(elif_nodes[0]) {
                // `elif True:` — vendor `goto done`: skip rest + else.
                done = true;
                break;
            }

            comp.emit.jump(lab_end as usize, &mut comp.scopes);
            comp.emit.label_assign(lab_fail as usize);
        }

        if !done {
            // else block (may be null — compile_node no-ops on null).
            if !pn_is_null(nodes[3]) {
                comp.compile_node(nodes[3]);
            }
        }
    }

    comp.emit.label_assign(lab_end as usize);
}

pub fn compile_while_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor: compile.c:1392-1412 compile_while_stmt.
    // while_stmt: condition, body, else_block.
    //
    // BYTE-CRITICAL label ordering (START_BREAK_CONTINUE_BLOCK):
    //   1. break_label allocated FIRST.
    //   2. continue_label allocated SECOND.
    //   3. top_label allocated THIRD (inside the body, only when the
    //      condition is not const-false; `while False:` emits nothing
    //      for the loop body at all).
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();

    // START_BREAK_CONTINUE_BLOCK
    let old_break = comp.break_label;
    let old_continue = comp.continue_label;
    let old_excl = comp.break_continue_except_level;
    let lab_break = comp.comp_next_label();
    let lab_continue = comp.comp_next_label();
    comp.break_label = lab_break as usize;
    comp.continue_label = lab_continue as usize;
    comp.break_continue_except_level = comp.cur_except_level;

    if !comp.tree.parse_node_is_const_false(nodes[0]) {
        let lab_top = comp.comp_next_label();
        if !comp.tree.parse_node_is_const_true(nodes[0]) {
            // `while True:` — vendor skips the jump-to-cond.
            comp.emit.jump(lab_continue as usize, &mut comp.scopes);
        }
        comp.emit.label_assign(lab_top as usize);
        comp.compile_node(nodes[1]); // body
        comp.emit.label_assign(lab_continue as usize);
        c_if_cond(comp, nodes[0], true, lab_top); // condition
    }

    // END_BREAK_CONTINUE_BLOCK — break/continue in the else target the
    // ENCLOSING loop, not this one.
    comp.break_label = old_break;
    comp.continue_label = old_continue;
    comp.break_continue_except_level = old_excl;

    if !pn_is_null(nodes[2]) {
        comp.compile_node(nodes[2]);
    } // else

    comp.emit.label_assign(lab_break as usize);
}

// ─────────────────────────────────────────────────────────────────────────
// §16 — For-stmt + optimised-range.
//
// The range() recognizer is BYTE-CRITICAL — it must trigger on exactly
// the same call shapes vendor recognises (range(stop), range(start,stop),
// range(start,stop,step) with all small-int args). Vendor compile.c
// lines 1668-1750.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_for_stmt(comp: &mut Compiler, pn: Pn) {
    // Vendor: compile.c:1512-1590 compile_for_stmt (with the range()
    // recognizer optimisation at compile.c:1516-1567 dispatching to
    // compile_for_stmt_optimised_range at compile.c:1430-1510).
    //
    // for_stmt: target, iter, body, else_block.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();

    // range() recognizer ────────────────────────────────────────────────
    // Pattern: target is a bare identifier AND iter is an atom_expr_normal
    // whose head is the identifier `range` and whose first trailer is
    // trailer_paren. If 1..3 plain args, dispatch to the optimised form.
    if pn_is_id(nodes[0])
        && comp
            .tree
            .is_struct_kind(nodes[1], grammar::RULE_atom_expr_normal)
    {
        let it_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();
        let head = it_nodes[0];
        let trail0 = it_nodes[1];
        if pn_is_id(head)
            && pn_leaf_arg(head) as Qstr == comp.wk(wk_idx::RANGE)
            && comp
                .tree
                .is_struct_kind(trail0, grammar::RULE_trailer_paren)
        {
            let pn_range_args = comp.tree.struct_nodes(trail0)[0];
            let args = comp.tree.extract_list(pn_range_args, grammar::RULE_arglist);
            let n_args = args.len();
            let mut pn_range_start: Pn = 0;
            let mut pn_range_end: Pn = 0;
            let mut pn_range_step: Pn = 0;
            let mut optimize = false;
            if (1..=3).contains(&n_args) {
                optimize = true;
                if n_args == 1 {
                    pn_range_start = crate::compiler::parser::pn_new_small_int(0);
                    pn_range_end = args[0];
                    pn_range_step = crate::compiler::parser::pn_new_small_int(1);
                } else if n_args == 2 {
                    pn_range_start = args[0];
                    pn_range_end = args[1];
                    pn_range_step = crate::compiler::parser::pn_new_small_int(1);
                } else {
                    pn_range_start = args[0];
                    pn_range_end = args[1];
                    pn_range_step = args[2];
                    if !pn_is_small_int(pn_range_step)
                        || pn_leaf_small_int(pn_range_step) == 0
                    {
                        optimize = false;
                    }
                }
                // start/end must NOT be *arg / **arg / kwarg.
                if optimize && pn_is_struct(pn_range_start) {
                    let k = comp.tree.struct_kind(pn_range_start);
                    if k == grammar::RULE_arglist_star
                        || k == grammar::RULE_arglist_dbl_star
                        || k == grammar::RULE_argument
                    {
                        optimize = false;
                    }
                }
                if optimize && pn_is_struct(pn_range_end) {
                    let k = comp.tree.struct_kind(pn_range_end);
                    if k == grammar::RULE_arglist_star
                        || k == grammar::RULE_arglist_dbl_star
                        || k == grammar::RULE_argument
                    {
                        optimize = false;
                    }
                }
            }
            if optimize {
                compile_for_stmt_optimised_range(
                    comp,
                    nodes[0],
                    pn_range_start,
                    pn_range_end,
                    pn_range_step,
                    nodes[2],
                    nodes[3],
                );
                return;
            }
        }
    }

    // Generic for-loop ─────────────────────────────────────────────────
    // START_BREAK_CONTINUE_BLOCK
    let old_break = comp.break_label;
    let old_continue = comp.continue_label;
    let old_excl = comp.break_continue_except_level;
    let lab_break_raw = comp.comp_next_label();
    let lab_continue = comp.comp_next_label();
    comp.break_label = (lab_break_raw as usize) | MP_EMIT_BREAK_FROM_FOR;
    comp.continue_label = lab_continue as usize;
    comp.break_continue_except_level = comp.cur_except_level;

    let lab_pop = comp.comp_next_label();

    comp.compile_node(nodes[1]); // iterator
    comp.emit.get_iter(true, &mut comp.scopes);
    comp.emit.label_assign(lab_continue as usize);
    comp.emit.for_iter(lab_pop as usize, &mut comp.scopes);
    c_assign(comp, nodes[0], AssignKind::Store); // loop variable
    comp.compile_node(nodes[2]); // body
    comp.emit.jump(lab_continue as usize, &mut comp.scopes);
    comp.emit.label_assign(lab_pop as usize);
    comp.emit.for_iter_end(&mut comp.scopes);

    // END_BREAK_CONTINUE_BLOCK — break/continue in else target outer loop.
    comp.break_label = old_break;
    comp.continue_label = old_continue;
    comp.break_continue_except_level = old_excl;

    if !pn_is_null(nodes[3]) {
        comp.compile_node(nodes[3]);
    } // else

    comp.emit.label_assign(lab_break_raw as usize);
}

// Vendor compile.c:1430-1510 compile_for_stmt_optimised_range.
fn compile_for_stmt_optimised_range(
    comp: &mut Compiler,
    pn_var: Pn,
    pn_start: Pn,
    pn_end: Pn,
    pn_step: Pn,
    pn_body: Pn,
    pn_else: Pn,
) {
    let old_break = comp.break_label;
    let old_continue = comp.continue_label;
    let old_excl = comp.break_continue_except_level;
    let lab_break = comp.comp_next_label();
    let lab_continue = comp.comp_next_label();
    comp.break_label = lab_break as usize;
    comp.continue_label = lab_continue as usize;
    comp.break_continue_except_level = comp.cur_except_level;

    let lab_top = comp.comp_next_label();
    let lab_entry = comp.comp_next_label();

    let end_on_stack = !pn_is_small_int(pn_end);
    if end_on_stack {
        comp.compile_node(pn_end);
    }

    comp.compile_node(pn_start);

    comp.emit.jump(lab_entry as usize, &mut comp.scopes);
    comp.emit.label_assign(lab_top as usize);

    comp.emit.dup_top(&mut comp.scopes);
    c_assign(comp, pn_var, AssignKind::Store);

    comp.compile_node(pn_body);

    comp.emit.label_assign(lab_continue as usize);

    comp.compile_node(pn_step);
    comp.emit
        .binary_op(MP_BINARY_OP_INPLACE_ADD, &mut comp.scopes);

    comp.emit.label_assign(lab_entry as usize);

    if end_on_stack {
        comp.emit.dup_top_two(&mut comp.scopes);
        comp.emit.rot_two(&mut comp.scopes);
    } else {
        comp.emit.dup_top(&mut comp.scopes);
        comp.compile_node(pn_end);
    }
    if pn_leaf_small_int(pn_step) >= 0 {
        comp.emit.binary_op(MP_BINARY_OP_LESS, &mut comp.scopes);
    } else {
        comp.emit.binary_op(MP_BINARY_OP_MORE, &mut comp.scopes);
    }
    comp.emit
        .pop_jump_if(true, lab_top as usize, &mut comp.scopes);

    comp.break_label = old_break;
    comp.continue_label = old_continue;
    comp.break_continue_except_level = old_excl;

    let mut lab_end: u16 = 0;
    if !pn_is_null(pn_else) {
        comp.emit.pop_top(&mut comp.scopes);
        if end_on_stack {
            comp.emit.pop_top(&mut comp.scopes);
        }
        comp.compile_node(pn_else);
        lab_end = comp.comp_next_label();
        comp.emit.jump(lab_end as usize, &mut comp.scopes);
        let adj: i32 = 1 + if end_on_stack { 1 } else { 0 };
        comp.emit.adjust_stack_size(adj, &mut comp.scopes);
    }

    comp.emit.label_assign(lab_break as usize);

    comp.emit.pop_top(&mut comp.scopes);
    if end_on_stack {
        comp.emit.pop_top(&mut comp.scopes);
    }

    if !pn_is_null(pn_else) {
        comp.emit.label_assign(lab_end as usize);
    }
}

