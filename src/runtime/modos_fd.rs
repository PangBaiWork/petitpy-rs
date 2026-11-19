//! Lane D2: the fd-level `os` builtins (`modos.zig`
//! `py_os_open` / `py_os_close` / `py_os_read` /
//! `py_os_write` / `py_os_lseek` / `py_os_fsync`).
//!
//! The `func`s in the os dict are stubs (allocated by
//! `fd_stub`); the real bodies here run through the
//! `call_function_ctx` interception — they need
//! `state.os_fds` (the fd table), which a plain `BuiltinFn`
//! cannot reach.
//!
//! The fd table (`VmState.os_fds`) maps a VM-opened fd to its
//! `std::fs::File`. `os.open` inserts (the fd comes from the
//! OS via `AsRawFd`, so it never collides with 0/1/2);
//! `os.close` removes (dropping the `File` closes the fd for
//! real). There is no safe std API for an arbitrary fd
//! (`from_raw_fd` is `unsafe`, banned), so fds 0/1/2 are
//! handled inline via `std::io::{stdin, stdout, stderr}`:
//!
//! | fd | read | write | lseek | fsync | close |
//! |---|---|---|---|---|---|
//! | 0 (stdin) | `stdin().read` | EBADF | ESPIPE on pipes/ttys, real `lseek(2)` under `unsafe-ffi` (G6a U9) | EINVAL on pipes/ttys, real `fsync(2)` under `unsafe-ffi` (G6a U9) | real `close(2)` under `unsafe-ffi`, else tracked in `os_closed_stdio` (G6a U3) |
//! | 1 (stdout) | EBADF | `stdout().write` loop | ESPIPE on pipes/ttys, real `lseek(2)` under `unsafe-ffi` (G6a U9) | EINVAL on pipes/ttys, real `fsync(2)` under `unsafe-ffi` (G6a U9) | real `close(2)` under `unsafe-ffi`, else tracked (G6a U3) |
//! | 2 (stderr) | EBADF | `stderr().write` loop | ESPIPE on pipes/ttys, real `lseek(2)` under `unsafe-ffi` (G6a U9) | EINVAL on pipes/ttys, real `fsync(2)` under `unsafe-ffi` (G6a U9) | real `close(2)` under `unsafe-ffi`, else tracked (G6a U3) |
//! | owned | table `read` (once) | table `write` loop | `seek` | `sync_all` | remove from the table |
//! | unknown/closed | EBADF | EBADF | EBADF | EBADF | real `close(2)` under `unsafe-ffi` (G6a U3), else EBADF |
//!
//! Notes: on the default build `os.close(0/1/2)` cannot close the
//! real fd in safe Rust — the fd is recorded in `os_closed_stdio`
//! instead, so every later use raises EBADF (the observable
//! behavior); the only divergence is that a later `os.open` will
//! NOT reuse the number (the reference's `close(0)` frees it for
//! reuse — see §4). Under `--features unsafe-ffi` (G6a) the real
//! `close(2)` runs first and the record is kept, so the number IS
//! reused, exactly like the reference. `os.fsync` on stdio returns
//! EINVAL on the default build: the reference calls `fsync(2)`,
//! which returns EINVAL on the pipes/ttys stdout is in the test
//! environment; under `unsafe-ffi` the real `fsync(2)` runs, so a
//! stdio fd redirected to a regular file syncs (U9). `os.lseek`
//! on stdio is always ESPIPE on the default build
//! (`std::io::Stdin` has no `Seek` — the same divergence
//! `sys.stdin.seek` already records); under `unsafe-ffi` the
//! real `lseek(2)` runs, so pipes/ttys still give ESPIPE from
//! the kernel while regular files seek (U9). A file object
//! holds the RAW fd number on both sides, so `os.close` on an
//! `open()` file's fd leaves the object stale by number — a
//! later write/flush through it goes to whatever reused the
//! number, exactly like the reference (probed, G6a).

/// The six `os` builtins that need VM state (the fd table),
/// for the `call_function_ctx` interception (D2). The `func`s
/// are stubs; the real bodies live below in this file.
#[derive(Clone, Copy, Debug)]
pub struct OsFdBuiltins {
    pub open: Value,
    pub close: Value,
    pub read: Value,
    pub write: Value,
    pub lseek: Value,
    pub fsync: Value,
}

