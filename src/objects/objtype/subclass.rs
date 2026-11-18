//! Subclass / isinstance tests (`objtype.zig:409-594`).
//!
//! * `mp_obj_is_subclass_fast` (:409) — the parent-chain walk.
//! * `mp_obj_cast_to_native_base` (:439) — native-base unwrapping
//!   (returns `None` for the Zig's NULL).
//! * `isSubclassImpl` (:524) / `mp_obj_is_subclass` (:551) /
//!   `mp_builtin_issubclass` (:555) / `mp_builtin_isinstance`
//!   (:569) — the public tests, plus the `issubclass` /
//!   `isinstance` `FunBuiltin` objects (:577/:581).
//!
//! The union (`__origin__` / `__args__`) branch of
//! `isSubclassImpl` needs `mp_load_method_maybe`, so `is_subclass`
//! takes the `VmSeams` — the common paths (type / tuple
//! classinfo) never touch it. The `BuiltinFn` adapters use the
//! default seams (`call_via_type_slot` + no `load_maybe`): a union
//! classinfo without a real `load_maybe` falls through to
//! `IssubclassArg2`, until C14 wires the real one (documented
//! limitation, not a divergence — no UnionType object exists in
//! the tree yet; the `int | None` object is C14/C15's).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::callables::objfun::fun_builtin_2_type_data;
use crate::objects::obj::{obj_get_type, type_data, BuiltinTypes};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::BuiltinFn;
use crate::objects::value::{FunBuiltinData, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};

use super::errors::ObjTypeError;
use super::seams::{with_stub_seams, VmSeams};

/// `mp_obj_is_type` (`obj.zig`, inlined here): the value is a heap
/// object whose header type is exactly `type_`.
pub fn is_exact_type(arena: &GcArena, type_: GcRef, value: Value) -> bool {
    match value {
        Value::Heap(r) => arena.header(r).type_ == Some(type_),
        _ => false,
    }
}

/// Is `value` a type object? A PAYLOAD check (`Obj::Type`), not a
/// header check: the fixture builds types with header `None`
/// (bootstrap types — the ROM registry gives them real headers),
/// so `is_exact_type(arena, types.type_, v)` is false for every
/// fixture type. The payload is the ground truth in both cases —
/// only type objects carry an `Obj::Type` payload — and this is
/// the same check `is_subclass` already uses for its classinfo
/// (subclass.rs:176). The Zig's `isTypeType` is the header
/// spelling of the same question; the two agree once headers exist.
pub fn is_type_value(arena: &GcArena, value: Value) -> bool {
    matches!(value, Value::Heap(r) if matches!(arena.get(r), Obj::Type(_)))
}

/// `mp_obj_is_subclass_fast` (`objtype.zig:409-437`): is `object`
/// a subclass of `classinfo`? Both must be type objects. Walks the
/// parent chain — `base` for a single parent, `bases` (recursing
/// into every base but the last, tail-calling into the last) for
/// multiple inheritance. Faithful to the Zig: a type with
/// `base == None` and `bases == None` (a ROM builtin like
/// `int`/`bool`, whose parent is only in `mro`) matches only
/// itself here.
///
/// Fixture-built types carry their parent in `mro` as well, so a
/// well-formed type's `mro` is a subset of its parent-chain
/// closure: the `mro` membership fallback below can only fire
/// for hand-built types (it cannot diverge for real types).
pub fn is_subclass_fast(arena: &GcArena, object: Value, classinfo: Value) -> bool {
    let Value::Heap(mut cur) = object else {
        return false;
    };
    let Value::Heap(target) = classinfo else {
        return false;
    };
    loop {
        if cur == target {
            return true;
        }
        let Some(t) = type_data(arena, cur) else {
            return false;
        };
        match t.bases {
            // Multiple parents: recurse into every base but the
            // last, tail-call into the last.
            Some(bases) => {
                let Obj::Tuple(items) = arena.get(bases) else {
                    return false;
                };
                let mut found = false;
                for base in items.iter().take(items.len().saturating_sub(1)) {
                    if is_subclass_fast(arena, *base, classinfo) {
                        found = true;
                        break;
                    }
                }
                if found {
                    return true;
                }
                match items.last() {
                    Some(Value::Heap(last)) => cur = *last,
                    _ => return false,
                }
            }
            None => match t.base {
                Some(parent) => cur = parent,
                // No parent recorded: fall back to the MRO (the
                // Zig's `mroOf` appends `object` for a parentless
                // type, so `int`'s MRO contains `object` even
                // though its parent chain does not).
                None => return t.mro.contains(&target),
            },
        }
    }
}

