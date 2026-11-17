//! Import (`runtime/builtinimport.zig`, 190 lines) — `__import__`
//! (`mp_builtin___import___default`, `MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN`
//! 1..5 args) and its `importImpl` / `processComponent`.
//!
//! The search itself lives in `import_search.rs`
//! (`try_load_component`); this file owns the dotted-name walk,
//! the builtin-module lookups around it, the return-value rule,
//! and the `__import__` entry point.
//!
//! Zig facts ported verbatim (verified against
//! `builtinimport.zig:69-171`):
//! * `importImpl` does NOT validate `globals` / `locals` /
//!   `fromlist` — it reads only `args[0]` (name), `args[3]`
//!   (fromtuple, default `None`) and `args[4]` (level, default
//!   0). (An earlier plan guessed a dict check — wrong.)
//! * Level: `(isize)(bitcast(args[4])) >> 1 != 0` → relative
//!   import. For a small int that is the value itself; for ANY
//!   other REPR_A word (heap pointer, qstr, `None`, `True`) the
//!   shift is non-zero. So: `SmallInt(0)` passes, everything
//!   else raises `NotImplementedError("relative import")`.
//! * The dotted walk is the Zig's index loop (`i` from 1 — a
//!   leading `.` is NOT a separator, so `".foo"` is one
//!   component `".foo"`; `"a..b"` yields `"a"`, `""`, `"b"`).
//! * Return: `fromtuple is None` → the TOP module, else the
//!   LEAF (any non-`None` fromlist — even `()` — returns the
//!   leaf; the opcode path never passes an empty tuple).
//! * `processComponent`: cached (`sys.modules`, by full-name
//!   bytes) → top-level builtin (non-ext) → search → builtin
//!   (ext); submodule: search → hook onto the parent
//!   (`mp_store_attr`) → a built-in parent's module attribute.
//!   Miss (full config): `ModuleNotFoundError("No module named
//!   'FULL'")`, or `"…; 'PARENT' is not a package"` when the
//!   parent has no `__path__` (the `__path__` probe swallows
//!   any error — `pending_exc = 0` unconditionally).
//! * The full dotted name is interned by the CALLER's `InternFn`
//!   (once per component); interning works on the dispatch path
//!   too (`from_strn` takes `&self` since C15b).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::import_search::{
    load_attr_maybe, loaded_lookup_by_bytes, try_load_component, value_str_bytes,
};
use crate::runtime::runtime::dunder;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};

/// Look a name up in a builtin-module table BYTES (no interning
/// needed — the dispatch path cannot intern a genuinely-new
/// name; the tables are small).
fn builtin_lookup_by_bytes(
    ctx: &CallCtx,
    state: &VmState,
    name: &[u8],
    ext: bool,
) -> Option<Value> {
    let table = if ext {
        &state.extensible_modules
    } else {
        &state.builtin_modules
    };
    for (q, v) in table {
        if ctx.pool.qstr_str(*q as u16) == name {
            return Some(*v);
        }
    }
    None
}

/// Is `v` an EXACT module (`mirrors.isExactType(v,
/// &mp_type_module)` — header type == the module type)?
fn is_exact_module(ctx: &CallCtx, state: &VmState, v: Value) -> bool {
    matches!(v, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Module(_)))
        && obj_get_type(ctx.arena, &ctx.types, v) == Some(state.vm_types.module)
}

