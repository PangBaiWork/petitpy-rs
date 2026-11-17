//! Tests for the generator port (Lane C10b), part 2: `throw`,
//! exceptions + PEP 479, `close`, print, types, locals dict, stubs.
//!
//! Helpers live in `objgenerator_tests.rs` (same module tree).

use super::objgenerator::*;
use super::objgenerator_tests::{exc_type, exc_value, script_runner, state_of, wrap_gen, GEN_RET_SRC, GEN_SRC};
use crate::objects::containers::tests::{empty_dict, Fixture};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBcData, GenState, Obj, Value};

// ── throw ──────────────────────────────────────────────────────────

#[test]
fn throw_selects_value_arg() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let type_exc = exc_value(&mut fx, "ValueError", vec![]);
    let value_exc = exc_value(&mut fx, "TypeError", vec![]);
    let mut ctx = fx.ctx();
    // throw(type, value) delivers the VALUE as the exception.
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Yield(Value::SmallInt(1))], &log);
    assert_eq!(
        gen_instance_throw(&mut ctx, gen, &mut frame, &mut runner, &[type_exc, value_exc])
            .unwrap(),
        Some(Value::SmallInt(1))
    );
    assert_eq!(*log.borrow(), vec![(Value::None, Some(value_exc))]);
    // throw(type, None) keeps the type (None is not an override).
    let log2 = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner2 = script_runner(vec![RunOutcome::Yield(Value::SmallInt(2))], &log2);
    assert_eq!(
        gen_instance_throw(
            &mut ctx,
            gen,
            &mut frame,
            &mut runner2,
            &[type_exc, Value::None]
        )
        .unwrap(),
        Some(Value::SmallInt(2))
    );
    assert_eq!(*log2.borrow(), vec![(Value::None, Some(type_exc))]);
    // throw(type) alone delivers the type.
    let log3 = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner3 = script_runner(vec![RunOutcome::Yield(Value::SmallInt(3))], &log3);
    assert_eq!(
        gen_instance_throw(&mut ctx, gen, &mut frame, &mut runner3, &[type_exc]).unwrap(),
        Some(Value::SmallInt(3))
    );
    assert_eq!(*log3.borrow(), vec![(Value::None, Some(type_exc))]);
}

#[test]
fn throw_arity() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let exc = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![], &log);
    assert_eq!(
        gen_instance_throw(&mut ctx, gen, &mut frame, &mut runner, &[]).unwrap_err(),
        GenError::ArgNumTypesMismatch
    );
    assert_eq!(
        gen_instance_throw(
            &mut ctx,
            gen,
            &mut frame,
            &mut runner,
            &[exc, exc, exc, exc]
        )
        .unwrap_err(),
        GenError::ArgNumTypesMismatch
    );
    assert!(log.borrow().is_empty());
}

// ── exceptions + PEP 479 ───────────────────────────────────────────

#[test]
fn raised_exception_propagates_and_closes() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let err_val = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(err_val)], &log);
    let err = gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap_err();
    assert_eq!(err, GenError::Raised(err_val));
    assert_eq!(err.exc_name(), "");
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
}

#[test]
fn pep479_stopiteration_becomes_runtime_error() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let stop_val = exc_value(&mut fx, "StopIteration", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(stop_val)], &log);
    let err = gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap_err();
    assert_eq!(err, GenError::GeneratorRaisedStopIteration);
    assert_eq!(err.exc_name(), "RuntimeError");
    assert_eq!(err.message(), Some("generator raised StopIteration"));
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
}

#[test]
fn pep479_applies_to_stopiteration_subclass() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let stop_type = exc_type(&mut fx, "StopIteration", vec![]);
    let sub_val = exc_value(&mut fx, "MyStop", vec![stop_type]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(sub_val)], &log);
    let err = gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap_err();
    assert_eq!(err, GenError::GeneratorRaisedStopIteration);
}

#[test]
fn pep479_does_not_fire_for_other_exceptions() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let val = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(val)], &log);
    let err = gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap_err();
    assert_eq!(err, GenError::Raised(val));
}

