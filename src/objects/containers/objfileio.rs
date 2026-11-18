//! FILE-backed stream types: `FileIO` / `TextIOWrapper` + `open()`
//! (`src/modules/file_io.zig`, 538 lines) + the stdio objects
//! (`npy_file_io_init`).
//!
//! Mapping (Zig → Rust):
//! | Zig | Rust |
//! | --- | --- |
//! | `NpyFile { base, fp, is_stdio }` | `Obj::File(FileData)` (`value_data.rs`): `handle` is `Owned(Rc<RefCell<File>>)` for `open()` files or a stdio marker; `is_stdio` is implied by the marker; `closed` is `fp == NULL` |
//! | `npyFileRead` / `npyFileWrite` / `npyFileIoctl` | `fileio_read` / `fileio_write` / `fileio_ioctl` (the `StreamP` fns) |
//! | `npyFilePrint` | `fileio_print`: `"<io.FileIO %d>"` / `"<io.TextIOWrapper %d>"`, fd `-1` when closed |
//! | `npyFileFileno` | `fileio_fileno_builtin` |
//! | `npy_file_locals_dict_table` (13 entries) | `FILEIO_LOCALS` — the 8 stream methods are C16's shared bodies; `fileno` is this lane's; `__del__` reuses the close body; `__enter__` is `identity_builtin` |
//! | `mp_type_fileio` / `mp_type_textio` | `fileio_type_data` / `textio_type_data` (flags `0x0180`, no `make_new` — `slot_index_make_new = 0`; textio has the attr slot, fileio does not) |
//! | `npyTextioAttr` (`.buffer`) | `textio_attr`: serves `.buffer` only for the three stdio objects, via the `FileData.buffer` sibling (stable identity, probed) |
//! | `mp_sys_stdin/stdout/stderr_obj` + `*_buffer_obj` | `build_stdio_objects` (built by the D2 step in `Vm::new`, not here) |
//! | `mpBuiltinOpen` | `open_builtin` (positional) + `open_builtin_kw` (kwargs ignored, as the Zig ignores them) → `open_impl` |
//!
//! Pure `std`: `std::fs::File` + `std::io::{Read, Write, Seek}`
//! for owned files; `std::io::{stdin, stdout, stderr}` for the
//! stdio handles. There is no safe `std` API for an arbitrary fd
//! (`from_raw_fd` is `unsafe`, and the tree is `unsafe`-free),
//! so the fds `open()` returns are the real OS fds
//! (`AsRawFd`) and `os.open`'s fd table (D2's `modos_fd`) keeps
//! the `File`s itself — the two open paths never share handles.
//!
//! Divergences (all deliberate, recorded here + in the result
//! file): (1) `read(<-1)` is `MemoryError` via C16's shared
//! `stream_read_generic` — the reference ABORTS (rc=134,
//! `stream.zig:303` `@intCast` panic); aborting is not a
//! behaviour to mirror. (2) `sys.stdin.seek` always raises
//! `OSError(ESPIPE)` — `std::io::Stdin` has no `Seek` impl;
//! the reference seeks stdin when it is a regular file.
//! (3) `sys.stdin.flush()` is a no-op — glibc `fflush(stdin)`
//! discards buffered input, which safe `std` cannot express.
//! (4) The embedding-host output sink (`output.sink` in
//! `npyFileWrite`) is F1's: with an embedding sink installed
//! on this thread, stdio writes go to the host's write callback
//! (see `fileio_write`); otherwise stdio writes go to the
//! process stdout/stderr directly, as `print()` does today.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;

