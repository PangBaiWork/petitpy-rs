//! Funcdef / lambdef codegen-pass helpers — §27 of `compile.zig`.
//!
//! Param walkers, `close_over_variables_etc`, funcdef/classdef
//! helpers, scope param registration. Split out of `funcdef.rs`
//! (file-size rule); re-exported from there.

use super::funcdef_call::compile_trailer_paren_helper;
use super::{Compiler, Pass};
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_struct, pn_leaf_arg, Pn,
};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{
    IdInfoKind, ScopeId, ScopeKind, ID_FLAG_IS_DBL_STAR_PARAM, ID_FLAG_IS_PARAM,
    ID_FLAG_IS_STAR_PARAM, SCOPE_FLAG_DEFKWARGS, SCOPE_FLAG_VARARGS,
    SCOPE_FLAG_VARKEYWORDS,
};



// ─────────────────────────────────────────────────────────────────────────
// §27 — Funcdef / lambdef codegen-pass helpers.
// ─────────────────────────────────────────────────────────────────────────

/// Codegen-pass param walker (compile.c:659).
fn compile_funcdef_lambdef_param(comp: &mut Compiler, pn: Pn) {
    let mut pn_kind: i32 = -1;
    if !pn_is_id(pn) {
        debug_assert!(pn_is_struct(pn));
        pn_kind = comp.tree.struct_kind(pn) as i32;
    }

    let is_star = pn_kind == grammar::RULE_typedargslist_star as i32
        || pn_kind == grammar::RULE_varargslist_star as i32;
    let is_dbl_star = pn_kind == grammar::RULE_typedargslist_dbl_star as i32
        || pn_kind == grammar::RULE_varargslist_dbl_star as i32;
    // PEP 570 `/` marker — MicroPython doesn't enforce positional-only
    // semantics; the grammar accepts the marker so imports don't fail,
    // and we skip it here (no binding, no default handling).
    let is_slash = pn_kind == grammar::RULE_typedargslist_slash as i32
        || pn_kind == grammar::RULE_varargslist_slash as i32;

    if is_star {
        comp.have_star = true;
        return;
    }
    if is_dbl_star {
        return;
    }
    if is_slash {
        return;
    }

    let pn_id: Pn;
    let mut pn_equal: Pn = crate::compiler::parser::PN_NULL;
    if pn_kind == -1 {
        pn_id = pn;
    } else if pn_kind == grammar::RULE_typedargslist_name as i32 {
        let inner: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        pn_id = inner[0];
        pn_equal = inner[2];
    } else {
        let inner: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        pn_id = inner[0];
        pn_equal = inner[1];
    }

    if pn_is_null(pn_equal) {
        if !comp.have_star && comp.num_default_params != 0 {
            comp.compile_syntax_error(pn, "non-default argument follows default argument");
        }
        return;
    }

    if comp.have_star {
        comp.num_dict_params += 1;
        if comp.num_dict_params == 1 {
            if comp.num_default_params > 0 {
                comp.emit.build(
                    comp.num_default_params as usize,
                    MP_EMIT_BUILD_TUPLE,
                    &mut comp.scopes,
                );
            } else {
                comp.emit.load_null(&mut comp.scopes);
            }
            comp.emit
                .build(0, MP_EMIT_BUILD_MAP, &mut comp.scopes);
        }
        comp.compile_node(pn_equal);
        comp.emit
            .load_const_str(pn_leaf_arg(pn_id) as Qstr, &mut comp.scopes);
        comp.emit.store_map(&mut comp.scopes);
    } else {
        comp.num_default_params += 1;
        comp.compile_node(pn_equal);
    }
}

