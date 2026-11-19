//! The `_subprocess` module (Zig
//! `src/modules/mod_subprocess.zig`, 373 lines): the
//! fork+exec+wait+pipes primitive behind
//! `petitpy-lib/subprocess.py` (`run` / `check_output` /
//! `call` / `CompletedProcess` / `CalledProcessError` /
//! `TimeoutExpired`, `PIPE` / `STDOUT` / `DEVNULL` — that
//! Python layer is D5's frozen bundle, not this module).
//!
//! Sole export: `_subprocess._spawn_and_wait(argv, input,
//! timeout, env, cwd, stdin, stdout, stderr) -> (returncode,
//! stdout bytes or None, stderr bytes or None, timed_out)`.
//! `argv` is a list of str; `env` is `None` or a list of
//! `"NAME=value"` strings; `timeout` is seconds (`-1` =
//! none, as the Python layer passes); the three streams
//! take `INHERIT` (-4), `PIPE` (-1), `STDOUT` (-2, stderr
//! only in practice), `DEVNULL` (-3), or a file descriptor
//! (>= 0). Output is read while the child runs, so a child
//! that writes more than a pipe holds does not block.
//!
//! Pure-`std` implementation (`std::process::Command` +
//! reader/writer threads; no `libc` fork/exec, no `poll`).
//! One implementation on every platform and on both
//! builds (Phase H): the `unsafe-ffi` branches (G6b U5/U8)
//! are deleted, so the default and feature builds run the
//! same code here.
//! - Timeout kill: `std`'s `Child::kill` sends `SIGKILL`
//!   (there is no `SIGTERM` in `std`, and CPython's
//!   `Popen.kill()` is `SIGKILL` too), so a timed-out
//!   child reports `returncode -9` with no grace period.
//! - A file-descriptor stream mode (>= 0) dups the fd
//!   (`BorrowedFd::borrow_raw` + `try_clone_to_owned`)
//!   onto the stream: the dup shares the original's
//!   file offset and append mode, and socket fds work —
//!   the `dup2` semantics, without `pre_exec`. A fd that
//!   does not resolve (closed, never opened) fails the
//!   dup with `EBADF` (`OSError(9)`). `borrow_raw` is
//!   `unsafe` in `std`, so this file has exactly one
//!   `unsafe` block (the dup itself, with the invariant
//!   in the `fd_stdio` docs); no FFI, no C. On Windows
//!   the fds are synthetic (`state.os_fds`, which a
//!   `FunBuiltin` cannot see), so only 0/1/2 dup the std
//!   handles and any other fd is `EBADF` (see the result
//!   file §4).
//! - Spawn failure (missing executable, bad `cwd`): the
//!   Zig's child `_exit(127)`s after fork, so the parent
//!   reports `127` with empty captured output; `Command`
//!   reports the failure at spawn, so the errno is mapped
//!   here: `ENOENT` / `EACCES` / `ENOTDIR` / `ELOOP` /
//!   `ENAMETOOLONG` / `ETXTBSY` / `ENOEXEC` become the
//!   `(127, b''|None, b''|None, False)` tuple, anything else
//!   becomes `OSError(errno)` (the Zig's `fork()` failure
//!   shape).
//! - `argv` must be a list. A str `argv` ABORTS the
//!   reference (`incorrect alignment` panic,
//!   `MpObjList.fromObj`); a tuple `argv` ABORTS it too
//!   (`attempt to use null value` panic in `cStrArray`).
//!   This port returns `OSError(EINVAL)` for both, the safe
//!   `std` shape for a bad `argv`.
//! - An env entry without `=` is passed as `(entry, "")`
//!   (the child sees `FOO=`); the Zig passes the bare
//!   string through to `execvpe`.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::io::{AsFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::{obj_get_float, obj_get_int};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::{static_qstr_id, str_bytes_of, QstrResolver};
use crate::objects::strings::objstr_a::new_bytes;
use crate::objects::value::{FunBuiltinData, GcRef, Obj, Value};
use crate::objects::objtype::objmodule::new_module;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

/// Stream modes (`mod_subprocess.zig:119-122`).
const INHERIT: i64 = -4;
const PIPE: i64 = -1;
const STDOUT: i64 = -2;
const DEVNULL: i64 = -3;

/// `OSError(EINVAL)` for a malformed `argv` / `env` /
/// `cwd` (the Zig's `cStrArray` NULL / `mp_obj_str_get_str`
/// NULL shape).
fn einval(ctx: &mut CallCtx) -> PyError {
    raise_os_error(ctx, 22)
}

