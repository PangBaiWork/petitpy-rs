//! The `struct` builtin module (`src/modules/modstruct.zig`, 374
//! lines — itself a port of vendor `py/modstruct.c`).
//!
//! The typecode-driven byte walking lives in C9b's
//! `src/objects/containers/binary.rs` (`binary_get_size` /
//! `binary_get_val` / `binary_set_val`); this module is the
//! format-string layer on top: `getFmtType` / `getFmtNum` /
//! `calcSizeItems` and the four exports `calcsize` / `pack` /
//! `pack_into` / `unpack` (with `unpack_from` the same fn
//! object, as in the C).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d3_result.txt` §2):
//! - The byte-order prefix is `'!'` → `'>'`, `'@'` / `'<'` /
//!   `'>'` kept, `'='` consumed but NOT a valid struct type —
//!   `binary_get_size` yields size 0 for it, so `calcsize('=i')`
//!   is `ValueError("bad typecode")` (the Rust `binary.rs`
//!   port already behaves this way).
//! - `'f'` / `'d'` / `'e'` have SIZES but their value branches
//!   are dead (the Zig's `binary.zig` FLOAT=0): `unpack('<f',
//!   ...)` returns the INT bit pattern, `pack('<f', 1.5)` is
//!   `TypeError("can't convert to int")`.
//! - `'@'` uses native sizes/aligns (`l` = 8, `@hi` = 8 vs
//!   `<hi` = 6); `'x'` pads without aligning.
//! - Extra `pack` args are silently ignored; missing args are
//!   zero-filled (the buffer is zero-initialised).
//! - `unpack` on a non-buffer is `ValueError("buffer too
//!   small")` (the Zig ignores the `getBufferRaise` failure and
//!   reads an empty buffer); `pack_into` into immutable
//!   bytes/str is the same error (BUFFER_WRITE refused).
//! - Negative offsets count from the end; still negative is
//!   "buffer too small".
//! - `'S'` get returns an empty str in safe Rust (the
//!   `binary.rs` documented limit); `'O'` round-trips the
//!   REPR_A word.
//! - There is NO `struct.error` type.
//!
//! Divergences (documented, unobservable for sane inputs):
//! - `pack('<s', <non-buffer>)` zero-fills in Rust; the Zig
//!   reads an uninitialised buffer (undefined behaviour).
//! - Absurd repeat counts saturate instead of reading a
//!   mistagged heap-int value (the Zig's `getFmtNum` bit-casts
//!   `smallIntValue` of a heap int).

use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::{
    array_item_size, binary_get_size, binary_get_val, binary_set_val, buffer_bytes,
    new_int_from_u64, BinaryError, TYPECODE_MASK,
};
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_int;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::{static_qstr_id, str_bytes_of, QstrResolver};
use crate::objects::value::{BytesData, FunBuiltinData, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};

/// Record a `BinaryError` richly (the C6 convention: the
/// exception type + terse message go in `pending_raise`).
fn record_binary(ctx: &mut CallCtx, e: BinaryError) -> PyError {
    if matches!(e, BinaryError::OutOfMemory) {
        return PyError::OutOfMemory;
    }
    if !e.exc_name().is_empty() {
        ctx.record_raise(e.exc_name(), e.message());
    }
    PyError::PyRaise
}

/// `raiseValueErrorBufferTooSmall` (`modstruct.zig:81-84`).
fn buffer_too_small(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("ValueError", Some("buffer too small"));
    PyError::PyRaise
}

/// The format string of an argument (`mp_obj_str_get_str`):
/// str or bytes only — anything else is the polled
/// `TypeError` the Zig records (via `str_bytes_of`'s `None`
/// → `IntConversion` is wrong here, so this is its own
/// error: the Zig's `mp_obj_str_get_str` raises a bare
/// TypeError on a non-str/bytes argument).
fn fmt_bytes(ctx: &mut CallCtx, v: Value) -> Result<Vec<u8>, PyError> {
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
    match str_bytes_of(ctx.arena, ctx.types, resolver, v) {
        Some(b) => Ok(b),
        None => {
            ctx.record_raise("TypeError", None);
            Err(PyError::PyRaise)
        }
    }
}

