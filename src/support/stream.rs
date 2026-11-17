//! The stream protocol (`support/stream.zig` — Zig port of
//! vendor `py/stream.c`).
//!
//! Only the PROTOCOL core lives here: `mp_stream_p_t`
//! resolution, `mp_stream_rw`'s shared loop, `mp_stream_write`,
//! `mp_stream_close` / `mp_stream_seek`, `streamReadall`,
//! `streamReadGeneric`, `streamReadintoWriteGeneric`, the
//! unbuffered readline/readlines/iter helpers, and the 14
//! `STREAM_METHODS` fun objects. No file or socket backend
//! (those are D's `modio` / `file_io`); a backend is any type
//! whose `TypeData.protocol` is `Some` (see `StreamP` in
//! `crate::objects::slots`).
//!
//! Mapping to the Zig (`stream.zig`, line numbers from the
//! 0.9.0 tree):
//! | Rust | Zig |
//! |---|---|
//! | `get_stream` | `getStream` (`.protocol` slot, `None` if absent) |
//! | `get_stream_raise` | `mp_get_stream_raise` — the Zig `@panic`s when the type lacks the protocol; vendor C raises `OSError("stream operation not supported")` and Rust follows the C |
//! | `is_nonblocking_error` | `isNonblockingError` — literally `return false` (`MICROPY_STREAMS_NON_BLOCK=0` under MINIMUM ROM level), so every non-blocking branch in the Zig is dead code here |
//! | `stream_rw_read` / `stream_rw_write` | `mp_stream_rw` — one C function; Rust splits it in two because the buffer is `&mut [u8]` for read and `&[u8]` for write (the loop is shared: `stream_rw_loop`) |
//! | `stream_write` | `mp_stream_write` (Obj-returning) |
//! | `stream_write_adaptor` | `mp_stream_write_adaptor` |
//! | `stream_close` | `mp_stream_close` |
//! | `stream_seek` | `mp_stream_seek` (new offset via `SeekState.result`) |
//! | `stream_readall` | `streamReadall` — calls `stream_p.read` DIRECTLY in 256-byte chunks, NOT `mp_stream_rw` |
//! | `stream_read_generic` | `streamReadGeneric` |
//! | `stream_readinto_write_generic` | `streamReadintoWriteGeneric` |
//! | `stream_unbuffered_readline` | `streamUnbufferedReadline` |
//! | `stream_unbuffered_readlines` | `streamUnbufferedReadlines` |
//! | `stream_unbuffered_iter` | `mp_stream_unbuffered_iter` (`None` = StopIteration) |
//! | `STREAM_METHODS` | the 14 `mp_stream_*_obj` exports |
//!
//! Error convention: a backend's `read` / `write` / `ioctl`
//! returns `PyResult<usize>` — `Ok(n)` is the Zig's byte
//! count, `Err(PyError::PyRaise)` is the Zig's
//! `MP_STREAM_ERROR` + `errcode` (the backend records the
//! `OSError` itself before returning, exactly as
//! `mp_raise_OSError` does). `Err(PyError::OutOfMemory)` is
//! the Zig's allocation failure.
//!
//! `vstr` allocation failures are `PyError::OutOfMemory`
//! (the Zig raises `MemoryError` from `m_malloc`); Rust
//! `Vec` growth that could abort instead goes through
//! `try_reserve_exact` / `try_reserve` so a huge request
//! fails the same way instead of aborting the process.

use crate::compiler::build_options::STR_UNICODE;
use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int, obj_get_type};
use crate::objects::slots::{
    IoctlArg, SeekState, StreamIoctlFn, StreamP, StreamReadFn, StreamWriteFn,
};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};

// ── Constants (`stream.zig` top) ─────────────────────────────────────────

/// `MP_STREAM_RW_READ` (0): read direction of `mp_stream_rw`.
pub const MP_STREAM_RW_READ: u8 = 0;
/// `MP_STREAM_RW_WRITE` (1): write direction of `mp_stream_rw`.
pub const MP_STREAM_RW_WRITE: u8 = 1;
/// `MP_STREAM_RW_ONCE` (2): single backend call, no loop.
pub const MP_STREAM_RW_ONCE: u8 = 2;

/// `MP_STREAM_OP_READ` (1): `get_stream_raise` read requirement.
pub const MP_STREAM_OP_READ: u8 = 1;
/// `MP_STREAM_OP_WRITE` (2): `get_stream_raise` write requirement.
pub const MP_STREAM_OP_WRITE: u8 = 2;
/// `MP_STREAM_OP_IOCTL` (4): `get_stream_raise` ioctl requirement.
pub const MP_STREAM_OP_IOCTL: u8 = 4;