// ── close ──────────────────────────────────────────────────────────

#[test]
fn close_on_new_goes_through_resume_and_swallows_exit() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let ge = exc_value(&mut fx, "GeneratorExit", vec![]);
    let mut ctx = fx.ctx();
    // The runner simulates the VM raising GeneratorExit at frame
    // entry (the body never runs): it returns the injected
    // exception straight back.
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(ge)], &log);
    assert_eq!(
        gen_instance_close(&mut ctx, gen, &mut frame, &mut runner, ge).unwrap(),
        Value::None
    );
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
    // The runner WAS called (close is not a fast path for New).
    assert_eq!(*log.borrow(), vec![(Value::None, Some(ge))]);
}

#[test]
fn close_yielding_again_is_runtime_error() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let ge = exc_value(&mut fx, "GeneratorExit", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(
        vec![
            RunOutcome::Yield(Value::SmallInt(1)),
            RunOutcome::Yield(Value::SmallInt(99)),
        ],
        &log,
    );
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(1))
    );
    let err = gen_instance_close(&mut ctx, gen, &mut frame, &mut runner, ge).unwrap_err();
    assert_eq!(err, GenError::IgnoredGeneratorExit);
    assert_eq!(err.exc_name(), "RuntimeError");
    assert_eq!(err.message(), Some("generator ignored GeneratorExit"));
    // The generator is left suspended (the Yield branch does not close it).
    assert_eq!(state_of(&ctx, gen), GenState::Suspended);
}

#[test]
fn close_rejects_other_exceptions() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let ge = exc_value(&mut fx, "GeneratorExit", vec![]);
    let other = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(
        vec![
            RunOutcome::Yield(Value::SmallInt(1)),
            RunOutcome::Exception(other),
        ],
        &log,
    );
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(1))
    );
    let err = gen_instance_close(&mut ctx, gen, &mut frame, &mut runner, ge).unwrap_err();
    assert_eq!(err, GenError::Raised(other));
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
}

#[test]
fn close_swallows_generator_exit_subclass() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let ge = exc_value(&mut fx, "GeneratorExit", vec![]);
    let ge_type = exc_type(&mut fx, "GeneratorExit", vec![]);
    let sub = exc_value(&mut fx, "MyExit", vec![ge_type]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Exception(sub)], &log);
    assert_eq!(
        gen_instance_close(&mut ctx, gen, &mut frame, &mut runner, ge).unwrap(),
        Value::None
    );
}

#[test]
fn close_on_closed_is_noop() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let ge = exc_value(&mut fx, "GeneratorExit", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Normal(Value::None)], &log);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        None
    );
    let log2 = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner2 = script_runner(vec![], &log2);
    assert_eq!(
        gen_instance_close(&mut ctx, gen, &mut frame, &mut runner2, ge).unwrap(),
        Value::None
    );
    assert!(log2.borrow().is_empty());
}

// ── print, types, locals dict, stubs ───────────────────────────────

#[test]
fn print_renders_name_and_address() {
    let mut fx = Fixture::new();
    let (gen, _frame) = wrap_gen(&mut fx, GEN_RET_SRC, "mygen", &[], &[]);
    let mut ctx = fx.ctx();
    let mut out = String::new();
    gen_instance_print(
        &mut ctx,
        gen,
        crate::objects::slots::PrintKind::Repr,
        &mut out,
    );
    assert!(out.starts_with("<generator object 'mygen' at 0x"), "{out}");
    assert!(out.ends_with('>'), "{out}");
    let hex = out.trim_start_matches("<generator object 'mygen' at 0x").trim_end_matches('>');
    assert!(u64::from_str_radix(hex, 16).is_ok(), "{out}");
}

