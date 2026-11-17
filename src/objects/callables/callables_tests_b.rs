//! Tests for `callables` — prepare + runner (Lane C10a, part 2).
//!
//! Shares the fixture/helpers in `callables_tests.rs`
//! (`CFx`, `compile_fun`, `n_state_of`, `expected_ip`,
//! `tuple_items`).

use super::callables_tests::{compile_fun, expected_ip, n_state_of, tuple_items, CFx};
use super::objfun::*;
use crate::compiler::mpy::RawCode;
use crate::memory::gc::CallCtx;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::PyError;
use crate::runtime::bc::{CodeState, SetupError};

// ── `fun_bc_prepare_call` ───────────────────────────────────────

/// The compiled `f(a, b=2)` plus a `FunBc` for it (default `2`
/// supplied as `def_pos`, as `MAKE_FUNCTION` would). Returns the
/// fun's `GcRef` — `fun_bc_prepare_call` takes the ref, while
/// `fun_bc_call` takes the `Value` (`Value::Heap(fun)`).
fn prep_fixture(cfx: &mut CFx) -> (RawCode, Vec<u16>, GcRef) {
    let (code, table) = compile_fun("def f(a, b=2):\n    return a\n", &mut cfx.fx.pool);
    let def_pos = cfx.fx.heap_tuple(vec![Value::SmallInt(2)]);
    let name_id = cfx.q("f");
    let type_ = cfx.fun_bc;
    let mut ctx = cfx.ctx();
    let fun = new_fun_bc(&mut ctx, type_, name_id, 2, 0, Some(def_pos), None, None);
    (code, table, fun)
}

#[test]
fn prepare_binds_positionals_and_defaults() {
    let mut cfx = CFx::new();
    let (code, table, fun) = prep_fixture(&mut cfx);
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1)];
    let cs = fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 1, 0, &args).unwrap();
    let n = n_state_of(&code);
    assert_eq!(cs.n_state, n);
    assert_eq!(cs.ip, expected_ip(&code));
    // `state[n_state - 1 - i]` holds positional `i`.
    assert_eq!(cs.state[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(cs.state[n - 2], Some(Value::SmallInt(2)));
    assert_eq!(cs.state[..n - 2].iter().filter(|s| s.is_some()).count(), 0);
}

#[test]
fn prepare_errors() {
    let mut cfx = CFx::new();
    let (code, table, fun) = prep_fixture(&mut cfx);
    let empty = cfx.empty_tuple;
    // All kwarg name ids are resolved BEFORE the `ctx` is
    // created (a live `ctx` borrows the fixture mutably).
    let a_id = cfx.q("a");
    let b_id = cfx.q("b");
    let c_id = cfx.q("c");
    let mut ctx = cfx.ctx();
    // No args at all (probed: `f()` → terse mismatch).
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 0, 0, &[]).unwrap_err(),
        SetupError::ArgCountMismatch
    );
    // Too many positionals (probed: `f(1, 2, 3)` → terse mismatch).
    let args = [Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)];
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 3, 0, &args).unwrap_err(),
        SetupError::ArgCountMismatch
    );
    // Missing positional, kw branch (probed: `f(b=3)` → missing #0).
    let args = [Value::Qstr(b_id), Value::SmallInt(3)];
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 0, 1, &args).unwrap_err(),
        SetupError::MissingPositional { index: 0 }
    );
    // Unexpected keyword (probed: `f(1, c=5)`).
    let args = [Value::SmallInt(1), Value::Qstr(c_id), Value::SmallInt(5)];
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 1, 1, &args).unwrap_err(),
        SetupError::UnexpectedKeyword
    );
    // Multiple values (probed: `f(a=1, a=2)`).
    let args = [
        Value::SmallInt(1),
        Value::Qstr(a_id),
        Value::SmallInt(2),
    ];
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, fun, &code, &table, empty, 1, 1, &args).unwrap_err(),
        SetupError::MultipleValues { qstr: a_id }
    );
}

