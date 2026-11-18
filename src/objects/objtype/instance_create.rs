//! Instance creation (`objtype.zig:2034-2214`): `mp_obj_new_instance`,
//! `instance_count_native_bases`, `native_base_init_wrapper`,
//! `static_class_method_make_new`, `instanceMakeNewImpl` and
//! `mp_load_super_method`.
//!
//! Split out of `typecreate.rs` (the 800-line file cap). The
//! type-creation half (`mp_obj_new_type`, `type`, `super`) lives
//! there; this file owns everything that CONSTRUCTS instances.
//! `instance_make_new` is referenced by `typecreate.rs`'s
//! `TypeData` literal for user types; `load_super_method` calls
//! `typecreate.rs`'s `super_attr_impl` directly (the temporary
//! `Super` object the Zig allocates is unobservable).

use crate::memory::gc::CallCtx;
use crate::objects::slots::CallHook;
use crate::objects::callables::objfun::fun_builtin_var_type_data;
use crate::objects::obj::{obj_get_type, type_data, FLAG_INSTANCE_TYPE};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{
    GcRef, InstanceData, MapData, Obj, StaticClassMethodData, Value,
};
use crate::pyerr::{PyError, PyResult};

use super::errors::{arg_check_num_sig, ObjTypeError};
use super::lookup::{class_lookup, LookupHit, SlotKind};
use super::seams::{with_call_hook_seams, with_stub_seams, VmSeams};
use super::typecreate::super_attr_impl;

/// A dunder name's static qstr id (`None` = not a static qstr).
fn dunder(name: &[u8]) -> Option<u32> {
    static_qstr_id(name)
}


/// `instance_count_native_bases` (`objtype.zig:2034-2068`): how many
/// native bases a type has (0 or 1 for a well-formed type — more
/// than 1 is the layout conflict `mp_obj_new_type` rejects), plus
/// the LAST native base found. `object` stops the walk with 0; a
/// native type stops it with 1; a type with no parent stops it with
/// 0; an MI bases tuple SUMS the recursive counts (so
/// `class C(A, B)` with both `A` and `B` subclassing `int` counts
/// 2 — the mro-flattened count would wrongly say 1, which is why
/// `TypeData.bases` exists at all).
pub fn count_native_bases(ctx: &CallCtx, type_: GcRef) -> (usize, Option<GcRef>) {
    let mut count = 0usize;
    let mut last_native_base = None;
    let mut t = type_;
    loop {
        if t == ctx.types.object {
            break;
        }
        let Some(d) = type_data(ctx.arena, t) else {
            break;
        };
        if d.flags & FLAG_INSTANCE_TYPE == 0 {
            count += 1;
            last_native_base = Some(t);
            break;
        }
        if let Some(bases) = d.bases {
            if let Obj::Tuple(items) = ctx.arena.get(bases) {
                for item in items {
                    if let Value::Heap(bt) = item {
                        let (c, nb) = count_native_bases(ctx, *bt);
                        count += c;
                        if nb.is_some() {
                            last_native_base = nb;
                        }
                    }
                }
            }
            break;
        }
        match d.base {
            Some(parent) => t = parent,
            None => break,
        }
    }
    (count, last_native_base)
}

/// `mp_obj_new_instance` (`objtype.zig:2084-2100`): allocate a blank
/// instance of `class` — empty members map, `subobj` sized by the
/// native-base count (0 or 1 slot). A non-zero count primes
/// `subobj[0]` with the `native_base_init_wrapper` sentinel: the
/// native base is still uninitialised, and `__init__` (or
/// `instance_make_new`'s tail) constructs it through the wrapper.
pub fn new_instance(ctx: &mut CallCtx, class: GcRef) -> Value {
    let (count, _) = count_native_bases(ctx, class);
    debug_assert!(
        count < 2,
        "new_instance on a type with 2+ native bases (mp_obj_new_type rejects those)"
    );
    let mut subobj = Vec::with_capacity(count);
    if count != 0 {
        subobj.push(Value::Heap(ctx.types.native_init_wrapper));
    }
    let r = ctx.alloc(
        Some(class),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj,
        }),
    );
    Value::Heap(r)
}

