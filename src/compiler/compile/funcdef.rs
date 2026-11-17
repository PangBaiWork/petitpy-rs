//! Funcdef / classdef / lambdef / decorated / comprehensions —
//! §20, §24 of `compile.zig`.
//!
//! The scope-stash design: Zig writes child `Scope*` pointers into
//! parse-tree node slots (funcdef nodes[4], classdef nodes[3], lambdef
//! nodes[2], comp_for nodes[3]). Rust keeps the same keying in
//! `Compiler::scope_slots: HashMap<Pn, ScopeId>` — the struct node's
//! own `Pn` is the key, the `ScopeId` the value. The SCOPE pass
//! creates the scope and stashes it; later passes read it back.
//!
//! Call/brace helpers live in `funcdef_call`, §27 param/codegen
//! helpers in `funcdef_params`; both are re-exported here so the
//! dispatch table and driver keep one `funcdef::` namespace.

use super::assign::c_assign;
use super::expr::{c_if_cond};
use super::funcdef_params::compile_funcdef_lambdef;
use super::{Compiler, Pass};
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_struct, pn_leaf_arg, Pn,
};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{
    ScopeId, ScopeKind, SCOPE_FLAG_GENERATOR, };


// ─────────────────────────────────────────────────────────────────────────
// §20 — Funcdef / classdef / lambdef / decorated.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_funcdef(comp: &mut Compiler, pn: Pn) {
    // Port of compile_funcdef (compile.c:957). The full algorithm lives in
    // compile_funcdef_helper; this wrapper stores the result by name.
    let emit_options = comp.scopes.scopes[comp.scope_cur.expect("funcdef: no scope_cur")]
        .emit_options;
    let fname = compile_funcdef_helper(comp, pn, emit_options);
    comp.compile_store_id(fname);
}

pub fn compile_classdef(comp: &mut Compiler, pn: Pn) {
    // Port of compile_classdef (compile.c:2742). All work in helper.
    let emit_options = comp.scopes.scopes[comp.scope_cur.expect("classdef: no scope_cur")]
        .emit_options;
    let cname = compile_classdef_helper(comp, pn, emit_options);
    comp.compile_store_id(cname);
}

pub fn compile_lambdef(comp: &mut Compiler, pn: Pn) {
    // Port of compile_lambdef (compile.c:2136). Lambda's parse-node
    // layout: nodes[0]=varargslist params, nodes[1]=body expr, nodes[2]
    // is a parser-managed slot we hijack to stash the child scope across
    // passes (mirrors vendor pns->nodes[2] = (mp_parse_node_t)s).
    let this_scope: ScopeId;
    if comp.pass == Pass::Scope {
        let emit_options =
            comp.scopes.scopes[comp.scope_cur.expect("lambdef: no scope_cur")].emit_options;
        let s = comp.scope_new_and_link(ScopeKind::Lambda, pn, emit_options);
        comp.scope_slots.insert(pn, s);
        this_scope = s;
    } else {
        this_scope = *comp
            .scope_slots
            .get(&pn)
            .expect("lambdef: scope slot missing (SCOPE pass did not run?)");
    }
    let params = comp.tree.struct_nodes(pn)[0];
    compile_funcdef_lambdef(comp, this_scope, params, grammar::RULE_varargslist);
}