/// `getFmtType` (`modstruct.zig:103-111`): the byte-order
/// prefix; `'!'` becomes `'>'`; anything else leaves `'@'`
/// WITHOUT consuming the byte. `pos` is the cursor into the
/// format bytes.
fn get_fmt_type(fmt: &[u8], pos: &mut usize) -> u8 {
    let Some(&t) = fmt.get(*pos) else {
        return b'@';
    };
    match t {
        b'!' => {
            *pos += 1;
            b'>'
        }
        b'@' | b'=' | b'<' | b'>' => {
            *pos += 1;
            t
        }
        _ => b'@',
    }
}

/// `getFmtNum` (`modstruct.zig:113-122`): the digit run at
/// the cursor (the cursor is ON a digit). Saturating, so an
/// absurd run does not wrap.
fn get_fmt_num(fmt: &[u8], pos: &mut usize) -> usize {
    let mut n: usize = 0;
    while let Some(&c) = fmt.get(*pos) {
        if !c.is_ascii_digit() {
            break;
        }
        n = n
            .saturating_mul(10)
            .saturating_add((c - b'0') as usize);
        *pos += 1;
    }
    n
}

/// `calcSizeItems` (`modstruct.zig:124-146`): `(item count,
/// total size)`. `'x'` contributes size only, `'s'`
/// contributes one item; every other typecode contributes
/// `cnt` items, each aligned to its alignment then sized.
/// Saturating arithmetic (see module docs).
fn calc_size_items(fmt: &[u8]) -> Result<(usize, usize), BinaryError> {
    let mut pos = 0;
    let fmt_type = get_fmt_type(fmt, &mut pos);
    let mut total_cnt: usize = 0;
    let mut size: usize = 0;
    while let Some(&c) = fmt.get(pos) {
        let mut cnt: usize = 1;
        if c.is_ascii_digit() {
            cnt = get_fmt_num(fmt, &mut pos);
        }
        let Some(&c) = fmt.get(pos) else {
            // A trailing digit run (`calcsize("2")`): the Zig
            // calls `mp_binary_get_size(fmt_type, 0)` here, which
            // is `ValueError("bad typecode")`.
            return Err(BinaryError::BadTypecode);
        };
        if c == b'x' {
            size = size.saturating_add(cnt);
        } else if c == b's' {
            total_cnt = total_cnt.saturating_add(1);
            size = size.saturating_add(cnt);
        } else {
            total_cnt = total_cnt.saturating_add(cnt);
            let (sz, alignv) = binary_get_size(fmt_type, c)?;
            let mut left = cnt;
            while left > 0 {
                size = (size + alignv - 1) & !(alignv - 1);
                size = size.saturating_add(sz);
                left -= 1;
            }
        }
        pos += 1;
    }
    Ok((total_cnt, size))
}

/// `structCalcsizeImpl` (`modstruct.zig:148-153`).
fn struct_calcsize(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fmt = fmt_bytes(ctx, args[0])?;
    let (_, size) = calc_size_items(&fmt).map_err(|e| record_binary(ctx, e))?;
    Ok(new_int_from_u64(ctx, size as u64))
}

