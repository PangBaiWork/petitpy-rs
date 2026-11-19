//! `objarray.zig` operations — append/extend, the binary op,
//! subscr, the buffer slot, the iterator (the second half of
//! the array lane; `objarray.rs` holds construction, the
//! make_news, print and the type tables).
//!
//! Faithful points (see `objarray.rs` for the constructor
//! notes):
//! - `arrayAppend`: `free == 0` grows by 8 in the Zig; the
//!   `Vec` manages capacity here (unobservable). A failed
//!   `set_val_array` does NOT bump `len` (ADR 012 D4).
//! - `arrayExtend` with a buffer arg is a bulk memcpy (the
//!   arg bytes are CLONED first, so the self-extend case
//!   needs no re-fetch); otherwise the `getiter` loop.
//! - `ADD` reinterprets the rhs bytes at the lhs element
//!   size (`rhs_len = rhs_bufinfo.len / sz`); a memoryview
//!   lhs returns NULL. An empty + empty ADD aborts the
//!   reference (`res.items.?` on NULL) — Rust returns the
//!   empty array.
//! - `CONTAINS`: a buffer rhs searches only an exact
//!   bytearray lhs (`find_subbytes`, direction 1); an
//!   int/float rhs raises bare `NotImplementedError`; anything
//!   else is `False`.
//! - Comparisons: a non-buffer rhs is `False` (probed:
//!   `array('i', [1]) < 1` is `False`); same mapped typecode
//!   + not `f`/`d` + (`EQUAL` or unsigned) → `seq_cmp_bytes`,
//!   else bare `NotImplementedError`. The `is_unsigned` bool
//!   is shared across the two `typecodeForComparison` calls
//!   (write-only-true, so the final value is the OR).
//! - `!=` is NOT a slot op: the VM inverts `==`
//!   (`runtime.zig:556`) — the slot returns `PyRaise`.
//! - Slice store is `Vec::splice` on the destination byte
//!   range (the Zig's grow/no-grow memmove pairs); a
//!   memoryview lhs writes into its SOURCE's bytes at
//!   `(free + beg) * sz` and must not change length.
//! - The iterator derives its offset from the array bits
//!   (`free` for a view) on the fly — the Zig's fixed 40-byte
//!   overflow bug is documented in `objarray.zig`.

use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::{
    array_item_size, binary_set_val_bytes, buffer_bytes, ARRAY_TYPECODE_FLAG_RW,
    BYTEARRAY_TYPECODE, TYPECODE_MASK,
};
use crate::objects::containers::objarray::{
    array_data, array_get_element, array_typecode, typecode_for_comparison, ArrayError,
    MEMVIEW_OFFSET_MAX,
};
use crate::objects::containers::objdict_methods::collect_iterable;
use crate::objects::containers::sequence::ARRAY_ITER_TAG;
use crate::objects::obj::{ObjError, BUFFER_WRITE};
use crate::objects::obj_ops::get_index;
use crate::objects::slots::{BinaryOp, BufferInfo, CallHook, SubscrArg};
use crate::objects::strings::objstr_a::{fast_slice_indexes, find_subbytes};
use crate::objects::strings::objstr_methods_c::seq_cmp_bytes;
use crate::objects::value::{ArrayData, Obj, Value};
use crate::pyerr::{PyError, PyResult};

/// The `collect_iterable` error mapped onto the array
/// errors (only "object not iterable" is reachable here —
/// the rest collapse to `PyRaise`).
fn iter_error(e: crate::objects::containers::objdict::DictError) -> ArrayError {
    use crate::objects::containers::objdict::DictError;
    match e {
        DictError::TypeError(msg) if msg == "object not iterable" => {
            ArrayError::NotIterable
        }
        DictError::OutOfMemory => ArrayError::OutOfMemory,
        _ => ArrayError::PyRaise,
    }
}