/// `MP_STREAM_FLUSH` (1).
pub const MP_STREAM_FLUSH: u32 = 1;
/// `MP_STREAM_SEEK` (2).
pub const MP_STREAM_SEEK: u32 = 2;
/// `MP_STREAM_CLOSE` (4).
pub const MP_STREAM_CLOSE: u32 = 4;

/// `SEEK_SET` (0).
pub const SEEK_SET: i32 = 0;
/// `SEEK_CUR` (1).
pub const SEEK_CUR: i32 = 1;
/// `SEEK_END` (2).
pub const SEEK_END: i32 = 2;

/// `MP_EINVAL` (22): `streamSeek` with `SEEK_SET` and a
/// negative offset.
pub const MP_EINVAL: i32 = 22;

/// `MP_EAGAIN` (11): `iobase_read_write` when the `readinto` /
/// `write` method returns `None` (vendor `py/modio.c`).
pub const MP_EAGAIN: i32 = 11;

/// `MP_EFBIG` (27): `stringio_write` when the new position
/// overflows `mp_uint_t` (vendor `py/objstringio.c`).
pub const MP_EFBIG: i32 = 27;

/// `DEFAULT_BUFFER_SIZE` (256): `streamReadall`'s chunk size.
pub const DEFAULT_BUFFER_SIZE: usize = 256;

// ── Stream-slot access ───────────────────────────────────────────────────

/// `getStream` (`stream.zig:118-123`): the type's `.protocol`
/// slot, or `None` when the type has none (the Zig unwraps
/// with `.?` and panics; Rust returns `None`).
pub fn get_stream(ctx: &CallCtx, stream: Value) -> Option<StreamP> {
    let t = obj_get_type(ctx.arena, ctx.types, stream)?;
    // The C copies the base type's slots into a subclass type at
    // creation (`mp_obj_new_type`, vendor `py/objtype.c`), so a
    // subclass instance resolves the base's protocol; Rust user
    // types do not inherit `protocol` (`typecreate` sets `None`),
    // so walk the MRO here instead — `class S(io.StringIO)` must
    // `read()` like a `StringIO` (probed 2026-11-18: `S("hello")`
    // `.read()` returns `'hello'`). The header type itself is
    // checked first (ROM types carry the protocol directly; their
    // `mro` may be empty).
    let mut chain: Vec<GcRef> = vec![t];
    if let Obj::Type(td) = ctx.arena.get(t) {
        chain.extend(td.mro.iter().copied());
    }
    for cand in chain {
        if let Obj::Type(td) = ctx.arena.get(cand) {
            if let Some(p) = td.protocol {
                return Some(p);
            }
        }
    }
    None
}

/// `mp_get_stream_raise` (`py/stream.c:75`): the protocol, or
/// an `OSError` ("stream operation not supported") when the
/// type lacks the protocol slot or any requested function is
/// `None`. The Zig `@panic`s here; vendor C raises that
/// `OSError` and Rust follows the C.
pub fn get_stream_raise(ctx: &mut CallCtx, stream: Value, flags: u8) -> PyResult<StreamP> {
    let stream_p = get_stream(ctx, stream);
    let ok = match stream_p {
        Some(p) => {
            let need_read = flags & MP_STREAM_OP_READ != 0;
            let need_write = flags & MP_STREAM_OP_WRITE != 0;
            let need_ioctl = flags & MP_STREAM_OP_IOCTL != 0;
            !(need_read && p.read.is_none()
                || need_write && p.write.is_none()
                || need_ioctl && p.ioctl.is_none())
        }
        None => false,
    };
    match (stream_p, ok) {
        (Some(p), true) => Ok(p),
        _ => {
            ctx.record_raise("OSError", Some("stream operation not supported"));
            Err(PyError::PyRaise)
        }
    }
}

/// `isNonblockingError` (`stream.zig:112-114`): hard-coded
/// `false` (`MICROPY_STREAMS_NON_BLOCK=0` under MINIMUM ROM
/// level). Kept as a function so a future flip edits one
/// place, exactly as the Zig keeps the inline.
#[inline]
pub fn is_nonblocking_error(_errno: i32) -> bool {
    false
}

// ── `mp_stream_rw` ───────────────────────────────────────────────────────