#[test]
fn type_data_flags_names_and_slots() {
    let mut fx = Fixture::new();
    let gen_id = static_qstr_id(b"generator").expect("generator is static");
    let wrap_dict = empty_dict(&mut fx.arena, &fx.roots);
    let wrap_td = gen_wrap_type_data(wrap_dict);
    assert_eq!(wrap_td.name, gen_id);
    assert_eq!(wrap_td.flags, 0x0020);
    assert!(wrap_td.call.is_some());
    assert!(wrap_td.print.is_none());
    let inst_dict = empty_dict(&mut fx.arena, &fx.roots);
    let inst_td = gen_instance_type_data(inst_dict);
    assert_eq!(inst_td.name, gen_id);
    assert_eq!(inst_td.flags, 0x0080);
    assert!(inst_td.print.is_some());
    assert!(inst_td.getiter.is_some());
    assert!(inst_td.call.is_none());
    // getiter returns the generator itself.
    let (gen, _frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let mut ctx = fx.ctx();
    let getiter = inst_td.getiter.unwrap();
    assert_eq!(getiter(&mut ctx, gen).unwrap(), gen);
}

#[test]
fn locals_dict_has_four_methods_with_sigs() {
    let mut fx = Fixture::new();
    let dict = {
        let mut ctx = fx.ctx();
        gen_instance_locals_dict(&mut ctx)
    };
    let pairs = fx.pairs(Value::Heap(dict));
    assert_eq!(pairs.len(), 4);
    let names: Vec<String> = pairs
        .iter()
        .map(|(k, _)| match k {
            Value::Qstr(id) => {
                String::from_utf8_lossy(fx.pool.qstr_str(*id as u16)).into_owned()
            }
            _ => panic!("method key is not a qstr"),
        })
        .collect();
    assert_eq!(names, vec!["send", "throw", "close", "pend_throw"]);
    // Each value is a FunBuiltin with the (min << 8) | max sig.
    let sigs: Vec<u16> = pairs
        .iter()
        .map(|(_, v)| match v {
            Value::Heap(r) => match fx.arena.get(*r) {
                Obj::FunBuiltin(f) => f.n_args,
                _ => panic!("method value is not a FunBuiltin"),
            },
            _ => panic!("method value is not heap"),
        })
        .collect();
    assert_eq!(sigs, vec![0x0202, 0x0204, 0x0101, 0x0202]);
}

#[test]
fn builtin_stubs_and_wrap_slot_are_not_implemented() {
    let mut fx = Fixture::new();
    let (gen, _frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let wrap_dict = empty_dict(&mut fx.arena, &fx.roots);
    let wrap_td = gen_wrap_type_data(wrap_dict);
    let call = wrap_td.call.unwrap();
    let fun = {
        let mut ctx2 = fx.ctx();
        Value::Heap(ctx2.alloc(None, Obj::FunBc(FunBcData {
            name: 0,
            n_args: 0,
            code: 0,
            def_pos: Vec::new(),
            def_kw: None,
            context: None,
        })))
    };
    let mut ctx = fx.ctx();
    // Correct arity still returns PyRaise (the runner lives in C13).
    for (f, args) in [
        (gen_instance_send_builtin as crate::objects::slots::BuiltinFn, vec![gen, Value::None]),
        (gen_instance_throw_builtin as crate::objects::slots::BuiltinFn, vec![gen, Value::None]),
        (gen_instance_close_builtin as crate::objects::slots::BuiltinFn, vec![gen]),
        (gen_instance_pend_throw_builtin as crate::objects::slots::BuiltinFn, vec![gen, Value::None]),
    ] {
        assert_eq!(
            f(&mut ctx, &args).unwrap_err(),
            crate::pyerr::PyError::PyRaise
        );
    }
    // Wrong arity is the same bare signal (data dropped at the
    // boundary — the C6 convention; the real sig check raises
    // TypeError("argument num/types mismatch") in the VM).
    assert_eq!(
        gen_instance_send_builtin(&mut ctx, &[]).unwrap_err(),
        crate::pyerr::PyError::PyRaise
    );
    // The gen_wrap type's call slot is the same story.
    assert_eq!(
        call(&mut ctx, fun, 0, 0, &[]).unwrap_err(),
        crate::pyerr::PyError::PyRaise
    );
}
