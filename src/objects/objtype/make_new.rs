//! Builtin `make_new` adapters (Lane C13b).
//!
//! `TypeData.make_new` is a `MakeNewFn` (`slots.rs:233`):
//! `fn(&mut CallCtx, GcRef, &[Value], usize) -> PyResult<Value>`
//! — the type being constructed, the flat `(args, n_kw)`
//! layout (`type_call`, `typecreate.rs:183`). Most builtin
//! `*_make_new` fns already take exactly that shape; the
//! adapters below only map the lane-local error type onto
//! `PyResult` via `ctx.record_typed` (the `TypedRaise`
//! contract — the VM reads the recorded name/message from
//! `pending_raise`; a bare `.map_err(E::to_pyerror)` would
//! drop them, the `objarray_methods.rs:245` precedent).
//!
//! The true adapters reshape their inputs:
//! - `int`: builds the `QstrResolver` from `ctx.pool`
//!   (`qstr_str` panics on an out-of-range id, like the
//!   Zig's `@panic("qstr_str: out-of-range qstr")` — the
//!   resolver returns `None` instead, which `str_bytes_of`
//!   treats as "not a string").
//! - `str` / `bytes` / `bytearray`: split the flat layout
//!   into the positional prefix and the `**` keyword pairs
//!   (`dict_make_new`'s `n_pos = args.len() - n_kw * 2` is
//!   the same layout).
//! - `array` / `bytearray`: take the sibling type from
//!   `ctx.types` (`array_new` picks the bytearray header
//!   when the typecode is `BYTEARRAY_TYPECODE` (1) —
//!   `array('\x01')` is a `bytearray` on the reference
//!   binary, probed 2026-11-18).
//! - `map` / `filter` / `zip` / `enumerate`: drop `type_`
//!   (their `*_make_new` fns do not take it — the iterator
//!   header is `None`, dispatch is by the `Obj::Iterator`
//!   payload tag, C11).
//! - `set`: one adapter serves both `set` and `frozenset`
//!   (`set_make_new` passes `type_` through to `alloc_set`,
//!   like the Zig's `npy_set_make_new`, `objset.zig:133-151`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::ObjError;
use crate::objects::objtype::instance::instance_unary_op;
use crate::objects::objtype::seams::with_call_hook_seams;
use crate::objects::slots::{CallHook, UnaryOp};
use crate::objects::value::Obj;
use crate::objects::containers::objarray_make_new::{
    array_make_new, bytearray_make_new, memoryview_make_new,
};
use crate::objects::containers::objdict::dict_make_new;
use crate::objects::containers::objlist::list_make_new;
use crate::objects::containers::objproperty::property_make_new;
use crate::objects::containers::objrange::range_make_new;
use crate::objects::containers::objset::set_make_new;
use crate::objects::containers::objslice::slice_make_new;
use crate::objects::containers::objstringio::{iobase_make_new, stringio_make_new};
use crate::objects::containers::objtuple::tuple_make_new;
use crate::objects::iters::objdeque::deque_make_new;
use crate::objects::iters::objenumerate::enumerate_make_new;
use crate::objects::iters::objfilter::filter_make_new;
use crate::objects::iters::objmap::map_make_new;
use crate::objects::iters::objnamedtuple::namedtuple_make_new;
use crate::objects::iters::objreversed::reversed_make_new;
use crate::objects::iters::objzip::zip_make_new;
use crate::objects::primitives::objbool::bool_make_new;
use crate::objects::primitives::objcomplex::complex_make_new;
use crate::objects::primitives::objfloat::float_make_new;
use crate::objects::primitives::objint::int_make_new;
use crate::objects::primitives::ops::QstrResolver;
use crate::objects::strings::objstr_methods_c::{bytes_make_new, str_make_new, KwArg};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

