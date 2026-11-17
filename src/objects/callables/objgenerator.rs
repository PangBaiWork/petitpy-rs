//! Generator objects (Lane C10b).
//!
//! Port of `src/objects/callables/objgenerator.zig` (503 lines,
//! 0 test blocks): `gen_wrap_call`, `mp_obj_gen_resume`,
//! `genResumeAndRaiseImpl`, `gen_instance_iternext` / `send` /
//! `throw` / `close` / `pend_throw` / `print`, the
//! `mp_const_GeneratorExit_obj` singleton (as a caller-supplied
//! value — see `gen_instance_close`), and the ROM types
//! `mp_type_gen_wrap` / `mp_type_gen_instance` (as
//! `gen_wrap_type_data` / `gen_instance_type_data`).
//!
//! # Where the Zig's globals went (C13 seams)
//!
//! The Zig leans on three pieces of VM-global state this lane
//! does not have yet; each is an explicit parameter instead, and
//! the result file lists them for C13:
//!
//! - **The code store.** `FunBcData.code` is an index into the
//!   VM's code store (C13). `gen_wrap_call` therefore takes the
//!   resolved `RawCode`, the module `qstr_table`, the function's
//!   `extra_args` (defaults) and the VM-owned `empty_tuple`
//!   explicitly — exactly what C13 reads out of its store. The
//!   `gen_wrap` type's `call` slot is a stub returning `PyRaise`
//!   (not implemented) until C13 wires it to `gen_wrap_call`.
//! - **The frame.** The Zig gen instance OWNS its `MpCodeState`
//!   inline (flex area); the Rust `GeneratorData` deliberately
//!   does NOT embed C5's `CodeState` (that would be a
//!   `value` ↔ `bc` module cycle — see the `GeneratorData`
//!   docs). `gen_wrap_call` returns the frame next to the
//!   generator handle, and every resume function takes it as a
//!   parameter. The VM owns the frame and associates it with its
//!   generator.
//! - **The runner.** `mp_execute_bytecode` is C13's VM. The
//!   execute step is a `&mut GenRunner` parameter (see below).
//!   The `BuiltinFn` method adapters (`gen_instance_*_builtin`)
//!   cannot take a runner through the frozen `BuiltinFn`
//!   signature, so they are stubs returning `PyRaise` until C13
//!   grows `CallCtx` into the Vm (design doc D5) and wires them
//!   to the `gen_instance_*` implementations.
//! - **The exception types.** There is no `StopIteration` /
//!   `GeneratorExit` type object yet (C12). The PEP 479 check
//!   and the close-path `GeneratorExit` check use
//!   `exc_is_subclass_named` (MRO walk by type NAME — exact
//!   until C12 provides type identity), and `gen_instance_close`
//!   takes the `GeneratorExit` instance as a parameter (the
//!   VM-owned singleton; C12 creates it, C13 passes it).
//!
//! # Rust spellings of Zig idioms
//!
//! - `code_state.ip == 0` (already-stopped) → `GenState::Closed`
//!   (Rust `ip` is a byte offset; 0 is a valid offset after
//!   setup, so it cannot be the marker — `bc.rs` sets `ip` past
//!   the prelude).
//! - `sp == state - 1` (just-started) → `GenState::New`
//!   (Rust's `CodeState` has no `sp`; the VM owns the value
//!   stack — see `bc.rs`).
//! - `pend_exc == OBJ_NULL` (running) → `GenState::Running`;
//!   `pend_exc` otherwise → `GeneratorData.pend_throw`.
//! - `ret_val` + `VM_RETURN_*` → `GenResume { kind, value }`;
//!   the runner's yielded/returned/raised value travels in
//!   `RunOutcome`, so no `sp[0]` / `state[0]` reads are needed.
//! - `makeStopIteration(v)` → `Ok(None)` from `iternext` (the
//!   C11 convention) with the value stashed in
//!   `GeneratorData.stop_value` for C13's `next()`; `send` /
//!   `throw` raise `GenError::StopIteration(value)` instead.
//! - `mp_obj_new_exception_msg` sites → typed `GenError`
//!   variants (the exception object is built by C13 from
//!   `exc_name()` + `message()`; `Raised(Value)` carries an
//!   already-built exception object straight through).

