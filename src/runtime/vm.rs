//! The VM — port of `src/runtime/vm.zig` (1,233 lines, 0 test blocks).
//!
//! `mp_execute_bytecode` is the only entry (`execute_frame` here):
//! build a dispatch context and loop `dispatch_loop` (in
//! `vm_dispatch.rs`); `.normal` → `RunOutcome::Normal`,
//! `.yield` → `RunOutcome::Yield`, `.raised` → `exception_handler`
//! and re-enter on catch, `.ret_exc` → the exception directly
//! (unknown opcode, bypasses the handler), `.reenter` → tier-up
//! (wasm only — the arm is kept, unreachable on native).
//!
//! Shape differences from the Zig, all deliberate and documented:
//!
//! * **The value stack lives in the frame** (`CodeState.stack`,
//!   `Vec<Option<Value>>`, `None` == `MP_OBJ_NULL` — every `Value`
//!   variant is a real Python value, so no variant can serve as
//!   NULL). The Zig's `sp` pointer is `frame.stack.len()`; `sp[0]`
//!   is `stack.last()`. `fastn` (`statePtr() + n_state - 1`) is
//!   `frame.state[n_state - 1 - u]`.
//! * **`frame.ip` stays at the opcode byte during the opcode.**
//!   The Zig's `fetch` syncs `csm.ip` to the opcode byte and the
//!   local `ip` cursor advances past the operands; `csm.ip` is
//!   only rewritten at the next fetch. The Rust dispatch keeps a
//!   local `ip` cursor the same way and writes it back to
//!   `frame.ip` at the end of each opcode — so when a helper raises
//!   mid-opcode, `frame.ip` is still the opcode byte, which is
//!   what `exception_handler` needs (`cur_op`, the FOR_ITER
//!   `ulabel` at `ip + 1`, the traceback `bc` offset).
//! * **GC rooting (D6).** The frame's live values are invisible to
//!   the arena, so the dispatch syncs them into `ctx.roots.stack`
//!   at the top of every iteration: `roots.stack.truncate(mark)`
//!   then extend with the frame's stack + state + `fun` +
//!   `prev_exc` values (see `sync_frame_roots`). `mark` is the
//!   `roots.stack` length on `execute_frame` entry, so nested
//!   frames keep disjoint regions and the caller's region survives
//!   a nested call. The VM's long-lived values (type objects,
//!   singletons, globals, builtins, module registry) are pinned in
//!   `roots.extra` (`Vm::pin`); suspended generator frames are
//!   re-rooted by `rebuild_frame_roots` on suspend/resume
//!   (truncate `extra` to `extra_base`, re-extend with every
//!   suspended frame's values). `pending_exc` is NOT pinned — it
//!   is drained and consumed inside the dispatch, and the
//!   per-iteration sync covers it while it lives.
//! * **No NLR/longjmp.** Raises travel as `RunOutcome::Exception`
//!   / the dispatch's `Raised` arm, exactly as the Zig's
//!   `ctx.outcome = .raised` + `ctx.pending_exc`.
//! * **`reenter` (tier-up) is kept as an arm** but never taken on
//!   native (`reenter_for_tier_up` is wasm-only in the Zig).
//! * **The `attr` slot does not exist yet** (C12a owns it): where
//!   the VM needs an attribute lookup it uses the C10a direct
//!   fns (`fun_bc_attr` / `bound_meth_attr` / `closure_attr`) and
//!   the type locals-dict lookup — see `load_method` in
//!   `vm_ops.rs` (C14 subsumes it once `attr` lands).
//! * **Imports are stubs** (C15): `IMPORT_NAME` / `IMPORT_FROM` /
//!   `IMPORT_STAR` raise `ImportError` (see `vm_ops.rs`).
//! * **Float/complex ops are stubs** (C18 owns the float/complex
//!   type slots): a float operand reaches the float type's
//!   `binary_op` slot, which is absent, so `binary_op` falls
//!   through to `binopUnsupported` (TypeError). Documented in
//!   `vm_ops.rs`.
//!
//! The runner seam (`src/runtime/runner.rs`, unified by C13):
//! `execute_frame` IS the `VmRunner` body — `gen_resume` /
//! `fun_bc_call` take it as a closure capturing a re-borrowed
//! `&mut VmState` (see `vm_ops.rs::resume_generator`).