/// `native_base_init_wrapper` (`objtype.zig:2070-2083`): the
/// `BuiltinKwFn` behind the wrapper object. `args[0]` is the
/// instance; the native base is recomputed from the instance's
/// type; the native base's `make_new` slot is called with the
/// remaining positionals plus the kwargs (rebuilt into the flat
/// layout); the result replaces `subobj[0]`; returns `None`.
/// (The `func` field of the wrapper object is an unreachable stub
/// — the call dispatch prefers `kw_func` when set, see
/// `call_fun_builtin`.)
pub fn native_base_init_wrapper(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let Some(instance_v) = args.first().copied() else {
        return Err(PyError::PyRaise);
    };
    let Value::Heap(ir) = instance_v else {
        return Err(PyError::PyRaise);
    };
    let (type_, has_subobj) = match ctx.arena.get(ir) {
        Obj::Instance(d) => (ctx.arena.header(ir).type_, !d.subobj.is_empty()),
        _ => return Err(PyError::PyRaise),
    };
    let Some(native_base) = type_.and_then(|t| count_native_bases(ctx, t).1) else {
        return Err(PyError::PyRaise);
    };
    let mut flat: Vec<Value> = Vec::with_capacity(args.len() - 1 + 2 * kwargs.len());
    flat.extend_from_slice(&args[1..]);
    for (name, value) in kwargs {
        flat.push(Value::Qstr(*name));
        flat.push(*value);
    }
    let make_new = type_data(ctx.arena, native_base).and_then(|d| d.make_new);
    let Some(f) = make_new else {
        return Err(PyError::PyRaise);
    };
    let result = f(ctx, native_base, &flat, kwargs.len(), None)?;
    if !has_subobj {
        return Err(PyError::PyRaise);
    }
    if let Obj::Instance(d) = ctx.arena.get_mut(ir) {
        if !d.subobj.is_empty() {
            d.subobj[0] = result;
        }
    }
    Ok(Value::None)
}

/// The wrapper's unreachable `func` (see `native_base_init_wrapper`).
fn wrapper_unreachable(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// Allocate the `native_base_init_wrapper_obj` singleton
/// (`objtype.zig:734`): a `FunBuiltinVar` named `__init__` whose
/// `kw_func` is `native_base_init_wrapper`. Call this ONCE per
/// registry (the ROM): the returned value's IDENTITY is what
/// `new_instance` primes into `subobj[0]` and what
/// `instance_make_new` compares against, so two wrappers are NOT
/// interchangeable. (`n_args` is `fun_n_args(1, 255)` — the u16
/// packing caps `max` at 255; the Zig's `0xffff` is unrepresentable
/// in the port, documented.)
pub fn native_base_init_wrapper_obj(ctx: &mut CallCtx, dict: GcRef) -> Value {
    let type_ = ctx.alloc(None, Obj::Type(fun_builtin_var_type_data(dict)));
    let name = static_qstr_id(b"__init__").expect("__init__ is a static qstr");
    let data = crate::objects::value::FunBuiltinData {
        name,
        n_args: fun_n_args(1, 255),
        func: wrapper_unreachable as crate::objects::slots::BuiltinFn,
        kw_func: Some(native_base_init_wrapper),
    };
    Value::Heap(ctx.alloc(Some(type_), Obj::FunBuiltin(data)))
}

/// `static_class_method_make_new` (`objtype.zig:2112-2118`): the
/// `staticmethod` / `classmethod` type's `make_new` slot. Sig (1, 1),
/// polled — wrong arity surfaces the terse sig error (probed:
/// `staticmethod()` / `staticmethod(1, 2)` / `classmethod()`).
/// Allocates a `StaticClassMethod{fun: args[0]}` with the header
/// type = the called type (staticmethod or classmethod).
pub fn static_class_method_make_new(
    ctx: &mut CallCtx,
    self_type: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let sig_err = arg_check_num_sig(n_pos, n_kw, 1, 1, false);
    let result: Result<Value, ObjTypeError> = if n_pos == 1 {
        let r = ctx.alloc(
            Some(self_type),
            Obj::StaticClassMethod(StaticClassMethodData { fun: args[0] }),
        );
        Ok(Value::Heap(r))
    } else {
        Err(ObjTypeError::ArgNumTypesMismatch)
    };
    match result {
        Ok(v) => match sig_err {
            Some(e) => Err(e.to_pyerror()),
            None => Ok(v),
        },
        Err(e) => Err(e.to_pyerror()),
    }
}

/// `instanceMakeNewImpl` (`objtype.zig:2124-2200`) as a `MakeNewFn`:
/// `Some` hook → the VM's real call hook drives `__new__` /
/// `__init__` (`with_call_hook_seams` — a `FunBc` callee actually
/// executes, E2); `None` → the stub seams (a `FunBc` callee
/// raises `PyRaise`, the pre-E2 behaviour).
pub fn instance_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    match call {
        Some(hook) => with_call_hook_seams(hook, |seams| {
            instance_make_new_inner(ctx, type_, args, n_kw, seams)
        }),
        None => {
            with_stub_seams(|seams| instance_make_new_inner(ctx, type_, args, n_kw, seams))
        }
    }
}