/// `mp_obj_get_int` (`obj.rs`), mapped through the typed
/// raise (`ObjError::IntConversion` is TypeError "can't
/// convert to int").
fn int_arg(ctx: &mut CallCtx, v: Value) -> PyResult<i64> {
    obj_get_int(ctx, ctx.types, v).map_err(|e| ctx.record_typed(e))
}

/// `mp_obj_get_float` (`obj.rs`), mapped through the typed
/// raise (`ObjError::FloatConversion` is TypeError "can't
/// convert to float").
fn float_arg(ctx: &mut CallCtx, v: Value) -> PyResult<f64> {
    obj_get_float(ctx, ctx.types, v).map_err(|e| ctx.record_typed(e))
}

/// `str` / `bytes` as bytes (`str_bytes_of`: qstr / `str`
/// / `bytes` only). `None` is a TypeError "can't convert
/// to str implicitly".
fn str_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
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
            ctx.record_raise("TypeError", Some("can't convert to str implicitly"));
            Err(PyError::PyRaise)
        }
    }
}

/// `cStrArray` (`mod_subprocess.zig:138-148`): a list of
/// str into `Vec<String>`. A non-list, or a non-str item,
/// is `EINVAL`; an empty list is `EINVAL`
/// (`argv[0] == NULL`). (A tuple aborts the reference —
/// see the module docs — so it is `EINVAL` here, not
/// accepted.)
fn str_list_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<String>> {
    let items = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => l.items.clone(),
            _ => return Err(einval(ctx)),
        },
        _ => return Err(einval(ctx)),
    };
    if items.is_empty() {
        return Err(einval(ctx));
    }
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
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        // No `str_arg` here: it records a TypeError first,
        // and `record_raise` is first-wins, so the EINVAL
        // below would never land.
        let Some(bytes) = str_bytes_of(ctx.arena, ctx.types, resolver, item) else {
            return Err(einval(ctx));
        };
        out.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    Ok(out)
}

/// The `input` argument: `None` -> `None`; str/bytes ->
/// the bytes; anything else is TypeError "can't convert to
/// str implicitly" (the Zig's `mp_obj_str_get_data` raises
/// there).
fn input_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Option<Vec<u8>>> {
    match v {
        Value::None => Ok(None),
        _ => Ok(Some(str_arg(ctx, v)?)),
    }
}

/// The `env` argument: `None` -> `None` (inherit); a list
/// of str -> the entries; anything else is `EINVAL`.
fn env_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Option<Vec<String>>> {
    match v {
        Value::None => Ok(None),
        _ => Ok(Some(str_list_arg(ctx, v)?)),
    }
}

/// The `cwd` argument: `None` -> `None`; str -> the path;
/// anything else is `EINVAL` (the Zig's
/// `mp_obj_str_get_str` NULL shape).
fn cwd_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Option<String>> {
    match v {
        Value::None => Ok(None),
        _ => {
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
            // As in `str_list_arg`: no `str_arg` — its
            // TypeError record would shadow the EINVAL.
            let Some(bytes) = str_bytes_of(ctx.arena, ctx.types, resolver, v) else {
                return Err(einval(ctx));
            };
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
    }
}

/// Resolve a file-descriptor stream mode (>= 0) to a
/// `Stdio` by duping the fd (Phase H: the `pre_exec`
/// `dup2` (U8) is deleted; the dup IS the `dup2`
/// semantics). The dup shares the original's file offset
/// and append mode, and a socket fd dups like any other
/// fd. A fd that does not resolve fails the dup with
/// `EBADF` (`OSError(9)`).
///
/// Unix: `BorrowedFd::borrow_raw` + `try_clone_to_owned`
/// — this covers every fd the runtime holds (os files,
/// sockets, pipes, stdio), without touching the `os_fds`
/// / `SOCKETS` tables. NOTE: `borrow_raw` is `unsafe`
/// in `std` (the brief's "fully safe" premise was wrong
/// — see the result file §4): the single `unsafe` block
/// below is the only one in this file, and it is sound —
/// the borrow lives for one expression and is immediately
/// dup'd (`dup(2)` is safe for any fd value, failing
/// `EBADF` on a closed/never-open fd); the dup'd
/// `OwnedFd` owns the NEW fd number, so no double-close
/// is possible; no code path closes the original fd while
/// the borrow lives (the VM runs this call on one thread
/// and the drain/feed threads never close fds).
///
/// Windows: the fds are synthetic (`state.os_fds`,
/// which a `FunBuiltin` cannot see), so only 0/1/2 dup
/// the std handles; anything else is `EBADF` (see the
/// result file §4).
fn fd_stdio(ctx: &mut CallCtx, fd: i64) -> PyResult<Stdio> {
    #[cfg(unix)]
    {
        let fd32 = i32::try_from(fd).map_err(|_| raise_os_error(ctx, 9))?;
        // SAFETY: see the fn docs — the borrow is dup'd
        // immediately; `dup(2)` fails `EBADF` on a bad fd
        // and the new `OwnedFd` owns only the new number.
        let owned = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd32) }
            .try_clone_to_owned()
            .map_err(|e| io_os_error(ctx, e))?;
        Ok(Stdio::from(std::fs::File::from(owned)))
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::io::AsHandle;
        let owned = match fd {
            0 => std::io::stdin()
                .as_handle()
                .try_clone_to_owned()
                .map_err(|e| io_os_error(ctx, e))?,
            1 => std::io::stdout()
                .as_handle()
                .try_clone_to_owned()
                .map_err(|e| io_os_error(ctx, e))?,
            2 => std::io::stderr()
                .as_handle()
                .try_clone_to_owned()
                .map_err(|e| io_os_error(ctx, e))?,
            _ => return Err(raise_os_error(ctx, 9)),
        };
        Ok(Stdio::from(std::fs::File::from(owned)))
    }
}

