//! The `socket` module (H-socket rewrite): a SINGLE
//! cross-platform pure-`std` implementation. Behaviour is
//! defined by the Python `socket` API (CPython semantics),
//! identical on every platform and architecture; no libc
//! FFI, no C, no feature-gated branches. The
//! old Windows stub (`modsocket_win.rs`) and the old
//! AF_UNIX-only implementation are gone.
//!
//! What is implemented (CPython semantics):
//! `socket.socket(family, type[, proto])` with `AF_UNIX`
//! / `AF_INET` / `AF_INET6` × `SOCK_STREAM` / `SOCK_DGRAM`;
//! methods `bind` / `listen` / `accept` / `connect` /
//! `recv` / `send` / `sendall` / `sendto` / `recvfrom` /
//! `setblocking` / `settimeout` / `gettimeout` /
//! `getsockopt` / `setsockopt` / `fileno` / `close` /
//! `dup` / `makefile`; module function
//! `getaddrinfo(host, port[, family[, type[, proto]]])`;
//! `fromfd` exists but raises `NotImplementedError`
//! (safe Rust cannot take ownership of a raw fd).
//! `gethostname` / `gethostbyname` are NOT implemented
//! (`std` has no API).
//!
//! Transport: `AF_INET`/`AF_INET6` via `std::net`
//! (`TcpListener` / `TcpStream` / `UdpSocket`);
//! `AF_UNIX` via `std::os::unix::net` (`cfg(unix)`). On
//! Windows `socket(AF_UNIX, ...)` raises
//! `NotImplementedError` at construction (`std` has no
//! AF_UNIX API there).
//!
//! Representation: a socket is an `Obj::Instance` with an
//! empty members map and `subobj = [fd, family, type,
//! proto]` (small ints), header type the socket `TypeData`,
//! allocated with `alloc_with_finaliser` so the GC
//! finaliser drain runs `__del__` (= `close`) when the
//! socket becomes unreachable. The OS socket itself lives
//! in the process-global `SOCKETS` table, keyed by the OS
//! fd on Unix / the `SOCKET` handle value on Windows
//! (`TableKey = i64`; `value.rs`/`gc.rs` are over the line
//! cap, so there is no new `Obj` variant and no arena
//! field for it). Operations that can block clone the
//! stored object (`try_clone`, safe) and never hold the
//! table lock across I/O.
//!
//! Known limitations (all documented here, see the result
//! file §4):
//! - The constructor cannot create an unbound stream
//!   socket in pure `std` (no such constructor), so the
//!   instance is created over a REAL placeholder socket
//!   (`UdpSocket::bind("127.0.0.1:0")`, a real fd) and
//!   `bind()`/`connect()` swap in the real object — so the
//!   fd number changes across `bind()`/`connect()` (the
//!   reference keeps one fd throughout). `fileno()`
//!   returns the placeholder fd before, the real fd after.
//!   A `bind()` that already listens (std's `bind` listens)
//!   means `listen()` is a validation no-op, and a client
//!   connecting between `bind()` and `listen()` is accepted.
//! - `bind("")` on `AF_UNIX` binds an UNNAMED socket (std
//!   behaviour), not the reference's abstract socket.
//! - Timeouts: `settimeout`/`setblocking`/`gettimeout` use
//!   shadow state (std has no timeout getters) and land on
//!   the socket via `set_read_timeout` / `set_write_timeout`
//!   / `set_nonblocking`. `accept()` ignores a listener
//!   timeout (no readiness interface is built or used in
//!   this lane); `connect()` with a timeout uses
//!   `TcpStream::connect_timeout` for `AF_INET`/`AF_INET6`
//!   only (`UnixStream::connect` has no timeout form), and
//!   a nonblocking `connect()` blocks (std's `connect`
//!   creates a blocking socket).
//! - `getsockopt`/`setsockopt` keep per-socket shadow state
//!   `(level, opt) -> bytes`. Only these land in `std`:
//!   `TCP_NODELAY` -> `set_nodelay` (stream),
//!   `SO_BROADCAST` -> `set_broadcast` (UDP), `IP_TTL` ->
//!   `set_ttl` (INET), `SO_RCVTIMEO`/`SO_SNDTIMEO` ->
//!   read/write timeouts. `SO_REUSEADDR`, `SO_RCVBUF`,
//!   `SO_SNDBUF` are shadow-only (std has no query API;
//!   `SO_REUSEADDR` is set by std itself at `bind`).
//!   `SO_PEERCRED` is `OSError(ENOPROTOOPT)` everywhere
//!   (std has no stable API); any other `(level, opt)` is
//!   `ENOPROTOOPT` too.
//! - `recv()` has no size cap (the old 4096/`EFBIG` cap was
//!   the Zig's stack buffer); a request over 64 MiB is
//!   `MemoryError` (a `Vec` that large would abort).
//! - `sendto` on a stream socket is `EISCONN` (connected)
//!   or `ENOTCONN` (not connected); `send`/`sendall` on a
//!   listener is `ENOTCONN` (107, CPython — this also fixes
//!   the G6b §4 leftover, where it was `EINVAL`).
//! - `makefile()` is stream-only (`EINVAL` otherwise) and
//!   returns this module's `socketfile` wrapper object
//!   (`read(n)` / `readline([limit])` / `write()` /
//!   `flush()` / `close()`); the read/write sides are
//!   `try_clone()`d dups, so closing the socket does not
//!   close the file.
//! - Names: the socket API names (`connect`, `sendto`,
//!   `makefile`, ...) are NOT static qstrs in the
//!   original, so method/module names are interned at
//!   runtime via `RuntimeQstr::from_strn` (which returns
//!   the static id on hit); the `.mpy` loader interns by
//!   string, so the ids line up at runtime (covered by the
//!   unit tests, which compile and run real scripts).

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::time::Duration;

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_get_value, dict_store};
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::{new_tuple, tuple_items};
use crate::objects::obj::{obj_get_float, obj_get_int, obj_get_int_maybe};
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::{str_bytes_of, QstrResolver};
use crate::objects::slots::{BuiltinFn, MethodEntry};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{
    DictData, FunBuiltinData, GcRef, InstanceData, MapData, Obj, TypeData, Value,
};
use crate::objects::objtype::objmodule::new_module;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::{malloc_fail, raise_os_error};

/// Linux errno values used below.
const EINVAL: i32 = 22;
const ENAMETOOLONG: i32 = 36;
const EPIPE: i32 = 32;
const EBADF: i32 = 9;
const ENOPROTOOPT: i32 = 92;
const EAFNOSUPPORT: i32 = 97;
const EOPNOTSUPP: i32 = 95;
const ENOTCONN: i32 = 107;
const EISCONN: i32 = 106;
const EPROTONOSUPPORT: i32 = 93;

/// `sockaddr_un.sun_path` length.
const SUN_PATH_LEN: usize = 108;

/// Largest single `recv`/`recvfrom` buffer (a bigger
/// request is `MemoryError`, not a `Vec` that aborts).
const MAX_RECV: i64 = 64 * 1024 * 1024;

/// Address families / socket types (also module constants).
const AF_UNSPEC: i32 = 0;
const AF_UNIX: i32 = 1;
const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const IPPROTO_IP: i32 = 0;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_UDP: i32 = 17;
const SOL_SOCKET: i32 = 1;
const SO_REUSEADDR: i32 = 2;
const SO_BROADCAST: i32 = 6;
const SO_SNDBUF: i32 = 7;
const SO_RCVBUF: i32 = 8;
const SO_RCVTIMEO: i32 = 20;
const SO_SNDTIMEO: i32 = 21;
const IP_TTL: i32 = 2;
const TCP_NODELAY: i32 = 1;
const SO_PEERCRED: i32 = 17;

/// Process-global socket-table key: the OS fd on Unix, the
/// `SOCKET` handle value on Windows. `fileno()` returns it.
type TableKey = i64;

/// The owned OS side of one socket instance.
enum SocketState {
    /// Not yet bound/connected: the placeholder `UdpSocket`
    /// bound to `127.0.0.1:0` (a real fd), replaced by the
    /// real object at `bind()`/`connect()`. For a datagram
    /// socket the placeholder IS usable: `sendto` /
    /// `recv` / `recvfrom` work on it (CPython auto-binds an
    /// unbound UDP socket the same way).
    Unbound(UdpSocket),
    /// Bound stream listener (INET). Already listening:
    /// `std`'s `bind` listens, so `listen()` is a
    /// validation no-op.
    ListenerTcp(TcpListener),
    /// Bound stream listener (UNIX).
    #[cfg(unix)]
    ListenerUnix(UnixListener),
    /// Connected / accepted stream (INET).
    StreamTcp(TcpStream),
    /// Connected / accepted stream (UNIX).
    #[cfg(unix)]
    StreamUnix(UnixStream),
    /// Datagram socket (INET), bound and/or connected.
    Datagram(UdpSocket),
    /// Datagram socket (UNIX), bound and/or connected.
    #[cfg(unix)]
    DatagramUnix(UnixDatagram),
}

