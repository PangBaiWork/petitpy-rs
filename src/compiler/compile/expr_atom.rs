//! Atom-expr + trailers + call — §22 of `compile.zig`.
//!
//! Atom expressions, trailers (call/subscript/attribute),
//! dict/set makers, testlists. Split out of `expr.rs`
//! (file-size rule); re-exported from there.

use super::funcdef::compile_comprehension_stub;
use super::funcdef_call::{compile_atom_brace_helper, compile_trailer_paren_helper};
use super::{wk_idx, Compiler};
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_struct,
    pn_leaf_arg, Pn, };
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{ScopeKind};

// §22 — Atom-expr + trailers + call.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_atom_expr_normal(comp: &mut Compiler, pn: Pn) {
    // Vendor-faithful port of compile_atom_expr_normal (py/compile.c:2300).
    // Two peepholes:
    //   (a) super() within a function -> LOAD __class__ + LOAD <self> + ...,
    //       with an extra fold when followed by .name(...) so we emit
    //       LOAD_SUPER_METHOD + CALL_METHOD instead of allocating a super
    //       bound-method object.
    //   (b) any .name FOLLOWED BY (...) -> LOAD_METHOD + CALL_METHOD
    //       (PyPy-style method-call optimisation).
    // OrderedDict({...}) peephole is gated by MICROPY_COMP_CONST_LITERAL,
    // which is 0 at nano's ROM level — omitted here, matches vendor under
    // the same #ifdef.

    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();

    // 1. Compile the subject of the expression.
    comp.compile_node(nodes[0]);

    // 2. compile_atom_expr_await may call us with a NULL node.
    if pn_is_null(nodes[1]) {
        return;
    }

    // 3. Resolve the trailer array. Either a single trailer at nodes[1]
    //    or an atom_expr_trailers wrapper holding the chain.
    let trail_nodes: Vec<Pn> = if comp
        .tree
        .is_struct_kind(nodes[1], grammar::RULE_atom_expr_trailers)
    {
        comp.tree.struct_nodes(nodes[1]).to_vec()
    } else {
        vec![nodes[1]]
    };
    let num_trail = trail_nodes.len();

    let mut i: usize = 0;

    // 4. super() peephole — only inside a function body, when the atom
    //    is the identifier `super` and the first trailer is `()`.
    let sc = comp.scope_cur.expect("atom_expr_normal: no scope_cur");
    let t0 = trail_nodes[0];
    let is_paren_empty = comp
        .tree
        .is_struct_kind(t0, grammar::RULE_trailer_paren)
        && pn_is_null(comp.tree.struct_nodes(t0)[0]);
    if comp.scopes.scopes[sc].kind == ScopeKind::Function
        && pn_is_id(nodes[0])
        && pn_leaf_arg(nodes[0]) as Qstr == comp.wk(wk_idx::SUPER)
        && is_paren_empty
    {
        // Matched "super()" inside a function.

        // Load the enclosing class so super can search for a parent.
        let class_q = comp.wk(wk_idx::CLASS);
        comp.compile_load_id(class_q);

        // Locate the first parameter (assumed to be `self`) and load it.
        let mut found = false;
        for id in &comp.scopes.scopes[sc].id_info {
            if (id.flags & crate::compiler::scope::ID_FLAG_IS_PARAM) != 0 {
                comp.compile_load_id(id.qst);
                found = true;
                break;
            }
        }
        if !found {
            // Really a TypeError; vendor uses syntax-error machinery.
            comp.compile_syntax_error(trail_nodes[0], "super() can't find self");
            return;
        }

        // super().method(...) fold: eliminates the super bound-method
        // heap allocation by going straight to LOAD_SUPER_METHOD + CALL.
        if num_trail >= 3
            && comp
                .tree
                .is_struct_kind(trail_nodes[1], grammar::RULE_trailer_period)
            && comp
                .tree
                .is_struct_kind(trail_nodes[2], grammar::RULE_trailer_paren)
        {
            let period_nodes: Vec<Pn> = comp.tree.struct_nodes(trail_nodes[1]).to_vec();
            let paren_nodes: Vec<Pn> = comp.tree.struct_nodes(trail_nodes[2]).to_vec();
            let mname = pn_leaf_arg(period_nodes[0]) as Qstr;
            comp.emit
                .load_method(mname, true, &mut comp.scopes);
            compile_trailer_paren_helper(comp, paren_nodes[0], true, 0);
            i = 3;
        } else {
            // Bare super() call: 2 positional args (class, self) on stack.
            comp.emit.call_function(2, 0, 0, &mut comp.scopes);
            i = 1;
        }
    }

    // 5. Walk remaining trailers, applying the LOAD_METHOD peephole on
    //    every adjacent (.name)+(paren) pair.
    while i < num_trail {
        if i + 1 < num_trail
            && comp
                .tree
                .is_struct_kind(trail_nodes[i], grammar::RULE_trailer_period)
            && comp
                .tree
                .is_struct_kind(trail_nodes[i + 1], grammar::RULE_trailer_paren)
        {
            let period_nodes: Vec<Pn> = comp.tree.struct_nodes(trail_nodes[i]).to_vec();
            let paren_nodes: Vec<Pn> =
                comp.tree.struct_nodes(trail_nodes[i + 1]).to_vec();
            let mname = pn_leaf_arg(period_nodes[0]) as Qstr;
            comp.emit
                .load_method(mname, false, &mut comp.scopes);
            compile_trailer_paren_helper(comp, paren_nodes[0], true, 0);
            i += 1;
        } else {
            // trailer_paren, trailer_bracket, or trailer_period — dispatch
            // through the normal handler table.
            comp.compile_node(trail_nodes[i]);
        }
        i += 1;
    }
}