/// `instanceMakeNewImpl` (`objtype.zig:2124-2200`): construct an
/// instance of `type_`. The `__new__` lookup passes `obj = NULL`
/// with slot `MakeNew`: `NULL` or `SENTINEL` → a blank instance
/// (the native base is constructed LAST, so `__init__` can do it
/// via `super().__init__()`); a member → a Python `__new__` call
/// with `(self_type, *args, **kwargs)` — when its result's type is
/// not `self_type`, `__init__` is SKIPPED and the result returned
/// (probed: `N()` = 42). `__init__` is looked up with slot 0 on
/// the instance; a raise propagates (it is NOT overwritten by the
/// None-check); a non-`None` return raises `InitMustReturnNone`
/// (probed: "__init__() should return None").
pub fn instance_make_new_inner(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    seams: &mut VmSeams,
) -> PyResult<Value> {
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let new_q = dunder(b"__new__").expect("__new__ is a static qstr");
    let init_q = dunder(b"__init__").expect("__init__ is a static qstr");
    let new_hit = class_lookup(
        ctx,
        None,
        new_q,
        Some(SlotKind::MakeNew),
        false,
        type_,
        seams,
    );
    match new_hit {
        None | Some(LookupHit::NativeSlot(_)) => {
            let instance = new_instance(ctx, type_);
            run_init(ctx, instance, type_, init_q, args, n_pos, n_kw, seams)?;
            // Native-base construction, blank branch ONLY: if the
            // native base is still the wrapper (i.e. `__init__`
            // did not construct it via `super().__init__()`),
            // construct it now with the original args.
            if let Some(nb) = count_native_bases(ctx, type_).1 {
                let Value::Heap(ir) = instance else {
                    return Ok(instance);
                };
                let still_wrapper = match ctx.arena.get(ir) {
                    Obj::Instance(d) => {
                        d.subobj.first()
                            == Some(&Value::Heap(ctx.types.native_init_wrapper))
                    }
                    _ => false,
                };
                if still_wrapper {
                    if let Some(f) = type_data(ctx.arena, nb).and_then(|d| d.make_new) {
                        // Forward the call seam as the hook: a
                        // native base built from an iterable
                        // (e.g. `class D(dict): ...; D([('k', 1)])`)
                        // gets the same hook the `__init__` path
                        // runs on (E2).
                        let result = f(ctx, nb, args, n_kw, Some(&mut *seams.call))?;
                        if let Obj::Instance(d) = ctx.arena.get_mut(ir) {
                            if !d.subobj.is_empty() {
                                d.subobj[0] = result;
                            }
                        }
                    }
                }
            }
            Ok(instance)
        }
        Some(LookupHit::Member(fun, bound)) => {
            // Python `__new__`: call with (self_type, *args, **kwargs).
            let mut new_args: Vec<Value> = Vec::with_capacity(1 + args.len());
            new_args.push(Value::Heap(type_));
            new_args.extend_from_slice(args);
            let result = seams.call_pair(ctx, fun, bound, 1 + n_pos, n_kw, &new_args)?;
            if obj_get_type(ctx.arena, ctx.types, result) != Some(type_) {
                return Ok(result);
            }
            run_init(ctx, result, type_, init_q, args, n_pos, n_kw, seams)?;
            Ok(result)
        }
    }
}

/// Look `__init__` up (slot 0, on the instance) and call it with
/// the original args; a non-`None` return raises
/// `InitMustReturnNone`. A raise from the call propagates (the `?`
/// runs before the None-check, as in the Zig).
#[allow(clippy::too_many_arguments)]
fn run_init(
    ctx: &mut CallCtx,
    instance: Value,
    type_: GcRef,
    init_q: u32,
    args: &[Value],
    n_pos: usize,
    n_kw: usize,
    seams: &mut VmSeams,
) -> PyResult<()> {
    let init_hit = class_lookup(ctx, Some(instance), init_q, None, false, type_, seams);
    if let Some(LookupHit::Member(fun, bound)) = init_hit {
        let ret = seams.call_pair(ctx, fun, bound, n_pos, n_kw, args)?;
        if !matches!(ret, Value::None) {
            return Err(ObjTypeError::InitMustReturnNone.to_pyerror());
        }
    }
    Ok(())
}

/// `mp_load_super_method` (`objtype.zig:463-477`): load `attr`
/// through a temporary `Super{type, obj}`. The temporary object
/// itself is unobservable, so this calls `super_attr`'s logic
/// directly instead of allocating one. `None` = the super lookup
/// found nothing. (C13/C14 replace the stub seams inside
/// `super_attr`.)
pub fn load_super_method(
    ctx: &mut CallCtx,
    type_: GcRef,
    self_obj: Value,
    attr: u32,
) -> PyResult<Option<Value>> {
    with_stub_seams(|seams| super_attr_impl(ctx, type_, self_obj, attr, seams))
}