/// Copy resolved element bytes into a destination slot
/// (the second half of the resolve-then-copy split — see
/// `binary_set_val_bytes`).
fn copy_element_bytes(dest: &mut [u8], elem_index: usize, size: usize, bytes: &[u8; 8]) {
    let off = elem_index * size;
    let end = (off + size).min(dest.len());
    if off < dest.len() {
        dest[off..end].copy_from_slice(&bytes[..end - off]);
    }
}

/// Set one element, resolving a view to its source
/// (`(free + index)` into the base bytes). Used by the
/// extend-impl and the subscr store.
fn array_set_element(
    ctx: &mut CallCtx,
    array: Value,
    index: usize,
    value: Value,
) -> Result<(), ArrayError> {
    let Value::Heap(r) = array else {
        return Err(ArrayError::PyRaise);
    };
    let (tc, dest_ref, elem_index) = match ctx.arena.get(r) {
        Obj::Array(a) => {
            let tc = a.typecode & TYPECODE_MASK;
            match a.source {
                Some(src) => (tc, src, a.free + index),
                None => (tc, r, index),
            }
        }
        _ => return Err(ArrayError::PyRaise),
    };
    // Resolve first (needs `&mut ctx`), copy second (needs
    // `&mut` destination) — the two borrows cannot overlap.
    let (size, bytes) = binary_set_val_bytes(ctx, tc, value).map_err(ArrayError::from)?;
    match ctx.arena.get_mut(dest_ref) {
        Obj::Array(d) => {
            copy_element_bytes(&mut d.data, elem_index, size, &bytes);
            Ok(())
        }
        _ => Err(ArrayError::PyRaise),
    }
}

/// `arrayExtendImpl` (`objarray.zig:172-186`): the `getiter`/
/// `iternext` loop. `len == 0` appends per item, else it
/// writes per index (the items come from
/// `collect_iterable` — the Array arm is C9b's, the
/// generator gap is C11's). A failure stops the fill; the
/// half-filled array is discarded by the caller, so
/// stopping at the first error is unobservable.
pub fn array_extend_impl(
    ctx: &mut CallCtx,
    array: Value,
    arg: Value,
    len: usize,
    call: Option<CallHook<'_, '_>>,
) -> Result<(), ArrayError> {
    let items = collect_iterable(ctx, arg, call).map_err(iter_error)?;
    if len == 0 {
        for it in items {
            array_append(ctx, array, it)?;
        }
    } else {
        for (i, it) in items.into_iter().enumerate() {
            if i >= len {
                break;
            }
            array_set_element(ctx, array, i, it)?;
        }
    }
    Ok(())
}

/// `arrayAppend` (`objarray.zig:443-471`): set element `len`,
/// bump `len` only on success (a failed store leaves the
/// length unchanged). Views are not appendable (the Zig
/// asserts `isArrayOrBytearray` — the method table never
/// wires append for memoryview).
pub fn array_append(ctx: &mut CallCtx, self_: Value, arg: Value) -> Result<Value, ArrayError> {
    let Value::Heap(r) = self_ else {
        return Err(ArrayError::PyRaise);
    };
    let (tc, len, is_view) = match ctx.arena.get(r) {
        Obj::Array(a) => (a.typecode & TYPECODE_MASK, a.len, a.source.is_some()),
        _ => return Err(ArrayError::PyRaise),
    };
    if is_view {
        return Err(ArrayError::PyRaise);
    }
    let sz = array_item_size(tc)?;
    // Resolve first (needs `&mut ctx`) — a failure leaves
    // the length unchanged (ADR 012 D4); only then grow the
    // buffer and copy the bytes in.
    let (size, bytes) = binary_set_val_bytes(ctx, tc, arg).map_err(ArrayError::from)?;
    debug_assert_eq!(size, sz);
    if let Obj::Array(a) = ctx.arena.get_mut(r) {
        a.data.resize((len + 1) * sz, 0);
        copy_element_bytes(&mut a.data, len, size, &bytes);
        a.len += 1;
    }
    Ok(Value::None)
}

