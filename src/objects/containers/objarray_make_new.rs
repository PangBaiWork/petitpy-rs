//! `objarray.zig` make-new constructors — `array` /
//! `bytearray` / `memoryview` (split out of `objarray.rs` by
//! Lane E2e: that file was 793 lines, at the 800 cap, and this
//! lane edits `bytearray_make_new`).

use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::{
    array_item_size, buffer_bytes, ARRAY_TYPECODE_FLAG_RW, BYTEARRAY_TYPECODE,
};
use crate::objects::containers::objarray::{
    array_construct, array_new, is_int_value, is_str_value, ArrayError,
};
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::CallHook;
use crate::objects::strings::objstr_a::str_data_pool;
use crate::objects::strings::objstr_methods_c::{kw_make_new, KwArg};
use crate::objects::value::{ArrayData, GcRef, Obj, Value};

/// `arrayMakeNewImpl` (`objarray.zig:271-281`): sig (1, 2,
/// false), polled. The typecode is the FIRST character of
/// the str argument (`array('ii')` is `array('i')`); an
/// empty str reads the NUL terminator → typecode 0 → "bad
/// typecode". A non-str first argument raises "can't convert
/// to str implicitly" (probed: `array(1)`).
pub fn array_make_new(
    ctx: &mut CallCtx,
    array_type: GcRef,
    bytearray_type: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, ArrayError> {
    let sig_err = if n_kw > 0 || args.is_empty() || args.len() > 2 {
        Some(ArrayError::ArgNumTypesMismatch)
    } else {
        None
    };
    let tc_str = match args.first() {
        // D4: the pool-aware `str_data_pool`, not the
        // arena-only `str_data` — the typecode is usually a
        // string literal (`'i'`), which is a DYNAMIC qstr
        // (not in the static table), so `str_data` returned
        // `None` and `array('i', ...)` raised "can't convert
        // to str implicitly" (the C13b `str_equal` fix class).
        Some(&a) => match str_data_pool(ctx.arena, ctx.pool, a) {
            Some(b) => b,
            None => return Err(ArrayError::NotStr),
        },
        None => Vec::new(),
    };
    let tc = tc_str.first().copied().unwrap_or(0);
    if args.len() == 1 {
        if let Some(e) = sig_err {
            return Err(e);
        }
        let r = array_new(ctx, array_type, bytearray_type, tc, 0)?;
        return Ok(Value::Heap(r));
    }
    let out = array_construct(ctx, array_type, bytearray_type, tc, args[1], call);
    if let Some(e) = sig_err {
        return Err(e);
    }
    out
}

/// `bytearrayMakeNewImpl` (`objarray.zig:286-303`) + the
/// full-config kw path (`npy_bytearray_make_new`, with
/// `kwMakeNew` over `(source, encoding, errors)` and
/// `codec_ctor_defaults = [None, qstr("utf-8"), None]` —
/// C8b has not landed, so the defaults are spelled out
/// here from `objstr.zig:1869`).
/// - 0 args → empty bytearray.
/// - int first arg (NOT bool — `mp_obj_is_int(True)` is
///   false) → zero-filled of that length; a negative length
///   raises `ValueError("negative count")` (the reference
///   ABORTS on `@intCast`); a heap int goes through
///   `obj_get_int` → `OverflowError` for 2**70 (probed).
/// - str first arg with exactly 1 arg → `TypeError("wrong
///   number of arguments")` (probed: `bytearray("ab")`).
/// - otherwise `arrayConstruct(BYTEARRAY_TYPECODE, args[0])`
///   (an extra positional is ignored — probed:
///   `bytearray(3, 4)` is 3 zero bytes).
pub fn bytearray_make_new(
    ctx: &mut CallCtx,
    array_type: GcRef,
    bytearray_type: GcRef,
    args: &[Value],
    kwargs: &[KwArg],
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, ArrayError> {
    let owned: Vec<Value>;
    let args: &[Value] = if kwargs.is_empty() {
        args
    } else {
        let names = [
            static_qstr_id(b"source").expect("source is a static qstr"),
            static_qstr_id(b"encoding").expect("encoding is a static qstr"),
            static_qstr_id(b"errors").expect("errors is a static qstr"),
        ];
        let utf8 = static_qstr_id(b"utf-8").expect("utf-8 is a static qstr");
        let defaults: [Option<Value>; 3] = [None, Some(Value::Qstr(utf8)), None];
        owned = kw_make_new(args, kwargs, &names, &defaults)?;
        &owned
    };
    let sig_err = if args.len() > 3 {
        Some(ArrayError::ArgNumTypesMismatch)
    } else {
        None
    };
    if args.is_empty() {
        if let Some(e) = sig_err {
            return Err(e);
        }
        let r = array_new(ctx, array_type, bytearray_type, BYTEARRAY_TYPECODE, 0)?;
        return Ok(Value::Heap(r));
    }
    if is_int_value(ctx, args[0]) {
        let n = obj_get_int(ctx, ctx.types, args[0])?;
        if n < 0 {
            return Err(ArrayError::NegativeCount);
        }
        let r = array_new(
            ctx,
            array_type,
            bytearray_type,
            BYTEARRAY_TYPECODE,
            n as usize,
        )?;
        if let Some(e) = sig_err {
            return Err(e);
        }
        return Ok(Value::Heap(r));
    }
    // E2e: a str literal loads as a dynamic `Qstr`, which the
    // header-type `is_str_value` misses — `bytearray('1234')`
    // slipped through to `array_construct` and returned
    // `bytearray(b'1234')` instead of the vendor's
    // `TypeError("wrong number of arguments")` (probed).
    if (matches!(args[0], Value::Qstr(_)) || is_str_value(ctx, args[0])) && args.len() == 1 {
        return Err(ArrayError::WrongNumArgs);
    }
    let out = array_construct(ctx, array_type, bytearray_type, BYTEARRAY_TYPECODE, args[0], call);
    if let Some(e) = sig_err {
        return Err(e);
    }
    out
}

/// `npy_memoryview_make_new` (`objarray.zig:316-344`): sig
/// (1, 1, false), polled. The arg must have a buffer (else
/// the buffer-protocol `TypeError` — the reference ABORTS
/// here, see module docs); the view's typecode is the
/// buffer's, its length `bufinfo.len / sz`. A memoryview arg
/// flattens: the new view inherits the base items pointer
/// (`source`) and the `free` offset. A writable buffer sets
/// the `0x80` RW flag.
pub fn memoryview_make_new(
    ctx: &mut CallCtx,
    memoryview_type: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, ArrayError> {
    let sig_err = if n_kw > 0 || args.len() != 1 {
        Some(ArrayError::ArgNumTypesMismatch)
    } else {
        None
    };
    let arg = args.first().copied().unwrap_or(Value::None);
    let (bytes, tc) = match buffer_bytes(ctx, arg) {
        Some(b) => b,
        None => return Err(ArrayError::Obj(ObjError::BufferProtocol)),
    };
    let sz = array_item_size(tc)?;
    let len = bytes.len() / sz;
    // Flatten a view-of-a-view: the source is the BASE object
    // and `free` the combined element offset.
    let (source, free) = match arg {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Array(a) => match a.source {
                Some(base) => (Some(base), a.free),
                None => (Some(r), 0),
            },
            _ => (Some(r), 0),
        },
        _ => (None, 0),
    };
    // Writable iff the arg is an array-family object that is
    // not a read-only view (arrays and bytearrays are always
    // writable; bytes/str never are).
    let rw = match arg {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Array(a) => (a.typecode & ARRAY_TYPECODE_FLAG_RW) != 0 || a.source.is_none(),
            _ => false,
        },
        _ => false,
    };
    let r = ctx.alloc(
        Some(memoryview_type),
        Obj::Array(ArrayData {
            typecode: if rw { tc | ARRAY_TYPECODE_FLAG_RW } else { tc },
            free,
            len,
            data: Vec::new(),
            source,
        }),
    );
    if let Some(e) = sig_err {
        return Err(e);
    }
    Ok(Value::Heap(r))
}
