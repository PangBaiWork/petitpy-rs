//! Type creation and the `type` / `super` machinery (`objtype.zig:1486-2214`).
//!
//! This is the second half of the objtype lane: everything that
//! CREATES types and instances, plus the `type` and `super` types
//! themselves. The lookup / subclass / instance-slot half lives in
//! `lookup.rs`, `subclass.rs` and `instance.rs`.
//!
//! Fidelity notes (all verified against `objtype.zig` and the
//! reference binary; the Zig line refs are on each item):
//!
//! * `type()` and `super()` follow the POLLED sig model (see
//!   `errors.rs::arg_check_num_sig`): the sig check records the
//!   default error, the logic runs with missing args modelled as
//!   `None`, a logic error overwrites the sig error, and on logic
//!   success a sig error surfaces. Probed: `super(int)` → bare
//!   `TypeError`; `super(int, 1, 2)` / `super(int, 1, x=1)` /
//!   `type("X", (object,), {}, extra=1)` / `type(1, x=1)` → the
//!   terse `argument num/types mismatch`; `super(1)` / `super()` →
//!   the `issubclass() arg 2` error.
//! * `mp_obj_new_type` stores `__mro__` in the locals dict ONLY
//!   when `bases_len >= 2` or the single base has a cached MRO
//!   (`objtype.zig:1975-1985`); the layout-conflict check runs
//!   AFTER that store (`:1988-1992`); `__new__` is wrapped only
//!   when `is_fun` (a heap object whose type's NAME is `function` —
//!   the builtin function types are ALSO named `function` in the
//!   Zig, `objfun.zig:410,486`, so a builtin `__new__` wraps too);
//!   `__set_name__` is snapshot-then-call (`:1934-1968`).
//! * `super_attr`'s MRO branch walks each subsequent MRO class
//!   with per-class `lookupIn` (NOT `class_lookup` —
//!   `objtype.zig:1578-1596`); its last resort is a `class_lookup`
//!   from `object` with slot 0 (`:1630-1637`).
//! * The `AttrFn` type is LOAD-only (see `slots.rs`): the store
//!   paths are free fns returning `PyResult<bool>` (`type_store_attr`
//!   here, `instance_store_attr` in `instance.rs`), the same split
//!   the C9c property lane established.
//! * Rooting: `mp_obj_new_type` roots the new type in `ctx.roots`
//!   across the fallible post-allocation steps (MRO, `__set_name__`
//!   calls) and unroots it before returning — the CALLER roots the
//!   returned value (the VM roots call results). `new_instance`
//!   returns its instance unrooted for the same reason.
//! * The name of a type created from a heap `str` is interned
//!   through `get_qstr` / the pool's `find_strn`. A heap str whose
//!   content is neither static nor already in the pool cannot be
//!   interned without the VM's `&mut` pool (C13 gap) —
//!   `type_name_qstr` reports `OutOfMemory` for it (the pool's own
//!   failure signal), documented, not silently renamed.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{
    dict_delete, dict_get_value, dict_store, is_ordered_dict_type, DictError,
};
use crate::objects::obj::{obj_get_type, type_data};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::strings::objstr_b::{get_qstr, get_str};
use crate::objects::value::{
    GcRef, Obj, StaticClassMethodData, SuperData, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};

use super::errors::{arg_check_num_sig, ObjTypeError};
use super::instance::{
    bind_pair, instance_attr, instance_binary_op_slot, instance_call_slot,
    instance_get_buffer_slot, instance_getiter_slot, instance_subscr_slot,
    instance_unary_op_slot,
};
use super::instance_print::instance_print_slot;
use super::lookup::{
    attr_value_is_special_accessor, c3_mro, cached_mro, class_lookup,
    lookup_in, mro_of, mro_tuple, scan_special_accessors_flag,
    type_bases_tuple, type_locals_dict_readonly, LookupHit, SlotKind,
};
use super::instance_create::{count_native_bases, instance_make_new};
use super::seams::{with_stub_seams, VmSeams};
use super::subclass::{is_exact_type, is_subclass, is_type_value};

/// `TYPE_BASE_FLAGS` (`objtype.zig:1898`): the flags every type
/// created by `mp_obj_new_type` starts with — `EQ_NOT_REFLEXIVE |
/// EQ_CHECKS_OTHER_TYPE | EQ_HAS_NEQ_TEST | ITER_IS_GETITER(0) |
/// INSTANCE_TYPE = 0x021C`. The tests' `USER_FLAGS` mirrors this
/// constant (see `tests.rs`).
pub const TYPE_BASE_FLAGS: u16 = 0x021C;

