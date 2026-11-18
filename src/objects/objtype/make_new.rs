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
use crate::objects::containers::objarray::{
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
) -> PyResult<Value> {
    list_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `tuple` (`objtuple.rs:258`): direct, error mapping only.
pub fn tuple_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    tuple_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `dict` (`objdict.rs:298`): direct, error mapping only.
pub fn dict_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    dict_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `set` — and `frozenset` with the same adapter
/// (`objset.rs:438`): direct, error mapping only.
pub fn set_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    set_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `range` (`objrange.rs:191`): direct, error mapping only.
pub fn range_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    range_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `slice` (`objslice.rs:335`): direct, error mapping only.
pub fn slice_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    slice_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `deque` (`objdeque.rs:115`): direct, error mapping only.
pub fn deque_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    deque_make_new(ctx, type_, args, n_kw).map_err(|e| ctx.record_typed(e))
}

/// `property` (`objproperty.rs:143`): direct, error
/// mapping only.
pub fn property_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
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
) -> PyResult<Value> {
    let v = filter_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `zip` (`objzip.rs:25`): header fix-up as `map`.
pub fn zip_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
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
) -> PyResult<Value> {
    let v = enumerate_make_new(ctx, args, n_kw).map_err(|e| ctx.record_typed(e))?;
    if let Value::Heap(r) = v {
        ctx.arena.set_header_type(r, Some(type_));
    }
    Ok(v)
}

/// `int` (`objint.rs:223`): builds the `QstrResolver` from
/// `ctx.pool` (see the module doc), error mapping only.
pub fn int_make_new_slot(
    ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
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
) -> PyResult<Value> {
    let types = ctx.types;
    let (pos, kwargs) = split_kwargs(ctx, args, n_kw)?;
    bytes_make_new(ctx, types, pos, &kwargs).map_err(|e| ctx.record_typed(e))
}

/// `array` (`objarray.rs:389`): the sibling bytearray type
/// comes from `ctx.types` (see the module doc).
pub fn array_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    let bytearray_type = ctx.types.bytearray;
    array_make_new(ctx, type_, bytearray_type, args, n_kw)
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
) -> PyResult<Value> {
    let array_type = ctx.types.array;
    let (pos, kwargs) = split_kwargs(ctx, args, n_kw)?;
    bytearray_make_new(ctx, array_type, type_, pos, &kwargs)
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
) -> PyResult<Value> {
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    namedtuple_make_new(ctx, type_, args, n_kw, &mut intern)
        .map_err(|e| ctx.record_typed(e))
}