use std::collections::HashMap;

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::mpy;
use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::objexcept_types::ExceptionTypes;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, FunBuiltinData, GcRef, MapData, Obj, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::builtinevex::{compile_builtin_fn, eval_builtin_fn, exec_builtin_fn};
use crate::runtime::builtinimport::import_builtin_fn;
use crate::runtime::code_store::CodeStore;
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::vm_builtins::{
    build_class_builtin_fn, next_builtin_fn, print_builtin, print_builtin_nokw,
};
use crate::runtime::vm_types::build_vm_types;

/// The type objects the VM owns beyond `BuiltinTypes` (built once
/// by `build_vm_types`, rooted in `roots.extra` for the VM's
/// lifetime).
#[derive(Clone, Copy)]
pub struct VmTypes {
    pub fun_bc: GcRef,
    pub gen_wrap: GcRef,
    pub gen_instance: GcRef,
    pub closure: GcRef,
    pub bound_meth: GcRef,
    pub module: GcRef,
    pub object: GcRef,
    pub ellipsis: GcRef,
    pub slice: GcRef,
    pub fun_builtin_var: GcRef,
    pub array: GcRef,
    pub bytearray: GcRef,
    pub memoryview: GcRef,
    /// The cell type (`MAKE_CLOSURE` allocates cells with it).
    pub cell: GcRef,
    /// The property type (bound as `property` in builtins).
    pub property: GcRef,
    /// The `map` type (bound as `map` in builtins, C13b).
    pub map: GcRef,
    /// The `filter` type (bound as `filter` in builtins, C13b).
    pub filter: GcRef,
    /// The `zip` type (bound as `zip` in builtins, C13b).
    pub zip: GcRef,
    /// The `enumerate` type (bound as `enumerate` in builtins,
    /// C13b).
    pub enumerate: GcRef,
}