/// Split the flat `(args, n_kw)` layout into the positional
/// prefix and the `**` keyword pairs. The VM never produces
/// a malformed layout (`args.len() < 2 * n_kw`); a direct
/// caller that does gets the terse mismatch. A non-`Qstr`
/// keyword name is unreachable from the VM (`intern_kw_key`
/// in `vm_call.rs` interns `**` keys first, raising the
/// same message below when a `Str` was never interned) —
/// the adapter records that message defensively.
fn split_kwargs<'a>(
    ctx: &mut CallCtx,
    args: &'a [Value],
    n_kw: usize,
) -> Result<(&'a [Value], Vec<KwArg>), PyError> {
    let Some(n_pos) = args.len().checked_sub(2 * n_kw) else {
        // (`ObjTypeError` has no `TypedRaise` impl — the message
        // is `ObjTypeError::ArgNumTypesMismatch`'s, recorded
        // directly.)
        ctx.record_raise("TypeError", Some("argument num/types mismatch"));
        return Err(PyError::PyRaise);
    };
    let mut kwargs = Vec::with_capacity(n_kw);
    for pair in args[n_pos..].chunks_exact(2) {
        let Value::Qstr(name) = pair[0] else {
            ctx.record_raise(
                "TypeError",
                Some("keyword argument name is not an interned string"),
            );
            return Err(PyError::PyRaise);
        };
        kwargs.push(KwArg {
            name,
            value: pair[1],
        });
    }
    Ok((&args[..n_pos], kwargs))
}

/// `list` (`objlist.rs:267`): direct, error mapping only.
pub fn list_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    list_make_new(ctx, type_, args, n_kw, call).map_err(|e| ctx.record_typed(e))
}

/// `tuple` (`objtuple.rs:258`): direct, error mapping only.
pub fn tuple_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    tuple_make_new(ctx, type_, args, n_kw, call).map_err(|e| ctx.record_typed(e))
}

/// `dict` (`objdict.rs:298`): direct, error mapping only.
pub fn dict_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    dict_make_new(ctx, type_, args, n_kw, call).map_err(|e| ctx.record_typed(e))
}

/// `set` — and `frozenset` with the same adapter
/// (`objset.rs:438`): direct, error mapping only.
pub fn set_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    set_make_new(ctx, type_, args, n_kw, call).map_err(|e| ctx.record_typed(e))
}

/// `range` (`objrange.rs:191`): direct, error mapping only.
pub fn range_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    range_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `slice` (`objslice.rs:335`): direct, error mapping only.
pub fn slice_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    slice_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `deque` (`objdeque.rs:115`): direct, error mapping only.
pub fn deque_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    deque_make_new(ctx, type_, args, n_kw, call).map_err(|e| ctx.record_typed(e))
}

