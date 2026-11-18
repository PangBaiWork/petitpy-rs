//! The VM's call dispatch — the `runtime.zig` ports the
//! dispatch calls: `mp_call_function_n_kw` /
//! `mp_call_method_n_kw` (`runtime.zig:811-886`),
//! `mp_call_prepare_args_n_kw_var` (`:896-1048`), and the
//! `next` / `__build_class__` builtins. Split out of
//! `vm_ops.rs` (which keeps the exception builders and
//! `make_function` / `make_closure`).
//!
//! Raise convention: every helper here returns
//! `Result<Value, Value>` — `Err(exc)` carries the exception
//! OBJECT (built via `exc_from_typed` / `exc_from_pyerror` /
//! `exc_from_gen_error` from `vm_ops`).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::{call_fun_builtin, fun_bc_prepare_call};
use crate::objects::callables::objgenerator::{gen_wrap_call, GenError};
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::{exception_make_new, is_exception_type};
use crate::objects::objtype::typecreate::type_call;
use crate::objects::primitives::objcell::cell_set;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, GcRef, GenState, MapData, Obj, Value};
use crate::runtime::runner::RunOutcome;
use crate::runtime::vm::VmState;
use crate::runtime::vm_gen::{close_generator_ctx, resume_generator_value_ctx};
use crate::runtime::vm_ops::{exc_from_gen_error, exc_from_pyerror, exc_from_setup, exc_from_typed, type_error};
use crate::runtime::vm_raise::execute_frame;
use crate::runtime::vm_rt_name::load_method_maybe_ctx;

// ── Call dispatch ────────────────────────────────────────────────────

/// `mp_call_function_n_kw` (`runtime.zig:811-834`): call a value.
/// Dispatches on the value's kind BEFORE the type call slot —
/// the `FunBc`/`gen_wrap`/`FunBuiltin`/`BoundMeth`/`Closure`
/// payloads are handled here (their type call slots are the
/// null stubs from C10a/C10b); exception types go through
/// `exception_make_new`; any other value with a type call slot
/// uses it (C12a's `type_call`); otherwise `TypeError("object
/// not callable")`.
pub fn call_function_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if let Some(r) = crate::runtime::vm_call_intercept::intercept_builtin_call(
        ctx, state, fun, n_args, n_kw, args,
    ) {
        return r;
    }
    match fun {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBc(_) => call_fun_bc_ctx(ctx, state, r, n_args, n_kw, args),
            Obj::FunBuiltin(_) => call_fun_builtin_ctx(ctx, state, r, n_args, n_kw, args),
            Obj::BoundMeth(d) => {
                let (func, self_) = (d.func, d.self_);
                let mut full: Vec<Value> = Vec::with_capacity(args.len() + 1);
                full.push(self_);
                full.extend_from_slice(args);
                call_function_ctx(ctx, state, func, n_args + 1, n_kw, &full)
            }
            Obj::Closure(d) => {
                let (func, cells) = (d.fun, d.cells.clone());
                let n_cells = cells.len();
                let mut full: Vec<Value> = Vec::with_capacity(args.len() + n_cells);
                for cell in cells {
                    full.push(Value::Heap(cell));
                }
                full.extend_from_slice(args);
                call_function_ctx(ctx, state, Value::Heap(func), n_args + n_cells, n_kw, &full)
            }
            Obj::Type(_) => call_type_ctx(ctx, state, r, n_args, n_kw, args),
            // An instance tries its `__call__` first, with the
            // VM's real call hook (E2); no `__call__` falls
            // through to the type's call slot (which raises
            // "object not callable" for instances).
            _ => match crate::runtime::vm_seams::try_instance_call(
                ctx, state, fun, n_args, n_kw, args,
            ) {
                Ok(Some(v)) => Ok(v),
                Ok(None) => call_via_type_slot_ctx(ctx, state, fun, n_args, n_kw, args),
                Err(exc) => Err(exc),
            },
        },
        // Non-heap values (small ints, bools, …) have no
        // `__call__`, but route through the same try for
        // uniformity — it returns `None` for non-instances.
        _ => match crate::runtime::vm_seams::try_instance_call(
            ctx, state, fun, n_args, n_kw, args,
        ) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => call_via_type_slot_ctx(ctx, state, fun, n_args, n_kw, args),
            Err(exc) => Err(exc),
        },
    }
}

