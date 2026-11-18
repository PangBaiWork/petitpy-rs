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
/// dict, then the builtins override dict (`builtins.X = Y` —
/// E2i wires the `VmState.builtins_override_dict` the VM
/// persists, so a store into the builtins module is visible
/// here), then the builtins globals dict, else
/// `NameError("name '%q' isn't defined")`.
pub fn load_global_ctx(ctx: &mut CallCtx, state: &mut VmState, qst: u32) -> Result<Value, Value> {
    use crate::objects::containers::objdict::dict_get_value;
    let key = Value::Qstr(qst);
    if let Ok(v) = dict_get_value(ctx, Value::Heap(state.globals), key) {
        return Ok(v);
    }
    if let Some(od) = state.builtins_override_dict {
        if let Ok(v) = dict_get_value(ctx, od, key) {
            return Ok(v);
        }
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
/// `mp_delete_name` (`runtime.zig:343`): delete from the locals
/// dict. The vendor's `dict_get_helper` (`REMOVE_IF_FOUND`,
/// `objdict.c`) RAISES `KeyError(key)` when the name is missing
/// (the Zig's `mp_delete_name` ignores the return value, but the
/// raise travels on the error channel) — so a double `del x`
/// raises (`del_name.py`'s bare except prints "NameError";
/// `del_global.py` notes the KeyError in its FIXME comment).
pub fn delete_name_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    qst: u32,
) -> Result<(), Value> {
    use crate::objects::containers::objdict::dict_delete;
    use crate::runtime::vm_ops::exc_from_pyerror;
    dict_delete(ctx, Value::Heap(state.locals_dict()), Value::Qstr(qst))
        .map(|_| ())
        .map_err(|e| {
            let sig = ctx.record_typed(e);
            exc_from_pyerror(ctx, state, sig)
        })
}

/// `mp_delete_global` (`runtime.zig:346`): delete from the
/// globals dict; same `KeyError` rule as `delete_name_ctx`.
pub fn delete_global_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    qst: u32,
) -> Result<(), Value> {
    use crate::objects::containers::objdict::dict_delete;
    use crate::runtime::vm_ops::exc_from_pyerror;
    dict_delete(ctx, Value::Heap(state.globals), Value::Qstr(qst))
        .map(|_| ())
        .map_err(|e| {
            let sig = ctx.record_typed(e);
            exc_from_pyerror(ctx, state, sig)
        })
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
        // E2f: C11 iterators and generators carry header `None`
        // (the `type(iter(...))` gap — out of scope here), so the
        // flag check above never fires for them. The reference
        // resolves `__next__` on both (probed:
        // `iter([1]).__next__()` → the first element,
        // `g.__next__()` → the first yield) — the payload IS an
        // iterator, so bind the `next` builtin directly (the
        // Zig's iterator type carries `__next__` in its locals;
        // this is the same binding without the type). This path
        // cannot recurse into `iternext_allow_raise_ctx`: its
        // `load_next_method_ctx` does a direct dict lookup and
        // never calls this function.
        if let Value::Heap(r) = obj {
            if matches!(ctx.arena.get(r), Obj::Iterator(_) | Obj::Generator(_)) {
                return Ok((Some(state.next_builtin), Some(obj)));
            }
        }
    }
    // An instance tries its attribute load first, with the
    // VM's real call hook (E2): the members map, `__dict__`,
    // the class lookup, a `property` fget, a descriptor
    // `__get__`, and the `__getattr__` fallback. The value is
    // already bound when it is a method (`instance_load_attr`
    // returns `bind_pair`), so the second element is `None`
    // (no self to push). `Ok(None)` falls through to the
    // attr-slot dispatch and the type-locals lookup below.
    if let Value::Heap(_) = obj {
        match crate::runtime::vm_seams::try_instance_load_attr(ctx, state, obj, attr) {
            Ok(Some(v)) => return Ok((Some(v), None)),
            Ok(None) => {}
            Err(exc) => return Err(exc),
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
        // D2: module `__getattr__` (PEP 562) — `module_attr`
        // runs with seam None, so a module `__getattr__` fun
        // never fires through it. Run it here, where
        // (ctx, state) are both live for `call_function_ctx`
        // (the C13 gap: "C13 wires the seam when the VM's
        // call machinery lands"). Only for modules, only when
        // the attr slot found nothing, never for `__getattr__`
        // itself (no recursion). A raise from the
        // `__getattr__` fun propagates unchanged (the
        // `os.path` import failure is a ModuleNotFoundError,
        // per the probe).
        let module_dict = if let Value::Heap(r) = obj {
            match ctx.arena.get(r) {
                Obj::Module(m) => Some(m.dict),
                _ => None,
            }
        } else {
            None
        };
        if let Some(dict) = module_dict {
            let getattr_id = static_qstr_id(b"__getattr__").unwrap_or(u32::MAX);
            if attr != getattr_id {
                use crate::objects::containers::objdict::dict_get_value;
                if let Ok(fun) =
                    dict_get_value(ctx, Value::Heap(dict), Value::Qstr(getattr_id))
                {
                    return crate::runtime::vm_call::call_function_ctx(
                        ctx,
                        state,
                        fun,
                        1,
                        0,
                        &[Value::Qstr(attr)],
                    )
                    .map(|v| (Some(v), None));
                }
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
/// `super_attr`, `objtype.c:1349`): build the temporary
/// `super(type, obj)` object and load the method through it —
/// i.e. the SAME `super_attr` path `load_method_maybe_ctx`
/// uses (E2f: the old MRO-dict-only walk below missed the
/// native `__init__` SENTINEL — a native base's `make_new`
/// reached via `SLOT_OFFSET_MAKE_NEW` — and fell through to
/// `object.__init__`, so `super().__init__(iterable)` raised
/// `TypeError("argument num/types mismatch")` while the stored
/// form (`f = super().__init__; f(iterable)`, which goes
/// through `load_method_maybe_ctx`) worked. The Zig has no
/// separate walk: `mp_load_super_method` IS `mp_load_method`
/// on a temporary super object.)
pub fn load_super_method_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    type_: GcRef,
    obj: Value,
    attr: u32,
) -> Result<(Value, Option<Value>), Value> {
    use crate::objects::objtype::seams::with_stub_seams;
    use crate::objects::objtype::typecreate::super_attr_impl;
    match with_stub_seams(|seams| super_attr_impl(ctx, type_, obj, attr, seams)) {
        Ok(Some(method)) => Ok((method, None)),
        Ok(None) => {
            let name = String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
            Err(crate::objects::objtype::objexcept::new_exception_msg(
                ctx,
                state.exc.attribute_error,
                &format!("no attribute '{name}'"),
            ))
        }
        Err(e) => Err(crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e)),
    }
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

/// `mp_store_attr` (`runtime.zig:1255-1275`): the real C14
/// `runtime_method::store_attr` — the attr slot (C12a's), the
/// `Instance` members map, descriptors. `value == None` deletes
/// (the `delattr` path; `del x.attr` compiles to `LOAD_NULL;
/// STORE_ATTR`, so the null slot arrives here as `None`,
/// distinct from storing the `None` object). The old stub
/// always raised `AttributeError` (the C13b ignored test
/// `makenew_class_member_store` waited on this). The MODULE
/// case goes through C14's `module_store_attr` (D2): `sys.path
/// = [...]` stores into the delegation slot (probed on the
/// reference), `del sys.path` / `sys.foo = 42` raise
/// AttributeError, `del mymod.x` deletes from the module dict
/// (missing → KeyError, per C14).
pub fn store_attr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    base: Value,
    attr: u32,
    value: Option<Value>,
) -> Result<(), Value> {
    use crate::runtime::import_search::rt_error_to_value;
    use crate::runtime::runtime::{Builtins, ExcRefs, RtError, RuntimeSeams};
    use crate::runtime::runtime_method::store_attr;
    use crate::runtime::vm_rt_iter::drain_seam_raise;
    let mut seams = RuntimeSeams::stub();
    let mut builtins = Builtins {
        globals: Value::Heap(state.builtins),
        override_dict: state.builtins_override_dict,
    };
    let globals = Value::Heap(state.globals);
    let next_obj = state.next_builtin;
    let exc = ExcRefs::from_exception_types(&state.exc);
    // The VM's real call hook (E2), scoped so it is dropped
    // before the error drain below borrows `state` again.
    let result = {
        let mut hook = crate::runtime::vm_seams::make_hook(state);
        store_attr(
            ctx,
            base,
            attr,
            value,
            &exc,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            Some(&mut hook),
        )
    };
    // A store into the builtins module installs the override
    // dict — persist it on the VM (E2i), or the next global
    // load / import would not see the override.
    state.builtins_override_dict = builtins.override_dict;
    result.map_err(|e| match e {
        RtError::PyRaise => drain_seam_raise(ctx, state),
        other => rt_error_to_value(ctx, state, other),
    })
}