/// The shared loop of `mp_stream_rw` (`py/stream.c:24`): call
/// `op(done, remaining)` until the request is satisfied, a
/// `0` return (EOF / no progress), or an error. `ONCE`
/// returns the first call's count immediately. `op` gets the
/// byte position and the remaining count; the caller slices
/// its buffer. (The Zig's `MP_STREAM_ERROR` + non-blocking
/// branch is dead: `is_nonblocking_error` is `false`, so any
/// error propagates via `?`.)
fn stream_rw_loop<F>(size: usize, flags: u8, mut op: F) -> PyResult<usize>
where
    F: FnMut(usize, usize) -> PyResult<usize>,
{
    let mut remaining = size;
    let mut done = 0usize;
    while remaining > 0 {
        let out_sz = op(done, remaining)?;
        // For read, out_sz == 0 means EOF. For write, it's
        // unspecified what it means, but we don't make any
        // progress, so returning is still the best option.
        if out_sz == 0 {
            return Ok(done);
        }
        if flags & MP_STREAM_RW_ONCE != 0 {
            return Ok(out_sz);
        }
        remaining -= out_sz;
        done += out_sz;
    }
    Ok(done)
}

/// `mp_stream_rw` in the READ direction.
pub fn stream_rw_read(
    ctx: &mut CallCtx,
    stream: Value,
    buf: &mut [u8],
    flags: u8,
) -> PyResult<usize> {
    let stream_p = get_stream_raise(ctx, stream, MP_STREAM_OP_READ)?;
    let read: StreamReadFn = stream_p
        .read
        .expect("get_stream_raise checked MP_STREAM_OP_READ");
    stream_rw_loop(buf.len(), flags, |pos, remaining| {
        read(ctx, stream, &mut buf[pos..pos + remaining])
    })
}

/// `mp_stream_rw` in the WRITE direction.
pub fn stream_rw_write(
    ctx: &mut CallCtx,
    stream: Value,
    buf: &[u8],
    flags: u8,
) -> PyResult<usize> {
    let stream_p = get_stream_raise(ctx, stream, MP_STREAM_OP_WRITE)?;
    let write: StreamWriteFn = stream_p
        .write
        .expect("get_stream_raise checked MP_STREAM_OP_WRITE");
    stream_rw_loop(buf.len(), flags, |pos, remaining| {
        write(ctx, stream, &buf[pos..pos + remaining])
    })
}

// ── `mp_stream_write` / adaptor / close / seek ──────────────────────────

/// `mp_stream_write` (`py/stream.c:221`): one `mp_stream_rw`
/// (error raises `OSError`), returning the byte count as an
/// int. The Zig builds it with `newSmallInt` (no window
/// check); a byte count is always in-window, so `Value::int`
/// is identical.
pub fn stream_write(
    ctx: &mut CallCtx,
    stream: Value,
    buf: &[u8],
    flags: u8,
) -> PyResult<Value> {
    let out_sz = stream_rw_write(ctx, stream, buf, flags)?;
    Ok(Value::int(
        ctx.arena,
        Some(ctx.types.int),
        ctx.roots,
        out_sz as i64,
    ))
}

/// `mp_stream_write_adaptor` (`py/stream.c:229`): the
/// `mp_plat_print` backend — write `buf` to the stream and
/// discard the count (errors still raise).
pub fn stream_write_adaptor(ctx: &mut CallCtx, stream: Value, buf: &[u8]) -> PyResult<()> {
    let _ = stream_write(ctx, stream, buf, MP_STREAM_RW_WRITE)?;
    Ok(())
}

/// `mp_stream_close` (`py/stream.c:411`): `ioctl(CLOSE)`.
pub fn stream_close(ctx: &mut CallCtx, stream: Value) -> PyResult<Value> {
    let stream_p = get_stream_raise(ctx, stream, MP_STREAM_OP_IOCTL)?;
    let ioctl: StreamIoctlFn = stream_p
        .ioctl
        .expect("get_stream_raise checked MP_STREAM_OP_IOCTL");
    ioctl(ctx, stream, MP_STREAM_CLOSE, IoctlArg::None)?;
    Ok(Value::None)
}

/// `mp_stream_seek` (`py/stream.c:193`): `ioctl(SEEK)` with a
/// `SeekState`; returns the new absolute offset the backend
/// wrote into it.
pub fn stream_seek(
    ctx: &mut CallCtx,
    stream: Value,
    offset: i64,
    whence: i32,
) -> PyResult<i64> {
    let stream_p = get_stream_raise(ctx, stream, MP_STREAM_OP_IOCTL)?;
    let ioctl: StreamIoctlFn = stream_p
        .ioctl
        .expect("get_stream_raise checked MP_STREAM_OP_IOCTL");
    let mut state = SeekState::new(offset, whence);
    ioctl(ctx, stream, MP_STREAM_SEEK, IoctlArg::Seek(&mut state))?;
    Ok(state.result)
}

// ── readall + read ───────────────────────────────────────────────────────