/// `mp_obj_cast_to_native_base` (`objtype.zig:439-449`): when
/// `self_in`'s type IS the native type, return `self_in`; when
/// its type is a subclass of the native type, return
/// `subobj[0]` (the initialised native-base object); else `None`
/// (the Zig's NULL). The Zig reads `subobj[0]` unconditionally
/// in the subclass case — a subobj-less instance is UB there;
/// Rust returns `None` instead (documented divergence).
pub fn cast_to_native_base(
    arena: &GcArena,
    types: &BuiltinTypes,
    self_in: Value,
    native_type: Value,
) -> Option<Value> {
    let self_type = obj_get_type(arena, types, self_in)?;
    if Value::Heap(self_type) == native_type {
        return Some(self_in);
    }
    if !is_subclass_fast(arena, Value::Heap(self_type), native_type) {
        return None;
    }
    instance_subobj0(arena, self_in)
}

/// `instancePtrSubobj0` (`objtype.zig:366-369`): the instance's
/// native-base object. `None` when the instance has no subobj —
/// the Zig reads `subobj[0]` unconditionally there (UB for a
/// subobj-less instance); callers fall back to the instance
/// itself or to `None`.
pub fn instance_subobj0(arena: &GcArena, inst: Value) -> Option<Value> {
    let Value::Heap(r) = inst else {
        return None;
    };
    let Obj::Instance(data) = arena.get(r) else {
        return None;
    };
    data.subobj.first().copied()
}

/// `opmethods.nativeBase` (`opmethods.zig:27-40`): the native
/// value behind a subclass instance — `subobj[0]` when the
/// instance's type is a Python subclass of a native type, the
/// value itself otherwise (the Zig's `instance_count_native_bases`
/// guard: a plain user instance has no native base). F3: the one
/// shared spelling for the `%` / `str.format` / `str.join` /
/// `nativeSubclassOp` unwrap — the old call sites each carried
/// their own "identity until instances exist (C12)" comment.
pub fn native_base(arena: &GcArena, v: Value) -> Value {
    instance_subobj0(arena, v).unwrap_or(v)
}

/// The type object behind a `Value` that must be a type
/// (`None` = not a type object).
fn type_ref(arena: &GcArena, value: Value) -> Option<GcRef> {
    match value {
        Value::Heap(r) if matches!(arena.get(r), Obj::Type(_)) => Some(r),
        _ => None,
    }
}

/// `unionArgs` (`objtype.zig:518-522`): the `__args__` tuple of a
/// union-like classinfo, or `None`. A classinfo WITH `__origin__`
/// (a generic alias, `list[int]`) is NOT a union — `None`.
/// `__origin__` / `__args__` are probed through the `load_maybe`
/// seam (C14's `mp_load_method_maybe`).
fn union_args(ctx: &mut CallCtx, classinfo: Value, seams: &mut VmSeams) -> Option<Value> {
    // `mp_load_method_maybe(classinfo, MP_QSTR___origin__)`.
    if seams.load_maybe(ctx, classinfo, origin_qstr()).is_some() {
        return None;
    }
    // `mp_load_method_maybe(classinfo, MP_QSTR___args__)`.
    match seams.load_maybe(ctx, classinfo, args_qstr()) {
        Some((args, _)) => Some(args),
        None => None,
    }
}

