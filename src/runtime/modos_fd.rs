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
//! | 0 (stdin) | `stdin().read` | EBADF | ESPIPE | EINVAL (see below) | tracked in `os_closed_stdio` |
//! | 1 (stdout) | EBADF | `stdout().write` loop | ESPIPE | EINVAL | tracked |
//! | 2 (stderr) | EBADF | `stderr().write` loop | ESPIPE | EINVAL | tracked |
//! | owned | table `read` (once) | table `write` loop | `seek` | `sync_all` | remove from the table |
//! | unknown/closed | EBADF | EBADF | EBADF | EBADF | EBADF |
//!
//! Notes: `os.close(0/1/2)` cannot close the real fd in safe
//! Rust — the fd is recorded in `os_closed_stdio` instead, so
//! every later use raises EBADF (the observable behavior);
//! the only divergence is that a later `os.open` will NOT
//! reuse the number (the reference's `close(0)` frees it for
//! reuse — see §4). `os.fsync` on stdio returns EINVAL: the
//! reference calls `fsync(2)`, which returns EINVAL on the
//! pipes/ttys stdout is in the test environment, and there is
//! no safe std `fsync` for fd 0/1/2 (see §4 — it diverges
//! when stdout is a regular file). `os.lseek` on stdio is
//! always ESPIPE (`std::io::Stdin` has no `Seek` — the same
//! divergence `sys.stdin.seek` already records).

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
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

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
    opts.custom_flags(flags as i32);
    opts.mode(mode);
    let file = opts
        .open(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| io_exc(ctx, state, e))?;
    let fd = file.as_raw_fd();
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


/// `py_os_close` (`modos.zig:311-315`).
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
        if !state.os_closed_stdio.insert(fd) {
            return Err(errno_exc(ctx, state, EBADF));
        }
        return Ok(Value::None);
    }
    match state.os_fds.remove(&fd) {
        Some(_) => Ok(Value::None),
        None => Err(errno_exc(ctx, state, EBADF)),
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
            Some(f) => f.read(&mut buf[..n as usize]),
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

/// `py_os_lseek` (`modos.zig:345-350`).
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
        // No `Seek` on `std::io::Stdin`/`Stdout`/`Stderr` —
        // always ESPIPE (the `sys.stdin.seek` divergence).
        return Err(errno_exc(ctx, state, 29));
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
            Ok(Value::SmallInt(at as i64))
        }
        None => Err(errno_exc(ctx, state, EBADF)),
    }
}

/// `py_os_fsync` (`modos.zig:352-356`).
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
        // No safe std `fsync` for fd 0/1/2 — EINVAL, the
        // reference's answer on pipes/ttys (see the module docs).
        return Err(errno_exc(ctx, state, EINVAL));
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
