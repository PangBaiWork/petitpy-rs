//! Name / global loads and stores (`runtime.zig:288-351`).
//!
//! The Zig reads the thread state (`MpStateThread.localsObj()` /
//! `globalsObj()`); Rust takes the dicts as explicit parameters
//! (C5 precedent — there is no global thread state here, C13's
//! `VmState` owns the thread-level slot).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{
    dict_delete, dict_get_value, dict_store, DictError,
};
use crate::objects::value::Value;
use crate::runtime::runtime::{dunder, Builtins, RtError};

/// Map a `DictError` onto `RtError` (`KeyError` is a lookup miss,
/// handled by the callers — this is for the other variants).
pub(crate) fn dict_err_to_rt(e: DictError) -> RtError {
    match e {
        DictError::TypeError(m) => RtError::TypeError(m.to_string()),
        DictError::ValueError(m) => RtError::ValueError(m.to_string()),
        DictError::OutOfMemory => RtError::OutOfMemory,
        DictError::PyRaise => RtError::PyRaise,
        // `KeyError` never reaches here (callers match it first);
        // `KeyErrorPopitemEmpty` cannot come out of a lookup/store.
        DictError::KeyError(_) | DictError::KeyErrorPopitemEmpty => RtError::PyRaise,
    }
}

/// Look a qstr key up in a dict: `Ok(Some(v))` = found,
/// `Ok(None)` = `KeyError` (miss), `Err` = any other dict error.
pub(crate) fn dict_lookup_qstr(
    ctx: &mut CallCtx,
    dict: Value,
    qst: u32,
) -> Result<Option<Value>, RtError> {
    match dict_get_value(ctx, dict, Value::Qstr(qst)) {
        Ok(v) => Ok(Some(v)),
        Err(DictError::KeyError(_)) => Ok(None),
        Err(e) => Err(dict_err_to_rt(e)),
    }
}

/// `mp_load_name` (`runtime.zig:288-298`): outer scope
/// (`locals == globals`) falls straight to `mp_load_global`;
/// otherwise the locals dict first, then globals.
pub fn load_name(
    ctx: &mut CallCtx,
    qst: u32,
    locals: Value,
    globals: Value,
    builtins: &Builtins,
) -> Result<Value, RtError> {
    if locals != globals {
        if let Some(v) = dict_lookup_qstr(ctx, locals, qst)? {
            return Ok(v);
        }
    }
    load_global(ctx, qst, globals, builtins)
}

/// `mp_load_global` (`runtime.zig:301-325`): globals, then the
/// full-config override dict (`builtins.X = Y`), then the builtins
/// globals — else `NameError("name '%q' isn't defined")`.
pub fn load_global(
    ctx: &mut CallCtx,
    qst: u32,
    globals: Value,
    builtins: &Builtins,
) -> Result<Value, RtError> {
    if let Some(v) = dict_lookup_qstr(ctx, globals, qst)? {
        return Ok(v);
    }
    if let Some(od) = builtins.override_dict {
        if let Some(v) = dict_lookup_qstr(ctx, od, qst)? {
            return Ok(v);
        }
    }
    if let Some(v) = dict_lookup_qstr(ctx, builtins.globals, qst)? {
        return Ok(v);
    }
    let name = String::from_utf8_lossy(ctx.pool.qstr_str(qst as u16)).into_owned();
    Err(RtError::NameError(format!("name '{name}' isn't defined")))
}

/// `mp_load_build_class` (`runtime.zig:328-336`): the override
/// dict's `__build_class__` when installed (full), else the
/// `__build_class__` object D1 owns (taken as a parameter).
pub fn load_build_class(
    ctx: &mut CallCtx,
    builtins: &Builtins,
    default_build_class: Value,
) -> Result<Value, RtError> {
    if let Some(od) = builtins.override_dict {
        if let Some(q) = dunder(b"__build_class__") {
            if let Some(v) = dict_lookup_qstr(ctx, od, q)? {
                return Ok(v);
            }
        }
    }
    Ok(default_build_class)
}

/// `mp_store_name` (`runtime.zig:337-339`): store into the locals
/// dict. Errors are ignored, exactly as the Zig's `_ =`.
pub fn store_name(ctx: &mut CallCtx, locals: Value, qst: u32, value: Value) {
    let _ = dict_store(ctx, locals, Value::Qstr(qst), value);
}

/// `mp_store_global` (`runtime.zig:340-342`): store into the
/// globals dict. Errors are ignored, exactly as the Zig's `_ =`.
pub fn store_global(ctx: &mut CallCtx, globals: Value, qst: u32, value: Value) {
    let _ = dict_store(ctx, globals, Value::Qstr(qst), value);
}

/// `mp_delete_name` (`runtime.zig:343-345`): delete from the locals
/// dict. Errors are ignored, exactly as the Zig's `_ =`.
pub fn delete_name(ctx: &mut CallCtx, locals: Value, qst: u32) {
    let _ = dict_delete(ctx, locals, Value::Qstr(qst));
}

/// `mp_delete_global` (`runtime.zig:346-348`): delete from the
/// globals dict. Errors are ignored, exactly as the Zig's `_ =`.
pub fn delete_global(ctx: &mut CallCtx, globals: Value, qst: u32) {
    let _ = dict_delete(ctx, globals, Value::Qstr(qst));
}
