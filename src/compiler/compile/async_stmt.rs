//! Async statements — §33.1 of `compile.zig`.
//!
//! `async for` / `async with` lower to the try/except/finally shapes
//! vendor uses (compile.c:1790-1977); `await` is `compile_atom_expr_normal`
//! followed by `yield from`. `async def` is a regular funcdef whose
//! scope gets the GENERATOR flag.

use super::assign::{c_assign, AssignKind};
use super::funcdef::compile_funcdef;
use super::{wk_idx, Compiler};
use crate::compiler::emit::consts::*;
use crate::compiler::grammar_data as grammar;
use crate::compiler::parser::{pn_is_null, pn_is_struct, Pn};
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{ScopeKind, SCOPE_FLAG_GENERATOR};

fn compile_yield_from(comp: &mut Compiler) {
    comp.emit.get_iter(false, &mut comp.scopes);
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    comp.emit
        .yield_op(MP_EMIT_YIELD_FROM, &mut comp.scopes);
}

// Vendor compile_await_object_method (compile.c:1790): LOAD_METHOD m,
// CALL_METHOD 0 args, YIELD_FROM. Used by async_for (m=__anext__) and
// async_with (m=__aenter__/__aexit__).
fn compile_await_object_method(comp: &mut Compiler, method: Qstr) {
    comp.emit
        .load_method(method, false, &mut comp.scopes);
    comp.emit.call_method(0, 0, 0, &mut comp.scopes);
    compile_yield_from(comp);
}

// Vendor compile_async_for_stmt (compile.c:1796): async-for is sugar for
// `try: while True: yield from iter.__anext__(); body except
// StopAsyncIteration: pop; goto else`. The bytecode mirrors a try-except
// inside a while loop without START_BREAK_CONTINUE_BLOCK's optimised
// for-loop fast-path.
pub fn compile_async_for_stmt(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();

    let lab_while_else = comp.comp_next_label();
    let lab_try_exception = comp.comp_next_label();
    let lab_try_else = comp.comp_next_label();
    let lab_try_finally = comp.comp_next_label();

    // Stack: (...)
    // Compile the iterator expression and call its __aiter__ method.
    comp.compile_node(nodes[1]); // iterator
    let aiter_q = comp.wk(wk_idx::AITER);
    comp.emit
        .load_method(aiter_q, false, &mut comp.scopes);
    comp.emit.call_method(0, 0, 0, &mut comp.scopes);
    // Stack: (..., iterable)

    // START_BREAK_CONTINUE_BLOCK
    let old_break = comp.break_label;
    let old_continue = comp.continue_label;
    let old_excl = comp.break_continue_except_level;
    let lab_break = comp.comp_next_label();
    let lab_continue = comp.comp_next_label();
    comp.break_label = lab_break as usize;
    comp.continue_label = lab_continue as usize;
    comp.break_continue_except_level = comp.cur_except_level;

    comp.emit.label_assign(lab_continue as usize);

    comp.compile_increase_except_level(lab_try_exception, MP_EMIT_SETUP_BLOCK_EXCEPT);

    comp.emit.dup_top(&mut comp.scopes);
    // Stack: (..., iterable, iterable)

    // yield from iterable.__anext__()
    let anext_q = comp.wk(wk_idx::ANEXT);
    compile_await_object_method(comp, anext_q);
    // Stack: (..., iterable, yielded_value)

    c_assign(comp, nodes[0], AssignKind::Store); // loop variable
    // Stack: (..., iterable)
    comp.emit
        .pop_except_jump(lab_try_else as usize, &mut comp.scopes);

    comp.emit.label_assign(lab_try_exception as usize);
    comp.emit.start_except_handler(&mut comp.scopes);
    comp.emit.dup_top(&mut comp.scopes);
    let stop_async_q = comp.wk(wk_idx::STOP_ASYNC_ITERATION);
    comp.emit.load_global(
        stop_async_q,
        MP_EMIT_IDOP_GLOBAL_GLOBAL,
        &mut comp.scopes,
    );
    comp.emit
        .binary_op(MP_BINARY_OP_EXCEPTION_MATCH, &mut comp.scopes);
    comp.emit
        .pop_jump_if(false, lab_try_finally as usize, &mut comp.scopes);
    comp.emit.pop_top(&mut comp.scopes); // pop exception instance
    // pop_except_jump pops the except block and jumps. Vendor passes
    // within_exc_handler=true; the bytecode encoder ignores it.
    comp.emit
        .pop_except_jump(lab_while_else as usize, &mut comp.scopes);

    comp.emit.label_assign(lab_try_finally as usize);
    comp.emit.adjust_stack_size(1, &mut comp.scopes); // if we jump here, the exc is on the stack
    comp.compile_decrease_except_level();
    comp.emit.end_except_handler(&mut comp.scopes);

    // Stack: (..., iterable)

    comp.emit.label_assign(lab_try_else as usize);
    comp.compile_node(nodes[2]); // body

    comp.emit.jump(lab_continue as usize, &mut comp.scopes);
    // END_BREAK_CONTINUE_BLOCK — break/continue in else target outer loop
    comp.break_label = old_break;
    comp.continue_label = old_continue;
    comp.break_continue_except_level = old_excl;

    comp.emit.label_assign(lab_while_else as usize);
    if !pn_is_null(nodes[3]) {
        comp.compile_node(nodes[3]);
    } // else

    comp.emit.label_assign(lab_break as usize);
    // Stack: (..., iterable)

    comp.emit.pop_top(&mut comp.scopes);
    // Stack: (...)
}

