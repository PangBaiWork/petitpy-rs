//! `io.StringIO` / `io.BytesIO` / `io.IOBase` (vendor
//! `py/objstringio.c` + `py/modio.c`).
//!
//! Scope (brief `scripts/vendorc_io_brief.md`): the three
//! in-memory stream types and their protocol fns, the `io`
//! module object (minus `open`, which is D's `file_io`), the
//! `help` builtin's object path is elsewhere
//! (`runtime/builtinhelp.rs`), and `mp_warning`
//! (`runtime/warning.rs`). File/socket backends are D's;
//! module registration into `VmState.builtin_modules` is D1's
//! — `build_io_module` only returns the module object.
//!
//! The read/write/seek/tell/flush/close/readline methods are
//! C16's `STREAM_METHODS` bodies (`support/stream_methods.rs`),
//! NOT re-implemented here: the `StringIO` locals dict is the
//! 11-entry table from `objstringio.c:217-229` (read, readinto,
//! readline, write, seek, tell, flush, close, getvalue,
//! `__enter__`, `__exit__`), and the protocol vtable is the
//! shared `StreamP` (`stringio_stream_p` / `bytesio_stream_p`,
//! `objstringio.c:234-264`).
//!
//! Probed divergences from the reference binary (2026-11-18,
//! all intentional — the C crashes, Rust raises):
//! - `seek(0, 2)` on a closed `StringIO` segfaults the
//!   original (NULL `vstr` deref, `objstringio.c:124`);
//!   Rust raises `ValueError("I/O operation on closed file")`.
//! - `readinto("xx")` (a `str` target) aborts the original
//!   (GPF in the `memcpy`, `objstringio.c:65`); Rust keeps
//!   C16's `TypeError` (buffer refuses `BUFFER_WRITE`).
//! - `io.IOBase()` returns one static singleton in the C
//!   (`modio.c:44,46-52`); Rust allocates a fresh object per
//!   call (CPython-compatible).
//! - `StringIO` of a negative int wraps to a huge unsigned in
//!   the C and `vstr_new` fails; Rust maps the wrapped size
//!   through the `MAX_ALLOC` cap to the same `MemoryError`.
//! - A single allocation over 16 MiB fails in the original
//!   (`src/memory/gc.zig:69` `MAX_ALLOC`); Rust enforces the
//!   same cap on the initial size and on write growth.
//!
//! `Obj::StringIo` is also the payload of `IOBase` instances
//! (always empty): the C's `iobase_make_new` allocates a bare
//! `mp_obj_base_t` with no buffer, and the `IOBase` protocol
//! fns never touch the payload — they delegate to methods.

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::objects::containers::binary::buffer_bytes;
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::primitives::ops::is_exact_int;
use crate::objects::slots::{IoctlArg, PrintKind};
use crate::objects::strings::objstr_a::{new_bytes, new_str, str_data_pool};
use crate::objects::value::{GcRef, Obj, StringIoData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::raise::{malloc_fail, raise_os_error};
use crate::support::stream::{
    get_stream, MP_EFBIG, MP_EINVAL, MP_STREAM_CLOSE, MP_STREAM_FLUSH, MP_STREAM_SEEK, SEEK_CUR,
    SEEK_END, SEEK_SET,
};

/// The allocator's single-allocation cap
/// (`src/memory/gc.zig:69`): a `StringIO` whose initial size,
/// or whose buffer after a write, would exceed 16 MiB raises
/// `MemoryError` — probed 2026-11-18 (`StringIO(2**24)` ok,
/// `StringIO(2**24 + 1)` fails).
pub const MAX_ALLOC: usize = 16 * 1024 * 1024;

/// `ValueError("I/O operation on closed file")`
/// (`check_stringio_is_open`, `objstringio.c:40-45`,
/// live under `MICROPY_CPYTHON_COMPAT`).
const CLOSED_MSG: &str = "I/O operation on closed file";

// ── errors ──────────────────────────────────────────────────────────

/// `stringio_make_new` / `iobase_make_new` failures. The
/// adapter in `objtype/make_new.rs` records these through
/// `CallCtx::record_typed` (the C13b pattern).
#[derive(Clone, Debug)]
pub enum StringIoError {
    /// `MemoryError("memory allocation failed")` — the initial
    /// size, or a write, past `MAX_ALLOC`, or a host
    /// reservation failure.
    OutOfMemory,
    /// `TypeError("object with buffer protocol required")` —
    /// the constructor arg has no buffer (`mp_get_buffer_raise`,
    /// `objstringio.c:195`; probed: `StringIO(True)` /
    /// `StringIO(3.5)` / `StringIO([1])`).
    BufferProtocol,
    /// A wrapped `ObjError` (today only `Overflow` from
    /// `mp_obj_get_int` on a bignum size: `StringIO(2**70)` →
    /// `OverflowError("overflow converting long int to machine
    /// word")`, probed 2026-11-18).
    Obj(ObjError),
    /// A slot already recorded its raise.
    PyRaise,
}

impl StringIoError {
    pub fn exc_name(&self) -> &'static str {
        match self {
            StringIoError::OutOfMemory => "MemoryError",
            StringIoError::BufferProtocol => "TypeError",
            StringIoError::Obj(e) => e.exc_type().map(|t| t.name()).unwrap_or(""),
            StringIoError::PyRaise => "",
        }
    }

    pub fn message(&self) -> Option<&'static str> {
        match self {
            // A `vstr` allocation failure is the C's
            // `m_malloc_fail` (`MemoryError("memory allocation
            // failed")`, `raise.zig:83-90`) — NOT the arena
            // allocator's bare `OutOfMemory` signal, which the
            // VM renders as "out of memory". `to_pyerror`
            // returns `PyRaise` for it so `record_typed`
            // records the name + message (it only records
            // when the signal is `PyRaise`).
            StringIoError::OutOfMemory => Some("memory allocation failed"),
            StringIoError::BufferProtocol => Some("object with buffer protocol required"),
            StringIoError::Obj(e) => e.message(),
            StringIoError::PyRaise => None,
        }
    }

    pub fn to_pyerror(self) -> PyError {
        // See `message`: the OOM case is a recorded
        // `MemoryError`, not the bare allocator signal.
        PyError::PyRaise
    }
}

