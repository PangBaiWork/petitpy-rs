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
//!
//! The `attr` slot (C12a), the import system (C15b) and the
//! float/complex slots (C18a) have all landed since the earlier
//! doc bullets were written; their details live in
//! `runtime_method.rs`, `builtinimport.rs` / `import_search.rs`
//! and `objfloat.rs` / `objcomplex.rs` respectively.
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
use crate::objects::value::{GcRef, Value};
use crate::runtime::bc::CodeState;
use crate::runtime::code_store::CodeStore;
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::vm_types::build_vm_types;

// `VmTypes` lives in `vm_types.rs` (moved by E1, file-size cap);
// re-exported here so `crate::runtime::vm::VmTypes` keeps working.
pub use crate::runtime::vm_types::VmTypes;

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
    /// returns a code object — `Obj::Code`, E1).
    pub compile_builtin: Value,
    /// The `__import__` builtin (intercepted by GcRef identity
    /// in `call_function_ctx`, like `eval` — its `func` is a
    /// stub, see `builtinimport.rs`). Added by C15b.
    pub import_builtin: Value,
    /// The `help` builtin (intercepted by GcRef identity in
    /// `call_function_ctx`, like `eval` — its `func` is a
    /// stub, see `builtinhelp.rs`). Added by C18b.
    pub help_builtin: Value,
    /// The D1 VM-intercepted builtins (same interception as
    /// `eval` — their `func`s are the shared stub, see
    /// `builtin_fns::intercepted_stub_fn` and `builtin_vm.rs`):
    /// `globals`, `locals`, `dir`, `sorted`, `min`, `max`,
    /// `getattr`, `setattr`, `hasattr`, `delattr`, `iter`,
    /// `all`, `any`, `sum`.
    pub globals_builtin: Value,
    pub locals_builtin: Value,
    pub dir_builtin: Value,
    pub sorted_builtin: Value,
    /// The `str.join` / `bytes.join` method builtins (E2b):
    /// two DIFFERENT `FunBuiltin` objects (each type dict is
    /// built from its own table, `vm_types.rs`), both wrapping
    /// the same `str_join` fn. Intercepted by GcRef identity
    /// in `call_function_ctx` (see `call_join_ctx` in
    /// `builtin_vm.rs`) — `str_join` is ctx-only and cannot
    /// resume a generator, so `', '.join(gen())` needs the
    /// VM's real call hook to collect the iterable first.
    pub str_join_builtin: Value,
    pub bytes_join_builtin: Value,
    /// The `list.sort` method builtin (E2c): the `FunBuiltin`
    /// from the list type dict. Intercepted by GcRef identity
    /// in `call_function_ctx` (see `call_list_sort_ctx` in
    /// `builtin_vm.rs`) — `list_sort` is ctx-only and returns
    /// `PyRaise` for a non-`None` key, so `l.sort(key=f)`
    /// needs the VM's real call hook to run `f`.
    pub list_sort_builtin: Value,
    pub min_builtin: Value,
    pub max_builtin: Value,
    pub getattr_builtin: Value,
    pub setattr_builtin: Value,
    pub hasattr_builtin: Value,
    pub delattr_builtin: Value,
    pub iter_builtin: Value,
    pub all_builtin: Value,
    pub any_builtin: Value,
    pub sum_builtin: Value,
    /// The `repr` builtin (intercepted by GcRef identity in
    /// `call_function_ctx` — an instance's `__repr__` needs the
    /// VM's real call hook, see `builtin_str`). Added by E2.
    pub repr_builtin: Value,
    /// The `len` builtin (same interception as `repr` — an
    /// instance's `__len__` needs the VM's real call hook, see
    /// `builtin_str`). Added by E2.
    pub len_builtin: Value,
    /// The `print` builtin (same interception as `repr` — an
    /// instance arg's `__str__` needs the VM's real call hook,
    /// see `builtin_str`). Added by E2.
    pub print_builtin: Value,
    /// The `NotImplemented` singleton value (D1): an
    /// `Obj::Instance` with the shared ellipsis type as header
    /// (the vendor's `mp_type_singleton` carries both it and
    /// `Ellipsis`, `objsingleton.c:47-53`); `ellipsis_print`
    /// spells it `NotImplemented`.
    pub not_implemented: Value,
    /// The scheduler pending-exception slot
    /// (`MP_STATE_THREAD(mp_pending_exception)`,
    /// `scheduler.zig:33`): `None` = `MP_OBJ_NULL` = nothing
    /// pending. Added by C15b; set by `mp_sched_exception`,
    /// drained by `pending_exc_check` (the dispatch loop's
    /// per-iteration poll, `vm.zig:110-114` Tier G).
    pub pending_sched: Option<Value>,
    /// Whether the compiler is linked into this VM
    /// (`main_vm.zig`: the `petitpy-vm` binary sets the
    /// compiler-bridge stubs, so `eval` / `exec` / `compile`
    /// and the import system's `.py` compile path raise
    /// `NotImplementedError("compiler not linked in
    /// petitpy-vm")`; the `petitpy` binary leaves it true).
    /// Added by E1.
    pub compiler_linked: bool,
    /// `sys.path` (`MP_STATE_VM(sys_mutable[0])`,
    /// `runtime.zig:276-279`): the import search path. Default
    /// `[""]` — `mp_init` creates an empty list and appends the
    /// empty qstr (the current directory); the entry point
    /// overwrites `[0]` with the script dir and appends
    /// `PETITPY_LIB` (`main.zig:244-248`, Phase E). Added by
    /// C15b. A `Vec<String>` (not a Python list object — the
    /// `sys` module that exposes it is D's `modsys`).
    pub sys_path: Vec<String>,
    /// The `sys.path` list OBJECT (`sys_mutable[0]`'s value,
    /// `modsys.zig`): the same list `sys_path` is built from.
    /// Python `sys.path.append(x)` mutates this object, and
    /// the import search reads it (see `search_top_level`);
    /// `set_sys_path` syncs it in place. `None` until D2's
    /// module step builds it. Added by D2.
    pub sys_path_list: Option<GcRef>,
    /// The live `sys.modules` dict object
    /// (`mp_loaded_modules_dict`, `modsys.zig`): the sys
    /// dict's `modules` entry points at this dict, and the
    /// import system keeps it in sync with `modules` (file
    /// modules only — builtin modules are never registered,
    /// probed). `None` until D2's module step builds it.
    /// Added by D2.
    pub sys_modules_dict: Option<GcRef>,
    /// The `sys.argv` list object (`modsys.zig`): default
    /// `[]`; Phase E's entry fills it in place. `None` until
    /// D2's module step builds it. Added by D2.
    pub sys_argv_list: Option<GcRef>,
    /// The stdio stream objects (`npy_file_io_init`,
    /// `file_io.zig`): `TextIOWrapper` over fds 0/1/2.
    /// `sys.print_exception`'s default target is `sys_stdout`
    /// (the ROM object — `sys.stdout` cannot be rebound,
    /// probed 2026-11-18); D1's `input()` reads `sys_stdin`.
    /// `Value::None` until D2's module step builds them.
    /// Added by D2.
    pub sys_stdin: Value,
    pub sys_stdout: Value,
    pub sys_stderr: Value,
    /// The `sys.print_exception` builtin (intercepted by
    /// GcRef identity in `call_function_ctx` — it needs the
    /// VM's `sys_stdout` for its default target; its `func`
    /// is a stub, like `next`'s). Added by D2.
    pub sys_print_exception_builtin: Value,
    /// The `os.open` fd table: fd → open `File`. `os.open`
    /// inserts, `os.close` removes (closing the file),
    /// `os.read`/`os.write`/`os.lseek`/`os.fsync` look up.
    /// There is no safe `std` API for an arbitrary fd
    /// (`from_raw_fd` is `unsafe`), so VM-opened files are
    /// tracked here; fds 0/1/2 are handled inline via
    /// `std::io::{stdin, stdout, stderr}`. Added by D2.
    pub os_fds: HashMap<i32, std::fs::File>,
    /// Stdio fds closed by `os.close(0/1/2)` — the real fd
    /// cannot be closed in safe Rust, so the closure is
    /// tracked here and every later use raises EBADF
    /// (`modos_fd.rs`). Added by D2.
    pub os_closed_stdio: std::collections::HashSet<i32>,
    /// The `os.open`/`os.close`/`os.read`/`os.write`/
    /// `os.lseek`/`os.fsync` builtins (intercepted by GcRef
    /// identity in `call_function_ctx` — they need
    /// `state.os_fds`; their `func`s are stubs, like
    /// `next`'s). Added by D2.
    pub os_fd_builtins: crate::runtime::modos_fd::OsFdBuiltins,
    /// The `os.__getattr__` builtin (intercepted by GcRef
    /// identity — it needs the VM's import machinery, see
    /// `modos.rs`). Added by D2.
    pub os_getattr_builtin: Value,
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
    /// Lane D4 module state (`micropython.opt_level`, the
    /// `__main__` module, the `signal` handler table — see
    /// `d4state.rs`). Added by D4.
    pub d4: crate::runtime::d4state::D4State,
    /// The intercepted `re.sub` builtin (D5): the module/type
    /// dicts hold a bare-raise stub `FunBuiltin` for `sub`
    /// (it calls user code — a callable replacement — so the
    /// real body lives in `modre::call_sub_ctx`, reached by
    /// GcRef identity here, the D1/D4 pattern). `Value::None`
    /// until `build_d5_modules` stores the real builtin.
    pub re_sub_builtin: Value,
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
            fileio: GcRef(0),
            textio: GcRef(0),
            attrtuple: GcRef(0),
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
            dict_view: GcRef(0),
            dict_view_it: GcRef(0),
            socket: None,
            deque: None,
            ordereddict: None,
            namedtuple_fn: None,
            re_type: None,
            match_type: None,
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
                help_builtin: Value::None,
                globals_builtin: Value::None,
                locals_builtin: Value::None,
                dir_builtin: Value::None,
                sorted_builtin: Value::None,
                str_join_builtin: Value::None,
                bytes_join_builtin: Value::None,
                list_sort_builtin: Value::None,
                min_builtin: Value::None,
                max_builtin: Value::None,
                getattr_builtin: Value::None,
                setattr_builtin: Value::None,
                hasattr_builtin: Value::None,
                delattr_builtin: Value::None,
                iter_builtin: Value::None,
                all_builtin: Value::None,
                any_builtin: Value::None,
                sum_builtin: Value::None,
                repr_builtin: Value::None,
                len_builtin: Value::None,
                print_builtin: Value::None,
                not_implemented: Value::None,
                pending_sched: None,
                compiler_linked: true,
                sys_path: vec![String::new()],
                sys_path_list: None,
                sys_modules_dict: None,
                sys_argv_list: None,
                sys_stdin: Value::None,
                sys_stdout: Value::None,
                sys_stderr: Value::None,
                sys_print_exception_builtin: Value::None,
                os_fds: HashMap::new(),
            os_closed_stdio: std::collections::HashSet::new(),
                os_fd_builtins: crate::runtime::modos_fd::OsFdBuiltins {
                    open: Value::None,
                    close: Value::None,
                    read: Value::None,
                    write: Value::None,
                    lseek: Value::None,
                    fsync: Value::None,
                },
                os_getattr_builtin: Value::None,
                frozen_modules: crate::runtime::frozen_bundle::frozen_table(),
                builtin_modules: Vec::new(),
                extensible_modules: Vec::new(),
                d4: crate::runtime::d4state::D4State::default(),
                re_sub_builtin: Value::None,
                modules: HashMap::new(),
                vm_types,
                extra_base: 0,
            },
        };
        vm.build_builtins();
        crate::runtime::vm_modules::build_d2_modules(&mut vm);
        crate::runtime::vm_modules::build_d3_modules(&mut vm);
        crate::runtime::vm_modules::build_d4_modules(&mut vm);
        crate::runtime::vm_modules::build_d5_modules(&mut vm);
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
            for v in crate::runtime::vm_roots::frame_roots_of(frame) {
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

    /// D2: build the `os`/`sys` modules + the stdio objects and
    /// register them as builtin modules (the original's `mp_init`
    /// registers ALL builtin modules at startup; D1's registration
    /// owns the `io` module only — different names, so the
    /// appends cannot collide). The stdio objects are reachable
    /// from the sys dict (`stdin`/`stdout`/`stderr` entries)
    /// and the `.buffer` siblings via `Obj::children()`, so
    /// pinning `sys` roots them; the shared attrtuple type
    /// (`os.stat` results + `sys.implementation` headers) is
    /// pinned explicitly (header types are not traced).
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