/// A `FunBc` call: a `gen_wrap`-typed function creates a
/// generator (`gen_wrap_call`); a plain one prepares the frame
/// and executes it (globals switched to the function's module
/// globals, restored afterwards — the Zig's `mp_execute_bytecode`
/// globals save/restore).
fn call_fun_bc_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: GcRef,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let code_idx = match ctx.arena.get(fun) {
        Obj::FunBc(d) => d.code,
        _ => unreachable!("call_fun_bc on a non-FunBc"),
    };
    // The function's module globals (its context) — `None`
    // means "keep the current globals" (test-built functions).
    let module_globals = match ctx.arena.get(fun) {
        Obj::FunBc(d) => d.context.and_then(|m| match ctx.arena.get(m) {
            Obj::Module(md) => Some(md.dict),
            _ => None,
        }),
        _ => None,
    };
    let is_gen_wrap = match obj_get_type(ctx.arena, ctx.types, Value::Heap(fun)) {
        Some(t) => t == state.vm_types.gen_wrap,
        None => false,
    };
    let empty_tuple = state.empty_tuple;
    if is_gen_wrap {
        let gen_type = state.vm_types.gen_instance;
        // E2: a generator function called with missing defaulted
        // args panicked in `setup_code_state` (`extra_args` index
        // out of bounds) — the wrap passed `&[]` instead of the
        // `FunBc`'s defaults. The plain path (`fun_bc_prepare_call`)
        // builds `extra_args` from `def_pos` (+ `def_kw`); the wrap
        // must do the same (`count(10)`, `islice([..], 2)`).
        let extra_args: Vec<Value> = match ctx.arena.get(fun) {
            Obj::FunBc(d) => {
                let mut v = d.def_pos.clone();
                if let Some(kw) = d.def_kw {
                    v.push(kw);
                }
                v
            }
            _ => Vec::new(),
        };
        // The prepare borrows the code store (shared) while the
        // error mapping borrows the state (mutable) — so the
        // `gen_wrap_call` runs in a block whose borrows end
        // before the `map_err`.
        let wrapped = {
            let code = state.code_store.code(code_idx);
            let qstr_table = state.code_store.qstr_table(code_idx);
            gen_wrap_call(
                ctx,
                fun,
                code,
                qstr_table,
                &extra_args,
                empty_tuple,
                gen_type,
                n_args,
                n_kw,
                args,
            )
        };
        let (gen, frame) =
            wrapped.map_err(|e| exc_from_gen_error(ctx, state, e))?;
        state.frames.insert(gen, frame);
        return Ok(Value::Heap(gen));
    }
    let prepared = {
        let code = state.code_store.code(code_idx);
        let qstr_table = state.code_store.qstr_table(code_idx);
        fun_bc_prepare_call(
            ctx, fun, code, qstr_table, empty_tuple, n_args, n_kw, args,
        )
    };
    let mut frame =
        prepared.map_err(|e| exc_from_setup(ctx, state, e))?;
    let old_globals = state.globals;
    if let Some(g) = module_globals {
        state.globals = g;
    }
    let outcome = execute_frame(ctx, state, &mut frame, Value::None, None);
    state.globals = old_globals;
    match outcome {
        RunOutcome::Normal(v) => Ok(v),
        RunOutcome::Yield(_) => {
            // A plain function cannot yield — the compiler only
            // emits YIELD in generator code (scope flag 0x01),
            // which takes the `gen_wrap` branch above.
            unreachable!("YIELD from a non-generator FunBc");
        }
        RunOutcome::Exception(exc) => Err(exc),
    }
}