/// The VM's mutable state (the `Ctx` + `mp_state_ctx` pieces
/// `execute_frame` needs). `Vm` splits into a `CallCtx` (arena /
/// roots / types / pool / pending_raise) and this state — disjoint
/// field borrows, so helpers take `(&mut CallCtx, &mut VmState, …)`.
pub struct VmState {
    pub code_store: CodeStore,
    /// The current globals dict (saved/restored around nested
    /// calls into another module's function — the Zig's
    /// `old_globals`).
    pub globals: GcRef,
    /// The current locals dict (`mp_state_ctx.thread.dict_locals`,
    /// `jcb_glue.zig`): set by `eval`/`exec` (`codeExecute`,
    /// `builtinevex.zig:64-84` — C15a) and by a class body
    /// (`__build_class__` sets it to a fresh dict and restores it
    /// afterwards — C13b); `None` means "locals == globals" (the
    /// module level — the Zig's `dict_locals` starts `NULL` and
    /// `mp_load_name` falls through to globals). `load_name`
    /// reads it first, `store_name` writes it
    /// (`runtime.zig:288-301,336-341`). Behavior is identical
    /// when `None`.
    pub locals: Option<GcRef>,
    /// The builtins globals dict (`mp_load_global`'s fallback).
    pub builtins: GcRef,
    /// The VM-owned empty-tuple singleton (`mp_const_empty_tuple`).
    pub empty_tuple: Value,
    /// The VM-owned Ellipsis singleton (`mp_const_ellipsis_obj`).
    pub ellipsis: Value,
    /// Suspended generator frames, keyed by generator object.
    pub frames: HashMap<GcRef, CodeState>,
    /// The pending exception object (the Zig's `pyerr.pending_exc`).
    pub pending_exc: Option<Value>,
    /// The 28 ROM exception types (C12b).
    pub exc: ExceptionTypes,
    /// The `next` builtin (intercepted by GcRef identity in
    /// `call_function` — its `func` is a stub, see `vm_ops.rs`).
    pub next_builtin: Value,
    /// The `__build_class__` builtin — intercepted by GcRef
    /// identity in `call_function` (it needs the VM's frame
    /// execution + the locals override, see
    /// `call_build_class_ctx` in `vm_call.rs`); its `func` is
    /// a stub, like `next`'s.
    pub build_class_builtin: Value,
    /// The `eval` builtin (intercepted by GcRef identity in
    /// `call_function_ctx` — its `func` is a stub, see
    /// `builtinevex.rs`).
    pub eval_builtin: Value,
    /// The `exec` builtin (same interception as `eval`).
    pub exec_builtin: Value,
    /// The `compile` builtin (same interception as `eval`;
    /// raises `NotImplementedError` — there is no `Obj::Code`
    /// yet, see `builtinevex.rs`).
    pub compile_builtin: Value,
    /// The `__import__` builtin (intercepted by GcRef identity
    /// in `call_function_ctx`, like `eval` — its `func` is a
    /// stub, see `builtinimport.rs`). Added by C15b.
    pub import_builtin: Value,
    /// The scheduler pending-exception slot
    /// (`MP_STATE_THREAD(mp_pending_exception)`,
    /// `scheduler.zig:33`): `None` = `MP_OBJ_NULL` = nothing
    /// pending. Added by C15b; set by `mp_sched_exception`,
    /// drained by `pending_exc_check` (the dispatch loop's
    /// per-iteration poll, `vm.zig:110-114` Tier G).
    pub pending_sched: Option<Value>,
    /// `sys.path` (`MP_STATE_VM(sys_mutable[0])`,
    /// `runtime.zig:276-279`): the import search path. Default
    /// `[""]` — `mp_init` creates an empty list and appends the
    /// empty qstr (the current directory); the entry point
    /// overwrites `[0]` with the script dir and appends
    /// `PETITPY_LIB` (`main.zig:244-248`, Phase E). Added by
    /// C15b. A `Vec<String>` (not a Python list object — the
    /// `sys` module that exposes it is D's `modsys`).
    pub sys_path: Vec<String>,
    /// The frozen-module table (`frozen_modules.zig`): `(dotted
    /// name, .mpy bytes)` pairs. Empty by default (the default
    /// build has `MICROPY_FROZEN_TINY_ENABLED=0`); Phase D fills
    /// it from the frozen petitpy-lib bundle. Added by C15b.
    pub frozen_modules: crate::runtime::frozen_modules::FrozenTable,
    /// Registered builtin modules (`mp_module_get_builtin`'s
    /// non-extensible table, `objmodule.zig`): `(name qstr,
    /// module)` pairs. Empty until D1 registers modules (the
    /// `sys` module lives here). Added by C15b.
    pub builtin_modules: Vec<(u32, Value)>,
    /// Registered extensible builtin modules (the `u`-strip
    /// table — `import ufoo` maps to extensible `foo`).
    /// Added by C15b; filled by D1.
    pub extensible_modules: Vec<(u32, Value)>,
    /// Loaded modules by name qstr (C15 fills this via imports).
    pub modules: HashMap<u32, GcRef>,
    /// The VM-owned type objects (fun_bc, gen_wrap, …) — kept in
    /// the state (not on `Vm`) so `split()` hands them to the
    /// helpers alongside the rest of the state.
    pub vm_types: VmTypes,
    /// `roots.extra` length after the pinned values — the
    /// truncation point for `rebuild_frame_roots` (kept in the
    /// state so the resume path can re-root after a yield).
    pub extra_base: usize,
}

impl VmState {
    /// `mp_globals_get` (`jcb_glue.zig`): the current globals
    /// dict — a plain field read, as the Zig's accessor is a
    /// plain read of `mp_state_ctx.thread.dict_globals`.
    pub fn globals_dict(&self) -> GcRef {
        self.globals
    }

    /// `mp_globals_set` (`jcb_glue.zig`): replace the current
    /// globals dict.
    pub fn set_globals_dict(&mut self, dict: GcRef) {
        self.globals = dict;
    }

    /// `mp_locals_get` (`jcb_glue.zig`): the current locals dict
    /// — `locals` when set, else `globals` (the Zig's
    /// `dict_locals` is `NULL` at module level; `mp_load_name`
    /// falls through to globals there).
    pub fn locals_dict(&self) -> GcRef {
        self.locals.unwrap_or(self.globals)
    }

    /// `mp_locals_set` (`jcb_glue.zig`): replace the current
    /// locals dict (`None` = back to "locals == globals").
    pub fn set_locals_dict(&mut self, dict: Option<GcRef>) {
        self.locals = dict;
    }
}

