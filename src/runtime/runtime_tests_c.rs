//! Lane C14 tests (part 3): the call protocol and unpack.
//! Part 1 (`runtime_tests.rs`) covers names, unary/binary
//! dispatch; part 2 (`runtime_tests_b.rs`) covers
//! method/attribute loading, iteration, resume, and imports.
//! The `Env` and the `seven` / `echo` fixtures live in part 1.

use super::runtime_tests::{echo, seven, Env};
use crate::objects::slots::BinaryOp;
use crate::objects::value::Value;
use crate::runtime::runtime::RuntimeSeams;
use crate::runtime::runtime_call::{
    call_function_n_kw, call_method_n_kw, call_method_n_kw_var, call_prepare_args_n_kw_var,
};
use crate::runtime::runtime_iter::{unpack_ex, unpack_sequence};
use crate::runtime::runtime_method::MethodDest;

// ── call protocol ───────────────────────────────────────────────────

#[test]
fn call_not_callable_and_builtin() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // The `next` builtin is callable through the dispatch.
    let list = env.list_v(&[Value::SmallInt(41), Value::SmallInt(42)]);
    let it = {
        let mut ctx2 = env.ctx();
        let mut seams = RuntimeSeams::stub();
        crate::runtime::runtime_iter::getiter(&mut ctx2, list, next_obj, &mut seams).unwrap()
    };
    let mut ctx = env.ctx();
    let err = call_function_n_kw(&mut ctx, Value::SmallInt(1), 0, 0, &[]).unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "object not callable");
    let first = call_function_n_kw(&mut ctx, next_obj, 1, 0, &[it]).unwrap();
    assert_eq!(first, Value::SmallInt(41));
}

#[test]
fn call_method_dest_bound_and_unbound() {
    let mut env = Env::new();
    let seven_v = env.fun_v(b"seven", 0, 0, seven);
    let echo_v = env.fun_v(b"echo", 1, 1, echo);
    let self_v = Value::SmallInt(99);
    let mut ctx = env.ctx();
    // Unbound: called with exactly the given args.
    let dest = MethodDest { func: seven_v, self_: None };
    assert_eq!(call_method_n_kw(&mut ctx, 0, 0, &dest, &[]).unwrap(), Value::SmallInt(7));
    // Bound: self is prepended (the Zig's args[1] rule) — so
    // the bound target must take self as its first arg: `echo`
    // returns it. (A 0-arg fn called bound would be an
    // arg-count mismatch in the original too.)
    let dest = MethodDest { func: echo_v, self_: Some(self_v) };
    assert_eq!(call_method_n_kw(&mut ctx, 0, 0, &dest, &[]).unwrap(), Value::SmallInt(99));
}

#[test]
fn prepare_args_star_and_dstar() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let a_q = env.q(b"a");
    let b_q = env.q(b"b");
    let fun = env.fun_v(b"f", 0, 0, seven);
    let star = env.list_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let dstar = env.dict_v(&[
        (Value::Qstr(a_q), Value::SmallInt(9)),
        (Value::Qstr(b_q), Value::SmallInt(8)),
    ]);
    let mut ctx = env.ctx();
    let pool = ctx.pool;
    let mut intern = Env::intern_seam(pool);
    let mut seams = RuntimeSeams::stub();
    let prep = call_prepare_args_n_kw_var(
        &mut ctx,
        fun,
        None,
        &[Value::SmallInt(0)],
        &[star],
        &[(None, dstar)],
        &mut intern,
        next_obj,
        &mut seams,
    )
    .unwrap();
    assert_eq!(prep.fun, fun);
    assert_eq!(prep.n_args, 3);
    assert_eq!(prep.n_kw, 2);
    assert_eq!(
        prep.args,
        vec![
            Value::SmallInt(0),
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::Qstr(a_q),
            Value::SmallInt(9),
            Value::Qstr(b_q),
            Value::SmallInt(8),
        ]
    );
}

#[test]
fn prepare_args_non_str_dstar_key_errors() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let fun = env.fun_v(b"f", 0, 0, seven);
    let dstar = env.dict_v(&[(Value::SmallInt(1), Value::SmallInt(2))]);
    let mut ctx = env.ctx();
    let pool = ctx.pool;
    let mut intern = Env::intern_seam(pool);
    let mut seams = RuntimeSeams::stub();
    let err = call_prepare_args_n_kw_var(
        &mut ctx,
        fun,
        None,
        &[Value::SmallInt(0)],
        &[],
        &[(None, dstar)],
        &mut intern,
        next_obj,
        &mut seams,
    )
    .unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "can't convert to str implicitly");
}