use crate::memory::gc::CallCtx;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::strings::objstr_a::{new_bytes, str_data_pool, StrError};
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::modos::{arg_int, os_err, path_arg, EINVAL, EBADF, EFBIG};
use crate::runtime::raise::raise_os_error;
use crate::runtime::vm::VmState;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// Synthetic fd allocator for Windows: the VM's fd table
/// (`VmState.os_fds`) is keyed by the real fd on Unix
/// (`File::as_raw_fd`); Windows has no small-int fds (a
/// `File` exposes a `HANDLE`), so Windows files get a
/// monotonically increasing synthetic id instead. The
/// counter is process-global (an `AtomicI32`) because the
/// file-object constructor (`objfileio`) has no `VmState`
/// handle — only `CallCtx` — while `os.open` registers
/// into the same table; ids never repeat within a process,
/// so a `fileno()` can never collide with a live entry.
/// (On Unix this is unused: fds come from the kernel.)
#[cfg(not(unix))]
static NEXT_WIN_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(100);

/// Allocate the next synthetic Windows fd (see `NEXT_WIN_FD`).
#[cfg(not(unix))]
pub(crate) fn alloc_win_fd() -> i32 {
    NEXT_WIN_FD.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Allocate one of the six fd builtins in the os dict — the
/// `func` is a stub (intercepted in `call_function_ctx`).
pub fn fd_stub(
    ctx: &mut CallCtx,
    fun_builtin_var: crate::objects::value::GcRef,
    name: &str,
    min: u16,
    max: u16,
) -> Value {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("os fd builtin name is not a static qstr: {}", name));
    Value::Heap(ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(min, max),
            func: fd_stub_fn,
            kw_func: None,
        }),
    ))
}

/// The stub `func` for an intercepted fd builtin — a bare
/// raise, like the other intercepted stubs.
fn fd_stub_fn(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// The arity check the six intercepted bodies share
/// (`call_function_ctx` bypasses the normal dispatch, so the
/// sig is checked here — the same message the dispatcher
/// raises).
fn check_arity(
    ctx: &mut CallCtx,
    state: &VmState,
    n_args: usize,
    n_kw: usize,
    min: usize,
    max: usize,
) -> Result<(), Value> {
    if n_kw != 0 || n_args < min || n_args > max {
        return Err(crate::runtime::vm_ops::type_error(
            ctx,
            state,
            "argument num/types mismatch",
        ));
    }
    Ok(())
}

/// `py_os_open` (`modos.zig:292-299`): `open(path, flags[,
/// mode])`, mode default `0o777`. The access mode comes from
/// `flags & 3`; every other flag is passed through verbatim
/// via `custom_flags` (so `O_CLOEXEC` / `O_NONBLOCK` /
/// `O_DIRECTORY` behave exactly like the C call, and
/// `O_EXCL` without `O_CREAT` is ignored, as `open(2)`
/// ignores it — probed).
pub fn os_open_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 2, 3)?;
    let path = path_arg(ctx, args[0]).map_err(|e| exc(ctx, state, e))?;
    let flags = arg_int(ctx, args[1]).map_err(|e| exc(ctx, state, e))?;
    let mode: u32 = match args.get(2) {
        Some(v) => arg_int(ctx, *v).map_err(|e| exc(ctx, state, e))? as u32,
        None => 0o777,
    };
    // The mode is parsed (and validated) on every platform,
    // but only applied on Unix — Windows has no per-create
    // permission bits.
    #[cfg(not(unix))]
    let _ = mode;
    let mut opts = std::fs::OpenOptions::new();
    match flags & 0b11 {
        0 => {
            opts.read(true);
        }
        1 => {
            opts.write(true);
        }
        _ => {
            opts.read(true).write(true);
        }
    }
    // `O_CLOEXEC` / `O_NONBLOCK` / `O_DIRECTORY` have no std
    // equivalent; on Windows the raw-flags call does not exist
    // at all (no `OpenOptionsExt`), so the flags beyond r/w/a
    // are ignored there.
    #[cfg(unix)]
    opts.custom_flags(flags as i32);
    #[cfg(unix)]
    opts.mode(mode);
    let file = opts
        .open(crate::support::os_str::os_string_from_bytes(&path))
        .map_err(|e| io_exc(ctx, state, e))?;
    // The fd is the real kernel fd on Unix; on Windows it is
    // a synthetic id from the process-global counter (see
    // `alloc_win_fd`) — `os.close`/`read`/`write`/`lseek`/
    // `fsync` all go through the same table, so the key kind
    // is invisible to them.
    #[cfg(unix)]
    let fd = file.as_raw_fd();
    #[cfg(not(unix))]
    let fd = alloc_win_fd();
    state.os_fds.insert(fd, file);
    Ok(Value::SmallInt(i64::from(fd)))
}

