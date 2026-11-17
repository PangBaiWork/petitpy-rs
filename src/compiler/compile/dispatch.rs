//! §30 — `compile_node`: top-level dispatch.
//!
//! Three tag forms — small-int, leaf (id/string/token), or struct
//! (rule). The struct case looks up the dispatch table and calls the
//! handler.
//!
//! The table is built by a `const fn` with explicit assignments per
//! rule id (Zig uses `@field` reflection over `CompileRules`; Rust
//! has no equivalent, so the mapping is spelled out — the same
//! name→rule pairs, checked by the dispatch-table unit tests).

use crate::compiler::compile::Compiler;
use crate::compiler::emit::consts::{
    MP_TOKEN_ELLIPSIS, MP_TOKEN_KW_FALSE, MP_TOKEN_KW_NONE, MP_TOKEN_KW_TRUE,
};
use crate::compiler::grammar_data as grammar;
use crate::compiler::grammar_ext;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_leaf, pn_is_null, pn_is_small_int, pn_leaf_arg, pn_leaf_kind,
    pn_leaf_small_int, Pn, PN_ID, PN_STRING, PN_TOKEN,
};
use crate::compiler::qstr::Qstr;
use super::{async_stmt, expr, funcdef, handlers};

/// Handler signature shared by every `compile_*` in the dispatch
/// table. (Zig: `CompileFn = *const fn (*Compiler, *PnStruct) void` —
/// the Rust handler takes the struct node's `Pn` and reads its
/// children from `Compiler::tree`.)
pub type Handler = fn(&mut Compiler, Pn);

pub(crate) static DISPATCH_TABLE: [Handler; grammar::NUM_RULES] = build_dispatch_table();

const fn build_dispatch_table() -> [Handler; grammar::NUM_RULES] {
    let mut tbl: [Handler; grammar::NUM_RULES] =
        [handlers::compile_generic_all_nodes; grammar::NUM_RULES];
    tbl[grammar::RULE_file_input as usize] = handlers::compile_file_input;
    tbl[grammar::RULE_single_input as usize] = handlers::compile_single_input;
    tbl[grammar::RULE_eval_input as usize] = handlers::compile_eval_input;

    tbl[grammar::RULE_expr_stmt as usize] = handlers::compile_expr_stmt;
    tbl[grammar::RULE_del_stmt as usize] = handlers::compile_del_stmt;
    tbl[grammar::RULE_pass_stmt as usize] = handlers::compile_pass_stmt;
    tbl[grammar::RULE_break_stmt as usize] = handlers::compile_break_cont_stmt;
    tbl[grammar::RULE_continue_stmt as usize] = handlers::compile_break_cont_stmt;
    tbl[grammar::RULE_return_stmt as usize] = handlers::compile_return_stmt;
    tbl[grammar::RULE_yield_stmt as usize] = handlers::compile_yield_stmt;
    tbl[grammar::RULE_raise_stmt as usize] = handlers::compile_raise_stmt;
    tbl[grammar::RULE_import_name as usize] = handlers::compile_import_name;
    tbl[grammar::RULE_import_from as usize] = handlers::compile_import_from;
    tbl[grammar::RULE_global_stmt as usize] = handlers::compile_global_nonlocal_stmt;
    tbl[grammar::RULE_nonlocal_stmt as usize] = handlers::compile_global_nonlocal_stmt;
    tbl[grammar::RULE_assert_stmt as usize] = handlers::compile_assert_stmt;

    tbl[grammar::RULE_if_stmt as usize] = handlers::compile_if_stmt;
    tbl[grammar::RULE_while_stmt as usize] = handlers::compile_while_stmt;
    tbl[grammar::RULE_for_stmt as usize] = handlers::compile_for_stmt;
    tbl[grammar::RULE_try_stmt as usize] = handlers::compile_try_stmt;
    tbl[grammar::RULE_with_stmt as usize] = handlers::compile_with_stmt;

    tbl[grammar::RULE_suite_block_stmts as usize] = handlers::compile_suite_block_stmts;

    tbl[grammar::RULE_namedexpr_test as usize] = expr::compile_namedexpr;
    tbl[grammar::RULE_test_if_expr as usize] = expr::compile_test_if_expr;
    tbl[grammar::RULE_lambdef as usize] = funcdef::compile_lambdef;

    tbl[grammar::RULE_or_test as usize] = expr::compile_or_test;
    tbl[grammar::RULE_and_test as usize] = expr::compile_and_test;
    tbl[grammar::RULE_not_test_2 as usize] = expr::compile_not_test_2;
    tbl[grammar::RULE_comparison as usize] = expr::compile_comparison;
    tbl[grammar::RULE_star_expr as usize] = expr::compile_star_expr;
    tbl[grammar::RULE_expr as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_xor_expr as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_and_expr as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_shift_expr as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_arith_expr as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_term as usize] = expr::compile_binary_op;
    tbl[grammar::RULE_factor_2 as usize] = expr::compile_factor_2;
    tbl[grammar::RULE_power as usize] = expr::compile_power;

    tbl[grammar::RULE_atom_expr_normal as usize] = expr::compile_atom_expr_normal;
    tbl[grammar::RULE_atom_paren as usize] = expr::compile_atom_paren;
    tbl[grammar::RULE_atom_bracket as usize] = expr::compile_atom_bracket;
    tbl[grammar::RULE_atom_brace as usize] = expr::compile_atom_brace;
    tbl[grammar::RULE_trailer_paren as usize] = expr::compile_trailer_paren;
    tbl[grammar::RULE_trailer_bracket as usize] = expr::compile_trailer_bracket;
    tbl[grammar::RULE_trailer_period as usize] = expr::compile_trailer_period;

    // Slice / subscript handlers — without these, x[a:b] / x[a:b:c] /
    // x[a, b] fall through to compile_generic_all_nodes and miss BUILD_SLICE.
    tbl[grammar::RULE_subscript_2 as usize] = expr::compile_subscript;
    tbl[grammar::RULE_subscript_3 as usize] = expr::compile_subscript;
    tbl[grammar::RULE_subscriptlist as usize] = expr::compile_subscriptlist;

    // Dict key:value pairs — vendor swaps to value-then-key for STORE_MAP.
    tbl[grammar::RULE_dictorsetmaker_item as usize] = expr::compile_dictorsetmaker_item;
    // PEP 448 `{**expr}` — petitpy grammar overlay adds this rule.
    tbl[grammar::RULE_dictorsetmaker_dbl_star as usize] =
        expr::compile_dictorsetmaker_dbl_star;

    tbl[grammar::RULE_testlist as usize] = expr::compile_testlist;
    tbl[grammar::RULE_testlist_comp as usize] = expr::compile_testlist_comp;
    // When `a, b, c` appears on the RHS of an assignment / `return` /
    // any other rvalue context, the parser produces a
    // testlist_star_expr (or exprlist for the `for`-target tuple)
    // struct. Vendor's compile_node dispatches them to a generic
    // tuple-builder so the values get wrapped in a BUILD_TUPLE before
    // downstream consumers (UNPACK_SEQUENCE, iteration, etc.) try to
    // read them.
    //
    // LHS context goes through c_assign, not the dispatch table, so
    // adding testlist_star_expr here doesn't double-wrap the LHS.
    tbl[grammar::RULE_testlist_star_expr as usize] = expr::compile_testlist;
    tbl[grammar::RULE_exprlist as usize] = expr::compile_testlist;

    tbl[grammar::RULE_classdef as usize] = funcdef::compile_classdef;
    tbl[grammar::RULE_yield_expr as usize] = expr::compile_yield_expr;

    tbl[grammar::RULE_const_object as usize] = expr::compile_const_object;

    tbl[grammar::RULE_funcdef as usize] = funcdef::compile_funcdef;
    tbl[grammar::RULE_decorated as usize] = funcdef::compile_decorated;

    // ASYNC_AWAIT — A2/A3 (see §33.1). async_funcdef is reached only
    // via the decorated path (compile_decorated dispatches based on
    // body kind).
    tbl[grammar::RULE_async_funcdef as usize] = async_stmt::compile_async_funcdef;
    tbl[grammar::RULE_async_stmt as usize] = async_stmt::compile_async_stmt;
    tbl[grammar::RULE_atom_expr_await as usize] = async_stmt::compile_atom_expr_await;

    tbl
}