/// `property` (`objproperty.rs:143`): direct, error
/// mapping only.
pub fn property_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    property_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `memoryview` (`objarray.rs:508`): direct, error mapping
/// only.
pub fn memoryview_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    memoryview_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `map` (`objmap.rs:25`): the header type is the type being
/// constructed — C11's `new_iter` allocates with a `None` header
/// (no type exists in its lane), so `type(m)` / `obj_get_type`
/// returned `None` for a map object.
pub fn map_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let v = map_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `filter` (`objfilter.rs:25`): header fix-up as `map`.
pub fn filter_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let v = filter_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `reversed` (`objreversed.rs:17-51`): the `__reversed__`
/// delegation needs a call seam at construction time, which
/// the slot now carries from the VM hook (E2f — the C13b stub
/// seam is gone). The header stamp applies ONLY to the fresh
/// reversed iterator: the delegation returns the method's
/// result as-is (the Zig's `mp_obj_reversed_make_new` returns
/// `dest` untouched), and stamping ITS header would retype
/// the caller's object (probed: `print(reversed(B()))`
/// showed `<reversed>` for a delegated list — the list's own
/// header had been overwritten).
pub fn reversed_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    use crate::objects::iters::iter::{CallOutcome, IterSeams, REVERSED_ITER_TAG};
    use crate::objects::value::Obj;
    // E2f: on the VM path the hook runs the `__reversed__` /
    // `__len__` calls inside `reversed_make_new` (a FunBc
    // method cannot run through the Raise-always seams the
    // ctx-only path uses). The adapter maps a hook raise to
    // `Raise` — `reversed_make_new` treats `Stop` and `Raise`
    // the same (both `PyRaise`), so no Stop/ Raise split is
    // needed here.
    let mut hook_call;
    let mut raise_call =
        |_: &mut CallCtx, _: Value, _: &[Value]| -> CallOutcome { CallOutcome::Raise };
    let call_ref: &mut dyn FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome = match call {
        Some(hook) => {
            hook_call = |ctx: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
                match hook.call(ctx, fun, args.len(), 0, args) {
                    Ok(v) => CallOutcome::Value(v),
                    Err(_) => CallOutcome::Raise,
                }
            };
            &mut hook_call
        }
        None => &mut raise_call,
    };
    let mut getitem =
        |_: &mut CallCtx, _: Value, _: i64| -> CallOutcome { CallOutcome::Raise };
    let mut seams = IterSeams {
        call: call_ref,
        getitem: &mut getitem,
        // No `next` seam: the slot is ctx-only for iteration
        // (E2c — same as the `collect_iterable` fallback);
        // `reversed_iternext` takes its seams from the
        // `iternext` dispatch instead.
        next: None,
        next_builtin: None,
    };
    let v = reversed_make_new(ctx, args, n_kw, &mut seams).map_err(|e| ctx.record_typed(e))?;
    // Stamp the header ONLY on the fresh reversed iterator
    // (tag 114 in `extra[0]`): a delegated result keeps its
    // own header (see the doc comment above).
    if let Value::Heap(r) = v {
        if matches!(ctx.arena.get(r), Obj::Iterator(d)
            if d.extra.first() == Some(&Value::SmallInt(REVERSED_ITER_TAG)))
        {
            ctx.arena.set_header_type(r, Some(type_));
        }
    }
    Ok(v)
}