/// Build the `Stdio` for one stream mode. `role` is 0 for
/// stdin, 1 for stdout, 2 for stderr. `STDOUT` (-2) dups
/// the parent's stdout (fd 1) onto the child fd — for
/// stdout itself that is just `inherit()`; for stderr it
/// is only correct when the child's stdout IS the parent's
/// (INHERIT) — the other stdout modes go through
/// `stderr_stdio_for` / the shared pair in `spawn_and_wait`
/// (the Zig's `dup2(1, 2)` dups the CHILD's stdout, after
/// it was set up).
fn stdio_for(ctx: &mut CallCtx, mode: i64, role: usize) -> PyResult<Stdio> {
    match mode {
        INHERIT => Ok(Stdio::inherit()),
        PIPE => Ok(Stdio::piped()),
        STDOUT => {
            if role == 1 {
                Ok(Stdio::inherit())
            } else {
                // A dup of the parent's stdout (fd 1 / handle),
                // via `as_fd` / `as_handle` + `try_clone_to_owned`
                // (safe).
                #[cfg(unix)]
                {
                    let owned = std::io::stdout()
                        .as_fd()
                        .try_clone_to_owned()
                        .map_err(|e| io_os_error(ctx, e))?;
                    Ok(Stdio::from(owned))
                }
                #[cfg(not(unix))]
                {
                    use std::os::windows::io::AsHandle;
                    let owned = std::io::stdout()
                        .as_handle()
                        .try_clone_to_owned()
                        .map_err(|e| io_os_error(ctx, e))?;
                    Ok(Stdio::from(owned))
                }
            }
        }
        DEVNULL => Ok(Stdio::null()),
        fd if fd >= 0 => fd_stdio(ctx, fd),
        _ => Err(einval(ctx)),
    }
}

/// The stderr `Stdio` when `stderr == STDOUT` and stdout
/// is NOT a pipe (the pipe case is the shared pair in
/// `spawn_and_wait`): the Zig's `dup2(1, 2)` redirects
/// stderr to the CHILD's stdout stream, so stdout=DEVNULL
/// means stderr is `/dev/null`, stdout=fd means stderr
/// re-opens that fd write-only, and stdout=INHERIT (or
/// the degenerate stdout=STDOUT) means a dup of the
/// parent's stdout (the old `stdio_for` STDOUT arm).
fn stderr_stdio_for(ctx: &mut CallCtx, modes: [i64; 3]) -> PyResult<Stdio> {
    if modes[2] != STDOUT {
        return stdio_for(ctx, modes[2], 2);
    }
    match modes[1] {
        DEVNULL => Ok(Stdio::null()),
        // A fresh dup of the stdout fd (Phase H): both
        // child fds point at the same open file
        // description, so the two streams interleave in
        // write order with a shared offset — the Zig's
        // `dup2(1, 2)` of the CHILD's stdout.
        fd if fd >= 0 => fd_stdio(ctx, fd),
        _ => stdio_for(ctx, STDOUT, 2),
    }
}