// Vendor compile_async_with_stmt_helper (compile.c:1863): nested
// recursion across `async with a, b: body`. Each pre-bit is either a
// PN_with_item (`expr as name`) or a bare expression.
fn compile_async_with_stmt_helper(comp: &mut Compiler, n: usize, nodes: &[Pn], body: Pn) {
    if n == 0 {
        comp.compile_node(body);
        return;
    }

    let l_finally_block = comp.comp_next_label();
    let l_aexit_no_exc = comp.comp_next_label();
    let l_ret_unwind_jump = comp.comp_next_label();
    let l_end = comp.comp_next_label();

    if comp
        .tree
        .is_struct_kind(nodes[0], grammar::RULE_with_item)
    {
        // `expr as name`
        let pns: Vec<Pn> = comp.tree.struct_nodes(nodes[0]).to_vec();
        comp.compile_node(pns[0]);
        comp.emit.dup_top(&mut comp.scopes);
        let aenter_q = comp.wk(wk_idx::AENTER);
        compile_await_object_method(comp, aenter_q);
        c_assign(comp, pns[1], AssignKind::Store);
    } else {
        // bare expression
        comp.compile_node(nodes[0]);
        comp.emit.dup_top(&mut comp.scopes);
        let aenter_q = comp.wk(wk_idx::AENTER);
        compile_await_object_method(comp, aenter_q);
        comp.emit.pop_top(&mut comp.scopes);
    }

    // Start the try-finally statement.
    comp.compile_increase_except_level(l_finally_block, MP_EMIT_SETUP_BLOCK_FINALLY);

    // Compile any additional pre-bits of the "async with", and also the body.
    comp.emit.adjust_stack_size(3, &mut comp.scopes); // stack adjust for possible UNWIND_JUMP state
    compile_async_with_stmt_helper(comp, n - 1, &nodes[1..], body);
    comp.emit.adjust_stack_size(-3, &mut comp.scopes);

    // Now the "finally" block. There are 3 cases at this point:
    // 1. no exception, fall through: stack (..., ctx_mgr)
    // 2. exception propagating out: stack (..., ctx_mgr, exc)
    // 3. return or unwind jump: stack (..., ctx_mgr, X, INT)

    // Handle case 1: vendor's emit_bc_async_with_setup_finally inlines
    // LOAD_CONST_NONE + ROT_TWO + JUMP(l_aexit_no_exc) and the
    // l_finally_block prefix. We inline it here directly (no new opcode).
    // Stack: (..., ctx_mgr)
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    comp.emit.rot_two(&mut comp.scopes);
    comp.emit.jump(l_aexit_no_exc as usize, &mut comp.scopes);

    // Start of "finally" block — entered via exception / return / unwind.
    comp.emit.label_assign(l_finally_block as usize);
    // Detect which case: is TOS an exception?
    comp.emit.dup_top(&mut comp.scopes);
    comp.emit.load_global(
        MP_QSTR_BASE_EXCEPTION,
        MP_EMIT_IDOP_GLOBAL_GLOBAL,
        &mut comp.scopes,
    );
    comp.emit
        .binary_op(MP_BINARY_OP_EXCEPTION_MATCH, &mut comp.scopes);
    comp.emit
        .pop_jump_if(false, l_ret_unwind_jump as usize, &mut comp.scopes);

    // Case 2: exception. Call __aexit__ and either swallow or re-raise.
    // Stack: (..., ctx_mgr, exc)
    comp.emit.dup_top(&mut comp.scopes);
    comp.emit.rot_three(&mut comp.scopes);
    comp.emit.rot_two(&mut comp.scopes);
    let aexit_q = comp.wk(wk_idx::AEXIT);
    comp.emit
        .load_method(aexit_q, false, &mut comp.scopes);
    comp.emit.rot_three(&mut comp.scopes);
    comp.emit.rot_three(&mut comp.scopes);
    comp.emit.dup_top(&mut comp.scopes);
    // type(exc): use the `type` builtin (MICROPY_CPYTHON_COMPAT off path).
    let type_q = comp.qstr_from_strn(b"type");
    comp.compile_load_id(type_q);
    comp.emit.rot_two(&mut comp.scopes);
    comp.emit.call_function(1, 0, 0, &mut comp.scopes);
    comp.emit.rot_two(&mut comp.scopes);
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes); // dummy traceback
    // Stack: (..., exc, __aexit__, ctx_mgr, type(exc), exc, None)
    comp.emit.call_method(3, 0, 0, &mut comp.scopes);
    compile_yield_from(comp);
    comp.emit
        .pop_jump_if(false, l_end as usize, &mut comp.scopes);
    comp.emit.pop_top(&mut comp.scopes); // pop exception
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes); // replace with None to swallow
    // Stack: (..., None)
    comp.emit.jump(l_end as usize, &mut comp.scopes);
    comp.emit.adjust_stack_size(2, &mut comp.scopes);

    // Case 3: return/unwind. Stack: (..., ctx_mgr, X, INT)
    comp.emit.label_assign(l_ret_unwind_jump as usize);
    comp.emit.rot_three(&mut comp.scopes);
    comp.emit.rot_three(&mut comp.scopes);
    comp.emit.label_assign(l_aexit_no_exc as usize);
    // Arrived from case 1 (jump) or case 3 (fall through):
    //   case 1: (..., None, ctx_mgr); case 3: (..., X, INT, ctx_mgr)
    let aexit_q2 = comp.wk(wk_idx::AEXIT);
    comp.emit
        .load_method(aexit_q2, false, &mut comp.scopes);
    comp.emit
        .load_const_tok(MP_TOKEN_KW_NONE, &mut comp.scopes);
    comp.emit.dup_top(&mut comp.scopes);
    comp.emit.dup_top(&mut comp.scopes);
    comp.emit.call_method(3, 0, 0, &mut comp.scopes);
    compile_yield_from(comp);
    comp.emit.pop_top(&mut comp.scopes);
    // Stack: case 1: (..., None) or case 3: (..., X, INT)
    comp.emit.adjust_stack_size(-1, &mut comp.scopes);

    // End of "finally" block.
    // Stack: (..., None) | (..., exc) | (..., X, INT)
    comp.emit.label_assign(l_end as usize);
    comp.compile_decrease_except_level();
}

