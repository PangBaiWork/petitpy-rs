//! Tests for the generator port (Lane C10b).
//!
//! The runner is a scripted stub (a closure returning queued
//! `RunOutcome`s and logging its `(send, throw)` inputs), so the
//! whole state machine is pinned without the VM. Reference-binary
//! probe values are recorded in `scripts/callables_b_result.txt`.

use std::collections::VecDeque;

use super::objgenerator::*;
use crate::compiler::bridge::compile_to_mpy;
use crate::compiler::mpy::{self, Module, QstrEntry, RawCode};
use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::containers::tests::{empty_dict, Fixture};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{ExceptionData, FunBcData, GenState, Obj, TypeData, Value};
use crate::runtime::bc::{prelude_sig_decode, CodeState};
use crate::runtime::qstr::RuntimeQstr;

// ── helpers ────────────────────────────────────────────────────────

/// Compile `source` (a module defining exactly one function) and
/// return that function's `RawCode` plus the resolved module qstr
/// table — the `bc_tests.rs` `compile_fun` pattern, replicated
/// here (C5's helpers are private to its test module).
pub(crate) fn compile_fun(source: &str, pool: &mut RuntimeQstr) -> (RawCode, Vec<u16>) {
    let bytes =
        compile_to_mpy(source, InputKind::FileInput).expect("test source must compile");
    let Module { qstrs, rc, .. } = mpy::read(&bytes).expect("test .mpy must read");
    assert_eq!(
        rc.children.len(),
        1,
        "test module must define exactly one function"
    );
    let fun = rc.children.into_iter().next().unwrap();
    let mut table = Vec::with_capacity(qstrs.len());
    for e in &qstrs {
        match e {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => {
                table.push(pool.from_strn(bytes).expect("qstr intern"))
            }
        }
    }
    (fun, table)
}

/// Wrap `source`'s single function into a generator: compile it,
/// build its `FunBc`, call `gen_wrap_call` with `args` (positional
/// only) and `extra_args` (defaults). Returns the generator handle
/// and its frame.
pub(crate) fn wrap_gen(
    fx: &mut Fixture,
    source: &str,
    name: &str,
    args: &[Value],
    extra_args: &[Value],
) -> (Value, CodeState) {
    let (code, table) = compile_fun(source, &mut fx.pool);
    let name_id = u32::from(
        fx.pool
            .from_strn(name.as_bytes())
            .expect("intern fun name"),
    );
    let mut cursor: &[u8] = &code.fun_data;
    let n_pos = prelude_sig_decode(&mut cursor).n_pos_args;
    let gen_type = fx.gen_instance;
    let (fun, empty_tuple) = {
        let mut ctx = fx.ctx();
        let fun = ctx.alloc(
            None,
            Obj::FunBc(FunBcData {
                name: name_id,
                n_args: n_pos as u16,
                code: 0,
                def_pos: Vec::new(),
                def_kw: None,
                context: None,
            }),
        );
        let empty_tuple = ctx.alloc(None, Obj::Tuple(Vec::new()));
        (fun, Value::Heap(empty_tuple))
    };
    let mut ctx = fx.ctx();
    let (gen, frame) = gen_wrap_call(
        &mut ctx,
        fun,
        &code,
        &table,
        extra_args,
        empty_tuple,
        gen_type,
        args.len(),
        0,
        args,
    )
    .expect("wrap must succeed");
    (Value::Heap(gen), frame)
}

/// Like `wrap_gen`, but returns the error instead of panicking.
pub(crate) fn try_wrap_gen(
    fx: &mut Fixture,
    source: &str,
    name: &str,
    args: &[Value],
    extra_args: &[Value],
) -> Result<(Value, CodeState), GenError> {
    let (code, table) = compile_fun(source, &mut fx.pool);
    let name_id = u32::from(
        fx.pool
            .from_strn(name.as_bytes())
            .expect("intern fun name"),
    );
    let mut cursor: &[u8] = &code.fun_data;
    let n_pos = prelude_sig_decode(&mut cursor).n_pos_args;
    let gen_type = fx.gen_instance;
    let (fun, empty_tuple) = {
        let mut ctx = fx.ctx();
        let fun = ctx.alloc(
            None,
            Obj::FunBc(FunBcData {
                name: name_id,
                n_args: n_pos as u16,
                code: 0,
                def_pos: Vec::new(),
                def_kw: None,
                context: None,
            }),
        );
        let empty_tuple = ctx.alloc(None, Obj::Tuple(Vec::new()));
        (fun, Value::Heap(empty_tuple))
    };
    let mut ctx = fx.ctx();
    gen_wrap_call(
        &mut ctx,
        fun,
        &code,
        &table,
        extra_args,
        empty_tuple,
        gen_type,
        args.len(),
        0,
        args,
    )
    .map(|(g, f)| (Value::Heap(g), f))
}

