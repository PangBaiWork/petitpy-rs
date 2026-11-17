//! The match statement — §34 of `compile.zig` (extension grammar,
//! see `grammar_ext`).
//!
//! There are no pattern-matching opcodes in MicroPython, so a match
//! lowers to the if/elif chain it is equivalent to. Reference
//! implementation, already proven differentially against the VM:
//! projects/pyfront/src/pyfront/patterns.py.
//!
//!     match subj:              <subj>; STORE .match
//!         case P1: body1       <test P1> -> next1; <bind P1>; body1; JUMP end
//!         case P2: body2   next1: <test P2> -> next2; ...
//!                            end:
//!
//! Patterns arrive as expression trees (grammar_ext explains why),
//! so this reads them back as patterns:
//!
//!     NAME              capture, or wildcard when it is `_`
//!     None/True/False   singleton — compared with `is`, per PEP 634
//!     match_stmt_alts   or-pattern: `a | b`, not bitwise-or
//!     match_stmt_pattern  as-pattern: inner pattern plus a name
//!     atom_bracket/paren  sequence — isinstance + len + per element
//!     atom_expr_normal    class — isinstance + per keyword attribute
//!     anything else     value pattern, compared with `==`
//!
//! Scope matches pyfront's deliberately, so the capability matrix reads
//! one gap rather than two: no star patterns, no positional class
//! sub-patterns, no mapping patterns. Each is refused by name.
//!
//! Tests run before any binding, so a failed case leaves no names bound.

use super::expr::c_if_cond;
use super::Compiler;
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::grammar_ext;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_id, pn_is_null, pn_is_token_kind, pn_leaf_arg, Pn,
};
use crate::compiler::qstr::Qstr;

/// Hidden local holding the subject while the chain tests it. The
/// leading dot keeps it out of reach of user code, as vendor does for
/// comprehension arguments (`.0`).
const MATCH_SUBJECT_NAME: &[u8] = b".match";

/// Where a sub-pattern's value lives, as a path from the subject temp.
/// Re-emitting the path costs a few loads and saves a temp per level.
/// The list is threaded down the stack, so nesting depth is bounded
/// by the parser's, not by a fixed cap here.
struct Subject<'a> {
    parent: Option<&'a Subject<'a>>,
    access: SubjectAccess,
}

enum SubjectAccess {
    Index(isize),
    Attr(Qstr),
    Key(Pn),
}

fn emit_subject(comp: &mut Compiler, subj: Option<&Subject<'_>>) {
    let s = match subj {
        None => {
            let q = comp.qstr_from_strn(MATCH_SUBJECT_NAME);
            comp.compile_load_id(q);
            return;
        }
        Some(s) => s,
    };
    emit_subject(comp, s.parent);
    match &s.access {
        SubjectAccess::Index(i) => {
            comp.emit
                .load_const_small_int(*i, &mut comp.scopes);
            comp.emit.subscr(MP_EMIT_SUBSCR_LOAD, &mut comp.scopes);
        }
        SubjectAccess::Attr(q) => {
            comp.emit.attr(*q, MP_EMIT_ATTR_LOAD, &mut comp.scopes);
        }
        SubjectAccess::Key(k) => {
            comp.compile_node(*k);
            comp.emit.subscr(MP_EMIT_SUBSCR_LOAD, &mut comp.scopes);
        }
    }
}

/// A sequence pattern's elements. `[a]` collapses to a bare node and
/// `[]` to PN_NULL, which `extract_list` already folds together.
fn match_seq_elems(comp: &Compiler, pn: Pn) -> Vec<Pn> {
    let first = comp.tree.struct_nodes(pn)[0];
    comp.tree.extract_list(first, grammar::RULE_testlist_comp)
}

/// Free-function adapter so `compile_node` can be passed as the
/// `emit_cls` function pointer to `emit_is_instance` (class patterns).
fn compile_node_fn(comp: &mut Compiler, pn: Pn) {
    comp.compile_node(pn);
}

