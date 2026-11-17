//! Call/brace helpers — `compile_trailer_paren_helper` +
//! `compile_atom_brace_helper` from `compile.zig`.
//!
//! Shared by the atom-expr compilers (§22) and the funcdef
//! machinery. Split out of `funcdef.rs` (file-size rule);
//! re-exported from there.

use super::expr::{compile_namedexpr_helper};
use super::funcdef::compile_comprehension_stub;
use super::{Compiler};
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_struct, pn_leaf_arg, Pn,
};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{
    ScopeKind, };


/// MP_SMALL_INT_BITS — bit-width of the tagged small-int payload
/// (63 on 64-bit hosts). Used by the `*args` count guard in
/// compile_trailer_paren_helper.
const SMALL_INT_BITS: usize = 63;

// Vendor compile_trailer_paren_helper (compile.c:2401).
pub fn compile_trailer_paren_helper(
    comp: &mut Compiler,
    pn_arglist_in: Pn,
    is_method_call: bool,
    n_positional_extra: usize,
) {
    let pn_arglist = pn_arglist_in;
    let args = comp.tree.extract_list(pn_arglist, grammar::RULE_arglist);
    let n_args = args.len();

    let mut n_positional: usize = n_positional_extra;
    let mut n_keyword: usize = 0;
    let mut star_flags: u8 = 0;
    let mut star_args: usize = 0;

    let mut i: usize = 0;
    while i < n_args {
        let arg = args[i];
        let mut handled_as_normal = false;

        if pn_is_struct(arg) {
            let kind = comp.tree.struct_kind(arg);
            let arg_nodes: Vec<Pn> = comp.tree.struct_nodes(arg).to_vec();

            if kind == grammar::RULE_arglist_star {
                if (star_flags & MP_EMIT_STAR_FLAG_DOUBLE) != 0 {
                    comp.compile_syntax_error(arg, "* arg after **");
                    return;
                }
                if i >= SMALL_INT_BITS - 1 {
                    comp.compile_syntax_error(arg, "too many args");
                    return;
                }
                star_flags |= MP_EMIT_STAR_FLAG_SINGLE;
                star_args |= 1usize << i;
                comp.compile_node(arg_nodes[0]);
                n_positional += 1;
            } else if kind == grammar::RULE_arglist_dbl_star {
                star_flags |= MP_EMIT_STAR_FLAG_DOUBLE;
                comp.emit.load_null(&mut comp.scopes);
                comp.compile_node(arg_nodes[0]);
                n_keyword += 1;
            } else if kind == grammar::RULE_argument {
                let arg0 = arg_nodes[0];
                let arg1 = arg_nodes[1];
                if comp
                    .tree
                    .is_struct_kind(arg1, grammar::RULE_argument_3)
                {
                    let walrus_nodes: Vec<Pn> = comp.tree.struct_nodes(arg1).to_vec();
                    compile_namedexpr_helper(comp, arg0, walrus_nodes[0]);
                    n_positional += 1;
                } else if !comp
                    .tree
                    .is_struct_kind(arg1, grammar::RULE_comp_for)
                {
                    if !pn_is_id(arg0) {
                        comp.compile_syntax_error(arg, "LHS of keyword arg must be an id");
                        return;
                    }
                    comp.emit
                        .load_const_str(pn_leaf_arg(arg0) as Qstr, &mut comp.scopes);
                    comp.compile_node(arg1);
                    n_keyword += 1;
                } else {
                    compile_comprehension_stub(comp, arg, ScopeKind::GenExpr);
                    n_positional += 1;
                }
            } else {
                handled_as_normal = true;
            }
        } else {
            handled_as_normal = true;
        }

        if handled_as_normal {
            if (star_flags & MP_EMIT_STAR_FLAG_DOUBLE) != 0 {
                comp.compile_syntax_error(arg, "positional arg after **");
                return;
            }
            if n_keyword > 0 {
                comp.compile_syntax_error(arg, "positional arg after keyword arg");
                return;
            }
            comp.compile_node(arg);
            n_positional += 1;
        }
        i += 1;
    }

    if star_flags != 0 {
        comp.emit
            .load_const_small_int(star_args as isize, &mut comp.scopes);
    }

    if is_method_call {
        comp.emit
            .call_method(n_positional, n_keyword, star_flags, &mut comp.scopes);
    } else {
        comp.emit
            .call_function(n_positional, n_keyword, star_flags, &mut comp.scopes);
    }
}

