//! Import family — §13 of `compile.zig`.
//!
//! `import a.b as c`, `from a import b`, dotted names. Split out
//! of `handlers.rs` (file-size rule); re-exported from there.

use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_id, pn_is_leaf, pn_is_null, pn_is_token_kind,
    pn_leaf_arg, pn_leaf_kind, Pn,
    PN_TOKEN,
};
use crate::compiler::qstr::Qstr;

// ─────────────────────────────────────────────────────────────────────────
// §13 — Import family.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_import_name(comp: &mut Compiler, pn: Pn) {
    // Vendor py/compile.c:1159 — apply_to_single_or_list over
    // PN_dotted_as_names dispatching to compile_dotted_as_name for each
    // entry.
    let first = comp.tree.struct_nodes(pn)[0];
    comp.apply_to_single_or_list(first, grammar::RULE_dotted_as_names, compile_dotted_as_name);
}

/// True if `pn` is a TOKEN leaf (of any token kind). Zig `pnIsToken`.
fn pn_is_token(pn: Pn) -> bool {
    pn_is_leaf(pn) && pn_leaf_kind(pn) == PN_TOKEN
}

/// Is this dotted-name node exactly the single identifier `name`?
fn matches_name(comp: &mut Compiler, pn: Pn, name: &[u8]) -> bool {
    pn_is_id(pn) && pn_leaf_arg(pn) as Qstr == comp.qstr_from_strn(name)
}

/// Vendor `doImportName` (compile.zig:2977). Resolves a (possibly
/// `as`-aliased) dotted import name into one IMPORT_NAME of the full
/// dotted string, plus ATTR_LOAD steps for the tail when aliased.
/// `q_base` receives the name the caller should store the result under:
/// the `as` name when present, else the first dotted component (or the
/// empty string for a bare relative `from . import …`).
fn do_import_name(comp: &mut Compiler, pn_in: Pn, q_base: &mut Qstr) {
    let mut pn = pn_in;
    let mut is_as = false;
    if comp
        .tree
        .is_struct_kind(pn, grammar::RULE_dotted_as_name)
    {
        let pns: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
        *q_base = pn_leaf_arg(pns[1]) as Qstr;
        pn = pns[0];
        is_as = true;
    }
    if pn_is_null(pn) {
        match comp.qctx.from_strn(b"") {
            Ok(q_empty) => {
                *q_base = q_empty;
                comp.emit
                    .import(q_empty, MP_EMIT_IMPORT_NAME, &mut comp.scopes);
            }
            Err(_) => comp.emit.overflow = true,
        }
        return;
    }
    if pn_is_id(pn) {
        let q_full = pn_leaf_arg(pn) as Qstr;
        if !is_as {
            *q_base = q_full;
        }
        comp.emit
            .import(q_full, MP_EMIT_IMPORT_NAME, &mut comp.scopes);
        return;
    }
    debug_assert!(comp
        .tree
        .is_struct_kind(pn, grammar::RULE_dotted_name));
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let n = nodes.len();
    if !is_as {
        *q_base = pn_leaf_arg(nodes[0]) as Qstr;
    }
    debug_assert!(n > 0);
    // Build the full dotted name "a.b.c" and intern it.
    let mut buf: Vec<u8> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            buf.push(b'.');
        }
        let q = pn_leaf_arg(*node) as Qstr;
        buf.extend_from_slice(comp.qctx.qstr_str(q));
    }
    match comp.qctx.from_strn(&buf) {
        Ok(q_full) => {
            comp.emit
                .import(q_full, MP_EMIT_IMPORT_NAME, &mut comp.scopes);
        }
        Err(_) => {
            comp.emit.overflow = true;
            return;
        }
    }
    if is_as {
        for node in &nodes[1..] {
            comp.emit.attr(
                pn_leaf_arg(*node) as Qstr,
                MP_EMIT_ATTR_LOAD,
                &mut comp.scopes,
            );
        }
    }
}

/// Vendor `compile_dotted_as_name` (compile.c:1148-1155).
pub fn compile_dotted_as_name(comp: &mut Compiler, pn: Pn) {
    comp.emit.load_const_small_int(0, &mut comp.scopes);
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    let mut q_base: Qstr = 0;
    do_import_name(comp, pn, &mut q_base);
    comp.compile_store_id(q_base);
}