/// A `FunBuiltin` call: the generator-instance method stubs are
/// intercepted by fn-pointer identity (they need the VM's
/// resume logic, which a bare `BuiltinFn` cannot carry);
/// everything else goes through `call_fun_builtin`.
fn call_fun_builtin_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: GcRef,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let (name, func) = match ctx.arena.get(fun) {
        Obj::FunBuiltin(d) => (d.name, d.func),
        _ => unreachable!("call_fun_builtin_vm on a non-FunBuiltin"),
    };
    // Generator-instance methods (`send` / `throw` / `close` /
    // `pend_throw`) are intercepted by NAME + a generator first
    // arg, NOT by function-pointer identity: the four
    // `gen_instance_*_builtin` stubs are byte-identical bodies
    // from one macro, so the release linker folds them (ICF) and
    // `fn_addr_eq` reports all four as equal — the first check
    // (`send`) then wins for every call, misrouting `throw` /
    // `close` as a plain `send` (the generator resumes normally
    // and exhausts instead of raising). The name is the method's
    // static qstr (`gen_instance_locals_dict`); the generator
    // check keeps the match from catching a same-named builtin
    // on another type.
    if let Some(gen) = args.first().copied() {
        if matches!(gen, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Generator(_))) {
            use crate::objects::primitives::ops::static_qstr_id;
            let via = if Some(name) == static_qstr_id(b"send") {
                Some(ResumeVia::Send)
            } else if Some(name) == static_qstr_id(b"throw") {
                Some(ResumeVia::Throw)
            } else if Some(name) == static_qstr_id(b"close") {
                Some(ResumeVia::Close)
            } else if Some(name) == static_qstr_id(b"pend_throw") {
                Some(ResumeVia::PendThrow)
            } else {
                None
            };
            if let Some(via) = via {
                return resume_via_method_ctx(ctx, state, args, via);
            }
        }
    }
    let _ = func;
    call_fun_builtin(ctx, Value::Heap(fun), n_args, n_kw, args)
        .map_err(|e| exc_from_pyerror(ctx, state, e))
}

/// Which generator-instance method was intercepted.
enum ResumeVia {
    Send,
    Throw,
    Close,
    PendThrow,
}