use std::fmt;

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::PrintKind;
use crate::objects::value::{
    DictData, FunBuiltinData, GcRef, GenState, GeneratorData, MapData, Obj, TypeData, Value,
};
use crate::compiler::mpy::RawCode;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::bc::{prelude_sig_decode, setup_code_state, CodeState, SetupError};
use crate::runtime::qstr::RuntimeQstr;

// ── the runner seam ────────────────────────────────────────────────

/// One VM step's outcome (`mp_execute_bytecode`'s `VM_RETURN_*`
/// kind + `ret_val`, `objgenerator.zig:178-255`). The value is the
/// yielded value (`Yield`), the return value (`Normal`) or the
/// exception object (`Exception`) — in the Zig these come from
/// `code_state.sp[0]` / `state_ptr[0]`; in Rust the runner hands
/// them over, because the value stack lives in the VM, not in
/// `CodeState` (see `bc.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunOutcome {
    /// `VM_RETURN_NORMAL` — the generator returned; the value is
    /// its return value (`sp[0]`).
    Normal(Value),
    /// `VM_RETURN_YIELD` — the generator yielded; the value is
    /// the yielded value (`sp[0]`).
    Yield(Value),
    /// `VM_RETURN_EXCEPTION` — the generator raised; the value is
    /// the exception object (`state[0]`).
    Exception(Value),
}

/// The C13 runner seam: one bytecode step of a generator frame.
///
/// `send` is the value to deliver as the pending `yield`
/// expression's result (`code_state.sp[0] = send_value` in the
/// Zig, done before the call there; passed in here because Rust's
/// `CodeState` has no `sp`); `throw` is the exception to inject
/// at the yield point (`mp_execute_bytecode`'s `inject_exc`),
/// `None` for a plain resume. The runner owns the value stack and
/// the globals save/restore (`globalsGet` / `globalsSet` in the
/// Zig) around the step.
///
/// This type is LOCAL to Lane C10b; Lane C10a defines its own
/// seam for `fun_bc_call`. The main agent unifies the two at
/// merge time; C13 is the consumer.
pub type GenRunner<'a> =
    dyn FnMut(&mut CallCtx<'a>, &mut CodeState, Value, Option<Value>) -> RunOutcome + 'a;

/// The resume's outcome (`mp_obj_gen_resume`'s `c_int` return +
/// `*ret_val`, `objgenerator.zig:178`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GenResume {
    pub kind: ResumeKind,
    pub value: Value,
}

/// `VM_RETURN_NORMAL` / `VM_RETURN_YIELD` / `VM_RETURN_EXCEPTION`
/// (`runtime.h`, `objgenerator.zig:105-107`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResumeKind {
    Normal,
    Yield,
    Exception,
}

// ── errors ─────────────────────────────────────────────────────────