impl TypedRaise for StringIoError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }

    fn raise_arg(&self) -> Option<Value> {
        None
    }

    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.message().map(|m| m.to_string())
    }

    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
    }
}

impl From<ObjError> for StringIoError {
    fn from(e: ObjError) -> StringIoError {
        match e {
            ObjError::OutOfMemory => StringIoError::OutOfMemory,
            other => StringIoError::Obj(other),
        }
    }
}

impl From<PyError> for StringIoError {
    fn from(e: PyError) -> StringIoError {
        match e {
            PyError::OutOfMemory => StringIoError::OutOfMemory,
            PyError::PyRaise => StringIoError::PyRaise,
        }
    }
}

// ── construction ────────────────────────────────────────────────────

/// The constructor arg's bytes: a `str`/`bytes` (pool-aware,
/// so a `Qstr` arg works) or any other buffer object
/// (`bytearray`, `memoryview`, `array` — `buffer_bytes`).
/// `None` = no buffer protocol.
fn arg_bytes(ctx: &CallCtx, arg: Value) -> Option<Vec<u8>> {
    if let Some(b) = str_data_pool(ctx.arena, ctx.pool, arg) {
        return Some(b);
    }
    buffer_bytes(ctx, arg).map(|(b, _tc)| b)
}

/// `stringio_make_new` (`objstringio.c:180-212`): only the
/// POSITIONAL args are read (`n_pos = args.len() - 2 * n_kw`,
/// the flat layout — `n_args` in the C counts positionals, so
/// a kwargs-only call reads nothing); extra positionals and
/// all kwargs are IGNORED — `(void)n_kw`; probed:
/// `StringIO(initial_value="x")` → `''`,
/// `StringIO("a", "b")` → `"a"`).
/// - no arg → empty buffer (`vstr_new(16)`);
/// - int arg → an empty buffer of that size (negative wraps to
///   a huge unsigned, so it fails at the `MAX_ALLOC` cap —
///   probed: `StringIO(-5)` → `MemoryError`);
/// - anything else → the arg's bytes copied in, `pos` reset to
///   0 (a `bytearray` arg takes the C's copy path, so the
///   original is never aliased — probed 2026-11-18).
pub fn stringio_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, StringIoError> {
    let _ = n_kw;
    let n_pos = args.len().saturating_sub(n_kw * 2);
    let mut buf: Vec<u8> = Vec::new();
    if n_pos > 0 {
        let arg = args[0];
        if is_exact_int(ctx.arena, ctx.types, arg) {
            // `mp_obj_is_int` is exact-int only: `bool` is NOT
            // an int here (probed: `StringIO(True)` →
            // `TypeError`), so `is_exact_int` — NOT
            // `obj_get_int_maybe`, which accepts `bool`.
            let n = obj_get_int(ctx, ctx.types, arg)?;
            // The C assigns to `mp_uint_t`: a negative size
            // wraps to a huge unsigned (`as` casts wrap the
            // same way) and `vstr_new` fails.
            let sz = n as usize;
            if sz > MAX_ALLOC {
                return Err(StringIoError::OutOfMemory);
            }
            buf.try_reserve_exact(sz)
                .map_err(|_| StringIoError::OutOfMemory)?;
        } else {
            let bytes = arg_bytes(ctx, arg).ok_or(StringIoError::BufferProtocol)?;
            if bytes.len() > MAX_ALLOC {
                return Err(StringIoError::OutOfMemory);
            }
            buf = bytes;
        }
    } else {
        buf.try_reserve_exact(16)
            .map_err(|_| StringIoError::OutOfMemory)?;
    }
    let r = ctx.alloc(
        Some(type_),
        Obj::StringIo(StringIoData {
            buf,
            pos: 0,
            closed: false,
        }),
    );
    Ok(Value::Heap(r))
}