// Port of close_over_variables_etc (compile.c:617).
pub fn close_over_variables_etc(
    comp: &mut Compiler,
    this_scope: ScopeId,
    n_pos_defaults: usize,
    n_kw_defaults: usize,
) {
    if n_kw_defaults > 0 {
        comp.scopes.scopes[this_scope].scope_flags |= SCOPE_FLAG_DEFKWARGS;
    }
    comp.scopes.scopes[this_scope].num_def_pos_args = n_pos_defaults as u16;

    let mut nfree: u8 = 0;
    let parent = comp.scope_cur.expect("close_over: no scope_cur");
    if comp.scopes.scopes[parent].kind != ScopeKind::Module {
        let parent_ids: Vec<(IdInfoKind, Qstr, u16)> = comp.scopes.scopes[parent]
            .id_info
            .iter()
            .map(|id| (id.kind, id.qst, id.local_num))
            .collect();
        // Free ids of the child scope, collected up front: the loop
        // below calls `load_local(&mut comp.scopes)` and cannot hold
        // an immutable borrow of the same arena.
        let this_free: Vec<Qstr> = comp.scopes.scopes[this_scope]
            .id_info
            .iter()
            .filter(|id| id.kind == IdInfoKind::Free)
            .map(|id| id.qst)
            .collect();
        for (id_kind, id_qst, id_local_num) in parent_ids {
            if (id_kind == IdInfoKind::Cell || id_kind == IdInfoKind::Free)
                && this_free.contains(&id_qst)
            {
                comp.emit.load_local(
                    id_local_num,
                    MP_EMIT_IDOP_LOCAL_FAST,
                    &mut comp.scopes,
                );
                nfree += 1;
            }
        }
    }

    if nfree == 0 {
        comp.emit
            .make_function(this_scope, n_pos_defaults, n_kw_defaults, &mut comp.scopes);
    } else {
        comp.emit.make_closure(
            this_scope,
            nfree,
            n_pos_defaults,
            n_kw_defaults,
            &mut comp.scopes,
        );
    }
}

// Port of compile_funcdef_lambdef (compile.c:750).
pub(crate) fn compile_funcdef_lambdef(
    comp: &mut Compiler,
    scope: ScopeId,
    pn_params: Pn,
    pn_list_kind: u8,
) {
    let orig_have_star = comp.have_star;
    let orig_num_dict = comp.num_dict_params;
    let orig_num_default = comp.num_default_params;

    comp.have_star = false;
    comp.num_dict_params = 0;
    comp.num_default_params = 0;
    comp.apply_to_single_or_list(pn_params, pn_list_kind, compile_funcdef_lambdef_param);

    if comp.compile_error.is_some() {
        return;
    }

    if comp.num_default_params > 0 && comp.num_dict_params == 0 {
        comp.emit.build(
            comp.num_default_params as usize,
            MP_EMIT_BUILD_TUPLE,
            &mut comp.scopes,
        );
        comp.emit.load_null(&mut comp.scopes);
    }

    close_over_variables_etc(
        comp,
        scope,
        comp.num_default_params as usize,
        comp.num_dict_params as usize,
    );

    comp.have_star = orig_have_star;
    comp.num_dict_params = orig_num_dict;
    comp.num_default_params = orig_num_default;
}

// Port of compile_funcdef_helper (compile.c:786).
pub fn compile_funcdef_helper(comp: &mut Compiler, pn: Pn, emit_options: u16) -> Qstr {
    if comp.pass == Pass::Scope {
        let s = comp.scope_new_and_link(ScopeKind::Function, pn, emit_options);
        comp.scope_slots.insert(pn, s);
    }
    let fscope = *comp
        .scope_slots
        .get(&pn)
        .expect("funcdef_helper: scope slot missing (SCOPE pass did not run?)");
    let params = comp.tree.struct_nodes(pn)[1];
    compile_funcdef_lambdef(comp, fscope, params, grammar::RULE_typedargslist);
    comp.scopes.scopes[fscope].simple_name
}

// Port of compile_classdef_helper (compile.c:806).
pub fn compile_classdef_helper(comp: &mut Compiler, pn: Pn, emit_options: u16) -> Qstr {
    if comp.pass == Pass::Scope {
        let s = comp.scope_new_and_link(ScopeKind::Class, pn, emit_options);
        comp.scope_slots.insert(pn, s);
    }

    comp.emit.load_build_class(&mut comp.scopes);

    let cscope = *comp
        .scope_slots
        .get(&pn)
        .expect("classdef_helper: scope slot missing (SCOPE pass did not run?)");
    close_over_variables_etc(comp, cscope, 0, 0);
    let simple_name = comp.scopes.scopes[cscope].simple_name;
    comp.emit
        .load_const_str(simple_name, &mut comp.scopes);

    // Vendor compile_classdef_helper (compile.c:806-831) routes the base
    // list THROUGH compile_trailer_paren_helper with n_positional_extra=2.
    // That preserves metaclass=X (kwarg), *bases, **kw, and the
    // CALL_FUNCTION_VAR_KW flags. The earlier scaffold hand-rolled a
    // bare callFunction(n, 0, 0) which silently dropped them.
    let mut parents = comp.tree.struct_nodes(pn)[1];
    if comp
        .tree
        .is_struct_kind(parents, grammar::RULE_classdef_2)
    {
        parents = crate::compiler::parser::PN_NULL;
    }
    if pn_is_null(parents) {
        comp.emit.call_function(2, 0, 0, &mut comp.scopes);
    } else {
        compile_trailer_paren_helper(comp, parents, false, 2);
    }

    simple_name
}

