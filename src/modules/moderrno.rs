//! The `errno` builtin module (`src/modules/moderrno.zig`, 185 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/moderrno.c`):
//! the `errorcode` dict (int → name, 22 entries) plus the 22
//! `E*` integer constants — 24 entries (`__name__` + `errorcode`
//! + 22). Values are the Linux libc numbers (verified against
//! the reference binary 2026-11-18: `ENOENT == 2`,
//! `EINPROGRESS == 115`, `errorcode[2] == 'ENOENT'`).
//!
//! The Zig also exports `mp_errno_to_str` (used by `objexcept.c`
//! to render `OSError` args); the Rust `OSError` rendering
//! already carries the same 22-entry table (C12b,
//! `objexcept.rs`) — no second copy is built here.

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, new_d4_module, store};
use crate::objects::containers::objdict::dict_store;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::strings::objstr_a::new_str;
use crate::objects::value::{DictData, GcRef, MapData, Obj, Value};
use crate::pyerr::PyResult;

/// The 22 `E*` constants (`moderrno.zig`, values from vendor
/// `py/mperrno.h` — the Linux libc numbers), in table order.
const ERRORS: &[(&str, i64)] = &[
    ("EPERM", 1),
    ("ENOENT", 2),
    ("EIO", 5),
    ("EBADF", 9),
    ("EAGAIN", 11),
    ("ENOMEM", 12),
    ("EACCES", 13),
    ("EEXIST", 17),
    ("ENODEV", 19),
    ("EISDIR", 21),
    ("EINVAL", 22),
    ("EOPNOTSUPP", 95),
    ("EADDRINUSE", 98),
    ("ECONNABORTED", 103),
    ("ECONNRESET", 104),
    ("ENOBUFS", 105),
    ("ENOTCONN", 107),
    ("ETIMEDOUT", 110),
    ("ECONNREFUSED", 111),
    ("EHOSTUNREACH", 113),
    ("EALREADY", 114),
    ("EINPROGRESS", 115),
];

/// `build_errno_module` (`moderrno.zig` tables): 24 entries.
/// The `errorcode` dict maps int → name string (keys are
/// integers, so its map is NOT all-qstrs — the Zig sets
/// `all_keys_are_qstrs = 0` for it).
pub fn build_errno_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let _ = fun_builtin_var;
    let (module, dict) = new_d4_module(ctx, "errno", module_type);
    // The `errorcode` dict (a plain dict — integer keys).
    let errorcode = ctx.alloc(
        Some(ctx.types.dict),
        Obj::Dict(DictData { map: MapData::new() }),
    );
    let errorcode_v = Value::Heap(errorcode);
    ctx.roots.push(errorcode_v);
    for (name, num) in ERRORS {
        let name_v = new_str(ctx, ctx.types, name.as_bytes());
        ctx.roots.push(name_v);
        dict_store(ctx, errorcode_v, Value::SmallInt(*num), name_v)
            .map_err(|e| ctx.record_typed(e))?;
    }
    if let Obj::Dict(d) = ctx.arena.get_mut(errorcode) {
        d.map.is_fixed = true;
    }
    store(ctx, dict, "errorcode", errorcode_v);
    for (name, num) in ERRORS {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("errno constant is not a static qstr: {name}"));
        dict_store(ctx, dict, Value::Qstr(id), Value::SmallInt(*num))
            .map_err(|e| ctx.record_typed(e))
            .expect("errno dict store cannot fail on a fresh dict");
    }
    // Keep the `errorcode` binding visible (the `new_module`
    // helper stores `__name__` only).
    fix_dict(ctx, dict);
    Ok(module)
}