/// Map a `PyError` from a `BuiltinFn`-shaped helper into the
/// `Value` exception the interception returns (the pending
/// raise recorded by the helper drains to the exception
/// object).
fn exc(ctx: &mut CallCtx, state: &VmState, e: PyError) -> Value {
    crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e)
}
/// An `OSError(errno)` exception `Value`, built in two
/// sequential steps — a nested `exc(ctx, state,
/// raise_os_error(ctx, n))` double-borrows `ctx` inside a
/// closure.
fn errno_exc(ctx: &mut CallCtx, state: &VmState, n: i32) -> Value {
    let pe = raise_os_error(ctx, n);
    exc(ctx, state, pe)
}

/// An `OSError` from a `std::io::Error`, built the same way.
fn io_exc(ctx: &mut CallCtx, state: &VmState, e: std::io::Error) -> Value {
    let pe = os_err(ctx, &e);
    exc(ctx, state, pe)
}


/// `py_os_close` (`modos.zig:311-315`): `close(fd)` straight
/// through, errno on failure. Under `--features unsafe-ffi`
/// (G6a) the real `close(2)` runs for every fd — an unregistered
/// fd (e.g. an `open()` file's fd) and stdio 0–2 included. On
/// the default build the old shape stays: stdio is only
/// recorded in `os_closed_stdio` (the fd number is NOT freed
/// for reuse) and an unregistered fd is EBADF.
pub fn os_close_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 1, 1)?;
    let fd = arg_int(ctx, args[0]).map_err(|e| exc(ctx, state, e))? as i32;
    if (0..=2).contains(&fd) {
        if state.os_closed_stdio.contains(&fd) {
            return Err(errno_exc(ctx, state, EBADF));
        }
        #[cfg(feature = "unsafe-ffi")]
        {
            // FFI invariant: `close` takes any i32 fd number;
            // the kernel fails EBADF on an already-closed fd, it
            // cannot fault. A failure here raises exactly like the
            // Zig's `close(fd) < 0` branch.
            if let Err(e) = crate::support::ffi::sys_close(fd) {
                return Err(errno_exc(ctx, state, e));
            }
        }
        state.os_closed_stdio.insert(fd);
        return Ok(Value::None);
    }
    // Feature path (G6a): the reference closes by NUMBER
    // (`close(fd)`), with no fd table — so a registered fd whose
    // number was already closed out from under it (e.g. by an
    // `open()` file's `close()`) fails EBADF there. Table removal
    // alone would return Ok, so run the real close first: on
    // failure raise the errno (the dead entry stays in the
    // table).
    #[cfg(feature = "unsafe-ffi")]
    {
        // FFI invariant: `close` takes any i32 fd number; the
        // kernel fails EBADF on a bad fd, it cannot fault.
        if let Err(e) = crate::support::ffi::sys_close(fd) {
            return Err(errno_exc(ctx, state, e));
        }
        // The fd is already closed by `sys_close` above, so
        // the table `File` must NOT be dropped: its Drop
        // would close the number a SECOND time. The G6a
        // comment assumed that re-close's EBADF was ignored
        // — but the debug runtime aborts on it ("IO Safety
        // violation: owned file descriptor already closed",
        // rc=134; a bare `os.open` + `os.close` aborted
        // before this fix), and in release the second close
        // could land on a reused fd number. `into_raw_fd`
        // consumes the `File` without closing (safe: it only
        // transfers ownership of the number, which is
        // already closed). H-select fix for a pre-existing
        // G6a bug, found by the feature-build probes.
        if let Some(f) = state.os_fds.remove(&fd) {
            #[cfg(unix)]
            {
                use std::os::unix::io::IntoRawFd;
                let raw = f.into_raw_fd();
                debug_assert_eq!(raw, fd);
            }
            #[cfg(not(unix))]
            {
                drop(f);
            }
        }
        // H-select: the monitor's read-ahead bytes belong to
        // a closed description — mark it stale (and clear the
        // buffer) so a later file reusing this fd number
        // starts fresh.
        crate::support::readiness::note_closed(fd);
        return Ok(Value::None);
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        match state.os_fds.remove(&fd) {
            Some(_) => {
                crate::support::readiness::note_closed(fd);
                Ok(Value::None)
            }
            None => Err(errno_exc(ctx, state, EBADF)),
        }
    }
}