/// An exception type object with this name and MRO (the MRO is
/// hand-built so subclass checks are testable pre-C12). Names that
/// are not static qstrs (test-only subclass names like `"MyStop"`)
/// are interned in the runtime pool instead — ids start at
/// `N_TOTAL`, the same combined id space as `TypeData.name`.
pub(crate) fn exc_type(fx: &mut Fixture, name: &str, mro: Vec<crate::objects::value::GcRef>) -> crate::objects::value::GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let id: u32 = match static_qstr_id(name.as_bytes()) {
        Some(id) => id,
        None => fx
            .pool
            .from_str(name)
            .expect("exc type name internable") as u32,
    };
    let t = fx.arena.alloc(
        None,
        Obj::Type(TypeData {
            name: id,
            flags: 0,
            base: None,
            mro,
            dict,
namedtuple_fields: None,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
        }),
        &fx.roots,
    );
    fx.roots.push(Value::Heap(t));
    t
}

/// An exception instance of a type built by `exc_type`.
pub(crate) fn exc_value(
    fx: &mut Fixture,
    type_name: &str,
    mro: Vec<crate::objects::value::GcRef>,
) -> Value {
    let t = exc_type(fx, type_name, mro);
    let mut ctx = fx.ctx();
    Value::Heap(ctx.alloc(
        Some(t),
        Obj::Exception(ExceptionData {
            type_: t,
            args: Vec::new(),
            traceback: Vec::new(),
            cause: None,
        }),
    ))
}

/// The state of a generator value (panics on non-generators —
/// tests assert the error paths separately). Takes the `CallCtx`
/// (not the fixture) so it can be called while a `ctx` is alive.
pub(crate) fn state_of(ctx: &CallCtx, gen: Value) -> GenState {
    match gen {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Generator(g) => g.state,
            _ => panic!("state_of on a non-generator"),
        },
        _ => panic!("state_of on an immediate"),
    }
}

