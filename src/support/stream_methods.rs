//! Stream method bodies + the `STREAM_METHODS` table
//! (`support/stream.zig` — second half of Lane C16's stream
//! port; the protocol core — resolution, `mp_stream_rw`,
//! `streamReadall`, `streamReadGeneric` — lives in
//! `super::stream`).
//!
//! - `stream_readinto_write_generic` (`streamReadintoWriteGeneric`)
//! - `stream_unbuffered_readline` / `readlines` / `iter`
//! - the 14 builtin bodies + `STREAM_METHODS`
//!
//! The write-back helper (`write_buffer_back`) resolves a
//! buffer value to its owning arena object (following `Array`
//! source chains) and copies into it — the safe-Rust form of
//! the Zig writing through `bufinfo.buf` (no raw-pointer
//! deref; see `buffer_bytes` for the read half).

use super::stream::{
    get_stream_raise, stream_close, stream_read_generic, stream_rw_read, stream_seek,
    stream_write, MP_EINVAL, MP_STREAM_FLUSH, MP_STREAM_OP_IOCTL, MP_STREAM_OP_READ,
    MP_STREAM_RW_ONCE, MP_STREAM_RW_READ, MP_STREAM_RW_WRITE, SEEK_CUR, SEEK_SET,
};
use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::{array_item_size, buffer_bytes, TYPECODE_MASK};
use crate::objects::obj::{
    obj_get_int, obj_get_int_truncated, obj_is_true, ObjError, BUFFER_READ, BUFFER_WRITE,
    BUFFER_RAISE_IF_UNSUPPORTED,
};
use crate::objects::obj_ops::get_buffer;
use crate::objects::slots::{BufferInfo, IoctlArg, MethodEntry, StreamIoctlFn, StreamReadFn};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

// ── readinto / write ─────────────────────────────────────────────────────

/// Resolve a buffer value to the arena object that OWNS its
/// bytes plus the byte offset into it: `Bytes` / `Str` are
/// their own root; an `Array` view follows its `source`
/// chain (`free * sz` per hop, exactly as
/// `array_get_buffer` resolves the base pointer). `None`
/// when the value has no such root (no buffer provider
/// today — defensive; `get_buffer` already validated the
/// protocol before this is called).
fn resolve_buffer_root(ctx: &CallCtx, value: Value) -> Option<(crate::objects::value::GcRef, usize)> {
    let Value::Heap(mut r) = value else {
        return None;
    };
    let mut off = 0usize;
    loop {
        match ctx.arena.get(r) {
            Obj::Bytes(_) | Obj::Str(_) => return Some((r, off)),
            Obj::Array(a) => match a.source {
                None => return Some((r, off)),
                Some(src) => {
                    let sz = array_item_size(a.typecode & TYPECODE_MASK).ok()?;
                    off += a.free * sz;
                    r = src;
                }
            },
            _ => return None,
        }
    }
}

/// Write `data` back into a buffer at byte offset `off`
/// (`streamReadinto`'s destination). The bytes land in the
/// owning object via `arena.get_mut` — the safe-Rust form of
/// the Zig writing through `bufinfo.buf` (no raw-pointer
/// deref, exactly as the tree's `buffer_bytes` reads through
/// a copy). `Obj::Str` / `Obj::Bytes` never reach here:
/// vendor `mp_obj_str_get_buffer` refuses `BUFFER_WRITE`
/// ("can't write to a string"), so `get_buffer` above
/// already raised for them.
fn write_buffer_back(
    ctx: &mut CallCtx,
    target: Value,
    off: usize,
    data: &[u8],
) -> PyResult<()> {
    let Some((root, base_off)) = resolve_buffer_root(ctx, target) else {
        return Err(ctx.record_typed(ObjError::BufferProtocol));
    };
    let start = base_off + off;
    let end = start + data.len();
    match ctx.arena.get_mut(root) {
        Obj::Array(a) => {
            if end > a.data.len() {
                return Err(ctx.record_typed(ObjError::BufferProtocol));
            }
            a.data[start..end].copy_from_slice(data);
        }
        Obj::Bytes(b) => {
            if end > b.bytes.len() {
                return Err(ctx.record_typed(ObjError::BufferProtocol));
            }
            b.bytes[start..end].copy_from_slice(data);
        }
        Obj::Str(s) => {
            if end > s.bytes.len() {
                return Err(ctx.record_typed(ObjError::BufferProtocol));
            }
            s.bytes[start..end].copy_from_slice(data);
        }
        _ => return Err(ctx.record_typed(ObjError::BufferProtocol)),
    }
    Ok(())
}