// Vendor compile_decorated (compile.c:889).
//
// Built-in @micropython.* decorators (vendor compile.c:838-887) are
// NOT recognised here: no test in our corpus uses them, the relevant
// emit options (NATIVE, VIPER, ASM) are out-of-scope for nano, and
// recognising them would just convert the user-visible NameError on
// undefined `micropython` into a different one. If any future test
// needs @micropython.bytecode, port compile_built_in_decorator at
// that point.
pub fn compile_decorated(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let decorators_pn = nodes[0];
    let list = comp
        .tree
        .extract_list(decorators_pn, grammar::RULE_decorators);
    let emit_options = comp.scopes.scopes[comp.scope_cur.expect("decorated: no scope_cur")]
        .emit_options;

    // For each decorator: compile the dotted-name head + attr chain,
    // then evaluate any (...) arguments. This leaves one stack value
    // per decorator before the body is compiled.
    for dec in &list {
        debug_assert!(comp
            .tree
            .is_struct_kind(*dec, grammar::RULE_decorator));
        let dec_nodes: Vec<Pn> = comp.tree.struct_nodes(*dec).to_vec();

        let name_pn = dec_nodes[0];
        let name_list = comp
            .tree
            .extract_list(name_pn, grammar::RULE_dotted_name);

        comp.compile_node(name_list[0]);
        let mut j: usize = 1;
        while j < name_list.len() {
            debug_assert!(pn_is_id(name_list[j]));
            comp.emit.attr(
                pn_leaf_arg(name_list[j]) as Qstr,
                MP_EMIT_ATTR_LOAD,
                &mut comp.scopes,
            );
            j += 1;
        }

        if !pn_is_null(dec_nodes[1]) {
            // `@deco(args)` — evaluate the args, which dispatches to
            // compile_trailer_paren and turns the LOAD on stack into a
            // call result (the "decorator factory" semantics).
            comp.compile_node(dec_nodes[1]);
        }
    }

    // Body is funcdef, classdef, or async_funcdef (vendor decorated_body
    // OR rule per compile.c:921-942 + grammar.h decorated_body).
    debug_assert!(pn_is_struct(nodes[1]));
    let body_pn = nodes[1];
    let body_kind = comp.tree.struct_kind(body_pn);
    let body_name: Qstr;
    if body_kind == grammar::RULE_funcdef {
        body_name = compile_funcdef_helper(comp, body_pn, emit_options);
    } else if body_kind == grammar::RULE_async_funcdef {
        // `@dec async def f(): ...` — vendor compile.c:936-942.
        let inner_funcdef = comp.tree.struct_nodes(body_pn)[0];
        debug_assert!(pn_is_struct(inner_funcdef));
        body_name = compile_funcdef_helper(comp, inner_funcdef, emit_options);
        let fscope = *comp
            .scope_slots
            .get(&inner_funcdef)
            .expect("decorated async: funcdef scope slot missing");
        comp.scopes.scopes[fscope].scope_flags |= SCOPE_FLAG_GENERATOR;
    } else if body_kind == grammar::RULE_classdef {
        body_name = compile_classdef_helper(comp, body_pn, emit_options);
    } else {
        comp.compile_syntax_error(body_pn, "decorated body must be function or class");
        return;
    }

    // Apply each decorator: stack has [d1, d2, ..., dN, body]; emit N
    // CALL_FUNCTION(1, 0, 0) ops so the final result is d1(d2(...dN(body))).
    for _ in 0..list.len() {
        comp.emit.call_function(1, 0, 0, &mut comp.scopes);
    }

    comp.compile_store_id(body_name);
}

// ─────────────────────────────────────────────────────────────────────────
// §24 — Comprehension stub + scope comp iter.
// ─────────────────────────────────────────────────────────────────────────