/// `isinstance(<subject>, <cls emitted by `emit_cls`>)`, leaving a bool.
fn emit_is_instance(
    comp: &mut Compiler,
    subj: Option<&Subject<'_>>,
    emit_cls: fn(&mut Compiler, Pn),
    cls: Pn,
) {
    let q = comp.qstr_from_strn(b"isinstance");
    comp.compile_load_id(q);
    emit_subject(comp, subj);
    emit_cls(comp, cls);
    comp.emit.call_function(2, 0, 0, &mut comp.scopes);
}

fn emit_seq_types(comp: &mut Compiler, _pn: Pn) {
    let list_q = comp.qstr_from_strn(b"list");
    comp.compile_load_id(list_q);
    let tuple_q = comp.qstr_from_strn(b"tuple");
    comp.compile_load_id(tuple_q);
    comp.emit
        .build(2, MP_EMIT_BUILD_TUPLE, &mut comp.scopes);
}

/// Index of the `*rest` element, null when there is none. A second star
/// is refused by name; the caller sees the error through `compile_error`.
/// Returns `None` on error (already reported), else `Some(star)`.
fn match_star_at(comp: &mut Compiler, elems: &[Pn]) -> Option<Option<usize>> {
    let mut found: Option<usize> = None;
    for (i, e) in elems.iter().enumerate() {
        if !comp
            .tree
            .is_struct_kind(*e, grammar::RULE_star_expr)
        {
            continue;
        }
        if found.is_some() {
            comp.compile_syntax_error(*e, "two stars in a sequence pattern");
            return None;
        }
        found = Some(i);
    }
    Some(found)
}

/// Where element `i` sits in the subject. Elements after a star count
/// from the end, so their position does not depend on how long the
/// subject turned out to be.
fn match_seq_index(i: usize, star: Option<usize>, n: usize) -> isize {
    let at = match star {
        None => return i as isize,
        Some(at) => at,
    };
    if i < at {
        return i as isize;
    }
    i as isize - n as isize
}

/// `*rest` binds the slice between the fixed head and tail — always a
/// list, per PEP 634, which `subj[1:]` on a tuple is not.
fn emit_star_bind(
    comp: &mut Compiler,
    star_pn: Pn,
    subj: Option<&Subject<'_>>,
    at: usize,
    n_after: usize,
) {
    let name = comp.tree.struct_nodes(star_pn)[0];
    if !pn_is_id(name) {
        comp.compile_syntax_error(star_pn, "star pattern must name a plain target");
        return;
    }
    let q: Qstr = pn_leaf_arg(name) as Qstr;
    if q == comp.qstr_from_strn(b"_") {
        return; // `*_` matches without binding
    }

    let list_q = comp.qstr_from_strn(b"list");
    comp.compile_load_id(list_q);
    emit_subject(comp, subj);
    comp.emit
        .load_const_small_int(at as isize, &mut comp.scopes);
    if n_after == 0 {
        comp.emit
            .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    } else {
        comp.emit
            .load_const_small_int(-(n_after as isize), &mut comp.scopes);
    }
    comp.emit
        .build(2, MP_EMIT_BUILD_SLICE, &mut comp.scopes);
    comp.emit.subscr(MP_EMIT_SUBSCR_LOAD, &mut comp.scopes);
    comp.emit.call_function(1, 0, 0, &mut comp.scopes);
    comp.compile_store_id(q);
}

fn emit_dict_type(comp: &mut Compiler, _pn: Pn) {
    let q = comp.qstr_from_strn(b"dict");
    comp.compile_load_id(q);
}

/// What `emit_mapping` is doing on this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapPass {
    TestOnly,
    BindOnly,
}