/// Route an intercepted generator-instance method call to the
/// VM's resume logic. `args[0]` is the generator (the bound
/// method prepends `self`); the remaining args are the method's
/// own args (`send(v)` / `throw(t, v?)` / `close()` /
/// `pend_throw(t, v?)`).
fn resume_via_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    args: &[Value],
    via: ResumeVia,
) -> Result<Value, Value> {
    let gen = args.first().copied().unwrap_or(Value::None);
    match via {
        ResumeVia::Send => {
            let send = args.get(1).copied().unwrap_or(Value::None);
            resume_generator_value_ctx(ctx, state, gen, send, None)
        }
        ResumeVia::Throw => {
            // `throw(type, value?)`: the second non-None arg IS
            // the exception (C10b). Pass it RAW — the Zig passes
            // `throw_value` straight through (`mp_resume`,
            // runtime.zig:1430-1453: a sub-iterator's `throw`
            // method receives the raw value; `mp_make_raise_obj`
            // is only the no-`throw`-method fallback, which
            // `resume_vm_ctx` already does). The conversion
            // happens at the injection point (`dispatch_loop`:
            // `make_raise_obj` when the frame does NOT sit on
            // `YIELD_FROM`). Converting here double-converts:
            // `g.throw(123)` into a `yield from Iter()` handed
            // the `throw` method a `TypeError` object instead
            // of `123` (E2b gen_yield_from_throw3).
            let exc = args.get(1).copied().unwrap_or(Value::None);
            let exc = if args.get(2).copied().unwrap_or(Value::None) != Value::None {
                args.get(2).copied().unwrap()
            } else {
                exc
            };
            resume_generator_value_ctx(ctx, state, gen, Value::None, Some(exc))
        }
        ResumeVia::Close => {
            let gen_exit = state.exc.generator_exit;
            let exit = crate::objects::objtype::objexcept_types::generator_exit_instance(
                ctx, gen_exit,
            );
            // `close_generator_ctx` (`gen_instance_close`):
            // a yield is `RuntimeError("generator ignored
            // GeneratorExit")`; a raise is swallowed iff it is
            // a `GeneratorExit`; normal is a successful close.
            // (NOT `resume_generator_value_ctx` — that is the
            // send/throw form, which propagates the raise.)
            match close_generator_ctx(ctx, state, gen, exit) {
                Ok(v) => Ok(v),
                Err(e) => Err(exc_from_gen_error(ctx, state, e)),
            }
        }
        ResumeVia::PendThrow => {
            // `pend_throw(type, value?)` queues a throw for the
            // next resume and returns the PREVIOUSLY queued value
            // (`genInstancePendThrowImpl`,
            // objgenerator.zig:324-331: `prev = self.pend_exc;
            // self.pend_exc = exc_in; return prev;` — initially
            // `OBJ_NONE`). Three E2b fixes against the old arm,
            // which always returned `None`: (1) return the
            // previous value; (2) `pend_throw(None)` CLEARS the
            // queue (the Zig stores `OBJ_NONE` and the resume
            // ignores it — storing the `make_raise_obj(None)`
            // TypeError instead made the resume throw it);
            // (3) a running generator raises `ValueError
            // ("generator already executing")` (the Zig's
            // `pend_exc == OBJ_NULL` guard; the direct fn
            // `gen_instance_pend_throw` already checks this).
            let exc = args.get(1).copied().unwrap_or(Value::None);
            let exc = if args.get(2).copied().unwrap_or(Value::None) != Value::None {
                args.get(2).copied().unwrap()
            } else {
                exc
            };
            // The Running check first (immutable read), then the
            // swap. Store the RAW value (`genInstancePendThrowImpl`:
            // `self.pend_exc = exc_in`) — the conversion happens
            // at the injection point (`dispatch_loop`:
            // `make_raise_obj` when the frame does NOT sit on
            // `YIELD_FROM`; `resume_vm_ctx` passes the raw value
            // to a sub-iterator's `throw` method). Storing the
            // converted object double-converts for `yield from`
            // (same bug as the old `Throw` arm above).
            let Value::Heap(r) = gen else {
                return Err(type_error(ctx, state, "pend_throw on a non-generator"));
            };
            let is_gen = matches!(ctx.arena.get(r), Obj::Generator(_));
            if !is_gen {
                return Err(type_error(ctx, state, "pend_throw on a non-generator"));
            }
            if matches!(ctx.arena.get(r), Obj::Generator(d) if d.state == GenState::Running) {
                return Err(exc_from_gen_error(ctx, state, GenError::AlreadyExecuting));
            }
            let queued = if exc != Value::None { Some(exc) } else { None };
            match ctx.arena.get_mut(r) {
                Obj::Generator(d) => {
                    let prev = std::mem::replace(&mut d.pend_throw, queued);
                    Ok(prev.unwrap_or(Value::None))
                }
                _ => Err(type_error(ctx, state, "pend_throw on a non-generator")),
            }
        }
    }
}

/// A type call (C13b): every non-exception type goes through
/// C12a's `type_call` (`typecreate.rs:183`) — it dispatches on
/// the type's `make_new` slot and raises `CantCreateInstance`
/// ("can't create instance") when there is none. Exception
/// types keep their direct path (`exception_make_new` — their
/// `make_new` slot is the same fn, but the direct call
/// preserves the pre-C13b error mapping, pinned by `vm_tests`).
pub(crate) fn call_type_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    type_: GcRef,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if is_exception_type(ctx.arena, Value::Heap(type_), state.exc.base_exception) {
        return exception_make_new(ctx, type_, args, n_kw)
            .map_err(|e| exc_from_typed(ctx, state, e));
    }
    // E2: the type call carries the VM's real call hook, so a
    // Python `__new__` / `__init__` actually executes (a `FunBc`
    // callee runs through `call_function_ctx` + `execute_frame`).
    // E2b: the hook is the `VmHook` pair (call + resume), built
    // once from `state` — a generator argument to a constructor
    // (`list(gen())`) resumes through the same hook. The hook
    // stashes the exception OBJECT on the `state.pending_exc`
    // side channel (the C14 contract: `PyError` cannot carry an
    // object) and returns `PyRaise`; the drain below takes it
    // first, else falls back to the recorded `pending_raise`
    // via `exc_from_pyerror`.
    let mut hook = crate::runtime::vm_seams::make_hook(state);
    type_call(ctx, Value::Heap(type_), n_args, n_kw, args, Some(&mut hook)).map_err(|e| {
        if let Some(exc) = state.pending_exc.take() {
            exc
        } else {
            exc_from_pyerror(ctx, state, e)
        }
    })
}