/// `arrayExtend` (`objarray.zig:472-503`): a buffer arg is a
/// bulk memcpy (`len = bufinfo.len / sz`, reinterpreted at
/// the self element size — the arg bytes are cloned first,
/// so self-extend needs no re-fetch); otherwise the iter
/// path via `arrayExtendImpl`.
/// `arrayExtend` (`objarray.zig`): a buffer arg is a bulk
/// memcpy; anything else goes through `array_extend_impl`
/// (the `getiter` loop) with the caller's hook — `None` on
/// the ctx-only paths (the method wrapper, `+=`), `Some`
/// on the VM intercept path (`call_array_extend_ctx`), so a
/// generator arg is collected through the VM's real call
/// hook (E2i; the `str.join` precedent).
pub fn array_extend(
    ctx: &mut CallCtx,
    self_: Value,
    arg: Value,
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, ArrayError> {
    if let Some((bytes, _)) = buffer_bytes(ctx, arg) {
        let Value::Heap(r) = self_ else {
            return Err(ArrayError::PyRaise);
        };
        let (tc, len, is_view) = match ctx.arena.get(r) {
            Obj::Array(a) => (a.typecode & TYPECODE_MASK, a.len, a.source.is_some()),
            _ => return Err(ArrayError::PyRaise),
        };
        if is_view {
            return Err(ArrayError::PyRaise);
        }
        let sz = array_item_size(tc)?;
        let n = bytes.len() / sz;
        if let Obj::Array(a) = ctx.arena.get_mut(r) {
            a.data.resize((len + n) * sz, 0);
            a.data[len * sz..(len + n) * sz].copy_from_slice(&bytes[..n * sz]);
            a.len += n;
        }
        return Ok(Value::None);
    }
    array_extend_impl(ctx, self_, arg, 0, call)?;
    Ok(Value::None)
}

/// Is `value` an int (`SmallInt` or heap `Obj::Int` — bool
/// is NOT an int) or a float?
fn is_int_or_float(ctx: &CallCtx, value: Value) -> bool {
    match value {
        Value::SmallInt(_) => true,
        Value::Heap(r) => matches!(ctx.arena.get(r), Obj::Int(_) | Obj::Float(_)),
        _ => false,
    }
}

/// `arrayBinaryOpImpl` (`objarray.zig:358-442`): `ADD`,
/// `INPLACE_ADD`, `CONTAINS`, the five comparisons. `!=`
/// is not a slot op (the VM inverts `==`). The rich error
/// is dropped at the slot boundary (the accepted C6/C7r
/// convention); the `ADD` buffer-protocol failure is the
/// `TypeError("object with buffer protocol required")`
/// (probed: `array('i', [1, 2]) + 5`).
///
/// The result of `ADD` / slice-load keeps the lhs header
/// type (the Zig's `arrayNew(lhs typecode)` picks bytearray
/// iff the typecode is 1, else array — exactly the lhs
/// type), so the slot reads it off the lhs instead of
/// needing the type objects as parameters.
pub fn array_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let is_view = matches!(array_data(ctx, lhs), Some(a) if a.source.is_some());
    match op {
        BinaryOp::Add => {
            if is_view {
                return Err(PyError::PyRaise);
            }
            let Some((lhs_bytes, lhs_tc)) = buffer_bytes(ctx, lhs) else {
                return Err(PyError::PyRaise);
            };
            let Some((rhs_bytes, _)) = buffer_bytes(ctx, rhs) else {
                return Err(ctx.record_typed(ArrayError::Obj(ObjError::BufferProtocol)));
            };
            let sz = match array_item_size(lhs_tc) {
                Ok(s) => s,
                Err(e) => return Err(ctx.record_typed(ArrayError::from(e))),
            };
            let rhs_len = rhs_bytes.len() / sz;
            let lhs_len = lhs_bytes.len() / sz;
            let mut out = Vec::with_capacity(lhs_bytes.len() + rhs_len * sz);
            out.extend_from_slice(&lhs_bytes);
            out.extend_from_slice(&rhs_bytes[..rhs_len * sz]);
            let header = match lhs {
                Value::Heap(r) => ctx.arena.header(r).type_,
                _ => None,
            };
            let r = ctx.alloc(
                header,
                Obj::Array(ArrayData {
                    typecode: lhs_tc,
                    free: 0,
                    len: lhs_len + rhs_len,
                    data: out,
                    source: None,
                }),
            );
            Ok(Value::Heap(r))
        }
        BinaryOp::InplaceAdd => {
            if is_view {
                return Err(PyError::PyRaise);
            }
            array_extend(ctx, lhs, rhs, None).map_err(|e| ctx.record_typed(e))?;
            Ok(lhs)
        }
        BinaryOp::Contains => {
            // The dispatcher converts `in` to `contains` with
            // swapped operands (vendor `runtime.c`), so here
            // `lhs` is the CONTAINER and `rhs` the element, as
            // the Zig's `arrayBinaryOpImpl` expects.
            if let Some((rhs_bytes, _)) = buffer_bytes(ctx, rhs) {
                let is_bytearray = array_typecode(ctx, lhs) == Some(BYTEARRAY_TYPECODE);
                if !is_bytearray {
                    return Ok(Value::Bool(false));
                }
                let Some((lhs_bytes, _)) = buffer_bytes(ctx, lhs) else {
                    return Err(PyError::PyRaise);
                };
                let found = find_subbytes(&lhs_bytes, &rhs_bytes, 1).is_some();
                return Ok(Value::Bool(found));
            }
            if is_int_or_float(ctx, rhs) {
                // Bare `NotImplementedError` (no message):
                // the vendor's `raiseNotImplemented()` sets
                // `pending_exc` AND returns PyRaise, so the
                // caller raises instead of falling back (the
                // bare `Err(PyRaise)` used to be swallowed to
                // `False`/`True` by the generic loop and the
                // `contains` iteration fallback).
                ctx.record_raise("NotImplementedError", None);
                return Err(PyError::PyRaise);
            }
            Ok(Value::Bool(false))
        }
        BinaryOp::Equal
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::More
        | BinaryOp::MoreEqual => {
            let Some((lhs_bytes, lhs_tc)) = buffer_bytes(ctx, lhs) else {
                return Err(PyError::PyRaise);
            };
            let Some((rhs_bytes, rhs_tc)) = buffer_bytes(ctx, rhs) else {
                // A non-buffer rhs is `False` for every
                // comparison (probed: `array('i', [1]) < 1`).
                return Ok(Value::Bool(false));
            };
            let (lhs_code, lhs_u) = typecode_for_comparison(lhs_tc);
            let (rhs_code, rhs_u) = typecode_for_comparison(rhs_tc);
            let is_unsigned = lhs_u || rhs_u;
            if lhs_code == rhs_code
                && lhs_code != b'f'
                && lhs_code != b'd'
                && (op == BinaryOp::Equal || is_unsigned)
            {
                return Ok(Value::Bool(seq_cmp_bytes(op, &lhs_bytes, &rhs_bytes)));
            }
            // Bare `NotImplementedError` (no message) — the
            // vendor's `raiseNotImplemented()` records it on
            // `pending_exc`; the bare `Err(PyRaise)` used to be
            // swallowed (the generic loop fell back to the
            // pointer comparison for `==`, to `TypeError` for
            // `<`, and the `contains` iteration fallback for
            // `in`).
            ctx.record_raise("NotImplementedError", None);
            Err(PyError::PyRaise)
        }
        _ => Err(PyError::PyRaise),
    }
}