/// The full-config miss error (`processComponent` tail,
/// `builtinimport.zig:115-130`).
fn module_not_found(
    ctx: &mut CallCtx,
    state: &mut VmState,
    full_slice: &[u8],
    level_slice: &[u8],
    parent: Option<Value>,
) -> Value {
    if let Some(parent_obj) = parent {
        // Probe `__path__`, swallowing ANY error (the Zig sets
        // `pending_exc = 0` unconditionally after the probe).
        let path_q = dunder(b"__path__").expect("__path__ is a static qstr");
        let (dest, _) =
            load_attr_maybe(ctx, state, parent_obj, path_q).unwrap_or((None, None));
        if dest.is_none() {
            let parent_len = full_slice.len() - level_slice.len() - 1;
            let parent_name = String::from_utf8_lossy(&full_slice[..parent_len]);
            let full_name = String::from_utf8_lossy(full_slice);
            return new_exception_msg(
                ctx,
                state.exc.module_not_found_error,
                &format!(
                    "No module named '{full_name}'; '{parent_name}' is not a package"
                ),
            );
        }
    }
    let full_name = String::from_utf8_lossy(full_slice);
    new_exception_msg(
        ctx,
        state.exc.module_not_found_error,
        &format!("No module named '{full_name}'"),
    )
}

/// Process one dotted component (`builtinimport.zig:72-131`).
/// `full_slice` is the absolute path up to here (`"foo.bar"`),
/// `level_slice` just the current component (`"bar"`),
/// `parent` the parent module object or `None` for top-level.
/// `intern` interns the full dotted name (once per component).
fn process_component(
    ctx: &mut CallCtx,
    state: &mut VmState,
    full_slice: &[u8],
    level_slice: &[u8],
    parent: Option<Value>,
    intern: &mut dyn FnMut(&[u8]) -> PyResult<u32>,
) -> Result<Value, Value> {
    let full_qstr = intern(full_slice)
        .map_err(|e| exc_from_pyerror(ctx, state, e))?;
    // Already loaded? Reuse.
    if let Some(cached) = loaded_lookup_by_bytes(ctx, state, full_slice) {
        return Ok(cached);
    }
    if parent.is_none() {
        // Top-level: built-ins first (non-ext), then the search,
        // then built-ins (ext).
        if let Some(nb) = builtin_lookup_by_bytes(ctx, state, level_slice, false) {
            return Ok(nb);
        }
        if let Some(found) =
            try_load_component(ctx, state, full_qstr, level_slice, None)?
        {
            return Ok(found);
        }
        if let Some(ext) = builtin_lookup_by_bytes(ctx, state, level_slice, true) {
            return Ok(ext);
        }
    } else {
        let parent_obj = parent.expect("parent is Some here");
        // The component qstr is needed twice below (hookup +
        // attribute probe) — intern once.
        let level_qstr = intern(level_slice)
            .map_err(|e| exc_from_pyerror(ctx, state, e))?;
        if let Some(found) = try_load_component(
            ctx,
            state,
            full_qstr,
            level_slice,
            Some(parent_obj),
        )? {
            // Hook the submodule onto its parent so attribute
            // access works (vendor `builtinimport.c:524`). For a
            // module parent this is a plain dict store
            // (`module_attr`'s store path); a fixed-dict parent
            // raises the same bare `TypeError` either way.
            let dict = match parent_obj {
                Value::Heap(r) => match ctx.arena.get(r) {
                    Obj::Module(m) => m.dict,
                    _ => unreachable!("parent is a module"),
                },
                _ => unreachable!("parent is a module"),
            };
            if let Err(e) = dict_store(
                ctx,
                Value::Heap(dict),
                Value::Qstr(level_qstr),
                found,
            ) {
                let err = ctx.record_typed(e);
                return Err(exc_from_pyerror(ctx, state, err));
            }
            return Ok(found);
        }
        // Neither on disk nor frozen: a built-in parent may serve
        // the submodule as an attribute (`import os.path`,
        // `os.__getattr__`).
        let (dest, self_) = load_attr_maybe(ctx, state, parent_obj, level_qstr)?;
        if let (Some(v), None) = (dest, self_) {
            if is_exact_module(ctx, state, v) {
                return Ok(v);
            }
        }
    }
    Err(module_not_found(ctx, state, full_slice, level_slice, parent))
}