/// `streamReadintoWriteGeneric` (`stream.zig:391-415`):
/// `readinto` / `readinto1` / `write` / `write1`. The buffer
/// flags are crossed: a WRITE call reads FROM the buffer
/// (`MP_BUFFER_READ`), a READ call writes INTO it
/// (`MP_BUFFER_WRITE`). The optional `max_len` / `off` /
/// `max_len` args are CPython extensions; `off` clamps to
/// the buffer length.
pub fn stream_readinto_write_generic(
    ctx: &mut CallCtx,
    args: &[Value],
    flags: u8,
) -> PyResult<Value> {
    let types = ctx.types;
    let buf_flags = if flags & MP_STREAM_RW_WRITE != 0 {
        BUFFER_READ
    } else {
        BUFFER_WRITE
    };
    let mut bufinfo = BufferInfo::empty();
    get_buffer(
        ctx,
        types,
        args[1],
        &mut bufinfo,
        buf_flags | BUFFER_RAISE_IF_UNSUPPORTED,
    )
    .map_err(|e| ctx.record_typed(e))?;
    let mut max_len = usize::MAX;
    let mut off = 0usize;
    if args.len() == 3 {
        max_len =
            obj_get_int_truncated(ctx, types, args[2]).map_err(|e| ctx.record_typed(e))? as usize;
    } else if args.len() == 4 {
        off =
            obj_get_int_truncated(ctx, types, args[2]).map_err(|e| ctx.record_typed(e))? as usize;
        max_len =
            obj_get_int_truncated(ctx, types, args[3]).map_err(|e| ctx.record_typed(e))? as usize;
        if off > bufinfo.len {
            off = bufinfo.len;
        }
    }
    let len = bufinfo.len - off;
    let n = if len < max_len { len } else { max_len };
    if flags & MP_STREAM_RW_WRITE != 0 {
        // Write: read the window OUT of the buffer (safe copy
        // via `buffer_bytes`) and push it through the stream.
        let (data, _) = buffer_bytes(ctx, args[1])
            .ok_or_else(|| ctx.record_typed(ObjError::BufferProtocol))?;
        let end = off + n;
        if end > data.len() {
            return Err(ctx.record_typed(ObjError::BufferProtocol));
        }
        return stream_write(ctx, args[0], &data[off..end], flags);
    }
    // Read: pull `n` bytes through the stream, then write
    // them back into the buffer.
    let mut tmp: Vec<u8> = Vec::new();
    tmp.try_reserve_exact(n)
        .map_err(|_| PyError::OutOfMemory)?;
    tmp.resize(n, 0);
    let got = stream_rw_read(ctx, args[0], &mut tmp, flags)?;
    tmp.truncate(got);
    write_buffer_back(ctx, args[1], off, &tmp)?;
    Ok(Value::int(
        ctx.arena,
        Some(types.int),
        ctx.roots,
        got as i64,
    ))
}

// ── unbuffered readline / readlines / iter ──────────────────────────────

/// `streamUnbufferedReadline`'s `max_size`:
/// `mirrors.smallIntValue` is a raw tag-unshift (`word >> 1`),
/// NOT a conversion — so every immediate reads back as its
/// unshifted word: small int → itself, `True` (30) → 15,
/// `False` (14) → 7, `None` (6) → 3, qstr (`(q<<3)|2`) →
/// `4q + 1`. A heap object reads back as its pointer
/// shifted — a huge positive number whose `vstr_init` fails
/// with `MemoryError`, so Rust returns `OutOfMemory`
/// directly for `Value::Heap`.
fn readline_max_size(arg: Value) -> PyResult<i64> {
    match arg {
        Value::SmallInt(n) => Ok(n),
        Value::Bool(true) => Ok(15),
        Value::Bool(false) => Ok(7),
        Value::None => Ok(3),
        Value::Qstr(id) => Ok(id as i64 * 4 + 1),
        Value::Heap(_) => Err(PyError::OutOfMemory),
    }
}