impl SocketState {
    /// `set_nonblocking` on the stored object (every
    /// variant supports it; the flag lives on the shared
    /// file description).
    fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        match self {
            SocketState::Unbound(u) | SocketState::Datagram(u) => u.set_nonblocking(nb),
            SocketState::ListenerTcp(l) => l.set_nonblocking(nb),
            #[cfg(unix)]
            SocketState::ListenerUnix(l) => l.set_nonblocking(nb),
            SocketState::StreamTcp(s) => s.set_nonblocking(nb),
            #[cfg(unix)]
            SocketState::StreamUnix(s) => s.set_nonblocking(nb),
            #[cfg(unix)]
            SocketState::DatagramUnix(d) => d.set_nonblocking(nb),
        }
    }

    /// `set_read_timeout` + `set_write_timeout` on the
    /// stored object. Listeners have no read/write
    /// timeouts in `std` — a no-op for them (the shadow
    /// state still records the value).
    fn set_timeouts(&self, r: Option<Duration>, w: Option<Duration>) -> io::Result<()> {
        match self {
            SocketState::ListenerTcp(_) => Ok(()),
            #[cfg(unix)]
            SocketState::ListenerUnix(_) => Ok(()),
            SocketState::Unbound(u) | SocketState::Datagram(u) => {
                u.set_read_timeout(r)?;
                u.set_write_timeout(w)
            }
            SocketState::StreamTcp(s) => {
                s.set_read_timeout(r)?;
                s.set_write_timeout(w)
            }
            #[cfg(unix)]
            SocketState::StreamUnix(s) => {
                s.set_read_timeout(r)?;
                s.set_write_timeout(w)
            }
            #[cfg(unix)]
            SocketState::DatagramUnix(d) => {
                d.set_read_timeout(r)?;
                d.set_write_timeout(w)
            }
        }
    }

    fn is_listener(&self) -> bool {
        match self {
            SocketState::ListenerTcp(_) => true,
            #[cfg(unix)]
            SocketState::ListenerUnix(_) => true,
            _ => false,
        }
    }

    fn is_stream(&self) -> bool {
        match self {
            SocketState::StreamTcp(_) => true,
            #[cfg(unix)]
            SocketState::StreamUnix(_) => true,
            _ => false,
        }
    }

    fn is_datagram(&self) -> bool {
        match self {
            SocketState::Datagram(_) => true,
            #[cfg(unix)]
            SocketState::DatagramUnix(_) => true,
            _ => false,
        }
    }
}

/// The process-global socket table: table key -> the owned
/// OS object. See the module docs for why this is a global
/// and not an arena field.
fn sockets_table() -> &'static Mutex<HashMap<TableKey, SocketState>> {
    static TABLE: OnceLock<Mutex<HashMap<TableKey, SocketState>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sockets() -> std::sync::MutexGuard<'static, HashMap<TableKey, SocketState>> {
    sockets_table().lock().unwrap_or_else(|e| e.into_inner())
}

/// Per-socket shadow settings (std has no timeout getters
/// and no query API for most socket options, so the last
/// set values live here, keyed by the same table key as
/// `SOCKETS`; the entry migrates when the key changes at
/// `bind()`/`connect()`/`dup()`).
#[derive(Clone, Default)]
struct SockSettings {
    /// `gettimeout()` source: `None` = blocking,
    /// `Some(ZERO)` = nonblocking, `Some(d)` = timeout.
    /// Set only by `settimeout` / `setblocking`.
    timeout: Option<Duration>,
    /// Applied read/write timeouts (set by `settimeout`
    /// and by `SO_RCVTIMEO` / `SO_SNDTIMEO`).
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    /// Nonblocking flag (set by `setblocking` /
    /// `settimeout(0)`).
    nonblocking: bool,
    /// `(level, opt) -> canonical bytes` (int options:
    /// 4-byte little-endian). Timeout options live in
    /// `read_timeout` / `write_timeout`, not here.
    opts: HashMap<(i32, i32), Vec<u8>>,
}

fn settings_table() -> &'static Mutex<HashMap<TableKey, SockSettings>> {
    static TABLE: OnceLock<Mutex<HashMap<TableKey, SockSettings>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn settings() -> std::sync::MutexGuard<'static, HashMap<TableKey, SockSettings>> {
    settings_table().lock().unwrap_or_else(|e| e.into_inner())
}

/// Read a socket's settings (default when never set).
fn get_settings(key: TableKey) -> SockSettings {
    settings().get(&key).cloned().unwrap_or_default()
}

/// Move a settings entry to a new key (bind/connect/dup
/// change the fd); the old entry is removed.
fn migrate_settings(old: TableKey, new: TableKey) {
    if old == new {
        return;
    }
    if let Some(s) = settings().remove(&old) {
        settings().insert(new, s);
    }
}

/// Apply the shadow settings to a freshly swapped-in state
/// (bind/connect/dup): nonblocking, read/write timeouts,
/// and the std-landing sockopts. Best-effort: the shadow
/// is the source of truth, so a failed apply is ignored.
fn apply_settings(state: &SocketState, s: &SockSettings) {
    let _ = state.set_nonblocking(s.nonblocking);
    let _ = state.set_timeouts(s.read_timeout, s.write_timeout);
    for ((level, opt), v) in &s.opts {
        apply_opt(state, *level, *opt, v);
    }
}

/// Apply one landed sockopt to a state object (the
/// `setsockopt` landing path, shared with `apply_settings`).
fn apply_opt(state: &SocketState, level: i32, opt: i32, v: &[u8]) {
    let n = int_opt_value(v);
    match (level, opt) {
        (IPPROTO_TCP, TCP_NODELAY) => {
            if let SocketState::StreamTcp(s) = state {
                let _ = s.set_nodelay(n != 0);
            }
        }
        (SOL_SOCKET, SO_BROADCAST) => {
            if let SocketState::Datagram(u) = state {
                let _ = u.set_broadcast(n != 0);
            }
        }
        (IPPROTO_IP, IP_TTL) => {
            let ttl = n.clamp(0, 255) as u8;
            match state {
                SocketState::StreamTcp(s) => {
                    let _ = s.set_ttl(ttl as u32);
                }
                SocketState::Datagram(u) => {
                    let _ = u.set_ttl(ttl as u32);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// First 4 bytes of a canonical int-opt value as `i32`
/// (0 when short — only called on stored values).
fn int_opt_value(v: &[u8]) -> i32 {
    if v.len() >= 4 {
        i32::from_le_bytes([v[0], v[1], v[2], v[3]])
    } else {
        0
    }
}

/// The `read`/`write` side of a `makefile()` object:
/// a `try_clone()`d dup of the underlying stream, so
/// closing the socket does not close the file.
enum SocketIo {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for SocketIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            SocketIo::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            SocketIo::Unix(s) => s.read(buf),
        }
    }
}

impl Write for SocketIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            SocketIo::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            SocketIo::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            SocketIo::Tcp(s) => s.flush(),
            #[cfg(unix)]
            SocketIo::Unix(s) => s.flush(),
        }
    }
}

/// The owned side of one `makefile()` object: a buffered
/// reader and writer over `try_clone()`d dups. The entry's
/// absence from the table means closed (the instance's
/// `subobj[0]` is `-1` then); the reader/writer are
/// `Option` so an I/O call can take one out and not hold
/// the table lock across blocking I/O.
struct SocketFileState {
    reader: Option<BufReader<SocketIo>>,
    writer: Option<BufWriter<SocketIo>>,
    can_read: bool,
    can_write: bool,
}

/// The process-global makefile table: file id -> state.
fn socket_files_table() -> &'static Mutex<HashMap<u64, SocketFileState>> {
    static TABLE: OnceLock<Mutex<HashMap<u64, SocketFileState>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn socket_files() -> std::sync::MutexGuard<'static, HashMap<u64, SocketFileState>> {
    socket_files_table().lock().unwrap_or_else(|e| e.into_inner())
}

fn next_file_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The raw fd / `SOCKET` handle of a `std` socket object,
/// as the table key.
#[cfg(unix)]
fn sock_key<T: AsRawFd>(t: &T) -> TableKey {
    t.as_raw_fd() as TableKey
}

/// The raw fd / `SOCKET` handle of a `std` socket object,
/// as the table key.
#[cfg(windows)]
fn sock_key<T: AsRawSocket>(t: &T) -> TableKey {
    t.as_raw_socket() as TableKey
}

/// The table key of a stored state object.
fn sock_key_of(state: &SocketState) -> TableKey {
    match state {
        SocketState::Unbound(u) | SocketState::Datagram(u) => sock_key(u),
        SocketState::ListenerTcp(l) => sock_key(l),
        #[cfg(unix)]
        SocketState::ListenerUnix(l) => sock_key(l),
        SocketState::StreamTcp(s) => sock_key(s),
        #[cfg(unix)]
        SocketState::StreamUnix(s) => sock_key(s),
        #[cfg(unix)]
        SocketState::DatagramUnix(d) => sock_key(d),
    }
}

/// Classify `fd` for `select.select` (H-select). Returns
/// `None` when `fd` is not a socket in the table (the caller
/// then tries the os fd table, else `EBADF`).
/// `Some(SocketProbe::Unprobable)`: any socket in the table
/// (stream, listener, unbound, or datagram, INET or UNIX).
/// Pure `std` on stable Rust cannot probe its read-readiness
/// without consuming data — `UnixStream::peek` is unstable
/// (`unix_socket_peek`, still unstable on rustc 1.99.1), a
/// monitor thread's `recv` would consume bytes the socket's
/// `recv` path (in this file) does not drain, and a listener
/// has no non-destructive probe at all (accept would consume
/// the pending connection) — so `select` reports such a
/// socket not-ready. That is a known gap, see
/// `scripts/h_select_result.txt` §4; the unblock is a socket
/// monitor + a `recv` drain, which touches this file's `recv`
/// path and belongs after the H-socket rewrite lands.
/// (The old `Bare` variant is gone: H-socket cut `fromfd` —
/// it raises `NotImplementedError` now — so no socket fd can
/// exist without an owned table entry; an unknown fd falls
/// through to the os fd table / `EBADF`.) This is the only
/// `modsocket.rs` addition this lane makes (the brief's peek
/// design assumed a stable `UnixStream::peek`, which does
/// not exist).
pub(crate) enum SocketProbe {
    Unprobable,
}

pub(crate) fn socket_probe(fd: i32) -> Option<SocketProbe> {
    // The table is keyed by the OS fd (`TableKey = i64`).
    if sockets().contains_key(&(fd as TableKey)) {
        Some(SocketProbe::Unprobable)
    } else {
        None
    }
}

/// A small int, infallible for socket fields (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket field fits the small-int window")
}

