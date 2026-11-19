//! The `socket` module (Zig `src/modules/modsocket.zig`, 431
//! lines): the BSD-socket subset for the AF_UNIX RPC listener
//! (`hop3-rootd`): `socket.socket(family, type[, proto])`,
//! `socket.fromfd(fd, family, type[, proto])`, methods
//! `bind` / `listen` / `accept` / `recv` / `sendall` /
//! `setblocking` / `getsockopt` / `fileno` / `close` (`__del__`
//! is the same object as `close`), constants `AF_UNIX`,
//! `AF_INET`, `AF_INET6`, `SOCK_STREAM`, `SOCK_DGRAM`,
//! `SOL_SOCKET`, `SO_REUSEADDR`, `SO_PEERCRED` (17, the Linux
//! value, hardcoded as in the Zig).
//!
//! Not shipped, as in the Zig: `connect`, `sendto`,
//! `recvfrom`, `setsockopt`, `makefile`, `gethostname`,
//! `gethostbyname`, IPv4/6 address-tuple encoding.
//!
//! Representation: a socket is an `Obj::Instance` with an
//! empty members map and
//! `subobj = [fd, family, type, proto]` (small ints), header
//! type the socket `TypeData` (flags `0x0000`, no print/call/
//! unary/binary/attr/subscr/iter/buffer/protocol/parent slots,
//! locals dict the 10-entry table below), allocated with
//! `alloc_with_finaliser` so the GC finaliser drain runs
//! `__del__` (= `close`) when the socket becomes unreachable
//! (the Zig's `m_malloc_with_finaliser`). The default print
//! is `<socket>` (`obj_print_helper`'s no-slot branch).
//!
//! The OS socket itself lives in the process-global
//! `SOCKETS` table, keyed by raw fd: a `BuiltinFn` receives
//! only a `CallCtx` (no `VmState` handle), and both
//! `value.rs` (886) and `gc.rs` (830) are already over the
//! 800-line cap, so there is no new `Obj` variant and no
//! arena field for it. Fds are process-unique, so keying by
//! fd is correct across VMs and threads; `close()` removes
//! the entry and the fd closes when the owned object drops
//! (except a `fromfd` foreign fd — see below).
//! Operations that can block (accept/recv/sendall) always
//! work on a dup of the fd, never on the table's object,
//! so the table lock is never held across I/O.
//!
//! Pure-`std` divergences from the Zig (all recorded here,
//! see the result file §4) — each is fixed under the
//! `unsafe-ffi` feature (G6b: U4/U6/U7) and keeps the
//! old shape on the default build:
//! - The constructor cannot call `socket()`: `std` has no
//!   unbound stream-socket constructor (`UnixListener::bind`
//!   creates, binds AND listens). The instance is created
//!   over a `/dev/null` placeholder fd and `bind()` swaps in
//!   the real listener, so the fd number changes across
//!   `bind()` (the reference keeps one fd throughout) and a
//!   client connecting between `bind()` and `listen()` is
//!   accepted (the reference refuses it — `bind()` already
//!   listens, backlog 128, and `listen(backlog)` is a
//!   validation no-op after `bind()`). Under `unsafe-ffi`
//!   the constructor calls the real `socket(2)`, `bind()`
//!   runs the real `bind(2)` on the same fd, and `listen()`
//!   runs the real `listen(2)` (U6).
//! - `fromfd` does NOT take ownership of the fd: closing the
//!   socket drops the table entry but leaves the caller's fd
//!   open (the Zig wraps without dup and closes it; CPython
//!   dups it). Under `unsafe-ffi` the socket owns the fd
//!   (U4). `fromfd(-1, ...)` creates an instance with fd -1
//!   and no table entry, which behaves exactly like a closed
//!   socket. I/O on a foreign fd without an owned object is
//!   EBADF (no dup-able object).
//! - `getsockopt` has no stable-`std` implementation
//!   (`UnixStream::peer_cred` is unstable
//!   `peer_credentials_unix_socket`): after the `buflen`
//!   validation it raises `OSError(ENOPROTOOPT)` for every
//!   `(level, opt)`. The reference returns the real bytes
//!   (e.g. the 12-byte `ucred` for `SO_PEERCRED`). Under
//!   `unsafe-ffi` the real `getsockopt(2)` runs (U7).
//! - Integer arguments are truncated to `i32` (`as` cast);
//!   the Zig's `@intCast` would abort on overflow.
//! - `bind("")` on the default build binds an UNNAMED
//!   socket (`UnixListener::bind` passes addrlen 2 for
//!   the empty path): it listens, but no client can
//!   connect to it by name; the reference binds an
//!   abstract-named socket (it passes the full
//!   `sockaddr_un` size with an all-zero path). Under
//!   `unsafe-ffi` `bind("")` binds the abstract socket
//!   (U6).
//! - `sendall` on a listener is `EINVAL` (the `is_stream`
//!   gate); the reference's `send(2)` there is `ENOTCONN`
//!   (107, probed). Out of scope for G6b (the brief leaves
//!   `sendall` unchanged).