/// The fallback for values with no payload dispatch: the value's
/// TYPE's call slot, else `TypeError("object not callable")`.
fn call_via_type_slot_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    let type_ = obj_get_type(ctx.arena, ctx.types, fun);
    let call = type_.and_then(|t| match ctx.arena.get(t) {
        Obj::Type(d) => d.call,
        _ => None,
    });
    match call {
        Some(f) => f(ctx, fun, n_args, n_kw, args)
            .map_err(|e| exc_from_pyerror(ctx, state, e)),
        None => Err(type_error(ctx, state, "object not callable")),
    }
}

// ── `next` ───────────────────────────────────────────────────────────

/// The `next` builtin (`mp_builtin_next`): `next(it)` raises
/// `StopIteration` on exhaustion; `next(it, default)` returns
/// the default. Intercepted by GcRef identity in
/// `call_function` (its `func` is a stub). E2b: the 1-arg form
/// goes through `iternext_allow_raise_ctx` (not
/// `iternext_vm_ctx`) — the translating form swallows the
/// generator's return value into a bare `StopIteration`, but
/// `next(g)` on a generator that `return`s 42 must raise
/// `StopIteration(42)` (the `generator_return` corpus test:
/// `yield from` reads the value through this path). The 2-arg
/// form keeps the translating form (`Ok(None)` → default).
pub(crate) fn call_next_ctx(ctx: &mut CallCtx, state: &mut VmState, n_args: usize, args: &[Value]) -> Result<Value, Value> {
    if n_args == 0 || n_args > 2 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    let it = args[0];
    if n_args == 1 {
        return crate::runtime::vm_rt_iter::iternext_allow_raise_ctx(ctx, state, it);
    }
    match crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)? {
        Some(v) => Ok(v),
        None => Ok(args[1]),
    }
}