use crate::memory::gc::CallCtx;
use crate::objects::obj::FLAG_ITER_IS_STREAM;
use crate::objects::primitives::ops::{is_str, static_qstr_id};
use crate::objects::slots::{AttrFn, IoctlArg, MethodEntry, PrintKind, StreamP};
use crate::objects::strings::objstr_a::str_data_pool;
use crate::objects::value::{FileData, FileHandle, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;
use crate::runtime::vm_types::locals_dict_from_entries;
use crate::support::stream::{
    MP_STREAM_CLOSE, MP_STREAM_FLUSH, MP_STREAM_SEEK, SEEK_CUR, SEEK_END, SEEK_SET,
};
use crate::objects::containers::objstringio::identity_builtin;
use crate::support::stream_methods::{
    stream_close_builtin, stream_exit_builtin, stream_flush_builtin, stream_read_builtin,
    stream_readinto_builtin, stream_seek_builtin, stream_tell_builtin,
    stream_unbuffered_readline_builtin, stream_unbuffered_readlines_builtin,
    stream_write_builtin,
};

/// `EBADF` (9): `file_io.zig`'s own constant (`EBADF: c_int = 9`).
const EBADF: i32 = 9;
/// `EINVAL` (22): `file_io.zig`'s own constant.
const EINVAL: i32 = 22;
/// `ESPIPE` (29): seek on a pipe — the stdio seek error. The
/// errno-name table (`objexcept.rs`) leaves 29 unnamed, so
/// `str(OSError(29))` is `"29"`, as on the reference.
const ESPIPE: i32 = 29;
/// `EIO` (5): fallback errno when `std::io::Error` carries no
/// OS code (should not happen for real file I/O).
const EIO: i32 = 5;

/// Map a `std::io::Error` to the `OSError` the C's
/// `mp_raise_OSError(errnoVal())` raises: the raw OS errno
/// when present, `EIO` otherwise.
fn io_err(ctx: &mut CallCtx, e: std::io::Error) -> PyError {
    raise_os_error(ctx, e.raw_os_error().unwrap_or(EIO))
}

// ── slot resolution ───────────────────────────────────────────────────

/// Resolve `obj` to its `Obj::File` slot (`stringio_slot`'s
/// shape, `objstringio.rs:275`). Unlike StringIO this does
/// NOT raise on closed files: the C's read/write/ioctl return
/// `MP_STREAM_ERROR` + `EBADF` when `fp == NULL`, and the
/// closed check belongs in each protocol fn.
fn fileio_slot(ctx: &mut CallCtx, obj: Value) -> PyResult<GcRef> {
    let r = match obj {
        Value::Heap(r) => r,
        _ => return Err(PyError::PyRaise),
    };
    match ctx.arena.get(r) {
        Obj::File(_) => Ok(r),
        _ => Err(PyError::PyRaise),
    }
}

/// The content type of `obj`'s stream: `str` when the header
/// type's protocol (resolved through the MRO) says text
/// (`stringio_is_text`, `objstringio.rs:299`).
fn fileio_is_text(ctx: &CallCtx, obj: Value) -> bool {
    crate::support::stream::get_stream(ctx, obj)
        .map(|p| p.is_text)
        .unwrap_or(false)
}

// ── stream-protocol fns ───────────────────────────────────────────────

/// `npyFileRead` (`file_io.zig:145-157`): `fp == NULL` →
/// `EBADF`; `fread` error → errno (the C also `clearerr`s —
/// Rust has no error flag to clear). A short read is fine:
/// the `mp_stream_rw` loop re-calls until the buffer is full
/// or EOF. Stdin reads through the shared `io::stdin()`
/// handle (its internal `BufReader` mirrors the C stdio
/// buffer); reading stdout/stderr is `EBADF`.
pub fn fileio_read(ctx: &mut CallCtx, obj: Value, buf: &mut [u8]) -> PyResult<usize> {
    let r = fileio_slot(ctx, obj)?;
    let (closed, handle) = match ctx.arena.get(r) {
        Obj::File(d) => (d.closed, d.handle.clone()),
        _ => return Err(PyError::PyRaise),
    };
    if closed {
        return Err(raise_os_error(ctx, EBADF));
    }
    let n = match &handle {
        FileHandle::Owned(rc) => rc.borrow_mut().read(buf),
        FileHandle::Stdin => std::io::stdin().read(buf),
        FileHandle::Stdout | FileHandle::Stderr => {
            return Err(raise_os_error(ctx, EBADF));
        }
        FileHandle::Closed => return Err(raise_os_error(ctx, EBADF)),
    };
    match n {
        Ok(n) => Ok(n),
        Err(e) => Err(io_err(ctx, e)),
    }
}

/// `npyFileWrite` (`file_io.zig:159-176`): `fp == NULL` →
/// `EBADF`; short `fwrite` → errno. One `write` call per
/// invocation (the `mp_stream_rw` loop handles partial
/// writes). Writing stdin is `EBADF`. F1: a stdio stdout/stderr
/// write with an embedding sink installed on this thread goes
/// to the host's write callback (`output.sink`, `output.zig`)
/// instead of the process streams — the sink consumes the whole
/// buffer (it has no return value), so the full length is
/// reported, exactly as the Zig's sink branch does.
pub fn fileio_write(ctx: &mut CallCtx, obj: Value, buf: &[u8]) -> PyResult<usize> {
    let r = fileio_slot(ctx, obj)?;
    let (closed, handle) = match ctx.arena.get(r) {
        Obj::File(d) => (d.closed, d.handle.clone()),
        _ => return Err(PyError::PyRaise),
    };
    if closed {
        return Err(raise_os_error(ctx, EBADF));
    }
    let n = match &handle {
        FileHandle::Owned(rc) => rc.borrow_mut().write(buf),
        FileHandle::Stdout => {
            if crate::support::output::write_sink(1, buf) {
                return Ok(buf.len());
            }
            std::io::stdout().write(buf)
        }
        FileHandle::Stderr => {
            if crate::support::output::write_sink(2, buf) {
                return Ok(buf.len());
            }
            std::io::stderr().write(buf)
        }
        FileHandle::Stdin => return Err(raise_os_error(ctx, EBADF)),
        FileHandle::Closed => return Err(raise_os_error(ctx, EBADF)),
    };
    match n {
        Ok(n) => Ok(n),
        Err(e) => Err(io_err(ctx, e)),
    }
}

/// `npyFileIoctl` (`file_io.zig:178-225`): `SEEK` → `fseek`
/// + `ftell` (bad whence → `EINVAL`; `fseek` failure →
/// errno; `ftell < 0` → errno); `FLUSH` → `fflush`;
/// `CLOSE` → `fclose` unless stdio, `fp = NULL`, returns 0
/// even when already closed; anything else → `EINVAL`.
/// Seeking stdio always raises `ESPIPE` (see module doc).
pub fn fileio_ioctl(
    ctx: &mut CallCtx,
    obj: Value,
    request: u32,
    arg: IoctlArg<'_>,
) -> PyResult<usize> {
    let r = fileio_slot(ctx, obj)?;
    match request {
        MP_STREAM_SEEK => {
            let IoctlArg::Seek(state) = arg else {
                return Err(PyError::PyRaise);
            };
            let (closed, handle) = match ctx.arena.get(r) {
                Obj::File(d) => (d.closed, d.handle.clone()),
                _ => return Err(PyError::PyRaise),
            };
            if closed {
                return Err(raise_os_error(ctx, EBADF));
            }
            let seek = match state.whence {
                SEEK_SET => SeekFrom::Start(state.offset as u64),
                SEEK_CUR => SeekFrom::Current(state.offset),
                SEEK_END => SeekFrom::End(state.offset),
                _ => return Err(raise_os_error(ctx, EINVAL)),
            };
            let pos = match &handle {
                FileHandle::Owned(rc) => rc.borrow_mut().seek(seek),
                // No safe `std` Seek for stdio handles.
                FileHandle::Stdin | FileHandle::Stdout | FileHandle::Stderr => {
                    return Err(raise_os_error(ctx, ESPIPE));
                }
                FileHandle::Closed => return Err(raise_os_error(ctx, EBADF)),
            };
            match pos {
                Ok(pos) => {
                    state.offset = pos as i64;
                    Ok(0)
                }
                Err(e) => Err(io_err(ctx, e)),
            }
        }
        MP_STREAM_FLUSH => {
            let (closed, handle) = match ctx.arena.get(r) {
                Obj::File(d) => (d.closed, d.handle.clone()),
                _ => return Err(PyError::PyRaise),
            };
            if closed {
                return Err(raise_os_error(ctx, EBADF));
            }
            let res = match &handle {
                FileHandle::Owned(rc) => rc.borrow_mut().flush(),
                FileHandle::Stdout => std::io::stdout().flush(),
                FileHandle::Stderr => std::io::stderr().flush(),
                // glibc `fflush(stdin)` discards buffered input;
                // safe `std` cannot express it — no-op.
                FileHandle::Stdin => Ok(()),
                FileHandle::Closed => return Err(raise_os_error(ctx, EBADF)),
            };
            match res {
                Ok(()) => Ok(0),
                Err(e) => Err(io_err(ctx, e)),
            }
        }
        MP_STREAM_CLOSE => {
            let Obj::File(data) = ctx.arena.get_mut(r) else {
                return Err(PyError::PyRaise);
            };
            if !data.closed {
                data.closed = true;
                // Replacing the handle drops the `Rc` — the
                // `File` closes NOW (the C's `fclose`), not
                // when the GC collects the object. Stdio
                // handles are markers, not owned files: the
                // fd stays open (the C's `is_stdio` guard).
                if matches!(data.handle, FileHandle::Owned(_)) {
                    data.handle = FileHandle::Closed;
                }
            }
            Ok(0)
        }
        _ => Err(raise_os_error(ctx, EINVAL)),
    }
}

// ── print / fileno / attr ─────────────────────────────────────────────

/// `npyFilePrint` (`file_io.zig:228-234`): `"<io.%s %d>"`
/// with the header type's name and `fileno(fp)` — `-1` when
/// closed (`fp == NULL`). The name follows `is_text`, as
/// `stringio_print` does.
pub fn fileio_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let (fd, is_text) = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::File(d) => (if d.closed { -1 } else { d.fd }, d.is_text),
            _ => (-1, false),
        },
        _ => (-1, fileio_is_text(ctx, value)),
    };
    let _ = if is_text {
        sink.write_str(&format!("<io.TextIOWrapper {fd}>"))
    } else {
        sink.write_str(&format!("<io.FileIO {fd}>"))
    };
}