/// Intern a name in the runtime pool (dynamic names — the
/// socket API names are NOT static qstrs in the original,
/// so `static_qstr_id` cannot see them; `from_strn`
/// returns the static id on hit).
fn qid(ctx: &mut CallCtx, name: &[u8]) -> u32 {
    ctx.pool
        .from_strn(name)
        .map(|q| q as u32)
        .expect("socket name interns")
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

/// `mp_obj_str_get_str` as bytes, `None` when the value is
/// not str/bytes (callers map it to `EINVAL`, the old
/// `bind(path)` shape — `bind(123)` is `EINVAL`, pinned by
/// the unit tests).
fn path_bytes(ctx: &mut CallCtx, v: Value) -> Option<Vec<u8>> {
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
    str_bytes_of(ctx.arena, ctx.types, resolver, v)
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

/// The `socketfile` type object (`BuiltinTypes.socketfile`);
/// `makefile` needs it to allocate file instances.
fn socketfile_type(ctx: &mut CallCtx) -> PyResult<GcRef> {
    match ctx.types.socketfile {
        Some(t) => Ok(t),
        None => {
            ctx.record_raise("TypeError", Some("socketfile type not registered"));
            Err(PyError::PyRaise)
        }
    }
}

/// Read `self` (args[0]) as a socket instance: returns the
/// arena ref, the table key, and `[family, type, proto]`.
fn self_socket(ctx: &mut CallCtx, args: &[Value]) -> PyResult<(GcRef, TableKey, [i32; 3])> {
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
                nums[0],
                nums[1] as i32,
                nums[2] as i32,
                nums[3] as i32,
            )
        }
        _ => return Err(bad(ctx)),
    };
    Ok((*r, fd, [family, type_, proto]))
}

/// Write back `subobj[0]` (the table key) after `bind()` /
/// `connect()` swap the placeholder for the real object,
/// and after `close()`.
fn set_fd(ctx: &mut CallCtx, r: GcRef, fd: TableKey) {
    if let Obj::Instance(d) = ctx.arena.get_mut(r) {
        if !d.subobj.is_empty() {
            d.subobj[0] = small(fd);
        }
    }
}

/// Allocate a socket instance (finaliser set, so the GC
/// drain runs `__del__` when it becomes unreachable).
fn alloc_socket(
    ctx: &mut CallCtx,
    fd: TableKey,
    family: i32,
    type_: i32,
    proto: i32,
) -> PyResult<Value> {
    let socket_type = socket_type(ctx)?;
    let inst = Obj::Instance(InstanceData {
        members: MapData::new(),
        subobj: vec![
            small(fd),
            small(family as i64),
            small(type_ as i64),
            small(proto as i64),
        ],
    });
    let roots = &*ctx.roots;
    let r = ctx.arena.alloc_with_finaliser(Some(socket_type), inst, roots);
    Ok(Value::Heap(r))
}

/// Allocate a `makefile()` instance (finaliser set, so the
/// GC drain runs `__del__` = `close` when it becomes
/// unreachable). `subobj = [file_id]`.
fn alloc_file(ctx: &mut CallCtx, file_id: u64) -> PyResult<Value> {
    let file_type = socketfile_type(ctx)?;
    let inst = Obj::Instance(InstanceData {
        members: MapData::new(),
        subobj: vec![small(file_id as i64)],
    });
    let roots = &*ctx.roots;
    let r = ctx.arena.alloc_with_finaliser(Some(file_type), inst, roots);
    Ok(Value::Heap(r))
}

/// Read `self` (args[0]) as a `makefile()` instance:
/// returns the arena ref and the file id (`-1` after
/// `close()` — callers map that to the closed-file
/// `ValueError`).
fn self_file(ctx: &mut CallCtx, args: &[Value]) -> PyResult<(GcRef, i64)> {
    let bad = |ctx: &mut CallCtx| -> PyError {
        ctx.record_raise("TypeError", Some("expected a socket file"));
        PyError::PyRaise
    };
    let Some(Value::Heap(r)) = args.first() else {
        return Err(bad(ctx));
    };
    match ctx.arena.get(*r) {
        Obj::Instance(d) if d.subobj.len() == 1 => match d.subobj[0] {
            Value::SmallInt(n) => Ok((*r, n)),
            _ => Err(bad(ctx)),
        },
        _ => Err(bad(ctx)),
    }
}

/// Write back a file instance's `subobj[0]` (the file id)
/// after `close()` (`-1`).
fn set_file_id(ctx: &mut CallCtx, r: GcRef, id: i64) {
    if let Obj::Instance(d) = ctx.arena.get_mut(r) {
        if !d.subobj.is_empty() {
            d.subobj[0] = small(id);
        }
    }
}

/// Build a locals dict from a `MethodEntry` table, like
/// `locals_dict_from_entries` but interning names at
/// runtime (`from_strn`) — the socket API names are not
/// static qstrs, so the shared helper would panic on them.
fn locals_dict(ctx: &mut CallCtx, fun_builtin_var: GcRef, entries: &[MethodEntry]) -> GcRef {
    let pairs: Vec<(Value, Value)> = entries
        .iter()
        .map(|e| {
            let name = qid(ctx, e.name.as_bytes());
            let fun = ctx.alloc(
                Some(fun_builtin_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name,
                    n_args: fun_n_args(e.min, e.max),
                    func: e.func,
                    kw_func: e.kw_func,
                }),
            );
            (Value::Qstr(name), Value::Heap(fun))
        })
        .collect();
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(pairs),
        }),
    )
}

// ── Address parsing ─────────────────────────────────────────────────

/// Parse a CPython `AF_INET`/`AF_INET6` address: a
/// `(host, port)` tuple. Host is str/bytes (`""` means the
/// wildcard address); port is an int in `0..=65535`
/// (`OverflowError` outside). A non-tuple / wrong-length
/// tuple is `TypeError`.
fn parse_inet_addr(ctx: &mut CallCtx, v: Value, family: i32) -> PyResult<SocketAddr> {
    let Some(items) = tuple_items(ctx, v) else {
        ctx.record_raise("TypeError", Some("address must be a (host, port) tuple"));
        return Err(PyError::PyRaise);
    };
    if items.len() != 2 {
        ctx.record_raise("TypeError", Some("address tuple must have 2 items"));
        return Err(PyError::PyRaise);
    }
    // Copy the two values out first: `items` borrows
    // `ctx` immutably and the converters need `&mut ctx`.
    let (host_v, port_v) = (items[0], items[1]);
    let host = str_arg(ctx, host_v)?;
    let port = int_arg(ctx, port_v)?;
    if !(0..=65535).contains(&port) {
        ctx.record_raise("OverflowError", Some("port must be 0-65535"));
        return Err(PyError::PyRaise);
    }
    resolve_inet(ctx, &host, port as u16, family)
}

/// Resolve an INET host to one `SocketAddr` of the
/// requested family: an IP literal parses directly (`""`
/// is the wildcard; an IPv4 literal under `AF_INET6`
/// maps to IPv6-mapped), otherwise `to_socket_addrs`
/// resolves the name and the first address of the right
/// family wins. No address of the family is `EAFNOSUPPORT`.
fn resolve_inet(
    ctx: &mut CallCtx,
    host_bytes: &[u8],
    port: u16,
    family: i32,
) -> PyResult<SocketAddr> {
    let host = String::from_utf8_lossy(host_bytes);
    if host.is_empty() {
        let ip: IpAddr = if family == AF_INET6 {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };
        return Ok(SocketAddr::new(ip, port));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let ip = match (family, ip) {
            (AF_INET6, IpAddr::V4(v4)) => IpAddr::V6(v4.to_ipv6_mapped()),
            (AF_INET, IpAddr::V6(_)) => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
            _ => ip,
        };
        return Ok(SocketAddr::new(ip, port));
    }
    let addrs = (&*host, port).to_socket_addrs().map_err(|e| io_os_error(ctx, e))?;
    for a in addrs {
        match (family, a) {
            (AF_INET, SocketAddr::V4(_)) => return Ok(a),
            (AF_INET6, SocketAddr::V6(_)) => return Ok(a),
            _ => {}
        }
    }
    Err(raise_os_error(ctx, EAFNOSUPPORT))
}

/// Parse a CPython `AF_UNIX` address: a path str/bytes.
/// A non-str/bytes value is `EINVAL` (the old `bind(123)`
/// shape, pinned by the unit tests); the path is the
/// bytes up to the first NUL; `>= 108` bytes is
/// `ENAMETOOLONG`.
fn parse_unix_path(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
    let Some(bytes) = path_bytes(ctx, v) else {
        return Err(raise_os_error(ctx, EINVAL));
    };
    let plen = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    if plen >= SUN_PATH_LEN {
        return Err(raise_os_error(ctx, ENAMETOOLONG));
    }
    Ok(bytes[..plen].to_vec())
}

/// The peer address of an INET socket as a CPython
/// `(host, port)` tuple value.
fn inet_addr_tuple(ctx: &mut CallCtx, a: SocketAddr) -> Value {
    let types = ctx.types;
    let host = new_str(ctx, types, a.ip().to_string().as_bytes());
    let port = small(a.port() as i64);
    Value::Heap(new_tuple(ctx, types.tuple, &[host, port]))
}

/// The peer address of an `AF_UNIX` socket as bytes: the
/// bound pathname, or `b''` when unnamed (the old
/// `accept()` shape, pinned by the unit tests).
#[cfg(unix)]
fn unix_addr_bytes(ctx: &mut CallCtx, a: &std::os::unix::net::SocketAddr) -> Value {
    match a.as_pathname() {
        Some(p) => {
            use std::os::unix::ffi::OsStrExt;
            new_bytes(ctx, ctx.types, p.as_os_str().as_bytes())
        }
        None => new_bytes(ctx, ctx.types, b""),
    }
}

// ── Module-level constructors ───────────────────────────────────────