/// Errors from the generator operations (the C9a `DictError`
/// shape: `exc_name()` for the VM's pending-exception wiring).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GenError {
    /// The generator raised this exception object
    /// (`genResumeAndRaiseImpl`'s `VM_RETURN_EXCEPTION` branch:
    /// `pending_exc = ret`). C13 puts the value straight into
    /// `pending_exc`; `exc_name()` is `""` because the name
    /// lives on the object itself.
    Raised(Value),
    /// `StopIteration` raised by `send` / `throw` on exhaustion
    /// (`genResumeAndRaiseImpl` with `raise_stop_iteration`).
    /// `None` = bare (the generator returned `None` — the Zig
    /// maps `OBJ_NONE` → `OBJ_NULL`); `Some(v)` = the return
    /// value as the exception's single arg.
    StopIteration(Option<Value>),
    /// `ValueError("generator already executing")` — the
    /// reentrance guard (`pend_exc == OBJ_NULL`).
    AlreadyExecuting,
    /// `TypeError("can't send non-None value to a just-started
    /// generator")`.
    CantSendNonNone,
    /// `RuntimeError("generator ignored GeneratorExit")` —
    /// `close` resumed the generator and it yielded again.
    IgnoredGeneratorExit,
    /// `RuntimeError("generator raised StopIteration")` — PEP 479
    /// (`mp_obj_gen_resume`'s exception branch).
    GeneratorRaisedStopIteration,
    /// `TypeError("argument num/types mismatch")` — the VM's sig
    /// check on the method adapters (`send` 2 args, `throw`
    /// 2..4, `close` 1, `pend_throw` 2, all counting `self`).
    ArgNumTypesMismatch,
    /// A `setup_code_state` failure from `gen_wrap_call` (arg
    /// binding). Use `setup_message(pool)` for the rendered
    /// TERSE text.
    Setup(SetupError),
    /// A slot raised while resuming (data dropped at the boundary
    /// — the accepted C6/C7r convention).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl GenError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            GenError::Raised(_) => "",
            GenError::StopIteration(_) => "StopIteration",
            GenError::AlreadyExecuting => "ValueError",
            GenError::CantSendNonNone => "TypeError",
            GenError::IgnoredGeneratorExit => "RuntimeError",
            GenError::GeneratorRaisedStopIteration => "RuntimeError",
            GenError::ArgNumTypesMismatch => "TypeError",
            GenError::Setup(_) => "TypeError",
            GenError::PyRaise => "",
            GenError::OutOfMemory => "MemoryError",
        }
    }

    /// The TERSE message literal (`None` where the message is
    /// not a literal: `Raised` carries an object, `StopIteration`
    /// carries its arg, `Setup` needs the qstr pool — see
    /// `setup_message`).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            GenError::Raised(_) => None,
            GenError::StopIteration(_) => None,
            GenError::AlreadyExecuting => Some("generator already executing"),
            GenError::CantSendNonNone => {
                Some("can't send non-None value to a just-started generator")
            }
            GenError::IgnoredGeneratorExit => Some("generator ignored GeneratorExit"),
            GenError::GeneratorRaisedStopIteration => {
                Some("generator raised StopIteration")
            }
            GenError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            GenError::Setup(_) => None,
            GenError::PyRaise => None,
            GenError::OutOfMemory => None,
        }
    }

    /// Render a `Setup` error's TERSE message (needs the qstr
    /// pool for the `%q` names). `None` for non-`Setup` errors.
    pub fn setup_message(&self, pool: &RuntimeQstr) -> Option<String> {
        match self {
            GenError::Setup(e) => Some(e.message(pool)),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            GenError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────

/// `mp_obj_is_subclass_fast` for a raised value, by type NAME:
/// walk the value's header type MRO (self first) comparing names.
/// Exact until C12 provides type identity — the MRO walk already
/// handles a fixture-built subclass (an `mro` populated by hand),
/// which is how the PEP 479 / `GeneratorExit` checks are tested.
/// Any non-matching value (including non-exception values) is
/// simply not a subclass.
fn exc_is_subclass_named(arena: &GcArena, exc: Value, name: u32) -> bool {
    let Value::Heap(r) = exc else {
        return false;
    };
    let Some(t) = arena.header(r).type_ else {
        return false;
    };
    let mut chain = vec![t];
    if let Obj::Type(td) = arena.get(t) {
        chain.extend(td.mro.iter().copied());
    }
    chain.iter().any(|&tr| match arena.get(tr) {
        Obj::Type(td) => td.name == name,
        _ => false,
    })
}

/// The generator payload of a value (`None` for anything else —
/// the Zig `std.debug.assert(isExactType(...))` sites; Rust
/// returns `PyRaise` instead of asserting, like `list_iternext`).
fn gen_data<'a>(arena: &'a GcArena, gen: Value) -> Option<&'a GeneratorData> {
    match gen {
        Value::Heap(r) => match arena.get(r) {
            Obj::Generator(g) => Some(g),
            _ => None,
        },
        _ => None,
    }
}

/// Mutable version of `gen_data`.
fn gen_data_mut<'a>(arena: &'a mut GcArena, gen: Value) -> Option<&'a mut GeneratorData> {
    match gen {
        Value::Heap(r) => match arena.get_mut(r) {
            Obj::Generator(g) => Some(g),
            _ => None,
        },
        _ => None,
    }
}

// ── gen_wrap_call ──────────────────────────────────────────────────