use std::collections::HashMap;
#[cfg(not(feature = "unsafe-ffi"))]
use std::ffi::OsStr;
#[cfg(not(feature = "unsafe-ffi"))]
use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(not(feature = "unsafe-ffi"))]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(not(feature = "unsafe-ffi"))]
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_get_value, dict_store};
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_int;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::{static_qstr_id, str_bytes_of, QstrResolver};
use crate::objects::slots::{BuiltinFn, MethodEntry};
use crate::objects::strings::objstr_a::new_bytes;
use crate::objects::value::{
    FunBuiltinData, GcRef, InstanceData, MapData, Obj, TypeData, Value,
};
use crate::objects::objtype::objmodule::new_module;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;
use crate::runtime::vm_types::locals_dict_from_entries;

/// Linux errno values used below (the Zig reads them from
/// `errno.h` / `sys/socket.h` via `cImport`).
const EINVAL: i32 = 22;
const ENAMETOOLONG: i32 = 36;
const EAFNOSUPPORT: i32 = 97;
const EFBIG: i32 = 27;
const EPIPE: i32 = 32;
const EBADF: i32 = 9;
/// Only the default build raises this (the feature build
/// runs the real `getsockopt(2)` instead).
#[cfg(not(feature = "unsafe-ffi"))]
const ENOPROTOOPT: i32 = 92;

/// `sockaddr_un.sun_path` length (the Zig's
/// `sun.sun_path.len`).
const SUN_PATH_LEN: usize = 108;

/// The AF_UNIX family value (also the module constant).
const AF_UNIX: i32 = 1;

/// The process-global socket table: raw fd -> the owned OS
/// object. See the module docs for why this is a global and
/// not an arena field.
fn sockets_table() -> &'static Mutex<HashMap<RawFd, SocketState>> {
    static TABLE: OnceLock<Mutex<HashMap<RawFd, SocketState>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The owned OS side of one socket instance.
enum SocketState {
    /// Not yet bound: under `unsafe-ffi` the real socket
    /// fd from `socket(2)` (U6); on the default build a
    /// `/dev/null` placeholder fd, replaced by a real
    /// listener at `bind()`.
    Unbound(OwnedFd),
    /// Bound (and, under `std`, already listening). Under
    /// `unsafe-ffi` this also holds a merely-bound socket:
    /// the fd is wrapped as a `UnixListener` but the
    /// kernel has not been asked to listen yet, so
    /// `accept()` fails `EINVAL` there (the kernel
    /// decides) until `listen()` runs the real `listen(2)`.
    Listener(UnixListener),
    /// Connected (accepted).
    Stream(UnixStream),
    /// A foreign fd wrapped by `fromfd` (F5b). Under
    /// `unsafe-ffi` the socket OWNS the fd (U4 — the
    /// reference wraps without dup and closes it, unlike
    /// CPython which dups it); `close()` drops the entry
    /// and the OS fd closes. On the default build there is
    /// no owned object — the fd is only a table key, and
    /// `close()` drops the entry but leaves the caller's
    /// fd open (the documented divergence, see
    /// `scripts/f5b_result.txt` §4). I/O on a foreign fd
    /// without an owned object is EBADF.
    Foreign(Option<OwnedFd>),
}

fn sockets() -> std::sync::MutexGuard<'static, HashMap<RawFd, SocketState>> {
    sockets_table().lock().unwrap_or_else(|e| e.into_inner())
}