/// `__build_class__` (`modbuiltins.zig:532-571`) — intercepted
/// by identity in `call_function_ctx` (like `next`): a bare
/// `BuiltinFn` cannot run the body (it needs the VM's frame
/// execution + the locals override), so the stub body is
/// unreachable through the VM.
/// Protocol: fresh class-locals dict → save/restore `locals`
/// around the body call (`mp_locals_set`) → resolve the
/// metaclass (`type` with no bases, else the type of the first
/// base) → meta-call `meta(name, tuple(bases), locals)` →
/// `__class__` cell set → PEP 487 `__init_subclass__` (full
/// only — we are full config). Keyword args are ignored (the
/// Zig drops them — `metaclass=` included). A raise anywhere
/// propagates (the Zig returns the new class with
/// `pending_exc` set; the Rust returns the `Err`).
pub(crate) fn call_build_class_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    _n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    // The sig check (`fun_n_args(2, 255)`, the Zig's
    // `varFun(2, FUN_ARGS_MAX)`) runs before the interception,
    // but never index blindly on the VM path.
    let Some(&body) = args.first() else {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    };
    let Some(&name) = args.get(1) else {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    };
    // Fresh class-locals dict, rooted across the body call and
    // the meta-call (it is only reachable from `state.locals`
    // until the new type stores it). Header is the dict type
    // (the Zig's `mp_obj_new_dict(0)`) — `mp_obj_new_type`
    // rejects a locals value whose type is not a dict.
    let class_dict = ctx.alloc(
        Some(ctx.types.dict),
        Obj::Dict(DictData { map: MapData::new() }),
    );
    ctx.roots.push(Value::Heap(class_dict));
    // Unrooting removes exactly the pushed entry (the
    // `mp_obj_new_type` pattern) — the frame roots around it
    // are untouched.
    let unroot = |ctx: &mut CallCtx, r: GcRef| {
        if let Some(pos) = ctx.roots.extra.iter().rposition(|v| *v == Value::Heap(r)) {
            ctx.roots.extra.remove(pos);
        }
    };
    let old_locals = state.locals;
    state.locals = Some(class_dict);
    let cell = call_function_ctx(ctx, state, body, 0, 0, &[]);
    state.locals = old_locals;
    let cell = match cell {
        Ok(v) => v,
        Err(e) => {
            unroot(ctx, class_dict);
            return Err(e);
        }
    };
    // Resolve the metaclass (`modbuiltins.zig:552-555`).
    let meta = if n_args == 2 {
        Value::Heap(ctx.types.type_)
    } else {
        match obj_get_type(ctx.arena, ctx.types, args[2]) {
            Some(t) => Value::Heap(t),
            None => {
                unroot(ctx, class_dict);
                return Err(type_error(ctx, state, "argument num/types mismatch"));
            }
        }
    };
    let bases = Value::Heap(new_tuple(ctx, ctx.types.tuple, &args[2..n_args]));
    let meta_args = [name, bases, Value::Heap(class_dict)];
    let new_class = match call_function_ctx(ctx, state, meta, 3, 0, &meta_args) {
        Ok(v) => v,
        Err(e) => {
            unroot(ctx, class_dict);
            return Err(e);
        }
    };
    let Value::Heap(new_class_ref) = new_class else {
        unroot(ctx, class_dict);
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    };
    // The new type holds the class dict now; root the class
    // across the cell set + `__init_subclass__` (both can
    // allocate), then unroot the dict (reachable via the class).
    ctx.roots.push(new_class);
    unroot(ctx, class_dict);
    // `__class__` cell (`modbuiltins.zig:566-568`): the body's
    // return value is the cell, or `None`. A non-cell return
    // cannot come from compiled code; it is ignored.
    if let Value::Heap(r) = cell {
        if let Obj::Cell(_) = ctx.arena.get(r) {
            cell_set(ctx.arena, r, new_class);
        }
    }
    // PEP 487 (`modbuiltins.zig:574-588`): the first base with
    // an `__init_subclass__` is called with the new class.
    // The Zig calls `dest[0]` with one arg and ignores
    // `dest[1]` — the Rust does the same.
    let init_subclass_q = static_qstr_id(b"__init_subclass__").unwrap_or(u32::MAX);
    for base in &args[2..n_args] {
        let (method, _self_) = match load_method_maybe_ctx(ctx, state, *base, init_subclass_q)
        {
            Ok(pair) => pair,
            Err(e) => {
                unroot(ctx, new_class_ref);
                return Err(e);
            }
        };
        if let Some(m) = method {
            if let Err(e) = call_function_ctx(ctx, state, m, 1, 0, &[new_class]) {
                unroot(ctx, new_class_ref);
                return Err(e);
            }
            break;
        }
    }
    unroot(ctx, new_class_ref);
    Ok(new_class)
}

// ── `call_method` ────────────────────────────────────────────────────

/// `mp_call_method_n_kw` (`runtime.zig:838-845`): call a
/// (method, self-or-NULL) pair from `LOAD_METHOD`. `adjust` is 1
/// when self is present — the call args are `[self, arg1, …]`.
pub fn call_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    method: Value,
    self_: Option<Value>,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    match self_ {
        Some(s) => {
            let mut full: Vec<Value> = Vec::with_capacity(args.len() + 1);
            full.push(s);
            full.extend_from_slice(args);
            call_function_ctx(ctx, state, method, n_args + 1, n_kw, &full)
        }
        None => call_function_ctx(ctx, state, method, n_args, n_kw, args),
    }
}