/// `gen_wrap_call` (`objgenerator.zig:154-175`): the generating-fn
/// constructor — decode the sig prelude, run `mp_setup_code_state`
/// (C5's `setup_code_state`), allocate the generator instance.
///
/// The Zig reads the bytecode off the `FunBc` object and the def
/// args off its trailing area; the Rust `FunBcData` carries
/// neither (C10a owns that extension), so the caller — C13's Vm,
/// which owns the code store — supplies them: `code` is the
/// function's `RawCode`, `qstr_table` the module's resolved qstr
/// table, `extra_args` the function's default args (creation
/// order: positional defaults, then the defkwargs dict iff the
/// `DEFKWARGS` scope flag is set), `empty_tuple` the VM-owned
/// `mp_const_empty_tuple` singleton. `gen_type` is the header
/// type for the new instance (the `gen_instance` type object).
///
/// Returns the generator handle AND its frame — the frame lives
/// in the VM (see the module docs), not in the arena object.
/// `n_exc_stack` is decoded (the Zig sizes its flex area with
/// it) but unused: Rust's `CodeState` has no exception stack
/// (C5's `setup_code_state_helper` discards it the same way).
pub fn gen_wrap_call(
    ctx: &mut CallCtx,
    fun: GcRef,
    code: &RawCode,
    qstr_table: &[u16],
    extra_args: &[Value],
    empty_tuple: Value,
    gen_type: GcRef,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<(GcRef, CodeState), GenError> {
    let mut cursor: &[u8] = &code.fun_data;
    let sig = prelude_sig_decode(&mut cursor);
    let n_state = sig.n_state;
    let _ = sig.n_exc_stack;
    let frame = setup_code_state(
        fun,
        code,
        qstr_table,
        extra_args,
        empty_tuple,
        n_state,
        n_args,
        n_kw,
        args,
        &mut *ctx.arena,
        &*ctx.roots,
    )
    .map_err(GenError::Setup)?;
    let gen = ctx.alloc(
        Some(gen_type),
        Obj::Generator(GeneratorData {
            fun,
            state: GenState::New,
            pend_throw: None,
            stop_value: None,
        }),
    );
    Ok((gen, frame))
}

// ── mp_obj_gen_resume ──────────────────────────────────────────────

/// `mp_obj_gen_resume` (`objgenerator.zig:178-255`): resume a
/// generator one step.
///
/// - Already-stopped (`GenState::Closed`, the Zig's
///   `code_state.ip == 0`) → optimised `Normal(None)` — the
///   `StopIteration(None)` fast path.
/// - Already executing (`GenState::Running`, the Zig's
///   `pend_exc == OBJ_NULL`) → `AlreadyExecuting`.
/// - Just-started (`GenState::New`, the Zig's `sp == state - 1`)
///   with a non-`None` send → `CantSendNonNone`.
/// - A queued `.pend_throw()` overrides the passed `throw`
///   (the Zig's PEND_THROW branch, full config) and is consumed.
/// - Otherwise the frame is marked `Running` and the `runner`
///   steps it; afterwards the state is `Suspended` (`Yield`) or
///   `Closed` (`Normal` / `Exception` — the Zig's `clearIp`).
/// - PEP 479: an `Exception` outcome whose exception is a
///   `StopIteration` subclass is reported as
///   `GeneratorRaisedStopIteration` instead of the bare
///   `Exception` outcome (the Zig substitutes the
///   `RuntimeError` object in place).
///
/// The globals save/restore around the step
/// (`globalsGet` / `globalsSet`) is the runner's job (it owns
/// the globals), as is applying `send` as the pending `yield`
/// expression's result.
pub fn gen_resume<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
    send: Value,
    throw: Option<Value>,
) -> Result<GenResume, GenError> {
    let state = match gen_data(ctx.arena, gen) {
        Some(g) => g.state,
        None => return Err(GenError::PyRaise),
    };
    if state == GenState::Closed {
        // Already-stopped → optimised StopIteration(None).
        return Ok(GenResume {
            kind: ResumeKind::Normal,
            value: Value::None,
        });
    }
    if state == GenState::Running {
        return Err(GenError::AlreadyExecuting);
    }
    if state == GenState::New && send != Value::None {
        return Err(GenError::CantSendNonNone);
    }
    // PEND_THROW: a queued throw overrides the passed one.
    let throw = match gen_data_mut(ctx.arena, gen) {
        Some(g) => g.pend_throw.take().or(throw),
        None => return Err(GenError::PyRaise),
    };
    // Mark as running.
    if let Some(g) = gen_data_mut(ctx.arena, gen) {
        g.state = GenState::Running;
    }
    let outcome = runner(ctx, frame, send, throw);
    // Mark as not running; Yield suspends, anything else closes.
    let (kind, value) = match outcome {
        RunOutcome::Normal(v) => (ResumeKind::Normal, v),
        RunOutcome::Yield(v) => (ResumeKind::Yield, v),
        RunOutcome::Exception(v) => (ResumeKind::Exception, v),
    };
    if let Some(g) = gen_data_mut(ctx.arena, gen) {
        g.state = if kind == ResumeKind::Yield {
            GenState::Suspended
        } else {
            GenState::Closed
        };
    }
    if kind == ResumeKind::Exception {
        // PEP 479: StopIteration raised inside a generator →
        // RuntimeError (checked here, in `mp_obj_gen_resume`,
        // not in the caller — `objgenerator.zig:238-242`).
        let stop_id = crate::objects::primitives::ops::static_qstr_id(b"StopIteration")
            .expect("StopIteration is a static qstr");
        if exc_is_subclass_named(ctx.arena, value, stop_id) {
            return Err(GenError::GeneratorRaisedStopIteration);
        }
    }
    Ok(GenResume { kind, value })
}

