//! Load/store name/global/attr — the `runtime.zig:288-346` and
//! `:1126-1275` ports: `mp_load_name` / `mp_load_global` /
//! `mp_store_name` / `mp_store_global` / `mp_delete_name` /
//! `mp_delete_global` / `mp_load_build_class`, and the interim
//! `mp_load_method` / `mp_load_attr` / `mp_store_attr`
//! (C14 subsumes `load_method` once C12a lands the `attr` slot).
//! Split out of `vm_rt.rs` (file-size rule).

use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::lookup::convert_member_lookup;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, exc_from_typed};
use crate::runtime::vm_rt::pending_exc_or_type_error;

// ── load/store name/global ───────────────────────────────────────────

/// `mp_load_name` (`runtime.zig:288-296`): when the locals
/// dict is distinct from the globals dict (`localsNeGlobals`,
/// `mp_mirrors.zig:275-276` — `dict_locals != dict_globals`),
/// look the name up in locals FIRST, then fall through to
/// `mp_load_global` (globals, then builtins). When locals ==
/// globals (the common case — `VmState.locals` is `None`),
/// this IS `mp_load_global`. The distinct-locals case is set
/// only by `eval`/`exec` (`codeExecute` — C15a wires it) and
/// by a class body (C12a's namespace — the compiler emits
/// `LOAD_NAME` there too).
pub fn load_name_ctx(ctx: &mut CallCtx, state: &mut VmState, qst: u32) -> Result<Value, Value> {
    // `mp_load_name` (`runtime.zig:288-301`): the locals first
    // (`state.locals`, set by `eval`/`exec` or by a class body),
    // then the module globals / builtins (`mp_load_global`).
    // When locals == globals the dict is the same, so the guard
    // only skips a redundant lookup.
    use crate::objects::containers::objdict::dict_get_value;
    if let Some(l) = state.locals {
        if l != state.globals {
            let key = Value::Qstr(qst);
            if let Ok(v) = dict_get_value(ctx, Value::Heap(l), key) {
                return Ok(v);
            }
        }
    }
    load_global_ctx(ctx, state, qst)
}

/// `mp_load_global` (`runtime.zig:301-324`): the module globals
/// dict, then the builtins globals dict (the `override_dict`
/// step is D1's `builtins.X = Y` support — the builtins dict
/// IS `state.builtins` here), else `NameError("name '%q'
/// isn't defined")`.
pub fn load_global_ctx(ctx: &mut CallCtx, state: &mut VmState, qst: u32) -> Result<Value, Value> {
    use crate::objects::containers::objdict::dict_get_value;
    let key = Value::Qstr(qst);
    if let Ok(v) = dict_get_value(ctx, Value::Heap(state.globals), key) {
        return Ok(v);
    }
    if let Ok(v) = dict_get_value(ctx, Value::Heap(state.builtins), key) {
        return Ok(v);
    }
    let name = String::from_utf8_lossy(ctx.pool.qstr_str(qst as u16)).into_owned();
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.name_error,
        &format!("name '{name}' isn't defined"),
    ))
}

/// `mp_store_name` / `mp_store_global` (`runtime.zig:337-342`):
/// `STORE_NAME` stores into the locals dict unconditionally
/// (`dict_locals`, which IS the globals dict when
/// `VmState.locals` is `None`); `STORE_GLOBAL` stores into the
/// globals dict. A store into a fixed dict raises (the globals
/// dict is never fixed).
pub fn store_name_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    qst: u32,
    value: Value,
) -> Result<(), Value> {
    // `mp_store_name` (`runtime.zig:336-341`): the locals
    // (`state.locals`, set by `eval`/`exec` or by a class body)
    // when set, else the module globals — `locals_dict()` is
    // exactly `locals.unwrap_or(globals)`.
    use crate::objects::containers::objdict::dict_store;
    dict_store(
        ctx,
        Value::Heap(state.locals_dict()),
        Value::Qstr(qst),
        value,
    )
    .map(|_| ())
    .map_err(|e| exc_from_typed(ctx, state, e))
}

pub fn store_global_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    qst: u32,
    value: Value,
) -> Result<(), Value> {
    use crate::objects::containers::objdict::dict_store;
    dict_store(ctx, Value::Heap(state.globals), Value::Qstr(qst), value)
        .map(|_| ())
        .map_err(|e| exc_from_typed(ctx, state, e))
}