/// `socket.socket(family, type[, proto=0])` (variadic
/// 2..3). CPython fails at construction for a bad
/// family (`EAFNOSUPPORT`), a bad type (`EINVAL`), or a
/// proto that does not match the family/type
/// (`EPROTONOSUPPORT`); this constructor validates the
/// same way, then creates the instance over a REAL
/// placeholder socket (`UdpSocket::bind("127.0.0.1:0")`)
/// — pure `std` has no unbound-socket constructor, so the
/// real object is swapped in at `bind()`/`connect()` and
/// the fd number changes there (see the module docs).
/// `AF_UNIX` on Windows raises `NotImplementedError`
/// (`std` has no AF_UNIX API there).
fn socket_ctor(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let family = int_arg(ctx, args[0])? as i32;
    let type_ = int_arg(ctx, args[1])? as i32;
    let proto = match args.get(2) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    match family {
        AF_UNIX | AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    match type_ {
        SOCK_STREAM | SOCK_DGRAM => {}
        _ => return Err(raise_os_error(ctx, EINVAL)),
    }
    let proto_ok = match (family, type_) {
        (AF_UNIX, _) => proto == 0,
        (_, SOCK_STREAM) => proto == 0 || proto == IPPROTO_TCP,
        (_, SOCK_DGRAM) => proto == 0 || proto == IPPROTO_UDP,
        _ => false,
    };
    if !proto_ok {
        return Err(raise_os_error(ctx, EPROTONOSUPPORT));
    }
    #[cfg(windows)]
    if family == AF_UNIX {
        ctx.record_raise(
            "NotImplementedError",
            Some("AF_UNIX is not supported on Windows"),
        );
        return Err(PyError::PyRaise);
    }
    let placeholder = UdpSocket::bind("127.0.0.1:0").map_err(|e| io_os_error(ctx, e))?;
    let key = sock_key(&placeholder);
    sockets().insert(key, SocketState::Unbound(placeholder));
    alloc_socket(ctx, key, family, type_, proto)
}

/// `socket.fromfd(fd, family, type[, proto=0])`: CUT in
/// H-socket. Safe Rust cannot take ownership of a raw fd,
/// so the function exists only to raise
/// `NotImplementedError` uniformly on every platform.
fn socket_fromfd(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.record_raise(
        "NotImplementedError",
        Some("socket.fromfd is not implemented"),
    );
    Err(PyError::PyRaise)
}

/// `socket.getaddrinfo(host, port[, family[, type[,
/// proto]]])` (variadic 2..5): `ToSocketAddrs` for the
/// common paths. Returns a list of 5-tuples
/// `(family, type, proto, canonname, (host, port))`;
/// `canonname` is `""` (`std` provides none). Host is
/// str/bytes, or `None` for the local host; port is an
/// int (`0..=65535`) or a service-name str (numeric, or
/// one of the well-known names in `resolve_service` — an
/// unknown service is `OSError(EAI_NONAME)`). A lookup
/// failure is `OSError` (CPython raises `gaierror`, a
/// subclass of `OSError` — this VM has no `gaierror`
/// type); a family with no address is `EAFNOSUPPORT`.
/// `getaddrinfo`'s `EAI_NONAME` (-2, CPython's
/// `gaierror` errno for an unknown host or service).
const EAI_NONAME: i32 = -2;

/// Resolve a service name to a port number: a numeric
/// string first, then a small table of well-known
/// services (`std` has no `getservbyname`; CPython's
/// `getaddrinfo` resolves these via the system resolver).
/// Unknown names are `None` (the caller raises
/// `OSError(EAI_NONAME)`).
fn resolve_service(service: &str) -> Option<u16> {
    if let Ok(p) = service.parse::<u16>() {
        return Some(p);
    }
    Some(match service {
        "ftp" => 21,
        "ssh" => 22,
        "telnet" => 23,
        "smtp" => 25,
        "dns" => 53,
        "http" => 80,
        "pop3" => 110,
        "imap" => 143,
        "https" => 443,
        _ => return None,
    })
}

fn socket_getaddrinfo(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let host_bytes: Option<Vec<u8>> = match args[0] {
        Value::None => None,
        v => Some(str_arg(ctx, v)?),
    };
    let family = match args.get(2) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => AF_UNSPEC,
    };
    let type_ = match args.get(3) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => SOCK_STREAM,
    };
    let proto = match args.get(4) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    match family {
        AF_UNSPEC | AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    let host = match &host_bytes {
        None => "localhost".to_string(),
        Some(b) if b.is_empty() => {
            if family == AF_INET6 {
                "::".to_string()
            } else {
                "0.0.0.0".to_string()
            }
        }
        Some(b) => String::from_utf8_lossy(b).into_owned(),
    };
    // Resolve: an int port goes through as a number, a str
    // port as a service name (`resolve_service` handles
    // numeric strings and well-known names).
    let mut out: Vec<Value> = Vec::new();
    let resolved: Vec<SocketAddr> = match args[1] {
        Value::None => {
            ctx.record_raise("TypeError", Some("getaddrinfo port must be int or str"));
            return Err(PyError::PyRaise);
        }
        v => {
            let port_int = obj_get_int_maybe(ctx, ctx.types, v);
            match port_int {
                Ok(Some(p)) => {
                    if !(0..=65535).contains(&p) {
                        ctx.record_raise("OverflowError", Some("port must be 0-65535"));
                        return Err(PyError::PyRaise);
                    }
                    (host.as_str(), p as u16).to_socket_addrs()
                        .map_err(|e| io_os_error(ctx, e))?
                        .collect()
                }
                _ => {
                    let service = str_arg(ctx, v)?;
                    let service = String::from_utf8_lossy(&service);
                    let port = match resolve_service(&service) {
                        Some(p) => p,
                        None => return Err(raise_os_error(ctx, EAI_NONAME)),
                    };
                    (host.as_str(), port)
                        .to_socket_addrs()
                        .map_err(|e| io_os_error(ctx, e))?
                        .collect()
                }
            }
        }
    };
    let types = ctx.types;
    for a in resolved {
        let fam = match a {
            SocketAddr::V4(_) => AF_INET,
            SocketAddr::V6(_) => AF_INET6,
        };
        match (family, fam) {
            (AF_INET, AF_INET) | (AF_INET6, AF_INET6) | (AF_UNSPEC, _) => {}
            _ => continue,
        }
        let canon = new_str(ctx, types, b"");
        let sockaddr = inet_addr_tuple(ctx, a);
        let item = new_tuple(
            ctx,
            types.tuple,
            &[small(fam as i64), small(type_ as i64), small(proto as i64), canon, sockaddr],
        );
        out.push(Value::Heap(item));
    }
    Ok(Value::Heap(new_list(ctx, types.list, &out)))
}

// ── Methods (self = args[0] via type-method dispatch) ───────────────

/// Swap the placeholder for a freshly bound/connected
/// object: drop the old table entry, install the new
/// state under its own key, move the settings over,
/// re-apply them to the new object, and update
/// `subobj[0]`. Returns the new key.
fn swap_in(ctx: &mut CallCtx, r: GcRef, old_key: TableKey, new_state: SocketState) -> TableKey {
    let new_key = sock_key_of(&new_state);
    sockets().remove(&old_key);
    sockets().insert(new_key, new_state);
    migrate_settings(old_key, new_key);
    let s = get_settings(new_key);
    if let Some(st) = sockets().get(&new_key) {
        apply_settings(st, &s);
    }
    set_fd(ctx, r, new_key);
    new_key
}

/// A parsed `connect` / `sendto` target address.
enum TargetAddr {
    Inet(SocketAddr),
    #[cfg(unix)]
    Unix(Vec<u8>),
}

/// A cloned handle for I/O (the table lock is released
/// before it is used).
enum IoHandle {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    Udp(UdpSocket),
    #[cfg(unix)]
    UdpUnix(UnixDatagram),
}

impl IoHandle {
    /// One read: stream `read`, datagram `recv`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            IoHandle::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            IoHandle::Unix(s) => s.read(buf),
            IoHandle::Udp(u) => u.recv(buf),
            #[cfg(unix)]
            IoHandle::UdpUnix(d) => d.recv(buf),
        }
    }

    /// One write: stream `write`, datagram `send`.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            IoHandle::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            IoHandle::Unix(s) => s.write(buf),
            IoHandle::Udp(u) => u.send(buf),
            #[cfg(unix)]
            IoHandle::UdpUnix(d) => d.send(buf),
        }
    }

    /// `write_all`: stream only (datagram `send_all` is a
    /// single `send` — datagrams are atomic).
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            IoHandle::Tcp(s) => s.write_all(buf),
            #[cfg(unix)]
            IoHandle::Unix(s) => s.write_all(buf),
            IoHandle::Udp(u) => u.send(buf).map(|_| ()),
            #[cfg(unix)]
            IoHandle::UdpUnix(d) => d.send(buf).map(|_| ()),
        }
    }

    /// One `recvfrom`: stream sockets read + report the
    /// peer address; datagram sockets `recv_from`.
    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<(usize, IoAddr)> {
        match self {
            IoHandle::Tcp(s) => {
                let n = s.read(buf)?;
                let a = s.peer_addr()?;
                Ok((n, IoAddr::Inet(a)))
            }
            #[cfg(unix)]
            IoHandle::Unix(s) => {
                let n = s.read(buf)?;
                let a = s.peer_addr()?;
                Ok((n, IoAddr::Unix(a)))
            }
            IoHandle::Udp(u) => {
                let (n, a) = u.recv_from(buf)?;
                Ok((n, IoAddr::Inet(a)))
            }
            #[cfg(unix)]
            IoHandle::UdpUnix(d) => {
                let (n, a) = d.recv_from(buf)?;
                Ok((n, IoAddr::Unix(a)))
            }
        }
    }

    /// One `sendto` (datagram sockets only; callers gate
    /// the state first).
    fn send_to(&mut self, buf: &[u8], target: &TargetAddr) -> io::Result<usize> {
        match (self, target) {
            (IoHandle::Udp(u), TargetAddr::Inet(a)) => u.send_to(buf, a),
            #[cfg(unix)]
            (IoHandle::UdpUnix(d), TargetAddr::Unix(p)) => {
                use std::ffi::OsStr;
                use std::os::unix::ffi::OsStrExt;
                use std::path::Path;
                d.send_to(buf, Path::new(OsStr::from_bytes(p)))
            }
            _ => Err(io::Error::from_raw_os_error(EINVAL)),
        }
    }
}

/// A peer address from `recvfrom` / `accept`.
enum IoAddr {
    Inet(SocketAddr),
    #[cfg(unix)]
    Unix(std::os::unix::net::SocketAddr),
}