/// Is the fd's entry a stream-like one (`recv`/`sendall`
/// may use it)? A `Foreign` entry counts only when it owns
/// its fd (U4): the kernel then decides the real error
/// (a fromfd listener reads `EINVAL`, a fromfd stream
/// reads). The lock is released before the result is
/// used.
fn is_stream(fd: RawFd) -> bool {
    matches!(
        sockets().get(&fd),
        Some(SocketState::Stream(_)) | Some(SocketState::Foreign(Some(_)))
    )
}

/// Is the fd's entry one `accept` may use (a listener, or
/// a foreign fd that might be one)? A `Foreign` entry
/// counts only when it owns its fd (U4 — a fromfd listener
/// may accept, probed on the reference); the kernel
/// decides whether the fd is actually listening
/// (`accept` on a bound-but-not-listening socket is
/// `EINVAL`). The lock is released before the result is
/// used.
fn is_listener(fd: RawFd) -> bool {
    matches!(
        sockets().get(&fd),
        Some(SocketState::Listener(_)) | Some(SocketState::Foreign(Some(_)))
    )
}

/// Duplicate the fd (safe: `as_fd` + `try_clone_to_owned`
/// dups it) for an operation that may block, so the table
/// lock is never held across I/O. The dup shares the file
/// description with the original, so flags set on it stick.
fn dup_fd(ctx: &mut CallCtx, fd: RawFd) -> PyResult<OwnedFd> {
    let table = sockets();
    let owned = match table.get(&fd) {
        Some(SocketState::Unbound(f)) => f.as_fd().try_clone_to_owned(),
        Some(SocketState::Listener(l)) => l.as_fd().try_clone_to_owned(),
        Some(SocketState::Stream(s)) => s.as_fd().try_clone_to_owned(),
        // A foreign fd dups only when it owns its fd (U4);
        // without an owned object there is nothing to dup.
        Some(SocketState::Foreign(Some(f))) => f.as_fd().try_clone_to_owned(),
        Some(SocketState::Foreign(None)) => return Err(raise_os_error(ctx, EBADF)),
        None => return Err(raise_os_error(ctx, EBADF)),
    };
    owned.map_err(|e| io_os_error(ctx, e))
}

/// A small int, infallible for socket fields (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket field fits the small-int window")
}

/// `mp_obj_get_int` (`obj.rs`), mapped through the typed
/// raise (`ObjError::IntConversion` is TypeError "can't
/// convert to int").
fn int_arg(ctx: &mut CallCtx, v: Value) -> PyResult<i64> {
    obj_get_int(ctx, ctx.types, v).map_err(|e| ctx.record_typed(e))
}

/// `mp_obj_str_get_str` as a `Vec<u8>` (`str_bytes_of`:
/// qstr / `str` / `bytes` only). `None` is a TypeError
/// "can't convert to str implicitly" — the `sendall` shape
/// (`mp_obj_str_get_data` raises; probed:
/// `sendall(bytearray(b"hi"))` is that TypeError).
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

/// Map an `io::Error` to `OSError(errno)`: the raw errno
/// when the error carries one, else a small `ErrorKind`
/// table (the socket calls are syscalls, so the raw errno
/// is present in practice).
fn io_os_error(ctx: &mut CallCtx, e: io::Error) -> PyError {
    let errno = match e.raw_os_error() {
        Some(n) => n,
        None => match e.kind() {
            io::ErrorKind::NotFound => 2,
            io::ErrorKind::PermissionDenied => 13,
            io::ErrorKind::AlreadyExists => 17,
            io::ErrorKind::InvalidInput => EINVAL,
            io::ErrorKind::WouldBlock => 11,
            io::ErrorKind::BrokenPipe => EPIPE,
            _ => 5,
        },
    };
    raise_os_error(ctx, errno)
}

/// The socket type object (`BuiltinTypes.socket`); the
/// constructor and `accept` need it to allocate instances.
/// `None` means no socket module was built and registered
/// — unreachable through the module itself; a defensive
/// TypeError rather than a panic.
fn socket_type(ctx: &mut CallCtx) -> PyResult<GcRef> {
    match ctx.types.socket {
        Some(t) => Ok(t),
        None => {
            ctx.record_raise("TypeError", Some("socket type not registered"));
            Err(PyError::PyRaise)
        }
    }
}