/// Walk a `{...}` pattern, once per pass. The grammar shapes a dict
/// display as a first entry plus a comma-tail, and a *set* as the same
/// shape with bare entries instead of `dictorsetmaker_item` — which is
/// how a set literal is told apart and refused.
fn emit_mapping(
    comp: &mut Compiler,
    pat: Pn,
    subj: Option<&Subject<'_>>,
    lab_fail: u16,
    pass: MapPass,
) {
    let brace = comp.tree.struct_nodes(pat)[0];

    if pass == MapPass::TestOnly {
        emit_is_instance(comp, subj, emit_dict_type, pat);
        comp.emit
            .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);
    }

    // `case {}:` names no keys, so the isinstance test is the whole of
    // it: per PEP 634 an empty mapping pattern matches *any* mapping,
    // which is the opposite of what `{}` means as an expression.
    if pn_is_null(brace) {
        return;
    }

    // `{k: v}` collapses to the bare entry; more than one wraps it.
    let mut first = brace;
    let mut rest: Vec<Pn> = Vec::new();
    if comp
        .tree
        .is_struct_kind(brace, grammar::RULE_dictorsetmaker)
    {
        let n: Vec<Pn> = comp.tree.struct_nodes(brace).to_vec();
        first = n[0];
        let tail = n[1];
        if !pn_is_null(tail)
            && comp
                .tree
                .is_struct_kind(tail, grammar::RULE_dictorsetmaker_list)
        {
            let scratch = comp.tree.struct_nodes(tail)[0];
            if !pn_is_null(scratch) {
                rest = comp
                    .tree
                    .extract_list(scratch, grammar::RULE_dictorsetmaker_list2);
            }
        }
    }

    if !emit_map_entry(comp, first, subj, lab_fail, pass) {
        return;
    }
    for entry in &rest {
        if !emit_map_entry(comp, *entry, subj, lab_fail, pass) {
            return;
        }
    }
}

/// One `key: pattern` entry. Returns false once an error is raised.
fn emit_map_entry(
    comp: &mut Compiler,
    entry: Pn,
    subj: Option<&Subject<'_>>,
    lab_fail: u16,
    pass: MapPass,
) -> bool {
    if !comp
        .tree
        .is_struct_kind(entry, grammar::RULE_dictorsetmaker_item)
    {
        // A bare entry is a set element, or `**rest`.
        comp.compile_syntax_error(entry, "set or ** in a mapping pattern");
        return false;
    }
    let kv: Vec<Pn> = comp.tree.struct_nodes(entry).to_vec();
    let sub = Subject {
        parent: subj,
        access: SubjectAccess::Key(kv[0]),
    };
    if pass == MapPass::TestOnly {
        // The key must be present before its value can be tested; a
        // missing key is a failed match, not a KeyError.
        comp.compile_node(kv[0]);
        emit_subject(comp, subj);
        comp.emit.binary_op(MP_BINARY_OP_IN, &mut comp.scopes);
        comp.emit
            .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);
        emit_pattern_test(comp, kv[1], Some(&sub), lab_fail);
    } else {
        emit_pattern_bind(comp, kv[1], Some(&sub));
    }
    true
}

/// How `pat` looks as a class pattern. `.dotted` has to be told apart
/// from `.none`: left to fall through to the value pattern, `case
/// mod.P(x=1)` would *call* mod.P and compare the result, which looks
/// like working code.
enum ClassPat {
    None,
    Dotted,
    Simple(Pn),
}

fn match_class_pat(comp: &Compiler, pat: Pn) -> ClassPat {
    if !comp
        .tree
        .is_struct_kind(pat, grammar::RULE_atom_expr_normal)
    {
        return ClassPat::None;
    }
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pat).to_vec();
    let last = nodes[nodes.len() - 1];
    if comp
        .tree
        .is_struct_kind(last, grammar::RULE_trailer_paren)
    {
        return if nodes.len() == 2 {
            ClassPat::Simple(last)
        } else {
            ClassPat::Dotted
        };
    }
    // More than one trailer groups them under atom_expr_trailers, which
    // is the shape `mod.P(...)` arrives in.
    if comp
        .tree
        .is_struct_kind(last, grammar::RULE_atom_expr_trailers)
    {
        let ts: Vec<Pn> = comp.tree.struct_nodes(last).to_vec();
        let lt = ts[ts.len() - 1];
        if comp
            .tree
            .is_struct_kind(lt, grammar::RULE_trailer_paren)
        {
            return ClassPat::Dotted;
        }
    }
    ClassPat::None
}