/// `streamUnbufferedReadline` (`stream.zig:430-478`): one byte
/// at a time until `\n`, EOF (0 read), or `max_size` bytes.
/// `max_size < -1` is a `MemoryError` (the Zig's
/// `vstr_init` with a negative size); `max_size == 0` returns
/// empty immediately; `max_size == -1` is unlimited. A read
/// error raises `OSError` (the non-blocking branches are
/// dead — `is_nonblocking_error` is `false`).
pub fn stream_unbuffered_readline(
    ctx: &mut CallCtx,
    args: &[Value],
) -> PyResult<Value> {
    let stream_p = get_stream_raise(ctx, args[0], MP_STREAM_OP_READ)?;
    let read: StreamReadFn = stream_p
        .read
        .expect("get_stream_raise checked MP_STREAM_OP_READ");
    let mut max_size: i64 = -1;
    if args.len() > 1 {
        max_size = readline_max_size(args[1])?;
    }
    if max_size < -1 {
        return Err(PyError::OutOfMemory);
    }
    // `vstr_init(&vstr, max_size)` allocates upfront when
    // bounded (16 when unlimited); failure is a MemoryError.
    let mut out: Vec<u8> = Vec::new();
    if max_size != -1 {
        out.try_reserve_exact(max_size as usize)
            .map_err(|_| PyError::OutOfMemory)?;
    } else {
        out.try_reserve_exact(16)
            .map_err(|_| PyError::OutOfMemory)?;
    }
    let mut remaining = max_size;
    while remaining == -1 || remaining != 0 {
        if remaining != -1 {
            remaining -= 1;
        }
        let mut one = [0u8; 1];
        let n = read(ctx, args[0], &mut one)?;
        if n == 0 {
            break;
        }
        // Geometric growth with `MemoryError` (not abort) on
        // failure — the Zig's `vstr_add_len` equivalent.
        if out.len() == out.capacity() {
            let extra = out.capacity().max(1);
            out.try_reserve(extra)
                .map_err(|_| PyError::OutOfMemory)?;
        }
        out.push(one[0]);
        if one[0] == b'\n' {
            break;
        }
    }
    if stream_p.is_text {
        Ok(new_str(ctx, ctx.types, &out))
    } else {
        Ok(new_bytes(ctx, ctx.types, &out))
    }
}

/// `streamUnbufferedReadlines` (`stream.zig:480-488`): lines
/// until a falsy one (the empty line at EOF).
pub fn stream_unbuffered_readlines(ctx: &mut CallCtx, stream: Value) -> PyResult<Value> {
    let types = ctx.types;
    let mut items: Vec<Value> = Vec::new();
    loop {
        let line = stream_unbuffered_readline(ctx, &[stream])?;
        if !obj_is_true(ctx, types, line)? {
            break;
        }
        items.push(line);
    }
    Ok(Value::Heap(crate::objects::containers::objlist::new_list(
        ctx,
        types.list,
        &items,
    )))
}

/// `mp_stream_unbuffered_iter` (`py/stream.c:403`): one line,
/// or `None` for `StopIteration` (the empty line at EOF).
pub fn stream_unbuffered_iter(
    ctx: &mut CallCtx,
    stream: Value,
) -> PyResult<Option<Value>> {
    let line = stream_unbuffered_readline(ctx, &[stream])?;
    if obj_is_true(ctx, ctx.types, line)? {
        Ok(Some(line))
    } else {
        Ok(None)
    }
}

// ── Builtin method bodies ───────────────────────────────────────────────

pub(crate) fn stream_read_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_read_generic(ctx, args, MP_STREAM_RW_READ)
}

fn stream_read1_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_read_generic(ctx, args, MP_STREAM_RW_READ | MP_STREAM_RW_ONCE)
}

pub(crate) fn stream_write_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_readinto_write_generic(ctx, args, MP_STREAM_RW_WRITE)
}

fn stream_write1_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_readinto_write_generic(ctx, args, MP_STREAM_RW_WRITE | MP_STREAM_RW_ONCE)
}

pub(crate) fn stream_readinto_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_readinto_write_generic(ctx, args, MP_STREAM_RW_READ)
}

fn stream_readinto1_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_readinto_write_generic(ctx, args, MP_STREAM_RW_READ | MP_STREAM_RW_ONCE)
}

pub(crate) fn stream_unbuffered_readline_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_unbuffered_readline(ctx, args)
}

pub(crate) fn stream_unbuffered_readlines_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_unbuffered_readlines(ctx, args[0])
}

pub(crate) fn stream_close_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_close(ctx, args[0])
}

pub(crate) fn stream_exit_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    stream_close(ctx, args[0])
}