/// Read `self` (args[0]) as a socket instance: returns the
/// arena ref, the fd, and `[family, type, proto]`.
fn self_socket(ctx: &mut CallCtx, args: &[Value]) -> PyResult<(GcRef, i32, [i32; 3])> {
    let bad = |ctx: &mut CallCtx| -> PyError {
        ctx.record_raise("TypeError", Some("expected a socket"));
        PyError::PyRaise
    };
    let Some(Value::Heap(r)) = args.first() else {
        return Err(bad(ctx));
    };
    let (fd, family, type_, proto) = match ctx.arena.get(*r) {
        Obj::Instance(d) if d.subobj.len() == 4 => {
            let mut nums = [0i64; 4];
            for (i, v) in d.subobj.iter().enumerate() {
                let Value::SmallInt(n) = v else {
                    return Err(bad(ctx));
                };
                nums[i] = *n;
            }
            (
                nums[0] as i32,
                nums[1] as i32,
                nums[2] as i32,
                nums[3] as i32,
            )
        }
        _ => return Err(bad(ctx)),
    };
    Ok((*r, fd, [family, type_, proto]))
}

/// Write back `subobj[0]` (the fd) after `bind()` swaps the
/// placeholder for the listener, and after `close()`.
fn set_fd(ctx: &mut CallCtx, r: GcRef, fd: i32) {
    if let Obj::Instance(d) = ctx.arena.get_mut(r) {
        if !d.subobj.is_empty() {
            d.subobj[0] = small(fd as i64);
        }
    }
}

/// Allocate a socket instance (finaliser set, so the GC
/// drain runs `__del__` when it becomes unreachable).
fn alloc_socket(ctx: &mut CallCtx, fd: i32, family: i32, type_: i32, proto: i32) -> PyResult<Value> {
    let socket_type = socket_type(ctx)?;
    let inst = Obj::Instance(InstanceData {
        members: MapData::new(),
        subobj: vec![
            small(fd as i64),
            small(family as i64),
            small(type_ as i64),
            small(proto as i64),
        ],
    });
    let roots = &*ctx.roots;
    let r = ctx
        .arena
        .alloc_with_finaliser(Some(socket_type), inst, roots);
    Ok(Value::Heap(r))
}

// ── Module-level constructors ───────────────────────────────────────

/// `socket.socket(family, type[, proto=0])` (`modsocket.zig`
/// `py_socket_ctor`, variadic 2..3). Under `unsafe-ffi`
/// the constructor calls the real `socket(2)` (U6), so it
/// FAILS for a bad family/type — `EAFNOSUPPORT`, as the
/// reference does — and the instance holds the real fd
/// throughout (`bind()` never changes the fd number). On
/// the default build the OS socket cannot be created yet
/// in pure `std` (no unbound stream-socket constructor),
/// so the instance is created over a `/dev/null`
/// placeholder fd; `bind()` swaps in the real listener.
/// The constructor therefore never fails for a bad
/// family/type on the default build; the failure surfaces
/// at `bind()` as `EAFNOSUPPORT` for a non-`AF_UNIX`
/// family.
fn socket_ctor(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let family = int_arg(ctx, args[0])? as i32;
    let type_ = int_arg(ctx, args[1])? as i32;
    let proto = match args.get(2) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    #[cfg(feature = "unsafe-ffi")]
    let (fd, owned) = {
        let fd = crate::support::ffi::sys_socket(family, type_, proto)
            .map_err(|e| raise_os_error(ctx, e))?;
        // FFI invariant: the fd came back from the kernel's
        // `socket(2)` just above, so it is open and owned
        // by this process; taking ownership transfers it to
        // the socket table (exactly one owner).
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        (fd, owned)
    };
    #[cfg(not(feature = "unsafe-ffi"))]
    let (fd, owned) = {
        let placeholder = File::open("/dev/null").map_err(|e| io_os_error(ctx, e))?;
        let fd = placeholder.as_raw_fd();
        (fd, OwnedFd::from(placeholder))
    };
    sockets().insert(fd, SocketState::Unbound(owned));
    alloc_socket(ctx, fd, family, type_, proto)
}