// ── genResumeAndRaiseImpl ──────────────────────────────────────────

/// `genResumeAndRaiseImpl` (`objgenerator.zig:257-276`): the
/// dispatch helper behind `iternext` / `send` / `throw`.
///
/// - `Yield` → the yielded value.
/// - `Exception` → the exception object, raised
///   (`GenError::Raised` — C13 puts it in `pending_exc`).
/// - `Normal` → exhaustion: the return value is stashed in
///   `stop_value` (`None` maps to "no value", the Zig's
///   `OBJ_NONE` → `OBJ_NULL`); `raise_stop_iteration` decides
///   between raising `StopIteration(value)` (`send` / `throw`)
///   and returning `None` = exhausted (`iternext`, the C11
///   convention).
pub fn gen_resume_and_raise<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
    send: Value,
    throw: Option<Value>,
    raise_stop_iteration: bool,
) -> Result<Option<Value>, GenError> {
    let res = gen_resume(ctx, gen, frame, runner, send, throw)?;
    match res.kind {
        ResumeKind::Yield => Ok(Some(res.value)),
        ResumeKind::Exception => Err(GenError::Raised(res.value)),
        ResumeKind::Normal => {
            let stop = if res.value == Value::None {
                None
            } else {
                Some(res.value)
            };
            if let Some(g) = gen_data_mut(ctx.arena, gen) {
                g.stop_value = stop;
            }
            if raise_stop_iteration {
                Err(GenError::StopIteration(stop))
            } else {
                Ok(None)
            }
        }
    }
}

// ── method implementations ─────────────────────────────────────────

/// `gen_instance_iternext` (`objgenerator.zig:278`): `next(gen)`.
/// `None` = exhausted (the C11 `iternext` convention); the return
/// value is stashed in `stop_value` (see `gen_resume_and_raise`).
pub fn gen_instance_iternext<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
) -> Result<Option<Value>, GenError> {
    gen_resume_and_raise(ctx, gen, frame, runner, Value::None, None, false)
}

/// `gen_instance_send` (`objgenerator.zig:282`): `gen.send(v)`.
/// Exhaustion raises `StopIteration(value)` (not `None`).
pub fn gen_instance_send<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
    send_value: Value,
) -> Result<Option<Value>, GenError> {
    gen_resume_and_raise(ctx, gen, frame, runner, send_value, None, true)
}

/// `gen_instance_throw` (`objgenerator.zig:286`): `gen.throw(type[,
/// value[, traceback]])` — `args` are the user args AFTER `self`
/// (1..=3). The exception is `args[0]`, unless a second arg is
/// present and not `None`, in which case THAT is the exception
/// (the Zig passes it straight through as `throw_value`; the VM's
/// inject path validates it — a non-exception becomes
/// `TypeError("exceptions must derive from BaseException")`,
/// probed on the reference binary). The traceback arg is ignored,
/// exactly as in the Zig.
pub fn gen_instance_throw<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
    args: &[Value],
) -> Result<Option<Value>, GenError> {
    if args.is_empty() || args.len() > 3 {
        return Err(GenError::ArgNumTypesMismatch);
    }
    let mut exc = args[0];
    if args.len() > 1 && args[1] != Value::None {
        exc = args[1];
    }
    gen_resume_and_raise(ctx, gen, frame, runner, Value::None, Some(exc), true)
}