// Shared body of compile_scope_func_lambda_param (compile.c:2859).
fn c_scope_func_lambda_param(
    comp: &mut Compiler,
    pn: Pn,
    pn_name: u8,
    pn_star: u8,
    pn_dbl_star: u8,
) {
    let sc = comp.scope_cur.expect("c_scope_func_lambda_param: no scope_cur");

    if (comp.scopes.scopes[sc].scope_flags & SCOPE_FLAG_VARKEYWORDS) != 0 {
        comp.compile_syntax_error(pn, "invalid syntax");
        return;
    }

    let mut param_name: Qstr = 0;
    let mut param_flag: u8 = ID_FLAG_IS_PARAM;

    if pn_is_id(pn) {
        param_name = pn_leaf_arg(pn) as Qstr;
        if comp.have_star {
            comp.scopes.scopes[sc].num_kwonly_args += 1;
        } else {
            comp.scopes.scopes[sc].num_pos_args += 1;
        }
    } else {
        debug_assert!(pn_is_struct(pn));
        let k = comp.tree.struct_kind(pn);
        // PEP 570 `/` marker — no binding, no counter bump.
        if k == grammar::RULE_typedargslist_slash || k == grammar::RULE_varargslist_slash {
            return;
        }
        if k == pn_name {
            param_name = pn_leaf_arg(comp.tree.struct_nodes(pn)[0]) as Qstr;
            if comp.have_star {
                comp.scopes.scopes[sc].num_kwonly_args += 1;
            } else {
                comp.scopes.scopes[sc].num_pos_args += 1;
            }
        } else if k == pn_star {
            if comp.have_star {
                comp.compile_syntax_error(pn, "invalid syntax");
                return;
            }
            comp.have_star = true;
            param_flag = ID_FLAG_IS_PARAM | ID_FLAG_IS_STAR_PARAM;
            let inner = comp.tree.struct_nodes(pn)[0];
            if pn_is_null(inner) {
                // bare star — nothing to bind
            } else if pn_is_id(inner) {
                comp.scopes.scopes[sc].scope_flags |= SCOPE_FLAG_VARARGS;
                param_name = pn_leaf_arg(inner) as Qstr;
            } else {
                debug_assert!(pn_is_struct(inner));
                comp.scopes.scopes[sc].scope_flags |= SCOPE_FLAG_VARARGS;
                let tfp_nodes: Vec<Pn> = comp.tree.struct_nodes(inner).to_vec();
                param_name = pn_leaf_arg(tfp_nodes[0]) as Qstr;
            }
        } else {
            debug_assert_eq!(k, pn_dbl_star);
            param_name = pn_leaf_arg(comp.tree.struct_nodes(pn)[0]) as Qstr;
            param_flag = ID_FLAG_IS_PARAM | ID_FLAG_IS_DBL_STAR_PARAM;
            comp.scopes.scopes[sc].scope_flags |= SCOPE_FLAG_VARKEYWORDS;
        }
    }

    if param_name != 0 {
        let idx = comp
            .scopes
            .find_or_add(sc, param_name, IdInfoKind::Undecided);
        if comp.scopes.scopes[sc].id_info[idx].kind != IdInfoKind::Undecided {
            comp.compile_syntax_error(pn, "argument name reused");
            return;
        }
        comp.scopes.scopes[sc].id_info[idx].kind = IdInfoKind::Local;
        comp.scopes.scopes[sc].id_info[idx].flags = param_flag;
    }
}

pub fn c_scope_func_param(comp: &mut Compiler, pn: Pn) {
    c_scope_func_lambda_param(
        comp,
        pn,
        grammar::RULE_typedargslist_name,
        grammar::RULE_typedargslist_star,
        grammar::RULE_typedargslist_dbl_star,
    );
}

pub fn c_scope_lambda_param(comp: &mut Compiler, pn: Pn) {
    c_scope_func_lambda_param(
        comp,
        pn,
        grammar::RULE_varargslist_name,
        grammar::RULE_varargslist_star,
        grammar::RULE_varargslist_dbl_star,
    );
}
