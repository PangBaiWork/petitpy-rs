//! Instance printing (`instance_print`, objtype.zig:1638-1687).
//!
//! Split out of `instance.rs` (the 800-line file cap). See that
//! module's docs for the fidelity notes that apply here too
//! (a raising `__str__`/`__repr__` falls through to the
//! fallback). The exception-subclass branch (objtype.zig:1659-1672)
//! is wired here since the C12a merge: a native exception as the
//! subobj prints the subclass name first, then the base exception
//! with `PRINT_EXC_SUBCLASS` (`exception_print_inner`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_type, type_data};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::value::{GcRef, Obj, Value};

use super::lookup::{class_lookup, LookupHit, SlotKind};
use super::objexcept::{exception_print_inner, is_exception_type_obj};
use super::seams::with_stub_seams;
use super::subclass::instance_subobj0;

/// A dunder name's static qstr id (`None` = not a static qstr).
fn dunder(name: &[u8]) -> Option<u32> {
    static_qstr_id(name)
}

/// The instance's type (`None` = not a heap instance).
fn instance_type_of(ctx: &CallCtx, self_in: Value) -> Option<GcRef> {
    obj_get_type(ctx.arena, ctx.types, self_in)
}

// ── print ───────────────────────────────────────────────────────────

/// `instance_print` (objtype.zig:1638-1687): look `__str__`
/// (kind 0) / `__repr__` (kind 1) up with SLOT_OFFSET_PRINT; if
/// NULL and kind == 0 fall back to `__repr__` with slot 0.
/// SENTINEL → the native base: a native exception subobj takes
/// the exception-subclass branch (objtype.zig:1659-1672 — the
/// subclass name first for non-STR kinds, then the base
/// exception with `PRINT_EXC_SUBCLASS`); otherwise the base
/// prints with the same kind. Member → call it with 0 args and
/// print the result as STR; else the `<%s object at %p>`
/// fallback (the address is the arena index here). A raising
/// `__str__`/`__repr__` falls through to the fallback (see the
/// module docs).
pub fn instance_print(
    ctx: &mut CallCtx,
    value: Value,
    kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let types = ctx.types;
    let Some(type_) = instance_type_of(ctx, value) else {
        return;
    };
    let mut hit = None;
    let mut use_repr = false;
    if kind == PrintKind::Str {
        if let Some(q) = dunder(b"__str__") {
            hit = lookup_print_method(ctx, value, type_, q, Some(SlotKind::Print));
        }
        if hit.is_none() {
            use_repr = true;
        }
    }
    if hit.is_none() && (kind != PrintKind::Str || use_repr) {
        if let Some(q) = dunder(b"__repr__") {
            // The `__repr__` fallback lookup uses slot 0 when it
            // follows a missed `__str__` (objtype.zig:1655-1665);
            // the direct `__repr__` lookup (kind != STR) uses
            // SLOT_OFFSET_PRINT.
            let slot = if use_repr { None } else { Some(SlotKind::Print) };
            hit = lookup_print_method(ctx, value, type_, q, slot);
        }
    }
    match hit {
        Some(LookupHit::Member(fun, _bound)) => {
            // The Zig calls the UNBOUND fun with self as its arg
            // (`mp_call_function_1(member[0], self_in)`) — the
            // bound pair's `self` is not used here.
            let ret = with_stub_seams(|seams| seams.call(ctx, fun, 1, 0, &[value]));
            match ret {
                Ok(v) => obj_print_helper(ctx, types, v, PrintKind::Str, sink),
                Err(_) => print_fallback(ctx, value, type_, sink),
            }
        }
        Some(LookupHit::NativeSlot(_)) => {
            match instance_subobj0(ctx.arena, value) {
                Some(sub0) => {
                    if is_native_exception_subobj(ctx, sub0) {
                        // The exception-subclass branch
                        // (objtype.zig:1659-1672): the subclass
                        // (instance) name first for non-STR kinds,
                        // then the base exception without its own
                        // name (`PRINT_EXC_SUBCLASS`).
                        if kind != PrintKind::Str {
                            let name = type_name(ctx, type_);
                            let _ = write!(sink, "{name}");
                        }
                        exception_print_inner(ctx, sub0, kind, true, sink);
                    } else {
                        obj_print_helper(ctx, types, sub0, kind, sink);
                    }
                }
                // E2b: no subobj — the value itself may be a
                // user-exception instance built by the
                // `call_type_ctx` shortcut (vm_call.rs): a bare
                // `Obj::Exception` whose header type is the user
                // class (the Zig builds `Obj::Instance` +
                // exception subobj instead). Print it through
                // the same exception-subclass branch, with the
                // value itself as the exception payload.
                None => {
                    if is_shortcut_exception(ctx, value) {
                        if kind != PrintKind::Str {
                            let name = type_name(ctx, type_);
                            let _ = write!(sink, "{name}");
                        }
                        exception_print_inner(ctx, value, kind, true, sink);
                    } else {
                        print_fallback(ctx, value, type_, sink);
                    }
                }
            }
        }
        None => print_fallback(ctx, value, type_, sink),
    }
}

