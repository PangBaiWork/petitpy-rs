//! Bytecode + built-in functions (`objfun.zig`, 532 lines).
//!
//! Ports, in order:
//! - `fun_builtin_0/1/2/3/var_call` (`objfun.zig:72-100`): the
//!   `FunBuiltin` call adapters. Fully live — they call the Rust
//!   `BuiltinFn` / `BuiltinKwFn` pointers directly.
//! - `mp_obj_fun_get_name` (`:167`): in the Zig this decodes the
//!   name from the bytecode prelude + the module's qstr table on
//!   every call. The Rust `FunBcData` stores the *resolved* qstr
//!   id (C13's loader decodes it once, from the prelude index
//!   through the module qstr table), so `fun_bc_get_name` is a
//!   field read.
//! - `mp_obj_new_fun_bc` (`:204-241`): builds a `FunBc` with its
//!   trailing extra args (positional defaults unpacked from the
//!   `def_args[0]` tuple, keyword-defaults dict from `def_args[1]`).
//! - `fun_bc_call` (`:315-380`): prelude decode, code-state
//!   allocation, `mp_setup_code_state`, then `mp_execute_bytecode`
//!   (`:~366`), globals save/restore, result handling. The VM run
//!   is C13's: this lane ports it as `fun_bc_prepare_call`
//!   (everything up to the execute line — directly testable) plus
//!   `fun_bc_call` with the execute step behind a **runner seam**
//!   (see `RunOutcome` below).
//! - `fun_bc_print` (`:453`): `"<function NAME at 0xADDR>"`
//!   (`CPYTHON_COMPAT`), kind ignored.
//! - `mp_obj_fun_bc_attr` (`:465`): `__name__` / `__globals__`
//!   only (`FUNCTION_ATTRS`); `__code__` is dead under both
//!   configs, everything else is not found.
//!
//! ## Runner seam (for C13)
//!
//! The Zig calls `mp_execute_bytecode(code_state, 0)` — a global
//! VM entry point. Rust has no VM global yet, so the execute step
//! is a parameter: `runner: &mut dyn FnMut(&mut CallCtx, &mut
//! CodeState) -> RunOutcome`. The runner returns the outcome; the
//! *value* travels in the outcome (not in `CodeState.sp[0]` as in
//! the Zig — the Rust `CodeState` (`bc.rs`) has no value-stack
//! pointer; the VM manages the stack). `RunOutcome::NeedsVm` is
//! the seam's "I cannot run this" answer: the `fun_bc_call_slot`
//! adapter (the `CallFn` registered on the type) passes a null
//! runner that always answers `NeedsVm`, because the slot has no
//! `RawCode` / qstr table / empty-tuple singleton to prepare with
//! — those live in C13's code store. C13's dispatcher calls
//! `fun_bc_call` directly with the resolved code and the real
//! runner, or threads the runner into `CallCtx` (design doc D5:
//! "C13 grows CallCtx into the Vm") and replaces the slot adapter.
//!
//! ## Deferred to C13 (with Zig refs)
//!
//! - The `mp_execute_bytecode` step itself (`objfun.zig:~366`).
//! - Globals save/restore: `code_state.old_globals =
//!   globalsGet()` (`:350`) and `globalsSet(old_globals)`
//!   (`:~368`). The Rust `CodeState` has no `old_globals` field
//!   (`bc.rs`: "`sp` and `old_globals` live outside this struct").
//! - `FunBcData.context` (the module object) is filled by C13's
//!   loader; `__globals__` reads through it.
//!
//! ## Deliberate divergences
//!
//! - State allocation: the Zig uses a fixed 88-byte stack buffer
//!   for the common case and heap-allocates (with a `MemoryError`
//!   on heap failure) for larger frames (`:325-343`). Rust has no
//!   dynamic stack allocation; the state is a `Vec` inside
//!   `CodeState`, and an allocation failure aborts (Rust alloc
//!   failure) instead of raising `MemoryError`. Corpus-invisible
//!   (same class as the Zig's own heap-fail note).
//! - The print address is the arena slot index (`GcRef.0`), not a
//!   heap pointer: the original's address is a per-run link
//!   artifact (probed `<function f at 0x79d9227ffd60>`), and no
//!   test can pin it. Format is identical.

