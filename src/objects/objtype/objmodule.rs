//! Module objects (`objmodule.zig`, 232 lines, 0 test blocks; the
//! Zig port of vendor `py/objmodule.c` bodies).
//!
//! What lives here: the module type (`module_type_data`),
//! `new_module`, `module_generic_attr`, `module_get_builtin`,
//! `module_attr` (load) + `module_store_attr` (store/delete), and
//! `module_print`. The import system itself is C15's
//! (`builtinimport`) — this lane ports the TYPE + constructors +
//! attribute access, not importing.
//!
//! Zig-isms ported exactly:
//! - `mp_obj_new_module` registers in the global loaded-modules
//!   table and returns the EXISTING module on a second call.
//!   Registration/dedup is VM state (C15) — `new_module` here is
//!   the allocator (fresh module + empty dict + `__name__`); the
//!   caller registers it.
//! - `module_attr` load order: dict hit → `__dict__` (CPython
//!   compat = 1) → `__getattr__` (MODULE_GETATTR = 1, any attr
//!   except `__getattr__` itself) → module delegation (the C
//!   `npy_mod_attr_try_delegation` table — no delegation table
//!   exists in Rust yet, so this falls through to "not found").
//! - `module_attr` store order: dict not fixed → store/delete in
//!   the dict; dict fixed AND full AND the dict IS the builtins
//!   globals dict → the Zig-side override dict
//!   (`mp_module_builtins_override_dict`, created on first use);
//!   otherwise delegation (→ unhandled here).
//! - `module_print`: `"<module 'name'>"` or `"<module 'name'
//!   from 'file'>"` when `__file__` is present
//!   (`MICROPY_MODULE___FILE__` is on — ROM level at least
//!   core features); a non-str `__name__`/`__file__` falls back
//!   to empty (the Zig's polled-sentinel `mp_obj_str_get_str`
//!   rule).
//! - `mp_module_get_builtin`: the non-extensible map first, then
//!   the `usys` → sys special case, then (extensible builds) the
//!   `u`-prefix strip (`import ufoo` → extensible `foo`); with
//!   `extensible == true` a miss returns NULL immediately.
//!   The `__init__` call on lookup goes through the `init`
//!   seam (a VM call — the Zig's `mp_call_function_0`).
//!
//! Seams (C10 pattern — the VM supplies the real call):
//! - `module_attr_load` takes an optional `getattr_call` seam
//!   (`__getattr__(name)`); `None` means no VM call is available
//!   and the `__getattr__` path is skipped (the `AttrFn` adapter
//!   `module_attr` passes `None` — C13 wires the seam).
//! - `module_get_builtin` takes an optional `init` seam
//!   (`__init__()`); `None` skips the call.
//! - The builtins-override dict is caller-owned state: the VM
//!   holds an `Option<GcRef>` and passes `&mut` to
//!   `module_store_attr` (no globals in Rust).
//!
//! Divergences (documented, not silent):
//! - Module delegation (`npy_mod_attr_try_delegation`, the
//!   `MICROPY_MODULE_DELEGATIONS` table) has no Rust counterpart
//!   yet — load/store report "not found" / "unhandled" where the
//!   Zig would consult the table. No delegation table exists in
//!   the tree today (C15/D own extension modules).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_delete, dict_get_value, dict_store, DictError};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::value::{DictData, GcRef, MapData, ModuleData, Obj, TypeData, Value};
use crate::pyerr::PyResult;

/// `mp_type_module` ROM data. `base` is the `object` type
/// (`type(sys).__mro__ == (module, object)`,
/// `type(sys).__bases__ == (object,)`, probed 2026-11-17);
/// `mro` is left empty — the bootstrap fills `[self, object]`
/// after allocation (the `dict_type_data` precedent). Slots: attr
/// (`module_attr`) + print (`module_print`); the `attr` slot
/// itself is C12a's field — the standalone `module_attr` fn is
/// wired then.
pub fn module_type_data(dict: GcRef, object: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"module").expect("MP_QSTR_module is in the static table"),
        flags: 0,
        base: Some(object),
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(module_print),
    }
}