/// PEP 617 `with (a, b, …):` wraps the item list in a with_body_paren
/// struct — unwrap it so downstream sees the raw list node.
/// (Same helper as in handlers.rs; kept local to this module.)
fn extract_with_list(comp: &Compiler, pn: Pn) -> Pn {
    if comp
        .tree
        .is_struct_kind(pn, grammar::RULE_with_body_paren)
    {
        return comp.tree.struct_nodes(pn)[0];
    }
    pn
}

// Vendor compile_async_with_stmt (compile.c:1967).
pub fn compile_async_with_stmt(comp: &mut Compiler, pn: Pn) {
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    let pre_list = extract_with_list(comp, nodes[0]);
    let list_kind = if comp
        .tree
        .is_struct_kind(pre_list, grammar::RULE_with_paren_list)
    {
        grammar::RULE_with_paren_list
    } else {
        grammar::RULE_with_stmt_list
    };
    let list = comp.tree.extract_list(pre_list, list_kind);
    debug_assert!(!list.is_empty());
    compile_async_with_stmt_helper(comp, list.len(), &list, nodes[1]);
}

// Vendor compile_async_stmt (compile.c:1977). Dispatches the inner
// (funcdef | with_stmt | for_stmt). The async_stmt grammar rule is
// `KW_ASYNC async_stmt_2`; KW_ASYNC isn't pushed, async_stmt_2 is a
// transparent OR rule, so pns->nodes[0] is the inner funcdef/with/for
// struct directly.
pub fn compile_async_stmt(comp: &mut Compiler, pn: Pn) {
    let inner = comp.tree.struct_nodes(pn)[0];
    debug_assert!(pn_is_struct(inner));
    let k = comp.tree.struct_kind(inner);
    if k == grammar::RULE_funcdef {
        // async def: compile as a regular funcdef, then mark scope as generator.
        compile_funcdef(comp, inner);
        let fscope = *comp
            .scope_slots
            .get(&inner)
            .expect("async_stmt: funcdef scope slot missing");
        comp.scopes.scopes[fscope].scope_flags |= SCOPE_FLAG_GENERATOR;
    } else {
        // async for/with: must be inside an async function (generator scope).
        let sc = comp.scope_cur.expect("async_stmt: no scope_cur");
        let sf = comp.scopes.scopes[sc].scope_flags;
        if (sf & SCOPE_FLAG_GENERATOR) == 0 {
            comp.compile_syntax_error(inner, "async for/with outside async function");
            return;
        }
        if k == grammar::RULE_for_stmt {
            compile_async_for_stmt(comp, inner);
        } else {
            debug_assert_eq!(k, grammar::RULE_with_stmt);
            compile_async_with_stmt(comp, inner);
        }
    }
}