// Vendor compile_atom_brace_helper (compile.c:2559).
pub fn compile_atom_brace_helper(comp: &mut Compiler, pns_in: Pn, create_map: bool) {
    let pn = comp.tree.struct_nodes(pns_in)[0];
    if pn_is_null(pn) {
        if create_map {
            comp.emit
                .build(0, MP_EMIT_BUILD_MAP, &mut comp.scopes);
        }
        return;
    }

    if !pn_is_struct(pn) {
        comp.compile_node(pn);
        comp.emit
            .build(1, MP_EMIT_BUILD_SET, &mut comp.scopes);
        return;
    }

    let kind = comp.tree.struct_kind(pn);

    if kind == grammar::RULE_dictorsetmaker_item {
        if create_map {
            comp.emit
                .build(1, MP_EMIT_BUILD_MAP, &mut comp.scopes);
        }
        comp.compile_node(pn);
        comp.emit.store_map(&mut comp.scopes);
        return;
    }

    if kind == grammar::RULE_dictorsetmaker_dbl_star {
        // Single-item dict `{**d}` — build empty, merge in place.
        if create_map {
            comp.emit
                .build(0, MP_EMIT_BUILD_MAP, &mut comp.scopes);
        }
        comp.compile_node(pn);
        return;
    }

    if kind == grammar::RULE_dictorsetmaker {
        let ds_nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        debug_assert!(pn_is_struct(ds_nodes[1]));
        let pns1_kind = comp.tree.struct_kind(ds_nodes[1]);

        if pns1_kind == grammar::RULE_dictorsetmaker_list {
            let pn_tail = comp.tree.struct_nodes(ds_nodes[1])[0];
            let tail = comp
                .tree
                .extract_list(pn_tail, grammar::RULE_dictorsetmaker_list2);
            let n_tail = tail.len();

            let first_is_kv = comp
                .tree
                .is_struct_kind(ds_nodes[0], grammar::RULE_dictorsetmaker_item);
            let first_is_dbl = comp.tree.is_struct_kind(
                ds_nodes[0],
                grammar::RULE_dictorsetmaker_dbl_star,
            );
            let is_dict = first_is_kv || first_is_dbl;
            if is_dict {
                if create_map {
                    comp.emit.build(
                        1 + n_tail,
                        MP_EMIT_BUILD_MAP,
                        &mut comp.scopes,
                    );
                }
                if first_is_kv {
                    comp.compile_node(ds_nodes[0]);
                    comp.emit.store_map(&mut comp.scopes);
                } else {
                    // dbl_star merges in place; no store_map follow-up.
                    comp.compile_node(ds_nodes[0]);
                }
            } else {
                comp.compile_node(ds_nodes[0]);
            }

            for pn_i in &tail {
                let is_key_value = comp
                    .tree
                    .is_struct_kind(*pn_i, grammar::RULE_dictorsetmaker_item);
                let is_dbl_star = comp.tree.is_struct_kind(
                    *pn_i,
                    grammar::RULE_dictorsetmaker_dbl_star,
                );
                if is_dict {
                    if !is_key_value && !is_dbl_star {
                        comp.compile_syntax_error(pn, "expecting key:value for dict");
                        return;
                    }
                    comp.compile_node(*pn_i);
                    if is_key_value {
                        comp.emit.store_map(&mut comp.scopes);
                    }
                    // dbl_star merges in place — nothing more to emit.
                } else {
                    if is_key_value {
                        comp.compile_syntax_error(pn, "expecting just a value for set");
                        return;
                    }
                    if is_dbl_star {
                        comp.compile_syntax_error(
                            pn,
                            "** unpacking not allowed in set literal",
                        );
                        return;
                    }
                    comp.compile_node(*pn_i);
                }
            }

            if !is_dict {
                comp.emit.build(
                    1 + n_tail,
                    MP_EMIT_BUILD_SET,
                    &mut comp.scopes,
                );
            }
            return;
        }

        debug_assert_eq!(pns1_kind, grammar::RULE_comp_for);
        if comp.tree.is_struct_kind(
            ds_nodes[0],
            grammar::RULE_dictorsetmaker_dbl_star,
        ) {
            comp.compile_syntax_error(
                pn,
                "dict unpacking cannot be used in dict comprehension",
            );
            return;
        }
        let scope_kind: ScopeKind = if comp.tree.is_struct_kind(
            ds_nodes[0],
            grammar::RULE_dictorsetmaker_item,
        ) {
            ScopeKind::DictComp
        } else {
            ScopeKind::SetComp
        };
        compile_comprehension_stub(comp, pn, scope_kind);
        return;
    }

    comp.compile_node(pn);
    comp.emit
        .build(1, MP_EMIT_BUILD_SET, &mut comp.scopes);
}