/// `mp_obj_new_module` (`objmodule.zig:81`), allocation half:
/// a fresh module named `name` with an empty globals dict
/// holding `__name__`. The header types are the real ones (the
/// Zig's `npy_mod_alloc_module_context` tags the module with
/// `&mp_type_module`; the globals dict is a plain dict).
/// (Registration in the loaded-modules table is C15's — see the
/// module docs.)
pub fn new_module(ctx: &mut CallCtx, name: u32, module_type: GcRef) -> Value {
    let dict = ctx.alloc(
        Some(ctx.types.dict),
        Obj::Dict(DictData { map: MapData::new() }),
    );
    ctx.roots.push(Value::Heap(dict));
    let module = ctx.alloc(
        Some(module_type),
        Obj::Module(ModuleData { name, dict }),
    );
    ctx.roots.push(Value::Heap(module));
    let name_id = static_qstr_id(b"__name__").expect("__name__ is a static qstr");
    let _ = dict_store(
        ctx,
        Value::Heap(dict),
        Value::Qstr(name_id),
        Value::Qstr(name),
    );
    Value::Heap(module)
}

/// The module's globals dict as a `Value`.
fn module_dict(ctx: &CallCtx, module: Value) -> Option<Value> {
    match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Some(Value::Heap(m.dict)),
            _ => None,
        },
        _ => None,
    }
}

/// Look a qstr key up in a dict value: `Some(value)` on a hit,
/// `None` on a miss (any error other than missing-key propagates
/// as `PyRaise` — the dict lookup cannot fail otherwise).
fn dict_lookup(ctx: &mut CallCtx, dict: Value, key: u32) -> PyResult<Option<Value>> {
    match dict_get_value(ctx, dict, Value::Qstr(key)) {
        Ok(v) => Ok(Some(v)),
        Err(DictError::KeyError(_)) => Ok(None),
        Err(e) => Err(e.to_pyerror()),
    }
}

/// `mp_module_generic_attr` (`objmodule.zig:64`): linear search
/// over a parallel (keys, values) table. `op` selects the mode:
/// - `Load` → the value, or `None` when the attr is not in the table.
/// - `Store(v)` → stores, returning `Some(Value::None)` when the
///   key was found (handled) or `None` when it was not (unhandled).
/// - `Delete` → deletes, same handled/not-found convention as store.
/// (The Zig signals "handled" by clearing `dest[0]`; the
/// `Some(Value::None)` return is that signal in Rust.)
#[derive(Clone, Debug)]
pub enum GenericAttrOp {
    Load,
    Store(Value),
    Delete,
}

pub fn module_generic_attr(
    attr: u32,
    keys: &[u32],
    values: &mut [Value],
    op: GenericAttrOp,
) -> Option<Value> {
    debug_assert_eq!(keys.len(), values.len(), "parallel (keys, values) table");
    for (i, k) in keys.iter().enumerate() {
        if *k == attr {
            return match op {
                GenericAttrOp::Load => Some(values[i]),
                GenericAttrOp::Store(v) => {
                    values[i] = v;
                    Some(Value::None)
                }
                GenericAttrOp::Delete => {
                    values[i] = Value::None;
                    Some(Value::None)
                }
            };
        }
    }
    None
}