/// A dunder name's static qstr id (`None` = not a static qstr).
fn dunder(name: &[u8]) -> Option<u32> {
    static_qstr_id(name)
}

/// Map a dict-layer error onto the objtype error type. The only
/// dict errors the type-creation paths can see are a fixed-map
/// mutation (bare `TypeError`) and `OutOfMemory`; a `KeyError`
/// cannot arise (all stores use qstr keys that always hash).
fn map_dict_err(e: DictError) -> ObjTypeError {
    match e {
        DictError::OutOfMemory => ObjTypeError::OutOfMemory,
        _ => ObjTypeError::BareTypeError,
    }
}

/// `isFun` (`objtype.zig:349`): a heap object whose type's NAME is
/// `function`. Note this is a NAME check, not a flags check — the
/// builtin function types are also named `function` in the Zig
/// (`objfun.zig:410,486`), so a builtin `__new__` counts.
pub fn is_fun(ctx: &CallCtx, v: Value) -> bool {
    let Value::Heap(_) = v else {
        return false;
    };
    let Some(t) = obj_get_type(ctx.arena, ctx.types, v) else {
        return false;
    };
    let Some(d) = type_data(ctx.arena, t) else {
        return false;
    };
    Some(d.name) == static_qstr_id(b"function")
}

/// The name qstr of a `type()` call's first argument: a `Qstr`
/// value carries its id; a heap `str` goes through `get_qstr`
/// (static content) or the pool's `find_strn` (content already
/// interned). A non-str raises `NotStr` ("can't convert to str
/// implicitly" — probed: `type(1, (), {})`); a heap str whose
/// content is neither static nor in the pool is the C13 gap (see
/// the module docs) and reports `OutOfMemory`.
fn type_name_qstr(ctx: &mut CallCtx, name_v: Value) -> Result<u32, ObjTypeError> {
    if let Some(id) = get_qstr(ctx.arena, ctx.types, name_v) {
        return Ok(id);
    }
    if let Some(bytes) = get_str(ctx.arena, name_v) {
        let id = ctx.pool.find_strn(bytes);
        if id != 0 {
            return Ok(id as u32);
        }
        return Err(ObjTypeError::OutOfMemory);
    }
    Err(ObjTypeError::NotStr)
}

// ── type() ───────────────────────────────────────────────────────────

/// `typeMakeNewImpl` (`objtype.zig:1486-1502`): the `type` type's
/// `make_new` slot. Sig (1, 3), polled. One arg → that arg's type;
/// three args → `mp_obj_new_type`; anything else → `TypeTakesOneOrThree`.
pub fn type_make_new(
    ctx: &mut CallCtx,
    _type: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let sig_err = arg_check_num_sig(n_pos, n_kw, 1, 3, false);
    let result: Result<Value, ObjTypeError> = if n_pos == 1 {
        if is_type_value(ctx.arena, args[0]) {
            // A type value's type is the `type` type (its header
            // is `None` in the fixture, so `obj_get_type` cannot
            // see it — see `is_type_value`).
            Ok(Value::Heap(ctx.types.type_))
        } else {
            match obj_get_type(ctx.arena, ctx.types, args[0]) {
                Some(t) => Ok(Value::Heap(t)),
                None => Err(ObjTypeError::BareTypeError),
            }
        }
    } else if n_pos == 3 {
        let name = type_name_qstr(ctx, args[0]).map_err(|e| e.to_pyerror())?;
        with_stub_seams(|seams| mp_obj_new_type(ctx, name, args[1], args[2], seams))
    } else {
        Err(ObjTypeError::TypeTakesOneOrThree)
    };
    match result {
        Ok(v) => match sig_err {
            Some(e) => Err(e.to_pyerror()),
            None => Ok(v),
        },
        Err(e) => Err(e.to_pyerror()),
    }
}