/// The VM: the arena + roots + types + pool (the `CallCtx` parts)
/// plus the mutable state and the owned type objects.
pub struct Vm {
    pub arena: GcArena,
    pub roots: RootSet,
    pub types: BuiltinTypes,
    pub pool: RuntimeQstr,
    pub pending_raise: Option<PendingRaise>,
    pub state: VmState,
}

// ── Vm construction ──────────────────────────────────────────────────

impl Vm {
    /// Build a fresh VM: the arena, the type objects, the builtins
    /// dict, the singletons. Everything live is pinned in
    /// `roots.extra` (`extra_base` marks the end of the pinned
    /// region — suspended generator frames root after it).
    pub fn new() -> Vm {
        let mut arena = GcArena::new();
        let mut roots = RootSet::new();
        let pool = RuntimeQstr::new();
        let mut pending_raise: Option<PendingRaise> = None;
        // Bootstrap ctx: `build_vm_types` calls no helper that
        // reads `ctx.types` (audited 2026-11-17 — the type_data
        // fns are pure, the locals-dict builders and
        // `build_exception_types` only allocate), so a dummy
        // `BuiltinTypes` is safe here; every `CallCtx` the VM
        // creates afterwards carries the real one.
        let dummy = BuiltinTypes {
            int: GcRef(0),
            str_: GcRef(0),
            bytes: GcRef(0),
            bool_: GcRef(0),
            none_type: GcRef(0),
            float: GcRef(0),
            complex: GcRef(0),
            tuple: GcRef(0),
            list: GcRef(0),
            set: GcRef(0),
            frozenset: GcRef(0),
            range: GcRef(0),
            dict: GcRef(0),
            array: GcRef(0),
            bytearray: GcRef(0),
            type_: GcRef(0),
            object: GcRef(0),
            super_: GcRef(0),
            staticmethod: GcRef(0),
            classmethod: GcRef(0),
            boundmeth: GcRef(0),
            empty_tuple: GcRef(0),
            empty_bytes: GcRef(0),
            native_init_wrapper: GcRef(0),
            property: GcRef(0),
        };
        let (types, vm_types, exc, empty_tuple, ellipsis) = {
            let mut ctx = CallCtx::new(&mut arena, &mut roots, &dummy, &pool, &mut pending_raise);
            build_vm_types(&mut ctx)
        };
        let mut vm = Vm {
            arena,
            roots,
            types,
            pool,
            pending_raise,
            state: VmState {
                code_store: CodeStore::new(),
                globals: GcRef(0),
                locals: None,
                builtins: GcRef(0),
                empty_tuple,
                ellipsis,
                frames: HashMap::new(),
                pending_exc: None,
                exc,
                next_builtin: Value::None,
                build_class_builtin: Value::None,
                eval_builtin: Value::None,
                exec_builtin: Value::None,
                compile_builtin: Value::None,
                import_builtin: Value::None,
                pending_sched: None,
                sys_path: vec![String::new()],
                frozen_modules: crate::runtime::frozen_modules::FrozenTable::new(),
                builtin_modules: Vec::new(),
                extensible_modules: Vec::new(),
                modules: HashMap::new(),
                vm_types,
                extra_base: 0,
            },
        };
        vm.build_builtins();
        vm
    }

    /// Split the VM into a `CallCtx` and a `&mut VmState` (disjoint
    /// field borrows — the VM helpers take
    /// `(&mut CallCtx, &mut VmState, …)`).
    pub fn split(&mut self) -> (CallCtx<'_>, &mut VmState) {
        let ctx = CallCtx::new(
            &mut self.arena,
            &mut self.roots,
            &self.types,
            &self.pool,
            &mut self.pending_raise,
        );
        (ctx, &mut self.state)
    }

    /// Pin a live value in `roots.extra` (inserted BEFORE the
    /// suspended-frame region, so `rebuild_frame_roots` keeps
    /// working). Used for the type objects, singletons, the
    /// builtins dict and loaded modules.
    pub fn pin(&mut self, v: Value) {
        self.roots.extra.insert(self.state.extra_base, v);
        self.state.extra_base += 1;
    }

