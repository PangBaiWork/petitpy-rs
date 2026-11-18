//! The `grp` builtin module (`src/modules/modgrp.zig`, 147 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modgrp.c`):
//! `getgrnam`, `getgrgid` — 3 entries (`__name__` + 2). Both
//! return a 4-field attrtuple (`gr_name`, `gr_passwd`,
//! `gr_gid`, `gr_mem` — the last a list of member names).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `getgrnam('root')` == `(gr_name='root', gr_passwd='x',
//!   gr_gid=0, gr_mem=[])`.
//! - A missing name or gid raises `KeyError` with the
//!   argument; a non-string name raises `KeyError` too.
//! - `getgrgid` casts its argument to `c_uint` before the
//!   lookup (the Zig: `mp_obj_get_int` → `@intCast(c_uint)`).
//! - Divergence: a negative or huge gid (`getgrgid(-1)`) ABORTS
//!   the reference (`@intCast` panic, rc=134); Rust wraps
//!   (`as u32`) and raises `KeyError` instead (same class as
//!   `pwd`).
//!
//! Reads `/etc/group` with `std::fs::read_to_string`
//! (`sysdb.rs`) — same file, same fields as the Zig's `open` +
//! `readline`. No NSS/LDAP lookup.

use crate::memory::gc::CallCtx;
use crate::modules::sysdb::{find_row, read_etc_file};
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objattrtuple::attrtuple_new;
use crate::objects::containers::objlist::new_list;
use crate::objects::obj::obj_get_int;
use crate::objects::primitives::ops::{str_bytes_of, QstrResolver};
use crate::objects::strings::objstr_a::new_str;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

/// The 4 `struct group` field names, in order.
const GR_FIELDS: [&str; 4] = ["gr_name", "gr_passwd", "gr_gid", "gr_mem"];

/// `mp_obj_new_attrtuple` for one `group` row: fields 0,1 are
/// strings; field 2 is the gid int; field 3 is the
/// comma-separated member list (`gr_mem` — empty → `[]`).
fn grp_attrtuple(ctx: &mut CallCtx, row: &[String]) -> Value {
    let fields: Vec<u32> = GR_FIELDS
        .iter()
        .map(|f| ctx.pool.from_strn(f.as_bytes()).map(u32::from).unwrap_or(0))
        .collect();
    let s = |ctx: &mut CallCtx, i: usize| {
        new_str(ctx, ctx.types, row.get(i).map(String::as_str).unwrap_or("").as_bytes())
    };
    let gid = row.get(2).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let members: Vec<Value> = row
        .get(3)
        .map(|m| {
            m.split(',')
                .filter(|name| !name.is_empty())
                .map(|name| new_str(ctx, ctx.types, name.as_bytes()))
                .collect()
        })
        .unwrap_or_default();
    let mem_list = Value::Heap(new_list(ctx, ctx.types.list, &members));
    let items = vec![s(ctx, 0), s(ctx, 1), Value::SmallInt(gid), mem_list];
    attrtuple_new(ctx, ctx.types.attrtuple, fields, items)
}

/// A str/bytes argument as a UTF-8 string (`str_bytes_of`,
/// the `modjson.rs` resolver shape); `None` for anything else.
fn name_arg(ctx: &mut CallCtx, v: Value) -> Option<String> {
    let pool = ctx.pool;
    let resolver: QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    str_bytes_of(ctx.arena, ctx.types, resolver, v).and_then(|b| String::from_utf8(b).ok())
}

/// `py_grp_getgrnam` (`modgrp.zig`): look up by name; a
/// missing name (or a non-string argument) raises
/// `KeyError(arg)`.
fn grp_getgrnam_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let rows = read_etc_file("/etc/group");
    let found = name_arg(ctx, args[0]).and_then(|s| find_row(&rows, 0, &s).cloned());
    match found {
        Some(row) => Ok(grp_attrtuple(ctx, &row)),
        None => {
            ctx.record_raise_value("KeyError", args[0]);
            Err(PyError::PyRaise)
        },
    }
}

/// `py_grp_getgrgid` (`modgrp.zig`): look up by gid (cast to
/// `c_uint` — wrapping, see the module docs); a missing gid
/// raises `KeyError(arg)`.
fn grp_getgrgid_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let gid = obj_get_int(ctx, ctx.types, args[0]).map_err(|e| ctx.record_typed(e))?;
    let rows = read_etc_file("/etc/group");
    let needle = (gid as u32).to_string();
    match find_row(&rows, 2, &needle) {
        Some(row) => Ok(grp_attrtuple(ctx, row)),
        None => {
            ctx.record_raise_value("KeyError", args[0]);
            Err(PyError::PyRaise)
        },
    }
}

/// `build_grp_module` (`modgrp.zig` table): 3 entries.
pub fn build_grp_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let (module, dict) = new_d4_module(ctx, "grp", module_type);
    let getgrnam = mk_fun(ctx, fun_builtin_var, "getgrnam", 1, 1, grp_getgrnam_builtin, None);
    let getgrgid = mk_fun(ctx, fun_builtin_var, "getgrgid", 1, 1, grp_getgrgid_builtin, None);
    store(ctx, dict, "getgrnam", getgrnam);
    store(ctx, dict, "getgrgid", getgrgid);
    fix_dict(ctx, dict);
    Ok(module)
}