/// An `IoAddr` as a CPython address value: an
/// `(host, port)` tuple for INET, pathname bytes for
/// UNIX.
fn io_addr_value(ctx: &mut CallCtx, a: &IoAddr) -> Value {
    match a {
        IoAddr::Inet(sa) => inet_addr_tuple(ctx, *sa),
        #[cfg(unix)]
        IoAddr::Unix(sa) => unix_addr_bytes(ctx, sa),
    }
}

/// Clone a handle for I/O from the stored state.
/// `Listener` entries raise `listener_err` (`EINVAL` for
/// reads, `ENOTCONN` for writes); an `Unbound` stream
/// socket raises `unbound_stream_err`; an `Unbound`
/// datagram socket uses the placeholder (CPython
/// auto-binds an unbound UDP socket the same way).
/// A missing entry / negative fd is `EBADF`.
fn io_handle(
    ctx: &mut CallCtx,
    fd: TableKey,
    fields: [i32; 3],
    listener_err: i32,
    unbound_stream_err: i32,
) -> PyResult<IoHandle> {
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let table = sockets();
    match table.get(&fd) {
        Some(SocketState::StreamTcp(s)) => {
            Ok(IoHandle::Tcp(s.try_clone().map_err(|e| io_os_error(ctx, e))?))
        }
        #[cfg(unix)]
        Some(SocketState::StreamUnix(s)) => {
            Ok(IoHandle::Unix(s.try_clone().map_err(|e| io_os_error(ctx, e))?))
        }
        Some(SocketState::Datagram(u)) => {
            Ok(IoHandle::Udp(u.try_clone().map_err(|e| io_os_error(ctx, e))?))
        }
        #[cfg(unix)]
        Some(SocketState::DatagramUnix(d)) => {
            Ok(IoHandle::UdpUnix(d.try_clone().map_err(|e| io_os_error(ctx, e))?))
        }
        Some(SocketState::Unbound(u)) => {
            if fields[1] == SOCK_DGRAM {
                Ok(IoHandle::Udp(u.try_clone().map_err(|e| io_os_error(ctx, e))?))
            } else {
                Err(raise_os_error(ctx, unbound_stream_err))
            }
        }
        Some(_) => Err(raise_os_error(ctx, listener_err)),
        None => Err(raise_os_error(ctx, EBADF)),
    }
}

/// A cloned listener for `accept()` (the table lock is
/// released before the blocking call).
enum ClonedListener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener),
}

impl ClonedListener {
    fn accept(self) -> io::Result<(SocketState, IoAddr)> {
        match self {
            ClonedListener::Tcp(l) => {
                let (s, a) = l.accept()?;
                Ok((SocketState::StreamTcp(s), IoAddr::Inet(a)))
            }
            #[cfg(unix)]
            ClonedListener::Unix(l) => {
                let (s, a) = l.accept()?;
                Ok((SocketState::StreamUnix(s), IoAddr::Unix(a)))
            }
        }
    }
}

/// `AF_UNIX` bind (Unix only): stream -> listener,
/// datagram -> datagram socket.
#[cfg(unix)]
fn bind_unix(path: &[u8], type_: i32) -> io::Result<SocketState> {
    if type_ == SOCK_DGRAM {
        UnixDatagram::bind(unix_path(path)).map(SocketState::DatagramUnix)
    } else {
        UnixListener::bind(unix_path(path)).map(SocketState::ListenerUnix)
    }
}

/// `&[u8]` as a Unix path (Unix only).
#[cfg(unix)]
fn unix_path(path: &[u8]) -> &std::path::Path {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    std::path::Path::new(OsStr::from_bytes(path))
}

/// Canonical 4-byte little-endian encoding of an int
/// socket option (the kernel's `int` format, also what
/// CPython's `getsockopt` returns for int options).
fn int_opt_bytes(n: i32) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

/// A `timeval` (16 bytes: `tv_sec` u64 + `tv_usec` u64,
/// little-endian) for a timeout option; `None` is 16
/// zero bytes (no timeout).
fn timeval_bytes(d: Option<Duration>) -> Vec<u8> {
    let (sec, usec) = match d {
        Some(d) => (d.as_secs(), d.subsec_micros() as u64),
        None => (0, 0),
    };
    let mut v = Vec::with_capacity(16);
    v.extend_from_slice(&sec.to_le_bytes());
    v.extend_from_slice(&usec.to_le_bytes());
    v
}

/// Parse a `timeval` (16 bytes) back to a `Duration`;
/// `None` when the length is wrong or `usec >= 1_000_000`
/// (the kernel would reject it with `EINVAL`).
fn parse_timeval(v: &[u8]) -> Option<Duration> {
    if v.len() != 16 {
        return None;
    }
    let sec = u64::from_le_bytes(v[0..8].try_into().ok()?);
    let usec = u64::from_le_bytes(v[8..16].try_into().ok()?);
    if usec >= 1_000_000 {
        return None;
    }
    Some(Duration::new(sec, (usec * 1_000) as u32))
}

/// `sock.bind(address)`: only an unbound socket can bind
/// (rebinding is `EINVAL`; a closed socket is `EBADF`).
/// INET binds a `(host, port)` tuple; UNIX binds a path
/// (bytes up to the first NUL, `>= 108` is
/// `ENAMETOOLONG`, a non-str/bytes path is `EINVAL`).
/// On success the placeholder is swapped for the real
/// object (the fd changes — see the module docs). `std`'s
/// `bind` already listens, so a bound stream socket is a
/// listener immediately; `bind("")` on UNIX binds an
/// unnamed socket (std behaviour, not the reference's
/// abstract socket).
fn sock_bind(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, fields) = self_socket(ctx, args)?;
    let (family, type_) = (fields[0], fields[1]);
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let is_unbound = matches!(sockets().get(&fd), Some(SocketState::Unbound(_)));
    if !is_unbound {
        if sockets().get(&fd).is_none() {
            return Err(raise_os_error(ctx, EBADF));
        }
        return Err(raise_os_error(ctx, EINVAL));
    }
    if family == AF_UNIX {
        let path = parse_unix_path(ctx, args[1])?;
        #[cfg(unix)]
        {
            let new_state = bind_unix(&path, type_).map_err(|e| io_os_error(ctx, e))?;
            swap_in(ctx, r, fd, new_state);
            return Ok(Value::None);
        }
        #[cfg(windows)]
        {
            let _ = path;
            return Err(raise_os_error(ctx, EAFNOSUPPORT));
        }
    }
    let new_state: SocketState = match family {
        AF_INET | AF_INET6 => {
            let addr = parse_inet_addr(ctx, args[1], family)?;
            if type_ == SOCK_DGRAM {
                UdpSocket::bind(addr)
                    .map(SocketState::Datagram)
                    .map_err(|e| io_os_error(ctx, e))?
            } else {
                TcpListener::bind(addr)
                    .map(SocketState::ListenerTcp)
                    .map_err(|e| io_os_error(ctx, e))?
            }
        }
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    };
    swap_in(ctx, r, fd, new_state);
    Ok(Value::None)
}

/// `sock.listen(backlog)`: a listener succeeds (std's
/// `bind` already listens, so this only validates — the
/// backlog value is still converted first, so a non-int
/// is `TypeError`); a datagram socket is `EOPNOTSUPP`;
/// anything else (unbound, connected, closed) is
/// `EINVAL` / `EBADF`.
fn sock_listen(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let _backlog = match args.get(1) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    match sockets().get(&fd) {
        None => Err(raise_os_error(ctx, EBADF)),
        Some(s) if s.is_listener() => Ok(Value::None),
        Some(s) if s.is_datagram() => Err(raise_os_error(ctx, EOPNOTSUPP)),
        Some(_) => Err(raise_os_error(ctx, EINVAL)),
    }
}