/// `socket.fromfd(fd, family, type[, proto=0])`
/// (`modsocket.zig` `py_socket_fromfd`, variadic 3..4).
/// A negative fd creates an instance with no table entry,
/// behaving exactly like a closed socket (the Zig wraps it
/// directly and fails at use). Under `unsafe-ffi` a
/// non-negative fd is stored as an OWNED `OwnedFd` (U4):
/// the socket takes ownership exactly as the reference
/// does (it wraps without dup and closes it; CPython dups
/// it). `close()` then drops the entry and the OS fd
/// closes (`os.close(fd)` after `s.close()` is EBADF,
/// probed). When the fd is ALREADY in the socket table
/// (e.g. `fromfd` of a live listener's fd) the existing
/// entry is kept — the fd is already owned exactly once,
/// and both instances share it (the Zig has two objects
/// for one fd there too; the table makes the double
/// `close()` safe: the first removes the entry and closes
/// the fd, the second is a no-op remove). On the default
/// build a non-negative fd registers a `Foreign` entry
/// with no owned object: `fileno()` returns the fd and
/// `close()` drops the entry, but the OS fd is NOT closed
/// — the documented divergence, see
/// `scripts/f5b_result.txt` §4.
fn socket_fromfd(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fd = int_arg(ctx, args[0])? as i32;
    let family = int_arg(ctx, args[1])? as i32;
    let type_ = int_arg(ctx, args[2])? as i32;
    let proto = match args.get(3) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    if fd < 0 {
        return alloc_socket(ctx, fd, family, type_, proto);
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        if !sockets().contains_key(&fd) {
            // FFI invariant: the fd came from the caller,
            // who hands over ownership exactly as the
            // reference does (`fromfd` wraps without dup);
            // taking ownership transfers it to the socket
            // table (exactly one owner — the caller must
            // not close it afterwards).
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            sockets().insert(fd, SocketState::Foreign(Some(owned)));
        }
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        sockets().insert(fd, SocketState::Foreign(None));
    }
    alloc_socket(ctx, fd, family, type_, proto)
}

// ── Methods (self = args[0] via type-method dispatch) ───────────────

/// `sock.bind(path_or_addr)` (`modsocket.zig`
/// `py_sock_bind`): non-`AF_UNIX` is `EAFNOSUPPORT`; a
/// non-str/bytes path is `EINVAL` (the Zig's
/// `mp_obj_str_get_str` returns NULL); `plen >= 108` is
/// `ENAMETOOLONG`; the path is the bytes up to the first
/// NUL. `AF_INET` tuples are not supported (same branch as
/// the Zig's). Under `unsafe-ffi` the bind runs the real
/// `bind(2)` on the SAME fd (U6 — the fd number never
/// changes, and a client connecting between `bind()` and
/// `listen()` is refused, as the reference); `bind("")`
/// binds an abstract socket (the all-zeros address). The
/// socket is bound but NOT listening until `listen()`.
/// On the default build `UnixListener::bind` swaps the
/// placeholder for a real listener (already listening),
/// so the fd number changes and `bind("")` binds an
/// unnamed socket (it listens, but no client can connect
/// to it by name).
fn sock_bind(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, fields) = self_socket(ctx, args)?;
    let family = fields[0];
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if family != AF_UNIX {
        return Err(raise_os_error(ctx, EAFNOSUPPORT));
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
    let Some(path_bytes) = str_bytes_of(ctx.arena, ctx.types, resolver, args[1]) else {
        return Err(raise_os_error(ctx, EINVAL));
    };
    let plen = path_bytes
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(path_bytes.len());
    if plen >= SUN_PATH_LEN {
        return Err(raise_os_error(ctx, ENAMETOOLONG));
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        let addr = crate::support::ffi::SockaddrUn::new_unix(&path_bytes, plen);
        crate::support::ffi::sys_bind(fd, &addr).map_err(|e| raise_os_error(ctx, e))?;
        // On success the fd is bound (but not listening).
        // Move the real fd out of the Unbound entry and
        // wrap it as a listener — the fd number NEVER
        // changes. (A second bind fails EINVAL in the
        // kernel, so only the Unbound entry can reach
        // here; any other entry is put back unchanged.)
        // The table lock is released between the remove
        // and the insert (no lock is held across either).
        let prev = sockets().remove(&fd);
        match prev {
            Some(SocketState::Unbound(owned)) => {
                sockets().insert(fd, SocketState::Listener(UnixListener::from(owned)));
            }
            Some(other) => {
                sockets().insert(fd, other);
            }
            None => {}
        }
        let _ = r;
        return Ok(Value::None);
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let path = Path::new(OsStr::from_bytes(&path_bytes[..plen]));
        let listener = UnixListener::bind(path).map_err(|e| io_os_error(ctx, e))?;
        let new_fd = listener.as_raw_fd();
        // Drop the placeholder (closing its fd) and install the
        // listener under its own fd.
        sockets().remove(&fd);
        sockets().insert(new_fd, SocketState::Listener(listener));
        set_fd(ctx, r, new_fd);
        Ok(Value::None)
    }
}