/// Vendor compile_comprehension (compile.c:2496).
/// pns: a testlist_comp / dictorsetmaker / argument testlist whose
///   nodes[0] is the inner expression and nodes[1] is a comp_for struct.
/// kind: the scope kind (.list_comp / .dict_comp / .set_comp / .gen_expr).
pub fn compile_comprehension_stub(comp: &mut Compiler, pn: Pn, kind: ScopeKind) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    debug_assert_eq!(nodes.len(), 2);
    debug_assert!(comp
        .tree
        .is_struct_kind(nodes[1], grammar::RULE_comp_for));
    let comp_for = nodes[1];

    if comp.pass == Pass::Scope {
        let parent = comp.scope_cur.expect("comprehension_stub: no scope_cur");
        let parent_options = comp.scopes.scopes[parent].emit_options;
        let s = comp.scope_new_and_link(kind, pn, parent_options);
        // Stash the scope on comp_for nodes[3] so all subsequent
        // passes pick it up.
        comp.scope_slots.insert(comp_for, s);
    }

    let this_scope = *comp
        .scope_slots
        .get(&comp_for)
        .expect("comprehension_stub: comp_for scope slot missing");
    close_over_variables_etc(comp, this_scope, 0, 0);

    // Compile the source iterator IN THE PARENT scope (the for-clause's
    // RHS is evaluated before entering the comprehension function).
    let comp_for_nodes: Vec<Pn> = comp.tree.struct_nodes(comp_for).to_vec();
    comp.compile_node(comp_for_nodes[1]);
    if kind == ScopeKind::GenExpr {
        comp.emit.get_iter(false, &mut comp.scopes);
    }
    comp.emit.call_function(1, 0, 0, &mut comp.scopes);
}

/// Vendor compile_scope_comp_iter (compile.c:2954). Recursive walker
/// for nested for/if clauses in a comprehension scope. Called from
/// drive_scope_body's comp-like branch.
pub fn compile_scope_comp_iter(
    comp: &mut Compiler,
    pns_comp_for: Pn,
    pn_inner_expr: Pn,
    for_depth: u32,
) {
    let l_top = comp.comp_next_label();
    let l_end = comp.comp_next_label();
    comp.emit.label_assign(l_top as usize);
    comp.emit.for_iter(l_end as usize, &mut comp.scopes);
    let comp_for_nodes: Vec<Pn> = comp.tree.struct_nodes(pns_comp_for).to_vec();
    c_assign(comp, comp_for_nodes[0], super::assign::AssignKind::Store);

    let mut pn_iter = comp_for_nodes[2];
    // tail_recursion: vendor uses goto. Iterate over comp_if chains
    // until either NULL (emit inner expr) or comp_for (recurse).
    loop {
        if pn_is_null(pn_iter) {
            comp.compile_node(pn_inner_expr);
            let sc = comp.scope_cur.expect("scope_comp_iter: no scope_cur");
            if comp.scopes.scopes[sc].kind == ScopeKind::GenExpr {
                comp.emit
                    .yield_op(MP_EMIT_YIELD_VALUE, &mut comp.scopes);
                comp.emit.pop_top(&mut comp.scopes);
            } else {
                let kind = comp.scopes.scopes[sc].kind;
                comp.emit
                    .store_comp(kind, 4 * for_depth as usize + 5, &mut comp.scopes);
            }
            break;
        }
        if !pn_is_struct(pn_iter) {
            break;
        }
        let pns_iter_kind = comp.tree.struct_kind(pn_iter);
        if pns_iter_kind == grammar::RULE_comp_if {
            let iter_nodes: Vec<Pn> = comp.tree.struct_nodes(pn_iter).to_vec();
            c_if_cond(comp, iter_nodes[0], false, l_top);
            pn_iter = iter_nodes[1];
            continue;
        }
        debug_assert_eq!(pns_iter_kind, grammar::RULE_comp_for);
        // Nested for: compile the new iter (in this scope, since we're
        // inside the comprehension body now), GET_ITER_STACK, recurse.
        let iter_nodes: Vec<Pn> = comp.tree.struct_nodes(pn_iter).to_vec();
        comp.compile_node(iter_nodes[1]);
        comp.emit.get_iter(true, &mut comp.scopes);
        compile_scope_comp_iter(comp, pn_iter, pn_inner_expr, for_depth + 1);
        break;
    }

    comp.emit.jump(l_top as usize, &mut comp.scopes);
    comp.emit.label_assign(l_end as usize);
    comp.emit.for_iter_end(&mut comp.scopes);
}


pub use super::funcdef_call::{compile_atom_brace_helper, compile_trailer_paren_helper};
pub use super::funcdef_params::{c_scope_func_param, c_scope_lambda_param, close_over_variables_etc, compile_classdef_helper, compile_funcdef_helper};