/// `isSubclassImpl` (`objtype.zig:524-549`): `object` is a TYPE
/// object; `classinfo` is a type, a tuple of types, or a union
/// (via `__args__`). `Err(Arg2)` when the classinfo is none of
/// those.
pub fn is_subclass(
    ctx: &mut CallCtx,
    object: Value,
    classinfo: Value,
    seams: &mut VmSeams,
) -> Result<Value, ObjTypeError> {
    let items: Vec<Value> = match classinfo {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Type(_)) => {
            vec![classinfo]
        }
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Tuple(_)) => {
            match ctx.arena.get(r) {
                Obj::Tuple(items) => items.clone(),
                _ => unreachable!(),
            }
        }
        _ => match union_args(ctx, classinfo, seams) {
            Some(Value::Heap(r)) if matches!(ctx.arena.get(r), Obj::Tuple(_)) => {
                match ctx.arena.get(r) {
                    Obj::Tuple(items) => items.clone(),
                    _ => unreachable!(),
                }
            }
            _ => return Err(ObjTypeError::IssubclassArg2),
        },
    };
    // `object == items[i] || mp_obj_is_subclass_fast(object,
    // items[i])` — the Zig's `object == classinfo` short-circuit
    // (line 537) is subsumed by the fast walk's first step, but
    // keep it explicit: it also covers non-type `object` values
    // (a plain object is never a subclass of anything, so the
    // walk returns false and the equality is the only match).
    for item in items {
        if object == item {
            return Ok(Value::Bool(true));
        }
        if is_subclass_fast(ctx.arena, object, item) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// `mp_obj_is_subclass` (`objtype.zig:551-553`): `object` is any
/// value; the test is on its TYPE.
pub fn is_subclass_of_value(
    ctx: &mut CallCtx,
    object: Value,
    classinfo: Value,
    seams: &mut VmSeams,
) -> Result<Value, ObjTypeError> {
    let Some(type_) = obj_get_type(ctx.arena, ctx.types, object) else {
        return Err(ObjTypeError::BareTypeError);
    };
    is_subclass(ctx, Value::Heap(type_), classinfo, seams)
}

/// `mp_builtin_issubclass` (`objtype.zig:555-562`) as a
/// `BuiltinFn` (fixed arity 2 — the `fun_builtin_2_call` adapter
/// checks the count first).
pub fn issubclass_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [object, classinfo] = args else {
        return Err(PyError::PyRaise);
    };
    if type_ref(ctx.arena, *object).is_none() {
        return Err(ObjTypeError::IssubclassArg1.to_pyerror());
    }
    with_stub_seams(|seams| is_subclass(ctx, *object, *classinfo, seams).map_err(|e| e.to_pyerror()))
}

/// `mp_builtin_isinstance` (`objtype.zig:569-575`) as a
/// `BuiltinFn` (fixed arity 2).
pub fn isinstance_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [object, classinfo] = args else {
        return Err(PyError::PyRaise);
    };
    with_stub_seams(|seams| {
        is_subclass_of_value(ctx, *object, *classinfo, seams).map_err(|e| e.to_pyerror())
    })
}

/// The `issubclass` `FunBuiltin` object (`objtype.zig:577-580`):
/// `mp_obj_new_fun_builtin_fixed(2, 2, issubclass, MP_QSTR_issubclass)`.
pub fn issubclass_fun_obj(ctx: &mut CallCtx, dict: GcRef) -> Value {
    let type_ = ctx.alloc(None, Obj::Type(fun_builtin_2_type_data(dict)));
    let data = FunBuiltinData {
        name: qstr_id(b"issubclass"),
        n_args: fun_n_args(2, 2),
        func: issubclass_builtin as BuiltinFn,
        kw_func: None,
    };
    Value::Heap(ctx.alloc(Some(type_), Obj::FunBuiltin(data)))
}

/// The `isinstance` `FunBuiltin` object (`objtype.zig:581-584`).
pub fn isinstance_fun_obj(ctx: &mut CallCtx, dict: GcRef) -> Value {
    let type_ = ctx.alloc(None, Obj::Type(fun_builtin_2_type_data(dict)));
    let data = FunBuiltinData {
        name: qstr_id(b"isinstance"),
        n_args: fun_n_args(2, 2),
        func: isinstance_builtin as BuiltinFn,
        kw_func: None,
    };
    Value::Heap(ctx.alloc(Some(type_), Obj::FunBuiltin(data)))
}

/// `MP_QSTR___origin__` / `MP_QSTR___args__` — resolved through
/// the static qstr table (both are static qstrs).
fn origin_qstr() -> u32 {
    qstr_id(b"__origin__")
}

fn args_qstr() -> u32 {
    qstr_id(b"__args__")
}

/// Static qstr lookup (the C7 `static_qstr_id` helper).
fn qstr_id(name: &[u8]) -> u32 {
    crate::objects::primitives::ops::static_qstr_id(name)
        .expect("objtype dunder qstr is in the static table")
}
