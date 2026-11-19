//! Import entry points (`runtime.zig:1468-1575`):
//! `mp_import_name` / `mp_import_from` / `mp_import_all`.
//!
//! `mp_import_name(name, fromlist, level)` builds the 5-arg
//! call `[name, globals, None, fromlist, level]` — the
//! globals come from the caller (the Zig reads thread
//! globals; this lane takes them as an explicit parameter,
//! per the no-global-state rule) and the locals slot is
//! `None` (the Zig's `TODO should be locals`). A `__import__`
//! in the builtins override dict is called `(5, 0)`; else
//! the `import_default` seam (C15/D) runs.
//!
//! `mp_import_from` is the vendor `mp_import_from` under
//! `EXTERNAL_IMPORT=1`: an attribute miss falls back to a
//! submodule import — `"pkg.name"` in a 512-byte buffer,
//! interned, then `import_name(dot, True, 0)` (fromlist
//! `True` forces the leaf module). `mp_import_all` iterates
//! the module globals map in insertion order and stores the
//! non-underscore names into LOCALS (an explicit parameter
//! here); the `__all__` block is dead
//! (`MICROPY_MODULE___ALL__=0`).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::value::{Obj, Value};
use crate::runtime::runtime::{dunder, Builtins, RtError, RuntimeSeams};
use crate::runtime::runtime_call::call_function_n_kw;
use crate::runtime::runtime_method::load_method_maybe;
use crate::runtime::runtime_names::{dict_lookup_qstr, store_name};

/// The string bytes of a value: a qstr id from the pool, a
/// heap `Str`'s bytes, else `None` (the Zig's
/// `mp_obj_str_get_str` returning NULL). `pub(crate)` for the
/// G4/H9 `import_from` fallback peek (`vm_rt_iter.rs`).
pub(crate) fn str_bytes(ctx: &CallCtx, v: Value) -> Option<Vec<u8>> {
    match v {
        Value::Qstr(id) => Some(ctx.pool.qstr_str(id as u16).to_vec()),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => Some(s.bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `mp_import_name` (`runtime.zig:1468-1485`).
pub fn import_name(
    ctx: &mut CallCtx,
    name_q: u32,
    fromlist: Value,
    level: Value,
    globals: Value,
    builtins: &Builtins,
    seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let args = [
        Value::Qstr(name_q),
        globals,
        Value::None,
        fromlist,
        level,
    ];
    if let Some(od) = builtins.override_dict {
        let import_q = dunder(b"__import__").expect("__import__ is a static qstr");
        if let Some(custom) = dict_lookup_qstr(ctx, od, import_q)? {
            return call_function_n_kw(ctx, custom, 5, 0, &args);
        }
    }
    (seams.import_default)(ctx, &args).map_err(RtError::from)
}

/// `mp_import_from` (`runtime.zig:1492-1546`).
#[allow(clippy::too_many_arguments)]
pub fn import_from(
    ctx: &mut CallCtx,
    module: Value,
    name_q: u32,
    globals: Value,
    builtins: &Builtins,
    intern: crate::objects::iters::iter::InternFn,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let name_str =
        String::from_utf8_lossy(ctx.pool.qstr_str(name_q as u16)).into_owned();
    let cant_import = || RtError::ImportError(format!("can't import name '{name_str}'"));
    // Attribute first (bound method or plain value).
    if let Some(dest) = load_method_maybe(ctx, module, name_q, next_obj)? {
        return Ok(match dest.self_ {
            Some(self_obj) => Value::Heap(new_bound_meth(
                ctx,
                ctx.types.boundmeth,
                dest.func,
                self_obj,
            )),
            None => dest.func,
        });
    }
    // Attribute miss: fall back to a submodule import if the
    // module is a package (has `__path__`).
    let path_q = dunder(b"__path__").expect("__path__ is a static qstr");
    if load_method_maybe(ctx, module, path_q, next_obj)?.is_none() {
        return Err(cant_import());
    }
    // `__name__` as a string.
    let name_attr_q = dunder(b"__name__").expect("__name__ is a static qstr");
    let pkg_name = match load_method_maybe(ctx, module, name_attr_q, next_obj)? {
        Some(dest) => match str_bytes(ctx, dest.func) {
            Some(b) => b,
            None => return Err(cant_import()),
        },
        None => return Err(cant_import()),
    };
    // `"pkg.name"` in a 512-byte buffer.
    let sub_name = ctx.pool.qstr_str(name_q as u16);
    let total = pkg_name.len() + 1 + sub_name.len();
    if total > 512 {
        return Err(cant_import());
    }
    let mut dot_name = Vec::with_capacity(total);
    dot_name.extend_from_slice(&pkg_name);
    dot_name.push(b'.');
    dot_name.extend_from_slice(sub_name);
    let dot_q = intern(&dot_name).map_err(RtError::from)?;
    // fromlist `True` forces the leaf-module return.
    import_name(ctx, dot_q, Value::Bool(true), Value::SmallInt(0), globals, builtins, seams)
}

/// `mp_import_all` (`runtime.zig:1550-1570`): iterate the
/// module globals map and store the non-underscore names
/// into LOCALS. A non-str key raises bare `PyRaise` (the
/// Zig's `mp_obj_str_get_str(key) orelse return error.
/// PyRaise`).
pub fn import_all(
    ctx: &mut CallCtx,
    module: Value,
    locals: Value,
    intern: crate::objects::iters::iter::InternFn,
) -> Result<(), RtError> {
    let dict_ref = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(RtError::PyRaise),
        },
        _ => return Err(RtError::PyRaise),
    };
    let entries = match ctx.arena.get(dict_ref) {
        Obj::Dict(d) => d.map.entries.clone(),
        _ => return Err(RtError::PyRaise),
    };
    for e in entries {
        let Some(key) = e.key else { continue };
        let name_bytes = match str_bytes(ctx, key) {
            Some(b) => b,
            None => return Err(RtError::PyRaise),
        };
        if name_bytes.first() == Some(&b'_') {
            continue;
        }
        let qname = match key {
            Value::Qstr(id) => id,
            Value::Heap(_) => intern(&name_bytes).map_err(RtError::from)?,
            _ => return Err(RtError::PyRaise),
        };
        store_name(ctx, locals, qname, e.value);
    }
    Ok(())
}
