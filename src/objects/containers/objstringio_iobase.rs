//! `io.IOBase`'s stream protocol fns (vendor `py/modio.c`):
//! `iobase_read` / `iobase_write` / `iobase_ioctl` delegate to
//! the object's `readinto` / `write` / `ioctl` methods (see
//! `objstringio.rs` for the `StringIO` / `BytesIO` protocol
//! fns and `objstringio_types.rs` for the type objects).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objarray::new_bytearray;
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::IoctlArg;
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;
use crate::runtime::runtime::RtError;
use crate::runtime::runtime_call::call_method_n_kw;
use crate::runtime::runtime_method::load_method_maybe;
use crate::support::stream::MP_EAGAIN;
// ── IOBase protocol fns ─────────────────────────────────────────────

/// The `RtError` → `PyError` conversion at the `CallCtx`
/// boundary: `OutOfMemory` passes through, an already-recorded
/// `PyRaise` passes through, anything else is recorded by
/// name/message first (the `opmethods.rs:149` idiom —
/// `RtError::to_pyerror` drops the name/message by design).
fn rt_error(ctx: &mut CallCtx, e: RtError) -> PyError {
    match e {
        RtError::OutOfMemory => PyError::OutOfMemory,
        RtError::PyRaise => PyError::PyRaise,
        RtError::Raised(_) => PyError::PyRaise,
        other => {
            let msg = other.message();
            ctx.record_raise(other.exc_name(), Some(&msg));
            PyError::PyRaise
        }
    }
}

/// `iobase_read_write` (`modio.c:54-75`): load the `readinto`
/// / `write` method (the raising form — a missing method is
/// `AttributeError("no attribute 'readinto'")`, the
/// `runtime_method.rs:199` shape), call it with a bytearray
/// over the caller's buffer, and convert the return:
/// `None` → `OSError(EAGAIN)`; int `>= 0` → that count; int
/// `< 0` → `OSError(-ret)`; anything else → `TypeError`
/// (`mp_obj_get_int`).
///
/// The C wraps the caller's buffer in place (a stack
/// `mp_obj_array_t` over `buf`, no copy); Rust bytearrays own
/// their bytes, so a fresh bytearray of `size` is passed and
/// the returned count is copied back (read only). A method
/// that reads the buffer before writing sees zeros in Rust
/// vs uninitialised memory in C — no test can rely on the
/// C's garbage. A method returning more than `size` is
/// clamped to `size` (the C's behaviour there is memory
/// corruption past the caller's buffer).
fn iobase_read_write(
    ctx: &mut CallCtx,
    obj: Value,
    buf: &[u8],
    size: usize,
    write: bool,
    out: &mut [u8],
) -> PyResult<usize> {
    let (qst, method): (&[u8], &str) = if write {
        (b"write", "write")
    } else {
        (b"readinto", "readinto")
    };
    let attr = static_qstr_id(qst).expect("readinto/write are static qstrs");
    // `next_obj` is only consulted for the `__next__` fast
    // path (`runtime_method.rs:79`) — never for `readinto` /
    // `write` — so `None` is exact here.
    let dest = match load_method_maybe(ctx, obj, attr, Value::None) {
        Ok(Some(d)) => d,
        Ok(None) => {
            let msg = format!("no attribute '{method}'");
            ctx.record_raise("AttributeError", Some(&msg));
            return Err(PyError::PyRaise);
        }
        Err(e) => return Err(rt_error(ctx, e)),
    };
    // The bytearray the method sees: the input bytes for a
    // write, zeros of `size` for a read.
    let items: Vec<u8> = if write {
        buf[..size].to_vec()
    } else {
        vec![0u8; size]
    };
    let ba = new_bytearray(ctx, ctx.types.bytearray, &items);
    // The bytearray must be rooted across the method call: the
    // call allocates, a collection could trigger (the GC stress
    // rule), and `ba` is otherwise reachable only from this
    // stack slot (D6 — every `Value` held across an alloc must
    // be rooted).
    ctx.roots.push(Value::Heap(ba));
    let ret_obj = call_method_n_kw(ctx, 1, 0, &dest, &[Value::Heap(ba)])
        .map_err(|e| rt_error(ctx, e))?;
    if ret_obj == Value::None {
        return Err(raise_os_error(ctx, MP_EAGAIN));
    }
    let ret = obj_get_int(ctx, ctx.types, ret_obj).map_err(|e| match e {
        ObjError::OutOfMemory => PyError::OutOfMemory,
        other => ctx.record_typed(other),
    })?;
    if ret < 0 {
        let errno = (-ret) as i32;
        return Err(raise_os_error(ctx, errno));
    }
    let n = (ret as usize).min(size);
    if !write && n > 0 {
        // Copy the bytes the method wrote back into the
        // caller's buffer (bounded by what the bytearray
        // actually holds — a method may shrink it).
        if let Obj::Array(a) = ctx.arena.get(ba) {
            let have = a.data.len();
            let m = n.min(have);
            out[..m].copy_from_slice(&a.data[..m]);
            return Ok(m);
        }
        return Ok(0);
    }
    Ok(n)
}

/// `iobase_read` (`modio.c:54-75` with `MP_QSTR_readinto`).
pub fn iobase_read(ctx: &mut CallCtx, obj: Value, buf: &mut [u8]) -> PyResult<usize> {
    let size = buf.len();
    iobase_read_write(ctx, obj, &[], size, false, buf)
}

/// `iobase_write` (`modio.c:54-75` with `MP_QSTR_write`).
pub fn iobase_write(ctx: &mut CallCtx, obj: Value, buf: &[u8]) -> PyResult<usize> {
    let size = buf.len();
    let mut scratch: Vec<u8> = Vec::new();
    iobase_read_write(ctx, obj, buf, size, true, &mut scratch)
}

/// `iobase_ioctl` (`modio.c:77-91`): call the `ioctl` method
/// with `(request, arg)` as ints. Unlike `read_write` there
/// is NO `None` branch — `mp_obj_get_int(None)` raises
/// `TypeError`. `arg` is the seek-struct POINTER in the C
/// (meaningless to a Python override); Rust passes `0`.
pub fn iobase_ioctl(
    ctx: &mut CallCtx,
    obj: Value,
    request: u32,
    _arg: IoctlArg<'_>,
) -> PyResult<usize> {
    let attr = static_qstr_id(b"ioctl").expect("ioctl is a static qstr");
    // `next_obj`: see `iobase_read_write` — never consulted
    // for `ioctl`.
    let dest = match load_method_maybe(ctx, obj, attr, Value::None) {
        Ok(Some(d)) => d,
        Ok(None) => {
            ctx.record_raise("AttributeError", Some("no attribute 'ioctl'"));
            return Err(PyError::PyRaise);
        }
        Err(e) => return Err(rt_error(ctx, e)),
    };
    let req = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, i64::from(request));
    let arg = Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, 0);
    let ret_obj = call_method_n_kw(ctx, 2, 0, &dest, &[req, arg])
        .map_err(|e| rt_error(ctx, e))?;
    let ret = obj_get_int(ctx, ctx.types, ret_obj).map_err(|e| match e {
        ObjError::OutOfMemory => PyError::OutOfMemory,
        other => ctx.record_typed(other),
    })?;
    if ret >= 0 {
        Ok(ret as usize)
    } else {
        Err(raise_os_error(ctx, (-ret) as i32))
    }
}

// ── print / getvalue / identity ─────────────────────────────────────