pub fn compile_import_from(comp: &mut Compiler, pn: Pn) {
    // Vendor py/compile.c:1163-1249 — `from <module> import ...`.
    let mut pn_import_source = comp.tree.struct_nodes(pn)[0];

    // Walk the optional leading dots / ellipses to compute the relative
    // import level.
    let mut import_level: isize = 0;
    loop {
        let pn_rel: Pn;
        if pn_is_token(pn_import_source)
            || comp.tree.is_struct_kind(
                pn_import_source,
                grammar::RULE_one_or_more_period_or_ellipsis,
            )
        {
            // A bare token leaf (`.` / `...`) or the wrapping struct
            // both mean "relative import" — but a bare token is NOT a
            // struct, so extract_list on it returns the token itself
            // as the single element; the struct case extracts its
            // children. The two shapes are unified below by
            // extract_list.
            pn_rel = pn_import_source;
            pn_import_source = crate::compiler::parser::PN_NULL;
        } else if comp
            .tree
            .is_struct_kind(pn_import_source, grammar::RULE_import_from_2b)
        {
            let pns_2b: Vec<Pn> = comp.tree.struct_nodes(pn_import_source).to_vec();
            pn_rel = pns_2b[0];
            pn_import_source = pns_2b[1];
            debug_assert!(!pn_is_null(pn_import_source));
        } else {
            break;
        }

        let list = comp
            .tree
            .extract_list(pn_rel, grammar::RULE_one_or_more_period_or_ellipsis);
        for item in list {
            if pn_is_token_kind(item, TokenKind::DEL_PERIOD as u16) {
                import_level += 1;
            } else {
                import_level += 3;
            }
        }
        // Loop continues — drop into the next iteration on
        // import_from_2b chains; the `else { break; }` above exits
        // when no further wrapper is present.
    }

    // `from __future__ import ...` is a compiler directive, not an
    // import — CPython's compiler consumes it and emits nothing. Every
    // feature it can name is already the default here, so the whole
    // statement is a no-op.
    //
    // Vendor MicroPython has no such case and fails with `ImportError:
    // no module named '__future__'`, so this is a deliberate divergence
    // toward CPython, in the same family as the CPython-compatible
    // `collections.deque`.
    if import_level == 0 && matches_name(comp, pn_import_source, b"__future__") {
        return;
    }

    let from_list = comp.tree.struct_nodes(pn)[1];
    if pn_is_token_kind(from_list, TokenKind::OP_STAR as u16) {
        comp.emit
            .load_const_small_int(import_level, &mut comp.scopes);
        match comp.qctx.from_strn(b"*") {
            Ok(q_star) => {
                comp.emit.load_const_str(q_star, &mut comp.scopes);
            }
            Err(_) => {
                comp.emit.overflow = true;
                return;
            }
        }
        comp.emit.build(1, MP_EMIT_BUILD_TUPLE, &mut comp.scopes);
        let mut dummy_q: Qstr = 0;
        do_import_name(comp, pn_import_source, &mut dummy_q);
        comp.emit.import(0, MP_EMIT_IMPORT_STAR, &mut comp.scopes);
        return;
    }

    comp.emit
        .load_const_small_int(import_level, &mut comp.scopes);
    let pn_from = comp.tree.struct_nodes(pn)[1];
    let names = comp.tree.extract_list(pn_from, grammar::RULE_import_as_names);
    for name_pn in &names {
        debug_assert!(comp
            .tree
            .is_struct_kind(*name_pn, grammar::RULE_import_as_name));
        let pns3: Vec<Pn> = comp.tree.struct_nodes(*name_pn).to_vec();
        let id2 = pn_leaf_arg(pns3[0]) as Qstr;
        comp.emit.load_const_str(id2, &mut comp.scopes);
    }
    comp.emit
        .build(names.len(), MP_EMIT_BUILD_TUPLE, &mut comp.scopes);

    let mut dummy_q: Qstr = 0;
    do_import_name(comp, pn_import_source, &mut dummy_q);

    for name_pn in &names {
        debug_assert!(comp
            .tree
            .is_struct_kind(*name_pn, grammar::RULE_import_as_name));
        let pns3: Vec<Pn> = comp.tree.struct_nodes(*name_pn).to_vec();
        let id2 = pn_leaf_arg(pns3[0]) as Qstr;
        comp.emit
            .import(id2, MP_EMIT_IMPORT_FROM, &mut comp.scopes);
        let as_node = pns3[1];
        if pn_is_null(as_node) {
            comp.compile_store_id(id2);
        } else {
            comp.compile_store_id(pn_leaf_arg(as_node) as Qstr);
        }
    }
    comp.emit.pop_top(&mut comp.scopes);
}

// ─────────────────────────────────────────────────────────────────────────
