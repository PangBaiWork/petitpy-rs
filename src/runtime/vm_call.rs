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

use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::callables::objfun::{call_fun_builtin, fun_bc_prepare_call};
use crate::objects::callables::objgenerator::gen_wrap_call;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::{exception_make_new, is_exception_type, new_exception};
use crate::objects::objtype::typecreate::type_call;
use crate::objects::primitives::objcell::cell_set;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, GcRef, MapData, Obj, Value};
use crate::runtime::runner::RunOutcome;
use crate::runtime::vm::VmState;
use crate::runtime::vm_gen::{close_generator_ctx, resume_generator_value_ctx};
use crate::runtime::vm_ops::{exc_from_gen_error, exc_from_pyerror, exc_from_setup, exc_from_typed, type_error};
use crate::runtime::vm_raise::{execute_frame, make_raise_obj};
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
    // The `next` builtin is intercepted by GcRef identity (it
    // needs the VM's `iternext` seams — see `call_next`).
    if fun == state.next_builtin {
        return call_next_ctx(ctx, state, n_args, args);
    }
    // The `eval`/`exec`/`compile` builtins are intercepted the
    // same way (C15a): they need the VM's globals/locals and
    // code registration, which a `FunBuiltin` slot cannot see.
    // (`eval` is `EvalInput`, `exec` is `FileInput` —
    // `compile_glue`'s mode table; the two differ only in
    // whether the body value is returned.)
    if fun == state.eval_builtin {
        return crate::runtime::builtinevex::call_eval_exec_ctx(
            ctx,
            state,
            InputKind::EvalInput,
            n_args,
            n_kw,
            args,
        );
    }
    if fun == state.exec_builtin {
        return crate::runtime::builtinevex::call_eval_exec_ctx(
            ctx,
            state,
            InputKind::FileInput,
            n_args,
            n_kw,
            args,
        );
    }
    if fun == state.compile_builtin {
        return crate::runtime::builtinevex::call_compile_ctx(ctx, state, n_args, n_kw, args);
    }
    // `__build_class__` is intercepted the same way (C13b: it needs
    // the VM's frame execution + the locals override — see
    // `call_build_class_ctx`).
    if fun == state.build_class_builtin {
        return call_build_class_ctx(ctx, state, n_args, n_kw, args);
    }
    // The `__import__` builtin is intercepted the same way
    // (C15b): it needs the VM's module registry + `sys.path`,
    // which a `FunBuiltin` slot cannot see.
    if fun == state.import_builtin {
        return crate::runtime::builtinimport::call_import_ctx(
            ctx, state, n_args, n_kw, args,
        );
    }
    // The `help` builtin is intercepted the same way (C18b):
    // the `"modules"` path needs the VM's module registries,
    // which a `FunBuiltin` slot cannot see.
    if fun == state.help_builtin {
        return crate::runtime::builtinhelp::call_help_ctx(
            ctx, state, n_args, n_kw, args,
        );
    }
    // The D1 stateful builtins are intercepted the same way
    // (`builtin_vm.rs`): `globals` / `locals` need the VM's
    // dicts; `dir` / `sorted` / `min` / `max` / `getattr` /
    // `setattr` / `hasattr` / `delattr` / `iter` / `all` /
    // `any` / `sum` need the dispatch loop's iteration seams
    // and the ability to call user functions.
    if fun == state.globals_builtin {
        return crate::runtime::builtin_vm::call_globals_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.locals_builtin {
        return crate::runtime::builtin_vm::call_locals_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.dir_builtin {
        return crate::runtime::builtin_vm::call_dir_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.sorted_builtin {
        return crate::runtime::builtin_vm::call_sorted_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.min_builtin {
        return crate::runtime::builtin_vm::call_min_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.max_builtin {
        return crate::runtime::builtin_vm::call_max_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.getattr_builtin {
        return crate::runtime::builtin_vm::call_getattr_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.setattr_builtin {
        return crate::runtime::builtin_vm::call_setattr_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.hasattr_builtin {
        return crate::runtime::builtin_vm::call_hasattr_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.delattr_builtin {
        return crate::runtime::builtin_vm::call_delattr_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.iter_builtin {
        return crate::runtime::builtin_vm::call_iter_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.all_builtin {
        return crate::runtime::builtin_vm::call_all_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.any_builtin {
        return crate::runtime::builtin_vm::call_any_ctx(ctx, state, n_args, n_kw, args);
    }
    if fun == state.sum_builtin {
        return crate::runtime::builtin_vm::call_sum_ctx(ctx, state, n_args, n_kw, args);
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
            _ => call_via_type_slot_ctx(ctx, state, fun, n_args, n_kw, args),
        },
        _ => call_via_type_slot_ctx(ctx, state, fun, n_args, n_kw, args),
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
                &[],
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
            // the exception (C10b) — the VM inject path
            // validates it (`exceptions must derive from
            // BaseException`).
            let exc = args.get(1).copied().unwrap_or(Value::None);
            let exc = if args.get(2).copied().unwrap_or(Value::None) != Value::None {
                args.get(2).copied().unwrap()
            } else {
                exc
            };
            let exc = {
                make_raise_obj(ctx, state, exc)
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
            // `pend_throw(type, value?)` stores the throw for
            // the next resume (C10b's `gen_instance_pend_throw`).
            let exc = args.get(1).copied().unwrap_or(Value::None);
            let exc = if args.get(2).copied().unwrap_or(Value::None) != Value::None {
                args.get(2).copied().unwrap()
            } else {
                exc
            };
            let exc = make_raise_obj(ctx, state, exc);
            match gen {
                Value::Heap(r) => match ctx.arena.get_mut(r) {
                    Obj::Generator(d) => {
                        d.pend_throw = Some(exc);
                        Ok(Value::None)
                    }
                    _ => {
                        Err(type_error(ctx, state, "pend_throw on a non-generator"))
                    }
                },
                _ => {
                    Err(type_error(ctx, state, "pend_throw on a non-generator"))
                }
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
fn call_type_ctx(
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
    type_call(ctx, Value::Heap(type_), n_args, n_kw, args)
        .map_err(|e| exc_from_pyerror(ctx, state, e))
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
/// `call_function` (its `func` is a stub).
fn call_next_ctx(ctx: &mut CallCtx, state: &mut VmState, n_args: usize, args: &[Value]) -> Result<Value, Value> {
    if n_args == 0 || n_args > 2 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    let it = args[0];
    match crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)? {
        Some(v) => Ok(v),
        None => match args.get(1).copied() {
            Some(d) => Ok(d),
            None => Err(new_exception(ctx, state.exc.stop_iteration)),
        },
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
fn call_build_class_ctx(
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

/// `mp_call_method_n_kw_var` (`runtime.zig:1048-1054` +
/// `mp_call_prepare_args_n_kw_var`, `:896-1046`): a call with
/// `*args` / `**kwargs` unpacking. `args` is the raw stack
/// slice starting at the fun: `[fun, (self?), pos..., (key,
/// value) pairs..., star_mask]`, where a kw key of `None`
/// (MP_OBJ_NULL) marks a `**` dict unpacking and `star_mask`
/// is a small-int bitmask over the positionals (bit i set ⟹
/// positional i is a `*` unpacking). The expanded positionals
/// and kwargs are built in a fresh `Vec` (the Zig precomputes
/// the size for one `m_malloc`; Rust grows instead), then the
/// call goes through `call_function_ctx` / `call_method_ctx`.
/// A non-tuple/list `*` arg is iterated (`getiter` +
/// `iternext`); a non-dict `**` arg goes through its `keys`
/// method + `__getitem__` per key. A `**` key that is not a
/// string raises `TypeError("keywords must be strings")`.
pub fn call_method_var_kw_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    have_self: bool,
    n_args_n_kw: usize,
    args: &[Option<Value>],
) -> Result<Value, Value> {
    use crate::objects::obj::obj_get_type;
    let fun = args.first().copied().flatten().unwrap_or(Value::None);
    let mut rest = &args[1..];
    let self_ = if have_self {
        let s = rest.first().copied().flatten();
        rest = &rest[1..];
        s
    } else {
        None
    };
    let n_args = n_args_n_kw & 0xff;
    let n_kw = (n_args_n_kw >> 8) & 0xff;
    let star_mask = match rest.get(n_args + 2 * n_kw).copied().flatten() {
        Some(Value::SmallInt(m)) => m,
        _ => 0,
    };
    let mut expanded: Vec<Value> = Vec::new();
    let mut expanded_kw: Vec<(u32, Value)> = Vec::new();
    // Positional args — star bits expand.
    for i in 0..n_args {
        let arg = rest.get(i).copied().flatten().unwrap_or(Value::None);
        if star_mask & (1i64 << i) != 0 {
            let t = obj_get_type(ctx.arena, ctx.types, arg);
            if t == Some(ctx.types.tuple) {
                if let Value::Heap(r) = arg {
                    if let Obj::Tuple(items) = ctx.arena.get(r) {
                        expanded.extend_from_slice(items);
                    }
                }
            } else if t == Some(ctx.types.list) {
                if let Value::Heap(r) = arg {
                    if let Obj::List(d) = ctx.arena.get(r) {
                        expanded.extend_from_slice(&d.items);
                    }
                }
            } else {
                let it = crate::runtime::vm_rt_iter::getiter_vm_ctx(ctx, state, arg)?;
                while let Some(v) =
                    crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)?
                {
                    expanded.push(v);
                }
            }
        } else {
            expanded.push(arg);
        }
    }
    // Keyword pairs — a `None` key is a `**` unpacking.
    for i in 0..n_kw {
        let key = rest.get(n_args + i * 2).copied().flatten();
        let value = rest
            .get(n_args + i * 2 + 1)
            .copied()
            .flatten()
            .unwrap_or(Value::None);
        match key {
            None => {
                let t = obj_get_type(ctx.arena, ctx.types, value);
                if t == Some(ctx.types.dict) {
                    if let Value::Heap(r) = value {
                        // Collect the entries first: interning
                        // a key needs `&mut ctx`, which cannot
                        // run while the arena is borrowed by
                        // the entry iteration.
                        let entries: Vec<(Value, Value)> =
                            match ctx.arena.get(r) {
                                Obj::Dict(d) => d
                                    .map
                                    .entries
                                    .iter()
                                    .filter_map(|e| e.key.map(|k| (k, e.value)))
                                    .collect(),
                                _ => Vec::new(),
                            };
                        for (k, v) in entries {
                            let q = intern_kw_key(ctx, state, k)?;
                            expanded_kw.push((q, v));
                        }
                    }
                } else {
                    let keys_q =
                        crate::objects::primitives::ops::static_qstr_id(b"keys").unwrap_or(u32::MAX);
                    let getitem_q = crate::objects::primitives::ops::static_qstr_id(b"__getitem__")
                        .unwrap_or(u32::MAX);
                    let (keys_meth, keys_self) =
                        crate::runtime::vm_rt_name::load_method_ctx(ctx, state, value, keys_q)?;
                    let keys = call_method_ctx(ctx, state, keys_meth, keys_self, 0, 0, &[])?;
                    let it = crate::runtime::vm_rt_iter::getiter_vm_ctx(ctx, state, keys)?;
                    while let Some(k) =
                        crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)?
                    {
                        let q = intern_kw_key(ctx, state, k)?;
                        let (get_meth, get_self) = crate::runtime::vm_rt_name::load_method_ctx(
                            ctx, state, value, getitem_q,
                        )?;
                        let v = call_method_ctx(ctx, state, get_meth, get_self, 1, 0, &[k])?;
                        expanded_kw.push((q, v));
                    }
                }
            }
            Some(Value::Qstr(q)) => expanded_kw.push((q, value)),
            Some(k) => {
                let q = intern_kw_key(ctx, state, k)?;
                expanded_kw.push((q, value));
            }
        }
    }
    // Rebuild the flat call layout: positionals, then
    // interleaved (name, value) pairs.
    let mut flat: Vec<Value> = expanded;
    let n_pos = flat.len();
    for (q, v) in &expanded_kw {
        flat.push(Value::Qstr(*q));
        flat.push(*v);
    }
    let n_kw2 = expanded_kw.len();
    match self_ {
        Some(s) => call_method_ctx(ctx, state, fun, Some(s), n_pos, n_kw2, &flat),
        None => call_function_ctx(ctx, state, fun, n_pos, n_kw2, &flat),
    }
}

/// Intern a `**` kwarg key to a qstr id: a `Qstr` passes
/// through, a `Str` is looked up in the pool, anything else
/// raises `TypeError("keywords must be strings")`. The pool is
/// immutable through `CallCtx` (the C0 design), so a `Str` key
/// that was never interned cannot be created here — it raises
/// instead of being silently dropped (the Zig interns it; a
/// `&mut` pool on `CallCtx` would close the gap).
fn intern_kw_key(ctx: &mut CallCtx, state: &VmState, k: Value) -> Result<u32, Value> {
    match k {
        Value::Qstr(q) => Ok(q),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => {
                let bytes = s.bytes.clone();
                let q = ctx.pool.find_strn(&bytes);
                if q != 0 {
                    Ok(q as u32)
                } else {
                    Err(type_error(
                        ctx,
                        state,
                        "keyword argument name is not an interned string",
                    ))
                }
            }
            _ => Err(type_error(ctx, state, "keywords must be strings")),
        },
        _ => Err(type_error(ctx, state, "keywords must be strings")),
    }
}