/// `typeCallImpl` (`objtype.zig:1508-1515`): the `type` type's
/// `call` slot. A type without a `make_new` slot cannot be called
/// (`CantCreateInstance`); otherwise the call forwards to the
/// type's own `make_new` slot with the flat `(args, n_kw)` layout.
pub fn type_call(
    ctx: &mut CallCtx,
    self_in: Value,
    _n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let Value::Heap(t) = self_in else {
        return Err(ObjTypeError::BareTypeError.to_pyerror());
    };
    let make_new = type_data(ctx.arena, t).and_then(|d| d.make_new);
    match make_new {
        Some(f) => f(ctx, t, args, n_kw),
        // (C13b: record the message — the VM builds the
        // exception from `pending_raise`; a bare `to_pyerror`
        // would surface a message-less `TypeError`. No callee
        // ran here, so nothing in `pending_exc` can be masked.)
        None => {
            let e = ObjTypeError::CantCreateInstance;
            ctx.record_raise("TypeError", e.message());
            Err(PyError::PyRaise)
        }
    }
}

// ── super() ──────────────────────────────────────────────────────────

/// `superMakeNewImpl` (`objtype.zig:1546-1557`): the `super` type's
/// `make_new` slot. Sig (2, 2), polled; missing args are modelled
/// as `None`. `second_arg_obj` is `args[1]` when it is a type, else
/// the TYPE of `args[1]`; `is_subclass(second, args[0])` false →
/// bare `TypeError` (probed: `super(1, 2)`, `super(int, "x")`).
pub fn super_make_new(
    ctx: &mut CallCtx,
    _type: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let sig_err = arg_check_num_sig(n_pos, n_kw, 2, 2, false);
    let result: Result<Value, ObjTypeError> = (|| {
        let a0 = args.get(0).copied().unwrap_or(Value::None);
        let a1 = args.get(1).copied().unwrap_or(Value::None);
        let second = if is_type_value(ctx.arena, a1) {
            a1
        } else {
            match obj_get_type(ctx.arena, ctx.types, a1) {
                Some(t) => Value::Heap(t),
                None => return Err(ObjTypeError::BareTypeError),
            }
        };
        let ok = with_stub_seams(|seams| is_subclass(ctx, second, a0, seams))?;
        if !matches!(ok, Value::Bool(true)) {
            return Err(ObjTypeError::BareTypeError);
        }
        let Value::Heap(a0t) = a0 else {
            // Unreachable: `is_subclass` already errored unless
            // `a0` is a type, and every type value is a heap ref.
            return Err(ObjTypeError::BareTypeError);
        };
        let r = ctx.alloc(
            Some(ctx.types.super_),
            Obj::Super(SuperData { type_: a0t, obj: a1 }),
        );
        Ok(Value::Heap(r))
    })();
    match result {
        Ok(v) => match sig_err {
            Some(e) => Err(e.to_pyerror()),
            None => Ok(v),
        },
        Err(e) => Err(e.to_pyerror()),
    }
}

/// `super_attr` (`objtype.zig:1562-1640`): the `super` type's `attr`
/// slot (LOAD-only). `slot_offset` is `MAKE_NEW` for `__init__`,
/// else 0. The MRO branch walks each class AFTER `self_type` in
/// the cached MRO of the type of `self_obj`, with per-class
/// `lookupIn`; the parent branch walks the MI bases tuple (or the
/// single parent) with `class_lookup`; the last resort is a
/// `class_lookup` from `object` with slot 0. A `NativeSlot` hit
/// resolves to the `native_base_init_wrapper` bound to `self_obj`.
pub fn super_attr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
) -> PyResult<Option<Value>> {
    let Value::Heap(r) = self_in else {
        return Ok(None);
    };
    let (self_type, self_obj) = match ctx.arena.get(r) {
        Obj::Super(d) => (d.type_, d.obj),
        _ => return Ok(None),
    };
    with_stub_seams(|seams| super_attr_impl(ctx, self_type, self_obj, attr, seams))
}

