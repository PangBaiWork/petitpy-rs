//! Tests for `crate::runtime::argcheck` — the canonical arg-check
//! module (C15a). The packed-`sig` predicate is cross-checked
//! against the two pre-existing decoded helpers
//! (`errors::arg_check_num_sig`, `objfun::arg_check_num`) over a
//! grid; the parsers are checked against the Zig's `Impl` order
//! (positional first, then keyword lookup, then trailing checks).

use super::argcheck::*;
use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::arg_check_num;
use crate::objects::containers::tests::Fixture;
use crate::objects::objtype::errors::arg_check_num_sig;
use crate::objects::value::Value;
use crate::pyerr::PyError;

fn ctx_of<'a>(fx: &'a mut Fixture) -> CallCtx<'a> {
    CallCtx::new(
        &mut fx.arena,
        &mut fx.roots,
        &fx.types,
        &fx.pool,
        &mut fx.pending_raise,
    )
}

#[test]
fn sig_roundtrip_and_grid() {
    for (min, max, kw) in [(0, 0, false), (0, 1, false), (1, 3, true), (2, 2, false)] {
        let sig = make_sig(min, max, kw);
        assert_eq!(sig_decode(sig), (min as usize, max as usize, kw));
    }
    // The packed predicate must agree with both decoded helpers
    // everywhere (including the min == max branch the Zig spells
    // out separately).
    for min in 0u16..=3 {
        for max in min..=4 {
            for kw in [false, true] {
                let sig = make_sig(u32::from(min), u32::from(max), kw);
                for n_args in 0..=5usize {
                    for n_kw in 0..=2usize {
                        let want = arg_check_num(n_args, n_kw, min, max, kw);
                        assert_eq!(check_num_sig(n_args, n_kw, sig), want);
                        assert_eq!(
                            arg_check_num_sig(
                                n_args,
                                n_kw,
                                min as usize,
                                max as usize,
                                kw
                            )
                            .is_none(),
                            want
                        );
                    }
                }
            }
        }
    }
    // Keyword bit: kwargs without it are rejected at any count.
    assert!(!check_num_sig(0, 1, make_sig(0, 0, false)));
    assert!(check_num_sig(0, 1, make_sig(0, 0, true)));
}

#[test]
fn arg_check_num_sig_records_terse() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    // This is THIS module's packed-sig check (the errors.rs
    // decoded helper is only used by the grid test above).
    use super::argcheck::arg_check_num_sig as packed_check;
    assert!(packed_check(&mut ctx, 2, 0, make_sig(2, 2, false)).is_ok());
    assert!(ctx.pending_raise.is_none());
    let e = packed_check(&mut ctx, 1, 0, make_sig(2, 2, false));
    assert_eq!(e, Err(PyError::PyRaise));
    let p = ctx.pending_raise.as_ref().unwrap();
    assert_eq!(p.exc_name, "TypeError");
    assert_eq!(p.message.as_deref(), Some(TERSE_MISMATCH));
    // First record wins: a second failure keeps the first message.
    let _ = packed_check(&mut ctx, 0, 0, make_sig(5, 5, false));
    assert_eq!(ctx.pending_raise.as_ref().unwrap().message.as_deref(), Some(TERSE_MISMATCH));
}

fn spec(qst: u32, flags: u16, defval: ArgVal) -> ArgSpec {
    ArgSpec { qst, flags, defval }
}

#[test]
fn arg_parse_all_positional_defaults_and_kw() {
    let mut fx = Fixture::new();
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let qc = u32::from(fx.pool.from_strn(b"c").unwrap());
    let allowed = vec![
        spec(qa, ARG_OBJ | ARG_REQUIRED, ArgVal::Obj(Value::None)),
        spec(qb, ARG_INT, ArgVal::Int(7)),
        spec(qc, ARG_BOOL, ArgVal::Bool(false)),
    ];
    let mut ctx = ctx_of(&mut fx);
    // Positional fills a and b; c takes its default.
    let out = arg_parse_all(&mut ctx, &[Value::SmallInt(1), Value::SmallInt(2)], &[], &allowed)
        .unwrap();
    assert_eq!(
        out,
        vec![ArgVal::Obj(Value::SmallInt(1)), ArgVal::Int(2), ArgVal::Bool(false)]
    );
    // Keyword fills c; b takes its default.
    let out = arg_parse_all(
        &mut ctx,
        &[Value::SmallInt(1)],
        &[(qc, Value::SmallInt(1))],
        &allowed,
    )
    .unwrap();
    assert_eq!(
        out,
        vec![ArgVal::Obj(Value::SmallInt(1)), ArgVal::Int(7), ArgVal::Bool(true)]
    );
    // All-keyword call.
    let out = arg_parse_all(
        &mut ctx,
        &[],
        &[(qa, Value::SmallInt(5)), (qb, Value::SmallInt(0))],
        &allowed,
    )
    .unwrap();
    assert_eq!(
        out,
        vec![ArgVal::Obj(Value::SmallInt(5)), ArgVal::Int(0), ArgVal::Bool(false)]
    );
    // BOOL conversion: 0 is false.
    let out = arg_parse_all(&mut ctx, &[Value::SmallInt(1)], &[(qc, Value::SmallInt(0))], &allowed)
        .unwrap();
    assert_eq!(out[2], ArgVal::Bool(false));
}