/// Whether `sub0` is a native exception instance: an
/// `Obj::Exception` payload whose header type is an exception
/// type (make_new-slot identity — the Zig's
/// `mp_obj_is_native_exception_instance`, objexcept.zig:100,
/// i.e. `typeMakeNewIsExc(mp_obj_get_type(subobj0))`).
fn is_native_exception_subobj(ctx: &CallCtx, sub0: Value) -> bool {
    let Value::Heap(r) = sub0 else {
        return false;
    };
    if !matches!(ctx.arena.get(r), Obj::Exception(_)) {
        return false;
    }
    match obj_get_type(ctx.arena, ctx.types, sub0) {
        Some(t) => is_exception_type_obj(ctx.arena, t),
        None => false,
    }
}

/// Whether `value` is a user-exception instance built by the
/// `call_type_ctx` shortcut (vm_call.rs): a bare
/// `Obj::Exception` whose header type is a user class whose MRO
/// contains a ROM exception type (a type whose `make_new` IS
/// `exception_make_new_slot`). ROM exception instances never
/// reach this printer (their print slot is `exception_print`),
/// so an `Obj::Exception` here is always a shortcut value — the
/// MRO check is the belt-and-braces guard that the header type
/// really is an exception subclass.
fn is_shortcut_exception(ctx: &CallCtx, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    if !matches!(ctx.arena.get(r), Obj::Exception(_)) {
        return false;
    }
    let Some(t) = obj_get_type(ctx.arena, ctx.types, value) else {
        return false;
    };
    let Some(td) = type_data(ctx.arena, t) else {
        return false;
    };
    td.mro.iter().any(|mr| is_exception_type_obj(ctx.arena, *mr))
}

/// A type's name as a `String` (empty when the type data is
/// missing — cannot happen for well-formed arenas).
fn type_name(ctx: &CallCtx, type_: GcRef) -> String {
    match type_data(ctx.arena, type_) {
        Some(t) => {
            let bytes = ctx.pool.qstr_str(t.name as u16);
            String::from_utf8_lossy(bytes).into_owned()
        }
        None => String::new(),
    }
}

/// The print-slot lookup for `instance_print` (stub seams —
///
/// the `__str__`/`__repr__` lookup only needs the class
/// dictionary; a descriptor `__get__` behind it is C13's).
fn lookup_print_method(
    ctx: &mut CallCtx,
    value: Value,
    type_: GcRef,
    name_q: u32,
    slot: Option<SlotKind>,
) -> Option<LookupHit> {
    with_stub_seams(|seams| class_lookup(ctx, Some(value), name_q, slot, false, type_, seams))
}

/// The `<%s object at %p>` fallback (objtype.zig:1680-1687).
/// The address is the arena index (the Zig prints a pointer).
fn print_fallback(ctx: &CallCtx, value: Value, type_: GcRef, sink: &mut dyn crate::objects::slots::ByteSink) {
    let name = match type_data(ctx.arena, type_) {
        Some(t) => {
            let bytes = ctx.pool.qstr_str(t.name as u16);
            String::from_utf8_lossy(bytes).into_owned()
        }
        None => String::new(),
    };
    let addr = match value {
        Value::Heap(r) => r.0,
        _ => 0,
    };
    let _ = write!(sink, "<{name} object at {addr:x}>");
}

/// `instance_print` as a `PrintFn` slot.
pub fn instance_print_slot(
    ctx: &mut CallCtx,
    value: Value,
    kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    instance_print(ctx, value, kind, sink)
}