pub(crate) fn super_attr_impl(
    ctx: &mut CallCtx,
    self_type: GcRef,
    self_obj: Value,
    attr: u32,
    seams: &mut VmSeams,
) -> PyResult<Option<Value>> {
    let slot = if Some(attr) == dunder(b"__init__") {
        Some(SlotKind::MakeNew)
    } else {
        None
    };
    // `cachedMro(type_of(self_obj))` — `self_obj` itself when it
    // is a type.
    let obj_type = if is_type_value(ctx.arena, self_obj) {
        match self_obj {
            Value::Heap(t) => t,
            _ => return Ok(None),
        }
    } else {
        match obj_get_type(ctx.arena, ctx.types, self_obj) {
            Some(t) => t,
            None => return Ok(None),
        }
    };
    let mro = cached_mro(ctx, obj_type);
    let mro_applies = mro
        .as_ref()
        .is_some_and(|m| m.iter().any(|v| *v == Value::Heap(self_type)));
    let mut hit: Option<LookupHit> = None;
    if mro_applies {
        // The MRO branch: walk the classes AFTER self_type.
        let mro = mro.unwrap();
        let idx = mro
            .iter()
            .position(|v| *v == Value::Heap(self_type))
            .unwrap();
        for u in &mro[idx + 1..] {
            let Value::Heap(ut) = u else {
                continue;
            };
            if *ut == ctx.types.object {
                continue;
            }
            if let Some(h) =
                lookup_in(ctx, Some(self_obj), attr, slot, false, *ut, *ut, seams)
            {
                hit = Some(h);
                break;
            }
        }
        // Found nothing in the MRO tail: the parent branch is
        // skipped (it is an `else if` in the Zig) — fall through
        // to the last resort below.
    } else {
        // The parent branch (`typeHasParent`): copy the parent
        // fields out FIRST — `class_lookup` takes `&mut ctx`,
        // which conflicts with a live `&TypeData` borrow.
        let (bases_opt, base_opt) = match type_data(ctx.arena, self_type) {
            Some(d) => (d.bases, d.base),
            None => (None, None),
        };
        if let Some(bases) = bases_opt {
            // MI: walk the bases tuple with class_lookup.
            if let Obj::Tuple(items) = ctx.arena.get(bases) {
                for item in items.clone() {
                    let Value::Heap(bt) = item else {
                        continue;
                    };
                    if bt == ctx.types.object {
                        continue;
                    }
                    if let Some(h) =
                        class_lookup(ctx, Some(self_obj), attr, slot, false, bt, seams)
                    {
                        hit = Some(h);
                        break;
                    }
                }
            }
        } else if let Some(p) = base_opt {
            if p != ctx.types.object {
                hit = class_lookup(ctx, Some(self_obj), attr, slot, false, p, seams);
            }
        }
    }
    match hit {
        Some(LookupHit::NativeSlot(_)) => Ok(Some(bind_pair(
            ctx,
            Value::Heap(ctx.types.native_init_wrapper),
            Some(self_obj),
        ))),
        Some(LookupHit::Member(fun, bound)) => Ok(Some(bind_pair(ctx, fun, bound))),
        None => {
            // Last resort: `object` with slot 0.
            match class_lookup(ctx, Some(self_obj), attr, None, false, ctx.types.object, seams)
            {
                Some(LookupHit::Member(fun, bound)) => {
                    Ok(Some(bind_pair(ctx, fun, bound)))
                }
                _ => Ok(None),
            }
        }
    }
}

/// `super_print` (`objtype.zig:1532-1542`): `<super: {type}, {obj}>`,
/// both printed with `PRINT_STR` (kind 0).
pub fn super_print(
    ctx: &mut CallCtx,
    self_in: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let (t, obj) = match self_in {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Super(d) => (Value::Heap(d.type_), d.obj),
            _ => return,
        },
        _ => return,
    };
    let _ = sink.write_str("<super: ");
    obj_print_helper(ctx, ctx.types, t, PrintKind::Str, sink);
    let _ = sink.write_str(", ");
    obj_print_helper(ctx, ctx.types, obj, PrintKind::Str, sink);
    let _ = sink.write_str(">");
}

// ── the type type's own slots ────────────────────────────────────────

/// `type_print` (`objtype.zig:1688-1693`): `<class 'name'>`; the
/// kind is ignored.
pub fn type_print(
    ctx: &mut CallCtx,
    self_in: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let Value::Heap(r) = self_in else {
        return;
    };
    let Some(d) = type_data(ctx.arena, r) else {
        return;
    };
    let name = ctx.pool.qstr_str(d.name as u16);
    let _ = sink.write_str("<class '");
    let _ = sink.write_str(&String::from_utf8_lossy(name));
    let _ = sink.write_str("'>");
}