/// `mp_delete_name` / `mp_delete_global` (`runtime.zig:343-346`):
/// delete from the locals dict / the globals dict; a missing
/// key is silent (the Zig ignores the delete result).
pub fn delete_name_ctx(ctx: &mut CallCtx, state: &mut VmState, qst: u32) {
    use crate::objects::containers::objdict::dict_delete;
    let _ = dict_delete(ctx, Value::Heap(state.locals_dict()), Value::Qstr(qst));
}

pub fn delete_global_ctx(ctx: &mut CallCtx, state: &mut VmState, qst: u32) {
    use crate::objects::containers::objdict::dict_delete;
    let _ = dict_delete(ctx, Value::Heap(state.globals), Value::Qstr(qst));
}

/// `mp_load_build_class` (`runtime.zig:328`): the override dict's
/// `__build_class__` (D1) else the VM's `__build_class__`
/// builtin (a stub until C12a's `type_call`).
pub fn load_build_class_ctx(ctx: &mut CallCtx, state: &mut VmState) -> Result<Value, Value> {
    let _ = ctx;
    Ok(state.build_class_builtin)
}

// ── load/store attr + `load_method` (interim) ────────────────────────

/// `mp_load_method_maybe` (`runtime.zig:1195-1233`): the
/// (method, self-or-None) pair, or (None, None) when the
/// attribute does not exist. Order: `__class__` (full only) →
/// `__next__` + hasIterNext → the type's attr slot (delegated
/// to the C10a direct fns — `fun_bc_attr` / `bound_meth_attr`
/// / `closure_attr` — plus C12b's `module_attr` /
/// `exception_attr`, since `TypeData` has no `attr` field yet)
/// → the type locals-dict lookup + `convert_member_lookup`.
/// A raise from an attr fn propagates as `Err`.
pub fn load_method_maybe_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    obj: Value,
    attr: u32,
) -> Result<(Option<Value>, Option<Value>), Value> {
    use crate::objects::callables::objboundmeth::bound_meth_attr;
    use crate::objects::callables::objclosure::closure_attr;
    use crate::objects::callables::objfun::fun_bc_attr;
    use crate::objects::objtype::objexcept::exception_attr;
    use crate::objects::objtype::objmodule::module_attr;
    // `__class__` (full only — MICROPY_CPYTHON_COMPAT=1,
    // `runtime.zig:1205`): the type object itself.
    let class_q = static_qstr_id(b"__class__").unwrap_or(u32::MAX);
    if attr == class_q {
        if let Some(t) = obj_get_type(ctx.arena, ctx.types, obj) {
            return Ok((Some(Value::Heap(t)), None));
        }
    }
    let type_ = obj_get_type(ctx.arena, ctx.types, obj);
    // `__next__` + hasIterNext → the `next` builtin bound to obj.
    let next_q = static_qstr_id(b"__next__").unwrap_or(u32::MAX);
    if attr == next_q {
        if let Some(t) = type_ {
            if let Obj::Type(d) = ctx.arena.get(t) {
                if d.flags & crate::objects::iters::iter::FLAG_ITER_IS_ITERNEXT != 0 {
                    return Ok((Some(state.next_builtin), Some(obj)));
                }
            }
        }
    }
    // The type's attr slot — via the direct fns for now
    // (C12a added the `attr` field to `TypeData`, but the
    // `gen_wrap` literal still carries `attr: None` and the
    // module/exception dispatch below is payload-based;
    // C14 subsumes this whole function). C18a adds the generic
    // fallback to the type's own `attr` slot below — the Zig's
    // `mp_load_method_maybe` consults it for every type; complex
    // `.real` / `.imag` rides it.
    if let Some(t) = type_ {
        let mut attr_fn = if t == state.vm_types.fun_bc || t == state.vm_types.gen_wrap {
            Some(fun_bc_attr as crate::objects::slots::AttrFn)
        } else if t == state.vm_types.bound_meth {
            Some(bound_meth_attr as crate::objects::slots::AttrFn)
        } else if t == state.vm_types.closure {
            Some(closure_attr as crate::objects::slots::AttrFn)
        } else if let Value::Heap(r) = obj {
            match ctx.arena.get(r) {
                Obj::Module(_) => Some(module_attr as crate::objects::slots::AttrFn),
                Obj::Exception(_) => Some(exception_attr as crate::objects::slots::AttrFn),
                _ => None,
            }
        } else {
            None
        };
        // C18a: fall back to the type's own `attr` slot — the
        // Zig's `mp_load_method_maybe` consults it for every
        // type (`runtime.zig:1195-1233`). Only consulted when
        // the hardcoded dispatch found nothing, so no attr fn
        // runs twice. Complex `.real` / `.imag` rides this slot
        // (its `TypeData.attr` is `complex_attr`); any future
        // type with an `attr` slot gets the same path.
        if attr_fn.is_none() {
            if let Obj::Type(d) = ctx.arena.get(t) {
                attr_fn = d.attr;
            }
        }
        if let Some(f) = attr_fn {
            match f(ctx, obj, attr) {
                Ok(Some(v)) => return Ok((Some(v), None)),
                Ok(None) => {}
                Err(e) => return Err(exc_from_pyerror(ctx, state, e)),
            }
        }
    }
    // The type locals-dict lookup + `convert_member_lookup`.
    if let Some(t) = type_ {
        if let Obj::Type(d) = ctx.arena.get(t) {
            let dict = d.dict;
            let key = Value::Qstr(attr);
            use crate::objects::containers::objdict::dict_get_value;
            if let Ok(member) = dict_get_value(ctx, Value::Heap(dict), key) {
                let (method, bound) = convert_member_lookup(ctx, Some(obj), t, member);
                return Ok((Some(method), bound));
            }
        }
    }
    Ok((None, None))
}