/// `genInstanceCloseImpl` (`objgenerator.zig:295-314`): `gen.close()`.
/// Resumes with `GeneratorExit` injected (`generator_exit` is the
/// VM-owned singleton instance — C12 creates it, C13 passes it;
/// this lane has no exception types yet).
///
/// - `Yield` → the generator ignored the exit:
///   `RuntimeError("generator ignored GeneratorExit")`.
/// - `Exception` → swallow iff the raised exception is a
///   `GeneratorExit` (subclass check by name — see
///   `exc_is_subclass_named`); re-raise anything else.
/// - `Normal` → successful close, `None`.
///
/// A `Normal` resume on an already-closed or never-started
/// generator returns `None` without running the body (probed:
/// `close()` on a new generator runs neither the body nor its
/// `finally`).
pub fn gen_instance_close<'a>(
    ctx: &mut CallCtx<'a>,
    gen: Value,
    frame: &mut CodeState,
    runner: &mut GenRunner<'a>,
    generator_exit: Value,
) -> Result<Value, GenError> {
    let res = gen_resume(ctx, gen, frame, runner, Value::None, Some(generator_exit))?;
    match res.kind {
        ResumeKind::Yield => Err(GenError::IgnoredGeneratorExit),
        ResumeKind::Exception => {
            let ge_id = crate::objects::primitives::ops::static_qstr_id(b"GeneratorExit")
                .expect("GeneratorExit is a static qstr");
            if exc_is_subclass_named(ctx.arena, res.value, ge_id) {
                Ok(Value::None)
            } else {
                Err(GenError::Raised(res.value))
            }
        }
        ResumeKind::Normal => Ok(Value::None),
    }
}

/// `genInstancePendThrowImpl` (`objgenerator.zig:324-331`):
/// `gen.pend_throw(exc)` (full config only — the nano locals
/// dict omits it). Queues `exc` as the throw for the NEXT resume
/// (it overrides any throw passed to that resume) and returns the
/// previously queued value (`None` if there was none). A running
/// generator rejects it (`AlreadyExecuting`); a closed one
/// accepts it silently (the next resume hits the already-stopped
/// fast path first and never consumes it — same as the Zig).
pub fn gen_instance_pend_throw(
    ctx: &mut CallCtx,
    gen: Value,
    exc: Value,
) -> Result<Value, GenError> {
    let g = match gen_data_mut(ctx.arena, gen) {
        Some(g) => g,
        None => return Err(GenError::PyRaise),
    };
    if g.state == GenState::Running {
        return Err(GenError::AlreadyExecuting);
    }
    let prev = g.pend_throw.replace(exc);
    Ok(prev.unwrap_or(Value::None))
}

// ── print ──────────────────────────────────────────────────────────

/// `gen_instance_print` (`objgenerator.zig:337-347`): kind ignored
/// (`_ = kind`), `"<generator object 'name' at 0x…>"`. The name
/// is the function's name qstr; the address is the arena index
/// in hex — the Zig prints the raw object pointer, which is
/// non-deterministic across runs, so no test can pin it; the
/// arena index is the Rust `id()` model (C0) and keeps the
/// shape. A non-generator value prints nothing.
pub fn gen_instance_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn fmt::Write,
) {
    let (fun, r) = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Generator(g) => (g.fun, r),
            _ => return,
        },
        _ => return,
    };
    let name = match ctx.arena.get(fun) {
        // `Qstr = u16`; `FunBcData.name` is a u32 qstr id — the
        // same `as u16` narrowing `bc.rs` uses for qstr-table
        // indices (`.mpy` qstr indices are u16 by format).
        Obj::FunBc(f) => ctx.pool.qstr_str(f.name as u16),
        _ => b"?",
    };
    let _ = write!(
        sink,
        "<generator object '{}' at 0x{:x}>",
        String::from_utf8_lossy(name),
        r.0
    );
}

// ── ROM types ──────────────────────────────────────────────────────

