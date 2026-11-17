//! Tests for `callables` — bound methods + closures (Lane C10a,
//! part 3).
//!
//! Shares the fixture/helpers in `callables_tests.rs`
//! (`CFx`, `compile_fun`, `n_state_of`, `ret_arg0`, `ret_arg1`).

use super::callables_tests::{compile_fun, n_state_of, ret_arg0, ret_arg1, CFx};
use super::objboundmeth::*;
use super::objclosure::*;
use super::objfun::new_fun_bc;
use crate::runtime::runner::RunOutcome;
use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_id;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{wrap_small_int, FunBuiltinData, Obj, Value};
use crate::runtime::bc::CodeState;

// ── bound methods ───────────────────────────────────────────────

#[test]
fn bound_meth_call_slot_builtin_prepends_self() {
    let mut cfx = CFx::new();
    // A 2-arg builtin returning `args[1]`: the bound method
    // prepends `self`, so calling with `(7,)` returns 7.
    let t2 = cfx.fun_builtin_2;
    let f = cfx.builtin(t2, "two", 2, 2, ret_arg1);
    let self_value = Value::SmallInt(99);
    let bm_type = cfx.bound_meth;
    let fun_bc_type = cfx.fun_bc;
    let name_id = cfx.q("m");
    let mut ctx = cfx.ctx();
    let bm = new_bound_meth(&mut ctx, bm_type, f, self_value);
    let bm_value = Value::Heap(bm);
    let args = [Value::SmallInt(7)];
    assert_eq!(bound_meth_call_slot(&mut ctx, bm_value, 1, 0, &args), Ok(Value::SmallInt(7)));
    // Two positionals make three with `self` — over the sig.
    let args = [Value::SmallInt(7), Value::SmallInt(8)];
    assert!(bound_meth_call_slot(&mut ctx, bm_value, 2, 0, &args).is_err());
    // A FunBc underlying function cannot run without a runner.
    let fun_bc = Value::Heap(new_fun_bc(&mut ctx, fun_bc_type, name_id, 2, 0, None, None, None));
    let bm2 = new_bound_meth(&mut ctx, bm_type, fun_bc, self_value);
    assert!(bound_meth_call_slot(&mut ctx, Value::Heap(bm2), 1, 0, &[Value::SmallInt(7)])
        .is_err());
}

#[test]
fn bound_meth_call_primary_prepends_self_in_state() {
    let mut cfx = CFx::new();
    let (code, table) = compile_fun("def m(self, x):\n    return x\n", &mut cfx.fx.pool);
    let name_id = cfx.q("m");
    let type_ = cfx.fun_bc;
    let fun = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_fun_bc(&mut ctx, type_, name_id, 2, 0, None, None, None))
    };
    let self_value = Value::SmallInt(99);
    let bm_type = cfx.bound_meth;
    let bm = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_bound_meth(&mut ctx, bm_type, fun, self_value))
    };
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(7)];
    let mut seen: Vec<Option<Value>> = Vec::new();
    let mut runner =
        |_ctx: &mut CallCtx, cs: &mut CodeState, _send: Value, _throw: Option<Value>| {
            seen = cs.state.clone();
            RunOutcome::Normal(Value::SmallInt(7))
        };
    let r = bound_meth_call(&mut ctx, bm, &code, &table, empty, 1, 0, &args, &mut runner);
    assert_eq!(r, Ok(Value::SmallInt(7)));
    drop(runner);
    let n = n_state_of(&code);
    assert_eq!(seen[n - 1], Some(self_value));
    assert_eq!(seen[n - 2], Some(Value::SmallInt(7)));
}