/// Map an `io::Error` to `OSError(errno)`: the raw errno
/// when the error carries one, else a small `ErrorKind`
/// table.
fn io_os_error(ctx: &mut CallCtx, e: std::io::Error) -> PyError {
    let errno = match e.raw_os_error() {
        Some(n) => n,
        None => match e.kind() {
            std::io::ErrorKind::NotFound => 2,
            std::io::ErrorKind::PermissionDenied => 13,
            std::io::ErrorKind::AlreadyExists => 17,
            std::io::ErrorKind::InvalidInput => 22,
            std::io::ErrorKind::WouldBlock => 11,
            std::io::ErrorKind::BrokenPipe => 32,
            _ => 5,
        },
    };
    raise_os_error(ctx, errno)
}

/// The `(127, stdout, stderr, False)` tuple for an
/// exec/cwd failure (the Zig's child `_exit(127)`): the
/// captured output is empty bytes where the stream is a
/// pipe, `None` elsewhere.
fn exit_127(ctx: &mut CallCtx, modes: [i64; 3]) -> PyResult<Value> {
    let types = ctx.types;
    let out = if modes[1] == PIPE {
        new_bytes(ctx, types, b"")
    } else {
        Value::None
    };
    let err = if modes[2] == PIPE {
        new_bytes(ctx, types, b"")
    } else {
        Value::None
    };
    let tuple = new_tuple(
        ctx,
        types.tuple,
        &[Value::SmallInt(127), out, err, Value::Bool(false)],
    );
    Ok(Value::Heap(tuple))
}

/// Is this spawn error an exec/cwd failure (the child
/// would `_exit(127)`) rather than a `fork()` failure
/// (`OSError`)? `ENOENT` / `EACCES` / `ENOTDIR` / `ELOOP` /
/// `ENAMETOOLONG` / `ETXTBSY` / `ENOEXEC` cover the execvp
/// / chdir failures.
fn is_exec_failure(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(2 | 13 | 20 | 40 | 36 | 26 | 8))
}

/// Drain a pipe to the caller, chunk by chunk, on a
/// thread; the receiver gets each 16 KiB chunk as it
/// arrives (the final chunk is followed by the channel
/// closing when the thread ends on EOF / read error — the
/// Zig closes the stream on read failure and keeps what
/// it got). Chunked sends (not one send at EOF) matter
/// for the timeout path: after a kill the caller takes
/// what has arrived so far instead of waiting for EOF —
/// a killed child's grandchild may hold the pipe open
/// (probed: a `sh -c 'sleep 5'` timeout took 5.0 s with
/// a single EOF send, the reference takes 1.3 s).
fn drain_thread<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 16384];
        loop {
            match r.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Collect a drained stream. Normally block until the
/// drain thread finishes (EOF) and take everything; on
/// the timeout path take only what has arrived so far
/// without blocking — the reference returns what its
/// poll loop collected before the timeout, it does not
/// wait for EOF.
fn collect_stream(rx: mpsc::Receiver<Vec<u8>>, timed_out: bool) -> Vec<u8> {
    let mut buf = Vec::new();
    if timed_out {
        // Let the drain thread flush any chunk it is about
        // to send, then take what is available.
        std::thread::sleep(Duration::from_millis(10));
        while let Ok(chunk) = rx.try_recv() {
            buf.extend_from_slice(&chunk);
        }
    } else {
        while let Ok(chunk) = rx.recv() {
            buf.extend_from_slice(&chunk);
        }
    }
    buf
}

/// Feed `input` to the child's stdin on a thread, then
/// close it (EOF). A write failure (the child exited
/// early — `EPIPE`) just ends the feed; Rust ignores
/// `SIGPIPE` at startup, so the parent is never killed.
/// Returns the join handle (the caller joins it before
/// returning, so no writer outlives the call).
fn feed_thread(mut stdin: ChildStdin, input: Vec<u8>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut off = 0;
        while off < input.len() {
            match stdin.write(&input[off..]) {
                Ok(0) => break,
                Ok(n) => off += n,
                Err(_) => break,
            }
        }
        // Dropping `stdin` closes the pipe (EOF).
    })
}