/// A runner that returns `outcomes` in order, logging each
/// `(send, throw)` input into `log`.
///
/// The log is an `Rc<RefCell<…>>` on purpose: the closure only holds
/// a shared `&Rc`, so the test may read the log at any time — even
/// while the runner is alive and the `CallCtx` is still borrowed
/// (a `&mut Vec` log would fight the borrow checker there, because
/// the runner's `GenRunner<'a>` lifetime is unified with the
/// `CallCtx` lifetime at the call site).
pub(crate) fn script_runner(
    outcomes: Vec<RunOutcome>,
    log: &std::rc::Rc<std::cell::RefCell<Vec<(Value, Option<Value>)>>>,
) -> impl FnMut(&mut CallCtx<'_>, &mut CodeState, Value, Option<Value>) -> RunOutcome + '_ {
    let mut queue: VecDeque<RunOutcome> = outcomes.into();
    let log = std::rc::Rc::clone(log);
    move |_ctx: &mut CallCtx, _frame: &mut CodeState, send: Value, throw: Option<Value>| {
        log.borrow_mut().push((send, throw));
        queue.pop_front().expect("scripted runner ran out")
    }
}

pub(crate) const GEN_SRC: &str = "def g(a, b):\n    yield a\n    yield b\n";
pub(crate) const GEN_RET_SRC: &str = "def g():\n    yield 1\n    return 42\n";
pub(crate) const GEN_DEF_SRC: &str = "def g(a, b=7):\n    yield a\n";

// ── gen_wrap_call ──────────────────────────────────────────────────

#[test]
fn wrap_binds_positional_args() {
    let mut fx = Fixture::new();
    let (gen, frame) = wrap_gen(
        &mut fx,
        GEN_SRC,
        "g",
        &[Value::SmallInt(10), Value::SmallInt(20)],
        &[],
    );
    let ctx = fx.ctx();
    assert_eq!(state_of(&ctx, gen), GenState::New);
    // `setup_code_state` binds positionals into the frame slots.
    assert!(frame.state.iter().any(|s| *s == Some(Value::SmallInt(10))));
    assert!(frame.state.iter().any(|s| *s == Some(Value::SmallInt(20))));
    // n_state = S + 1: the prelude's S is 2 locals (`a`, `b`) and
    // `prelude_sig_decode` already adds the +1
    // (`DECODE_CODESTATE_SIZE`, bc.rs) — so the frame has 3 slots.
    assert_eq!(frame.n_state, 3);
}

#[test]
fn wrap_applies_default_args() {
    let mut fx = Fixture::new();
    let (gen, frame) = wrap_gen(&mut fx, GEN_DEF_SRC, "g", &[Value::SmallInt(3)], &[Value::SmallInt(7)]);
    let ctx = fx.ctx();
    assert_eq!(state_of(&ctx, gen), GenState::New);
    assert!(frame.state.iter().any(|s| *s == Some(Value::SmallInt(3))));
    assert!(frame.state.iter().any(|s| *s == Some(Value::SmallInt(7))));
}

#[test]
fn wrap_arg_count_error() {
    let mut fx = Fixture::new();
    let err = try_wrap_gen(
        &mut fx,
        GEN_SRC,
        "g",
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        &[],
    )
    .unwrap_err();
    assert!(matches!(err, GenError::Setup(crate::runtime::bc::SetupError::ArgCountMismatch)));
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(
        err.setup_message(&fx.pool).as_deref(),
        Some("argument num/types mismatch")
    );
}

#[test]
fn wrap_missing_arg_is_count_error() {
    let mut fx = Fixture::new();
    let err = try_wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1)], &[])
        .unwrap_err();
    assert!(matches!(err, GenError::Setup(crate::runtime::bc::SetupError::ArgCountMismatch)));
}

// ── resume: yields, send, exhaustion ──────────────────────────────

#[test]
fn iternext_yields_then_exhausts_with_return_value() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(
        vec![
            RunOutcome::Yield(Value::SmallInt(1)),
            RunOutcome::Normal(Value::SmallInt(42)),
        ],
        &log,
    );
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(1))
    );
    assert_eq!(state_of(&ctx, gen), GenState::Suspended);
    // Exhaustion: `None` (C11 convention) + return value stashed.
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        None
    );
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
    match gen {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Generator(g) => assert_eq!(g.stop_value, Some(Value::SmallInt(42))),
            _ => panic!("not a generator"),
        },
        _ => panic!("not heap"),
    }
    // The runner saw plain resumes: send None, no throw.
    assert_eq!(*log.borrow(), vec![(Value::None, None), (Value::None, None)]);
}

#[test]
fn send_delivers_values_and_exhaustion_raises_stop() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(
        vec![
            RunOutcome::Yield(Value::SmallInt(1)),
            RunOutcome::Yield(Value::SmallInt(2)),
            RunOutcome::Normal(Value::SmallInt(42)),
        ],
        &log,
    );
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(1))
    );
    assert_eq!(
        gen_instance_send(&mut ctx, gen, &mut frame, &mut runner, Value::SmallInt(10)).unwrap(),
        Some(Value::SmallInt(2))
    );
    // Exhaustion via `send` raises StopIteration(42) (not None).
    let err = gen_instance_send(&mut ctx, gen, &mut frame, &mut runner, Value::SmallInt(20))
        .unwrap_err();
    assert_eq!(err, GenError::StopIteration(Some(Value::SmallInt(42))));
    assert_eq!(err.exc_name(), "StopIteration");
    // The runner saw the send values.
    assert_eq!(
        *log.borrow(),
        vec![
            (Value::None, None),
            (Value::SmallInt(10), None),
            (Value::SmallInt(20), None)
        ]
    );
}

#[test]
fn send_non_none_to_new_is_error_and_leaves_new() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![], &log);
    let err = gen_instance_send(&mut ctx, gen, &mut frame, &mut runner, Value::SmallInt(5))
        .unwrap_err();
    assert_eq!(err, GenError::CantSendNonNone);
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(
        err.message(),
        Some("can't send non-None value to a just-started generator")
    );
    // Guard does not advance the generator or call the runner.
    assert_eq!(state_of(&ctx, gen), GenState::New);
    assert!(log.borrow().is_empty());
}