#[test]
fn bound_meth_hash_and_equal() {
    let mut cfx = CFx::new();
    let t1 = cfx.fun_builtin_1;
    let f = cfx.builtin(t1, "one", 1, 1, ret_arg0);
    let f2 = cfx.builtin(t1, "one", 1, 1, ret_arg0);
    let s1 = Value::SmallInt(1);
    let s2 = Value::SmallInt(2);
    let bm_type = cfx.bound_meth;
    let mut ctx = cfx.ctx();
    let bm1 = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, s1));
    let bm2 = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, s1));
    let bm3 = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, s2));
    let bm4 = Value::Heap(new_bound_meth(&mut ctx, bm_type, f2, s1));
    // HASH is `wrap_small_int(word(self) ^ word(meth))` over the
    // REPR_A words (`obj_id`).
    let word = |v: Value| match obj_id(v) {
        Value::SmallInt(w) => w,
        _ => panic!("obj_id must be a small int"),
    };
    let expected = Value::SmallInt(wrap_small_int(word(s1) ^ word(f)));
    assert_eq!(bound_meth_unary_op(&mut ctx, UnaryOp::Hash, bm1), Ok(expected));
    // Non-HASH unary ops are not implemented.
    assert!(bound_meth_unary_op(&mut ctx, UnaryOp::Positive, bm1).is_err());
    // EQUAL: same self + same func → True; different self →
    // False; a distinct builtin object (same body) → False
    // (`obj_equal` falls back to pointer comparison).
    assert_eq!(
        bound_meth_binary_op(&mut ctx, BinaryOp::Equal, bm1, bm2),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        bound_meth_binary_op(&mut ctx, BinaryOp::Equal, bm1, bm3),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        bound_meth_binary_op(&mut ctx, BinaryOp::Equal, bm1, bm4),
        Ok(Value::Bool(false))
    );
    // NOT_EQUAL is not a slot op; a non-bound-method rhs fails.
    assert!(bound_meth_binary_op(&mut ctx, BinaryOp::NotEqual, bm1, bm2).is_err());
    assert!(bound_meth_binary_op(&mut ctx, BinaryOp::Equal, bm1, Value::SmallInt(1)).is_err());
    drop(ctx);
    // Two heap strings with equal bytes compare equal as `self`.
    let s_a = cfx.fx.heap_str(b"abc");
    let s_b = cfx.fx.heap_str(b"abc");
    let mut ctx = cfx.ctx();
    let bm_a = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, s_a));
    let bm_b = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, s_b));
    assert_eq!(
        bound_meth_binary_op(&mut ctx, BinaryOp::Equal, bm_a, bm_b),
        Ok(Value::Bool(true))
    );
}

#[test]
fn bound_meth_attr_and_children() {
    let mut cfx = CFx::new();
    let name_id = cfx.q("m");
    let type_ = cfx.fun_bc;
    let fun = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None))
    };
    let bm_type = cfx.bound_meth;
    let self_value = Value::SmallInt(3);
    let f1_type = cfx.fun_builtin_1;
    let mut ctx = cfx.ctx();
    let bm = Value::Heap(new_bound_meth(&mut ctx, bm_type, fun, self_value));
    let name_attr = static_qstr_id(b"__name__").unwrap();
    assert_eq!(bound_meth_attr(&mut ctx, bm, name_attr), Ok(Some(Value::Qstr(name_id))));
    // A builtin underlying function never answers attributes
    // (probed: `[].append.__name__` → AttributeError).
    let f = {
        let name = static_qstr_id(b"append").unwrap();
        Value::Heap(ctx.alloc(
            Some(f1_type),
            Obj::FunBuiltin(FunBuiltinData {
                name,
                n_args: fun_n_args(1, 1),
                func: ret_arg0,
                kw_func: None,
            }),
        ))
    };
    let bm2 = Value::Heap(new_bound_meth(&mut ctx, bm_type, f, self_value));
    assert_eq!(bound_meth_attr(&mut ctx, bm2, name_attr), Ok(None));
    // `children()` traces the function and `self`.
    let r = match bm {
        Value::Heap(r) => r,
        _ => panic!("not a heap value"),
    };
    let children = match ctx.arena.get(r) {
        Obj::BoundMeth(d) => Obj::BoundMeth(d.clone()).children(),
        _ => panic!("not a BoundMeth"),
    };
    assert_eq!(children, vec![fun, self_value]);
}

// ── closures ────────────────────────────────────────────────────