/// `iobase_make_new` (`modio.c:46-52`): ignores type, args and
/// kwargs and returns the one static singleton. Rust allocates
/// a fresh empty `StringIo` payload per call instead
/// (CPython-compatible; the singleton is only observable via
/// `is`). The payload is never read: the `IOBase` protocol
/// fns delegate to methods.
pub fn iobase_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    _args: &[Value],
    _n_kw: usize,
) -> Result<Value, StringIoError> {
    let r = ctx.alloc(
        Some(type_),
        Obj::StringIo(StringIoData {
            buf: Vec::new(),
            pos: 0,
            closed: false,
        }),
    );
    Ok(Value::Heap(r))
}

// ── payload access ──────────────────────────────────────────────────

/// Resolve `obj` to its `StringIo` slot and enforce the
/// closed check (`check_stringio_is_open`); returns the slot
/// ref for the caller to mutate. The closed flag is read in a
/// separate borrow so the raise can be recorded without a
/// borrow conflict. A non-`StringIo` payload is unreachable
/// (the protocol lives only on types whose `make_new`
/// creates `StringIo` payloads) and returns a bare `PyRaise`,
/// the array lanes' pattern for the same case.
fn stringio_slot(ctx: &mut CallCtx, obj: Value) -> PyResult<GcRef> {
    let r = match obj {
        Value::Heap(r) => r,
        _ => return Err(PyError::PyRaise),
    };
    let (is_stringio, closed) = match ctx.arena.get(r) {
        Obj::StringIo(d) => (true, d.closed),
        _ => (false, false),
    };
    if !is_stringio {
        return Err(PyError::PyRaise);
    }
    if closed {
        ctx.record_raise("ValueError", Some(CLOSED_MSG));
        return Err(PyError::PyRaise);
    }
    Ok(r)
}