/// `gen_wrap_call` as the `gen_wrap` type's `call` slot — STUB:
/// the call needs C13's code store to resolve the `FunBc`'s
/// bytecode (see the module docs), so until C13 wires it this
/// returns `PyRaise` (not implemented, the C6 slot convention).
/// The real constructor is `gen_wrap_call` (explicit
/// code/qstr-table/extra-args parameters).
pub fn gen_wrap_call_slot(
    ctx: &mut CallCtx,
    _self: Value,
    _n_args: usize,
    _n_kw: usize,
    _args: &[Value],
) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// `mp_type_gen_wrap` (`objgenerator.zig:443-475`): the
/// generating-function type — flags `0x0020`
/// (`MP_TYPE_FLAG_BINDS_SELF`), name `generator`, slots attr
/// (`mp_obj_fun_bc_attr`, C10a's — `TypeData` has no attr slot
/// yet) and call (`gen_wrap_call`). `dict` is the caller-owned
/// locals dict.
pub fn gen_wrap_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"generator")
            .expect("generator is a static qstr"),
        flags: 0x0020,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: Some(gen_wrap_call_slot),
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    }
}

/// `mp_type_gen_instance` (`objgenerator.zig:477-510`): the
/// generator-instance type — flags `0x0080`
/// (`MP_TYPE_FLAG_ITER_IS_ITERNEXT`), name `generator`, slots
/// print (`gen_instance_print`), iter (`gen_instance_iternext` —
/// a generator is its own iterator, so `getiter` returns it
/// unchanged), locals_dict (filled by
/// `gen_instance_locals_dict`). `dict` is the caller-owned
/// locals dict.
pub fn gen_instance_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"generator")
            .expect("generator is a static qstr"),
        flags: 0x0080,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: Some(|_ctx: &mut CallCtx, self_: Value| Ok(self_)),
        subscr: None,
        buffer: None,
        print: Some(gen_instance_print),
    }
}

// ── BuiltinFn adapters (stubs until C13) ─────────────────────────

/// Adapt a method implementation to the frozen `BuiltinFn` shape.
/// The real implementations need a runner (and, for `close`, the
/// `GeneratorExit` singleton) that only the VM has; until C13
/// grows `CallCtx` into the Vm and wires these, they check the
/// arity (the VM's sig check — the reference binary raises
/// `TypeError("argument num/types mismatch")` for every wrong
/// count, probed 2026-11-17) and then return `PyRaise` (not
/// implemented). The full implementations are `gen_instance_send`
/// / `gen_instance_throw` / `gen_instance_close` /
/// `gen_instance_pend_throw` (explicit runner parameters).
macro_rules! gen_builtin_stub {
    ($name:ident, $min:expr, $max:expr) => {
        pub fn $name(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
            let _ = ctx;
            if args.len() < $min || args.len() > $max {
                return Err(PyError::PyRaise);
            }
            Err(PyError::PyRaise)
        }
    };
}

gen_builtin_stub!(gen_instance_send_builtin, 2, 2);
gen_builtin_stub!(gen_instance_throw_builtin, 2, 4);
gen_builtin_stub!(gen_instance_close_builtin, 1, 1);
gen_builtin_stub!(gen_instance_pend_throw_builtin, 2, 2);

/// `gen_instance_locals_dict` (`objgenerator.zig:386-441`): the
/// instance method table — `send` (fixed 2), `throw` (var 2..4),
/// `close` (fixed 1), `pend_throw` (fixed 2, full config only).
/// The `n_args` encoding is `fun_n_args(min, max)`
/// (`(min << 8) | max`, C7). Objects are allocated with a `None`
/// header type — the bootstrap pattern (the dict/builtin-function
/// types do not exist yet; cf. `int_locals_dict`).
pub fn gen_instance_locals_dict(ctx: &mut CallCtx) -> GcRef {
    let mut entries: Vec<(Value, Value)> = Vec::with_capacity(4);
    for (name, n_args, func) in [
        (
            b"send" as &[u8],
            fun_n_args(2, 2),
            gen_instance_send_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"throw" as &[u8],
            fun_n_args(2, 4),
            gen_instance_throw_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"close" as &[u8],
            fun_n_args(1, 1),
            gen_instance_close_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"pend_throw" as &[u8],
            fun_n_args(2, 2),
            gen_instance_pend_throw_builtin as crate::objects::slots::BuiltinFn,
        ),
    ] {
        let id = crate::objects::primitives::ops::static_qstr_id(name)
            .unwrap_or_else(|| panic!("{} is a static qstr", String::from_utf8_lossy(name)));
        let fun = ctx.alloc(
            None,
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args,
                func,
                // Fixed-arity builtins: no keyword parameters (C10a).
                kw_func: None,
            }),
        );
        entries.push((Value::Qstr(id), Value::Heap(fun)));
    }
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(entries),
        }),
    )
}