/// `type_attr` (`objtype.zig:1701-1759`, load half): the `type`
/// type's `attr` slot. `__name__` is ungated; `__dict__` /
/// `__bases__` / `__mro__` are full-only (we are the full config);
/// anything else goes through `class_lookup` with `is_type=true`,
/// `obj` = the type itself, slot 0.
pub fn type_attr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
) -> PyResult<Option<Value>> {
    let Value::Heap(r) = self_in else {
        return Ok(None);
    };
    let Some(d) = type_data(ctx.arena, r) else {
        return Ok(None);
    };
    let name = d.name;
    if Some(attr) == dunder(b"__name__") {
        return Ok(Some(Value::Qstr(name)));
    }
    if Some(attr) == dunder(b"__dict__") {
        return Ok(Some(type_locals_dict_readonly(ctx, r)?));
    }
    if Some(attr) == dunder(b"__bases__") {
        return Ok(Some(type_bases_tuple(ctx, r)));
    }
    if Some(attr) == dunder(b"__mro__") {
        return Ok(Some(mro_tuple(ctx, r)));
    }
    with_stub_seams(|seams| match class_lookup(ctx, Some(self_in), attr, None, true, r, seams)
    {
        Some(LookupHit::Member(fun, bound)) => Ok(Some(bind_pair(ctx, fun, bound))),
        // Slot 0 cannot produce a `NativeSlot` hit; no Value
        // spelling exists for one.
        Some(LookupHit::NativeSlot(_)) => Ok(None),
        None => Ok(None),
    })
}

