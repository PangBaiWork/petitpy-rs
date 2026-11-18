//! Method and attribute loading (`runtime.zig:1126-1276`):
//! `mp_load_method_maybe` / `mp_load_method` /
//! `mp_load_method_protected` / `mp_load_attr` / `mp_store_attr`,
//! plus the `next` builtin (`modbuiltins.zig:297-324`).
//!
//! `mp_load_method_maybe`'s structure, exactly as in the Zig:
//! zero-init stub type → `None`; full-config `__class__` → the
//! type object itself; `__next__` + `hasIterNext` → the `next`
//! builtin bound to the object; the type's `attr` slot → a found
//! value ends the search, `Ok(None)` is the SENTINEL
//! fall-through to the locals-dict lookup, a raise propagates;
//! then the type's OWN locals dict (`TypeData.dict` + the
//! general `dict_lookup_qstr` + C12a's `convert_member_lookup`).
//! Inherited methods reach this function through the instance
//! `attr` slot (C12a's `instance_load_attr` → `class_lookup`
//! MRO walk) — this function never walks the MRO itself.
//!
//! `mp_store_attr` in the Zig calls the type's `attr` slot
//! directly with `dest = [SENTINEL, value]`; the Rust `AttrFn`
//! is load-only, so the store side is spelled as the four free
//! functions that ARE the per-type attr-slot store
//! implementations (`instance_store_attr`, `type_store_attr`,
//! `exception_store_attr`, `module_store_attr`). A store through
//! an unknown attr slot cannot be expressed — no such type
//! exists in the tree today.

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::obj::{obj_get_type, type_data, ObjError, FLAG_ITER_IS_STREAM};
use crate::objects::objtype::instance::instance_store_attr;
use crate::objects::objtype::lookup::convert_member_lookup;
use crate::objects::objtype::objexcept::exception_store_attr;
use crate::objects::objtype::objmodule::module_store_attr;
use crate::objects::objtype::seams::{SubscrError, VmSeams};
use crate::objects::objtype::typecreate::type_store_attr;
use crate::objects::obj_ops::obj_subscr;
use crate::objects::slots::{BuiltinFn, SubscrArg};
use crate::objects::value::{FunBuiltinData, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::runtime::{dunder, Builtins, ExcRefs, RtError, RuntimeSeams};
use crate::runtime::runtime_call::call_function_n_kw;
use crate::runtime::runtime_iter::{iternext, iternext_allow_raise, IterNext};
use crate::runtime::runtime_names::dict_lookup_qstr;
use crate::runtime::runtime_ops::{binary_op, unary_op};

/// The Zig `dest[2]` array: the method function plus its bound
/// self (`None` = unbound — the Zig's `dest[1] == MP_OBJ_NULL`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MethodDest {
    pub func: Value,
    pub self_: Option<Value>,
}

/// `mp_load_method_maybe` (`runtime.zig:1195-1231`). Returns
/// `Result<Option<MethodDest>, RtError>` — NOT the brief sketch's
/// bare `Option`: the Zig's B2 comment in `mp_load_method`
/// requires the pending-exception propagate, and an attr-slot
/// raise must not be overwritten by "no attribute".
pub fn load_method_maybe(
    ctx: &mut CallCtx,
    obj: Value,
    attr: u32,
    next_obj: Value,
) -> Result<Option<MethodDest>, RtError> {
    let Some(t) = obj_get_type(ctx.arena, &ctx.types, obj) else {
        // Zero-init stub type (full config) → "no attribute".
        return Ok(None);
    };
    // Full-config `__class__` branch (MICROPY_CPYTHON_COMPAT=1).
    if attr == dunder(b"__class__").expect("__class__ is a static qstr") {
        return Ok(Some(MethodDest {
            func: Value::Heap(t),
            self_: None,
        }));
    }
    let (attr_fn, dict_ref, flags) = match type_data(ctx.arena, t) {
        Some(d) => (d.attr, d.dict, d.flags),
        None => return Ok(None),
    };
    // `__next__` builtin fast path.
    if attr == dunder(b"__next__").expect("__next__ is a static qstr")
        && flags & FLAG_ITER_IS_STREAM != 0
    {
        return Ok(Some(MethodDest {
            func: next_obj,
            self_: Some(obj),
        }));
    }
    // The type's `attr` slot.
    if let Some(f) = attr_fn {
        match f(ctx, obj, attr) {
            Ok(Some(v)) => {
                let (func, self_) = split_bound_meth(ctx, v);
                return Ok(Some(MethodDest { func, self_ }));
            }
            // SENTINEL: keep looking in the locals dict.
            Ok(None) => {}
            Err(e) => return Err(RtError::from(e)),
        }
    }
    // The type's OWN locals dict. Use the general lookup, not
    // `MapData::find_qstr`: ROM dicts are FIXED ordered maps
    // (`new_fixed`, empty index) and `find_qstr` is the compact
    // path — it indexes `index[pos]` and panics on them.
    if let Some(member) = dict_lookup_qstr(ctx, Value::Heap(dict_ref), attr)? {
        let (func, self_) = convert_member_lookup(ctx, Some(obj), t, member);
        return Ok(Some(MethodDest { func, self_ }));
    }
    Ok(None)
}