/// `npyFileFileno` (`file_io.zig:240-249`): `fp == NULL` →
/// `OSError(EBADF)` (polled); else the descriptor number.
pub fn fileio_fileno_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let r = fileio_slot(ctx, args[0])?;
    let (closed, fd) = match ctx.arena.get(r) {
        Obj::File(d) => (d.closed, d.fd),
        _ => return Err(PyError::PyRaise),
    };
    if closed {
        return Err(raise_os_error(ctx, EBADF));
    }
    Ok(Value::int(
        ctx.arena,
        Some(ctx.types.int),
        ctx.roots,
        i64::from(fd),
    ))
}

/// `npyTextioAttr` (`file_io.zig:315-333`): `.buffer` is
/// served only for the three stdio objects, and returns the
/// stable `FileData.buffer` sibling (`sys.stdin.buffer is
/// sys.stdin.buffer`, probed 2026-11-18). Anything else —
/// including `.buffer` on an `open()`ed file — returns
/// `None`, so the loader falls through to the locals dict
/// and raises `AttributeError`, exactly as the C's
/// `dest[1] = SENTINEL` path does.
pub fn textio_attr(ctx: &mut CallCtx, obj: Value, attr: u32) -> PyResult<Option<Value>> {
    let buffer_id = static_qstr_id(b"buffer").expect("buffer is a static qstr");
    if attr != buffer_id {
        return Ok(None);
    }
    let r = match obj {
        Value::Heap(r) => r,
        _ => return Ok(None),
    };
    match ctx.arena.get(r) {
        Obj::File(d) => Ok(d.buffer.map(Value::Heap)),
        _ => Ok(None),
    }
}

