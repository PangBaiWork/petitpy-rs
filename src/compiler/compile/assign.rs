//! Assignment targets — §10 of `compile.zig`.
//!
//! `c_assign` compiles a single assignment target in one of three modes:
//! `Store` (plain `=`), `AugLoad` (read the old value for `+=` etc.),
//! `AugStore` (write the computed value back). `c_assign_tuple` handles
//! tuple/list targets (with star handling), `c_assign_tuple_flat`
//! unpacks a statically-known target list without emitting a tuple, and
//! `c_assign_atom_expr` walks `a.b` / `a[b]` chains.
//!
//! The AugLoad/AugStore stack discipline is byte-critical and subtle:
//! AugLoad leaves the container on the stack under the loaded value
//! (`a.b += 1`: `[obj, value]`; `a[b] += 1`: `[obj, index, value]`),
//! and AugStore does NOT recompile the chain — it rotates the computed
//! value into place (`rot_two` for `.attr`, `rot_three` for `[subscr]`).

use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_leaf_arg, Pn,
};
use crate::compiler::qstr::Qstr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignKind {
    Store,
    AugLoad,
    AugStore,
}

/// Vendor `cAssign` (compile.c:530-590).
pub fn c_assign(comp: &mut Compiler, pn: Pn, kind: AssignKind) {
    if pn_is_id(pn) {
        let qst = pn_leaf_arg(pn) as Qstr;
        match kind {
            AssignKind::Store => comp.compile_store_id(qst),
            AssignKind::AugLoad => comp.compile_load_id(qst),
            AssignKind::AugStore => comp.compile_store_id(qst),
        }
        return;
    }

    if comp.tree.is_struct_kind(pn, grammar::RULE_atom_expr_normal) {
        c_assign_atom_expr(comp, pn, kind);
        return;
    }

    if comp.tree.is_struct_kind(pn, grammar::RULE_testlist_star_expr)
        || comp.tree.is_struct_kind(pn, grammar::RULE_testlist_comp)
        || comp.tree.is_struct_kind(pn, grammar::RULE_exprlist)
    {
        // Vendor c_assign:553-560 rejects augmented-assignment to a
        // tuple LHS ("a, b += c" / "(a, b) += c" / "[a, b] += c").
        if kind != AssignKind::Store {
            comp.compile_syntax_error(pn, "can't assign to expression");
            return;
        }
        c_assign_tuple(comp, pn);
        return;
    }

    if comp.tree.is_struct_kind(pn, grammar::RULE_atom_paren) {
        // `(a, b) = ...` — vendor compile.c:562. Empty paren is a
        // SyntaxError; otherwise the inner MUST be a testlist_comp
        // (single-name `(a)` collapses to `a` at parse time and never
        // reaches this branch). Sub-targets unpack via c_assign_tuple.
        let inner_opt = comp.tree.struct_nodes(pn).first().copied();
        let Some(inner) = inner_opt else {
            comp.compile_syntax_error(pn, "can't assign to expression");
            return;
        };
        if pn_is_null(inner) {
            comp.compile_syntax_error(pn, "can't assign to ()");
            return;
        }
        if kind != AssignKind::Store {
            comp.compile_syntax_error(pn, "can't assign to expression");
            return;
        }
        if comp
            .tree
            .is_struct_kind(inner, grammar::RULE_testlist_comp)
        {
            c_assign_tuple(comp, inner);
            return;
        }
        // Fallback for unusual single-item paren patterns the parser
        // might produce. Treat as `(x,)` semantic — UNPACK_SEQUENCE 1.
        c_assign_tuple_flat(comp, &[inner]);
        return;
    }

    if comp.tree.is_struct_kind(pn, grammar::RULE_atom_bracket) {
        // `[a, b] = ...` — vendor compile.c:577. Three sub-cases:
        //   * `[]`             — empty unpack (UNPACK_SEQUENCE 0)
        //   * `[a]`            — single-item unpack (UNPACK_SEQUENCE 1)
        //   * `[a, b, ...]`    — testlist_comp → c_assign_tuple full
        if kind != AssignKind::Store {
            comp.compile_syntax_error(pn, "can't assign to expression");
            return;
        }
        if comp.tree.struct_nodes(pn).is_empty() {
            // Truly empty struct (zero slots) — emit UNPACK_SEQUENCE 0.
            c_assign_tuple_flat(comp, &[]);
            return;
        }
        let inner = comp.tree.struct_nodes(pn)[0];
        if pn_is_null(inner) {
            // `[]` — empty list, allowed; emit UNPACK_SEQUENCE 0.
            c_assign_tuple_flat(comp, &[]);
            return;
        }
        if comp
            .tree
            .is_struct_kind(inner, grammar::RULE_testlist_comp)
        {
            c_assign_tuple(comp, inner);
            return;
        }
        // `[a]` — single-item list pattern. Vendor passes `pns->nodes`
        // (the 1-element array) to c_assign_tuple with num_tail=1.
        c_assign_tuple_flat(comp, &[inner]);
        return;
    }

    comp.compile_syntax_error(pn, "can't assign to expression");
}