/// The keyword sub-patterns of a class pattern, as (attribute, pattern).
/// A positional sub-pattern collapses to a bare node rather than an
/// `argument` struct, which is how it is told apart and refused.
/// Returns `None` on error (already reported).
fn match_class_args(comp: &mut Compiler, trailer: Pn) -> Option<Vec<Pn>> {
    let first = comp.tree.struct_nodes(trailer)[0];
    let list = comp.tree.extract_list(first, grammar::RULE_arglist);
    for a in &list {
        let ok = comp.tree.is_struct_kind(*a, grammar::RULE_argument)
            && pn_is_id(comp.tree.struct_nodes(*a)[0]);
        if !ok {
            comp.compile_syntax_error(*a, "positional sub-pattern in a class pattern");
            return None;
        }
    }
    Some(list)
}

fn match_class_arg_subject<'a>(
    comp: &Compiler,
    arg: Pn,
    subj: Option<&'a Subject<'a>>,
) -> Subject<'a> {
    let _ = comp;
    Subject {
        parent: subj,
        access: SubjectAccess::Attr(
            pn_leaf_arg(comp.tree.struct_nodes(arg)[0]) as Qstr,
        ),
    }
}

/// Is this pattern node a bare NAME (capture or wildcard)? A dotted name
/// is a value pattern, which is why only a leaf counts.
fn match_is_capture(pn: Pn) -> bool {
    pn_is_id(pn)
}

fn match_is_wildcard(comp: &mut Compiler, pn: Pn) -> bool {
    match_is_capture(pn) && pn_leaf_arg(pn) as Qstr == comp.qstr_from_strn(b"_")
}