// ── locals dict + types ───────────────────────────────────────────────

/// The 13-entry `npy_file_locals_dict_table`
/// (`file_io.zig:264-279`), in the C's order. The eight
/// stream methods are C16's shared bodies; `fileno` is this
/// lane's; `__del__` is the close body (the C binds the same
/// `mp_stream_close_obj` twice); `__enter__` is
/// `mp_identity_obj`; `__exit__` is C16's
/// `mp_stream___exit___obj`. NOTE the absences vs the
/// 14-entry `STREAM_METHODS`: no `read1` / `write1` /
/// `readinto1` / `ioctl` — the C's table does not have them.
pub const FILEIO_LOCALS: &[MethodEntry] = &[
    MethodEntry {
        name: "read",
        func: stream_read_builtin,
        min: 1,
        max: 2,
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
        name: "write",
        func: stream_write_builtin,
        min: 2,
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
        name: "fileno",
        func: fileio_fileno_builtin,
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
        name: "__del__",
        func: stream_close_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "__enter__",
        func: identity_builtin,
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
];

fn fileio_type_data_inner(dict: GcRef, is_text: bool, attr: Option<AttrFn>) -> TypeData {
    let name: &[u8] = if is_text { b"TextIOWrapper" } else { b"FileIO" };
    TypeData {
        name: static_qstr_id(name).expect("FileIO/TextIOWrapper is a static qstr"),
        flags: FLAG_ITER_IS_STREAM,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        // No `getiter` slot: `FLAG_ITER_IS_STREAM` makes the
        // getiter dispatch return the stream itself (C11/C16).
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx,
             value: Value,
             kind: PrintKind,
             sink: &mut dyn std::fmt::Write| {
                fileio_print(ctx, value, kind, sink)
            },
        ),
        attr,
        // `slot_index_make_new = 0`: the C types have no
        // constructor — `FileIO` is not even in the `io`
        // module's globals (probed: `io.FileIO` →
        // AttributeError), and `type(sys.stdin)()` raises
        // through the VM's generic "can't create instance".
        make_new: None,
        bases: None,
        namedtuple_fields: None,
        protocol: Some(StreamP::new(
            Some(fileio_read),
            Some(fileio_write),
            Some(fileio_ioctl),
            is_text,
        )),
    }
}