/// `sock.accept() -> (sock, addr)`: the client inherits
/// family/type/proto and the listener's settings
/// (CPython: the accepted socket takes the listener's
/// timeout). The address is an `(host, port)` tuple for
/// INET, pathname bytes (`b''` when unnamed) for UNIX.
/// The accept runs on a clone of the listener, so the
/// table lock is not held across the blocking call.
fn sock_accept(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let listener = {
        let table = sockets();
        match table.get(&fd) {
            Some(SocketState::ListenerTcp(l)) => {
                ClonedListener::Tcp(l.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::ListenerUnix(l)) => {
                ClonedListener::Unix(l.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(_) => return Err(raise_os_error(ctx, EINVAL)),
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    };
    let (new_state, addr) = listener.accept().map_err(|e| io_os_error(ctx, e))?;
    let new_key = sock_key_of(&new_state);
    let parent_settings = get_settings(fd);
    sockets().insert(new_key, new_state);
    if let Some(st) = sockets().get(&new_key) {
        apply_settings(st, &parent_settings);
    }
    settings().insert(new_key, parent_settings);
    let client = alloc_socket(ctx, new_key, fields[0], fields[1], fields[2])?;
    let addr_v = io_addr_value(ctx, &addr);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[client, addr_v]);
    Ok(Value::Heap(tuple))
}

/// `sock.connect(address)`: INET stream connects with
/// `TcpStream::connect` (or `connect_timeout` when a
/// timeout is set); UNIX stream with
/// `UnixStream::connect`; datagram sockets set the peer
/// filter (`UdpSocket::connect` / `UnixDatagram::connect`
/// — the fd does not change for INET datagrams, whose
/// placeholder is connected in place). A listener is
/// `EINVAL`, an already-connected socket is `EISCONN`,
/// a closed socket is `EBADF`. On success the
/// placeholder is swapped for the real object (stream
/// only — the fd changes, see the module docs); a failed
/// connect leaves the socket unbound.
fn sock_connect(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, fields) = self_socket(ctx, args)?;
    let (family, type_) = (fields[0], fields[1]);
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let target = match family {
        #[cfg(unix)]
        AF_UNIX => {
            let p = parse_unix_path(ctx, args[1])?;
            TargetAddr::Unix(p)
        }
        AF_INET | AF_INET6 => TargetAddr::Inet(parse_inet_addr(ctx, args[1], family)?),
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    };
    enum Action {
        NewStream,
        NewDatagram,
        ConnectDatagram,
    }
    let action = {
        let table = sockets();
        match table.get(&fd) {
            None => return Err(raise_os_error(ctx, EBADF)),
            Some(s) if s.is_listener() => return Err(raise_os_error(ctx, EINVAL)),
            Some(s) if s.is_stream() => return Err(raise_os_error(ctx, EISCONN)),
            Some(SocketState::Unbound(_)) => {
                if type_ == SOCK_DGRAM {
                    Action::NewDatagram
                } else {
                    Action::NewStream
                }
            }
            Some(SocketState::Datagram(u)) => {
                if u.peer_addr().is_ok() {
                    return Err(raise_os_error(ctx, EISCONN));
                }
                Action::ConnectDatagram
            }
            #[cfg(unix)]
            Some(SocketState::DatagramUnix(d)) => {
                if d.peer_addr().is_ok() {
                    return Err(raise_os_error(ctx, EISCONN));
                }
                Action::ConnectDatagram
            }
            Some(_) => return Err(raise_os_error(ctx, EINVAL)),
        }
    };
    match (action, target) {
        (Action::NewStream, TargetAddr::Inet(addr)) => {
            let timeout = get_settings(fd).timeout;
            let stream = match timeout {
                Some(d) => TcpStream::connect_timeout(&addr, d),
                None => TcpStream::connect(addr),
            }
            .map_err(|e| io_os_error(ctx, e))?;
            swap_in(ctx, r, fd, SocketState::StreamTcp(stream));
        }
        #[cfg(unix)]
        (Action::NewStream, TargetAddr::Unix(path)) => {
            let stream =
                UnixStream::connect(unix_path(&path)).map_err(|e| io_os_error(ctx, e))?;
            swap_in(ctx, r, fd, SocketState::StreamUnix(stream));
        }
        (Action::NewDatagram, TargetAddr::Inet(addr)) => {
            // Connect the placeholder in place (same fd):
            // clone it, connect the clone (the peer filter
            // lives on the shared description), then move
            // the stored object into the Datagram variant.
            let dup = {
                let table = sockets();
                match table.get(&fd) {
                    Some(SocketState::Unbound(u)) => {
                        u.try_clone().map_err(|e| io_os_error(ctx, e))?
                    }
                    _ => return Err(raise_os_error(ctx, EBADF)),
                }
            };
            dup.connect(addr).map_err(|e| io_os_error(ctx, e))?;
            let prev = sockets().remove(&fd);
            if let Some(SocketState::Unbound(u)) = prev {
                sockets().insert(fd, SocketState::Datagram(u));
            }
        }
        #[cfg(unix)]
        (Action::NewDatagram, TargetAddr::Unix(path)) => {
            let d = UnixDatagram::unbound().map_err(|e| io_os_error(ctx, e))?;
            d.connect(unix_path(&path)).map_err(|e| io_os_error(ctx, e))?;
            swap_in(ctx, r, fd, SocketState::DatagramUnix(d));
        }
        (Action::ConnectDatagram, TargetAddr::Inet(addr)) => {
            let dup = {
                let table = sockets();
                match table.get(&fd) {
                    Some(SocketState::Datagram(u)) => {
                        u.try_clone().map_err(|e| io_os_error(ctx, e))?
                    }
                    _ => return Err(raise_os_error(ctx, EBADF)),
                }
            };
            dup.connect(addr).map_err(|e| io_os_error(ctx, e))?;
        }
        #[cfg(unix)]
        (Action::ConnectDatagram, TargetAddr::Unix(path)) => {
            let dup = {
                let table = sockets();
                match table.get(&fd) {
                    Some(SocketState::DatagramUnix(d)) => {
                        d.try_clone().map_err(|e| io_os_error(ctx, e))?
                    }
                    _ => return Err(raise_os_error(ctx, EBADF)),
                }
            };
            dup.connect(unix_path(&path)).map_err(|e| io_os_error(ctx, e))?;
        }
    }
    Ok(Value::None)
}

/// `sock.recv(bufsize) -> bytes`: `n < 0` is `EINVAL`;
/// over 64 MiB is `MemoryError`; a listener or unbound
/// stream socket is `EINVAL`; a closed socket is
/// `EBADF`. Datagram reads return one datagram. The read
/// runs on a clone, so the table lock is not held across
/// blocking I/O. No size cap (the old 4096/`EFBIG` cap
/// was the Zig's stack buffer).
fn sock_recv(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let n_signed = int_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if n_signed < 0 {
        return Err(raise_os_error(ctx, EINVAL));
    }
    if n_signed > MAX_RECV {
        return Err(malloc_fail(ctx));
    }
    let mut h = io_handle(ctx, fd, fields, EINVAL, EINVAL)?;
    let mut buf = vec![0u8; n_signed as usize];
    let got = h.read(&mut buf).map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf[..got]))
}

/// `sock.send(bytes) -> int`: one write, returning the
/// bytes sent. The data must be str/bytes (a bytearray is
/// TypeError "can't convert to str implicitly", probed);
/// a listener is `ENOTCONN` (107); an unbound stream
/// socket is `EINVAL`; closed is `EBADF`.
fn sock_send(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    let mut h = io_handle(ctx, fd, fields, ENOTCONN, EINVAL)?;
    let n = h.write(&data).map_err(|e| io_os_error(ctx, e))?;
    Ok(small(n as i64))
}