#[test]
fn prepare_defaults_and_varargs() {
    let mut cfx = CFx::new();
    // `def g(a, b, c=3)`: one default; (1, 2) binds a/b, c=3.
    let (g_code, g_table) = compile_fun("def g(a, b, c=3):\n    return (a, b, c)\n", &mut cfx.fx.pool);
    let def_pos = cfx.fx.heap_tuple(vec![Value::SmallInt(3)]);
    let g_name = cfx.q("g");
    let g_type = cfx.fun_bc;
    let g = {
        let mut ctx = cfx.ctx();
        new_fun_bc(&mut ctx, g_type, g_name, 3, 0, Some(def_pos), None, None)
    };
    let empty = cfx.empty_tuple;
    // The kwarg name id is resolved BEFORE the `ctx` is created.
    let a_id = cfx.q("a");
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1), Value::SmallInt(2)];
    let cs = fun_bc_prepare_call(&mut ctx, g, &g_code, &g_table, empty, 2, 0, &args).unwrap();
    let n = n_state_of(&g_code);
    assert_eq!(cs.state[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(cs.state[n - 2], Some(Value::SmallInt(2)));
    assert_eq!(cs.state[n - 3], Some(Value::SmallInt(3)));
    // (1,) is too few (probed: `g(1)` → terse mismatch).
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, g, &g_code, &g_table, empty, 1, 0, &args[..1]).unwrap_err(),
        SetupError::ArgCountMismatch
    );
    // (a=1) via kwargs: `b` missing at index 1 (probed).
    let args = [Value::Qstr(a_id), Value::SmallInt(1)];
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, g, &g_code, &g_table, empty, 0, 1, &args).unwrap_err(),
        SetupError::MissingPositional { index: 1 }
    );
    drop(ctx);

    // `def v(a, *args)`: the extra positionals pack into a tuple
    // at `state[n_state - 1 - n_pos_args - n_kwonly_args]`.
    let (v_code, v_table) = compile_fun("def v(a, *args):\n    return args\n", &mut cfx.fx.pool);
    let v_name = cfx.q("v");
    let v_type = cfx.fun_bc;
    let v = {
        let mut ctx = cfx.ctx();
        new_fun_bc(&mut ctx, v_type, v_name, 1, 0, None, None, None)
    };
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)];
    let cs = fun_bc_prepare_call(&mut ctx, v, &v_code, &v_table, empty, 3, 0, &args).unwrap();
    let n = n_state_of(&v_code);
    assert_eq!(cs.state[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(
        tuple_items(&ctx.arena, cs.state[n - 2].unwrap()),
        vec![Value::SmallInt(2), Value::SmallInt(3)]
    );
    // `v()` → terse mismatch (probed); `v(1)` → empty tuple.
    assert_eq!(
        fun_bc_prepare_call(&mut ctx, v, &v_code, &v_table, empty, 0, 0, &[]).unwrap_err(),
        SetupError::ArgCountMismatch
    );
    let args = [Value::SmallInt(1)];
    let cs = fun_bc_prepare_call(&mut ctx, v, &v_code, &v_table, empty, 1, 0, &args).unwrap();
    let n = n_state_of(&v_code);
    assert_eq!(tuple_items(&ctx.arena, cs.state[n - 2].unwrap()), Vec::<Value>::new());
    drop(ctx);

    // `def w(a, *args, k=9)`: kwargs land after the var-args
    // slot; the default applies when the kwarg is absent.
    let (w_code, w_table) =
        compile_fun("def w(a, *args, k=9):\n    return (a, args, k)\n", &mut cfx.fx.pool);
    let def_kw = cfx.fx.dict();
    let k_id = cfx.q("k");
    cfx.fx.store(def_kw, Value::Qstr(k_id), Value::SmallInt(9));
    let w_name = cfx.q("w");
    let w_type = cfx.fun_bc;
    let w = {
        let mut ctx = cfx.ctx();
        new_fun_bc(&mut ctx, w_type, w_name, 1, 0, None, Some(def_kw), None)
    };
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)];
    let cs = fun_bc_prepare_call(&mut ctx, w, &w_code, &w_table, empty, 3, 0, &args).unwrap();
    let n = n_state_of(&w_code);
    // Layout: `state[n-1] = a`, `state[n-2] = k` (kwonly args
    // bind at `state[n_state - 1 - j]`), `state[n-3] = args`
    // (the var-args tuple sits at `vpk_idx = n_state - 1 -
    // n_pos_args - n_kwonly_args`, which subtracts the kwonly
    // args too — `bc.rs` `setup_code_state`).
    assert_eq!(cs.state[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(cs.state[n - 2], Some(Value::SmallInt(9)));
    assert_eq!(
        tuple_items(&ctx.arena, cs.state[n - 3].unwrap()),
        vec![Value::SmallInt(2), Value::SmallInt(3)]
    );
    // With `k=5` the kwarg wins over the default.
    let args = [
        Value::SmallInt(1),
        Value::SmallInt(2),
        Value::SmallInt(3),
        Value::Qstr(k_id),
        Value::SmallInt(5),
    ];
    let cs = fun_bc_prepare_call(&mut ctx, w, &w_code, &w_table, empty, 3, 1, &args).unwrap();
    let n = n_state_of(&w_code);
    assert_eq!(cs.state[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(cs.state[n - 2], Some(Value::SmallInt(5)));
    assert_eq!(
        tuple_items(&ctx.arena, cs.state[n - 3].unwrap()),
        vec![Value::SmallInt(2), Value::SmallInt(3)]
    );
}

// ── `fun_bc_call` with a stub runner ────────────────────────────

#[test]
fn fun_bc_call_runner_outcomes() {
    let mut cfx = CFx::new();
    let (code, table, fun) = prep_fixture(&mut cfx);
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1)];
    // Normal: the runner sees the prepared state (a=1, b=2) and
    // its value is returned.
    let mut seen: Vec<Option<Value>> = Vec::new();
    {
        let mut runner = |_ctx: &mut CallCtx, cs: &mut CodeState| {
            seen = cs.state.clone();
            RunOutcome::Normal(Value::SmallInt(42))
        };
        let r = fun_bc_call(
            &mut ctx,
            Value::Heap(fun),
            &code,
            &table,
            empty,
            1,
            0,
            &args,
            &mut runner,
        );
        assert_eq!(r, Ok(Value::SmallInt(42)));
    }
    drop(seen);
    // Exception: the runner's value becomes `PyRaise` (the data
    // is dropped at the boundary until C13 adds pending_exc).
    let mut runner = |_ctx: &mut CallCtx, _cs: &mut CodeState| {
        RunOutcome::Exception(Value::None)
    };
    assert_eq!(
        fun_bc_call(
            &mut ctx,
            Value::Heap(fun),
            &code,
            &table,
            empty,
            1,
            0,
            &args,
            &mut runner
        ),
        Err(PyError::PyRaise)
    );
    // NeedsVm: the slot has no runner, so the call fails.
    let mut runner = |_ctx: &mut CallCtx, _cs: &mut CodeState| RunOutcome::NeedsVm;
    assert_eq!(
        fun_bc_call(
            &mut ctx,
            Value::Heap(fun),
            &code,
            &table,
            empty,
            1,
            0,
            &args,
            &mut runner
        ),
        Err(PyError::PyRaise)
    );
    // The registered slot has no runner at all.
    assert!(fun_bc_call_slot(&mut ctx, Value::Heap(fun), 1, 0, &args).is_err());
    // A non-FunBc value: not callable here.
    assert!(fun_bc_call_slot(&mut ctx, Value::SmallInt(1), 1, 0, &args).is_err());
}

#[test]
fn fun_bc_call_runner_sees_prepared_state() {
    let mut cfx = CFx::new();
    let (code, table, fun) = prep_fixture(&mut cfx);
    let empty = cfx.empty_tuple;
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(1)];
    let mut seen: Vec<Option<Value>> = Vec::new();
    let mut runner = |_ctx: &mut CallCtx, cs: &mut CodeState| {
        seen = cs.state.clone();
        RunOutcome::Normal(Value::None)
    };
    fun_bc_call(&mut ctx, Value::Heap(fun), &code, &table, empty, 1, 0, &args, &mut runner)
        .unwrap();
    drop(runner);
    let n = n_state_of(&code);
    assert_eq!(seen.len(), n);
    assert_eq!(seen[n - 1], Some(Value::SmallInt(1)));
    assert_eq!(seen[n - 2], Some(Value::SmallInt(2)));
}