#[test]
fn arg_parse_all_errors() {
    let mut fx = Fixture::new();
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let qz = u32::from(fx.pool.from_strn(b"z").unwrap());
    let allowed = vec![
        spec(qa, ARG_OBJ | ARG_REQUIRED, ArgVal::Obj(Value::None)),
        spec(qb, ARG_INT | ARG_KW_ONLY, ArgVal::Int(7)),
    ];
    // Missing required.
    let mut ctx = ctx_of(&mut fx);
    assert!(arg_parse_all(&mut ctx, &[], &[], &allowed).is_err());
    assert_eq!(
        ctx.pending_raise.as_ref().unwrap().message.as_deref(),
        Some(TERSE_MISMATCH)
    );
    // Unknown keyword.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        arg_parse_all(&mut ctx, &[Value::SmallInt(1)], &[(qz, Value::SmallInt(1))], &allowed)
            .is_err()
    );
    // Extra positional.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        arg_parse_all(
            &mut ctx,
            &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
            &[],
            &allowed
        )
        .is_err()
    );
    // KW_ONLY given positionally.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        arg_parse_all(&mut ctx, &[Value::SmallInt(1), Value::SmallInt(2)], &[], &allowed).is_err()
    );
    // Positional AND keyword for the same slot.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        arg_parse_all(&mut ctx, &[Value::SmallInt(1)], &[(qa, Value::SmallInt(2))], &allowed)
            .is_err()
    );
    // INT conversion failure records the conversion error: None
    // has no int conversion (the fixture NoneType has no
    // INT_MAYBE slot).
    let mut ctx = ctx_of(&mut fx);
    assert!(
        arg_parse_all(&mut ctx, &[Value::SmallInt(1)], &[(qb, Value::None)], &allowed).is_err()
    );
    assert!(ctx.pending_raise.is_some());
}

#[test]
fn kw_pos_parse_basic() {
    let mut fx = Fixture::new();
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let qc = u32::from(fx.pool.from_strn(b"c").unwrap());
    let names = [qa, qb, qc];
    let defaults = [None, Some(Value::SmallInt(9)), None];
    let mut ctx = ctx_of(&mut fx);
    // pos [1] + kw c=3: b takes its default, n counts to c.
    let r = kw_pos_parse(
        &mut ctx,
        &names,
        &defaults,
        1,
        &[Value::SmallInt(1)],
        &[(qc, Value::SmallInt(3))],
    )
    .unwrap();
    assert_eq!(
        r.args,
        vec![Some(Value::SmallInt(1)), Some(Value::SmallInt(9)), Some(Value::SmallInt(3))]
    );
    assert_eq!(r.n, 3);
    // Keyword-only b with a defaulted a: a takes its default.
    let defaults2 = [Some(Value::SmallInt(0)), Some(Value::SmallInt(9)), None];
    let r = kw_pos_parse(&mut ctx, &names, &defaults2, 0, &[], &[(qb, Value::SmallInt(5))])
        .unwrap();
    assert_eq!(
        r.args,
        vec![Some(Value::SmallInt(0)), Some(Value::SmallInt(5)), None]
    );
    assert_eq!(r.n, 2);
}

#[test]
fn kw_pos_parse_errors() {
    let mut fx = Fixture::new();
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let qz = u32::from(fx.pool.from_strn(b"z").unwrap());
    let names = [qa, qb];
    let defaults = [None, Some(Value::SmallInt(9))];
    let mut ctx = ctx_of(&mut fx);
    // Too many positionals.
    assert!(
        kw_pos_parse(
            &mut ctx,
            &names,
            &defaults,
            3,
            &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
            &[]
        )
        .is_err()
    );
    // Unknown keyword.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        kw_pos_parse(&mut ctx, &names, &defaults, 0, &[], &[(qz, Value::SmallInt(1))]).is_err()
    );
    // Duplicate (positional + keyword).
    let mut ctx = ctx_of(&mut fx);
    assert!(
        kw_pos_parse(
            &mut ctx,
            &names,
            &defaults,
            1,
            &[Value::SmallInt(1)],
            &[(qa, Value::SmallInt(2))]
        )
        .is_err()
    );
    // Gap below n with no default (kw b given, a missing).
    let mut ctx = ctx_of(&mut fx);
    assert!(
        kw_pos_parse(&mut ctx, &[qa, qb], &[None, None], 0, &[], &[(qb, Value::SmallInt(2))])
            .is_err()
    );
}

#[test]
fn kw_make_new_basic() {
    let mut fx = Fixture::new();
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let names = [qa, qb];
    let defaults = [None, None];
    let mut ctx = ctx_of(&mut fx);
    // args = [1, 'b', 2], n_kw = 1.
    let r = kw_make_new(
        &mut ctx,
        &names,
        &defaults,
        &[Value::SmallInt(1), Value::Qstr(qb), Value::SmallInt(2)],
        1,
    )
    .unwrap();
    assert_eq!(r.args, vec![Some(Value::SmallInt(1)), Some(Value::SmallInt(2))]);
    assert_eq!(r.n, 2);
    // Non-qstr key can never match a slot.
    let mut ctx = ctx_of(&mut fx);
    assert!(
        kw_make_new(
            &mut ctx,
            &names,
            &defaults,
            &[Value::SmallInt(1), Value::SmallInt(7), Value::SmallInt(2)],
            1,
        )
        .is_err()
    );
    // More keyword pairs than arguments.
    let mut ctx = ctx_of(&mut fx);
    assert!(kw_make_new(&mut ctx, &names, &defaults, &[Value::SmallInt(1)], 1).is_err());
}
