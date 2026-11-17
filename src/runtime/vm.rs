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
use crate::compiler::mpy::{self, QstrEntry, RawCode};
use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::callables::objfun::new_fun_bc;
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::objexcept_types::ExceptionTypes;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, FunBuiltinData, GcRef, MapData, Obj, Value};
use crate::pyerr::PyResult;
use crate::runtime::bc::CodeState;
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::vm_builtins::{
    build_class_builtin_fn, next_builtin_fn, print_builtin, print_builtin_nokw,
};
use crate::runtime::vm_types::build_vm_types;

/// One compiled code blob in the store: the raw bytecode, the
/// index of its module's resolved qstr table, the index of its
/// module's const-object table (converted `ObjEntry`s, pinned —
/// `LOAD_CONST_OBJ` reads them), and the module object (whose
/// globals dict is the code's globals).
#[derive(Debug, Clone)]
pub struct CodeEntry {
    pub raw: RawCode,
    pub qstr_table: usize,
    pub const_table: usize,
    pub module: GcRef,
}

/// The VM's code store: every compiled function/module's `RawCode`,
/// indexed by the code index in `FunBcData.code`. The Zig resolves
/// a child by index into its parent's `children` table
/// (`childTablePtr`, `mp_make_function_from_proto_fun`); the store
/// keeps that `(parent, child) -> code` map explicitly.
#[derive(Debug, Default)]
pub struct CodeStore {
    pub codes: Vec<CodeEntry>,
    pub qstr_tables: Vec<Vec<u16>>,
    /// Converted const-object tables, one per module (parallel to
    /// `qstr_tables`); the values are pinned in `roots.extra`
    /// at registration (see `load_module_from_mpy`).
    pub const_tables: Vec<Vec<Value>>,
    pub children: HashMap<(usize, usize), usize>,
}