#[test]
fn call_method_n_kw_var_star_iterable() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // Star-expanding a non-exact iterable goes through
    // getiter/iternext. A str would be the natural case
    // (`f(*"ab")`, probed), but the tree's str type has
    // `getiter: None` (C12 TODO — polymorph-iter type) and
    // C11's iternext has no str-iterator tag, so a str cannot
    // expand yet. A dict (real `dict_type_data` + getiter in
    // the fixture) expands its keys through the same path.
    let echo_v = env.fun_v(b"echo", 2, 2, echo);
    let a_q = env.q(b"a");
    let b_q = env.q(b"b");
    let d = env.dict_v(&[
        (Value::Qstr(a_q), Value::SmallInt(1)),
        (Value::Qstr(b_q), Value::SmallInt(2)),
    ]);
    let mut ctx = env.ctx();
    let pool = ctx.pool;
    let mut intern = Env::intern_seam(pool);
    let mut seams = RuntimeSeams::stub();
    // `echo` returns its first arg — the first expanded key.
    let got = call_method_n_kw_var(
        &mut ctx,
        echo_v,
        None,
        &[],
        &[d],
        &[],
        &mut intern,
        next_obj,
        &mut seams,
    )
    .unwrap();
    assert_eq!(got, Value::Qstr(a_q));
}

// ── unpack ──────────────────────────────────────────────────────────

#[test]
fn unpack_sequence_tuple_list_iter() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let tup = env.tuple_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let items = unpack_sequence(&mut ctx, tup, 2, next_obj, &mut seams).unwrap();
    assert_eq!(items, vec![Value::SmallInt(1), Value::SmallInt(2)]);
    drop(ctx);
    let list = env.list_v(&[Value::SmallInt(3)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let items = unpack_sequence(&mut ctx, list, 1, next_obj, &mut seams).unwrap();
    assert_eq!(items, vec![Value::SmallInt(3)]);
    // From an iterator (a list iterator, not a sequence).
    drop(ctx);
    let list2 = env.list_v(&[Value::SmallInt(4), Value::SmallInt(5)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = crate::runtime::runtime_iter::getiter(&mut ctx, list2, next_obj, &mut seams).unwrap();
    let items = unpack_sequence(&mut ctx, it, 2, next_obj, &mut seams).unwrap();
    assert_eq!(items, vec![Value::SmallInt(4), Value::SmallInt(5)]);
}

#[test]
fn unpack_sequence_wrong_count_errors() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let tup = env.tuple_v(&[Value::SmallInt(1)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    for n in [0, 2] {
        let err = unpack_sequence(&mut ctx, tup, n, next_obj, &mut seams).unwrap_err();
        assert_eq!(err.exc_name(), "ValueError");
        assert_eq!(err.message(), "wrong number of values to unpack");
    }
}

#[test]
fn unpack_ex_left_right_rest() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // `a, *b, c = [1, 2, 3, 4]` → left [1], rest [2, 3],
    // right [4]; `num_in = (1 << 8) | 1`.
    let list = env.list_v(&[
        Value::SmallInt(1),
        Value::SmallInt(2),
        Value::SmallInt(3),
        Value::SmallInt(4),
    ]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let ex = unpack_ex(&mut ctx, list, (1 << 8) | 1, next_obj, &mut seams).unwrap();
    assert_eq!(ex.left, vec![Value::SmallInt(1)]);
    assert_eq!(ex.right, vec![Value::SmallInt(4)]);
    let rest = ex.rest;
    drop(ctx);
    let rest_want = env.list_v(&[Value::SmallInt(2), Value::SmallInt(3)]);
    assert_eq!(
        env.binop(BinaryOp::Equal, rest, rest_want).unwrap(),
        Value::Bool(true)
    );
    // `a, *b = [1]` → left [1], rest [], right [].
    let list = env.list_v(&[Value::SmallInt(1)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let ex = unpack_ex(&mut ctx, list, 1, next_obj, &mut seams).unwrap();
    assert_eq!(ex.left, vec![Value::SmallInt(1)]);
    assert!(ex.right.is_empty());
    let empty = env.list_v(&[]);
    assert_eq!(env.binop(BinaryOp::Equal, ex.rest, empty).unwrap(), Value::Bool(true));
}