    /// Re-root every suspended generator frame: truncate `extra`
    /// to the pinned region and re-extend with each frame's live
    /// values (stack + state + `fun` + `prev_exc`). Called on
    /// suspend and on resume.
    pub fn rebuild_frame_roots(&mut self) {
        self.roots.extra.truncate(self.state.extra_base);
        for frame in self.state.frames.values() {
            for v in frame_roots_of(frame) {
                self.roots.extra.push(v);
            }
        }
    }

    /// Build the builtins dict and the VM-owned builtins
    /// (`print`, `next`, `__build_class__`). The dict binds the
    /// type objects, the 28 exception types, `Ellipsis`, and the
    /// three builtins. The full builtins module (D1, `modbuiltins`)
    /// extends this dict; the names bound here are the ones C13
    /// itself owns.
    fn build_builtins(&mut self) {
        // (Capture the VmTypes refs before the split — `split`
        // borrows `self`, so the fields are unreadable after it.
        // `VmTypes` is `Copy`, so this is a plain copy.)
        let vmt = self.state.vm_types;
        let (mut ctx, state) = self.split();
        let mut pairs: Vec<(Value, Value)> = Vec::new();
        let bind = |name: &str, v: Value, pairs: &mut Vec<(Value, Value)>| {
            let id = static_qstr_id(name.as_bytes())
                .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
            pairs.push((Value::Qstr(id), v));
        };
        let t = ctx.types;
        bind("int", Value::Heap(t.int), &mut pairs);
        bind("str", Value::Heap(t.str_), &mut pairs);
        bind("bytes", Value::Heap(t.bytes), &mut pairs);
        bind("bool", Value::Heap(t.bool_), &mut pairs);
        bind("float", Value::Heap(t.float), &mut pairs);
        bind("complex", Value::Heap(t.complex), &mut pairs);
        bind("list", Value::Heap(t.list), &mut pairs);
        bind("tuple", Value::Heap(t.tuple), &mut pairs);
        bind("dict", Value::Heap(t.dict), &mut pairs);
        bind("set", Value::Heap(t.set), &mut pairs);
        bind("frozenset", Value::Heap(t.frozenset), &mut pairs);
        bind("range", Value::Heap(t.range), &mut pairs);
        bind("object", Value::Heap(vmt.object), &mut pairs);
        bind("type", Value::Heap(t.type_), &mut pairs);
        bind("property", Value::Heap(vmt.property), &mut pairs);
        bind("bytearray", Value::Heap(vmt.bytearray), &mut pairs);
        bind("memoryview", Value::Heap(vmt.memoryview), &mut pairs);
        bind("slice", Value::Heap(vmt.slice), &mut pairs);
        bind("map", Value::Heap(vmt.map), &mut pairs);
        bind("filter", Value::Heap(vmt.filter), &mut pairs);
        bind("zip", Value::Heap(vmt.zip), &mut pairs);
        bind("enumerate", Value::Heap(vmt.enumerate), &mut pairs);
        bind("Ellipsis", state.ellipsis, &mut pairs);
        for (name, r) in [
            ("BaseException", state.exc.base_exception),
            ("SystemExit", state.exc.system_exit),
            ("KeyboardInterrupt", state.exc.keyboard_interrupt),
            ("GeneratorExit", state.exc.generator_exit),
            ("Exception", state.exc.exception),
            ("StopAsyncIteration", state.exc.stop_async_iteration),
            ("StopIteration", state.exc.stop_iteration),
            ("ArithmeticError", state.exc.arithmetic_error),
            ("OverflowError", state.exc.overflow_error),
            ("ZeroDivisionError", state.exc.zero_division_error),
            ("AssertionError", state.exc.assertion_error),
            ("AttributeError", state.exc.attribute_error),
            ("EOFError", state.exc.eof_error),
            ("ImportError", state.exc.import_error),
            ("ModuleNotFoundError", state.exc.module_not_found_error),
            ("LookupError", state.exc.lookup_error),
            ("IndexError", state.exc.index_error),
            ("KeyError", state.exc.key_error),
            ("MemoryError", state.exc.memory_error),
            ("NameError", state.exc.name_error),
            ("OSError", state.exc.os_error),
            ("RuntimeError", state.exc.runtime_error),
            ("NotImplementedError", state.exc.not_implemented_error),
            ("SyntaxError", state.exc.syntax_error),
            ("IndentationError", state.exc.indentation_error),
            ("TypeError", state.exc.type_error),
            ("ValueError", state.exc.value_error),
            ("UnicodeError", state.exc.unicode_error),
        ] {
            bind(name, Value::Heap(r), &mut pairs);
        }
        // `print` (KwFn: `sep`/`end`/`file`/`flush`).
        let print_id = static_qstr_id(b"print").expect("print is a static qstr");
        let print_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: print_id,
                n_args: fun_n_args(0, 255),
                func: print_builtin_nokw,
                kw_func: Some(print_builtin),
            }),
        );
        let print_val = Value::Heap(print_fun);
        bind("print", print_val, &mut pairs);
        // `next` (1–2 args; intercepted by identity in
        // `call_function` — its `func` is a stub).
        let next_id = static_qstr_id(b"next").expect("next is a static qstr");
        let next_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: next_id,
                n_args: fun_n_args(1, 2),
                func: next_builtin_fn,
                kw_func: None,
            }),
        );
        let next_val = Value::Heap(next_fun);
        bind("next", next_val, &mut pairs);
        // `__build_class__` (intercepted by identity in
        // `call_function` — its `func` is a stub).
        let bc_id = static_qstr_id(b"__build_class__").expect("__build_class__ is a static qstr");
        let bc_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: bc_id,
                n_args: fun_n_args(2, 255),
                func: build_class_builtin_fn,
                kw_func: None,
            }),
        );
        let bc_val = Value::Heap(bc_fun);
        bind("__build_class__", bc_val, &mut pairs);
        // `eval` / `exec` (1–3 args) and `compile` (3–6 args):
        // `MpObjFunBuiltinVar` objects (`builtinevex.zig:207-225`,
        // sigs `(1, 3, false)` / `(1, 3, false)` / `(3, 6,
        // false)`), intercepted by identity in `call_function_ctx`
        // — their `func`s are stubs, as `next`'s is.
        let mk_var = |ctx: &mut CallCtx,
                      name: &str,
                      min: u16,
                      max: u16,
                      func: crate::objects::slots::BuiltinFn,
                      pairs: &mut Vec<(Value, Value)>|
         -> Value {
            let id = static_qstr_id(name.as_bytes())
                .unwrap_or_else(|| panic!("builtin name is not a static qstr: {}", name));
            let f = ctx.alloc(
                Some(vmt.fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name: id,
                    n_args: fun_n_args(min, max),
                    func,
                    kw_func: None,
                }),
            );
            let v = Value::Heap(f);
            bind(name, v, pairs);
            v
        };
        let eval_val = mk_var(&mut ctx, "eval", 1, 3, eval_builtin_fn, &mut pairs);
        let exec_val = mk_var(&mut ctx, "exec", 1, 3, exec_builtin_fn, &mut pairs);
        let compile_val = mk_var(&mut ctx, "compile", 3, 6, compile_builtin_fn, &mut pairs);
        // `__import__` (1–5 args: `builtinimport.zig:179-184`,
        // `MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(..., 1, 5, ...)`),
        // intercepted by identity in `call_function_ctx` — its
        // `func` is a stub, as `eval`'s is. Added by C15b.
        let import_val =
            mk_var(&mut ctx, "__import__", 1, 5, import_builtin_fn, &mut pairs);

        let builtins = ctx.alloc(
            None,
            Obj::Dict(DictData {
                map: MapData::from_pairs(pairs),
            }),
        );
        state.builtins = builtins;
        // No module loaded yet: globals default to builtins (a
        // harmless default — `load_module` sets the real globals).
        state.globals = builtins;
        state.next_builtin = next_val;
        state.build_class_builtin = bc_val;
        state.eval_builtin = eval_val;
        state.exec_builtin = exec_val;
        state.compile_builtin = compile_val;
        state.import_builtin = import_val;
        // Pin everything live: the types, the singletons, the
        // builtins dict and the builtin values.
        let mut pins: Vec<Value> = Vec::new();
        for r in [
            t.int,
            t.str_,
            t.bytes,
            t.bool_,
            t.none_type,
            t.float,
            t.complex,
            t.tuple,
            t.list,
            t.set,
            t.frozenset,
            t.range,
            t.dict,
            t.type_,
        ] {
            pins.push(Value::Heap(r));
        }
        for r in [
            vmt.fun_bc,
            vmt.gen_wrap,
            vmt.gen_instance,
            vmt.closure,
            vmt.bound_meth,
            vmt.module,
            vmt.object,
            vmt.ellipsis,
            vmt.slice,
            vmt.fun_builtin_var,
            vmt.array,
            vmt.bytearray,
            vmt.memoryview,
            vmt.cell,
            vmt.property,
        ] {
            pins.push(Value::Heap(r));
        }
        for r in state.exc.all() {
            pins.push(Value::Heap(r));
        }
        pins.push(state.empty_tuple);
        pins.push(state.ellipsis);
        pins.push(Value::Heap(builtins));
        pins.push(print_val);
        pins.push(next_val);
        pins.push(bc_val);
        pins.push(eval_val);
        pins.push(exec_val);
        pins.push(compile_val);
        pins.push(import_val);
        drop(ctx);
        for v in pins {
            self.pin(v);
        }
    }

    /// Load a module from source text: compile (bridge,
    /// `debug_mode` false — the `petitpy` entry point), read the
    /// `.mpy`, register it, and run the module body. Returns the
    /// module object on success, or the escaped exception on
    /// failure.
    pub fn load_module_from_source(
        &mut self,
        source: &str,
        name: &str,
    ) -> Result<Value, VmError> {
        let mpy = compile_to_mpy_named(
            source,
            crate::compiler::parser::InputKind::FileInput,
            name,
        )
        .map_err(VmError::Compile)?;
        self.load_module_from_mpy(&mpy, name)
    }


    /// Load a module from `.mpy` bytes (the `petitpy-vm` entry
    /// point): read, register, run the module body.
    pub fn load_module_from_mpy(&mut self, data: &[u8], name: &str) -> Result<Value, VmError> {
        let (module_val, code_idx) = self.register_mpy_code(data, name, None, true)?;
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("new_module returns a heap module"),
        };
        // The module's globals dict is its `dict` — the frame's
        // globals while the body runs.
        let globals = match self.arena.get(module_ref) {
            Obj::Module(m) => m.dict,
            _ => unreachable!("module object"),
        };
        // Pin the module object (its dict is traced through it).
        self.pin(module_val);
        self.state.globals = globals;
        // Run the module body as a 0-arg function call.
        let fun = Value::Heap(self.call_module_fun(code_idx, module_val));
        let result = crate::runtime::vm_ops::call_function(self, fun, 0, 0, &[]);
        match result {
            Ok(_) => Ok(module_val),
            Err(exc) => Err(VmError::Raised(exc)),
        }
    }


    /// Build the module-body `FunBc` for a registered module code
    /// (context = the module, so its globals are the module dict).
    pub(crate) fn call_module_fun(&mut self, code_idx: usize, module_val: Value) -> GcRef {
        let (mut ctx, state) = self.split();
        Self::call_module_fun_ctx(&mut ctx, state, code_idx, module_val)
    }
}