/// Emit the test for `pat` against `subj`, jumping to `lab_fail` when it
/// does not match. Falls through when it does.
fn emit_pattern_test(
    comp: &mut Compiler,
    pat: Pn,
    subj: Option<&Subject<'_>>,
    lab_fail: u16,
) {
    // A capture or wildcard always matches; nothing to test.
    if match_is_capture(pat) {
        return;
    }

    // `a | b` — alternatives. Each that matches jumps past the rest.
    if comp
        .tree
        .is_struct_kind(pat, grammar_ext::RULE_match_stmt_alts)
    {
        let alts: Vec<Pn> = comp.tree.struct_nodes(pat).to_vec();
        let n = alts.len();
        let lab_hit = comp.comp_next_label();
        let mut i: usize = 0;
        while i < n {
            if i + 1 == n {
                // Last alternative: its failure is the whole pattern's.
                emit_pattern_test(comp, alts[i], subj, lab_fail);
            } else {
                let lab_try_next = comp.comp_next_label();
                emit_pattern_test(comp, alts[i], subj, lab_try_next);
                comp.emit.jump(lab_hit as usize, &mut comp.scopes);
                comp.emit.label_assign(lab_try_next as usize);
            }
            i += 1;
        }
        comp.emit.label_assign(lab_hit as usize);
        return;
    }

    // `P() as w` — test the inner pattern; the name binds later.
    if comp
        .tree
        .is_struct_kind(pat, grammar_ext::RULE_match_stmt_pattern)
    {
        let inner = comp.tree.struct_nodes(pat)[0];
        emit_pattern_test(comp, inner, subj, lab_fail);
        return;
    }

    // `[a, b]` / `(a, b)` — a sequence of known length.
    if comp.tree.is_struct_kind(pat, grammar::RULE_atom_bracket)
        || comp.tree.is_struct_kind(pat, grammar::RULE_atom_paren)
    {
        let elems = match_seq_elems(comp, pat);
        let star = match match_star_at(comp, &elems) {
            Some(s) => s,
            None => return,
        };

        // A str is a sequence but must not match one, per PEP 634, so
        // the test names list and tuple rather than asking for __len__.
        emit_is_instance(comp, subj, emit_seq_types, pat);
        comp.emit
            .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);

        // With a star the fixed elements are a minimum, not the length.
        let n_fixed = elems.len() - usize::from(star.is_some());
        let len_q = comp.qstr_from_strn(b"len");
        comp.compile_load_id(len_q);
        emit_subject(comp, subj);
        comp.emit.call_function(1, 0, 0, &mut comp.scopes);
        comp.emit
            .load_const_small_int(n_fixed as isize, &mut comp.scopes);
        comp.emit.binary_op(
            if star.is_some() {
                MP_BINARY_OP_MORE_EQUAL
            } else {
                MP_BINARY_OP_EQUAL
            },
            &mut comp.scopes,
        );
        comp.emit
            .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);

        let mut i: usize = 0;
        while i < elems.len() {
            if star == Some(i) {
                i += 1;
                continue; // a star always matches
            }
            let sub = Subject {
                parent: subj,
                access: SubjectAccess::Index(match_seq_index(i, star, elems.len())),
            };
            emit_pattern_test(comp, elems[i], Some(&sub), lab_fail);
            i += 1;
        }
        return;
    }

    // `{...}` — a mapping pattern. It matches on the keys it names and
    // ignores every other one, which is why it cannot be left to compile
    // as the dict equality test the same syntax means in an expression.
    if comp.tree.is_struct_kind(pat, grammar::RULE_atom_brace) {
        emit_mapping(comp, pat, subj, lab_fail, MapPass::TestOnly);
        return;
    }

    // `P(x=1)` — a class pattern.
    match match_class_pat(comp, pat) {
        ClassPat::Dotted => {
            comp.compile_syntax_error(pat, "dotted name in a class pattern");
            return;
        }
        ClassPat::Simple(trailer) => {
            let args = match match_class_args(comp, trailer) {
                Some(a) => a,
                None => return,
            };
            let cls = comp.tree.struct_nodes(pat)[0];
            emit_is_instance(comp, subj, compile_node_fn, cls);
            comp.emit
                .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);
            for arg in &args {
                let sub = match_class_arg_subject(comp, *arg, subj);
                let pat_arg = comp.tree.struct_nodes(*arg)[1];
                emit_pattern_test(comp, pat_arg, Some(&sub), lab_fail);
            }
            return;
        }
        ClassPat::None => {}
    }

    // Literal, singleton or value pattern. PEP 634 compares None, True
    // and False by identity and everything else by equality.
    emit_subject(comp, subj);
    comp.compile_node(pat);
    let singleton = pn_is_token_kind(pat, TokenKind::KW_NONE as u16)
        || pn_is_token_kind(pat, TokenKind::KW_TRUE as u16)
        || pn_is_token_kind(pat, TokenKind::KW_FALSE as u16);
    comp.emit.binary_op(
        if singleton {
            MP_BINARY_OP_IS
        } else {
            MP_BINARY_OP_EQUAL
        },
        &mut comp.scopes,
    );
    comp.emit
        .pop_jump_if(false, lab_fail as usize, &mut comp.scopes);
}

