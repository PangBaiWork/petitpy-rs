//! The `pwd` builtin module (`src/modules/modpwd.zig`, 147 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modpwd.c`):
//! `getpwnam`, `getpwuid` — 3 entries (`__name__` + 2). Both
//! return a 7-field attrtuple (`pw_name`, `pw_passwd`,
//! `pw_uid`, `pw_gid`, `pw_gecos`, `pw_dir`, `pw_shell`).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `getpwnam('root')` == `(pw_name='root', pw_passwd='x',
//!   pw_uid=0, pw_gid=0, pw_gecos='root', pw_dir='/root',
//!   pw_shell='/bin/bash')` (the `pw_passwd` field is the
//!   shadow placeholder `'x'`, not the real hash — same as
//!   libc's `getpwnam` on a shadowed system).
//! - A missing name or uid raises `KeyError` with the
//!   argument (`KeyError('nosuchuser123')`, `KeyError(99999)`);
//!   a non-string name (`getpwnam(123)`) raises `KeyError` too.
//! - `getpwuid` casts its argument to `c_uint` before the
//!   lookup (the Zig: `mp_obj_get_int` → `@intCast(c_uint)`).
//! - Divergence: a negative or huge uid (`getpwuid(-1)`,
//!   `getpwuid(2**40)`) ABORTS the reference (`@intCast`
//!   panic, rc=134); Rust wraps (`as u32`) and raises
//!   `KeyError` instead — the same abort-divergence class as
//!   the C8b/C9b original-abort cases.
//!
//! The Zig reads `/etc/passwd` through its own `open` +
//! `readline` (`modpwd.zig`); Rust reads it with
//! `std::fs::read_to_string` (`sysdb.rs` — same file, same
//! fields). No NSS/LDAP lookup is attempted — on this machine
//! the file IS the database.

use crate::memory::gc::CallCtx;
use crate::modules::sysdb::{find_row, read_etc_file};
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objattrtuple::attrtuple_new;
use crate::objects::obj::obj_get_int;
use crate::objects::strings::objstr_a::new_str;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

/// The 7 `struct passwd` field names, in order.
const PW_FIELDS: [&str; 7] = [
    "pw_name", "pw_passwd", "pw_uid", "pw_gid", "pw_gecos", "pw_dir", "pw_shell",
];

/// `mp_obj_new_attrtuple` for one `passwd` row: fields 0,1,4,5,6
/// are strings; fields 2,3 are the uid/gid ints (parsed from
/// the file — a malformed number yields 0, matching the Zig's
/// `parseInt` default).
fn pwd_attrtuple(ctx: &mut CallCtx, row: &[String]) -> Value {
    let fields: Vec<u32> = PW_FIELDS
        .iter()
        .map(|f| ctx.pool.from_strn(f.as_bytes()).map(u32::from).unwrap_or(0))
        .collect();
    let s = |ctx: &mut CallCtx, i: usize| {
        new_str(ctx, ctx.types, row.get(i).map(String::as_str).unwrap_or("").as_bytes())
    };
    let n = |i: usize| row.get(i).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let items = vec![
        s(ctx, 0),
        s(ctx, 1),
        Value::SmallInt(n(2)),
        Value::SmallInt(n(3)),
        s(ctx, 4),
        s(ctx, 5),
        s(ctx, 6),
    ];
    attrtuple_new(ctx, ctx.types.attrtuple, fields, items)
}

/// `py_pwd_getpwnam` (`modpwd.zig`): look up by name; a
/// missing name (or a non-string argument) raises
/// `KeyError(arg)`.
fn pwd_getpwnam_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = args[0];
    let pool = ctx.pool;
    let resolver: crate::objects::primitives::ops::QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    let rows = read_etc_file("/etc/passwd");
    let found = crate::objects::primitives::ops::str_bytes_of(ctx.arena, ctx.types, resolver, name)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| find_row(&rows, 0, &s).cloned());
    match found {
        Some(row) => Ok(pwd_attrtuple(ctx, &row)),
        None => {
            ctx.record_raise_value("KeyError", name);
            Err(PyError::PyRaise)
        },
    }
}

/// `py_pwd_getpwuid` (`modpwd.zig`): look up by uid (cast to
/// `c_uint` — wrapping, see the module docs); a missing uid
/// raises `KeyError(arg)`.
fn pwd_getpwuid_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let uid = obj_get_int(ctx, ctx.types, args[0]).map_err(|e| ctx.record_typed(e))?;
    let rows = read_etc_file("/etc/passwd");
    let needle = (uid as u32).to_string();
    match find_row(&rows, 2, &needle) {
        Some(row) => Ok(pwd_attrtuple(ctx, row)),
        None => {
            ctx.record_raise_value("KeyError", args[0]);
            Err(PyError::PyRaise)
        },
    }
}

/// `build_pwd_module` (`modpwd.zig` table): 3 entries.
pub fn build_pwd_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let (module, dict) = new_d4_module(ctx, "pwd", module_type);
    let getpwnam = mk_fun(ctx, fun_builtin_var, "getpwnam", 1, 1, pwd_getpwnam_builtin, None);
    let getpwuid = mk_fun(ctx, fun_builtin_var, "getpwuid", 1, 1, pwd_getpwuid_builtin, None);
    store(ctx, dict, "getpwnam", getpwnam);
    store(ctx, dict, "getpwuid", getpwuid);
    fix_dict(ctx, dict);
    Ok(module)
}