/// `sock.listen(backlog)` (`modsocket.zig`
/// `py_sock_listen`): under `unsafe-ffi` the real
/// `listen(2)` runs on the stored fd (U6) — for Unbound,
/// Bound (the `Listener` entry wrapping a merely-bound
/// fd), Listener AND Foreign states; the kernel is the
/// authority (an unbound socket fails `EINVAL` there,
/// probed on the reference; a bound one succeeds; a
/// listening one succeeds again). On success the state
/// becomes Listener (it already is — the entry is a
/// `UnixListener` wrapper either way). A closed socket
/// is `EBADF`. On the default build `UnixListener::bind`
/// already listens, so this only validates — a listener
/// succeeds, anything else (unbound, connected, closed)
/// is `EINVAL`.
/// The backlog value is still converted first (a non-int
/// is TypeError, as the Zig's `mp_obj_get_int`).
fn sock_listen(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let backlog = int_arg(ctx, args[1])? as i32;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        if !sockets().contains_key(&fd) {
            return Err(raise_os_error(ctx, EINVAL));
        }
        return crate::support::ffi::sys_listen(fd, backlog)
            .map(|()| Value::None)
            .map_err(|e| raise_os_error(ctx, e));
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        match sockets().get(&fd) {
            Some(SocketState::Listener(_)) => Ok(Value::None),
            _ => Err(raise_os_error(ctx, EINVAL)),
        }
    }
}

/// `sock.accept() -> (sock, addr)` (`modsocket.zig`
/// `py_sock_accept`): the client inherits family/type/
/// proto; the address is empty bytes (the Zig's
/// placeholder — real address extraction is untested).
/// The accept runs on a dup of the listener fd (the dup
/// shares the listen queue), so the table lock is not held
/// across the blocking call.
fn sock_accept(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if !is_listener(fd) {
        return Err(raise_os_error(ctx, EINVAL));
    }
    let dup = dup_fd(ctx, fd)?;
    let listener = UnixListener::from(dup);
    let stream = listener.accept().map_err(|e| io_os_error(ctx, e))?.0;
    let client_fd = stream.as_raw_fd();
    sockets().insert(client_fd, SocketState::Stream(stream));
    let client = alloc_socket(ctx, client_fd, fields[0], fields[1], fields[2])?;
    let types = ctx.types;
    let empty = new_bytes(ctx, types, b"");
    let tuple = new_tuple(ctx, types.tuple, &[client, empty]);
    Ok(Value::Heap(tuple))
}

/// `sock.recv(bufsize) -> bytes` (`modsocket.zig`
/// `py_sock_recv`): `n < 0` is `EINVAL`, `n > 4096` is
/// `EFBIG` (the Zig's 4096-byte stack buffer); a listener
/// or unbound socket is `EINVAL` (probed); a closed socket
/// is `EBADF`. The read runs on a dup of the fd.
fn sock_recv(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let n_signed = int_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if n_signed < 0 {
        return Err(raise_os_error(ctx, EINVAL));
    }
    if n_signed > 4096 {
        return Err(raise_os_error(ctx, EFBIG));
    }
    if !is_stream(fd) {
        return Err(raise_os_error(ctx, EINVAL));
    }
    let dup = dup_fd(ctx, fd)?;
    let mut stream = UnixStream::from(dup);
    let mut buf = vec![0u8; n_signed as usize];
    let got = stream.read(&mut buf).map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf[..got]))
}