pub fn compile_trailer_paren(comp: &mut Compiler, pn: Pn) {
    // Vendor compile.c:2665 — thin wrapper. The helper is shared with
    // method-call sites (compile_atom_expr_normal's load_method peephole).
    let arg = comp.tree.struct_nodes(pn)[0];
    compile_trailer_paren_helper(comp, arg, false, 0);
}

pub fn compile_trailer_bracket(comp: &mut Compiler, pn: Pn) {
    let arg = comp.tree.struct_nodes(pn)[0];
    comp.compile_node(arg);
    comp.emit.subscr(MP_EMIT_SUBSCR_LOAD, &mut comp.scopes);
}

/// Vendor compile_subscript (compile.c:2681). Builds a SLICE object
/// when called from compile_trailer_bracket → compile_node for a slice
/// expression like x[a:b] or x[a:b:c]. The subscript-list case
/// (multi-subscript like x[a, b]) routes through compile_subscriptlist,
/// which wraps in a tuple and lets BUILD_SUBSCR consume that.
pub fn compile_subscript(comp: &mut Compiler, pn: Pn) {
    let mut p = pn;
    let k0 = comp.tree.struct_kind(p);
    if k0 == grammar::RULE_subscript_2 {
        let nodes: Vec<Pn> = comp.tree.struct_nodes(p).to_vec();
        comp.compile_node(nodes[0]); // start
        debug_assert!(pn_is_struct(nodes[1]));
        p = nodes[1];
    } else {
        // PN_subscript_3 — start is None.
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    }
    debug_assert_eq!(comp.tree.struct_kind(p), grammar::RULE_subscript_3);

    let pn = comp.tree.struct_nodes(p)[0];
    if pn_is_null(pn) {
        // [?:]
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
        comp.emit
            .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
    } else if pn_is_struct(pn) {
        let k_inner = comp.tree.struct_kind(pn);
        if k_inner == grammar::RULE_subscript_3c {
            // [?:?:?]
            comp.emit
                .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
            let pn2 = comp.tree.struct_nodes(pn)[0];
            if pn_is_null(pn2) {
                comp.emit
                    .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
            } else {
                comp.compile_node(pn2);
                comp.emit
                    .build(3, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
            }
        } else if k_inner == grammar::RULE_subscript_3d {
            let inner_nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
            comp.compile_node(inner_nodes[0]);
            debug_assert!(pn_is_struct(inner_nodes[1]));
            let slop = inner_nodes[1];
            debug_assert_eq!(
                comp.tree.struct_kind(slop),
                grammar::RULE_sliceop
            );
            if pn_is_null(comp.tree.struct_nodes(slop)[0]) {
                comp.emit
                    .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
            } else {
                let step = comp.tree.struct_nodes(slop)[0];
                comp.compile_node(step);
                comp.emit
                    .build(3, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
            }
        } else {
            // [?:x]
            comp.compile_node(pn);
            comp.emit
                .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
        }
    } else {
        // [?:x]
        comp.compile_node(pn);
        comp.emit
            .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
    }
}

/// Vendor compile_subscriptlist (compile.c:2750-ish).
/// `x[a, b]` shape: builds a tuple of the subscripts, then leaves it
/// on the stack for the enclosing trailer_bracket's LOAD_SUBSCR.
pub fn compile_subscriptlist(comp: &mut Compiler, pn: Pn) {
    super::handlers::compile_generic_tuple(comp, pn);
}

/// Vendor compile_dictorsetmaker_item (compile.c:2736).
/// dict key:value pair — value pushed first, key second (vendor swaps
/// order to match STORE_MAP's stack expectation).
pub fn compile_dictorsetmaker_item(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    comp.compile_node(nodes[1]); // value
    comp.compile_node(nodes[0]); // key
}

/// PEP 448 `{**expr}` inside a dict display. Assumes the growing dict
/// is already on TOS; emits an in-place merge:
///   DUP_TOP; LOAD_METHOD update; <expr>; CALL_METHOD 1; POP_TOP
/// Net stack delta: 0. The pop discards update()'s None return.
pub fn compile_dictorsetmaker_dbl_star(comp: &mut Compiler, pn: Pn) {
    comp.emit.dup_top(&mut comp.scopes);
    let update_q = comp.wk(wk_idx::UPDATE);
    comp.emit
        .load_method(update_q, false, &mut comp.scopes);
    let arg = comp.tree.struct_nodes(pn)[0];
    comp.compile_node(arg);
    comp.emit.call_method(1, 0, 0, &mut comp.scopes);
    comp.emit.pop_top(&mut comp.scopes);
}

pub fn compile_trailer_period(comp: &mut Compiler, pn: Pn) {
    let q = pn_leaf_arg(comp.tree.struct_nodes(pn)[0]) as Qstr;
    comp.emit.attr(q, MP_EMIT_ATTR_LOAD, &mut comp.scopes);
}

/// Vendor MP_PARSE_NODE_TESTLIST_COMP_HAS_COMP_FOR (compile.c:62).
/// A testlist_comp is "(x for x in ...)" or "[x for x in ...)" shape
/// when it has exactly two nodes and the second is a comp_for struct.
fn testlist_comp_has_comp_for(comp: &Compiler, pn: Pn) -> bool {
    comp.tree.struct_nodes(pn).len() == 2
        && comp
            .tree
            .is_struct_kind(comp.tree.struct_nodes(pn)[1], grammar::RULE_comp_for)
}

pub fn compile_atom_paren(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_atom_paren (compile.c:2521).
    let inner = comp.tree.struct_nodes(pn)[0];
    if pn_is_null(inner) {
        comp.emit
            .build(0, MP_EMIT_BUILD_TUPLE, &mut comp.scopes);
        return;
    }
    debug_assert!(comp
        .tree
        .is_struct_kind(inner, grammar::RULE_testlist_comp));
    if testlist_comp_has_comp_for(comp, inner) {
        compile_comprehension_stub(comp, inner, ScopeKind::GenExpr);
    } else {
        super::handlers::compile_generic_tuple(comp, inner);
    }
}

pub fn compile_atom_bracket(comp: &mut Compiler, pn: Pn) {
    // Vendor compile_atom_bracket (compile.c:2538).
    let inner = comp.tree.struct_nodes(pn)[0];
    if pn_is_null(inner) {
        comp.emit
            .build(0, MP_EMIT_BUILD_LIST, &mut comp.scopes);
        return;
    }
    if comp
        .tree
        .is_struct_kind(inner, grammar::RULE_testlist_comp)
    {
        if testlist_comp_has_comp_for(comp, inner) {
            compile_comprehension_stub(comp, inner, ScopeKind::ListComp);
        } else {
            let inner_nodes: Vec<Pn> = comp.tree.struct_nodes(inner).to_vec();
            let n = inner_nodes.len();
            if children_have_star_expr(comp, &inner_nodes) {
                emit_list_with_star_unpack(comp, &inner_nodes);
            } else {
                for child in &inner_nodes {
                    comp.compile_node(*child);
                }
                comp.emit
                    .build(n, MP_EMIT_BUILD_LIST, &mut comp.scopes);
            }
        }
    } else {
        // Single item — a lone `*expr` is `[*expr]`, emit as unpack of one.
        if comp
            .tree
            .is_struct_kind(inner, grammar::RULE_star_expr)
        {
            emit_list_with_star_unpack(comp, &[inner]);
        } else {
            comp.compile_node(inner);
            comp.emit
                .build(1, MP_EMIT_BUILD_LIST, &mut comp.scopes);
        }
    }
}

fn children_have_star_expr(comp: &Compiler, nodes: &[Pn]) -> bool {
    nodes
        .iter()
        .any(|n| comp.tree.is_struct_kind(*n, grammar::RULE_star_expr))
}

/// PEP 448 list-unpack emit. Assumes at least one child is star_expr;
/// mixed forms like `[1, *a, 2]` fall through here too. Emits
///   BUILD_LIST 0
///   for each plain x:   DUP_TOP; LOAD_METHOD append; <x>; CALL_METHOD 1; POP_TOP
///   for each *e:        DUP_TOP; LOAD_METHOD extend; <e>; CALL_METHOD 1; POP_TOP
/// Net delta: +1 (the list stays on TOS).
fn emit_list_with_star_unpack(comp: &mut Compiler, nodes: &[Pn]) {
    comp.emit
        .build(0, MP_EMIT_BUILD_LIST, &mut comp.scopes);
    for node in nodes {
        if comp.tree.is_struct_kind(*node, grammar::RULE_star_expr) {
            let star_nodes: Vec<Pn> = comp.tree.struct_nodes(*node).to_vec();
            comp.emit.dup_top(&mut comp.scopes);
            let extend_q = comp.wk(wk_idx::EXTEND);
            comp.emit
                .load_method(extend_q, false, &mut comp.scopes);
            comp.compile_node(star_nodes[0]);
            comp.emit.call_method(1, 0, 0, &mut comp.scopes);
            comp.emit.pop_top(&mut comp.scopes);
        } else {
            comp.emit.dup_top(&mut comp.scopes);
            let append_q = comp.wk(wk_idx::APPEND);
            comp.emit
                .load_method(append_q, false, &mut comp.scopes);
            comp.compile_node(*node);
            comp.emit.call_method(1, 0, 0, &mut comp.scopes);
            comp.emit.pop_top(&mut comp.scopes);
        }
    }
}

pub fn compile_atom_brace(comp: &mut Compiler, pn: Pn) {
    // Vendor compile.c:2661 — thin wrapper around the helper with
    // create_map=true. The helper is shared with compile_atom_brace_helper
    // callers (e.g. class-body dict construction).
    compile_atom_brace_helper(comp, pn, true);
}

pub fn compile_testlist_comp(comp: &mut Compiler, pn: Pn) {
    // Tuple constructor body: build the tuple.
    super::handlers::compile_generic_tuple(comp, pn);
}

pub fn compile_testlist(comp: &mut Compiler, pn: Pn) {
    super::handlers::compile_generic_tuple(comp, pn);
}