/// `streamReadall` (`stream.zig:236-272`): read to EOF in
/// 256-byte chunks, calling `stream_p.read` DIRECTLY (not
/// `mp_stream_rw`). A short read shrinks the next chunk; a
/// full read resets it to 256.
pub fn stream_readall(ctx: &mut CallCtx, stream: Value) -> PyResult<Value> {
    let stream_p = get_stream_raise(ctx, stream, MP_STREAM_OP_READ)?;
    let read: StreamReadFn = stream_p
        .read
        .expect("get_stream_raise checked MP_STREAM_OP_READ");
    let mut out: Vec<u8> = Vec::new();
    let mut total_size = 0usize;
    let mut current_read = DEFAULT_BUFFER_SIZE;
    loop {
        let mut chunk = [0u8; DEFAULT_BUFFER_SIZE];
        let n = read(ctx, stream, &mut chunk[..current_read])?;
        if n == 0 {
            break;
        }
        out.try_reserve_exact(total_size + n)
            .map_err(|_| PyError::OutOfMemory)?;
        out.extend_from_slice(&chunk[..n]);
        total_size += n;
        if n < current_read {
            current_read -= n;
        } else {
            current_read = DEFAULT_BUFFER_SIZE;
        }
    }
    if stream_p.is_text {
        Ok(new_str(ctx, ctx.types, &out))
    } else {
        Ok(new_bytes(ctx, ctx.types, &out))
    }
}

/// `streamReadGeneric` (`stream.zig:272-360`): `read` / `read1`.
/// No size argument, or `sz == -1`, reads all; `sz < -1` is a
/// `MemoryError` (the Zig's `vstr_init_len` with a negative
/// size). The text branch (UTF-8 char counting) passes
/// `MP_STREAM_RW_READ` — NOT the caller's flags — so `read1`
/// on a text stream loops exactly like `read`; the binary
/// branch passes `flags` through (so `read1` honours ONCE).
pub fn stream_read_generic(
    ctx: &mut CallCtx,
    args: &[Value],
    flags: u8,
) -> PyResult<Value> {
    let types = ctx.types;
    if args.len() == 1 {
        return stream_readall(ctx, args[0]);
    }
    let sz = obj_get_int(ctx, types, args[1]).map_err(|e| ctx.record_typed(e))?;
    if sz == -1 {
        return stream_readall(ctx, args[0]);
    }
    if sz < -1 {
        return Err(PyError::OutOfMemory);
    }
    let stream_p = get_stream_raise(ctx, args[0], MP_STREAM_OP_READ)?;
    let sz = sz as usize;
    if STR_UNICODE && stream_p.is_text {
        // Text: read `sz` UNICODE characters, never over-reading.
        // `vstr_init(&vstr, sz)` allocates upfront; failure is a
        // MemoryError.
        let mut out: Vec<u8> = Vec::new();
        out.try_reserve_exact(sz)
            .map_err(|_| PyError::OutOfMemory)?;
        let mut more_bytes = sz;
        let mut last_buf_offset = 0usize;
        let mut chars_left = sz as i64;
        'outer: while more_bytes > 0 {
            let start = out.len();
            out.try_reserve_exact(start + more_bytes)
                .map_err(|_| PyError::OutOfMemory)?;
            out.resize(start + more_bytes, 0);
            let got = stream_rw_read(ctx, args[0], &mut out[start..], MP_STREAM_RW_READ)?;
            if got < more_bytes {
                out.truncate(start + got);
                if got == 0 {
                    break 'outer;
                }
            }
            // Count chars from the bytes just read.
            let mut off = last_buf_offset;
            loop {
                let b = out[off];
                let n = if b & 0x80 == 0 {
                    1
                } else if b & 0xe0 == 0xc0 {
                    2
                } else if b & 0xf0 == 0xe0 {
                    3
                } else if b & 0xf8 == 0xf0 {
                    4
                } else {
                    // Invalid lead byte: counts as a 5-byte char
                    // (the `n = 5` else-arm — live under full).
                    5
                };
                if off + n <= out.len() {
                    off += n;
                    chars_left -= 1;
                    last_buf_offset = off;
                    if off >= out.len() {
                        more_bytes = chars_left as usize;
                        break;
                    }
                } else {
                    more_bytes = (off + n - out.len()) + (chars_left - 1) as usize;
                    break;
                }
            }
        }
        return Ok(new_str(ctx, types, &out));
    }
    // Binary (or non-unicode build): ONE `mp_stream_rw` of
    // `sz` bytes; a short read returns short.
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(sz)
        .map_err(|_| PyError::OutOfMemory)?;
    buf.resize(sz, 0);
    let got = stream_rw_read(ctx, args[0], &mut buf, flags)?;
    buf.truncate(got);
    if stream_p.is_text {
        Ok(new_str(ctx, types, &buf))
    } else {
        Ok(new_bytes(ctx, types, &buf))
    }
}