/// A module-load failure: a compile error, an `.mpy` read error,
/// or an exception that escaped the module body.
#[derive(Debug)]
pub enum VmError {
    Compile(crate::compiler::compile::CompileError),
    Mpy(mpy::MpyError),
    OutOfMemory,
    Raised(Value),
}

/// The live values of one frame, for rooting (stack + state +
/// `fun` + the exc-stack `prev_exc`s).
pub(crate) fn frame_roots_of(frame: &CodeState) -> Vec<Value> {
    let mut out = Vec::new();
    out.push(Value::Heap(frame.fun));
    for v in &frame.state {
        if let Some(v) = v {
            out.push(*v);
        }
    }
    for v in &frame.stack {
        if let Some(v) = v {
            out.push(*v);
        }
    }
    for b in &frame.exc_stack {
        if let Some(v) = b.prev_exc {
            out.push(v);
        }
    }
    out
}

/// Sync the current frame's live values into `ctx.roots.stack`
/// (the dispatch calls this at the top of every iteration — see
/// the module docs). `mark` is the `roots.stack` length on
/// `execute_frame` entry.
pub(crate) fn sync_frame_roots(ctx: &mut CallCtx, frame: &CodeState, mark: usize) {
    ctx.roots.stack.truncate(mark);
    ctx.roots.stack.extend(frame_roots_of(frame));
}