/// `_subprocess._spawn_and_wait(argv, input, timeout, env,
/// cwd, stdin, stdout, stderr)`
/// (`mod_subprocess.zig:153-318`), sig (8, 8).
fn spawn_and_wait(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let argv = str_list_arg(ctx, args[0])?;
    let input = input_arg(ctx, args[1])?;
    let timeout = float_arg(ctx, args[2])?;
    let env = env_arg(ctx, args[3])?;
    let cwd = cwd_arg(ctx, args[4])?;
    let modes = [
        int_arg(ctx, args[5])?,
        int_arg(ctx, args[6])?,
        int_arg(ctx, args[7])?,
    ];
    // Windows has no shared fd to dup2 onto stderr (no
    // `pre_exec`, no Unix socket pair): `stderr=STDOUT` over
    // a piped stdout gets its own pipe instead, drained
    // separately below — the two streams are not interleaved
    // as on Unix, but no output is lost.
    #[cfg(windows)]
    let modes = if modes[1] == PIPE && modes[2] == STDOUT {
        [modes[0], modes[1], PIPE]
    } else {
        modes
    };
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(env) = &env {
        cmd.env_clear();
        for e in env {
            match e.split_once('=') {
                Some((k, v)) => {
                    cmd.env(k, v);
                }
                // No `=`: pass as `(entry, "")` (see the
                // module docs).
                None => {
                    cmd.env(e, "");
                }
            }
        }
    }
    if let Some(cwd) = &cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(stdio_for(ctx, modes[0], 0)?);
    // `stderr=STDOUT` with `stdout=PIPE`: both streams share
    // one socket pair (F5b) — the Zig dup2's the child's
    // stdout fd onto stderr, so the two streams interleave
    // in write order into the single drained result. A
    // `UnixStream::pair` is the safe-std way to make one
    // writable fd the child sees as both 1 and 2; the parent
    // drains the other end. (A socket is not a pipe, but the
    // child only writes bytes to it; ordering is preserved.)
    // Unix-only: on Windows the modes were remapped above so
    // this arm never runs (see the `shared_read` match).
    #[cfg(unix)]
    let mut shared_read: Option<UnixStream> = None;
    match (modes[1], modes[2]) {
        #[cfg(unix)]
        (PIPE, STDOUT) => {
            let (w, r) = UnixStream::pair().map_err(|e| io_os_error(ctx, e))?;
            // `Stdio` has no `From<UnixStream>` — dup the
            // write end twice as `OwnedFd`s (safe: `as_fd`
            // + `try_clone_to_owned`, the socket module's
            // pattern). The pair's own write end is dropped
            // here, so the child holds the only writers.
            let w_out: OwnedFd = w
                .as_fd()
                .try_clone_to_owned()
                .map_err(|e| io_os_error(ctx, e))?;
            let w_err: OwnedFd = w
                .as_fd()
                .try_clone_to_owned()
                .map_err(|e| io_os_error(ctx, e))?;
            cmd.stdout(Stdio::from(w_out));
            cmd.stderr(Stdio::from(w_err));
            shared_read = Some(r);
        }
        _ => {
            cmd.stdout(stdio_for(ctx, modes[1], 1)?);
            cmd.stderr(stderr_stdio_for(ctx, modes)?);
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if is_exec_failure(&e) {
                return exit_127(ctx, modes);
            }
            return Err(io_os_error(ctx, e));
        }
    };
    // Drop the builder NOW: it still owns the parent's copies
    // of the `Stdio` fds (e.g. the shared pair's write ends).
    // Until they close, a drain thread reading the pipe/pair
    // never sees EOF — `finish`'s `rx.recv()` would block
    // forever (F5b: the shared-pair test hung here until
    // this drop was added). The `Child` is independent of
    // the builder, so this is safe.
    drop(cmd);
    // Take the pipes: the reader/writer threads own them.
    let stdin_handle = match (modes[0] == PIPE, child.stdin.take(), &input) {
        (true, Some(s), Some(data)) => Some(feed_thread(s, data.clone())),
        (true, Some(s), None) => {
            drop(s);
            None
        }
        _ => None,
    };
    let stdout_rx: Option<mpsc::Receiver<Vec<u8>>> = {
        // The shared pair (stderr=STDOUT over a piped
        // stdout): drain the pair's read end — it carries
        // both streams, merged in write order.
        #[cfg(unix)]
        let rx = match (modes[1] == PIPE, shared_read) {
            (true, Some(r)) => Some(drain_thread(r)),
            (true, None) => match child.stdout.take() {
                Some(out) => Some(drain_thread(out)),
                _ => None,
            },
            _ => None,
        };
        // Windows has no shared pair (see the modes remap
        // above): stdout is always its own pipe.
        #[cfg(not(unix))]
        let rx = match (modes[1] == PIPE, child.stdout.take()) {
            (true, Some(out)) => Some(drain_thread(out)),
            _ => None,
        };
        rx
    };
    let stderr_rx: Option<mpsc::Receiver<Vec<u8>>> = match (modes[2] == PIPE, child.stderr.take())
    {
        (true, Some(err)) => Some(drain_thread(err)),
        _ => None,
    };
    // Poll `try_wait` like the Zig's `waitpid(WNOHANG)` +
    // `sleep10ms` loop, against the monotonic deadline.
    let deadline = if timeout >= 0.0 {
        Some(Instant::now() + Duration::from_secs_f64(timeout))
    } else {
        None
    };
    let mut timed_out = false;
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(io_os_error(ctx, e)),
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                timed_out = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if timed_out {
        // Unified (Phase H): always `Child::kill()`
        // (`SIGKILL`, no `SIGTERM`, no grace) — the
        // `unsafe-ffi` SIGTERM+grace path (U5) is deleted,
        // so both builds report returncode -9. `wait()`
        // reaps after the kill.
        let _ = child.kill();
        let status = child.wait().map_err(|e| io_os_error(ctx, e))?;
        return finish(ctx, modes, status, stdout_rx, stderr_rx, stdin_handle, true);
    }
    let status = status.expect("the loop only exits with a status or a timeout");
    finish(ctx, modes, status, stdout_rx, stderr_rx, stdin_handle, false)
}