/// `sock.sendall(bytes) -> None` (`modsocket.zig`
/// `py_sock_sendall`): the data must be str/bytes (a
/// bytearray is TypeError "can't convert to str
/// implicitly", probed); `write_all` retries `EINTR`
/// internally and maps `EPIPE` from a zero write; a
/// listener or unbound socket is `EINVAL`; closed is
/// `EBADF`. The write runs on a dup of the fd.
fn sock_sendall(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if !is_stream(fd) {
        return Err(raise_os_error(ctx, EINVAL));
    }
    let dup = dup_fd(ctx, fd)?;
    let mut stream = UnixStream::from(dup);
    stream.write_all(&data).map_err(|e| io_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `sock.setblocking(bool)` (`modsocket.zig`
/// `py_sock_setblocking`): `fcntl F_GETFL` / `F_SETFL
/// O_NONBLOCK` in the Zig; `set_nonblocking` in `std` (on a
/// dup of the fd — the flag lives on the shared file
/// description, so it sticks).
fn sock_setblocking(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let blocking = int_arg(ctx, args[1])? != 0;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    match sockets().get(&fd) {
        Some(SocketState::Unbound(_))
        | Some(SocketState::Listener(_))
        | Some(SocketState::Stream(_))
        | Some(SocketState::Foreign(Some(_))) => {}
        // A foreign fd without an owned object has no
        // dup-able object (F5b).
        Some(SocketState::Foreign(None)) => return Err(raise_os_error(ctx, EBADF)),
        None => return Err(raise_os_error(ctx, EBADF)),
    }
    let dup = dup_fd(ctx, fd)?;
    // Any `std` socket/file type exposes the same
    // `set_nonblocking`; wrap the dup as a stream — the flag
    // is per-description, so the wrapper type is irrelevant.
    let stream = UnixStream::from(dup);
    stream
        .set_nonblocking(!blocking)
        .map_err(|e| io_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `sock.getsockopt(level, opt, buflen) -> bytes`
/// (`modsocket.zig` `py_sock_getsockopt`, sig (4, 4)):
/// `buflen <= 0` or `> 4096` is `EINVAL`; a closed socket
/// is `EBADF`; under `unsafe-ffi` the real `getsockopt(2)`
/// runs (U7) and returns the bytes of the RETURNED buflen
/// (e.g. the 12-byte `ucred` for `SO_PEERCRED`, probed on
/// the reference). On the default build there is no
/// stable-`std` `getsockopt` (`peer_cred` is unstable), so
/// it is `ENOPROTOOPT` for every `(level, opt)` (see the
/// module docs).
fn sock_getsockopt(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let level = int_arg(ctx, args[1])? as i32;
    let opt = int_arg(ctx, args[2])? as i32;
    let buflen = int_arg(ctx, args[3])?;
    if buflen <= 0 || buflen > 4096 {
        return Err(raise_os_error(ctx, EINVAL));
    }
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        let bytes = crate::support::ffi::sys_getsockopt(fd, level, opt, buflen as i32)
            .map_err(|e| raise_os_error(ctx, e))?;
        return Ok(new_bytes(ctx, ctx.types, &bytes));
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        Err(raise_os_error(ctx, ENOPROTOOPT))
    }
}

/// `sock.fileno() -> int` (`modsocket.zig`
/// `py_sock_fileno`): the stored fd, `-1` after `close()`.
fn sock_fileno(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    Ok(small(fd as i64))
}

/// `sock.close() -> None` (`modsocket.zig` `py_sock_close`;
/// also `__del__`): idempotent; drops the table entry
/// (closing the fd) and stores `-1`.
fn sock_close(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, _fields) = self_socket(ctx, args)?;
    if fd >= 0 {
        sockets().remove(&fd);
        set_fd(ctx, r, -1);
    }
    Ok(Value::None)
}

// ── Type + module ───────────────────────────────────────────────────

/// The 10-entry `mp_socket_locals_dict_table`
/// (`modsocket.zig:307-318`), in the C's order. `__del__`
/// is NOT in this list: it shares the `close` object and is
/// stored by `build_socket_module` after the lookup below.
pub const SOCKET_LOCALS: &[MethodEntry] = &[
    MethodEntry {
        name: "bind",
        func: sock_bind,
        min: 2,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "listen",
        func: sock_listen,
        min: 2,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "accept",
        func: sock_accept,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "recv",
        func: sock_recv,
        min: 2,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "sendall",
        func: sock_sendall,
        min: 2,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "setblocking",
        func: sock_setblocking,
        min: 2,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "getsockopt",
        func: sock_getsockopt,
        min: 4,
        max: 4,
        kw_func: None,
    },
    MethodEntry {
        name: "fileno",
        func: sock_fileno,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "close",
        func: sock_close,
        min: 1,
        max: 1,
        kw_func: None,
    },
];

/// `mp_type_socket` (`modsocket.zig:341-363`): flags
/// `0x0000`, name `socket`, locals_dict slot only — no
/// print/call/unary/binary/attr/subscr/iter/buffer/
/// protocol/parent slots, no `make_new` (construction goes
/// through the module's constructor function).
pub fn socket_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"socket").expect("socket is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// Build the `socket` module object
/// (`modsocket.zig:367-410`): globals `{__name__, socket,
/// fromfd, AF_UNIX, AF_INET, AF_INET6, SOCK_STREAM,
/// SOCK_DGRAM, SOL_SOCKET, SO_REUSEADDR, SO_PEERCRED}` in
/// the Zig's table order (`__name__` is stored by
/// `new_module`). Returns `(module, socket_type)`: the
/// caller stores the type in `BuiltinTypes.socket` (the
/// constructor needs it to allocate instances) and pushes
/// the module into `VmState.builtin_modules` (registration
/// is the caller's, as with C18b's `build_io_module`).
pub fn build_socket_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, GcRef)> {
    let socket_id = static_qstr_id(b"socket").expect("socket is a static qstr");
    let module = new_module(ctx, socket_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let locals = locals_dict_from_entries(ctx, fun_builtin_var, None, SOCKET_LOCALS, &[], &[]);
    // `__del__` is the same object as `close`
    // (`modsocket.zig:318`): look the built value up and
    // store it under the second name.
    let close_id = static_qstr_id(b"close").expect("close is a static qstr");
    let close_v = dict_get_value(ctx, Value::Heap(locals), Value::Qstr(close_id))
        .map_err(|e| ctx.record_typed(e))?;
    let del_id = static_qstr_id(b"__del__").expect("__del__ is a static qstr");
    dict_store(ctx, Value::Heap(locals), Value::Qstr(del_id), close_v)
        .map_err(|e| ctx.record_typed(e))?;
    let socket_type = ctx.alloc(Some(ctx.types.type_), Obj::Type(socket_type_data(locals)));
    ctx.roots.push(Value::Heap(socket_type));
    let ctor = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: socket_id,
            n_args: fun_n_args(2, 3),
            func: socket_ctor as BuiltinFn,
            kw_func: None,
        }),
    );
    let fromfd_id = static_qstr_id(b"fromfd").expect("fromfd is a static qstr");
    let fromfd = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: fromfd_id,
            n_args: fun_n_args(3, 4),
            func: socket_fromfd as BuiltinFn,
            kw_func: None,
        }),
    );
    ctx.roots.push(Value::Heap(ctor));
    ctx.roots.push(Value::Heap(fromfd));
    dict_store(ctx, dict_v, Value::Qstr(socket_id), Value::Heap(ctor))
        .map_err(|e| ctx.record_typed(e))?;
    dict_store(ctx, dict_v, Value::Qstr(fromfd_id), Value::Heap(fromfd))
        .map_err(|e| ctx.record_typed(e))?;
    for (name, v) in [
        (b"AF_UNIX".as_slice(), 1i64),
        (b"AF_INET".as_slice(), 2),
        (b"AF_INET6".as_slice(), 10),
        (b"SOCK_STREAM".as_slice(), 1),
        (b"SOCK_DGRAM".as_slice(), 2),
        (b"SOL_SOCKET".as_slice(), 1),
        (b"SO_REUSEADDR".as_slice(), 2),
        (b"SO_PEERCRED".as_slice(), 17),
    ] {
        let id = static_qstr_id(name).expect("socket constant name is a static qstr");
        dict_store(ctx, dict_v, Value::Qstr(id), small(v)).map_err(|e| ctx.record_typed(e))?;
    }
    Ok((module, socket_type))
}