/// Helper for the atom_paren/atom_bracket single-item or empty cases
/// that vendor's c_assign handles by calling c_assign_tuple with an
/// inline array. Lifted into its own function so c_assign_tuple can
/// stay focused on the testlist_comp / star-target codepath.
pub fn c_assign_tuple_flat(comp: &mut Compiler, targets: &[Pn]) {
    let mut star_idx: Option<usize> = None;
    for (i, t) in targets.iter().enumerate() {
        if comp.tree.is_struct_kind(*t, grammar::RULE_star_expr) {
            if star_idx.is_some() {
                comp.compile_syntax_error(*t, "multiple starred targets");
                return;
            }
            star_idx = Some(i);
        }
    }

    if let Some(s) = star_idx {
        comp.emit
            .unpack_ex(s, targets.len() - s - 1, &mut comp.scopes);
    } else {
        comp.emit
            .unpack_sequence(targets.len(), &mut comp.scopes);
    }

    for t in targets {
        let mut tgt = *t;
        if comp.tree.is_struct_kind(tgt, grammar::RULE_star_expr) {
            tgt = comp.tree.struct_nodes(tgt)[0];
        }
        c_assign(comp, tgt, AssignKind::Store);
    }
}

/// `a.b = ...` / `a[b] = ...` — compile the chain except the last
/// trailer, then emit STORE_ATTR / STORE_SUBSCR. Mirrors vendor
/// c_assign_atom_expr (compile.c:450).
fn c_assign_atom_expr(comp: &mut Compiler, pn: Pn, kind: AssignKind) {
    // Vendor c_assign_atom_expr (compile.c:450). pn is atom_expr_normal
    // with 2 children: nodes[0] = atom, nodes[1] = trailer(s).
    //
    // If nodes[1] is RULE_atom_expr_trailers, it wraps N trailers (the
    // chain `a.b[c].d` shape). Otherwise nodes[1] IS the single trailer.
    // In either case we compile every trailer except the last as a load,
    // then dispatch on the last trailer's kind to emit STORE_ATTR or
    // STORE_SUBSCR.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    debug_assert_eq!(nodes.len(), 2);

    // ASSIGN_AUG_STORE skips re-compiling the LHS chain — the
    // augmented-load pass already left the result on the stack.
    if kind != AssignKind::AugStore {
        comp.compile_node(nodes[0]);
    }

    let mut last_trailer: Pn = nodes[1];
    if comp
        .tree
        .is_struct_kind(nodes[1], grammar::RULE_atom_expr_trailers)
    {
        let trail_nodes: Vec<Pn> = comp.tree.struct_nodes(nodes[1]).to_vec();
        let trail_n = trail_nodes.len();
        if kind != AssignKind::AugStore {
            let mut i: usize = 0;
            while i + 1 < trail_n {
                comp.compile_node(trail_nodes[i]);
                i += 1;
            }
        }
        last_trailer = trail_nodes[trail_n - 1];
    }

    if comp
        .tree
        .is_struct_kind(last_trailer, grammar::RULE_trailer_bracket)
    {
        let lpns: Vec<Pn> = comp.tree.struct_nodes(last_trailer).to_vec();
        if kind == AssignKind::AugStore {
            comp.emit.rot_three(&mut comp.scopes);
            comp.emit.subscr(MP_EMIT_SUBSCR_STORE, &mut comp.scopes);
        } else {
            comp.compile_node(lpns[0]);
            if kind == AssignKind::AugLoad {
                comp.emit.dup_top_two(&mut comp.scopes);
                comp.emit.subscr(MP_EMIT_SUBSCR_LOAD, &mut comp.scopes);
            } else {
                comp.emit.subscr(MP_EMIT_SUBSCR_STORE, &mut comp.scopes);
            }
        }
        return;
    }
    if comp
        .tree
        .is_struct_kind(last_trailer, grammar::RULE_trailer_period)
    {
        let lpns: Vec<Pn> = comp.tree.struct_nodes(last_trailer).to_vec();
        let q = pn_leaf_arg(lpns[0]) as Qstr;
        if kind == AssignKind::AugLoad {
            comp.emit.dup_top(&mut comp.scopes);
            comp.emit.attr(q, MP_EMIT_ATTR_LOAD, &mut comp.scopes);
        } else {
            if kind == AssignKind::AugStore {
                comp.emit.rot_two(&mut comp.scopes);
            }
            comp.emit.attr(q, MP_EMIT_ATTR_STORE, &mut comp.scopes);
        }
        return;
    }

    comp.compile_syntax_error(pn, "can't assign to expression");
}

/// `a, b, c = ...` — UNPACK_SEQUENCE (or UNPACK_EX if a star target is
/// present), then assign each component recursively. Mirrors vendor
/// c_assign_tuple.
fn c_assign_tuple(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let n = nodes.len();

    // Find a star target, if any.
    let mut star_idx: Option<usize> = None;
    for (i, t) in nodes.iter().enumerate() {
        if comp.tree.is_struct_kind(*t, grammar::RULE_star_expr) {
            if star_idx.is_some() {
                comp.compile_syntax_error(pn, "multiple starred targets");
                return;
            }
            star_idx = Some(i);
        }
    }

    if let Some(s) = star_idx {
        let n_left = s;
        let n_right = n - s - 1;
        comp.emit
            .unpack_ex(n_left, n_right, &mut comp.scopes);
    } else {
        comp.emit.unpack_sequence(n, &mut comp.scopes);
    }

    // Assign each component.
    for t in &nodes {
        let mut tgt = *t;
        if comp.tree.is_struct_kind(tgt, grammar::RULE_star_expr) {
            tgt = comp.tree.struct_nodes(tgt)[0];
        }
        c_assign(comp, tgt, AssignKind::Store);
    }
}