/// `py_os_read` (`modos.zig:317-329`).
pub fn os_read_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 2, 2)?;
    let fd = arg_int(ctx, args[0]).map_err(|e| exc(ctx, state, e))? as i32;
    let n = arg_int(ctx, args[1]).map_err(|e| exc(ctx, state, e))?;
    if n < 0 {
        return Err(errno_exc(ctx, state, EINVAL));
    }
    if n == 0 {
        return Ok(new_bytes(ctx, ctx.types, &[]));
    }
    if n > 4096 {
        return Err(errno_exc(ctx, state, EFBIG));
    }
    let mut buf = [0u8; 4096];
    // The table borrow must end before the error mapping
    // touches `state` again, so the I/O runs first and the
    // `io::Result` is mapped afterwards.
    let got: std::io::Result<usize> = match fd {
        0 => {
            if state.os_closed_stdio.contains(&0) {
                return Err(errno_exc(ctx, state, EBADF));
            }
            std::io::stdin().read(&mut buf[..n as usize])
        }
        1 | 2 => return Err(errno_exc(ctx, state, EBADF)),
        _ => match state.os_fds.get_mut(&fd) {
            Some(f) => {
                // H-select: drain the monitor's read-ahead
                // first (it already consumed those bytes from
                // the shared offset — an OS read now would
                // skip them); read the remainder from the OS
                // only when no live monitor is left.
                // `os.read` returns what one read yields — a
                // drained buffer counts as the read.
                let (n0, need_os) = crate::support::readiness::drain_into(
                    fd,
                    &mut buf[..n as usize],
                );
                if !need_os {
                    Ok(n0)
                } else {
                    f.read(&mut buf[n0..n as usize]).map(|n1| n0 + n1)
                }
            }
            None => return Err(errno_exc(ctx, state, EBADF)),
        },
    };
    let got = got.map_err(|e| io_exc(ctx, state, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf[..got]))
}

/// `py_os_write` (`modos.zig:331-343`): loop until all bytes
/// are written (a 0-write breaks, as the Zig's does).
pub fn os_write_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 2, 2)?;
    let fd = arg_int(ctx, args[0]).map_err(|e| exc(ctx, state, e))? as i32;
    let data = match str_data_pool(ctx.arena, ctx.pool, args[1]) {
        Some(b) => b,
        None => {
            let pe = ctx.record_typed(StrError::BadImplicitConversion);
            return Err(exc(ctx, state, pe));
        }
    };
    let mut off = 0usize;
    match fd {
        0 => return Err(errno_exc(ctx, state, EBADF)),
        1 | 2 => {
            if state.os_closed_stdio.contains(&fd) {
                return Err(errno_exc(ctx, state, EBADF));
            }
            let mut out: Box<dyn Write> = if fd == 1 {
                Box::new(std::io::stdout())
            } else {
                Box::new(std::io::stderr())
            };
            while off < data.len() {
                match out.write(&data[off..]) {
                    Ok(0) => break,
                    Ok(got) => off += got,
                    Err(e) => return Err(io_exc(ctx, state, e)),
                }
            }
            // P14: the reference's `os.write` is a raw `write(2)`
            // loop (`modos.zig:331-343`) — unbuffered, so a
            // newline-less write lands immediately. `std`'s
            // `Stdout` is a `LineWriter` that would buffer it
            // until the next `\n` or exit; flush it here so
            // `os.write(1, b'b')` arrives at once, ahead of any
            // `print()` output still sitting in the buffered
            // stdout (which this flush does NOT touch — the
            // buffer sits above `std`'s `Stdout`).
            let _ = out.flush();
        }
        _ => {
            // The table borrow must end before the error
            // mapping touches `state` again: run the write
            // loop first, mapping the `io::Result` after.
            let r: std::io::Result<usize> = match state.os_fds.get_mut(&fd) {
                Some(f) => {
                    let mut written = 0usize;
                    let mut r: std::io::Result<usize> = Ok(0);
                    while written < data.len() {
                        match f.write(&data[written..]) {
                            Ok(0) => break,
                            Ok(got) => written += got,
                            Err(e) => {
                                r = Err(e);
                                break;
                            }
                        }
                    }
                    if r.is_ok() {
                        r = Ok(written);
                    }
                    r
                }
                None => Err(std::io::Error::from_raw_os_error(EBADF)),
            };
            off = r.map_err(|e| io_exc(ctx, state, e))?;
        }
    }
    Ok(Value::SmallInt(off as i64))
}