/// `mp_type_fileio` (`file_io.zig:345-373`): flags
/// `FLAG_ITER_IS_STREAM`; slots print/protocol/locals_dict;
/// `is_text = false`; NO attr slot.
pub fn fileio_type_data(dict: GcRef) -> TypeData {
    fileio_type_data_inner(dict, false, None)
}

/// `mp_type_textio` (`file_io.zig:377-427`): same as fileio,
/// `is_text = true`, plus the attr slot (`.buffer`, under
/// full — `has_buffer = build_options.is_full`).
pub fn textio_type_data(dict: GcRef) -> TypeData {
    fileio_type_data_inner(dict, true, Some(textio_attr))
}

/// Build the two stream types on one shared locals dict (the
/// C uses the same `npy_file_locals_dict` for both) and
/// return `(fileio, textio)`. The caller (`build_vm_types`)
/// stores them in `BuiltinTypes` and adds them to the
/// header-type fix-up list (they are allocated with a `None`
/// header here, like every other type in that builder); the
/// types are pushed to `ctx.roots` here (the
/// `build_io_module` pattern).
pub fn build_fileio_types(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
) -> (GcRef, GcRef) {
    let locals = locals_dict_from_entries(ctx, fun_builtin_var, None, FILEIO_LOCALS, &[], &[]);
    let fileio = ctx.alloc(None, Obj::Type(fileio_type_data(locals)));
    let textio = ctx.alloc(None, Obj::Type(textio_type_data(locals)));
    ctx.roots.push(Value::Heap(fileio));
    ctx.roots.push(Value::Heap(textio));
    (fileio, textio)
}