/// Emit the bindings `pat` makes, once its test has passed.
fn emit_pattern_bind(comp: &mut Compiler, pat: Pn, subj: Option<&Subject<'_>>) {
    if match_is_capture(pat) {
        if match_is_wildcard(comp, pat) {
            return; // `_` binds nothing
        }
        emit_subject(comp, subj);
        comp.compile_store_id(pn_leaf_arg(pat) as Qstr);
        return;
    }

    // `P() as w` — the inner pattern's bindings, then the name.
    if comp
        .tree
        .is_struct_kind(pat, grammar_ext::RULE_match_stmt_pattern)
    {
        let n: Vec<Pn> = comp.tree.struct_nodes(pat).to_vec();
        emit_pattern_bind(comp, n[0], subj);
        emit_subject(comp, subj);
        let as_name = comp.tree.struct_nodes(n[1])[0];
        comp.compile_store_id(pn_leaf_arg(as_name) as Qstr);
        return;
    }

    if comp.tree.is_struct_kind(pat, grammar::RULE_atom_bracket)
        || comp.tree.is_struct_kind(pat, grammar::RULE_atom_paren)
    {
        let elems = match_seq_elems(comp, pat);
        let star = match match_star_at(comp, &elems) {
            Some(s) => s,
            None => return,
        };
        let mut i: usize = 0;
        while i < elems.len() {
            if star == Some(i) {
                emit_star_bind(comp, elems[i], subj, i, elems.len() - i - 1);
                i += 1;
                continue;
            }
            let at = match_seq_index(i, star, elems.len());
            let sub = Subject {
                parent: subj,
                access: SubjectAccess::Index(at),
            };
            emit_pattern_bind(comp, elems[i], Some(&sub));
            i += 1;
        }
        return;
    }

    if comp.tree.is_struct_kind(pat, grammar::RULE_atom_brace) {
        emit_mapping(comp, pat, subj, 0, MapPass::BindOnly);
        return;
    }

    match match_class_pat(comp, pat) {
        ClassPat::Simple(trailer) => {
            let args = match match_class_args(comp, trailer) {
                Some(a) => a,
                None => return,
            };
            for arg in &args {
                let sub = match_class_arg_subject(comp, *arg, subj);
                let pat_arg = comp.tree.struct_nodes(*arg)[1];
                emit_pattern_bind(comp, pat_arg, Some(&sub));
            }
            return;
        }
        // A dotted or non-class pattern binds nothing; the test arm has
        // already refused the dotted one.
        _ => {}
    }

    if comp
        .tree
        .is_struct_kind(pat, grammar_ext::RULE_match_stmt_alts)
    {
        // Alternatives would have to bind the same names on every arm
        // for the body to be able to use them; only the no-binding case
        // is allowed rather than emitting something half-defined.
        let alts: Vec<Pn> = comp.tree.struct_nodes(pat).to_vec();
        for alt in &alts {
            if match_is_capture(*alt) && !match_is_wildcard(comp, *alt) {
                comp.compile_syntax_error(pat, "capture in an alternative pattern");
                return;
            }
        }
    }
}

pub fn compile_match_stmt(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    // nodes[0] is the `match` NAME; the parser's soft-keyword hook has
    // already established what it says.
    comp.compile_node(nodes[1]);
    let subj_q = comp.qstr_from_strn(MATCH_SUBJECT_NAME);
    comp.compile_store_id(subj_q);

    let lab_end = comp.comp_next_label();

    let case_list = nodes[2];
    let cases = comp
        .tree
        .extract_list(case_list, grammar_ext::RULE_match_stmt_case_list);

    let mut i: usize = 0;
    while i < cases.len() {
        let cn: Vec<Pn> = comp.tree.struct_nodes(cases[i]).to_vec();

        // The grammar matches a bare NAME here because `case` is a soft
        // keyword with no token of its own; this is where it is checked.
        if !pn_is_id(cn[0])
            || pn_leaf_arg(cn[0]) as Qstr != comp.qstr_from_strn(b"case")
        {
            comp.compile_syntax_error(cases[i], "expected 'case'");
            return;
        }

        let lab_next = comp.comp_next_label();
        let pat = cn[1];

        emit_pattern_test(comp, pat, None, lab_next);
        // PEP 634 binds before the guard runs, so `case n if n > 5` can
        // see `n` — and the binding stands even when the guard fails.
        emit_pattern_bind(comp, pat, None);
        if !pn_is_null(cn[2]) {
            // guard: match_stmt_guard wraps the condition expression.
            let guard = comp.tree.struct_nodes(cn[2])[0];
            c_if_cond(comp, guard, false, lab_next);
        }
        comp.compile_node(cn[3]);

        if i + 1 < cases.len() {
            comp.emit.jump(lab_end as usize, &mut comp.scopes);
        }
        comp.emit.label_assign(lab_next as usize);
        i += 1;
    }

    comp.emit.label_assign(lab_end as usize);
}