/// `structUnpackFromImpl` (`modstruct.zig:161-208`): `unpack`
/// and `unpack_from` (sig (2, 3)) share this body.
fn struct_unpack_from(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fmt = fmt_bytes(ctx, args[0])?;
    let (num_items, total_sz) = calc_size_items(&fmt).map_err(|e| record_binary(ctx, e))?;
    let mut pos = 0;
    let fmt_type = get_fmt_type(&fmt, &mut pos);
    // A non-buffer reads as an empty buffer (see module
    // docs): the size check below then raises "buffer too
    // small" whenever the format needs bytes.
    let (buf, _) = buffer_bytes(ctx, args[1]).unwrap_or_default();
    let mut offset: i64 = 0;
    if args.len() > 2 {
        offset = obj_get_int(ctx, ctx.types, args[2]).map_err(|e| ctx.record_typed(e))?;
        if offset < 0 {
            offset += buf.len() as i64;
            if offset < 0 {
                return Err(buffer_too_small(ctx));
            }
        }
    }
    let base = offset as usize;
    if base.checked_add(total_sz).map_or(true, |end| end > buf.len()) {
        return Err(buffer_too_small(ctx));
    }
    let slice = &buf[base..];
    let mut p: usize = 0;
    let mut items: Vec<Value> = Vec::with_capacity(num_items);
    let mut i = 0;
    while i < num_items {
        let mut cnt: usize = 1;
        if let Some(&c) = fmt.get(pos) {
            if c.is_ascii_digit() {
                cnt = get_fmt_num(&fmt, &mut pos);
            }
        }
        let Some(&c) = fmt.get(pos) else {
            break;
        };
        if c == b'x' {
            p += cnt;
        } else if c == b's' {
            let bytes = slice.get(p..p + cnt).unwrap_or(&[]).to_vec();
            p += cnt;
            let v = Value::Heap(ctx.alloc(Some(ctx.types.bytes), Obj::Bytes(BytesData { bytes })));
            ctx.roots.push(v);
            items.push(v);
            i += 1;
        } else {
            while cnt > 0 {
                let item = binary_get_val(ctx, fmt_type, c, slice, &mut p)
                    .map_err(|e| record_binary(ctx, e))?;
                ctx.roots.push(item);
                items.push(item);
                i += 1;
                cnt -= 1;
            }
        }
        pos += 1;
    }
    let tuple = new_tuple(ctx, ctx.types.tuple, &items);
    Ok(Value::Heap(tuple))
}

/// `structPackIntoInternal` (`modstruct.zig:223-256`):
/// write `values` into `buf` (exactly `calcsize(fmt)` long,
/// zero-initialised by the caller). Extra values are ignored
/// (`fmt[0] == 0` → break); missing values leave the
/// zero-fill. A non-buffer `'s'` value zero-fills (see
/// module docs).
fn pack_into_internal(
    ctx: &mut CallCtx,
    fmt: &[u8],
    buf: &mut [u8],
    values: &[Value],
) -> Result<(), BinaryError> {
    let mut pos = 0;
    let fmt_type = get_fmt_type(fmt, &mut pos);
    let mut p: usize = 0;
    let mut i = 0;
    while i < values.len() {
        if pos >= fmt.len() {
            break;
        }
        let mut cnt: usize = 1;
        if let Some(&c) = fmt.get(pos) {
            if c.is_ascii_digit() {
                cnt = get_fmt_num(fmt, &mut pos);
            }
        }
        let Some(&c) = fmt.get(pos) else {
            break;
        };
        if c == b'x' {
            buf[p..p + cnt].fill(0);
            p += cnt;
        } else if c == b's' {
            let (bytes, _) = buffer_bytes(ctx, values[i]).unwrap_or_default();
            i += 1;
            let to_copy = bytes.len().min(cnt);
            buf[p..p + to_copy].copy_from_slice(&bytes[..to_copy]);
            buf[p + to_copy..p + cnt].fill(0);
            p += cnt;
        } else {
            while cnt > 0 && i < values.len() {
                binary_set_val(ctx, fmt_type, c, values[i], buf, &mut p)?;
                i += 1;
                cnt -= 1;
            }
        }
        pos += 1;
    }
    Ok(())
}

/// `structPackImpl` (`modstruct.zig:258-266`).
fn struct_pack(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fmt = fmt_bytes(ctx, args[0])?;
    let (_, size) = calc_size_items(&fmt).map_err(|e| record_binary(ctx, e))?;
    let mut buf = vec![0u8; size];
    pack_into_internal(ctx, &fmt, &mut buf, &args[1..]).map_err(|e| record_binary(ctx, e))?;
    Ok(Value::Heap(
        ctx.alloc(Some(ctx.types.bytes), Obj::Bytes(BytesData { bytes: buf })),
    ))
}