/// `mp_load_method` (`runtime.zig:1172-1188`): `load_method_maybe`
/// plus the `AttributeError("no attribute '%q'")` raise. A
/// recorded raise from the maybe-step propagates unchanged
/// (the B2 comment: don't overwrite it with the generic
/// message).
pub fn load_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    obj: Value,
    attr: u32,
) -> Result<(Value, Option<Value>), Value> {
    match load_method_maybe_ctx(ctx, state, obj, attr)? {
        (Some(method), self_) => Ok((method, self_)),
        (None, _) => {
            if ctx.has_pending_raise() {
                return Err(pending_exc_or_type_error(ctx, state));
            }
            let name = String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
            Err(crate::objects::objtype::objexcept::new_exception_msg(
                ctx,
                state.exc.attribute_error,
                &format!("no attribute '{name}'"),
            ))
        }
    }
}

/// `mp_load_super_method` (`objtype.zig:463` + vendor
/// `super_attr`, `objtype.c:1349`): build the `super(type,
/// obj)` lookup — walk the parent chain after `type` in
/// `type`'s own MRO (single inheritance, so the parent chain
/// IS `mro[1..]`), first dict hit wins, bound via
/// `convert_member_lookup` against `obj`. A miss raises
/// `AttributeError("no attribute '%q'")` (the `mp_load_method`
/// fallback). The `__init__`-native-`make_new` SENTINEL case
/// (`super_attr`'s `native_base_init_wrapper`) needs C12a's
/// `make_new` slot — until then a native base's `__init__`
/// is found only if it sits in the parent's dict.
pub fn load_super_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    type_: GcRef,
    obj: Value,
    attr: u32,
) -> Result<(Value, Option<Value>), Value> {
    use crate::objects::containers::objdict::dict_get_value;
    let mro: Vec<GcRef> = match ctx.arena.get(type_) {
        Obj::Type(d) => d.mro.clone(),
        _ => Vec::new(),
    };
    for class in mro.iter().skip(1) {
        let dict = match ctx.arena.get(*class) {
            Obj::Type(d) => d.dict,
            _ => continue,
        };
        if let Ok(member) = dict_get_value(ctx, Value::Heap(dict), Value::Qstr(attr)) {
            let (method, self_) = convert_member_lookup(ctx, Some(obj), *class, member);
            return Ok((method, self_));
        }
    }
    let name = String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.attribute_error,
        &format!("no attribute '{name}'"),
    ))
}

/// `mp_load_attr` (`runtime.zig:1126-1135`): `load_method` then
/// wrap a (method, self) pair in a bound method.
pub fn load_attr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    base: Value,
    attr: u32,
) -> Result<Value, Value> {
    use crate::objects::callables::objboundmeth::new_bound_meth;
    let (method, self_) = load_method_ctx(ctx, state, base, attr)?;
    match self_ {
        Some(s) => {
            let bound = new_bound_meth(ctx, state.vm_types.bound_meth, method, s);
            Ok(Value::Heap(bound))
        }
        None => Ok(method),
    }
}

/// `mp_store_attr` (`runtime.zig:1255-1275`): the attr slot
/// (C12a's — no `attr` field yet, so every store falls through
/// to the `AttributeError("no attribute '%q'")` raise).
/// Documented C14 gap: instance attribute stores need C12a's
/// `Instance` members map + the attr slot.
pub fn store_attr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    base: Value,
    attr: u32,
    _value: Value,
) -> Result<(), Value> {
    let _ = base;
    let name = String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
    Err(crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.attribute_error,
        &format!("no attribute '{name}'"),
    ))
}