/// `arraySubscrImpl` (`objarray.zig:505-686`): delete → NULL;
/// slice → load/store; else `mp_get_index` + get/set. A
/// memoryview load slices the VIEW (free += start, same
/// source); a store into a read-only view returns NULL; a
/// view store that changes the length is a compat error.
/// Any `fast_slice_indexes` failure (including step 0)
/// raises the step-1 `NotImplementedError` — unlike list,
/// where step 0 keeps "slice step can't be zero" (probed).
pub fn array_subscr(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let types = ctx.types;
    if arg == SubscrArg::Delete {
        return Err(PyError::PyRaise);
    }
    let Value::Heap(r) = self_ else {
        return Err(PyError::PyRaise);
    };
    let (tc, len, is_view, free, header) = match ctx.arena.get(r) {
        Obj::Array(a) => (
            a.typecode & TYPECODE_MASK,
            a.len,
            a.source.is_some(),
            a.free,
            ctx.arena.header(r).type_,
        ),
        _ => return Err(PyError::PyRaise),
    };
    let sz = match array_item_size(tc) {
        Ok(s) => s,
        Err(e) => return Err(ctx.record_typed(ArrayError::from(e))),
    };
    // A slice index is a heap `Obj::Slice` (C9c's type).
    let slice_data: Option<crate::objects::value::SliceData> = match index {
        Value::Heap(sr) => match ctx.arena.get(sr) {
            Obj::Slice(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(slice) = slice_data {
        let (bound, fast) = match fast_slice_indexes(ctx, types, &slice, len as i64) {
            Ok(v) => v,
            Err(_) => {
                return Err(ctx.record_typed(ArrayError::SliceStep));
            }
        };
        if !fast {
            return Err(ctx.record_typed(ArrayError::SliceStep));
        }
        let start = bound.start.max(0) as usize;
        let stop = bound.stop.max(0) as usize;
        let span = stop.saturating_sub(start);
        match arg {
            SubscrArg::Get => {
                if is_view {
                    if start > MEMVIEW_OFFSET_MAX {
                        return Err(ctx.record_typed(ArrayError::MemviewOffsetOverflow));
                    }
                    let source = match ctx.arena.get(r) {
                        Obj::Array(a) => a.source,
                        _ => None,
                    };
                    let view = ctx.alloc(
                        header,
                        Obj::Array(ArrayData {
                            typecode: match ctx.arena.get(r) {
                                Obj::Array(a) => a.typecode,
                                _ => tc,
                            },
                            free: free + start,
                            len: span,
                            data: Vec::new(),
                            source,
                        }),
                    );
                    return Ok(Value::Heap(view));
                }
                let bytes = match buffer_bytes(ctx, self_) {
                    Some((b, _)) => b,
                    None => return Err(PyError::PyRaise),
                };
                let res = ctx.alloc(
                    header,
                    Obj::Array(ArrayData {
                        typecode: tc,
                        free: 0,
                        len: span,
                        data: bytes[start * sz..(start + span) * sz].to_vec(),
                        source: None,
                    }),
                );
                return Ok(Value::Heap(res));
            }
            SubscrArg::Set(value) => {
                // Resolve the rhs bytes + element count.
                let (src_bytes, src_len) = if array_data(ctx, value).is_some() {
                    let src_tc = match array_typecode(ctx, value) {
                        Some(t) => t,
                        None => return Err(PyError::PyRaise),
                    };
                    let src_sz = match array_item_size(src_tc) {
                        Ok(s) => s,
                        Err(e) => return Err(ctx.record_typed(ArrayError::from(e))),
                    };
                    if src_sz != sz {
                        return Err(ctx.record_typed(ArrayError::SliceCompat));
                    }
                    let src_len = match array_data(ctx, value) {
                        Some(a) => a.len,
                        None => return Err(PyError::PyRaise),
                    };
                    let bytes = match buffer_bytes(ctx, value) {
                        Some((b, _)) => b,
                        None => return Err(PyError::PyRaise),
                    };
                    (bytes, src_len)
                } else if matches!(value, Value::Heap(br) if ctx.arena.header(br).type_ == Some(types.bytes)) {
                    if sz != 1 {
                        return Err(ctx.record_typed(ArrayError::SliceCompat));
                    }
                    let bytes = match buffer_bytes(ctx, value) {
                        Some((b, _)) => b,
                        None => return Err(PyError::PyRaise),
                    };
                    let n = bytes.len();
                    (bytes, n)
                } else {
                    return Err(ctx.record_typed(ArrayError::SliceRhsRequired));
                };
                let len_adj = src_len as isize - span as isize;
                // Resolve the destination: an owned array writes
                // into itself; a view writes into its SOURCE at
                // `(free + beg) * sz` and must not change length.
                let (dest_ref, byte_start, byte_end) = if is_view {
                    let is_rw = match ctx.arena.get(r) {
                        Obj::Array(a) => (a.typecode & ARRAY_TYPECODE_FLAG_RW) != 0,
                        _ => false,
                    };
                    if !is_rw {
                        return Err(PyError::PyRaise);
                    }
                    if len_adj != 0 {
                        return Err(ctx.record_typed(ArrayError::SliceCompat));
                    }
                    let src = match ctx.arena.get(r) {
                        Obj::Array(a) => a.source,
                        _ => None,
                    };
                    let Some(src) = src else {
                        return Err(PyError::PyRaise);
                    };
                    (src, (free + start) * sz, (free + stop) * sz)
                } else {
                    (r, start * sz, stop * sz)
                };
                let new_len = (len as isize + len_adj) as usize;
                match ctx.arena.get_mut(dest_ref) {
                    Obj::Array(dest) => {
                        dest.data.splice(
                            byte_start..byte_end,
                            src_bytes[..src_len * sz].iter().copied(),
                        );
                        if !is_view {
                            dest.len = new_len;
                        }
                    }
                    _ => return Err(PyError::PyRaise),
                }
                return Ok(Value::None);
            }
            SubscrArg::Delete => return Err(PyError::PyRaise),
        }
    }
    // Non-slice: `mp_get_index` (negative wrap, OOB →
    // `IndexError`, non-int → `TypeError`).
    let idx = match get_index(ctx, types, len, index, false) {
        Ok(i) => i,
        Err(e) => return Err(ctx.record_typed(ArrayError::from(e))),
    };
    match arg {
        SubscrArg::Get => Ok(array_get_element(ctx, self_, idx)),
        SubscrArg::Set(value) => {
            if is_view {
                let is_rw = match ctx.arena.get(r) {
                    Obj::Array(a) => (a.typecode & ARRAY_TYPECODE_FLAG_RW) != 0,
                    _ => false,
                };
                if !is_rw {
                    return Err(PyError::PyRaise);
                }
            }
            array_set_element(ctx, self_, idx, value).map_err(|e| ctx.record_typed(e))?;
            Ok(Value::None)
        }
        SubscrArg::Delete => Err(PyError::PyRaise),
    }
}

/// `arrayGetBuffer` (`objarray.zig:688-701`): `buf = items`,
/// `len = len * sz`, `typecode = masked`; a view's `buf +=
/// free * sz`. A write flag on a read-only view fails
/// (`Ok(false)` — the caller raises or falls through). The
/// pointer is stored, never dereferenced (the safe-Rust
/// buffer rule — see `objarray.rs`).
pub fn array_get_buffer(
    ctx: &mut CallCtx,
    self_: Value,
    bufinfo: &mut BufferInfo,
    flags: u32,
) -> PyResult<bool> {
    let Value::Heap(r) = self_ else {
        return Err(PyError::PyRaise);
    };
    let (tc, len, is_view, free, is_rw) = match ctx.arena.get(r) {
        Obj::Array(a) => (
            a.typecode & TYPECODE_MASK,
            a.len,
            a.source.is_some(),
            a.free,
            (a.typecode & ARRAY_TYPECODE_FLAG_RW) != 0,
        ),
        _ => return Err(PyError::PyRaise),
    };
    let sz = match array_item_size(tc) {
        Ok(s) => s,
        Err(e) => return Err(ctx.record_typed(ArrayError::from(e))),
    };
    if is_view && !is_rw && (flags & BUFFER_WRITE) != 0 {
        return Ok(false);
    }
    // Resolve the base pointer: owned → own `data`; view →
    // the source's `data` at `free * sz`. The offset is
    // computed as an integer (no deref ever happens here —
    // the pointer is only stored in `BufferInfo`, exactly
    // like the Zig's `@ptrFromInt`).
    let base_ptr: *const u8 = if is_view {
        let src = match ctx.arena.get(r) {
            Obj::Array(a) => a.source,
            _ => None,
        };
        let Some(src) = src else {
            return Err(PyError::PyRaise);
        };
        match ctx.arena.get(src) {
            Obj::Array(base) => base.data.as_ptr(),
            Obj::Bytes(b) => b.bytes.as_ptr(),
            Obj::Str(s) => s.bytes.as_ptr(),
            _ => return Err(PyError::PyRaise),
        }
    } else {
        match ctx.arena.get(r) {
            Obj::Array(a) => a.data.as_ptr(),
            _ => return Err(PyError::PyRaise),
        }
    };
    let ptr = ((base_ptr as usize) + free * sz) as *const u8;
    bufinfo.data = ptr;
    bufinfo.len = len * sz;
    bufinfo.typecode = tc;
    Ok(true)
}

/// `npy_array_iterator_new` + `npy_array_it_iternext`
/// (`objarray.zig:703-755`): a heap `Obj::Iterator` (tag
/// `ARRAY_ITER_TAG`, C9a's iterator convention) whose cursor
/// advances in place; the offset is derived from the array
/// bits on the fly (`free` for a view, else 0). Header is the
/// `iterator` ROM type (G4/H8 — `mp_type_polymorph_iter`).
pub fn array_getiter(ctx: &mut CallCtx, self_: Value) -> PyResult<Value> {
    Ok(Value::Heap(ctx.alloc(
        Some(ctx.types.iterator),
        Obj::Iterator(crate::objects::value::IteratorData {
            source: self_,
            index: 0,
            extra: vec![Value::SmallInt(ARRAY_ITER_TAG)],
        }),
    )))
}

pub fn array_iternext(
    ctx: &mut CallCtx,
    iter: Value,
) -> Result<Option<Value>, ArrayError> {
    let Value::Heap(r) = iter else {
        return Err(ArrayError::PyRaise);
    };
    let (source, index) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.index),
        _ => return Err(ArrayError::PyRaise),
    };
    let len = match array_data(ctx, source) {
        Some(a) => a.len,
        None => return Err(ArrayError::PyRaise),
    };
    if index < len {
        if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
            d.index = index + 1;
        }
        Ok(Some(array_get_element(ctx, source, index)))
    } else {
        Ok(None)
    }
}

fn arity(args: &[Value], n: usize) -> Result<(), PyError> {
    if args.len() != n {
        return Err(PyError::PyRaise);
    }
    Ok(())
}

/// `array.append` / `bytearray.append` (fun_builtin_2).
pub fn array_append_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    array_append(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

/// `array.extend` / `bytearray.extend` (fun_builtin_2).
pub fn array_extend_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    array_extend(ctx, args[0], args[1], None).map_err(|e| ctx.record_typed(e))
}

/// `array`'s / `bytearray`'s method table (C13, the Zig's
/// `mp_obj_array_methods` shape): name, body, positional arity
/// counting `self`. Sigs are the ones the adapters already
/// enforce (see their doc comments).
pub static ARRAY_METHODS: &[crate::objects::slots::MethodEntry] = &[
    crate::objects::slots::MethodEntry {
        name: "append",
        func: array_append_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "extend",
        func: array_extend_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
];