/// Resolve a writable array target to `(root ref, byte offset
/// into the root's storage, view byte length)`. A view
/// (`source: Some`) walks to its root, accumulating the byte
/// offset exactly as `buffer_bytes` does for reads. `None`
/// when the target is not an array at all.
fn writable_array_root(ctx: &CallCtx, v: Value) -> Option<(GcRef, usize, usize)> {
    let Value::Heap(mut r) = v else {
        return None;
    };
    let mut off = 0usize;
    loop {
        match ctx.arena.get(r) {
            Obj::Array(a) => {
                let sz = array_item_size(a.typecode & TYPECODE_MASK).ok()?;
                let len = a.len.saturating_mul(sz);
                match a.source {
                    None => return Some((r, off, len)),
                    Some(src) => {
                        off = off.saturating_add(a.free.saturating_mul(sz));
                        r = src;
                    }
                }
            }
            _ => return None,
        }
    }
}

/// `structPackIntoImpl` (`modstruct.zig:272-291`).
fn struct_pack_into(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fmt = fmt_bytes(ctx, args[0])?;
    let (_, size) = calc_size_items(&fmt).map_err(|e| record_binary(ctx, e))?;
    // The writable length: a non-array target (bytes/str/int)
    // reads as length 0, so the size check below raises
    // "buffer too small" exactly as the reference does.
    let (root, root_off, view_len) = match writable_array_root(ctx, args[1]) {
        Some(t) => t,
        None => (GcRef(0), 0, 0),
    };
    let mut offset = obj_get_int(ctx, ctx.types, args[2]).map_err(|e| ctx.record_typed(e))?;
    if offset < 0 {
        offset += view_len as i64;
        if offset < 0 {
            return Err(buffer_too_small(ctx));
        }
    }
    let p = offset as usize;
    if p.checked_add(size).map_or(true, |end| end > view_len) {
        return Err(buffer_too_small(ctx));
    }
    if size > 0 {
        let start = root_off + p;
        let end = start + size;
        // Copy-out / copy-back: `pack_into_internal` needs
        // `&mut ctx` while the destination lives in the arena,
        // and bytes past the last written value keep their
        // ORIGINAL content (not zero — the Zig writes into the
        // existing buffer).
        let mut dest = match ctx.arena.get(root) {
            Obj::Array(a) => a.data[start..end].to_vec(),
            _ => return Err(buffer_too_small(ctx)),
        };
        pack_into_internal(ctx, &fmt, &mut dest, &args[3..])
            .map_err(|e| record_binary(ctx, e))?;
        if let Obj::Array(a) = ctx.arena.get_mut(root) {
            a.data[start..end].copy_from_slice(&dest);
        }
    }
    Ok(Value::None)
}

/// Build the `struct` module object (`modstruct.zig:333-348`):
/// globals `{__name__: "struct", calcsize, pack, pack_into,
/// unpack, unpack_from}` — `unpack` and `unpack_from` are the
/// SAME fn object, as in the C. Registration into
/// `VmState.builtin_modules` is D1's (the `build_io_module`
/// pattern).
pub fn build_struct_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let struct_id = static_qstr_id(b"struct").expect("struct is a static qstr");
    let module = new_module(ctx, struct_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let bind = |ctx: &mut CallCtx, dict_v: Value, name: &str, min: u16, max: u16, func: crate::objects::slots::BuiltinFn| -> PyResult<Value> {
        let id = static_qstr_id(name.as_bytes()).expect("struct fn name is a static qstr");
        let fun = ctx.alloc(
            Some(fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: None,
            }),
        );
        let v = Value::Heap(fun);
        ctx.roots.push(v);
        dict_store(ctx, dict_v, Value::Qstr(id), v).map_err(|e| ctx.record_typed(e))?;
        Ok(v)
    };
    bind(ctx, dict_v, "calcsize", 1, 1, struct_calcsize)?;
    bind(ctx, dict_v, "pack", 1, 255, struct_pack)?;
    bind(ctx, dict_v, "pack_into", 3, 255, struct_pack_into)?;
    // `unpack` and `unpack_from` are one fn object.
    let unpack = bind(ctx, dict_v, "unpack", 2, 3, struct_unpack_from)?;
    let unpack_from_id =
        static_qstr_id(b"unpack_from").expect("unpack_from is a static qstr");
    dict_store(ctx, dict_v, Value::Qstr(unpack_from_id), unpack)
        .map_err(|e| ctx.record_typed(e))?;
    Ok(module)
}