impl CodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a module's `RawCode` and all its children
    /// recursively. Returns the module code's index. The module's
    /// qstr table is resolved once by the caller (static ids pass
    /// through, `New` bytes are interned into the pool — see
    /// `resolve_qstr_table`) and shared by every code in the
    /// module, as is the converted const-object table.
    pub fn register_module(
        &mut self,
        rc: RawCode,
        qstr_table: Vec<u16>,
        const_table: Vec<Value>,
        module: GcRef,
    ) -> usize {
        let table_idx = self.qstr_tables.len();
        self.qstr_tables.push(qstr_table);
        self.const_tables.push(const_table);
        self.register_code(rc, table_idx, module)
    }

    fn register_code(&mut self, rc: RawCode, table_idx: usize, module: GcRef) -> usize {
        let RawCode {
            kind,
            fun_data,
            children,
        } = rc;
        let idx = self.codes.len();
        // (Rebuild the RawCode without children — the children live
        // in the store now, keyed by (parent, child).)
        self.codes.push(CodeEntry {
            raw: RawCode {
                kind,
                fun_data,
                children: Vec::new(),
            },
            qstr_table: table_idx,
            const_table: table_idx,
            module,
        });
        for (i, child) in children.into_iter().enumerate() {
            let child_idx = self.register_code(child, table_idx, module);
            self.children.insert((idx, i), child_idx);
        }
        idx
    }

    pub fn child(&self, parent: usize, child: usize) -> Option<usize> {
        self.children.get(&(parent, child)).copied()
    }

    pub fn code(&self, idx: usize) -> &RawCode {
        &self.codes[idx].raw
    }

    pub fn qstr_table(&self, idx: usize) -> &[u16] {
        let t = self.codes[idx].qstr_table;
        &self.qstr_tables[t]
    }

    /// The module's converted const-object table (`LOAD_CONST_OBJ`
    /// reads `table[u]` — the same object every time, as in the
    /// Zig's `obj_table`).
    pub fn const_table(&self, idx: usize) -> &[Value] {
        let t = self.codes[idx].const_table;
        &self.const_tables[t]
    }

    pub fn module(&self, idx: usize) -> GcRef {
        self.codes[idx].module
    }
}

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
    /// The `__build_class__` builtin (stub until C12a's
    /// `type_call` — calling it raises a bare TypeError).
    pub build_class_builtin: Value,
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
                builtins: GcRef(0),
                empty_tuple,
                ellipsis,
                frames: HashMap::new(),
                pending_exc: None,
                exc,
                next_builtin: Value::None,
                build_class_builtin: Value::None,
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
        // `__build_class__` (stub until C12a's `type_call`).
        let bc_id = static_qstr_id(b"__build_class__").expect("__build_class__ is a static qstr");
        let bc_fun = ctx.alloc(
            Some(vmt.fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: bc_id,
                n_args: fun_n_args(1, 255),
                func: build_class_builtin_fn,
                kw_func: None,
            }),
        );
        let bc_val = Value::Heap(bc_fun);
        bind("__build_class__", bc_val, &mut pairs);

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
        // Pin everything live: the types, the singletons, the
        // builtins dict and the three builtin values.
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
        let module = mpy::read(data).map_err(VmError::Mpy)?;
        // (Intern with `&mut self.pool` BEFORE the split — the
        // ctx carries the pool shared.)
        let name_id = self
            .pool
            .from_strn(name.as_bytes())
            .map_err(|_| VmError::OutOfMemory)?;
        let qstr_table =
            resolve_qstr_table(&mut self.pool, &module.qstrs).map_err(|_| VmError::OutOfMemory)?;
        let module_type = self.state.vm_types.module;
        // Convert the module's const-object table once
        // (`obj_from_entry`) and pin every value in `roots.extra`
        // (inserted at `extra_base`, so `rebuild_frame_roots`
        // keeps them) — `LOAD_CONST_OBJ` must push the SAME
        // object on every load, as the Zig's `obj_table` does.
        // Each value is pinned immediately after conversion so a
        // GC during the next conversion cannot collect it.
        let const_table: Vec<Value> = {
            let (mut ctx, state) = self.split();
            let mut table = Vec::with_capacity(module.objs.len());
            for entry in &module.objs {
                let types = ctx.types;
                let v = crate::runtime::vm_raise::obj_from_entry(
                    &mut ctx, types, state.ellipsis, entry,
                )
                .map_err(|_| VmError::OutOfMemory)?;
                ctx.roots.extra.insert(state.extra_base, v);
                state.extra_base += 1;
                table.push(v);
            }
            table
        };
        let (module_val, code_idx) = {
            let (mut ctx, state) = self.split();
            let module_val = new_module(&mut ctx, name_id as u32, module_type);
            let module_ref = match module_val {
                Value::Heap(r) => r,
                _ => unreachable!("new_module returns a heap module"),
            };
            let code_idx = state.code_store.register_module(
                module.rc.clone(),
                qstr_table,
                const_table,
                module_ref,
            );
            state.modules.insert(name_id as u32, module_ref);
            // The module's globals dict is its `dict` — the
            // frame's globals while the body runs.
            let globals = match ctx.arena.get(module_ref) {
                Obj::Module(m) => m.dict,
                _ => unreachable!("module object"),
            };
            // Pin the module object (its dict is traced through it).
            drop(ctx);
            self.pin(module_val);
            self.state.globals = globals;
            (module_val, code_idx)
        };
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
    fn call_module_fun(&mut self, code_idx: usize, module_val: Value) -> GcRef {
        let fun_bc_ty = self.state.vm_types.fun_bc;
        let (mut ctx, _state) = self.split();
        let module_ref = match module_val {
            Value::Heap(r) => r,
            _ => unreachable!("module value"),
        };
        let name = static_qstr_id(b"<module>").unwrap_or(0);
        new_fun_bc(
            &mut ctx,
            fun_bc_ty,
            name,
            0,
            code_idx,
            None,
            None,
            Some(module_ref),
        )
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

/// Resolve a module's `.mpy` qstr table to ids: static entries
/// pass through, `New` bytes are interned into the (mutable)
/// pool. The `CallCtx` carries the pool shared, so callers
/// resolve the table with `&mut self.pool` BEFORE splitting the
/// VM into a ctx.
pub fn resolve_qstr_table(
    pool: &mut RuntimeQstr,
    qstrs: &[QstrEntry],
) -> PyResult<Vec<u16>> {
    let mut table = Vec::with_capacity(qstrs.len());
    for q in qstrs {
        match q {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => table.push(pool.from_strn(bytes)? as u16),
        }
    }
    Ok(table)
}