/// Build the `(returncode, stdout, stderr, timed_out)`
/// tuple: `returncode` is the exit code, or the negated
/// signal number (`ExitStatusExt::signal`, the Zig's
/// `WIFEXITED` / `WIFSIGNALED` bit ops); a piped stream
/// contributes its drained bytes, any other mode `None`.
/// The feeder thread is joined first so no writer outlives
/// the call.
fn finish(
    ctx: &mut CallCtx,
    modes: [i64; 3],
    status: std::process::ExitStatus,
    stdout_rx: Option<mpsc::Receiver<Vec<u8>>>,
    stderr_rx: Option<mpsc::Receiver<Vec<u8>>>,
    stdin_handle: Option<std::thread::JoinHandle<()>>,
    timed_out: bool,
) -> PyResult<Value> {
    if let Some(h) = stdin_handle {
        let _ = h.join();
    }
    let returncode: i64 = match status.code() {
        Some(code) => code as i64,
        // Unix: a signal death has no exit code — the negated
        // signal number. Windows processes always have an exit
        // code, so this arm is unreachable there.
        #[cfg(unix)]
        None => -(status.signal().unwrap_or(0) as i64),
        #[cfg(not(unix))]
        None => -1,
    };
    let types = ctx.types;
    let out = match (modes[1] == PIPE, stdout_rx) {
        (true, Some(rx)) => new_bytes(ctx, types, &collect_stream(rx, timed_out)),
        _ => Value::None,
    };
    let err = match (modes[2] == PIPE, stderr_rx) {
        (true, Some(rx)) => new_bytes(ctx, types, &collect_stream(rx, timed_out)),
        _ => Value::None,
    };
    let tuple = new_tuple(
        ctx,
        types.tuple,
        &[
            Value::small_int(returncode).unwrap_or(Value::None),
            out,
            err,
            Value::Bool(timed_out),
        ],
    );
    Ok(Value::Heap(tuple))
}

/// Build the `_subprocess` module object
/// (`mod_subprocess.zig:335-360`): globals `{__name__,
/// _spawn_and_wait}` (`__name__` is stored by
/// `new_module`). Registration into
/// `VmState.builtin_modules` is the caller's, as with
/// C18b's `build_io_module`.
pub fn build_subprocess_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let name_id = static_qstr_id(b"_subprocess").expect("_subprocess is a static qstr");
    let module = new_module(ctx, name_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let spawn_id = static_qstr_id(b"_spawn_and_wait").expect("_spawn_and_wait is a static qstr");
    let fun = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: spawn_id,
            n_args: fun_n_args(8, 8),
            func: spawn_and_wait as crate::objects::slots::BuiltinFn,
            kw_func: None,
        }),
    );
    ctx.roots.push(Value::Heap(fun));
    dict_store(ctx, dict_v, Value::Qstr(spawn_id), Value::Heap(fun))
        .map_err(|e| ctx.record_typed(e))?;
    Ok(module)
}