/// `mp_module_get_builtin` (`objmodule.zig:102`): look a built-in
/// module up by name. `modules` / `extensible_modules` are the two
/// ROM maps as (name-qstr, module) pairs (C15 owns the real maps);
/// `sys_module` is the `mp_module_sys` ROM module for the `usys`
/// special case (`None` when the sys module is not registered).
/// `init` is the `__init__()` call seam (`None` skips the call);
/// `intern` is the dynamic-qstr interning seam (`None` skips the
/// `u`-prefix strip — interning needs `&mut RuntimeQstr`, which
/// `CallCtx` does not carry; the VM supplies it, C13/C15).
/// Returns `None` for "not found" (the Zig `MP_OBJ_NULL`).
#[allow(clippy::too_many_arguments)]
pub fn module_get_builtin(
    ctx: &mut CallCtx,
    module_name: u32,
    extensible: bool,
    modules: &[(u32, Value)],
    extensible_modules: &[(u32, Value)],
    sys_module: Option<Value>,
    init: Option<&mut dyn FnMut(&mut CallCtx, Value) -> PyResult<()>>,
    intern: Option<&mut dyn FnMut(&[u8]) -> PyResult<u32>>,
) -> PyResult<Option<Value>> {
    let map: &[(u32, Value)] = if extensible { extensible_modules } else { modules };
    let mut found = map.iter().find(|(n, _)| *n == module_name).map(|(_, v)| *v);
    if found.is_none() {
        let usys = static_qstr_id(b"usys").expect("usys is a static qstr");
        if module_name == usys {
            found = sys_module;
        }
        if found.is_none() && extensible {
            return Ok(None);
        }
        if found.is_none() {
            // u-prefix strip: `import ufoo` maps to extensible `foo`.
            let bytes = ctx.pool.qstr_str(module_name as u16);
            if bytes.first() == Some(&b'u') && bytes.len() > 1 {
                if let Some(intern) = intern {
                    let stripped = intern(&bytes[1..])?;
                    found = extensible_modules
                        .iter()
                        .find(|(n, _)| *n == stripped)
                        .map(|(_, v)| *v);
                }
            }
        }
        if found.is_none() {
            return Ok(None);
        }
    }
    let module = found.expect("found is Some here");
    // Full config only: run `__init__` if the module object is
    // fully formed (base.type non-null — the Zig skips stub modules
    // whose base type is NULL) and the name is in its globals.
    // In Rust "fully formed" is a non-None header type.
    if let Some(init) = init {
        let formed = matches!(module, Value::Heap(r) if ctx.arena.header(r).type_.is_some());
        if formed {
            if let Some(dict) = module_dict(ctx, module) {
                let init_id = static_qstr_id(b"__init__").expect("__init__ is a static qstr");
                if let Some(fun) = dict_lookup(ctx, dict, init_id)? {
                    init(ctx, fun)?;
                }
            }
        }
    }
    Ok(Some(module))
}

/// `module_attr` (`objmodule.zig:148`), LOAD side. Order: dict
/// hit → `__dict__` → `__getattr__` (via the seam) →
/// delegation (not found here). The `AttrFn` adapter below
/// passes `getattr_call = None`.
pub fn module_attr_load(
    ctx: &mut CallCtx,
    module: Value,
    attr: u32,
    getattr_call: Option<&mut dyn FnMut(&mut CallCtx, Value, Value) -> PyResult<Value>>,
) -> PyResult<Option<Value>> {
    let Some(dict) = module_dict(ctx, module) else {
        return Ok(None);
    };
    if let Some(v) = dict_lookup(ctx, dict, attr)? {
        return Ok(Some(v));
    }
    if attr == static_qstr_id(b"__dict__").expect("__dict__ is a static qstr") {
        return Ok(Some(dict));
    }
    let getattr_id = static_qstr_id(b"__getattr__").expect("__getattr__ is a static qstr");
    if attr != getattr_id {
        if let Some(fun) = dict_lookup(ctx, dict, getattr_id)? {
            if let Some(call) = getattr_call {
                return Ok(Some(call(ctx, fun, Value::Qstr(attr))?));
            }
        }
    }
    // Module delegation (npy_mod_attr_try_delegation) — no table
    // in Rust yet (see module docs).
    Ok(None)
}

/// The module type's attr slot (load-only `AttrFn`, C10a):
/// `module_attr_load` with no `__getattr__` seam — C13 wires
/// the seam when the VM's call machinery lands.
pub fn module_attr(ctx: &mut CallCtx, module: Value, attr: u32) -> PyResult<Option<Value>> {
    module_attr_load(ctx, module, attr, None)
}