impl Compiler {
    pub fn compile_node(&mut self, pn: Pn) {
        if pn_is_null(pn) {
            return;
        }

        if pn_is_small_int(pn) {
            let v = pn_leaf_small_int(pn);
            self.emit.load_const_small_int(v, &mut self.scopes);
            return;
        }

        if pn_is_leaf(pn) {
            let lk = pn_leaf_kind(pn);
            let arg = pn_leaf_arg(pn);
            if lk == PN_ID {
                self.compile_load_id(arg as Qstr);
                return;
            }
            if lk == PN_STRING {
                self.emit.load_const_str(arg as Qstr, &mut self.scopes);
                return;
            }
            if lk == PN_TOKEN {
                // Vendor compile_node (compile.c:2805) for
                // MP_PARSE_NODE_TOKEN: NEWLINE is silently ignored
                // (file_input or single_input may let through a
                // NEWLINE if the file starts with a blank line or the
                // REPL sees an empty line). Everything else routes to
                // load_const_tok with the raw token id, which is
                // exactly the set our emit.load_const_tok understands.
                let tk = TokenKind::from_u8(arg as u8);
                match tk {
                    Some(TokenKind::NEWLINE) => {}
                    Some(TokenKind::KW_FALSE) => {
                        self.emit.load_const_tok(MP_TOKEN_KW_FALSE, &mut self.scopes)
                    }
                    Some(TokenKind::KW_NONE) => {
                        self.emit.load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes)
                    }
                    Some(TokenKind::KW_TRUE) => {
                        self.emit.load_const_tok(MP_TOKEN_KW_TRUE, &mut self.scopes)
                    }
                    Some(TokenKind::ELLIPSIS) => {
                        self.emit
                            .load_const_tok(MP_TOKEN_ELLIPSIS, &mut self.scopes)
                    }
                    _ => self.compile_syntax_error(pn, "unexpected token"),
                }
                return;
            }
            return;
        }

        // Struct.
        let k = self.tree.struct_kind(pn);
        if (k as usize) >= grammar::NUM_RULES {
            // An extension rule (grammar_ext). The dispatch table is
            // sized to the vendor grammar, so these must be handled
            // before it is indexed. The rules under match_stmt are
            // consumed by compile_match_stmt itself and never reach here.
            match k {
                grammar_ext::RULE_match_stmt => {
                    super::match_stmt::compile_match_stmt(self, pn)
                }
                _ => self.compile_syntax_error(pn, "unsupported extension rule"),
            }
            return;
        }
        let handler = DISPATCH_TABLE[k as usize];
        handler(self, pn);
    }
}