// Vendor decorated body handler for async_funcdef (compile.c:936): a
// `@dec async def f(): ...` whose body is reached through compileDecorated
// hits us here. Behaves like compileFuncdef wrapped to also flag scope as
// generator.
pub fn compile_async_funcdef(comp: &mut Compiler, pn: Pn) {
    // pn is the async_funcdef wrapper; nodes[0] is the inner funcdef.
    let inner = comp.tree.struct_nodes(pn)[0];
    debug_assert!(pn_is_struct(inner));
    debug_assert_eq!(comp.tree.struct_kind(inner), grammar::RULE_funcdef);
    compile_funcdef(comp, inner);
    let fscope = *comp
        .scope_slots
        .get(&inner)
        .expect("async_funcdef: funcdef scope slot missing");
    comp.scopes.scopes[fscope].scope_flags |= SCOPE_FLAG_GENERATOR;
}

// Vendor compile_atom_expr_await (compile.c:2769).
// The atom_expr_await rule is `KW_AWAIT atom opt(atom_expr_trailers)`;
// KW_AWAIT isn't pushed by the parser, so node layout is identical to
// atom_expr_normal (nodes[0]=atom, nodes[1]=trailers).
pub fn compile_atom_expr_await(comp: &mut Compiler, pn: Pn) {
    let sc = comp.scope_cur.expect("atom_expr_await: no scope_cur");
    let kind = comp.scopes.scopes[sc].kind;
    if kind != ScopeKind::Function && kind != ScopeKind::Lambda {
        comp.compile_syntax_error(pn, "'await' outside function");
        return;
    }
    super::expr::compile_atom_expr_normal(comp, pn);
    compile_yield_from(comp);
}