// ── stdio objects ─────────────────────────────────────────────────────

/// Allocate one `Obj::File` with the given handle, fd and
/// header type. `buffer` is the `.buffer` sibling for stdio
/// text objects, `None` otherwise. `finaliser` sets the GC
/// finaliser flag (`file_io.zig:510-511` — only `open()` files
/// are allocated with `m_malloc_with_finaliser`, so a dropped
/// file is closed by the collector; stdio objects are not
/// flagged and are rooted forever by the sys module).
fn new_file_object(
    ctx: &mut CallCtx,
    handle: FileHandle,
    fd: i32,
    is_text: bool,
    type_: GcRef,
    buffer: Option<GcRef>,
    finaliser: bool,
) -> Value {
    let obj = Obj::File(FileData {
        handle,
        fd,
        is_text,
        closed: false,
        buffer,
    });
    if finaliser {
        Value::Heap(ctx.alloc_with_finaliser(Some(type_), obj))
    } else {
        Value::Heap(ctx.alloc(Some(type_), obj))
    }
}

/// Build the six stdio objects (`npy_file_io_init`,
/// `file_io.zig:469-487`): `stdin`/`stdout`/`stderr` as
/// `TextIOWrapper` over fds 0/1/2, each with its `.buffer`
/// `FileIO` sibling over the same fd. Returns
/// `(stdin, stdout, stderr)`; the siblings are reachable
/// only through the text objects' `buffer` field (traced by
/// `Obj::children()`), so the caller must pin the three
/// text objects (the sys module does).
pub fn build_stdio_objects(
    ctx: &mut CallCtx,
    fileio: GcRef,
    textio: GcRef,
) -> (Value, Value, Value) {
    let mut one = |handle: FileHandle, fd: i32| -> Value {
        let buffer = new_file_object(ctx, handle.clone(), fd, false, fileio, None, false);
        let buffer_ref = match buffer {
            Value::Heap(r) => r,
            _ => unreachable!("new_file_object allocates a heap object"),
        };
        new_file_object(ctx, handle, fd, true, textio, Some(buffer_ref), false)
    };
    let stdin = one(FileHandle::Stdin, 0);
    let stdout = one(FileHandle::Stdout, 1);
    let stderr = one(FileHandle::Stderr, 2);
    ctx.roots.push(stdin);
    ctx.roots.push(stdout);
    ctx.roots.push(stderr);
    (stdin, stdout, stderr)
}

// ── open() ────────────────────────────────────────────────────────────

/// `mp_obj_str_get_str` on a path argument: str/bytes/qstr
/// → bytes; anything else → bare `TypeError` (polled).
/// `open(123)` raises `TypeError ''` on the reference.
fn path_bytes(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
    match str_data_pool(ctx.arena, ctx.pool, v) {
        Some(b) => Ok(b),
        None => {
            ctx.record_raise("TypeError", None);
            Err(PyError::PyRaise)
        }
    }
}