/// The content type of `obj`'s stream: `str` when the header
/// type's protocol (resolved through the MRO, so a subclass
/// instance counts as its base — probed: `class S(StringIO)`
/// `.getvalue()` is `str`, `repr(S())` is `<io.StringIO …>`)
/// has `is_text`, `bytes` otherwise.
fn stringio_is_text(ctx: &CallCtx, obj: Value) -> bool {
    get_stream(ctx, obj).map(|p| p.is_text).unwrap_or(false)
}

// ── StringIO protocol fns ───────────────────────────────────────────

/// `stringio_read` (`objstringio.c:59-70`): `0` at EOF (or
/// seeked past EOF); the closed check runs first.
pub fn stringio_read(ctx: &mut CallCtx, obj: Value, buf: &mut [u8]) -> PyResult<usize> {
    let r = stringio_slot(ctx, obj)?;
    let Obj::StringIo(data) = ctx.arena.get_mut(r) else {
        return Err(PyError::PyRaise);
    };
    if data.buf.len() <= data.pos {
        return Ok(0);
    }
    let remaining = data.buf.len() - data.pos;
    let n = buf.len().min(remaining);
    buf[..n].copy_from_slice(&data.buf[data.pos..data.pos + n]);
    data.pos += n;
    Ok(n)
}

/// `stringio_write` (`objstringio.c:78-107`): grows the buffer
/// with a zero-filled hole when `pos` is past EOF (probed:
/// `seek(5)` on `"ab"` then `write("c")` → `'ab\x00\x00\x00c'`);
/// `pos` overflow → `OSError(EFBIG)`; growth past `MAX_ALLOC`
/// → `MemoryError`.
pub fn stringio_write(ctx: &mut CallCtx, obj: Value, buf: &[u8]) -> PyResult<usize> {
    let r = stringio_slot(ctx, obj)?;
    // Read `pos` / `len` without holding the borrow: the
    // fallible reservation below needs `&mut ctx` for the
    // raise, which conflicts with a live payload borrow.
    let (pos, len) = match ctx.arena.get(r) {
        Obj::StringIo(d) => (d.pos, d.buf.len()),
        _ => return Err(PyError::PyRaise),
    };
    let new_pos = match pos.checked_add(buf.len()) {
        Some(p) => p,
        None => return Err(raise_os_error(ctx, MP_EFBIG)),
    };
    if new_pos > MAX_ALLOC {
        // `vstr` growth past the allocator cap (`m_malloc_fail`).
        return Err(malloc_fail(ctx));
    }
    if new_pos > len {
        let reserved = {
            let Obj::StringIo(data) = ctx.arena.get_mut(r) else {
                return Err(PyError::PyRaise);
            };
            match data.buf.try_reserve_exact(new_pos - len) {
                Ok(()) => {
                    // Zero-fills the hole (and the region about
                    // to be overwritten, which the copy below
                    // replaces).
                    data.buf.resize(new_pos, 0);
                    true
                }
                Err(_) => false,
            }
        };
        if !reserved {
            return Err(malloc_fail(ctx));
        }
    }
    let Obj::StringIo(data) = ctx.arena.get_mut(r) else {
        return Err(PyError::PyRaise);
    };
    data.buf[pos..new_pos].copy_from_slice(buf);
    data.pos = new_pos;
    Ok(buf.len())
}