/// Split an attr-slot value: a `BoundMeth` payload splits into
/// `(func, Some(self))`; anything else is `(value, None)`.
fn split_bound_meth(ctx: &CallCtx, v: Value) -> (Value, Option<Value>) {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::BoundMeth(b) => (b.func, Some(b.self_)),
            _ => (v, None),
        },
        _ => (v, None),
    }
}

/// `mp_load_method` (`runtime.zig:1172-1191`): maybe, plus the
/// B2 pending-exception propagate and the `AttributeError("no
/// attribute '%q'")` when nothing was found.
pub fn load_method(
    ctx: &mut CallCtx,
    obj: Value,
    attr: u32,
    next_obj: Value,
) -> Result<MethodDest, RtError> {
    match load_method_maybe(ctx, obj, attr, next_obj)? {
        Some(dest) => Ok(dest),
        None => Err(RtError::AttributeError(format!(
            "no attribute '{}'",
            String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16))
        ))),
    }
}

/// `mp_load_method_protected` (`runtime.zig:1235-1254`): maybe,
/// swallowing `AttributeError` (and bare `PyRaise` from the attr
/// call — the slot boundary erased the kind; corpus
/// `builtin_getattr.py` has a class B whose `__getattr__`
/// raises `AttributeError`, and `getattr(b, "b", "default")`
/// must return `"default"`). Swallowing a non-AttributeError
/// raise arriving as bare `PyRaise` is the documented known
/// gap (closable when the slot boundary carries the kind /
// C13's `pending_exc`). `catch_all_exc=true` swallows
/// everything; `OutOfMemory` is re-raised.
pub fn load_method_protected(
    ctx: &mut CallCtx,
    obj: Value,
    attr: u32,
    catch_all_exc: bool,
    next_obj: Value,
) -> Result<Option<MethodDest>, RtError> {
    match load_method_maybe(ctx, obj, attr, next_obj) {
        Ok(dest) => Ok(dest),
        Err(RtError::AttributeError(_)) => Ok(None),
        Err(RtError::PyRaise) => Ok(None),
        Err(e) => {
            if catch_all_exc {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// `mp_load_attr` (`runtime.zig:1126-1134`): `mp_load_method`
/// plus the bound-method wrap (`dest[1] == NULL` → the value
/// itself, else `new_bound_meth`).
pub fn load_attr(
    ctx: &mut CallCtx,
    obj: Value,
    attr: u32,
    next_obj: Value,
) -> Result<Value, RtError> {
    let dest = load_method(ctx, obj, attr, next_obj)?;
    match dest.self_ {
        Some(self_obj) => Ok(Value::Heap(new_bound_meth(
            ctx,
            ctx.types.boundmeth,
            dest.func,
            self_obj,
        ))),
        None => Ok(dest.func),
    }
}

/// `mp_store_attr` (`runtime.zig:1255-1276`): the Zig calls the
/// type's `attr` slot with `dest = [SENTINEL, value]`; the Rust
/// `AttrFn` is load-only, so this dispatches on the payload to
/// the four store implementations. `value == None` deletes (the
/// `Option<Value>` convention of the store fns). Anything else
/// → `AttributeError("no attribute '%q'")`.
#[allow(clippy::too_many_arguments)]
pub fn store_attr(
    ctx: &mut CallCtx,
    obj: Value,
    attr: u32,
    value: Option<Value>,
    exc: &ExcRefs,
    next_obj: Value,
    builtins: &mut Builtins,
    seams: &mut RuntimeSeams,
    globals: Value,
    mut call: Option<crate::objects::slots::CallHook<'_, '_>>,
) -> Result<(), RtError> {
    // Payload tag first: the `VmSeams` closures below borrow
    // `ctx` mutably, so the arena read must end before them.
    #[derive(PartialEq)]
    enum Kind {
        Instance,
        Type,
        Exception,
        Module,
        Other,
    }
    let kind = match obj {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Instance(_) => Kind::Instance,
            Obj::Type(_) => Kind::Type,
            Obj::Exception(_) => Kind::Exception,
            Obj::Module(_) => Kind::Module,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    };
    let attr_name =
        String::from_utf8_lossy(ctx.pool.qstr_str(attr as u16)).into_owned();
    match kind {
        Kind::Instance => {
            // The `VmSeams` for the instance store path, built
            // from this lane's real dispatch fns — NOT the stub
            // seams' `call_via_type_slot`, which fails for
            // header-`None` `FunBc` setters. The `load_maybe`
            // closure maps an error to `None` (the seam type
            // cannot carry it — the same erasure C12a's seams
            // accept); the `subscr` closure maps the real
            // `obj_subscr` onto `SubscrError` (never `KeyError`
            // — `ObjError` carries no kind, same as C12a's
            // stub). The closures live in this block and the
            // seams are used immediately, so no lifetime escape.
            let mut call_c = |ctx: &mut CallCtx,
                              fun: Value,
                              n_args: usize,
                              n_kw: usize,
                              args: &[Value]|
             -> Result<Value, PyError> {
                // The VM's real call hook (E2) when one is
                // passed, so a `FunBc` `__setattr__` / property
                // setter actually runs; the stub
                // `call_function_n_kw` on ctx-only paths.
                match call.as_deref_mut() {
                    Some(hook) => hook(ctx, fun, n_args, n_kw, args),
                    None => call_function_n_kw(ctx, fun, n_args, n_kw, args)
                        .map_err(|e| e.to_pyerror()),
                }
            };
            let mut load_c =
                |ctx: &mut CallCtx, obj: Value, attr: u32| -> Option<(Value, Option<Value>)> {
                    load_method_maybe(ctx, obj, attr, next_obj)
                        .ok()
                        .flatten()
                        .map(|d| (d.func, d.self_))
                };
            let mut unary_c = |ctx: &mut CallCtx,
                               op: crate::objects::slots::UnaryOp,
                               arg: Value|
             -> Result<Option<Value>, PyError> {
                unary_op(ctx, op, arg).map_err(|e| e.to_pyerror())
            };
            let mut binary_c = |ctx: &mut CallCtx,
                                op: crate::objects::slots::BinaryOp,
                                lhs: Value,
                                rhs: Value|
             -> Result<Option<Value>, PyError> {
                binary_op(ctx, op, lhs, rhs, exc, next_obj, builtins, seams, globals)
                    .map(Some)
                    .map_err(|e| e.to_pyerror())
            };
            let mut subscr_c = |ctx: &mut CallCtx,
                                b: Value,
                                i: Value,
                                a: SubscrArg|
             -> Result<Value, SubscrError> {
                match obj_subscr(ctx, &ctx.types, b, i, a) {
                    Ok(v) => Ok(v),
                    Err(ObjError::OutOfMemory) => Err(SubscrError::Raise(PyError::OutOfMemory)),
                    Err(e) => Err(SubscrError::Raise(e.to_pyerror())),
                }
            };
            let mut seams2 = VmSeams {
                call: &mut call_c,
                load_maybe: &mut load_c,
                unary: &mut unary_c,
                binary: &mut binary_c,
                subscr: &mut subscr_c,
            };
            instance_store_attr(ctx, obj, attr, value, &mut seams2)
                .map_err(RtError::from)
                .and_then(|handled| {
                    if handled {
                        Ok(())
                    } else {
                        Err(RtError::AttributeError(format!(
                            "no attribute '{attr_name}'"
                        )))
                    }
                })
        }
        Kind::Type => match type_store_attr(ctx, obj, attr, value).map_err(RtError::from)? {
            true => Ok(()),
            false => Err(RtError::AttributeError(format!(
                "no attribute '{attr_name}'"
            ))),
        },
        Kind::Exception => {
            match exception_store_attr(ctx, obj, attr, value).map_err(RtError::from)? {
                true => Ok(()),
                false => Err(RtError::AttributeError(format!(
                    "no attribute '{attr_name}'"
                ))),
            }
        }
        Kind::Module => {
            // The sys fixed-dict rule needs the builtins
            // module's dict (to recognise a store into
            // `builtins` itself, which lands in the override
            // dict) and the override dict by mutable reference
            // (a fresh one is installed on first store).
            let builtins_globals = match builtins.globals {
                Value::Heap(r) => Some(r),
                _ => None,
            };
            let mut od: Option<GcRef> = match builtins.override_dict {
                Some(Value::Heap(r)) => Some(r),
                _ => None,
            };
            let r = module_store_attr(ctx, obj, attr, value, builtins_globals, &mut od);
            if let Some(nr) = od {
                builtins.override_dict = Some(Value::Heap(nr));
            }
            match r {
                Ok(true) => Ok(()),
                Ok(false) => Err(RtError::AttributeError(format!(
                    "no attribute '{attr_name}'"
                ))),
                Err(crate::objects::containers::objdict::DictError::KeyError(k)) => {
                    // `del module.missing` → `KeyError(key)`:
                    // build the object here so the kind
                    // survives the `RtError` boundary as
                    // `Raised` (probed: `del mymod.nonexistent`
                    // → `KeyError 'nonexistent'`).
                    let e = crate::objects::objtype::objexcept::new_exception_args(
                        ctx,
                        exc.key_error,
                        &[k],
                    );
                    Err(RtError::Raised(e))
                }
                Err(e) => Err(crate::runtime::runtime_names::dict_err_to_rt(e)),
            }
        }
        Kind::Other => Err(RtError::AttributeError(format!(
            "no attribute '{attr_name}'"
        ))),
    }
}

// ── the `next` builtin ──────────────────────────────────────────────

/// `next2Impl` (`modbuiltins.zig:297-324`): 1-arg → the
/// `iternext_allow_raise` value, raising `StopIteration` on
/// exhaustion; 2-arg → `iternext` with the default on
/// exhaustion. Raises travel as `RtError::StopIteration` so the
/// VM can build the exception object.
pub fn builtin_next(
    ctx: &mut CallCtx,
    args: &[Value],
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let (iter, default) = match args.len() {
        1 => (args[0], None),
        2 => (args[0], Some(args[1])),
        _ => return Err(RtError::PyRaise),
    };
    match default {
        None => match iternext_allow_raise(ctx, iter, next_obj, seams)? {
            IterNext::Value(v) => Ok(v),
            IterNext::Stop(sv) => Err(RtError::StopIteration(sv)),
        },
        Some(dflt) => match iternext(ctx, iter, next_obj, seams)? {
            IterNext::Value(v) => Ok(v),
            IterNext::Stop(_) => Ok(dflt),
        },
    }
}

/// The bare-`BuiltinFn` adapter for the `next` builtin: it
/// cannot capture seams (a bare fn pointer), so it runs with
/// `RuntimeSeams::stub()` and a freshly built `next` object
/// for the `__next__` fast path. The full path (`builtin_next`
/// with explicit seams) is implemented and tested; non-generator
/// iterators work fully through the adapter, and `next(gen)`
/// through the adapter returns `PyRaise` until C13 grows
/// `CallCtx` into the full `Vm` (the `CallCtx` docs promise
/// this growth).
fn next_builtin_fn<'a>(ctx: &mut CallCtx<'a>, args: &[Value]) -> PyResult<Value> {
    let next_obj = next_builtin_obj(ctx);
    builtin_next(ctx, args, next_obj, &mut RuntimeSeams::stub()).map_err(|e| e.to_pyerror())
}

/// `next_obj`: the `next` builtin as a `FunBuiltin` (header
/// `None` — the bootstrap pattern: `fun_builtin_N` type
/// objects do not exist yet). D1 installs the object in the
/// builtins table.
pub fn next_builtin_obj(ctx: &mut CallCtx) -> Value {
    let name = dunder(b"next").expect("next is a static qstr");
    Value::Heap(ctx.alloc(
        None,
        Obj::FunBuiltin(FunBuiltinData {
            name,
            n_args: (1 << 8) | 2,
            func: next_builtin_fn as BuiltinFn,
            kw_func: None,
        }),
    ))
}