/// `module_attr`, STORE/DELETE side (`objmodule.zig:186`).
/// `value: Some(v)` stores; `None` deletes. `builtins_globals`
/// is the builtins module's globals dict (for the fixed-dict
/// override path); `override_dict` is the VM-owned
/// `mp_module_builtins_override_dict` slot (created on first
/// use). Returns `Ok(true)` when handled, `Ok(false)` when
/// delegation would be tried (unhandled here — the caller raises
/// AttributeError, as the reference binary does for fixed-dict
/// modules: `del sys.nonexistent` / `sys.foo = 42`). A
/// delete-missing on a NON-fixed dict raises `KeyError(key)`
/// (probed: `del mymod.nonexistent` → `KeyError 'nonexistent'`,
/// the pending error the Zig's `_ = mp_obj_dict_delete(...)`
/// leaves set) — hence the `DictError` return, which carries the
/// key for the VM (the C6 convention: the slot boundary would
/// drop it).
pub fn module_store_attr(
    ctx: &mut CallCtx,
    module: Value,
    attr: u32,
    value: Option<Value>,
    builtins_globals: Option<GcRef>,
    override_dict: &mut Option<GcRef>,
) -> Result<bool, DictError> {
    let Some(dict_v) = module_dict(ctx, module) else {
        return Ok(false);
    };
    let dict_r = match dict_v {
        Value::Heap(r) => r,
        _ => return Ok(false),
    };
    let is_fixed = match ctx.arena.get(dict_r) {
        Obj::Dict(d) => d.map.is_fixed,
        _ => return Ok(false),
    };
    let mut target = dict_v;
    if is_fixed {
        let is_builtins = builtins_globals == Some(dict_r);
        if is_builtins {
            if override_dict.is_none() {
                let fresh = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
                ctx.roots.push(Value::Heap(fresh));
                *override_dict = Some(fresh);
            }
            target = Value::Heap(override_dict.expect("override dict just created"));
        } else {
            // Delegation (npy_mod_attr_try_delegation) — unhandled.
            return Ok(false);
        }
    }
    let key = Value::Qstr(attr);
    match value {
        Some(v) => dict_store(ctx, target, key, v).map(|_| true),
        None => dict_delete(ctx, target, key).map(|_| true),
    }
}

/// `module_print` (`objmodule.zig:206`): `"<module 'name'>"` or
/// `"<module 'name' from 'file'>"`. Non-str `__name__` /
/// `__file__` fall back to empty (the Zig's polled-sentinel rule).
/// `__file__` is NOT a static qstr — it is config-gated out of
/// the static table and interned dynamically at runtime (the
/// Zig's `main.zig:148` comment; `npy_mod_QSTR_FILE` is
/// `MP_QSTR___file__` only when `MICROPY_MODULE___FILE__=1`) —
/// so the file entry is found by scanning the dict for a key
/// whose string is `"__file__"` (see `dict_lookup_by_str`),
/// not by id lookup.
pub fn module_print(ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let Some(dict) = module_dict(ctx, value) else {
        return;
    };
    let name_id = static_qstr_id(b"__name__").expect("__name__ is a static qstr");
    let name = dict_lookup(ctx, dict, name_id)
        .ok()
        .flatten()
        .and_then(|v| match v {
            Value::Qstr(id) => Some(String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned()),
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Str(s) => Some(String::from_utf8_lossy(&s.bytes).into_owned()),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_default();
    let file = dict_lookup_by_str(ctx, dict, b"__file__")
        .ok()
        .flatten()
        .and_then(|v| match v {
            Value::Qstr(id) => Some(String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned()),
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Str(s) => Some(String::from_utf8_lossy(&s.bytes).into_owned()),
                _ => None,
            },
            _ => None,
        });
    match file {
        Some(f) => {
            let _ = write!(sink, "<module '{}' from '{}'>", name, f);
        }
        None => {
            let _ = write!(sink, "<module '{}'>", name);
        }
    }
}

/// Look a key up in a dict by its STRING rather than its qstr id.
/// Only for keys that are not static qstrs (`__file__` — see
/// `module_print`); a linear scan over the dict's entries,
/// tombstones skipped. Returns `None` on a miss.
fn dict_lookup_by_str(ctx: &mut CallCtx, dict: Value, name: &[u8]) -> PyResult<Option<Value>> {
    let Value::Heap(r) = dict else {
        return Ok(None);
    };
    let Obj::Dict(d) = ctx.arena.get(r) else {
        return Ok(None);
    };
    for entry in &d.map.entries {
        if let Some(Value::Qstr(id)) = entry.key {
            if ctx.pool.qstr_str(id as u16) == name {
                return Ok(Some(entry.value));
            }
        }
    }
    Ok(None)
}