use crate::compiler::mpy::RawCode;
use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{FLAG_BINDS_SELF, FLAG_BUILTIN_FUN};
use crate::objects::slots::{AttrFn, CallFn, PrintKind};
use crate::objects::value::{FunBcData, FunBuiltinData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::qstr_table::Qstr;
use crate::runtime::bc::{prelude_sig_decode, setup_code_state, CodeState, SetupError};

// ── arity check (`mp_arg_check_num_sig`, `argcheck.zig`) ────────

/// `mp_arg_check_num_sig` (`argcheck.zig:44-58`, `pyerr.bridge`
/// wrapper at `:116`): the Zig sets `pending_exc` and returns —
/// the *polled-only* model. The adapters below are the terminal
/// callers of the check (they return without calling the function
/// when it fails), so returning the failure directly is exactly
/// the Zig behaviour. The check itself is a pure predicate so the
/// boundary table is unit-testable.
///
/// `sig` decoding (Zig): `takes_kw = sig & 1`, `n_min = sig >>
/// 17`, `n_max = (sig >> 1) & 0xffff`. The Rust `FunBuiltinData.
/// n_args` uses C7's packing (`fun_n_args`, `objint.rs:277`):
/// `(min << 8) | max`; `takes_kw` is `kw_func.is_some()` (see
/// `FunBuiltinData`). All failures are the single terse error
/// (`TypeError: argument num/types mismatch` — the adapters drop
/// the message at the slot boundary per the C6 convention; the
/// predicate is what the tests pin).
pub fn arg_check_num(n_args: usize, n_kw: usize, min: u16, max: u16, takes_kw: bool) -> bool {
    if n_kw != 0 && !takes_kw {
        return false;
    }
    let n_args = n_args as u16;
    // The Zig branches on `min == max` (`:52-58`); the two branches
    // are the same predicate, written once here.
    n_args >= min && n_args <= max
}

/// Decode C7's `fun_n_args` packing (`objint.rs:277`):
/// `(min << 8) | max`.
pub fn fun_sig_range(n_args: u16) -> (u16, u16) {
    (n_args >> 8, n_args & 0xff)
}

/// Read a `FunBuiltinData` out of a heap `Value` (the adapters'
/// `self`). `None` when the value is not a heap builtin function —
/// the Zig's `fixedSelf`/`varSelf` would read garbage; Rust
/// returns `PyRaise` instead (caller-contract guard).
fn builtin_data(arena: &GcArena, self_value: Value) -> Option<FunBuiltinData> {
    match self_value {
        Value::Heap(r) => match arena.get(r) {
            Obj::FunBuiltin(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ── `fun_builtin_*_call` (`objfun.zig:72-100`) ───────────────────

/// `fun_builtin_0_call` (`objfun.zig:72-76`): `argCheckNum(n_args,
/// n_kw, 0, 0)`, then call the 0-arg fn pointer.
pub fn fun_builtin_0_call(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    _args: &[Value],
) -> PyResult<Value> {
    let data = builtin_data(ctx.arena, self_value).ok_or(PyError::PyRaise)?;
    if !arg_check_num(n_args, n_kw, 0, 0, false) {
        return Err(PyError::PyRaise);
    }
    (data.func)(ctx, &[])
}

/// `fun_builtin_1_call` (`objfun.zig:79-83`).
pub fn fun_builtin_1_call(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = builtin_data(ctx.arena, self_value).ok_or(PyError::PyRaise)?;
    if !arg_check_num(n_args, n_kw, 1, 1, false) {
        return Err(PyError::PyRaise);
    }
    (data.func)(ctx, &args[..1])
}

/// `fun_builtin_2_call` (`objfun.zig:86-90`).
pub fn fun_builtin_2_call(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = builtin_data(ctx.arena, self_value).ok_or(PyError::PyRaise)?;
    if !arg_check_num(n_args, n_kw, 2, 2, false) {
        return Err(PyError::PyRaise);
    }
    (data.func)(ctx, &args[..2])
}

/// `fun_builtin_3_call` (`objfun.zig:93-97`).
pub fn fun_builtin_3_call(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = builtin_data(ctx.arena, self_value).ok_or(PyError::PyRaise)?;
    if !arg_check_num(n_args, n_kw, 3, 3, false) {
        return Err(PyError::PyRaise);
    }
    (data.func)(ctx, &args[..3])
}

/// `fun_builtin_var_call` (`objfun.zig:100-113`): the sig check
/// runs first (`mp_arg_check_num_sig`); on success the fn is
/// called with the positionals only (no-kw builtins) or with the
/// positionals + the fixed-table kw map (kw builtins — the Rust
/// form is the `&[(u32, Value)]` pairs slice of `BuiltinKwFn`).
/// `kw_func.is_some()` is the takes_kw bit (see `FunBuiltinData`).
/// This is the var adapter *and* the sig-dispatch used by the
/// bound-method / closure calls (which hold only the `Value`, not
/// the builtin's type — dispatching by sig is observably identical
/// to dispatching by the Zig's per-type call slot: the check is
/// purely the `(min, max, takes_kw)` predicate and the call passes
/// `args[..n_args]`).
pub fn fun_builtin_var_call(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    call_fun_builtin(ctx, self_value, n_args, n_kw, args)
}

/// Call a `FunBuiltin` value, dispatching on its sig
/// (`n_args` range + `kw_func.is_some()` as takes_kw) — the
/// Rust form of `mp_call_function_n_kw` restricted to builtin
/// functions (C13 owns the general dispatcher, which looks the
/// call slot up on the value's type). Used by
/// `bound_meth_call` / `npy_clos_fn_call`, which prepend
/// `self` / the closed-over cells and then call the underlying
/// fun without knowing its builtin type.
pub fn call_fun_builtin(
    ctx: &mut CallCtx,
    func_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = builtin_data(ctx.arena, func_value).ok_or(PyError::PyRaise)?;
    let (min, max) = fun_sig_range(data.n_args);
    let takes_kw = data.kw_func.is_some();
    if !arg_check_num(n_args, n_kw, min, max, takes_kw) {
        return Err(PyError::PyRaise);
    }
    match data.kw_func {
        Some(kw_func) => {
            // Kw pairs follow the positionals in `args` (`bc.rs`:
            // "n_args positionals followed by 2*n_kw (name, value)
            // pairs; names are Value::Qstr immediates"). A non-qstr
            // name is a caller-contract violation; the setup helper
            // tolerates it as id 0 (`bc.rs`), so do we.
            let pairs: Vec<(u32, Value)> = (0..n_kw)
                .map(|i| {
                    let name = args[n_args + 2 * i].as_qstr().unwrap_or(0);
                    (name, args[n_args + 2 * i + 1])
                })
                .collect();
            kw_func(ctx, &args[..n_args], &pairs)
        }
        None => (data.func)(ctx, &args[..n_args]),
    }
}

// ── `mp_obj_fun_get_name` (`objfun.zig:167-187`) ─────────────────

/// The function's name qstr id. The Zig decodes it from the
/// bytecode prelude + the module's qstr table on every call; the
/// Rust `FunBcData` stores the resolved id (C13's loader fills
/// it), so this is a field read. `None` when the value is not a
/// heap `FunBc` (the Zig would read garbage; Rust reports
/// absence).
pub fn fun_bc_get_name(arena: &GcArena, fun: Value) -> Option<u32> {
    match fun {
        Value::Heap(r) => match arena.get(r) {
            Obj::FunBc(d) => Some(d.name),
            _ => None,
        },
        _ => None,
    }
}

// ── `mp_obj_new_fun_bc` (`objfun.zig:204-241`) ───────────────────

/// Build a bytecode function object.
///
/// `def_pos_tuple` is `def_args[0]` (the tuple of positional
/// defaults, or `None`); `def_kw` is `def_args[1]` (the
/// keyword-defaults dict, or `None`). The tuple is unpacked here,
/// mirroring the Zig's `@memcpy` of the tuple items into the
/// trailing extra args. A non-tuple `def_pos_tuple` is a
/// caller-contract violation (the compiler only ever passes a
/// tuple or NULL); it is treated as "no defaults" rather than
/// reading garbage. `type_` is the `fun_bc` type object (the
/// caller supplies it — C12 owns the type registry); `code` is
/// the index into C13's code store; `context` is the module
/// object (`None` until C13's loader fills it).
pub fn new_fun_bc(
    ctx: &mut CallCtx,
    type_: GcRef,
    name: u32,
    n_args: u16,
    code: usize,
    def_pos_tuple: Option<Value>,
    def_kw: Option<Value>,
    context: Option<GcRef>,
) -> GcRef {
    let def_pos = match def_pos_tuple {
        Some(Value::Heap(r)) => match ctx.arena.get(r) {
            Obj::Tuple(items) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    ctx.alloc(
        Some(type_),
        Obj::FunBc(FunBcData {
            name,
            n_args,
            code,
            def_pos,
            def_kw,
            context,
        }),
    )
}

// ── `fun_bc_call`: prepare + runner seam (`objfun.zig:315-380`) ──

/// What the VM runner reports for one `mp_execute_bytecode` step
/// (`objfun.zig:~366`). The value travels in the outcome: the
/// Zig reads it from `code_state.sp[0]` (NORMAL) or
/// `code_state.statePtr()[0]` (EXCEPTION), but the Rust
/// `CodeState` (`bc.rs`) has no value-stack pointer — the VM
/// manages the stack, so the runner returns the value directly.
/// `NeedsVm` is the seam's "I cannot run this" answer (see the
/// module docs); the real VM runner never returns it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunOutcome {
    /// Normal return; the value is the call's result.
    Normal(Value),
    /// Exception return; the value is the exception object (the
    /// Zig sets `pending_exc` to it — dropped at the slot boundary
    /// until C13, per the C6 convention).
    Exception(Value),
    /// The runner cannot execute (no VM wired yet).
    NeedsVm,
}

/// `fun_bc_call` up to the `mp_execute_bytecode` line
/// (`objfun.zig:315-354`): decode the sig prelude (`n_state` from
/// `S + 1`), build the default-args `extra_args` (positional
/// defaults, then the defkwargs dict iff present — `setup_code_state`
/// reads the trailing dict only when the `DEFKWARGS` scope flag is
/// set), and bind the arguments via `setup_code_state` (`bc.rs`).
/// The Zig's `code_state.old_globals = globalsGet()` (`:350`) and
/// the setup polled check (`:353-357`) are folded in: a setup
/// failure is the `Err` return (the Zig returns NULL with
/// `pending_exc` set — the same polled-sentinel shape, one layer
/// up). The state allocation itself is a `Vec` inside `CodeState`
/// (see the module docs for the stack-buffer divergence).
/// Panics when `fun` is not a heap `FunBc` (caller-contract
/// violation — the Zig's `fromObj` would read garbage; cf.
/// `dict_entries_mut` in `bc.rs`).
pub fn fun_bc_prepare_call(
    ctx: &mut CallCtx,
    fun: GcRef,
    code: &RawCode,
    qstr_table: &[u16],
    empty_tuple: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<CodeState, SetupError> {
    let data = match ctx.arena.get(fun) {
        Obj::FunBc(d) => d.clone(),
        _ => panic!("fun_bc_prepare_call on a non-FunBc object"),
    };
    // Sig prelude decode (`MP_BC_PRELUDE_SIG_DECODE_INTO`,
    // `objfun.zig:282-311`): only `n_state` and `n_exc_stack`
    // are read here. `n_exc_stack` sizes the Zig's state buffer
    // only — the Rust `CodeState` has no exc-stack storage, so
    // it is decoded and discarded (as C5's setup does).
    let mut cursor: &[u8] = &code.fun_data;
    let sig = prelude_sig_decode(&mut cursor);
    let _ = sig.n_exc_stack;
    let n_state = sig.n_state;
    // `extra_args`: positional defaults, then the defkwargs dict
    // (`bc.rs` `setup_code_state` docs).
    let mut extra_args: Vec<Value> = data.def_pos.clone();
    if let Some(kw) = data.def_kw {
        extra_args.push(kw);
    }
    setup_code_state(
        fun,
        code,
        qstr_table,
        &extra_args,
        empty_tuple,
        n_state,
        n_args,
        n_kw,
        args,
        ctx.arena,
        ctx.roots,
    )
}

/// The full `fun_bc_call` (`objfun.zig:315-380`) with the execute
/// step behind the runner seam. `code` / `qstr_table` /
/// `empty_tuple` are resolved by the caller (C13's dispatcher:
/// the code index in `FunBcData` → `RawCode`, the module's
/// resolved qstr table, the VM-owned empty-tuple singleton).
/// Globals save/restore (`objfun.zig:350, ~368`) is C13's — the
/// call runs in the caller's globals. `RunOutcome::Exception`
/// returns `PyRaise` (the exception value is dropped at the slot
/// boundary until C13 adds `pending_exc`, C6 convention).
pub fn fun_bc_call(
    ctx: &mut CallCtx,
    self_value: Value,
    code: &RawCode,
    qstr_table: &[u16],
    empty_tuple: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    runner: &mut dyn FnMut(&mut CallCtx, &mut CodeState) -> RunOutcome,
) -> PyResult<Value> {
    let fun = match self_value {
        Value::Heap(r) => r,
        _ => return Err(PyError::PyRaise),
    };
    let mut cs = fun_bc_prepare_call(ctx, fun, code, qstr_table, empty_tuple, n_args, n_kw, args)
        .map_err(|_| PyError::PyRaise)?;
    match runner(ctx, &mut cs) {
        RunOutcome::Normal(v) => Ok(v),
        RunOutcome::Exception(_) => Err(PyError::PyRaise),
        RunOutcome::NeedsVm => Err(PyError::PyRaise),
    }
}

/// The `CallFn` registered on `mp_type_fun_bc`. A null runner:
/// the slot has no `RawCode` / qstr table / empty-tuple singleton
/// to prepare with (those live in C13's code store), so it always
/// reports the call as not-yet-runnable. C13 replaces this wiring
/// when the code store lands (see the module docs).
pub fn fun_bc_call_slot(
    _ctx: &mut CallCtx,
    _self_value: Value,
    _n_args: usize,
    _n_kw: usize,
    _args: &[Value],
) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

// ── `fun_bc_print` (`objfun.zig:453-461`) ────────────────────────

/// `"<function NAME at 0xADDR>"` (`CPYTHON_COMPAT`). `kind` is
/// ignored (`_ = kind` in the Zig — `str(f)` prints the same as
/// `repr(f)`, probed 2026-11-17). The address is the arena slot
/// index (see the module docs); the name resolves through the
/// qstr pool. Prints nothing when the value is not a heap `FunBc`
/// (unreachable via the slot dispatcher).
pub fn fun_bc_print(ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let (name, addr) = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBc(d) => (d.name, r.0),
            _ => return,
        },
        _ => return,
    };
    let name_bytes = ctx.pool.qstr_str(name as Qstr);
    let name_str = String::from_utf8_lossy(name_bytes);
    let _ = write!(sink, "<function {name_str} at 0x{addr:x}>");
}

// ── `mp_obj_fun_bc_attr` (`objfun.zig:465-478`) ──────────────────

/// Attribute load on a bytecode function. `__name__` → the name
/// qstr; `__globals__` → the module's globals dict (read through
/// `FunBcData.context` — `None` until C13's loader fills it, in
/// which case the attribute is not found); anything else →
/// `Ok(None)` (not found — the Zig leaves `dest[0]` NULL).
/// `__code__` is dead under both configs (the Zig's branch is
/// behind a comptime gate). Store/delete is not handled here (the
/// Zig returns early when `dest[0] != NULL`); the caller raises
/// `AttributeError` (probed: `f.__name__ = "x"` → `AttributeError`,
/// 2026-11-17).
pub fn fun_bc_attr(ctx: &mut CallCtx, self_value: Value, attr: u32) -> PyResult<Option<Value>> {
    let data = match self_value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBc(d) => d.clone(),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    // `__name__` / `__globals__` qstr ids (static — always present).
    let name_id = crate::objects::primitives::ops::static_qstr_id(b"__name__")
        .expect("__name__ is a static qstr");
    let globals_id = crate::objects::primitives::ops::static_qstr_id(b"__globals__")
        .expect("__globals__ is a static qstr");
    if attr == name_id {
        return Ok(Some(Value::Qstr(data.name)));
    }
    if attr == globals_id {
        if let Some(module) = data.context {
            if let Obj::Module(m) = ctx.arena.get(module) {
                return Ok(Some(Value::Heap(m.dict)));
            }
        }
        return Ok(None);
    }
    Ok(None)
}

// ── `mp_load_method_maybe` on a function (shared helper) ─────────

/// Look an attribute up on a function value the way
/// `mp_load_method_maybe` does for `bound_meth_attr` /
/// `mp_obj_closure_attr` (`objboundmeth.zig:57-62`,
/// `objclosure.zig:54-58`): the function's type attr slot, then —
/// for these types, nothing else (both `mp_type_fun_bc` and the
/// builtin-fun types have no locals dict). A `FunBc` answers via
/// `fun_bc_attr`; a `FunBuiltin` has no attr slot at all (probed:
/// `[].append.__name__` → `AttributeError`, 2026-11-17), so it
/// never answers. Other values answer nothing.
pub fn load_attr_on_fun(ctx: &mut CallCtx, fun: Value, attr: u32) -> PyResult<Option<Value>> {
    match fun {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBc(_) => fun_bc_attr(ctx, fun, attr),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

// ── type data (ROM singletons, `objfun.zig:405-451, 482-531`) ─────

/// `builtinFunType(call_fn)` (`objfun.zig:405-425`): flags
/// `FLAG_BINDS_SELF | FLAG_BUILTIN_FUN`, name `function`, only
/// the call slot filled (`slot_index_call = 1`, all others 0).
fn builtin_fun_type_data(dict: GcRef, call: CallFn) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"function")
            .expect("function is a static qstr"),
        flags: FLAG_BINDS_SELF | FLAG_BUILTIN_FUN,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: Some(call),
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: None,
        bases: None,
    }
}

/// `mp_type_fun_builtin_0`.
pub fn fun_builtin_0_type_data(dict: GcRef) -> TypeData {
    builtin_fun_type_data(dict, fun_builtin_0_call)
}

/// `mp_type_fun_builtin_1`.
pub fn fun_builtin_1_type_data(dict: GcRef) -> TypeData {
    builtin_fun_type_data(dict, fun_builtin_1_call)
}

/// `mp_type_fun_builtin_2`.
pub fn fun_builtin_2_type_data(dict: GcRef) -> TypeData {
    builtin_fun_type_data(dict, fun_builtin_2_call)
}

/// `mp_type_fun_builtin_3`.
pub fn fun_builtin_3_type_data(dict: GcRef) -> TypeData {
    builtin_fun_type_data(dict, fun_builtin_3_call)
}

/// `mp_type_fun_builtin_var`.
pub fn fun_builtin_var_type_data(dict: GcRef) -> TypeData {
    builtin_fun_type_data(dict, fun_builtin_var_call)
}

/// `mp_type_fun_bc` (`objfun.zig:482-531`): flags
/// `FLAG_BINDS_SELF`, name `function`, slots print=1, attr=2,
/// call=3 (`slot_index_*` in the Zig's `MpObjTypeFull`).
/// `attr` is `fun_bc_attr` (wired by C12a — the brief's attr-slot
/// item); `call` is the null-runner slot adapter (see
/// `fun_bc_call_slot`).
pub fn fun_bc_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"function")
            .expect("function is a static qstr"),
        flags: FLAG_BINDS_SELF,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: Some(fun_bc_call_slot),
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn std::fmt::Write| {
                fun_bc_print(ctx, value, kind, sink)
            },
        ),
        attr: Some(fun_bc_attr),
        make_new: None,
        bases: None,
    }
}

/// The load-only attr function for bytecode functions, as the
/// `AttrFn` slot type (`slots.rs`) — for C12 to register when
/// `TypeData` gains its `attr` field.
pub const FUN_BC_ATTR: AttrFn = fun_bc_attr;