/// `type_attr`'s store half (`objtype.zig:1740-1759`): a free fn —
/// the `AttrFn` type is load-only. `Ok(true)` = stored/deleted,
/// `Ok(false)` = not handled (a fixed locals map is a no-op in the
/// Zig — the store simply does not happen). `value = None` deletes.
/// A stored special accessor sets `HAS_SPECIAL_ACCESSORS` on the
/// type. (The Zig's "no locals dict" case is vacuous in Rust:
/// every `TypeData` carries its dict.)
pub fn type_store_attr(
    ctx: &mut CallCtx,
    self_in: Value,
    attr: u32,
    value: Option<Value>,
) -> PyResult<bool> {
    let Value::Heap(r) = self_in else {
        return Ok(false);
    };
    let (dict, is_fixed, has_special) = match type_data(ctx.arena, r) {
        Some(d) => {
            let is_fixed = match ctx.arena.get(d.dict) {
                Obj::Dict(dd) => dd.map.is_fixed,
                _ => false,
            };
            (d.dict, is_fixed, d.flags & crate::objects::obj::FLAG_HAS_SPECIAL_ACCESSORS != 0)
        }
        None => return Ok(false),
    };
    if is_fixed {
        return Ok(false);
    }
    let dict_v = Value::Heap(dict);
    match value {
        None => {
            // Delete: `REMOVE_IF_FOUND` — absent keys are a no-op.
            match dict_get_value(ctx, dict_v, Value::Qstr(attr)) {
                Ok(_) => {
                    dict_delete(ctx, dict_v, Value::Qstr(attr))
                        .map_err(|e| map_dict_err(e).to_pyerror())?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        }
        Some(v) => {
            if !has_special
                && with_stub_seams(|seams| {
                    attr_value_is_special_accessor(ctx, attr, v, seams)
                })
            {
                if let Obj::Type(d) = ctx.arena.get_mut(r) {
                    d.flags |= crate::objects::obj::FLAG_HAS_SPECIAL_ACCESSORS;
                }
            }
            dict_store(ctx, dict_v, Value::Qstr(attr), v)
                .map_err(|e| map_dict_err(e).to_pyerror())?;
            Ok(true)
        }
    }
}

// ── mp_obj_new_type ──────────────────────────────────────────────────

/// `mp_obj_new_type` (`objtype.zig:2015`, `newTypeImpl :1840-2010`):
/// create a user type from a name qstr, a bases tuple and a locals
/// dict. The validation order is the Zig's: bases must be exactly a
/// tuple and locals a dict-or-ordered-dict (bare `TypeError` for
/// either — probed), then the accessor scan, then each base must be
/// a type (bare `TypeError`) with a `make_new` slot
/// (`NotAcceptableBase`). The type is allocated with the 9 instance
/// slots by offset (`MAKE_NEW`/`PRINT`/`CALL`/`UNARY_OP`/
/// `BINARY_OP`/`ATTR`/`SUBSCR`/`ITER`/`BUFFER`) plus the parent
/// slot (the bases tuple itself for MI, the single base, or none),
/// then `new_type_finish` runs the MRO store, the layout check, the
/// `__new__` wrap and the `__set_name__` calls.
pub fn mp_obj_new_type(
    ctx: &mut CallCtx,
    name: u32,
    bases: Value,
    locals: Value,
    seams: &mut VmSeams,
) -> Result<Value, ObjTypeError> {
    if !is_exact_type(ctx.arena, ctx.types.tuple, bases) {
        return Err(ObjTypeError::BareTypeError);
    }
    let Value::Heap(bases_r) = bases else {
        return Err(ObjTypeError::BareTypeError);
    };
    let base_items: Vec<Value> = match ctx.arena.get(bases_r) {
        Obj::Tuple(items) => items.clone(),
        _ => return Err(ObjTypeError::BareTypeError),
    };
    let locals_type = obj_get_type(ctx.arena, ctx.types, locals);
    let locals_ok = match locals_type {
        Some(t) => t == ctx.types.dict || is_ordered_dict_type(ctx.arena, t),
        None => false,
    };
    if !locals_ok {
        return Err(ObjTypeError::BareTypeError);
    }
    let Value::Heap(locals_r) = locals else {
        return Err(ObjTypeError::BareTypeError);
    };
    let accessor_flag = scan_special_accessors_flag(ctx, locals, seams);
    let mut inherited_flag: u16 = 0;
    let mut base_refs: Vec<GcRef> = Vec::with_capacity(base_items.len());
    for item in &base_items {
        let Value::Heap(bt) = item else {
            return Err(ObjTypeError::BareTypeError);
        };
        if !is_type_value(ctx.arena, *item) {
            return Err(ObjTypeError::BareTypeError);
        }
        let Some(bd) = type_data(ctx.arena, *bt) else {
            return Err(ObjTypeError::BareTypeError);
        };
        if bd.make_new.is_none() {
            return Err(ObjTypeError::NotAcceptableBase);
        }
        if bd.flags & crate::objects::obj::FLAG_HAS_SPECIAL_ACCESSORS != 0 {
            inherited_flag |= crate::objects::obj::FLAG_HAS_SPECIAL_ACCESSORS;
        }
        base_refs.push(*bt);
    }
    let (base, bases_field) = match base_refs.len() {
        0 => (None, None),
        1 => (Some(base_refs[0]), None),
        _ => (None, Some(bases_r)),
    };
    let data = TypeData {
        name,
        flags: TYPE_BASE_FLAGS | accessor_flag | inherited_flag,
        base,
        bases: bases_field,
        mro: Vec::new(),
        dict: locals_r,
        namedtuple_fields: None,
        attr: Some(instance_attr),
        make_new: Some(instance_make_new),
        unary_op: Some(instance_unary_op_slot),
        binary_op: Some(instance_binary_op_slot),
        call: Some(instance_call_slot),
        getiter: Some(instance_getiter_slot),
        subscr: Some(instance_subscr_slot),
        buffer: Some(instance_get_buffer_slot),
        print: Some(instance_print_slot),
    };
    let type_ref = ctx.alloc(Some(ctx.types.type_), Obj::Type(data));
    // Root the new type across the fallible steps (MRO allocation,
    // `__set_name__` calls may allocate); unroot before returning.
    ctx.roots.push(Value::Heap(type_ref));
    let result = new_type_finish(ctx, type_ref, &base_refs, locals, seams);
    if let Some(pos) = ctx
        .roots
        .extra
        .iter()
        .rposition(|v| *v == Value::Heap(type_ref))
    {
        ctx.roots.extra.remove(pos);
    }
    result.map(|()| Value::Heap(type_ref))
}

/// The post-allocation half of `mp_obj_new_type`: the MRO store
/// (only when `bases_len >= 2` or the single base has a cached
/// MRO), the layout-conflict check (AFTER the store), the `__new__`
/// wrap and the `__set_name__` calls.
fn new_type_finish(
    ctx: &mut CallCtx,
    type_ref: GcRef,
    base_refs: &[GcRef],
    locals: Value,
    seams: &mut VmSeams,
) -> Result<(), ObjTypeError> {
    let type_v = Value::Heap(type_ref);
    let store_mro = base_refs.len() >= 2
        || (base_refs.len() == 1 && cached_mro(ctx, base_refs[0]).is_some());
    if store_mro {
        let mro_v = c3_mro(ctx, type_v, base_refs)?;
        let mro_q = dunder(b"__mro__").expect("__mro__ is a static qstr");
        dict_store(ctx, locals, Value::Qstr(mro_q), mro_v).map_err(map_dict_err)?;
        let items = values_to_refs(&tuple_items(ctx.arena, mro_v));
        if let Obj::Type(d) = ctx.arena.get_mut(type_ref) {
            d.mro = items;
        }
    } else {
        let items = values_to_refs(&mro_of(ctx, type_ref));
        if let Obj::Type(d) = ctx.arena.get_mut(type_ref) {
            d.mro = items;
        }
    }
    // Layout conflict AFTER the MRO store (objtype.zig:1988-1992).
    let (native_count, _) = count_native_bases(ctx, type_ref);
    if native_count > 1 {
        return Err(ObjTypeError::LayoutConflict);
    }
    wrap_new(ctx, locals)?;
    call_set_name_methods(ctx, type_ref, locals, seams)?;
    Ok(())
}

/// The `__mro__` tuple's items as type refs (a tuple item that is
/// not a heap type ref is dropped — it cannot happen for a
/// well-formed MRO, which `c3_mro` / `mro_of` build from types).
fn values_to_refs(items: &[Value]) -> Vec<GcRef> {
    items
        .iter()
        .filter_map(|v| match v {
            Value::Heap(r) => Some(*r),
            _ => None,
        })
        .collect()
}

/// The items of a tuple value (empty for a non-tuple).
fn tuple_items(arena: &crate::memory::gc::GcArena, v: Value) -> Vec<Value> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Tuple(items) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The `__new__` wrap (`objtype.zig:1944-1952`): when the locals
/// dict defines `__new__` AND `is_fun` holds for it, the entry is
/// replaced by a `StaticClassMethod` with the staticmethod type
/// (so `__new__` is looked up unbound). Full config only.
fn wrap_new(ctx: &mut CallCtx, locals: Value) -> Result<(), ObjTypeError> {
    let new_q = dunder(b"__new__").expect("__new__ is a static qstr");
    let new_v = match dict_get_value(ctx, locals, Value::Qstr(new_q)) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    if !is_fun(ctx, new_v) {
        return Ok(());
    }
    let wrapped = Value::Heap(ctx.alloc(
        Some(ctx.types.staticmethod),
        Obj::StaticClassMethod(StaticClassMethodData { fun: new_v }),
    ));
    dict_store(ctx, locals, Value::Qstr(new_q), wrapped).map_err(map_dict_err)?;
    Ok(())
}

/// The `__set_name__` collector (`objtype.zig:1934-1968`):
/// snapshot-then-call. First walk the locals entries and capture
/// `(fun, bound, key)` for every value whose `__set_name__` lookup
/// is BOUND (the Zig captures only when `dest[1] != 0`); then call
/// each captured method with `(owner, key)`. A raise propagates as
/// `ObjTypeError::Raised` (the C6 convention — the exception object
/// travels via `pending_exc`). The snapshot matters: a
/// `__set_name__` body may mutate sibling locals entries, and the
/// Zig still calls the SNAPSHOT, not the mutated dict.
fn call_set_name_methods(
    ctx: &mut CallCtx,
    type_ref: GcRef,
    locals: Value,
    seams: &mut VmSeams,
) -> Result<(), ObjTypeError> {
    let Some(set_name_q) = dunder(b"__set_name__") else {
        return Ok(());
    };
    let entries: Vec<(Value, Value)> = match locals {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Dict(d) => d
                .map
                .entries
                .iter()
                .filter_map(|e| e.key.map(|k| (k, e.value)))
                .collect(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let owner = Value::Heap(type_ref);
    let mut captured: Vec<(Value, Option<Value>, Value)> = Vec::new();
    for (key, value) in entries {
        if let Some((fun, bound)) = seams.load_maybe(ctx, value, set_name_q) {
            if bound.is_some() {
                captured.push((fun, bound, key));
            }
        }
    }
    for (fun, bound, key) in captured {
        seams
            .call_pair(ctx, fun, bound, 2, 0, &[owner, key])
            .map_err(|e| match e {
                PyError::OutOfMemory => ObjTypeError::OutOfMemory,
                PyError::PyRaise => ObjTypeError::Raised,
            })?;
    }
    Ok(())
}