/// `importImpl` (`builtinimport.zig:133-170`): the 5-arg
/// `__import__` body. `args` is `[name, globals, locals,
/// fromlist, level]` (or a prefix of it — defaults `None` /
/// `0`). `globals` / `locals` are read but never used (the Zig
/// ignores them); `fromlist` decides top-vs-leaf only through
/// `isNone`. Errors are the exception object itself.
pub fn import_impl(
    ctx: &mut CallCtx,
    state: &mut VmState,
    args: &[Value],
    intern: &mut dyn FnMut(&[u8]) -> PyResult<u32>,
) -> Result<Value, Value> {
    // Reject relative imports (`builtinimport.zig:134-138`).
    if args.len() >= 5 && !matches!(args[4], Value::SmallInt(0)) {
        return Err(new_exception_msg(
            ctx,
            state.exc.not_implemented_error,
            "relative import",
        ));
    }
    // Pull out the module-name bytes.
    let Some(name_bytes) = value_str_bytes(ctx, args[0]) else {
        return Err(new_exception_msg(
            ctx,
            state.exc.import_error,
            "module not found",
        ));
    };
    if name_bytes.is_empty() {
        return Err(new_exception_msg(
            ctx,
            state.exc.import_error,
            "module not found",
        ));
    }
    let name_slice = name_bytes.as_slice();
    let name_len = name_slice.len();
    // fromtuple decides whether to return the top-level or the
    // leaf (`builtinimport.zig:149-151`).
    let fromtuple = args.get(3).copied().unwrap_or(Value::None);
    // Iterate the dotted path (the Zig's index loop, `i` from
    // 1 — a leading `.` is not a separator).
    let mut top_module: Option<Value> = None;
    let mut outer_module: Option<Value> = None;
    let mut current_start = 0;
    let mut i = 1;
    while i <= name_len {
        if i == name_len || name_slice[i] == b'.' {
            let full_slice = &name_slice[..i];
            let level_slice = &name_slice[current_start..i];
            let m = process_component(
                ctx,
                state,
                full_slice,
                level_slice,
                outer_module,
                intern,
            )?;
            outer_module = Some(m);
            if top_module.is_none() {
                top_module = Some(m);
            }
            current_start = i + 1;
        }
        i += 1;
    }
    Ok(if fromtuple == Value::None {
        top_module.expect("name is non-empty, so a component ran")
    } else {
        outer_module.expect("name is non-empty, so a component ran")
    })
}

/// The C14 `import_default` seam: `import_impl` as a
/// `RuntimeSeams` closure (`runtime_import::import_name` calls
/// it with the 5 `__import__` args). An exception object is
/// stashed in `state.pending_exc` and `PyRaise` returned
/// (`PyError` cannot carry an object — the C14 factory
/// contract, see `scripts/import_result.txt` §3).
pub fn import_default_seam<'a>(
    state: &'a mut VmState,
) -> impl FnMut(&mut CallCtx, &[Value]) -> PyResult<Value> + 'a {
    move |ctx, args| {
        let pool = ctx.pool;
        let mut intern = |b: &[u8]| {
            pool.from_strn(b).map(u32::from)
        };
        match import_impl(ctx, state, args, &mut intern) {
            Ok(v) => Ok(v),
            Err(exc) => {
                state.pending_exc = Some(exc);
                Err(PyError::PyRaise)
            }
        }
    }
}

/// `mp_builtin___import__` as a `BuiltinFn` — a stub, reachable
/// only on a direct call bypassing the `Vm` (the interception
/// in `call_function_ctx` handles the real calls, as `eval`'s
/// stub does). A bare `Err(PyRaise)` with nothing recorded, so
/// the VM falls back to a bare `TypeError`.
pub(crate) fn import_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// The dispatch-path entry for the `__import__` builtin
/// (intercepted by GcRef identity in `call_function_ctx`):
/// arity 1–5, no kwargs (the `eval`/`compile` precedent), then
/// `import_impl` with a real interner (`from_strn` takes `&self`
/// since C15b).
pub fn call_import_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 0 || n_args > 5 || n_kw != 0 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    import_impl(ctx, state, args, &mut intern)
}