/// `sock.sendall(bytes) -> None`: the data must be
/// str/bytes (a bytearray is TypeError "can't convert to
/// str implicitly", probed); `write_all` retries `EINTR`
/// internally; a listener is `ENOTCONN` (107, CPython —
/// this fixes the G6b §4 leftover, where it was
/// `EINVAL`); an unbound stream socket is `EINVAL`;
/// closed is `EBADF`. Datagram `sendall` is a single
/// `send` (datagrams are atomic).
fn sock_sendall(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    let mut h = io_handle(ctx, fd, fields, ENOTCONN, EINVAL)?;
    h.write_all(&data).map_err(|e| io_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `sock.sendto(bytes, address) -> int`: datagram
/// sockets only. A stream socket is `EISCONN`, an
/// unbound stream socket or a listener is `ENOTCONN`,
/// a closed socket is `EBADF`.
fn sock_sendto(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let (family, type_) = (fields[0], fields[1]);
    let data = str_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let target = match family {
        #[cfg(unix)]
        AF_UNIX => {
            let p = parse_unix_path(ctx, args[2])?;
            TargetAddr::Unix(p)
        }
        AF_INET | AF_INET6 => TargetAddr::Inet(parse_inet_addr(ctx, args[2], family)?),
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    };
    let mut h = {
        let table = sockets();
        match table.get(&fd) {
            Some(SocketState::Datagram(u)) => {
                IoHandle::Udp(u.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::DatagramUnix(d)) => {
                IoHandle::UdpUnix(d.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(SocketState::Unbound(u)) => {
                if type_ == SOCK_DGRAM {
                    IoHandle::Udp(u.try_clone().map_err(|e| io_os_error(ctx, e))?)
                } else {
                    return Err(raise_os_error(ctx, ENOTCONN));
                }
            }
            Some(s) if s.is_stream() => return Err(raise_os_error(ctx, EISCONN)),
            Some(_) => return Err(raise_os_error(ctx, ENOTCONN)),
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    };
    let n = h.send_to(&data, &target).map_err(|e| io_os_error(ctx, e))?;
    Ok(small(n as i64))
}

/// `sock.recvfrom(bufsize) -> (bytes, address)`: like
/// `recv`, plus the peer address (`(host, port)` for
/// INET, pathname bytes for UNIX). Stream sockets read
/// and report the peer address.
fn sock_recvfrom(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let n_signed = int_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if n_signed < 0 {
        return Err(raise_os_error(ctx, EINVAL));
    }
    if n_signed > MAX_RECV {
        return Err(malloc_fail(ctx));
    }
    let mut h = io_handle(ctx, fd, fields, EINVAL, EINVAL)?;
    let mut buf = vec![0u8; n_signed as usize];
    let (got, addr) = h.recv_from(&mut buf).map_err(|e| io_os_error(ctx, e))?;
    let data = new_bytes(ctx, ctx.types, &buf[..got]);
    let addr_v = io_addr_value(ctx, &addr);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[data, addr_v]);
    Ok(Value::Heap(tuple))
}

/// `sock.setblocking(bool)`: `set_nonblocking` on the
/// stored object (the flag lives on the shared file
/// description, so it sticks). Also updates the shadow
/// timeout: blocking clears it, nonblocking sets it to
/// `0`.
fn sock_setblocking(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let blocking = int_arg(ctx, args[1])? != 0;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    {
        let table = sockets();
        match table.get(&fd) {
            Some(s) => s
                .set_nonblocking(!blocking)
                .map_err(|e| io_os_error(ctx, e))?,
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    }
    let mut st = get_settings(fd);
    st.nonblocking = !blocking;
    st.timeout = if blocking { None } else { Some(Duration::ZERO) };
    if blocking {
        st.read_timeout = None;
        st.write_timeout = None;
    }
    settings().insert(fd, st);
    Ok(Value::None)
}

/// `sock.settimeout(seconds)`: `None` = blocking with no
/// timeout, `0` = nonblocking, `> 0` = blocking with a
/// read/write timeout (landed via `set_read_timeout` /
/// `set_write_timeout`; listeners have no such methods
/// in `std`, so only the shadow records it there). A
/// negative value is `ValueError`, a non-finite or huge
/// value is `OverflowError`, a non-number is `TypeError`.
fn sock_settimeout(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let secs: Option<f64> = match args[1] {
        Value::None => None,
        v => {
            let f = obj_get_float(ctx, ctx.types, v).map_err(|e| ctx.record_typed(e))?;
            if f.is_nan() || f < 0.0 {
                ctx.record_raise(
                    "ValueError",
                    Some("Timeout value must be a non-negative number"),
                );
                return Err(PyError::PyRaise);
            }
            if !f.is_finite() || f > u64::MAX as f64 {
                ctx.record_raise("OverflowError", Some("timeout is too large"));
                return Err(PyError::PyRaise);
            }
            Some(f)
        }
    };
    let (dur, nonblocking, timeout) = match secs {
        None => (None, false, None),
        Some(f) if f == 0.0 => (None, true, Some(Duration::ZERO)),
        Some(f) => {
            let d = Duration::from_secs_f64(f);
            (Some(d), false, Some(d))
        }
    };
    {
        let table = sockets();
        match table.get(&fd) {
            Some(s) => {
                s.set_nonblocking(nonblocking)
                    .map_err(|e| io_os_error(ctx, e))?;
                s.set_timeouts(dur, dur)
                    .map_err(|e| io_os_error(ctx, e))?;
            }
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    }
    let mut st = get_settings(fd);
    st.timeout = timeout;
    st.read_timeout = dur;
    st.write_timeout = dur;
    st.nonblocking = nonblocking;
    settings().insert(fd, st);
    Ok(Value::None)
}

/// `sock.gettimeout()`: the last value set by
/// `settimeout` / `setblocking` (`None` = blocking,
/// `0.0` = nonblocking, else seconds). A closed socket
/// is `EBADF` (its shadow entry is removed at `close`,
/// so a reused fd can never inherit stale settings).
fn sock_gettimeout(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    match get_settings(fd).timeout {
        None => Ok(Value::None),
        Some(d) => Ok(new_float(ctx, ctx.types, d.as_secs_f64())),
    }
}

/// `sock.getsockopt(level, opt, buflen) -> bytes`:
/// `buflen <= 0` or `> 4096` is `EINVAL`; a closed socket
/// is `EBADF`. The value comes from the shadow state
/// (see the module docs): `SO_PEERCRED` and any unknown
/// `(level, opt)` are `ENOPROTOOPT`; unset
/// `SO_RCVBUF`/`SO_SNDBUF` are `ENOPROTOOPT` (their
/// kernel defaults are unknown); the other known options
/// return their shadow value or their known default
/// (`TCP_NODELAY`/`SO_BROADCAST`/`SO_REUSEADDR` 0,
/// `IP_TTL` 64, timeouts 16 zero bytes). The result is
/// truncated to `buflen`.
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
    if (level, opt) == (SOL_SOCKET, SO_PEERCRED) {
        return Err(raise_os_error(ctx, ENOPROTOOPT));
    }
    let st = get_settings(fd);
    let bytes: Vec<u8> = match (level, opt) {
        (IPPROTO_TCP, TCP_NODELAY) | (SOL_SOCKET, SO_BROADCAST) | (SOL_SOCKET, SO_REUSEADDR) => {
            int_opt_bytes(
                st.opts
                    .get(&(level, opt))
                    .map(|v| int_opt_value(v))
                    .unwrap_or(0),
            )
        }
        (IPPROTO_IP, IP_TTL) => int_opt_bytes(
            st.opts
                .get(&(level, opt))
                .map(|v| int_opt_value(v))
                .unwrap_or(64),
        ),
        (SOL_SOCKET, SO_RCVTIMEO) => timeval_bytes(st.read_timeout),
        (SOL_SOCKET, SO_SNDTIMEO) => timeval_bytes(st.write_timeout),
        (SOL_SOCKET, SO_RCVBUF) | (SOL_SOCKET, SO_SNDBUF) => match st.opts.get(&(level, opt)) {
            Some(v) => v.clone(),
            None => return Err(raise_os_error(ctx, ENOPROTOOPT)),
        },
        _ => return Err(raise_os_error(ctx, ENOPROTOOPT)),
    };
    let n = (buflen as usize).min(bytes.len());
    Ok(new_bytes(ctx, ctx.types, &bytes[..n]))
}

/// `sock.setsockopt(level, opt, value)`: the value is an
/// int or bytes. Known options are stored in the shadow
/// state and landed in `std` where possible (see the
/// module docs); `SO_PEERCRED` and any unknown
/// `(level, opt)` are `ENOPROTOOPT`. Int values for
/// timeout options are `EINVAL` (the kernel expects a
/// `timeval`); timeout bytes must be exactly 16
/// (`tv_sec` + `tv_usec`, `usec < 1_000_000`); int-option
/// bytes shorter than 4 are `EINVAL`; `IP_TTL` outside
/// `0..=255` is `EINVAL`.
fn sock_setsockopt(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let level = int_arg(ctx, args[1])? as i32;
    let opt = int_arg(ctx, args[2])? as i32;
    let value = args[3];
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    if (level, opt) == (SOL_SOCKET, SO_PEERCRED) {
        return Err(raise_os_error(ctx, ENOPROTOOPT));
    }
    // Parse the value: int first, else str/bytes.
    enum OptValue {
        Int(i32),
        Bytes(Vec<u8>),
    }
    let parsed = match obj_get_int_maybe(ctx, ctx.types, value) {
        Ok(Some(n)) => OptValue::Int(n as i32),
        _ => match str_arg(ctx, value) {
            Ok(b) => OptValue::Bytes(b),
            Err(e) => return Err(e),
        },
    };
    let mut st = get_settings(fd);
    match (level, opt) {
        (IPPROTO_TCP, TCP_NODELAY) | (SOL_SOCKET, SO_BROADCAST) | (SOL_SOCKET, SO_REUSEADDR)
        | (SOL_SOCKET, SO_RCVBUF) | (SOL_SOCKET, SO_SNDBUF) => {
            let stored = match parsed {
                OptValue::Int(n) => int_opt_bytes(n),
                OptValue::Bytes(b) => {
                    if (level, opt) == (SOL_SOCKET, SO_RCVBUF)
                        || (level, opt) == (SOL_SOCKET, SO_SNDBUF)
                    {
                        b
                    } else if b.len() >= 4 {
                        b[..4].to_vec()
                    } else {
                        return Err(raise_os_error(ctx, EINVAL));
                    }
                }
            };
            st.opts.insert((level, opt), stored.clone());
            settings().insert(fd, st);
            if let Some(s) = sockets().get(&fd) {
                apply_opt(s, level, opt, &stored);
            }
        }
        (IPPROTO_IP, IP_TTL) => {
            let n = match parsed {
                OptValue::Int(n) => n,
                OptValue::Bytes(b) => {
                    if b.len() >= 4 {
                        int_opt_value(&b)
                    } else {
                        return Err(raise_os_error(ctx, EINVAL));
                    }
                }
            };
            if !(0..=255).contains(&n) {
                return Err(raise_os_error(ctx, EINVAL));
            }
            let stored = int_opt_bytes(n);
            st.opts.insert((level, opt), stored.clone());
            settings().insert(fd, st);
            if let Some(s) = sockets().get(&fd) {
                apply_opt(s, level, opt, &stored);
            }
        }
        (SOL_SOCKET, SO_RCVTIMEO) | (SOL_SOCKET, SO_SNDTIMEO) => {
            let d = match parsed {
                OptValue::Int(_) => return Err(raise_os_error(ctx, EINVAL)),
                OptValue::Bytes(b) => match parse_timeval(&b) {
                    Some(d) => d,
                    None => return Err(raise_os_error(ctx, EINVAL)),
                },
            };
            if (level, opt) == (SOL_SOCKET, SO_RCVTIMEO) {
                st.read_timeout = Some(d);
            } else {
                st.write_timeout = Some(d);
            }
            let (r, w) = (st.read_timeout, st.write_timeout);
            settings().insert(fd, st);
            if let Some(s) = sockets().get(&fd) {
                let _ = s.set_timeouts(r, w);
            }
        }
        _ => return Err(raise_os_error(ctx, ENOPROTOOPT)),
    }
    Ok(Value::None)
}

/// `sock.fileno() -> int`: the stored table key, `-1`
/// after `close()`.
fn sock_fileno(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    Ok(small(fd))
}

/// `sock.close() -> None` (also `__del__`): idempotent;
/// drops the table entry (closing the fd) and the
/// settings entry (so a reused fd never inherits stale
/// settings), and stores `-1`.
fn sock_close(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, _fields) = self_socket(ctx, args)?;
    if fd >= 0 {
        sockets().remove(&fd);
        settings().remove(&fd);
        set_fd(ctx, r, -1);
    }
    Ok(Value::None)
}

/// `sock.dup() -> socket`: a new socket object sharing
/// the connection (a `try_clone()`d dup — same file
/// description, new fd). The settings are copied to the
/// new socket and re-applied to it.
fn sock_dup(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let new_state = {
        let table = sockets();
        match table.get(&fd) {
            Some(SocketState::Unbound(u)) => {
                SocketState::Unbound(u.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(SocketState::ListenerTcp(l)) => {
                SocketState::ListenerTcp(l.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::ListenerUnix(l)) => {
                SocketState::ListenerUnix(l.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(SocketState::StreamTcp(s)) => {
                SocketState::StreamTcp(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::StreamUnix(s)) => {
                SocketState::StreamUnix(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(SocketState::Datagram(u)) => {
                SocketState::Datagram(u.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::DatagramUnix(d)) => {
                SocketState::DatagramUnix(d.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    };
    let new_key = sock_key_of(&new_state);
    let st = get_settings(fd);
    sockets().insert(new_key, new_state);
    if let Some(s) = sockets().get(&new_key) {
        apply_settings(s, &st);
    }
    settings().insert(new_key, st);
    alloc_socket(ctx, new_key, fields[0], fields[1], fields[2])
}

/// `sock.makefile([mode])`: stream sockets only
/// (`EINVAL` otherwise). Returns this module's
/// `socketfile` wrapper object over two `try_clone()`d
/// dups (one for the reader, one for the writer), so
/// closing the socket does not close the file. Mode
/// defaults to `'r'`; only `[rwb+]` are accepted, at
/// least one of `r`/`w`/`+` (`ValueError` otherwise).
fn sock_makefile(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let mode = match args.get(1) {
        Some(v) => str_arg(ctx, *v)?,
        None => b"r".to_vec(),
    };
    let mut can_read = false;
    let mut can_write = false;
    for &b in &mode {
        match b {
            b'r' => can_read = true,
            b'w' => can_write = true,
            b'b' => {}
            b'+' => {
                can_read = true;
                can_write = true;
            }
            _ => {
                ctx.record_raise("ValueError", Some("invalid mode"));
                return Err(PyError::PyRaise);
            }
        }
    }
    if !can_read && !can_write {
        ctx.record_raise("ValueError", Some("invalid mode"));
        return Err(PyError::PyRaise);
    }
    let io = {
        let table = sockets();
        match table.get(&fd) {
            Some(SocketState::StreamTcp(s)) => {
                SocketIo::Tcp(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            #[cfg(unix)]
            Some(SocketState::StreamUnix(s)) => {
                SocketIo::Unix(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
            }
            Some(_) => return Err(raise_os_error(ctx, EINVAL)),
            None => return Err(raise_os_error(ctx, EBADF)),
        }
    };
    let io2 = match &io {
        SocketIo::Tcp(s) => {
            SocketIo::Tcp(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
        }
        #[cfg(unix)]
        SocketIo::Unix(s) => {
            SocketIo::Unix(s.try_clone().map_err(|e| io_os_error(ctx, e))?)
        }
    };
    let id = next_file_id();
    socket_files().insert(
        id,
        SocketFileState {
            reader: Some(BufReader::new(io)),
            writer: Some(BufWriter::new(io2)),
            can_read,
            can_write,
        },
    );
    alloc_file(ctx, id)
}

// ── `socketfile` methods (self = args[0]) ──────────────────────────

/// `f.read([n]) -> bytes`: `n < 0` (or omitted) reads to
/// EOF; `read(0)` is `b''`. The reader is taken out of
/// the table for the call (no lock across blocking I/O)
/// and put back afterwards. A closed file or a
/// write-only file is `ValueError`.
fn file_read(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let n = match args.get(1) {
        Some(v) => int_arg(ctx, *v)?,
        None => -1,
    };
    let mut reader = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_read {
            ctx.record_raise("ValueError", Some("not readable"));
            return Err(PyError::PyRaise);
        }
        match st.reader.take() {
            Some(r) => r,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let mut buf = Vec::new();
    let res = if n < 0 {
        reader.read_to_end(&mut buf)
    } else {
        let mut limited = reader.take(n as u64);
        let r = limited.read_to_end(&mut buf);
        reader = limited.into_inner();
        r
    };
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.reader = Some(reader);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf))
}

/// `f.readline([n]) -> bytes`: one line up to and
/// including `'\n'` (or EOF); `n >= 0` caps the bytes.
/// Same take/put-back and error rules as `read`.
fn file_readline(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let n = match args.get(1) {
        Some(v) => int_arg(ctx, *v)?,
        None => -1,
    };
    let mut reader = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_read {
            ctx.record_raise("ValueError", Some("not readable"));
            return Err(PyError::PyRaise);
        }
        match st.reader.take() {
            Some(r) => r,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let mut buf = Vec::new();
    let res = if n < 0 {
        reader.read_until(b'\n', &mut buf)
    } else {
        let mut limited = reader.take(n as u64);
        let r = limited.read_until(b'\n', &mut buf);
        reader = limited.into_inner();
        r
    };
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.reader = Some(reader);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf))
}

/// `f.write(bytes) -> int`: the data must be str/bytes
/// (a bytearray is TypeError "can't convert to str
/// implicitly"); returns the bytes written. A closed
/// file or a read-only file is `ValueError`.
fn file_write(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    let mut writer = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_write {
            ctx.record_raise("ValueError", Some("not writable"));
            return Err(PyError::PyRaise);
        }
        match st.writer.take() {
            Some(w) => w,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let res = writer.write_all(&data);
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.writer = Some(writer);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(small(data.len() as i64))
}

/// `f.flush() -> None`: flushes the writer. A closed
/// file is `ValueError`.
fn file_flush(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let mut writer = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        match st.writer.take() {
            Some(w) => w,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let res = writer.flush();
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.writer = Some(writer);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `f.close() -> None` (idempotent): flushes the writer,
/// drops the table entry (closing both dups), and stores
/// `-1`.
fn file_close(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, id) = self_file(ctx, args)?;
    let mut files = socket_files();
    if let Some(mut st) = files.remove(&(id as u64)) {
        if let Some(mut w) = st.writer.take() {
            let _ = w.flush();
        }
    }
    drop(files);
    set_file_id(ctx, r, -1);
    Ok(Value::None)
}

// ── Type / module tables ────────────────────────────────────────────

pub const SOCKET_LOCALS: &[MethodEntry] = &[
    MethodEntry { name: "bind", func: sock_bind, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "listen", func: sock_listen, min: 1, max: 2, kw_func: None },
    MethodEntry { name: "accept", func: sock_accept, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "connect", func: sock_connect, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "recv", func: sock_recv, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "send", func: sock_send, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "sendall", func: sock_sendall, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "sendto", func: sock_sendto, min: 3, max: 3, kw_func: None },
    MethodEntry { name: "recvfrom", func: sock_recvfrom, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "setblocking", func: sock_setblocking, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "settimeout", func: sock_settimeout, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "gettimeout", func: sock_gettimeout, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "getsockopt", func: sock_getsockopt, min: 4, max: 4, kw_func: None },
    MethodEntry { name: "setsockopt", func: sock_setsockopt, min: 4, max: 4, kw_func: None },
    MethodEntry { name: "fileno", func: sock_fileno, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "close", func: sock_close, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "dup", func: sock_dup, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "makefile", func: sock_makefile, min: 1, max: 2, kw_func: None },
];

pub const FILE_LOCALS: &[MethodEntry] = &[
    MethodEntry { name: "read", func: file_read, min: 1, max: 2, kw_func: None },
    MethodEntry { name: "readline", func: file_readline, min: 1, max: 2, kw_func: None },
    MethodEntry { name: "write", func: file_write, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "flush", func: file_flush, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "close", func: file_close, min: 1, max: 1, kw_func: None },
];

fn type_data(name_id: u32, dict: GcRef) -> TypeData {
    TypeData {
        name: name_id,
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

/// Build the `socket` module. Returns `(module, socket
/// type, socketfile type)` — the registrar in
/// `vm_modules.rs` stores both types in `BuiltinTypes`
/// (the builder cannot: `CallCtx.types` is `&`).
pub fn build_socket_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, GcRef, GcRef)> {
    let socket_id = qid(ctx, b"socket");
    let module = new_module(ctx, socket_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let locals = locals_dict(ctx, fun_builtin_var, SOCKET_LOCALS);
    // `__del__` is the same object as `close` (the old
    // module did this too): look the built value up and
    // store it under the second name.
    let close_id = qid(ctx, b"close");
    let close_v = dict_get_value(ctx, Value::Heap(locals), Value::Qstr(close_id))
        .map_err(|e| ctx.record_typed(e))?;
    let del_id = qid(ctx, b"__del__");
    dict_store(ctx, Value::Heap(locals), Value::Qstr(del_id), close_v)
        .map_err(|e| ctx.record_typed(e))?;
    let socket_type = ctx.alloc(Some(ctx.types.type_), Obj::Type(type_data(socket_id, locals)));
    ctx.roots.push(Value::Heap(socket_type));
    let file_locals = locals_dict(ctx, fun_builtin_var, FILE_LOCALS);
    let file_id = qid(ctx, b"socketfile");
    let socketfile_type =
        ctx.alloc(Some(ctx.types.type_), Obj::Type(type_data(file_id, file_locals)));
    ctx.roots.push(Value::Heap(socketfile_type));
    let ctor = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: socket_id,
            n_args: fun_n_args(2, 3),
            func: socket_ctor as BuiltinFn,
            kw_func: None,
        }),
    );
    let fromfd_id = qid(ctx, b"fromfd");
    let fromfd = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: fromfd_id,
            n_args: fun_n_args(3, 4),
            func: socket_fromfd as BuiltinFn,
            kw_func: None,
        }),
    );
    let gai_id = qid(ctx, b"getaddrinfo");
    let getaddrinfo = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: gai_id,
            n_args: fun_n_args(2, 5),
            func: socket_getaddrinfo as BuiltinFn,
            kw_func: None,
        }),
    );
    ctx.roots.push(Value::Heap(ctor));
    ctx.roots.push(Value::Heap(fromfd));
    ctx.roots.push(Value::Heap(getaddrinfo));
    dict_store(ctx, dict_v, Value::Qstr(socket_id), Value::Heap(ctor))
        .map_err(|e| ctx.record_typed(e))?;
    dict_store(ctx, dict_v, Value::Qstr(fromfd_id), Value::Heap(fromfd))
        .map_err(|e| ctx.record_typed(e))?;
    dict_store(ctx, dict_v, Value::Qstr(gai_id), Value::Heap(getaddrinfo))
        .map_err(|e| ctx.record_typed(e))?;
    for (name, v) in [
        (b"AF_UNSPEC".as_slice(), 0i64),
        (b"AF_UNIX".as_slice(), 1),
        (b"AF_INET".as_slice(), 2),
        (b"AF_INET6".as_slice(), 10),
        (b"SOCK_STREAM".as_slice(), 1),
        (b"SOCK_DGRAM".as_slice(), 2),
        (b"SOL_SOCKET".as_slice(), 1),
        (b"SO_REUSEADDR".as_slice(), 2),
        (b"SO_BROADCAST".as_slice(), 6),
        (b"SO_RCVBUF".as_slice(), 8),
        (b"SO_SNDBUF".as_slice(), 7),
        (b"SO_RCVTIMEO".as_slice(), 20),
        (b"SO_SNDTIMEO".as_slice(), 21),
        (b"SO_PEERCRED".as_slice(), 17),
        (b"IPPROTO_IP".as_slice(), 0),
        (b"IPPROTO_TCP".as_slice(), 6),
        (b"IPPROTO_UDP".as_slice(), 17),
        (b"TCP_NODELAY".as_slice(), 1),
        (b"IP_TTL".as_slice(), 2),
        (b"SHUT_RD".as_slice(), 0),
        (b"SHUT_WR".as_slice(), 1),
        (b"SHUT_RDWR".as_slice(), 2),
    ] {
        let id = qid(ctx, name);
        dict_store(ctx, dict_v, Value::Qstr(id), small(v)).map_err(|e| ctx.record_typed(e))?;
    }
    Ok((module, socket_type, socketfile_type))
}