pub(crate) fn stream_seek_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let types = ctx.types;
    let offset = obj_get_int(ctx, types, args[1]).map_err(|e| ctx.record_typed(e))?;
    let mut whence = SEEK_SET;
    if args.len() == 3 {
        whence = obj_get_int(ctx, types, args[2]).map_err(|e| ctx.record_typed(e))? as i32;
    }
    // In POSIX, it's an error to seek before end of stream.
    if whence == SEEK_SET && offset < 0 {
        return Err(raise_os_error(ctx, MP_EINVAL));
    }
    let res = stream_seek(ctx, args[0], offset, whence)?;
    Ok(Value::int(
        ctx.arena,
        Some(types.int),
        ctx.roots,
        res,
    ))
}

pub(crate) fn stream_tell_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // `streamTell`: `streamSeek(0, SEEK_CUR)`.
    let res = stream_seek(ctx, args[0], 0, SEEK_CUR)?;
    Ok(Value::int(
        ctx.arena,
        Some(ctx.types.int),
        ctx.roots,
        res,
    ))
}

pub(crate) fn stream_flush_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let stream_p = get_stream_raise(ctx, args[0], MP_STREAM_OP_IOCTL)?;
    let ioctl: StreamIoctlFn = stream_p
        .ioctl
        .expect("get_stream_raise checked MP_STREAM_OP_IOCTL");
    ioctl(ctx, args[0], MP_STREAM_FLUSH, IoctlArg::None)?;
    Ok(Value::None)
}

fn stream_ioctl_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let types = ctx.types;
    let mut val = 0usize;
    if args.len() > 2 {
        // `mp_get_buffer` WITHOUT the raise flag: a buffer
        // yields its pointer, anything else falls through to
        // the truncated int.
        let mut bufinfo = BufferInfo::empty();
        match get_buffer(ctx, types, args[2], &mut bufinfo, BUFFER_WRITE) {
            Ok(true) => val = bufinfo.data as usize,
            Ok(false) => {
                val = obj_get_int_truncated(ctx, types, args[2])
                    .map_err(|e| ctx.record_typed(e))? as usize;
            }
            Err(e) => return Err(ctx.record_typed(e)),
        }
    }
    let stream_p = get_stream_raise(ctx, args[0], MP_STREAM_OP_IOCTL)?;
    let ioctl: StreamIoctlFn = stream_p
        .ioctl
        .expect("get_stream_raise checked MP_STREAM_OP_IOCTL");
    let request =
        obj_get_int(ctx, types, args[1]).map_err(|e| ctx.record_typed(e))? as usize;
    let res = ioctl(ctx, args[0], request as u32, IoctlArg::Raw(val))?;
    Ok(Value::int(
        ctx.arena,
        Some(types.int),
        ctx.roots,
        res as i64,
    ))
}

// ── Method table ─────────────────────────────────────────────────────────

/// The 14 stream fun objects (`stream.zig:566-640` — the
/// `mp_stream_*_obj` exports), as a `MethodEntry` table in the
/// C15a `DICT_METHODS` shape. Sigs are `(min, max)` positionals
/// (`makeFunSig(min, max, false)`); `kw_func` is `None` for
/// all (no keyword args). D-lane stream types build their
/// locals dict from this table (`locals_dict_from_entries`).
/// NOTE: `read1` / `write1` / `readinto1` are NOT static
/// qstrs (absent from `qstrdefs.generated.h` too — the
/// original interns them dynamically at type setup), so D
/// must resolve those three via the runtime pool; the other
/// 11 names are static.
pub const STREAM_METHODS: &[MethodEntry] = &[
    MethodEntry {
        name: "read",
        func: stream_read_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "read1",
        func: stream_read1_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "write",
        func: stream_write_builtin,
        min: 2,
        max: 4,
        kw_func: None,
    },
    MethodEntry {
        name: "write1",
        func: stream_write1_builtin,
        min: 2,
        max: 4,
        kw_func: None,
    },
    MethodEntry {
        name: "readinto",
        func: stream_readinto_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    MethodEntry {
        name: "readinto1",
        func: stream_readinto1_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    MethodEntry {
        name: "readline",
        func: stream_unbuffered_readline_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "readlines",
        func: stream_unbuffered_readlines_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "close",
        func: stream_close_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "__exit__",
        func: stream_exit_builtin,
        min: 4,
        max: 4,
        kw_func: None,
    },
    MethodEntry {
        name: "seek",
        func: stream_seek_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    MethodEntry {
        name: "tell",
        func: stream_tell_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "flush",
        func: stream_flush_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "ioctl",
        func: stream_ioctl_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
];