/// Parse a glibc `fopen` mode string (`file_io.zig` passes
/// the mode straight to `fopen`). The first byte must be
/// `r`/`w`/`a` (anything else, including empty, → `EINVAL`,
/// probed); later bytes: `+` adds the other direction, `b`
/// selects binary, `x` selects exclusive creation (only
/// honoured with `w`/`a` — `"rx"` opens an existing file,
/// probed), `t` is ignored, and every other byte is ignored
/// too (`"rz"`, `"rw"`, `"r "` all open, probed). The mode
/// is a C string: truncate at the first NUL byte.
fn parse_fopen_mode(mode: &[u8]) -> Result<(std::fs::OpenOptions, bool), i32> {
    let mode: &[u8] = match mode.iter().position(|b| *b == 0) {
        Some(i) => &mode[..i],
        None => mode,
    };
    let first = match mode.first() {
        Some(b) => *b,
        None => return Err(EINVAL),
    };
    let mut opts = std::fs::OpenOptions::new();
    let mut binary = false;
    let mut plus = false;
    let mut exclusive = false;
    match first {
        b'r' => {
            opts.read(true);
        }
        b'w' => {
            opts.write(true).create(true).truncate(true);
        }
        b'a' => {
            opts.write(true).create(true).append(true);
        }
        _ => return Err(EINVAL),
    }
    for b in &mode[1..] {
        match b {
            b'+' => plus = true,
            b'b' => binary = true,
            b'x' => exclusive = true,
            _ => {}
        }
    }
    if plus {
        // `+` opens for update: read AND write, whatever the
        // base mode (glibc: `r+`/`w+`/`a+`).
        opts.read(true).write(true);
    }
    if exclusive && (first == b'w' || first == b'a') {
        opts.create_new(true);
    }
    Ok((opts, binary))
}

/// `mpBuiltinOpen` (`file_io.zig:490-515`): `open(path,
/// mode="r")`. The mode argument is used ONLY when it is a
/// qstr or an exact str (`open(p, 123)` and `open(p, b"rb")`
/// both open in text `"r"` mode, probed); kwargs are ignored
/// (`_ = kwargs`). `fopen` failure → `OSError(errno)`.
/// Returns a `TextIOWrapper` (no `b`) or `FileIO` (`b`).
fn open_impl(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.is_empty() {
        // `n_args < 1` → bare `TypeError` (the Zig's
        // `mp_raise_TypeError(null)`); unreachable through
        // `call_fun_builtin` (sig min 1), kept for direct
        // callers + tests.
        ctx.record_raise("TypeError", None);
        return Err(PyError::PyRaise);
    }
    let mut path = path_bytes(ctx, args[0])?;
    // C-string path: truncate at the first NUL (fopen would).
    if let Some(i) = path.iter().position(|b| *b == 0) {
        path.truncate(i);
    }
    let mode_owned: Vec<u8> = match args.get(1) {
        Some(v) if is_str(ctx.arena, ctx.types, *v) => {
            match str_data_pool(ctx.arena, ctx.pool, *v) {
                Some(b) => b,
                None => b"r".to_vec(),
            }
        }
        _ => b"r".to_vec(),
    };
    let (opts, binary) = match parse_fopen_mode(&mode_owned) {
        Ok(parsed) => parsed,
        Err(errno) => return Err(raise_os_error(ctx, errno)),
    };
    let path_os = std::ffi::OsStr::from_bytes(&path);
    let file = match opts.open(path_os) {
        Ok(f) => f,
        Err(e) => return Err(io_err(ctx, e)),
    };
    let fd = file.as_raw_fd();
    let type_ = if binary {
        ctx.types.fileio
    } else {
        ctx.types.textio
    };
    Ok(new_file_object(
        ctx,
        FileHandle::Owned(std::rc::Rc::new(std::cell::RefCell::new(file))),
        fd,
        !binary,
        type_,
        None,
        true,
    ))
}

/// `mp_builtin_open_obj` (positional half): `open(path,
/// mode="r")`. D1 binds this as the `func` of the builtin
/// (sig 1–0xffff); the kwargs half is `open_builtin_kw`.
pub fn open_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    open_impl(ctx, args)
}

/// `mp_builtin_open_obj` (kwargs half): same as
/// `open_builtin`, ignoring the keyword pairs — the Zig's
/// `mpBuiltinOpen` takes `kwargs` and discards them
/// (`open(p, encoding="utf-8")` works, probed). D1 binds
/// this as the `kw_func` so `call_fun_builtin` routes
/// kwargs calls here instead of rejecting them.
pub fn open_builtin_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    _pairs: &[(u32, Value)],
) -> PyResult<Value> {
    open_impl(ctx, args)
}