/// `py_os_lseek` (`modos.zig:345-350`): `lseek(fd, pos, how)`
/// straight through — the kernel is the authority on a bad
/// whence / negative offset (both EINVAL there). Under
/// `--features unsafe-ffi` (G6a) the stdio branch (fd 0–2)
/// calls the real `lseek(2)` too, so a stdio fd redirected
/// to a regular file seeks (U9); on pipes/ttys the kernel
/// returns ESPIPE, as before. The registered-fd arm is
/// unchanged (std `seek`, same outcomes). Default build:
/// stdio is always ESPIPE(29).
pub fn os_lseek_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 3, 3)?;
    let fd = arg_int(ctx, args[0]).map_err(|e| exc(ctx, state, e))? as i32;
    let pos = arg_int(ctx, args[1]).map_err(|e| exc(ctx, state, e))?;
    let whence = arg_int(ctx, args[2]).map_err(|e| exc(ctx, state, e))?;
    if state.os_closed_stdio.contains(&fd) {
        return Err(errno_exc(ctx, state, EBADF));
    }
    if (0..=2).contains(&fd) {
        #[cfg(feature = "unsafe-ffi")]
        {
            // FFI invariant: `lseek` takes an fd number and plain
            // integers; a bad fd fails EBADF, a bad whence fails
            // EINVAL — neither can fault.
            return match crate::support::ffi::sys_lseek(fd, pos, whence as i32) {
                Ok(at) => Ok(Value::SmallInt(at)),
                Err(e) => Err(errno_exc(ctx, state, e)),
            };
        }
        #[cfg(not(feature = "unsafe-ffi"))]
        {
            // No `Seek` on `std::io::Stdin`/`Stdout`/`Stderr` —
            // always ESPIPE (the `sys.stdin.seek` divergence).
            return Err(errno_exc(ctx, state, 29));
        }
    }
    let seek_from = match whence {
        0 => {
            if pos < 0 {
                return Err(errno_exc(ctx, state, EINVAL));
            }
            SeekFrom::Start(pos as u64)
        }
        1 => SeekFrom::Current(pos),
        2 => SeekFrom::End(pos),
        _ => return Err(errno_exc(ctx, state, EINVAL)),
    };
    match state.os_fds.get_mut(&fd) {
        Some(f) => {
            // The table borrow must end before the error
            // mapping touches `state` again.
            let at: std::io::Result<u64> = f.seek(seek_from);
            let at = at.map_err(|e| io_exc(ctx, state, e))?;
            // H-select: the offset moved, so the monitor's
            // read-ahead bytes (from the old offset) are
            // stale — invalidate it.
            crate::support::readiness::note_seek(fd);
            Ok(Value::SmallInt(at as i64))
        }
        None => Err(errno_exc(ctx, state, EBADF)),
    }
}

/// `py_os_fsync` (`modos.zig:352-356`): `fsync(fd)` straight
/// through. Under `--features unsafe-ffi` (G6a) the stdio
/// branch (fd 0–2) calls the real `fsync(2)` too, so a stdio
/// fd redirected to a regular file syncs (U9); on pipes/ttys
/// the kernel returns EINVAL, as before. The registered-fd
/// arm is unchanged (std `sync_all`). Default build: stdio
/// is always EINVAL.
pub fn os_fsync_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 1, 1)?;
    let fd = arg_int(ctx, args[0]).map_err(|e| exc(ctx, state, e))? as i32;
    if state.os_closed_stdio.contains(&fd) {
        return Err(errno_exc(ctx, state, EBADF));
    }
    if (0..=2).contains(&fd) {
        #[cfg(feature = "unsafe-ffi")]
        {
            // FFI invariant: `fsync` takes any i32 fd number; the
            // kernel fails EBADF on a bad fd, it cannot fault.
            return match crate::support::ffi::sys_fsync(fd) {
                Ok(()) => Ok(Value::None),
                Err(e) => Err(errno_exc(ctx, state, e)),
            };
        }
        #[cfg(not(feature = "unsafe-ffi"))]
        {
            // No safe std `fsync` for fd 0/1/2 — EINVAL, the
            // reference's answer on pipes/ttys (see the module docs).
            return Err(errno_exc(ctx, state, EINVAL));
        }
    }
    match state.os_fds.get_mut(&fd) {
        Some(f) => {
            // The table borrow must end before the error
            // mapping touches `state` again.
            let r: std::io::Result<()> = f.sync_all();
            r.map_err(|e| io_exc(ctx, state, e))?;
            Ok(Value::None)
        }
        None => Err(errno_exc(ctx, state, EBADF)),
    }
}