/// `stringio_ioctl` (`objstringio.c:110-159`). `SEEK`:
/// `SET` → `ref = 0`, `CUR` → `ref = pos`, `END` → `ref =
/// len` — `END` reads the buffer, so on a closed object the C
/// segfaults and Rust raises `ValueError` instead (probed);
/// `SET`/`CUR` work on a closed object (the `pos` field only
/// — probed: `seek(0)` / `seek(1, 1)` after `close()` work).
/// A negative `CUR`/`END` offset past 0 clamps to 0; a
/// positive offset past `mp_uint_t` → `OSError(EINVAL)`.
/// `FLUSH` → `0`; `CLOSE` frees the buffer and sets `closed`
/// but does NOT reset `pos` (probed: `tell()` after `close()`
/// is unchanged; double `close()` returns `None`); anything
/// else → `OSError(EINVAL)`.
pub fn stringio_ioctl(
    ctx: &mut CallCtx,
    obj: Value,
    request: u32,
    arg: IoctlArg<'_>,
) -> PyResult<usize> {
    match request {
        MP_STREAM_SEEK => {
            let IoctlArg::Seek(state) = arg else {
                return Err(PyError::PyRaise);
            };
            let (pos, len, closed) = match obj {
                Value::Heap(r) => match ctx.arena.get(r) {
                    Obj::StringIo(d) => (d.pos, d.buf.len(), d.closed),
                    _ => return Err(PyError::PyRaise),
                },
                _ => return Err(PyError::PyRaise),
            };
            let reference = match state.whence {
                SEEK_SET => 0usize,
                SEEK_CUR => pos,
                SEEK_END => {
                    if closed {
                        ctx.record_raise("ValueError", Some(CLOSED_MSG));
                        return Err(PyError::PyRaise);
                    }
                    len
                }
                _ => 0,
            };
            // Unsigned C arithmetic: the offset wraps on cast.
            let mut new_pos = reference.wrapping_add(state.offset as usize);
            if state.whence != SEEK_SET && state.offset < 0 {
                if new_pos > reference {
                    new_pos = 0;
                }
            } else if new_pos < reference {
                return Err(raise_os_error(ctx, MP_EINVAL));
            }
            state.result = new_pos as i64;
            let Value::Heap(r) = obj else {
                return Err(PyError::PyRaise);
            };
            let Obj::StringIo(data) = ctx.arena.get_mut(r) else {
                return Err(PyError::PyRaise);
            };
            data.pos = new_pos;
            Ok(0)
        }
        MP_STREAM_FLUSH => Ok(0),
        MP_STREAM_CLOSE => {
            let Value::Heap(r) = obj else {
                return Err(PyError::PyRaise);
            };
            let Obj::StringIo(data) = ctx.arena.get_mut(r) else {
                return Err(PyError::PyRaise);
            };
            data.buf = Vec::new();
            data.closed = true;
            Ok(0)
        }
        _ => Err(raise_os_error(ctx, MP_EINVAL)),
    }
}

/// `stringio_print` (`objstringio.c:48-53`):
/// `<io.StringIO 0x{slot:x}>` / `<io.BytesIO 0x{slot:x}>`,
/// chosen by the header type's protocol `is_text` (so a
/// subclass prints as its base — probed 2026-11-18). The
/// address is the arena slot index (the `fun_bc_print`
/// precedent); the C prints the object pointer.
pub fn stringio_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let is_text = stringio_is_text(ctx, value);
    let slot = match value {
        Value::Heap(r) => r.0,
        _ => 0,
    };
    let _ = if is_text {
        sink.write_str(&format!("<io.StringIO 0x{slot:x}>"))
    } else {
        sink.write_str(&format!("<io.BytesIO 0x{slot:x}>"))
    };
}

/// `stringio_getvalue` (`objstringio.c:165-171`): the whole
/// buffer as `str` (text stream) or `bytes` (binary stream),
/// by the header type's protocol `is_text`; the closed check
/// runs first.
pub fn stringio_getvalue(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let self_ = args[0];
    let r = stringio_slot(ctx, self_)?;
    let bytes = match ctx.arena.get(r) {
        Obj::StringIo(d) => d.buf.clone(),
        _ => return Err(PyError::PyRaise),
    };
    if stringio_is_text(ctx, self_) {
        Ok(new_str(ctx, ctx.types, &bytes))
    } else {
        Ok(new_bytes(ctx, ctx.types, &bytes))
    }
}

/// `mp_identity_obj` (`py/obj.c`): returns its argument —
/// the `__enter__` method of the `StringIO` locals dict.
pub fn identity_builtin(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[0])
}