/// `zip` (`objzip.rs:25`): header fix-up as `map`.
pub fn zip_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let v = zip_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `enumerate` (`objenumerate.rs:32`): header fix-up as `map`.
pub fn enumerate_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let v = enumerate_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `int` (`objint.rs:223`): builds the `QstrResolver` from
/// `ctx.pool` (see the module doc), error mapping only.
/// (E2h: with a VM hook, a user-class instance runs its
/// `__int__` through `instance_unary_op` INT_MAYBE first — the
/// vendor's `mpObjIntMakeNewImpl` INT_MAYBE step (objint.zig:
/// 151-154). `Some(v)` is the int itself (`finish_unary` passes
/// an exact int through unchanged, so `1 << 100` survives); `None` (no
/// `__int__`) falls through to `int_make_new`, which raises
/// `IntConversion` — the same `TypeError("can't convert to
/// int")` the slot path raises for a non-int-convertible
/// instance. A raise from the hooked call, or a non-int return,
/// BOTH report `TypeError("can't convert to int")` (the vendor's
/// INT_MAYBE NULL rule — the stashed raise is discarded, see the
/// `Err` arm).)
pub fn int_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    if let Some(hook) = call {
        let n_pos = args.len().saturating_sub(2 * n_kw);
        if n_pos == 1 {
            let arg = args[0];
            if matches!(arg, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Instance(_))) {
                match with_call_hook_seams(&mut *hook, |seams| {
                    instance_unary_op(ctx, UnaryOp::IntMaybe, arg, seams)
                }) {
                    Ok(Some(v)) => return Ok(v),
                    Ok(None) => {}
                    Err(_) => {
                        // The vendor's INT_MAYBE NULL rule: whether
                        // the `__int__` call raised or returned a
                        // non-int, `int()` reports TypeError "can't
                        // convert to int" — the stashed raise is
                        // discarded (see `VmHook::take_pending`),
                        // and the failure is recorded on
                        // `ctx.pending_raise` (the `int_make_new`
                        // convention — the VM drains it into the
                        // exception object).
                        let _ = hook.take_pending();
                        return Err(ctx.record_typed(ObjError::IntConversion));
                    }
                }
            }
        }
    }
    let types = ctx.types;
    let pool = ctx.pool;
    let resolver: QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    int_make_new(ctx, types, resolver, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `float` (`objfloat.rs`): builds the `QstrResolver` from
/// `ctx.pool` (same shape as `int_make_new_slot`), error mapping
/// only.
pub fn float_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let types = ctx.types;
    let pool = ctx.pool;
    let resolver: QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    float_make_new(ctx, types, resolver, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `complex` (`objcomplex.rs`): same resolver shape as
/// `float_make_new_slot`, error mapping only.
pub fn complex_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let types = ctx.types;
    let pool = ctx.pool;
    let resolver: QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    complex_make_new(ctx, types, resolver, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `bool` (`objbool.rs:79`): `types` is unused by the
/// callee but part of its signature; error mapping only.
pub fn bool_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let types = ctx.types;
    bool_make_new(ctx, types, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `str` (`objstr_methods_c.rs:546`): splits the flat
/// layout into positionals + `KwArg`s, error mapping only.
pub fn str_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let types = ctx.types;
    let (pos, kwargs) = split_kwargs(ctx, args, n_kw)?;
    str_make_new(ctx, types, type_, pos, &kwargs).map_err(|e| ctx.record_typed(e))
}

/// `bytes` (`objstr_methods_c.rs:591`): splits the flat
/// layout into positionals + `KwArg`s, error mapping only.
/// (`bytes_make_new` takes no `type_` — the 0-arg case
/// returns the `types.empty_bytes` singleton.)
pub fn bytes_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let types = ctx.types;
    let (pos, kwargs) = split_kwargs(ctx, args, n_kw)?;
    bytes_make_new(ctx, types, pos, &kwargs, call).map_err(|e| ctx.record_typed(e))
}

/// `array` (`objarray.rs:389`): the sibling bytearray type
/// comes from `ctx.types` (see the module doc).
pub fn array_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let bytearray_type = ctx.types.bytearray;
    array_make_new(ctx, type_, bytearray_type, args, n_kw, call)
        .map_err(|e| ctx.record_typed(e))
}

/// `bytearray` (`objarray.rs:440`): the sibling array type
/// comes from `ctx.types`; splits the flat layout into
/// positionals + `KwArg`s.
pub fn bytearray_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let array_type = ctx.types.array;
    let (pos, kwargs) = split_kwargs(ctx, args, n_kw)?;
    bytearray_make_new(ctx, array_type, type_, pos, &kwargs, call)
        .map_err(|e| ctx.record_typed(e))
}

/// `StringIO` / `BytesIO` (`objstringio.rs`): direct, error
/// mapping only — one adapter serves both types (the C uses
/// the same `stringio_make_new` for both, `objstringio.c:241
/// / 252`).
pub fn stringio_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    stringio_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `IOBase` (`objstringio.rs`): direct, error mapping only
/// (the C returns the static singleton, `modio.c:46-52`;
/// the Rust `iobase_make_new` allocates fresh instead).
pub fn iobase_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    iobase_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `namedtuple` class (`objnamedtuple.zig:163-238`): the
/// `namedtuple_make_new` fn takes an `InternFn` (keyword
/// names arrive as qstrs from the VM, heap `Str`s from a
/// direct caller — the pool interns them, the C15b
/// `from_strn` path). D4 wires it: C11 built the class
/// with `make_new: None`, so `P(1, 2)` through the VM
/// raised `TypeError("can't create instance")` although the
/// Zig attaches `namedtuple_make_new` as the class's slot 1
/// (`objnamedtuple.zig:196,210`).
pub fn namedtuple_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
    _call: Option<CallHook<'_, '_>>,
) -> PyResult<Value> {
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    namedtuple_make_new(ctx, type_, args, n_kw, &mut intern)
        .map_err(|e| ctx.record_typed(e))
}
