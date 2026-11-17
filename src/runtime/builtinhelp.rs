//! `help()` (vendor `py/builtinhelp.c`).
//!
//! Live under `MICROPY_PY_BUILTINS_HELP` (full config,
//! `mpconfigport.h:175`) with `MICROPY_PY_BUILTINS_HELP_MODULES`
//! (`mpconfigport.h:278`). Registered in the builtins dict by
//! `modbuiltins.zig:1158` (full-only — under nano the name is
//! not exposed); the Rust binding lives in `Vm::build_builtins`
//! with the same sig `(0, 1)`.
//!
//! The builtin is intercepted by `GcRef` identity in
//! `call_function_ctx` (the C15a eval/exec pattern — the
//! `"modules"` path needs the VM's module registries, which a
//! `FunBuiltin` slot cannot see); its `func` is a stub.
//!
//! Three paths (`mp_builtin_help`, `builtinhelp.c:177-188`):
//! - 0 args → `mp_help_default_text` verbatim;
//! - 1 arg that is the string `"modules"` → the module list;
//! - 1 arg otherwise → `mp_help_print_obj`.
//!
//! The `"modules"` check is VALUE-based: the C compares
//! `obj == MP_OBJ_NEW_QSTR(MP_QSTR_modules)`, but a
//! dynamically built `"modules"` (`a + "ules"`, or one
//! returned from a function) still lists the modules on the
//! reference binary (probed 2026-11-18) — so Rust matches
//! any `str` whose bytes are `b"modules"` (a `Qstr` or a
//! heap `Str`; `bytes` do NOT match, probed).
//!
//! Output goes to `MP_PYTHON_PRINTER` (fd 1 under full —
//! `MP_PLAT_PRINT`, the same printer `print` uses), collected
//! into one `String` first (the `obj_print` pattern).
//!
//! Iteration order: the C walks the raw `mp_map_t` table
//! (hash order for a hash dict, ROM order for a ROM dict).
//! Rust walks `MapData.entries` (insertion order) — the same
//! SET in the same order for ROM locals dicts (built by
//! `from_pairs` in ROM order); for a hash-ordered module
//! globals dict the reference prints hash-table order while
//! Rust prints insertion order (same names, different order).

use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_get_type;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::slots::PrintKind;
use crate::objects::strings::mpprint::{mp_print_str, Print, MP_PLAT_PRINT};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::type_error;

/// `mp_help_default_text` (`builtinhelp.c:38-51`), verbatim.
pub const HELP_DEFAULT_TEXT: &str = "Welcome to MicroPython!\n\nFor online docs please visit http://docs.micropython.org/\n\nControl commands:\n  CTRL-A        -- on a blank line, enter raw REPL mode\n  CTRL-B        -- on a blank line, enter normal REPL mode\n  CTRL-C        -- interrupt a running program\n  CTRL-D        -- on a blank line, exit or do a soft reset\n  CTRL-E        -- on a blank line, enter paste mode\n\nFor further help on a specific object, type help(obj)\n";

/// The `help` builtin's `func` — a stub, like `eval`'s (see
/// `builtinevex.rs`): the real work is `call_help_ctx`,
/// intercepted by identity in `call_function_ctx`.
pub(crate) fn help_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `mp_builtin_help` (`builtinhelp.c:177-188`): 0–1 args, no
/// kwargs (`help(1, 2)` → `TypeError("argument num/types
/// mismatch")`, probed); returns `None`.
pub fn call_help_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args > 1 || n_kw != 0 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    let mut out = String::new();
    if n_args == 0 {
        out.push_str(HELP_DEFAULT_TEXT);
    } else if is_modules_arg(ctx, args[0]) {
        print_modules(ctx, state, &mut out);
    } else {
        print_obj(ctx, args[0], &mut out);
    }
    let mut sink = MP_PLAT_PRINT;
    let mut print = Print::new(&mut sink);
    let _ = mp_print_str(&mut print, out.as_bytes());
    Ok(Value::None)
}

/// The `"modules"` arg: any `str` (qstr or heap) whose bytes
/// are exactly `b"modules"`.
pub(crate) fn is_modules_arg(ctx: &CallCtx, arg: Value) -> bool {
    match arg {
        Value::Qstr(id) => ctx.pool.qstr_str(id as u16) == b"modules",
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => s.bytes == b"modules",
            _ => false,
        },
        _ => false,
    }
}