#[test]
fn send_none_to_new_works_like_next() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Yield(Value::SmallInt(1))], &log);
    assert_eq!(
        gen_instance_send(&mut ctx, gen, &mut frame, &mut runner, Value::None).unwrap(),
        Some(Value::SmallInt(1))
    );
    assert_eq!(*log.borrow(), vec![(Value::None, None)]);
}

#[test]
fn resumed_fast_paths_do_not_call_runner() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let exc = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Normal(Value::SmallInt(42))], &log);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        None
    );
    // Exhausted: next / send / throw all hit the fast path.
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        None
    );
    assert_eq!(
        gen_instance_send(&mut ctx, gen, &mut frame, &mut runner, Value::SmallInt(1))
            .unwrap_err(),
        GenError::StopIteration(None)
    );
    assert_eq!(
        gen_instance_throw(&mut ctx, gen, &mut frame, &mut runner, &[exc]).unwrap_err(),
        GenError::StopIteration(None)
    );
    // The fast paths must not call the runner again: the log keeps
    // exactly the ONE call that exhausted the generator.
    assert_eq!(log.borrow().len(), 1);
}

// ── reentrance + pend_throw ────────────────────────────────────────

#[test]
fn resume_inside_runner_is_already_executing() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let mut ctx = fx.ctx();
    let mut runner = |ctx: &mut CallCtx, frame: &mut CodeState, _send: Value, _throw: Option<Value>| {
        // Re-enter the running generator: the guard must fire
        // before the nested call touches the runner.
        let mut dummy = |_: &mut CallCtx, _: &mut CodeState, _: Value, _: Option<Value>| {
            RunOutcome::Normal(Value::None)
        };
        let nested = gen_resume(ctx, gen, frame, &mut dummy, Value::None, None);
        assert_eq!(nested.unwrap_err(), GenError::AlreadyExecuting);
        RunOutcome::Yield(Value::SmallInt(1))
    };
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(1))
    );
    // The outer resume still completes (Suspended).
    assert_eq!(state_of(&ctx, gen), GenState::Suspended);
}

#[test]
fn pend_throw_overrides_and_returns_previous() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_SRC, "g", &[Value::SmallInt(1), Value::SmallInt(2)], &[]);
    let exc1 = exc_value(&mut fx, "ValueError", vec![]);
    let exc2 = exc_value(&mut fx, "TypeError", vec![]);
    let mut ctx = fx.ctx();
    assert_eq!(
        gen_instance_pend_throw(&mut ctx, gen, exc1).unwrap(),
        Value::None
    );
    assert_eq!(
        gen_instance_pend_throw(&mut ctx, gen, exc2).unwrap(),
        exc1
    );
    // The next resume delivers exc2 (not the passed throw).
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Yield(Value::SmallInt(9))], &log);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        Some(Value::SmallInt(9))
    );
    assert_eq!(*log.borrow(), vec![(Value::None, Some(exc2))]);
    // Consumed: the following resume has no throw.
    let log2 = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner2 = script_runner(vec![RunOutcome::Yield(Value::SmallInt(8))], &log2);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner2).unwrap(),
        Some(Value::SmallInt(8))
    );
    assert_eq!(*log2.borrow(), vec![(Value::None, None)]);
}

#[test]
fn pend_throw_on_closed_is_accepted_but_never_delivered() {
    let mut fx = Fixture::new();
    let (gen, mut frame) = wrap_gen(&mut fx, GEN_RET_SRC, "g", &[], &[]);
    let exc = exc_value(&mut fx, "ValueError", vec![]);
    let mut ctx = fx.ctx();
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner = script_runner(vec![RunOutcome::Normal(Value::None)], &log);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner).unwrap(),
        None
    );
    assert_eq!(state_of(&ctx, gen), GenState::Closed);
    // Accepted (the Zig sets pend_exc even when closed) …
    assert_eq!(
        gen_instance_pend_throw(&mut ctx, gen, exc).unwrap(),
        Value::None
    );
    // … but the fast path never consumes it.
    let log2 = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runner2 = script_runner(vec![], &log2);
    assert_eq!(
        gen_instance_iternext(&mut ctx, gen, &mut frame, &mut runner2).unwrap(),
        None
    );
    assert!(log2.borrow().is_empty());
}