#[test]
fn closure_call_slot_builtin_prepends_cells() {
    let mut cfx = CFx::new();
    // A 3-arg builtin returning `args[0]` / `args[1]`: the
    // closure prepends its cells, so the call sees them first.
    let t3 = cfx.fun_builtin_3;
    let f0 = cfx.builtin(t3, "three", 3, 3, ret_arg0);
    let f1 = cfx.builtin(t3, "three", 3, 3, ret_arg1);
    let c0 = cfx.cell(Some(Value::SmallInt(10)));
    let c1 = cfx.cell(Some(Value::SmallInt(20)));
    let cl_type = cfx.closure;
    let fun_bc_type = cfx.fun_bc;
    let fun_bc_name = cfx.q("h");
    let mut ctx = cfx.ctx();
    let cl0 = Value::Heap(new_closure(&mut ctx, cl_type, f0, vec![c0, c1]));
    let cl1 = Value::Heap(new_closure(&mut ctx, cl_type, f1, vec![c0, c1]));
    let x = Value::SmallInt(7);
    assert_eq!(
        closure_call_slot(&mut ctx, cl0, 1, 0, &[x]),
        Ok(Value::Heap(c0))
    );
    assert_eq!(
        closure_call_slot(&mut ctx, cl1, 1, 0, &[x]),
        Ok(Value::Heap(c1))
    );
    // `children()` traces the fun and the cells.
    let r = match cl0 {
        Value::Heap(r) => r,
        _ => panic!("not a heap value"),
    };
    let children = match ctx.arena.get(r) {
        Obj::Closure(d) => Obj::Closure(d.clone()).children(),
        _ => panic!("not a Closure"),
    };
    assert_eq!(children, vec![f0, Value::Heap(c0), Value::Heap(c1)]);
    // A FunBc underlying function cannot run without a runner.
    let fun_bc = Value::Heap(new_fun_bc(
        &mut ctx,
        fun_bc_type,
        fun_bc_name,
        3,
        0,
        None,
        None,
        None,
    ));
    let cl2 = Value::Heap(new_closure(&mut ctx, cl_type, fun_bc, vec![c0, c1]));
    assert!(closure_call_slot(&mut ctx, cl2, 1, 0, &[x]).is_err());
}

#[test]
fn closure_call_primary_prepends_cells_in_state() {
    let mut cfx = CFx::new();
    let (code, table) =
        compile_fun("def h(a, b, c):\n    return a\n", &mut cfx.fx.pool);
    let name_id = cfx.q("h");
    let type_ = cfx.fun_bc;
    let fun = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_fun_bc(&mut ctx, type_, name_id, 3, 0, None, None, None))
    };
    let c0 = cfx.cell(Some(Value::SmallInt(10)));
    let c1 = cfx.cell(Some(Value::SmallInt(20)));
    let cl_type = cfx.closure;
    let cl = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_closure(&mut ctx, cl_type, fun, vec![c0, c1]))
    };
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let x = Value::SmallInt(7);
    let mut seen: Vec<Option<Value>> = Vec::new();
    let mut runner =
        |_ctx: &mut CallCtx, cs: &mut CodeState, _send: Value, _throw: Option<Value>| {
            seen = cs.state.clone();
            RunOutcome::Normal(Value::SmallInt(10))
        };
    let r = closure_call(&mut ctx, cl, &code, &table, empty, 1, 0, &[x], &mut runner);
    assert_eq!(r, Ok(Value::SmallInt(10)));
    drop(runner);
    let n = n_state_of(&code);
    assert_eq!(seen[n - 1], Some(Value::Heap(c0)));
    assert_eq!(seen[n - 2], Some(Value::Heap(c1)));
    assert_eq!(seen[n - 3], Some(x));
}

#[test]
fn closure_attr_delegates_to_fun() {
    let mut cfx = CFx::new();
    let name_id = cfx.q("h");
    let type_ = cfx.fun_bc;
    let fun = {
        let mut ctx = cfx.ctx();
        Value::Heap(new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None))
    };
    let c0 = cfx.cell(None);
    let cl_type = cfx.closure;
    let mut ctx = cfx.ctx();
    let cl = Value::Heap(new_closure(&mut ctx, cl_type, fun, vec![c0]));
    let name_attr = static_qstr_id(b"__name__").unwrap();
    // Probed: `h.__name__ == "h"` on a real closure.
    assert_eq!(closure_attr(&mut ctx, cl, name_attr), Ok(Some(Value::Qstr(name_id))));
    let globals_attr = static_qstr_id(b"__globals__").unwrap();
    assert_eq!(closure_attr(&mut ctx, cl, globals_attr), Ok(None));
}