/// `mp_help_print_modules` (`builtinhelp.c:93-133`): the
/// builtin module names, the extensible module names, and the
/// frozen module names, sorted, printed column-first in 4
/// columns of width 18. No "Plus any modules on the
/// filesystem" line (`MICROPY_ENABLE_EXTERNAL_IMPORT=0`).
pub(crate) fn print_modules(ctx: &CallCtx, state: &VmState, out: &mut String) {
    let mut names: Vec<String> = Vec::new();
    for (id, _module) in &state.builtin_modules {
        names.push(name_bytes(ctx, *id));
    }
    for (id, _module) in &state.extensible_modules {
        names.push(name_bytes(ctx, *id));
    }
    for name in state.frozen_modules.names() {
        names.push(name.to_string());
    }
    // `mp_obj_list_sort` (default str ordering — bytewise).
    names.sort();
    const NUM_COLUMNS: usize = 4;
    const COLUMN_WIDTH: usize = 18;
    let len = names.len();
    if len == 0 {
        return;
    }
    let num_rows = len.div_ceil(NUM_COLUMNS);
    for i in 0..num_rows {
        let mut j = i;
        loop {
            let item = &names[j];
            out.push_str(item);
            j += num_rows;
            if j >= len {
                break;
            }
            // `gap = COLUMN_WIDTH - l; while (gap < 1) gap +=
            // COLUMN_WIDTH;` — pad to the next column boundary,
            // at least one space.
            let mut gap = COLUMN_WIDTH as isize - item.len() as isize;
            while gap < 1 {
                gap += COLUMN_WIDTH as isize;
            }
            for _ in 0..gap {
                out.push(' ');
            }
        }
        out.push('\n');
    }
}

/// A module name qstr id as a `String` (statics via the
/// static table, dynamic ids via the pool).
fn name_bytes(ctx: &CallCtx, id: u32) -> String {
    String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned()
}

/// `mp_help_print_obj` (`builtinhelp.c:135-173`): `object
/// {obj PRINT_STR} is of type {type-name}\n`, then one ` {name
/// PRINT_STR} -- {value PRINT_STR}\n` line per entry of the
/// map: a module's globals dict, a type's own locals dict, or
/// any other object's TYPE's locals dict (in map order).
pub(crate) fn print_obj(ctx: &mut CallCtx, obj: Value, out: &mut String) {
    use std::fmt::Write as _;
    let t = obj_get_type(ctx.arena, ctx.types, obj);
    out.push_str("object ");
    print_value(ctx, obj, out);
    let type_name = match t {
        Some(t) => match ctx.arena.get(t) {
            Obj::Type(d) => name_bytes(ctx, d.name),
            _ => String::new(),
        },
        None => String::new(),
    };
    let _ = writeln!(out, " is of type {type_name}");
    // The map to list: a module's globals; a type object's
    // OWN locals dict (`type == &mp_type_type`); otherwise
    // the object's type's locals dict. A type with no locals
    // dict lists nothing.
    let is_type_obj = t == Some(ctx.types.type_);
    let dict: Option<GcRef> = match obj {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Some(m.dict),
            Obj::Type(d) if is_type_obj => Some(d.dict),
            _ => type_dict(ctx, obj),
        },
        _ => type_dict(ctx, obj),
    };
    let Some(dict) = dict else {
        return;
    };
    let entries: Vec<(Value, Value)> = match ctx.arena.get(dict) {
        Obj::Dict(d) => d
            .map
            .entries
            .iter()
            .filter_map(|e| e.key.map(|k| (k, e.value)))
            .collect(),
        _ => return,
    };
    for (name, value) in entries {
        out.push_str("  ");
        print_value(ctx, name, out);
        out.push_str(" -- ");
        print_value(ctx, value, out);
        out.push('\n');
    }
}

/// A value printed with `PRINT_STR` into the help text
/// (`mp_obj_print(obj, PRINT_STR)`).
fn print_value(ctx: &mut CallCtx, value: Value, out: &mut String) {
    obj_print_helper(ctx, ctx.types, value, PrintKind::Str, out);
}

/// An object's type's locals dict (`MP_OBJ_TYPE_GET_SLOT(type,
/// locals_dict)`).
fn type_dict(ctx: &CallCtx, obj: Value) -> Option<GcRef> {
    let t = obj_get_type(ctx.arena, ctx.types, obj)?;
    match ctx.arena.get(t) {
        Obj::Type(d) => Some(d.dict),
        _ => None,
    }
}
