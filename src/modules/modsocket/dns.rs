//! DNS / service / address module functions for the
//! `socket` module (Lane S2, brief §3 S2). This is a
//! CHILD module of `modsocket` (declared as `mod dns;` in
//! `modsocket.rs`, file at `src/modules/modsocket/dns.rs`),
//! so it uses `modsocket`'s private helpers directly
//! (`super::int_arg`, `super::socket_ctor`, ...). That
//! keeps `modsocket.rs` under the 2500-line cap (it is at
//! 2463) — the new functions live here, `modsocket.rs`
//! only registers them, dispatches `getaddrinfo` to the
//! FFI path, and applies the default timeout in
//! `socket_ctor`.
//!
//! Behaviour (brief §3 S2): with `unsafe-ffi` ON, the
//! DNS / service functions use the `ffi_dns` wrappers
//! (`getaddrinfo` / `getnameinfo` / `gethostname` /
//! `getservbyname` / `getservbyport` / `inet_*`); with the
//! feature OFF, `getaddrinfo` keeps the existing
//! pure-`std` path (untouched), the pure functions
//! (`htonl` / `inet_*` via `std` parsing, `socketpair`
//! via `std`, `create_connection` / `create_server` via
//! the `std` socket machinery, `getdefaulttimeout` /
//! `setdefaulttimeout`, `getservbyname` / `getservbyport`
//! via the built-in table) still work, and the
//! DNS-only functions (`gethostbyname`, `gethostbyname_ex`,
//! `gethostbyaddr`, `getnameinfo`, `gethostname`,
//! `getfqdn`) raise `NotImplementedError` (the module's
//! established convention for unimplemented APIs —
//! `fromfd`, `os.umask` on the default build).
//! Functional equivalence with CPython is the bar, not
//! byte-for-byte behaviour (user, 2026-11-20): the
//! `getaddrinfo` 5-tuple echoes the requested
//! type/proto (as the existing `std` path does, pinned by
//! `socket_getaddrinfo` — CPython returns the resolved
//! `ai_protocol`, e.g. 6 for `SOCK_STREAM`); IPv6
//! addresses are 2-tuples `(host, port)` in both builds
//! (CPython returns 4-tuples with flowinfo / scope id —
//! the scope is dropped, noted in the result file);
//! `gethostbyname_ex` / `gethostbyaddr` return an empty
//! alias list (`getaddrinfo` / `getnameinfo` have no
//! aliases; CPython's `gethostbyname` does).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::time::Duration;

use super::{
    alloc_socket, inet_addr_tuple, int_arg, io_os_error, qid, resolve_service, small, sock_bind,
    sock_close, sock_connect, sock_listen, sock_setblocking, sock_setsockopt, sock_settimeout,
    sock_key, socket_ctor, sockets, str_arg, SocketState, AF_INET, AF_INET6, AF_UNSPEC, AF_UNIX,
    EAFNOSUPPORT, EOPNOTSUPP, EPROTONOSUPPORT, IPPROTO_TCP, SOCK_STREAM, SOL_SOCKET, SO_REUSEADDR,
};
// Default build only: used solely by the pure-std
// `sock_getsockname` / `sock_getpeername` below (the
// feature build registers S1's FFI versions instead).
#[cfg(not(feature = "unsafe-ffi"))]
use super::{self_socket, EBADF, ENOTCONN};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::{new_tuple, tuple_items};
use crate::objects::obj::{obj_get_float, obj_get_int_maybe};
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{FunBuiltinData, GcRef, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;
#[cfg(feature = "unsafe-ffi")]
use crate::support::ffi_dns::{
    getaddrinfo_owned, gethostname_owned, getnameinfo_owned, getservbyname_owned,
    getservbyport_owned, inet_ntoa_owned, inet_ntop_owned, inet_pton_owned, AddrInfoEntry,
    NameInfoAddr, ResolvedAddr,
};
/// Build #20: `inet_aton_owned` is defined only under
/// `#[cfg(all(feature = "unsafe-ffi", unix))]` in
/// `ffi_dns.rs` (Windows has no `inet_aton`), so the
/// unconditional import broke the Windows build
/// (E0432). The call site already dispatches to the
/// pure-Rust `parse_inet_aton` on Windows.
#[cfg(all(feature = "unsafe-ffi", unix))]
use crate::support::ffi_dns::inet_aton_owned;
/// Family translation across the FFI boundary (kernel
/// `AF_INET6` is 30 on macOS, 23 on Windows; the
/// Python-facing value stays 10). Used by
/// `ffi_getaddrinfo` / `socket_getaddrinfo_ffi` — see
/// Build #20.
#[cfg(feature = "unsafe-ffi")]
use crate::modules::modsocket_ffi::{translate_family, translate_family_back};

// ── Constants (Python-visible; cfg-correct values) ──────────────────

/// `getaddrinfo` flags (`AI_*`): the values are the same
/// on Unix and Windows except `AI_ADDRCONFIG`.
const AI_PASSIVE: i32 = 0x1;
const AI_CANONNAME: i32 = 0x2;
const AI_NUMERICHOST: i32 = 0x4;
const AI_NUMERICSERV: i32 = 0x8;
#[cfg(unix)]
const AI_ADDRCONFIG: i32 = 0x20;
#[cfg(windows)]
const AI_ADDRCONFIG: i32 = 0x400;
const AI_V4MAPPED: i32 = 0x800;
const AI_ALL: i32 = 0x100;

/// `getnameinfo` buffer sizes (the same on every
/// platform; CPython's `NI_MAXHOST` / `NI_MAXSERV`).
const NI_MAXHOST: i32 = 1025;
const NI_MAXSERV: i32 = 32;

/// `getnameinfo` flags (`NI_*`): Unix and Windows differ
/// (Winsock's values).
#[cfg(unix)]
const NI_NUMERICHOST: i32 = 1;
#[cfg(windows)]
const NI_NUMERICHOST: i32 = 0x02;
#[cfg(unix)]
const NI_NUMERICSERV: i32 = 2;
#[cfg(windows)]
const NI_NUMERICSERV: i32 = 0x08;
#[cfg(unix)]
const NI_NAMEREQD: i32 = 8;
#[cfg(windows)]
const NI_NAMEREQD: i32 = 0x04;
#[cfg(unix)]
const NI_DGRAM: i32 = 16;
#[cfg(windows)]
const NI_DGRAM: i32 = 0x10;

/// `getaddrinfo` / `getnameinfo` error codes (`EAI_*`):
/// Unix (glibc / BSD) values are negative; Windows uses
/// the Winsock codes (`ws2tcpip.h`). `EAI_ADDRFAMILY`
/// and `EAI_SYSTEM` do not exist on Windows (not defined
/// in `ws2tcpip.h`), so they are Unix-only here, as in
/// CPython. (`EAI_NONAME` is also a private const in
/// `modsocket.rs` — the Linux value, used by the `std`
/// `getaddrinfo` path; this local one is the
/// platform-correct value, used by the FFI paths.)
#[cfg(unix)]
const EAI_NONAME: i32 = -2;
#[cfg(windows)]
const EAI_NONAME: i32 = 11001;
#[cfg(unix)]
const EAI_BADFLAGS: i32 = -1;
#[cfg(windows)]
const EAI_BADFLAGS: i32 = 10022;
#[cfg(unix)]
const EAI_AGAIN: i32 = -3;
#[cfg(windows)]
const EAI_AGAIN: i32 = 11002;
#[cfg(unix)]
const EAI_FAIL: i32 = -4;
#[cfg(windows)]
const EAI_FAIL: i32 = 11003;
#[cfg(unix)]
const EAI_NODATA: i32 = -5;
#[cfg(windows)]
const EAI_NODATA: i32 = 11004;
#[cfg(unix)]
const EAI_FAMILY: i32 = -6;
#[cfg(windows)]
const EAI_FAMILY: i32 = 10047;
#[cfg(unix)]
const EAI_SOCKTYPE: i32 = -7;
#[cfg(windows)]
const EAI_SOCKTYPE: i32 = 10044;
#[cfg(unix)]
const EAI_SERVICE: i32 = -8;
#[cfg(windows)]
const EAI_SERVICE: i32 = 10109;
#[cfg(unix)]
const EAI_ADDRFAMILY: i32 = -9;
#[cfg(unix)]
const EAI_MEMORY: i32 = -10;
#[cfg(windows)]
const EAI_MEMORY: i32 = 10055;
#[cfg(unix)]
const EAI_SYSTEM: i32 = -11;

/// `SOMAXCONN` (the `listen` backlog default):
/// 128 on Unix, `0x7fffffff` on Windows (Winsock).
#[cfg(unix)]
const SOMAXCONN: i32 = 128;
#[cfg(windows)]
const SOMAXCONN: i32 = 0x7fff_ffff;

/// `SO_REUSEPORT` (Unix only — Winsock has none).
#[cfg(unix)]
const SO_REUSEPORT: i32 = 15;

/// IPv6 socket options (`IPPROTO_IPV6` is 41 on every
/// platform; `IPV6_V6ONLY` is 26 on Unix, 27 on Winsock).
const IPPROTO_IPV6: i32 = 41;
#[cfg(unix)]
const IPV6_V6ONLY: i32 = 26;
#[cfg(windows)]
const IPV6_V6ONLY: i32 = 27;

// ── Default timeout (module state) ──────────────────────────────────

/// The module-level default timeout for new sockets
/// (`socket.setdefaulttimeout` / `socket.getdefaulttimeout`,
/// CPython semantics): `None` = blocking (the initial
/// state). `socket_ctor` applies it to every new socket
/// (a no-op while it is `None`, so the default build's
/// existing behaviour is untouched).
fn default_timeout_table() -> &'static Mutex<Option<Duration>> {
    static TABLE: OnceLock<Mutex<Option<Duration>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(None))
}

/// The current default timeout (called by `socket_ctor`).
pub(crate) fn default_timeout() -> Option<Duration> {
    *default_timeout_table().lock().unwrap_or_else(|e| e.into_inner())
}

fn set_default_timeout(d: Option<Duration>) {
    *default_timeout_table().lock().unwrap_or_else(|e| e.into_inner()) = d;
}

/// Apply the module default timeout to a freshly
/// created socket (called from `socket_ctor`): a no-op
/// while it is `None`. The `SockSettings` entry carries
/// it across the later bind/connect swap
/// (`apply_settings`).
pub(crate) fn apply_default_timeout(key: super::TableKey, d: Duration) {
    // `get_mut`: S1's `SocketState::set_timeouts` takes
    // `&mut self` (the FFI setter updates the shadow
    // fields), so the immutable `get` no longer compiles.
    if let Some(st) = super::sockets().get_mut(&key) {
        let _ = st.set_timeouts(Some(d), Some(d));
    }
    super::settings().insert(
        key,
        super::SockSettings {
            timeout: Some(d),
            read_timeout: Some(d),
            write_timeout: Some(d),
            ..Default::default()
        },
    );
}

// ── Small helpers ───────────────────────────────────────────────────

/// `NotImplementedError(msg)` — the module's convention
/// for an API that exists but is not implemented on this
/// build (cf. `fromfd`, `os.umask` on the default build).
/// Only used on the feature-off build (the
/// `#[cfg(not(feature))]` branches below) and on
/// Windows (the `AF_UNIX` guards), so it is gated to
/// exactly those combinations.
#[cfg(any(not(feature = "unsafe-ffi"), windows))]
fn not_implemented(ctx: &mut CallCtx, msg: &'static str) -> PyError {
    ctx.record_raise("NotImplementedError", Some(msg));
    PyError::PyRaise
}

/// Map a raw error code to the `OSError` errno: on
/// Windows, Winsock codes go through `win32_to_errno`
/// (unknown codes pass through unchanged — e.g. a gai
/// code like 11001 stays 11001, as in CPython's
/// `gaierror`); elsewhere the code is already a POSIX
/// errno / gai code.
fn map_err(code: i32) -> i32 {
    #[cfg(windows)]
    {
        crate::runtime::modos::win32_to_errno(code)
    }
    #[cfg(not(windows))]
    {
        code
    }
}

/// Parse a timeout value (`settimeout` / `setdefaulttimeout`
/// shape): `None` = blocking, `0` = nonblocking, `> 0` =
/// a duration. A negative value is `ValueError`, a
/// non-finite or huge value is `OverflowError`, a
/// non-number is `TypeError` (mirrors `sock_settimeout`).
fn parse_timeout(ctx: &mut CallCtx, v: Value) -> PyResult<Option<f64>> {
    match v {
        Value::None => Ok(None),
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
            Ok(Some(f))
        }
    }
}

/// Parse a `(host, port)` address tuple (for
/// `create_connection` / `create_server`): host is
/// str/bytes or `None`, port is an int or a service-name
/// str. Returns `(host, port_value)` — the port is kept
/// as a `Value` so the resolver can handle both shapes.
fn parse_address_tuple(ctx: &mut CallCtx, v: Value) -> PyResult<(Option<Vec<u8>>, Value)> {
    let Some(items) = tuple_items(ctx, v) else {
        ctx.record_raise("TypeError", Some("address must be a (host, port) tuple"));
        return Err(PyError::PyRaise);
    };
    if items.len() != 2 {
        ctx.record_raise("TypeError", Some("address tuple must have 2 items"));
        return Err(PyError::PyRaise);
    }
    let (host_v, port_v) = (items[0], items[1]);
    let host = match host_v {
        Value::None => None,
        v => Some(str_arg(ctx, v)?),
    };
    // The port must be an int or a str (checked by the
    // resolver); validate the shape here so a bad port is
    // a `TypeError`, as in CPython.
    match obj_get_int_maybe(ctx, ctx.types, port_v) {
        Ok(Some(p)) => {
            if !(0..=65535).contains(&p) {
                ctx.record_raise("OverflowError", Some("port must be 0-65535"));
                return Err(PyError::PyRaise);
            }
        }
        _ => {
            let _ = str_arg(ctx, port_v)?;
        }
    }
    Ok((host, port_v))
}

/// The service name for a port from the built-in table
/// (the reverse of `resolve_service` — the same 9
/// entries; the FFI `getservbyport` is tried first).
fn service_by_port(port: u16) -> Option<&'static str> {
    Some(match port {
        21 => "ftp",
        22 => "ssh",
        23 => "telnet",
        25 => "smtp",
        53 => "dns",
        80 => "http",
        110 => "pop3",
        143 => "imap",
        443 => "https",
        _ => return None,
    })
}

/// `inet_aton`'s short forms, in pure Rust (man
/// `inet_aton`): each part is decimal, octal (`0`
/// prefix) or hex (`0x` prefix); 1 part is a 32-bit
/// value, 2 parts are `a.b` (b is 24-bit), 3 parts are
/// `a.b.c` (c is 16-bit), 4 parts are `a.b.c.d` (each
/// 8-bit). An invalid digit, a part out of range, or a
/// wrong part count is `None`. Used on every platform
/// when the feature is off, and on Windows when it is on
/// (Windows has no `inet_aton`); on Unix with the
/// feature on, the FFI `inet_aton` is used instead (same
/// result). Gated to exactly the combinations that call
/// it, so it is not dead code on Unix + feature.
#[cfg(not(all(feature = "unsafe-ffi", unix)))]
fn parse_inet_aton(s: &str) -> Option<[u8; 4]> {
    fn parse_part(p: &str) -> Option<u32> {
        if p.is_empty() {
            return None;
        }
        let (digits, radix) = if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            (h, 16)
        } else if p.len() > 1 && p.starts_with('0') {
            (&p[1..], 8)
        } else {
            (p, 10)
        };
        if digits.is_empty() {
            return None;
        }
        u32::from_str_radix(digits, radix).ok()
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut vals = [0u32; 4];
    for (i, p) in parts.iter().enumerate() {
        vals[i] = parse_part(p)?;
    }
    let (a, b, c, d) = match parts.len() {
        1 => {
            let v = vals[0];
            (
                (v >> 24) as u8,
                (v >> 16) as u8,
                (v >> 8) as u8,
                v as u8,
            )
        }
        2 => {
            if vals[0] > 0xff || vals[1] > 0xff_ffff {
                return None;
            }
            (
                vals[0] as u8,
                (vals[1] >> 16) as u8,
                (vals[1] >> 8) as u8,
                vals[1] as u8,
            )
        }
        3 => {
            if vals[0] > 0xff || vals[1] > 0xff || vals[2] > 0xffff {
                return None;
            }
            (
                vals[0] as u8,
                vals[1] as u8,
                (vals[2] >> 8) as u8,
                vals[2] as u8,
            )
        }
        _ => {
            if vals.iter().any(|v| *v > 0xff) {
                return None;
            }
            (vals[0] as u8, vals[1] as u8, vals[2] as u8, vals[3] as u8)
        }
    };
    Some([a, b, c, d])
}

// ── `getaddrinfo` (FFI path) + resolution helpers ───────────────────

/// The FFI `getaddrinfo` (brief §3 S2): `getaddrinfo(3)`
/// with a service-name fallback through the built-in
/// table. `service_is_name` is set when `service` is a
/// name (not a number) — only then is the table retried
/// on `EAI_NONAME` / `EAI_SERVICE`. On failure returns
/// the gai code (Unix) / Winsock code (Windows).
/// NOTE: glibc returns `EAI_SERVICE` (-8), not
/// `EAI_NONAME` (-2), for an unknown service name
/// (verified directly; CPython reports -8 too). But the
/// pinned contract (`modsocket_tests::socket_getaddrinfo`,
/// and the `std` path) is `OSError(-2)` for an unknown
/// service, so an unknown name resolves to `EAI_NONAME`
/// here — the table retry distinguishes "unknown" (not
/// in the table either → -2) from "known but wrong
/// socktype" (in the table → retried as a number, which
/// succeeds).
#[cfg(feature = "unsafe-ffi")]
fn ffi_getaddrinfo(
    node: Option<&str>,
    service: &str,
    service_is_name: bool,
    family: i32,
    socktype: i32,
    proto: i32,
    flags: i32,
) -> Result<Vec<AddrInfoEntry>, i32> {
    // Build #20: `family` here is Python-facing
    // (`AF_INET6` 10); the C hints need the kernel
    // family (30 on macOS, 23 on Windows) — passing 10
    // to Darwin's `getaddrinfo` resolves nothing.
    // (`AF_INET` 2 / `AF_UNSPEC` 0 are the identity, so
    // the other `getaddrinfo_owned` call sites that pass
    // only those need no translation.)
    let kernel_family = translate_family(family);
    match getaddrinfo_owned(node, service, kernel_family, socktype, proto, flags) {
        Ok(v) => Ok(v),
        Err(e) if service_is_name && (e == EAI_NONAME || e == EAI_SERVICE) => {
            // The C resolver does not know this service
            // name (or not for this socktype); retry
            // through the built-in 9-entry table (the
            // same table the `std` path uses).
            match resolve_service(service) {
                Some(port) => getaddrinfo_owned(
                    node,
                    &port.to_string(),
                    kernel_family,
                    socktype,
                    proto,
                    flags,
                ),
                None => Err(EAI_NONAME),
            }
        }
        Err(e) => Err(e),
    }
}

/// `socket.getaddrinfo(host, port[, family[, type[,
/// proto]]])` — the FFI path (called from
/// `modsocket.rs` when the feature is on). Mirrors the
/// `std` path's contract (pinned by
/// `socket_getaddrinfo`): the 5-tuple echoes the
/// requested type/proto (CPython returns the resolved
/// `ai_protocol`, e.g. 6 — deliberately not done here),
/// `canonname` is `''` (no `AI_CANONNAME` is passed),
/// the sockaddr is a 2-tuple `(host, port)` for INET and
/// INET6, a bad family is `OSError(EAFNOSUPPORT)`, an
/// unknown host/service is `OSError(EAI_NONAME)`.
/// Host `None` is the loopback (a null node, no
/// `AI_PASSIVE` — as in CPython); host `""` is the
/// wildcard (a null node + `AI_PASSIVE`).
#[cfg(feature = "unsafe-ffi")]
pub(crate) fn socket_getaddrinfo_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
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
    // Host → node + flags (None = loopback, "" = wildcard).
    let (node, flags) = match &host_bytes {
        None => (None, 0),
        Some(b) if b.is_empty() => (None, AI_PASSIVE),
        Some(b) => (Some(String::from_utf8_lossy(b).into_owned()), 0),
    };
    // Port → service string (an int port as a decimal
    // string, a str port as a service name).
    let (service, service_is_name) = match args[1] {
        Value::None => {
            ctx.record_raise("TypeError", Some("getaddrinfo port must be int or str"));
            return Err(PyError::PyRaise);
        }
        v => match obj_get_int_maybe(ctx, ctx.types, v) {
            Ok(Some(p)) => {
                if !(0..=65535).contains(&p) {
                    ctx.record_raise("OverflowError", Some("port must be 0-65535"));
                    return Err(PyError::PyRaise);
                }
                (p.to_string(), false)
            }
            _ => {
                let service = str_arg(ctx, v)?;
                (String::from_utf8_lossy(&service).into_owned(), true)
            }
        },
    };
    let entries = ffi_getaddrinfo(node.as_deref(), &service, service_is_name, family, type_, proto, flags)
        .map_err(|e| raise_os_error(ctx, map_err(e)))?;
    let types = ctx.types;
    let mut out: Vec<Value> = Vec::new();
    for e in entries {
        // Build #20: `e.family` is the KERNEL family
        // from `ai_family` (30 on macOS for IPv6);
        // translate it back to the Python-facing family
        // (10) BEFORE the filter and the tuple, or an
        // `AF_INET6` request filters everything out and
        // the tuple would carry 30.
        let fam = translate_family_back(e.family);
        match (family, fam) {
            (AF_INET, AF_INET) | (AF_INET6, AF_INET6) | (AF_UNSPEC, _) => {}
            _ => continue,
        }
        let canon = new_str(ctx, types, e.canonname.as_bytes());
        let sockaddr = match e.sockaddr {
            ResolvedAddr::Inet(a) => inet_addr_tuple(ctx, a),
            ResolvedAddr::Unix(p) => new_bytes(ctx, types, &p),
        };
        // type/proto echo the request (see the fn docs).
        let item = new_tuple(
            ctx,
            types.tuple,
            &[small(fam as i64), small(type_ as i64), small(proto as i64), canon, sockaddr],
        );
        out.push(Value::Heap(item));
    }
    Ok(Value::Heap(new_list(ctx, types.list, &out)))
}

/// Resolve `(host, port)` to a list of `SocketAddr`s for
/// `create_connection` / `create_server`. With the
/// feature on, the FFI `getaddrinfo`; with it off,
/// `ToSocketAddrs` (the same resolution the existing
/// `getaddrinfo` uses). `passive` adds `AI_PASSIVE`
/// (wildcard) for `create_server`. `host` is `None` for
/// a null host. An empty result is `OSError(EAFNOSUPPORT)`
/// (no address of the requested family).
fn resolve_addrs(
    ctx: &mut CallCtx,
    host: &Option<Vec<u8>>,
    port_v: Value,
    family: i32,
    passive: bool,
) -> PyResult<Vec<SocketAddr>> {
    let (port_num, service_name): (Option<u16>, Option<String>) =
        match obj_get_int_maybe(ctx, ctx.types, port_v) {
            Ok(Some(p)) => (Some(p as u16), None),
            _ => {
                let s = str_arg(ctx, port_v)?;
                (None, Some(String::from_utf8_lossy(&s).into_owned()))
            }
        };
    #[cfg(feature = "unsafe-ffi")]
    {
        let node = match host {
            None => None,
            Some(b) if b.is_empty() => None,
            Some(b) => Some(String::from_utf8_lossy(b).into_owned()),
        };
        // "" is the wildcard (null node + AI_PASSIVE);
        // None is the loopback (null node, no flag) —
        // unless `passive` (create_server) wants the
        // wildcard for both.
        let flags = if passive || matches!(host, Some(b) if b.is_empty()) {
            AI_PASSIVE
        } else {
            0
        };
        let (service, is_name) = match (&port_num, &service_name) {
            (Some(p), _) => (p.to_string(), false),
            (_, Some(s)) => (s.clone(), true),
            _ => unreachable!("port is int or str (validated by parse_address_tuple)"),
        };
        let entries =
            ffi_getaddrinfo(node.as_deref(), &service, is_name, family, SOCK_STREAM, 0, flags)
                .map_err(|e| raise_os_error(ctx, map_err(e)))?;
        let mut out = Vec::new();
        for e in entries {
            if let ResolvedAddr::Inet(a) = e.sockaddr {
                out.push(a);
            }
        }
        if out.is_empty() {
            return Err(raise_os_error(ctx, EAFNOSUPPORT));
        }
        Ok(out)
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        use std::net::ToSocketAddrs;
        let host_s = match host {
            None => {
                if passive {
                    if family == AF_INET6 {
                        "::".to_string()
                    } else {
                        "0.0.0.0".to_string()
                    }
                } else {
                    "localhost".to_string()
                }
            }
            Some(b) if b.is_empty() => {
                if family == AF_INET6 {
                    "::".to_string()
                } else {
                    "0.0.0.0".to_string()
                }
            }
            Some(b) => String::from_utf8_lossy(b).into_owned(),
        };
        let port = match (port_num, &service_name) {
            (Some(p), _) => p,
            (_, Some(s)) => match resolve_service(s) {
                Some(p) => p,
                None => return Err(raise_os_error(ctx, EAI_NONAME)),
            },
            _ => unreachable!("port is int or str (validated by parse_address_tuple)"),
        };
        let resolved: Vec<SocketAddr> = (host_s.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| io_os_error(ctx, e))?
            .collect();
        let mut out = Vec::new();
        for a in resolved {
            match (family, a) {
                (AF_INET, SocketAddr::V4(_))
                | (AF_INET6, SocketAddr::V6(_))
                | (AF_UNSPEC, _) => out.push(a),
                _ => {}
            }
        }
        if out.is_empty() {
            return Err(raise_os_error(ctx, EAFNOSUPPORT));
        }
        Ok(out)
    }
}

// ── `gethostbyname` family ──────────────────────────────────────────

/// The shared `gethostbyaddr` core (FFI only): resolve
/// `name` (an IP literal, or a hostname resolved via
/// `getaddrinfo`) to an `IpAddr`, reverse-resolve it,
/// and return `(hostname, aliases, ipaddrlist)`.
/// Aliases are always empty (`getnameinfo` has none —
/// CPython's `gethostbyname` does; noted in the file
/// header). On failure returns the `OSError` to raise.
#[cfg(feature = "unsafe-ffi")]
fn gethostbyaddr_inner(ctx: &mut CallCtx, name: &str) -> PyResult<(String, Vec<String>, Vec<String>)> {
    let ip: IpAddr = match name.parse() {
        Ok(ip) => ip,
        Err(_) => {
            // Not an IP literal — resolve it first
            // (CPython accepts hostnames here too).
            let entries = getaddrinfo_owned(Some(name), "0", AF_UNSPEC, SOCK_STREAM, 0, 0)
                .map_err(|e| raise_os_error(ctx, map_err(e)))?;
            let mut found = None;
            for e in entries {
                if let ResolvedAddr::Inet(a) = e.sockaddr {
                    found = Some(a.ip());
                    break;
                }
            }
            match found {
                Some(ip) => ip,
                None => return Err(raise_os_error(ctx, map_err(EAI_NONAME))),
            }
        }
    };
    let addr = match ip {
        IpAddr::V4(v4) => NameInfoAddr::Inet(v4, 0),
        IpAddr::V6(v6) => NameInfoAddr::Inet6(v6, 0, 0, 0),
    };
    let (hostname, _) =
        getnameinfo_owned(&addr, 0).map_err(|e| raise_os_error(ctx, map_err(e)))?;
    Ok((hostname, Vec::new(), vec![ip.to_string()]))
}

/// `socket.gethostbyname(name) -> str`: the first IPv4
/// address for `name` (FFI `getaddrinfo`, family
/// `AF_INET`). A name with no IPv4 address is
/// `OSError(EAI_NONAME)` (CPython's `gethostbyname` is
/// IPv4-only too).
pub(crate) fn socket_gethostbyname(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = str_arg(ctx, args[0])?;
    #[cfg(feature = "unsafe-ffi")]
    {
        let host = String::from_utf8_lossy(&name).into_owned();
        let entries = getaddrinfo_owned(Some(&host), "0", AF_INET, SOCK_STREAM, 0, 0)
            .map_err(|e| raise_os_error(ctx, map_err(e)))?;
        for e in entries {
            if let ResolvedAddr::Inet(a) = e.sockaddr {
                if a.is_ipv4() {
                    return Ok(new_str(ctx, ctx.types, a.ip().to_string().as_bytes()));
                }
            }
        }
        Err(raise_os_error(ctx, map_err(EAI_NONAME)))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = name;
        Err(not_implemented(ctx, "socket.gethostbyname is not implemented"))
    }
}

/// `socket.gethostbyname_ex(name) -> (hostname,
/// aliaslist, ipaddrlist)`: `getaddrinfo` with
/// `AI_CANONNAME` for the canonical hostname, all
/// resolved IPs in the list. The alias list is empty
/// (see the file header).
pub(crate) fn socket_gethostbyname_ex(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = str_arg(ctx, args[0])?;
    #[cfg(feature = "unsafe-ffi")]
    {
        let host = String::from_utf8_lossy(&name).into_owned();
        let entries =
            getaddrinfo_owned(Some(&host), "0", AF_UNSPEC, SOCK_STREAM, 0, AI_CANONNAME)
                .map_err(|e| raise_os_error(ctx, map_err(e)))?;
        if entries.is_empty() {
            return Err(raise_os_error(ctx, map_err(EAI_NONAME)));
        }
        let hostname = if entries[0].canonname.is_empty() {
            host.clone()
        } else {
            entries[0].canonname.clone()
        };
        let mut ips: Vec<Value> = Vec::new();
        for e in &entries {
            if let ResolvedAddr::Inet(a) = e.sockaddr {
                ips.push(new_str(ctx, ctx.types, a.ip().to_string().as_bytes()));
            }
        }
        let aliases = Value::Heap(new_list(ctx, ctx.types.list, &[]));
        let ip_list = Value::Heap(new_list(ctx, ctx.types.list, &ips));
        let host_v = new_str(ctx, ctx.types, hostname.as_bytes());
        Ok(Value::Heap(new_tuple(ctx, ctx.types.tuple, &[host_v, aliases, ip_list])))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = name;
        Err(not_implemented(ctx, "socket.gethostbyname_ex is not implemented"))
    }
}

/// `socket.gethostbyaddr(name) -> (hostname, aliaslist,
/// ipaddrlist)`: the shared core (FFI only).
pub(crate) fn socket_gethostbyaddr(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = str_arg(ctx, args[0])?;
    #[cfg(feature = "unsafe-ffi")]
    {
        let name_s = String::from_utf8_lossy(&name).into_owned();
        let (hostname, aliases, ips) = gethostbyaddr_inner(ctx, &name_s)?;
        let alias_vs: Vec<Value> = aliases
            .iter()
            .map(|a| new_str(ctx, ctx.types, a.as_bytes()))
            .collect();
        let ip_vs: Vec<Value> = ips
            .iter()
            .map(|a| new_str(ctx, ctx.types, a.as_bytes()))
            .collect();
        let host_v = new_str(ctx, ctx.types, hostname.as_bytes());
        let alias_list = Value::Heap(new_list(ctx, ctx.types.list, &alias_vs));
        let ip_list = Value::Heap(new_list(ctx, ctx.types.list, &ip_vs));
        Ok(Value::Heap(new_tuple(ctx, ctx.types.tuple, &[host_v, alias_list, ip_list])))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = name;
        Err(not_implemented(ctx, "socket.gethostbyaddr is not implemented"))
    }
}

/// Parse a `getnameinfo` sockaddr tuple: `(host, port)`
/// or `(host, port, flowinfo, scopeid)` for IPv6. The
/// host may be an IP literal or a hostname (CPython
/// resolves hostnames here via `getaddrinfo`, then
/// reverse-resolves the sockaddr).
#[cfg(feature = "unsafe-ffi")]
fn parse_nameinfo_addr(ctx: &mut CallCtx, v: Value) -> PyResult<NameInfoAddr> {
    let Some(items) = tuple_items(ctx, v) else {
        ctx.record_raise("TypeError", Some("getnameinfo address must be a tuple"));
        return Err(PyError::PyRaise);
    };
    if items.len() != 2 && items.len() != 4 {
        ctx.record_raise(
            "TypeError",
            Some("getnameinfo address must be a (host, port) or (host, port, flowinfo, scopeid) tuple"),
        );
        return Err(PyError::PyRaise);
    }
    // Copy the items out first: `tuple_items` borrows
    // `ctx`, and `str_arg`/`int_arg` need `&mut ctx`
    // (the same copy-out pattern `parse_address_tuple`
    // uses).
    let is_v6 = items.len() == 4;
    let (host_v, port_v, flow_v, scope_v) = if is_v6 {
        (items[0], items[1], Some(items[2]), Some(items[3]))
    } else {
        (items[0], items[1], None, None)
    };
    let host = str_arg(ctx, host_v)?;
    let port = int_arg(ctx, port_v)?;
    if !(0..=65535).contains(&port) {
        ctx.record_raise("OverflowError", Some("port must be 0-65535"));
        return Err(PyError::PyRaise);
    }
    let (flow, scope) = match (flow_v, scope_v) {
        (Some(f), Some(s)) => (int_arg(ctx, f)? as u32, int_arg(ctx, s)? as u32),
        _ => (0, 0),
    };
    let host_s = String::from_utf8_lossy(&host).into_owned();
    let ip: IpAddr = match host_s.parse() {
        Ok(ip) => ip,
        Err(_) => {
            let entries = getaddrinfo_owned(
                Some(&host_s),
                &port.to_string(),
                AF_UNSPEC,
                SOCK_STREAM,
                0,
                0,
            )
            .map_err(|e| raise_os_error(ctx, map_err(e)))?;
            let mut found = None;
            for e in entries {
                if let ResolvedAddr::Inet(a) = e.sockaddr {
                    found = Some(a.ip());
                    break;
                }
            }
            match found {
                Some(ip) => ip,
                None => return Err(raise_os_error(ctx, map_err(EAI_NONAME))),
            }
        }
    };
    Ok(match ip {
        IpAddr::V4(v4) => NameInfoAddr::Inet(v4, port as u16),
        IpAddr::V6(v6) => NameInfoAddr::Inet6(v6, port as u16, flow, scope),
    })
}

/// `socket.getnameinfo(sockaddr, flags) -> (host,
/// service)`: `getnameinfo(3)` on the parsed sockaddr;
/// `flags` pass through (`NI_NUMERICHOST` etc.).
pub(crate) fn socket_getnameinfo(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let flags = int_arg(ctx, args[1])? as i32;
    #[cfg(feature = "unsafe-ffi")]
    {
        let addr = parse_nameinfo_addr(ctx, args[0])?;
        let (host, serv) =
            getnameinfo_owned(&addr, flags).map_err(|e| raise_os_error(ctx, map_err(e)))?;
        let host_v = new_str(ctx, ctx.types, host.as_bytes());
        let serv_v = new_str(ctx, ctx.types, serv.as_bytes());
        Ok(Value::Heap(new_tuple(ctx, ctx.types.tuple, &[host_v, serv_v])))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = flags;
        Err(not_implemented(ctx, "socket.getnameinfo is not implemented"))
    }
}

/// `socket.gethostname() -> str`: `gethostname(2)`.
pub(crate) fn socket_gethostname(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    #[cfg(feature = "unsafe-ffi")]
    {
        let _ = args;
        let name = gethostname_owned().map_err(|e| raise_os_error(ctx, map_err(e)))?;
        Ok(new_str(ctx, ctx.types, name.as_bytes()))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = args;
        Err(not_implemented(ctx, "socket.gethostname is not implemented"))
    }
}

/// `socket.getfqdn([name]) -> str` (CPython semantics):
/// an empty name (or `"0.0.0.0"`) is the local hostname;
/// the reverse lookup's hostname wins (the first
/// candidate containing a `'.'`, else the hostname
/// itself); on lookup failure the name is returned
/// as-is.
pub(crate) fn socket_getfqdn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name_arg = match args.first() {
        Some(v) => str_arg(ctx, *v)?,
        None => Vec::new(),
    };
    #[cfg(feature = "unsafe-ffi")]
    {
        let mut name = String::from_utf8_lossy(&name_arg).trim().to_owned();
        if name.is_empty() || name == "0.0.0.0" {
            name = gethostname_owned().map_err(|e| raise_os_error(ctx, map_err(e)))?;
        }
        // The reverse lookup; on failure the name is
        // returned as-is (CPython's `except error: pass`).
        // The only candidate is the hostname itself (the
        // alias list is empty — see the file header), so
        // it wins whether or not it contains a '.'.
        if let Ok((hostname, _, _)) = gethostbyaddr_inner(ctx, &name) {
            name = hostname;
        }
        Ok(new_str(ctx, ctx.types, name.as_bytes()))
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let _ = name_arg;
        Err(not_implemented(ctx, "socket.getfqdn is not implemented"))
    }
}

// ── `getservbyname` / `getservbyport` ───────────────────────────────

/// The built-in service table (name → port) WITHOUT the
/// numeric-string fallback (for `getservbyname`, where a
/// numeric string is not a service name — CPython raises
/// `OSError` for `getservbyname("80")`, while
/// `resolve_service` parses it as port 80).
fn builtin_service_port(name: &str) -> Option<u16> {
    Some(match name {
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

/// `socket.getservbyname(name[, proto]) -> int`: the
/// FFI `getservbyname` first, then the built-in table,
/// then `OSError("service/proto not found")` (CPython's
/// message). A non-`"tcp"` / `"udp"` proto skips the
/// table fallback (the C call already rejected it).
pub(crate) fn socket_getservbyname(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = str_arg(ctx, args[0])?;
    let proto = match args.get(1) {
        Some(v) => Some(str_arg(ctx, *v)?),
        None => None,
    };
    let name_s = String::from_utf8_lossy(&name).into_owned();
    let proto_s = proto.as_ref().map(|p| String::from_utf8_lossy(p).into_owned());
    #[cfg(feature = "unsafe-ffi")]
    {
        if let Some(port) = getservbyname_owned(&name_s, proto_s.as_deref()) {
            return Ok(small(port as i64));
        }
    }
    let proto_ok = proto_s
        .as_deref()
        .map(|p| p == "tcp" || p == "udp")
        .unwrap_or(true);
    if proto_ok {
        if let Some(port) = builtin_service_port(&name_s) {
            return Ok(small(port as i64));
        }
    }
    ctx.record_raise("OSError", Some("service/proto not found"));
    Err(PyError::PyRaise)
}

/// `socket.getservbyport(port[, proto]) -> str`: the FFI
/// `getservbyport` first, then the built-in table, then
/// `OSError("port/proto not found")`.
pub(crate) fn socket_getservbyport(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let port = int_arg(ctx, args[0])?;
    if !(0..=65535).contains(&port) {
        ctx.record_raise("OverflowError", Some("port must be 0-65535"));
        return Err(PyError::PyRaise);
    }
    let proto = match args.get(1) {
        Some(v) => Some(str_arg(ctx, *v)?),
        None => None,
    };
    let proto_s = proto.as_ref().map(|p| String::from_utf8_lossy(p).into_owned());
    #[cfg(feature = "unsafe-ffi")]
    {
        if let Some(name) = getservbyport_owned(port as u16, proto_s.as_deref()) {
            return Ok(new_str(ctx, ctx.types, name.as_bytes()));
        }
    }
    let proto_ok = proto_s
        .as_deref()
        .map(|p| p == "tcp" || p == "udp")
        .unwrap_or(true);
    if proto_ok {
        if let Some(name) = service_by_port(port as u16) {
            return Ok(new_str(ctx, ctx.types, name.as_bytes()));
        }
    }
    ctx.record_raise("OSError", Some("port/proto not found"));
    Err(PyError::PyRaise)
}

// ── `create_connection` / `create_server` ───────────────────────────

/// Root a temporary `Value` across allocations (see
/// `RootSet.extra`): a socket created inside
/// `create_connection` / `create_server` / `socketpair`
/// is not reachable from the caller's frame until
/// returned, and an allocation-triggered GC would
/// otherwise collect it (running its `close` finaliser
/// mid-attempt). `unroot_temporary` removes exactly one
/// occurrence (robust against interleaved pushes by
/// callees).
fn root_temporary(ctx: &mut CallCtx, v: Value) {
    ctx.roots.push(v);
}

fn unroot_temporary(ctx: &mut CallCtx, v: Value) {
    if let Some(pos) = ctx.roots.extra.iter().position(|w| *w == v) {
        ctx.roots.extra.remove(pos);
    }
}

/// Save a just-recorded pending raise as the first /
/// last error of a multi-address attempt (CPython tries
/// each `getaddrinfo` result in turn and raises one
/// error at the end). Takes (clears) the pending raise.
fn save_attempt_error(
    ctx: &mut CallCtx,
    first: &mut Option<crate::memory::gc::PendingRaise>,
    last: &mut Option<crate::memory::gc::PendingRaise>,
) {
    if let Some(r) = ctx.take_pending_raise() {
        if first.is_none() {
            *first = Some(r);
        } else {
            *last = Some(r);
        }
    }
}

/// Re-raise the saved attempt error: the first error
/// when `all_errors` is set, else the last (CPython's
/// `create_connection` default). Unreachable in practice
/// (`resolve_addrs` guarantees at least one address).
fn raise_attempt_error(
    ctx: &mut CallCtx,
    first: Option<crate::memory::gc::PendingRaise>,
    last: Option<crate::memory::gc::PendingRaise>,
    all_errors: bool,
) -> PyError {
    let to_raise = if all_errors { first.or(last) } else { last.or(first) };
    match to_raise {
        Some(r) => {
            *ctx.pending_raise = Some(r);
            PyError::PyRaise
        }
        None => {
            ctx.record_raise("OSError", Some("could not connect"));
            PyError::PyRaise
        }
    }
}

/// `socket.create_connection(address[, timeout[,
/// source_address]][, all_errors])` (CPython semantics):
/// resolve the address, then try each resolved address
/// in turn (create → settimeout → bind `source_address`
/// → connect); the first success wins. By default the
/// LAST error is raised; with `all_errors` set, the
/// FIRST error is raised (CPython 3.12's `all_errors` —
/// this VM has no `ExceptionGroup`, so the first error
/// is the functional approximation; noted in the result
/// file). `timeout` is `None` (the arg was omitted →
/// the module default, already applied by
/// `socket_ctor`), `Some(None)` (blocking) or
/// `Some(Some(seconds))`.
fn create_connection_impl(
    ctx: &mut CallCtx,
    host: &Option<Vec<u8>>,
    port_v: Value,
    timeout: Option<Option<f64>>,
    source_address: &Option<(Option<Vec<u8>>, Value)>,
    all_errors: bool,
) -> PyResult<Value> {
    let addrs = resolve_addrs(ctx, host, port_v, AF_UNSPEC, false)?;
    let mut first_err: Option<crate::memory::gc::PendingRaise> = None;
    let mut last_err: Option<crate::memory::gc::PendingRaise> = None;
    for addr in &addrs {
        let family = if addr.is_ipv4() { AF_INET } else { AF_INET6 };
        let sock = match socket_ctor(
            ctx,
            &[small(family as i64), small(SOCK_STREAM as i64), small(0)],
        ) {
            Ok(s) => s,
            Err(_) => {
                save_attempt_error(ctx, &mut first_err, &mut last_err);
                continue;
            }
        };
        root_temporary(ctx, sock);
        // Hoist `new_str` out: it cannot nest inside the
        // `new_tuple` call — both borrow `ctx` mutably.
        let host_v = new_str(ctx, ctx.types, addr.ip().to_string().as_bytes());
        let addr_v = Value::Heap(new_tuple(
            ctx,
            ctx.types.tuple,
            &[host_v, small(addr.port() as i64)],
        ));
        let mut attempt_ok = true;
        match timeout {
            // Omitted: socket_ctor applied the module default.
            None => {}
            Some(None) => {
                if sock_settimeout(ctx, &[sock, Value::None]).is_err() {
                    attempt_ok = false;
                }
            }
            Some(Some(f)) => {
                let f_v = new_float(ctx, ctx.types, f);
                if sock_settimeout(ctx, &[sock, f_v]).is_err() {
                    attempt_ok = false;
                }
            }
        }
        if attempt_ok {
            if let Some((src_host, src_port_v)) = source_address {
                let host_v = match src_host {
                    None => Value::None,
                    Some(b) => new_str(ctx, ctx.types, b),
                };
                let src_v =
                    Value::Heap(new_tuple(ctx, ctx.types.tuple, &[host_v, *src_port_v]));
                if sock_bind(ctx, &[sock, src_v]).is_err() {
                    attempt_ok = false;
                }
            }
        }
        if attempt_ok && sock_connect(ctx, &[sock, addr_v]).is_err() {
            attempt_ok = false;
        }
        if attempt_ok {
            unroot_temporary(ctx, sock);
            return Ok(sock);
        }
        // Failed: close, clear the raise, remember it,
        // try the next address.
        let _ = sock_close(ctx, &[sock]);
        unroot_temporary(ctx, sock);
        save_attempt_error(ctx, &mut first_err, &mut last_err);
    }
    Err(raise_attempt_error(ctx, first_err, last_err, all_errors))
}

/// Parse the `create_connection` arguments (positionals
/// + keywords) and run the core. `timeout` /
/// `source_address` / `all_errors` are keyword-capable
/// (CPython: `timeout` and `source_address` are
/// positional-or-keyword, `all_errors` keyword-only —
/// this implementation also accepts `all_errors`
/// positionally, a harmless superset).
fn create_connection_args(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let (host, port_v) = parse_address_tuple(ctx, args[0])?;
    let timeout_id = qid(ctx, b"timeout");
    let source_id = qid(ctx, b"source_address");
    let all_errors_id = qid(ctx, b"all_errors");
    let mut timeout: Option<Option<f64>> = None;
    let mut source_address: Option<(Option<Vec<u8>>, Value)> = None;
    let mut all_errors = false;
    if let Some(v) = args.get(1) {
        timeout = Some(parse_timeout(ctx, *v)?);
    }
    if let Some(v) = args.get(2) {
        source_address = Some(parse_address_tuple(ctx, *v)?);
    }
    for (name, v) in kwargs {
        if *name == timeout_id {
            if timeout.is_some() {
                ctx.record_raise(
                    "TypeError",
                    Some("create_connection() got multiple values for keyword argument 'timeout'"),
                );
                return Err(PyError::PyRaise);
            }
            timeout = Some(parse_timeout(ctx, *v)?);
        } else if *name == source_id {
            if source_address.is_some() {
                ctx.record_raise(
                    "TypeError",
                    Some("create_connection() got multiple values for keyword argument 'source_address'"),
                );
                return Err(PyError::PyRaise);
            }
            source_address = Some(parse_address_tuple(ctx, *v)?);
        } else if *name == all_errors_id {
            all_errors = int_arg(ctx, *v)? != 0;
        } else {
            let kw_name = {
                let id16 = u16::try_from(*name).unwrap_or(0);
                if (id16 as usize) < ctx.pool.total() {
                    String::from_utf8_lossy(ctx.pool.qstr_str(id16)).into_owned()
                } else {
                    format!("<{name}>")
                }
            };
            ctx.record_raise(
                "TypeError",
                Some(&format!(
                    "create_connection() got an unexpected keyword argument '{kw_name}'"
                )),
            );
            return Err(PyError::PyRaise);
        }
    }
    create_connection_impl(ctx, &host, port_v, timeout, &source_address, all_errors)
}

/// `socket.create_connection` as a plain `BuiltinFn`
/// (the no-kw form — the call machinery routes through
/// `kw_func` when one is present, as for `list.sort`).
pub(crate) fn socket_create_connection(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    create_connection_args(ctx, args, &[])
}

/// `socket.create_connection` as a `BuiltinKwFn` adapter.
pub(crate) fn socket_create_connection_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    create_connection_args(ctx, args, kwargs)
}

/// `socket.create_server(address[, family[, backlog[,
/// reuse_port[, dualstack_ipv6]]]])` (CPython
/// semantics): resolve with `AI_PASSIVE`, then try each
/// address (create → `SO_REUSEADDR` → bind → listen →
/// `setblocking(False)`); the first success wins, else
/// the last error is raised. `reuse_port` failures are
/// ignored (CPython catches them); `IPV6_V6ONLY`
/// failures are not (CPython propagates them).
/// `backlog=None` is `SOMAXCONN`. All parameters after
/// `address` are keyword-only in CPython — this
/// implementation also accepts them positionally, a
/// harmless superset.
/// `create_server` for `AF_UNIX`: the address is a
/// path (str/bytes), not a `(host, port)` tuple, and
/// there is no `getaddrinfo` step (CPython binds the
/// path directly). Unix only — `AF_UNIX` is
/// `NotImplementedError` on Windows (see
/// `create_server_args`).
#[cfg(unix)]
fn create_server_unix_impl(
    ctx: &mut CallCtx,
    path: &[u8],
    backlog: Option<i64>,
    reuse_port: bool,
) -> PyResult<Value> {
    let sock = socket_ctor(
        ctx,
        &[small(AF_UNIX as i64), small(SOCK_STREAM as i64), small(0)],
    )?;
    root_temporary(ctx, sock);
    let path_v = new_str(ctx, ctx.types, path);
    let mut first_err: Option<crate::memory::gc::PendingRaise> = None;
    let mut last_err: Option<crate::memory::gc::PendingRaise> = None;
    let mut attempt_ok = true;
    if reuse_port {
        // CPython ignores a failed SO_REUSEPORT.
        if sock_setsockopt(
            ctx,
            &[sock, small(SOL_SOCKET as i64), small(SO_REUSEPORT as i64), small(1)],
        )
        .is_err()
        {
            let _ = ctx.take_pending_raise();
        }
    }
    if attempt_ok
        && sock_setsockopt(
            ctx,
            &[sock, small(SOL_SOCKET as i64), small(SO_REUSEADDR as i64), small(1)],
        )
        .is_err()
    {
        attempt_ok = false;
    }
    if attempt_ok && sock_bind(ctx, &[sock, path_v]).is_err() {
        attempt_ok = false;
    }
    if attempt_ok {
        let backlog_v = small(backlog.unwrap_or(SOMAXCONN as i64));
        if sock_listen(ctx, &[sock, backlog_v]).is_err() {
            attempt_ok = false;
        }
    }
    if attempt_ok && sock_setblocking(ctx, &[sock, small(0)]).is_err() {
        attempt_ok = false;
    }
    if attempt_ok {
        unroot_temporary(ctx, sock);
        return Ok(sock);
    }
    let _ = sock_close(ctx, &[sock]);
    unroot_temporary(ctx, sock);
    save_attempt_error(ctx, &mut first_err, &mut last_err);
    Err(raise_attempt_error(ctx, first_err, last_err, false))
}

/// The `AF_INET`/`AF_INET6` core of `create_server`:
/// resolve the address(es), then try each in turn
/// (bind + listen + `setblocking(False)`).
fn create_server_impl(
    ctx: &mut CallCtx,
    host: &Option<Vec<u8>>,
    port_v: Value,
    family: i32,
    backlog: Option<i64>,
    reuse_port: bool,
    dualstack_ipv6: bool,
) -> PyResult<Value> {
    match family {
        AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    let addrs = resolve_addrs(ctx, host, port_v, family, true)?;
    let mut first_err: Option<crate::memory::gc::PendingRaise> = None;
    let mut last_err: Option<crate::memory::gc::PendingRaise> = None;
    for addr in &addrs {
        let sock = match socket_ctor(
            ctx,
            &[small(family as i64), small(SOCK_STREAM as i64), small(0)],
        ) {
            Ok(s) => s,
            Err(_) => {
                save_attempt_error(ctx, &mut first_err, &mut last_err);
                continue;
            }
        };
        root_temporary(ctx, sock);
        // Hoist `new_str` out: it cannot nest inside the
        // `new_tuple` call — both borrow `ctx` mutably.
        let host_v = new_str(ctx, ctx.types, addr.ip().to_string().as_bytes());
        let addr_v = Value::Heap(new_tuple(
            ctx,
            ctx.types.tuple,
            &[host_v, small(addr.port() as i64)],
        ));
        let mut attempt_ok = true;
        if reuse_port {
            // CPython ignores a failed SO_REUSEPORT.
            #[cfg(unix)]
            {
                if sock_setsockopt(
                    ctx,
                    &[sock, small(SOL_SOCKET as i64), small(SO_REUSEPORT as i64), small(1)],
                )
                .is_err()
                {
                    let _ = ctx.take_pending_raise();
                }
            }
            #[cfg(windows)]
            {
                // No SO_REUSEPORT on Winsock; ignored.
            }
        }
        if attempt_ok && family == AF_INET6 && dualstack_ipv6 {
            if sock_setsockopt(
                ctx,
                &[sock, small(IPPROTO_IPV6 as i64), small(IPV6_V6ONLY as i64), small(0)],
            )
            .is_err()
            {
                attempt_ok = false;
            }
        }
        if attempt_ok
            && sock_setsockopt(
                ctx,
                &[sock, small(SOL_SOCKET as i64), small(SO_REUSEADDR as i64), small(1)],
            )
            .is_err()
        {
            attempt_ok = false;
        }
        if attempt_ok && sock_bind(ctx, &[sock, addr_v]).is_err() {
            attempt_ok = false;
        }
        if attempt_ok {
            let backlog_v = small(backlog.unwrap_or(SOMAXCONN as i64));
            if sock_listen(ctx, &[sock, backlog_v]).is_err() {
                attempt_ok = false;
            }
        }
        if attempt_ok && sock_setblocking(ctx, &[sock, small(0)]).is_err() {
            attempt_ok = false;
        }
        if attempt_ok {
            unroot_temporary(ctx, sock);
            return Ok(sock);
        }
        let _ = sock_close(ctx, &[sock]);
        unroot_temporary(ctx, sock);
        save_attempt_error(ctx, &mut first_err, &mut last_err);
    }
    Err(raise_attempt_error(ctx, first_err, last_err, false))
}

/// Parse the `create_server` arguments (positionals +
/// keywords) and run the core.
fn create_server_args(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    // Parse the options first: the address shape
    // depends on the family (a path for `AF_UNIX`, a
    // `(host, port)` tuple otherwise), so the address
    // is parsed after the family is known.
    let family_id = qid(ctx, b"family");
    let backlog_id = qid(ctx, b"backlog");
    let reuse_port_id = qid(ctx, b"reuse_port");
    let dualstack_id = qid(ctx, b"dualstack_ipv6");
    let mut family = AF_INET;
    let mut backlog: Option<i64> = None;
    let mut reuse_port = false;
    let mut dualstack_ipv6 = false;
    let mut family_set = false;
    let mut backlog_set = false;
    let mut reuse_port_set = false;
    let mut dualstack_set = false;
    if let Some(v) = args.get(1) {
        family = int_arg(ctx, *v)? as i32;
        family_set = true;
    }
    if let Some(v) = args.get(2) {
        backlog = match *v {
            Value::None => None,
            v => Some(int_arg(ctx, v)?),
        };
        backlog_set = true;
    }
    if let Some(v) = args.get(3) {
        reuse_port = int_arg(ctx, *v)? != 0;
        reuse_port_set = true;
    }
    if let Some(v) = args.get(4) {
        dualstack_ipv6 = int_arg(ctx, *v)? != 0;
        dualstack_set = true;
    }
    for (name, v) in kwargs {
        if *name == family_id {
            if family_set {
                ctx.record_raise(
                    "TypeError",
                    Some("create_server() got multiple values for keyword argument 'family'"),
                );
                return Err(PyError::PyRaise);
            }
            family = int_arg(ctx, *v)? as i32;
            family_set = true;
        } else if *name == backlog_id {
            if backlog_set {
                ctx.record_raise(
                    "TypeError",
                    Some("create_server() got multiple values for keyword argument 'backlog'"),
                );
                return Err(PyError::PyRaise);
            }
            backlog = match *v {
                Value::None => None,
                v => Some(int_arg(ctx, v)?),
            };
            backlog_set = true;
        } else if *name == reuse_port_id {
            if reuse_port_set {
                ctx.record_raise(
                    "TypeError",
                    Some("create_server() got multiple values for keyword argument 'reuse_port'"),
                );
                return Err(PyError::PyRaise);
            }
            reuse_port = int_arg(ctx, *v)? != 0;
            reuse_port_set = true;
        } else if *name == dualstack_id {
            if dualstack_set {
                ctx.record_raise(
                    "TypeError",
                    Some("create_server() got multiple values for keyword argument 'dualstack_ipv6'"),
                );
                return Err(PyError::PyRaise);
            }
            dualstack_ipv6 = int_arg(ctx, *v)? != 0;
            dualstack_set = true;
        } else {
            let kw_name = {
                let id16 = u16::try_from(*name).unwrap_or(0);
                if (id16 as usize) < ctx.pool.total() {
                    String::from_utf8_lossy(ctx.pool.qstr_str(id16)).into_owned()
                } else {
                    format!("<{name}>")
                }
            };
            ctx.record_raise(
                "TypeError",
                Some(&format!(
                    "create_server() got an unexpected keyword argument '{kw_name}'"
                )),
            );
            return Err(PyError::PyRaise);
        }
    }
    // Parse the address now that the family is known:
    // a path for `AF_UNIX`, a `(host, port)` tuple
    // otherwise (CPython's `create_server` accepts both
    // shapes).
    if family == AF_UNIX {
        #[cfg(unix)]
        {
            let path = str_arg(ctx, args[0])?;
            return create_server_unix_impl(ctx, &path, backlog, reuse_port);
        }
        #[cfg(windows)]
        {
            return Err(not_implemented(ctx, "AF_UNIX is not implemented on Windows"));
        }
    }
    let (host, port_v) = parse_address_tuple(ctx, args[0])?;
    create_server_impl(ctx, &host, port_v, family, backlog, reuse_port, dualstack_ipv6)
}

/// `socket.create_server` as a plain `BuiltinFn`.
pub(crate) fn socket_create_server(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    create_server_args(ctx, args, &[])
}

/// `socket.create_server` as a `BuiltinKwFn` adapter.
pub(crate) fn socket_create_server_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    create_server_args(ctx, args, kwargs)
}

// ── Default timeout ─────────────────────────────────────────────────

/// `socket.getdefaulttimeout() -> float | None`: the
/// module default (initially `None` = blocking).
pub(crate) fn socket_getdefaulttimeout(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let _ = args;
    match default_timeout() {
        None => Ok(Value::None),
        Some(d) => Ok(new_float(ctx, ctx.types, d.as_secs_f64())),
    }
}

/// `socket.setdefaulttimeout(timeout) -> None`: sets the
/// module default for subsequently created sockets
/// (`None` = blocking). Validated like `settimeout`.
pub(crate) fn socket_setdefaulttimeout(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let secs = parse_timeout(ctx, args[0])?;
    set_default_timeout(secs.map(Duration::from_secs_f64));
    Ok(Value::None)
}

// ── `socketpair` ────────────────────────────────────────────────────

/// Counter for unique `socketpair` temp paths (Unix).
fn socketpair_counter() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Wrap a connected `TcpStream` as a socket instance
/// (`StreamTcp` state + a fresh table entry).
fn wrap_stream_tcp(
    ctx: &mut CallCtx,
    s: TcpStream,
    family: i32,
    type_: i32,
    proto: i32,
) -> PyResult<Value> {
    let key = sock_key(&s);
    sockets().insert(key, SocketState::StreamTcp(s));
    alloc_socket(ctx, key, family, type_, proto)
}

/// Loopback TCP pair (INET / INET6, all platforms) —
/// the same approach CPython uses on Windows and for
/// non-`AF_UNIX` families.
fn socketpair_tcp(ctx: &mut CallCtx, family: i32, type_: i32, proto: i32) -> PyResult<Value> {
    let ip: IpAddr = if family == AF_INET6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let listener = TcpListener::bind(SocketAddr::new(ip, 0)).map_err(|e| io_os_error(ctx, e))?;
    let port = listener.local_addr().map_err(|e| io_os_error(ctx, e))?.port();
    let c1 = TcpStream::connect(SocketAddr::new(ip, port)).map_err(|e| io_os_error(ctx, e))?;
    let (c2, _) = listener.accept().map_err(|e| io_os_error(ctx, e))?;
    drop(listener);
    let s1 = wrap_stream_tcp(ctx, c1, family, type_, proto)?;
    let s2 = wrap_stream_tcp(ctx, c2, family, type_, proto)?;
    root_temporary(ctx, s1);
    root_temporary(ctx, s2);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[s1, s2]);
    unroot_temporary(ctx, s1);
    unroot_temporary(ctx, s2);
    Ok(Value::Heap(tuple))
}

/// `AF_UNIX` pair on Unix: a temporary filesystem path
/// (bind → connect → accept → unlink). `std` has no
/// stable abstract-socket constructor
/// (`SocketAddr::from_abstract_name` is still unstable
/// on rustc 1.99.1), and the FFI `socketpair(2)` belongs
/// to S1's `sys_socketpair` / `FfiSocket::from_raw`,
/// which are not in this worktree yet; the temp-path
/// pair is functionally identical and is replaced by
/// the real call when those land (noted in the result
/// file).
#[cfg(unix)]
fn socketpair_unix(ctx: &mut CallCtx, family: i32, type_: i32, proto: i32) -> PyResult<Value> {
    let n = socketpair_counter();
    let path =
        std::env::temp_dir().join(format!("petitpy-sp-{}-{}", std::process::id(), n));
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return Err(io_os_error(ctx, e)),
    };
    let c1 = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(io_os_error(ctx, e));
        }
    };
    let c2 = match listener.accept() {
        Ok((s, _)) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(io_os_error(ctx, e));
        }
    };
    drop(listener);
    let _ = std::fs::remove_file(&path);
    let key1 = sock_key(&c1);
    sockets().insert(key1, SocketState::StreamUnix(c1));
    let s1 = alloc_socket(ctx, key1, family, type_, proto)?;
    let key2 = sock_key(&c2);
    sockets().insert(key2, SocketState::StreamUnix(c2));
    let s2 = alloc_socket(ctx, key2, family, type_, proto)?;
    root_temporary(ctx, s1);
    root_temporary(ctx, s2);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[s1, s2]);
    unroot_temporary(ctx, s1);
    unroot_temporary(ctx, s2);
    Ok(Value::Heap(tuple))
}

/// `socket.socketpair([family[, type[, proto]]]) ->
/// (sock, sock)`: a pair of connected stream sockets.
/// Only `SOCK_STREAM` is supported (a datagram pair is
/// `EOPNOTSUPP` — CPython's loopback path is stream-only
/// too). INET / INET6 use a loopback TCP pair;
/// `AF_UNIX` on Unix uses a temporary path (see
/// `socketpair_unix`); `AF_UNIX` on Windows is
/// `NotImplementedError` (as for `socket()`).
pub(crate) fn socket_socketpair(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let family = match args.first() {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => AF_UNIX,
    };
    let type_ = match args.get(1) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => SOCK_STREAM,
    };
    let proto = match args.get(2) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    match family {
        AF_UNIX | AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    if type_ != SOCK_STREAM {
        return Err(raise_os_error(ctx, EOPNOTSUPP));
    }
    if !(proto == 0 || proto == IPPROTO_TCP) {
        return Err(raise_os_error(ctx, EPROTONOSUPPORT));
    }
    if family == AF_UNIX {
        #[cfg(windows)]
        {
            return Err(not_implemented(ctx, "AF_UNIX is not supported on Windows"));
        }
        #[cfg(unix)]
        {
            return socketpair_unix(ctx, family, type_, proto);
        }
    }
    socketpair_tcp(ctx, family, type_, proto)
}

// ── Byte order + address tools ──────────────────────────────────────

/// `socket.htonl(x)` / `ntohl(x)` / `htons(x)` /
/// `ntohs(x)`: pure-Rust byte swaps (no FFI — these are
/// macros, not syscalls). The input must be a
/// non-negative int (a negative value is `OverflowError`,
/// as in CPython's `PyLong_AsUnsignedLong`); values are
/// masked to the width (the Linux behaviour — CPython
/// truncates to `uint32_t` / `uint16_t` there; Windows
/// raises for out-of-range `unsigned long`, a platform
/// difference, noted in the result file).
fn byte_swap(ctx: &mut CallCtx, args: &[Value], bits: u32) -> PyResult<Value> {
    let n = int_arg(ctx, args[0])?;
    if n < 0 {
        ctx.record_raise(
            "OverflowError",
            Some("can't convert negative value to unsigned int"),
        );
        return Err(PyError::PyRaise);
    }
    let out = if bits == 32 {
        (n as u32).to_be() as i64
    } else {
        (n as u16).to_be() as i64
    };
    Ok(small(out))
}

pub(crate) fn socket_htonl(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    byte_swap(ctx, args, 32)
}

pub(crate) fn socket_ntohl(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    byte_swap(ctx, args, 32)
}

pub(crate) fn socket_htons(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    byte_swap(ctx, args, 16)
}

pub(crate) fn socket_ntohs(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    byte_swap(ctx, args, 16)
}

/// `socket.inet_aton(ip) -> bytes`: 4 packed octets,
/// including the short forms (`"127.1"`, `"0x7f.1"`).
/// On Unix with the feature on, the FFI `inet_aton`;
/// otherwise the pure-Rust parser (also used on
/// Windows, which has no `inet_aton`). An invalid
/// string is `OSError("illegal IP address string passed
/// to inet_aton")` (CPython's message).
pub(crate) fn socket_inet_aton(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let ip = str_arg(ctx, args[0])?;
    let ip_s = String::from_utf8_lossy(&ip).into_owned();
    // FFI on Unix + feature (trust it; it parses the
    // short forms too), else the pure-Rust parser. The
    // cfg'd block yields one `Option` in both builds,
    // so there is no unreachable fallthrough.
    let bytes: Option<[u8; 4]> = {
        #[cfg(all(feature = "unsafe-ffi", unix))]
        {
            inet_aton_owned(&ip_s).ok()
        }
        #[cfg(not(all(feature = "unsafe-ffi", unix)))]
        {
            parse_inet_aton(&ip_s)
        }
    };
    match bytes {
        Some(b) => Ok(new_bytes(ctx, ctx.types, &b)),
        None => {
            ctx.record_raise(
                "OSError",
                Some("illegal IP address string passed to inet_aton"),
            );
            Err(PyError::PyRaise)
        }
    }
}

/// `socket.inet_ntoa(packed) -> str`: 4 packed octets
/// as a dotted quad. A wrong length is `ValueError`
/// (CPython). On the feature build the FFI `inet_ntoa`,
/// otherwise `Ipv4Addr` formatting (same result).
pub(crate) fn socket_inet_ntoa(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let packed = str_arg(ctx, args[0])?;
    if packed.len() != 4 {
        ctx.record_raise("ValueError", Some("packed IP wrong length for inet_ntoa"));
        return Err(PyError::PyRaise);
    }
    let mut octets = [0u8; 4];
    octets.copy_from_slice(&packed);
    // FFI vs `std` formatting, one `String` in both
    // builds (the cfg'd block yields the value).
    let s: String = {
        #[cfg(feature = "unsafe-ffi")]
        {
            inet_ntoa_owned(&octets).map_err(|e| raise_os_error(ctx, map_err(e)))?
        }
        #[cfg(not(feature = "unsafe-ffi"))]
        {
            Ipv4Addr::from(octets).to_string()
        }
    };
    Ok(new_str(ctx, ctx.types, s.as_bytes()))
}

/// `socket.inet_pton(af, ip) -> bytes`: the packed form
/// (4 / 16 bytes). A bad family is `OSError(EAFNOSUPPORT)`;
/// an invalid string is `OSError("illegal IP address
/// string passed to inet_pton")` (CPython's message).
/// On the feature build the FFI `inet_pton`, otherwise
/// `std` parsing (the two agree on all valid inputs —
/// both are strict, both reject leading zeros).
pub(crate) fn socket_inet_pton(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let af = int_arg(ctx, args[0])? as i32;
    let ip = str_arg(ctx, args[1])?;
    let ip_s = String::from_utf8_lossy(&ip).into_owned();
    match af {
        AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    // FFI (trust it) vs the `std` parser; `Err(0)` is
    // the "invalid string" sentinel (`inet_pton` returns
    // 0 without setting errno). One `Result` in both
    // builds, so there is no unreachable fallthrough.
    let bytes: Result<Vec<u8>, i32> = {
        #[cfg(feature = "unsafe-ffi")]
        {
            inet_pton_owned(af, &ip_s)
        }
        #[cfg(not(feature = "unsafe-ffi"))]
        {
            match af {
                AF_INET => ip_s
                    .parse::<Ipv4Addr>()
                    .map(|v| v.octets().to_vec())
                    .map_err(|_| 0),
                AF_INET6 => ip_s
                    .parse::<Ipv6Addr>()
                    .map(|v| v.octets().to_vec())
                    .map_err(|_| 0),
                _ => Err(EAFNOSUPPORT),
            }
        }
    };
    match bytes {
        Ok(b) => Ok(new_bytes(ctx, ctx.types, &b)),
        Err(0) => {
            ctx.record_raise(
                "OSError",
                Some("illegal IP address string passed to inet_pton"),
            );
            Err(PyError::PyRaise)
        }
        Err(e) => Err(raise_os_error(ctx, map_err(e))),
    }
}

/// `socket.inet_ntop(af, packed) -> str`: the
/// presentation form. A bad family is
/// `OSError(EAFNOSUPPORT)`; a wrong length is
/// `ValueError` (CPython). On the feature build the FFI
/// `inet_ntop`, otherwise `std` formatting (both follow
/// RFC 5952's shortest form for IPv6).
pub(crate) fn socket_inet_ntop(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let af = int_arg(ctx, args[0])? as i32;
    let packed = str_arg(ctx, args[1])?;
    match af {
        AF_INET | AF_INET6 => {}
        _ => return Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
    let want = if af == AF_INET { 4 } else { 16 };
    if packed.len() != want {
        ctx.record_raise("ValueError", Some("packed IP wrong length for inet_ntop"));
        return Err(PyError::PyRaise);
    }
    // FFI vs `std` formatting, one `String` in both
    // builds (the cfg'd block yields the value).
    let s: String = {
        #[cfg(feature = "unsafe-ffi")]
        {
            inet_ntop_owned(af, &packed).map_err(|e| raise_os_error(ctx, map_err(e)))?
        }
        #[cfg(not(feature = "unsafe-ffi"))]
        {
            if af == AF_INET {
                let mut o = [0u8; 4];
                o.copy_from_slice(&packed);
                Ipv4Addr::from(o).to_string()
            } else {
                let mut o = [0u8; 16];
                o.copy_from_slice(&packed);
                Ipv6Addr::from(o).to_string()
            }
        }
    };
    Ok(new_str(ctx, ctx.types, s.as_bytes()))
}

// ── Registration ────────────────────────────────────────────────────

/// Register one module function (name, body, arity, kw
/// body) in the module dict (mirrors the
/// `build_socket_module` registration in `modsocket.rs`).
fn register_fn(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    dict_v: Value,
    name: &[u8],
    func: BuiltinFn,
    min: u16,
    max: u16,
    kw_func: Option<BuiltinKwFn>,
) -> PyResult<()> {
    let id = qid(ctx, name);
    let fun = ctx.alloc(
        Some(fun_builtin_var),
        crate::objects::value::Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(min, max),
            func,
            kw_func,
        }),
    );
    ctx.roots.push(Value::Heap(fun));
    dict_store(ctx, dict_v, Value::Qstr(id), Value::Heap(fun))
        .map_err(|e| ctx.record_typed(e))?;
    Ok(())
}

/// Register one module constant (name → small int).
fn register_const(ctx: &mut CallCtx, dict_v: Value, name: &[u8], v: i64) -> PyResult<()> {
    let id = qid(ctx, name);
    dict_store(ctx, dict_v, Value::Qstr(id), small(v)).map_err(|e| ctx.record_typed(e))?;
    Ok(())
}

/// Register this file's module functions and constants
/// in the `socket` module dict (called from
/// `build_socket_module` in `modsocket.rs`).
pub(crate) fn register_dns_functions(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    dict_v: Value,
) -> PyResult<()> {
    register_fn(ctx, fun_builtin_var, dict_v, b"gethostbyname", socket_gethostbyname, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"gethostbyname_ex", socket_gethostbyname_ex, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"gethostbyaddr", socket_gethostbyaddr, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"getnameinfo", socket_getnameinfo, 2, 2, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"gethostname", socket_gethostname, 0, 0, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"getfqdn", socket_getfqdn, 0, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"getservbyname", socket_getservbyname, 1, 2, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"getservbyport", socket_getservbyport, 1, 2, None)?;
    register_fn(
        ctx,
        fun_builtin_var,
        dict_v,
        b"create_connection",
        socket_create_connection,
        1,
        3,
        Some(socket_create_connection_kw),
    )?;
    register_fn(
        ctx,
        fun_builtin_var,
        dict_v,
        b"create_server",
        socket_create_server,
        1,
        5,
        Some(socket_create_server_kw),
    )?;
    register_fn(ctx, fun_builtin_var, dict_v, b"getdefaulttimeout", socket_getdefaulttimeout, 0, 0, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"setdefaulttimeout", socket_setdefaulttimeout, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"socketpair", socket_socketpair, 0, 3, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"htonl", socket_htonl, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"htons", socket_htons, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"ntohl", socket_ntohl, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"ntohs", socket_ntohs, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"inet_aton", socket_inet_aton, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"inet_ntoa", socket_inet_ntoa, 1, 1, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"inet_pton", socket_inet_pton, 2, 2, None)?;
    register_fn(ctx, fun_builtin_var, dict_v, b"inet_ntop", socket_inet_ntop, 2, 2, None)?;
    for (name, v) in [
        (b"AI_PASSIVE".as_slice(), AI_PASSIVE as i64),
        (b"AI_CANONNAME".as_slice(), AI_CANONNAME as i64),
        (b"AI_NUMERICHOST".as_slice(), AI_NUMERICHOST as i64),
        (b"AI_NUMERICSERV".as_slice(), AI_NUMERICSERV as i64),
        (b"AI_ADDRCONFIG".as_slice(), AI_ADDRCONFIG as i64),
        (b"AI_V4MAPPED".as_slice(), AI_V4MAPPED as i64),
        (b"AI_ALL".as_slice(), AI_ALL as i64),
        (b"NI_MAXHOST".as_slice(), NI_MAXHOST as i64),
        (b"NI_MAXSERV".as_slice(), NI_MAXSERV as i64),
        (b"NI_NUMERICHOST".as_slice(), NI_NUMERICHOST as i64),
        (b"NI_NUMERICSERV".as_slice(), NI_NUMERICSERV as i64),
        (b"NI_NAMEREQD".as_slice(), NI_NAMEREQD as i64),
        (b"NI_DGRAM".as_slice(), NI_DGRAM as i64),
        (b"EAI_BADFLAGS".as_slice(), EAI_BADFLAGS as i64),
        (b"EAI_NONAME".as_slice(), EAI_NONAME as i64),
        (b"EAI_AGAIN".as_slice(), EAI_AGAIN as i64),
        (b"EAI_FAIL".as_slice(), EAI_FAIL as i64),
        (b"EAI_NODATA".as_slice(), EAI_NODATA as i64),
        (b"EAI_FAMILY".as_slice(), EAI_FAMILY as i64),
        (b"EAI_SOCKTYPE".as_slice(), EAI_SOCKTYPE as i64),
        (b"EAI_SERVICE".as_slice(), EAI_SERVICE as i64),
        (b"EAI_MEMORY".as_slice(), EAI_MEMORY as i64),
        (b"SOMAXCONN".as_slice(), SOMAXCONN as i64),
        (b"IPPROTO_IPV6".as_slice(), IPPROTO_IPV6 as i64),
        (b"IPV6_V6ONLY".as_slice(), IPV6_V6ONLY as i64),
    ] {
        register_const(ctx, dict_v, name, v)?;
    }
    #[cfg(unix)]
    for (name, v) in [
        (b"EAI_ADDRFAMILY".as_slice(), EAI_ADDRFAMILY as i64),
        (b"EAI_SYSTEM".as_slice(), EAI_SYSTEM as i64),
        (b"SO_REUSEPORT".as_slice(), SO_REUSEPORT as i64),
    ] {
        register_const(ctx, dict_v, name, v)?;
    }
    Ok(())
}

// ── Feature-off `getaddrinfo` + socket address accessors ─────────────
// These live here (not in `modsocket.rs`) so `modsocket.rs` stays
// under the 2500-line cap: all `getaddrinfo` logic and the
// address accessors are in this module, `modsocket.rs` only
// dispatches (`socket_getaddrinfo`) and registers the methods.

/// The existing pure-`std` `getaddrinfo` (feature off):
/// 5-tuples via `ToSocketAddrs`, pinned by
/// `modsocket_tests.rs` (canonname `''`, type echoed,
/// proto `0`, bad family → 97, unknown service → -2).
/// NOTE: the unknown-service error is `super::EAI_NONAME`
/// (the `modsocket.rs` const, -2 on every platform) —
/// NOT this module's platform-correct `EAI_NONAME`
/// (11001 on Windows) — so the pinned `-2` holds on
/// Windows too.
#[cfg(not(feature = "unsafe-ffi"))]
pub(crate) fn socket_getaddrinfo_std(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    use std::net::ToSocketAddrs;
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
                        None => return Err(raise_os_error(ctx, super::EAI_NONAME)),
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

/// `sock.getsockname() -> address`: the socket's own
/// address — a `(host, port)` tuple for INET/INET6, a
/// path str for AF_UNIX. (Lane S2: the method did not
/// exist — the brief's §2.4 lists it among the
/// "existing" methods, but it was absent (as were
/// `getpeername`/`shutdown`); added here on the
/// pure-`std` path so `create_server` can learn its
/// ephemeral port. S1 adds the `SocketState::Ffi` arm
/// at merge — the variants are listed explicitly, so
/// the match goes non-exhaustive when `Ffi` lands.)
// Default build only: the pure-std `getsockname`.
// Under the feature build S1's FFI version
// (`modsocket_ffi::sock_getsockname`) is registered
// instead, so this one is compiled out there (its
// `match` has no `SocketState::Ffi` arm).
#[cfg(not(feature = "unsafe-ffi"))]
pub(crate) fn sock_getsockname(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let table = sockets();
    let state = match table.get(&fd) {
        Some(s) => s,
        None => return Err(raise_os_error(ctx, EBADF)),
    };
    let types = ctx.types;
    match state {
        SocketState::Unbound(u) | SocketState::Datagram(u) => {
            let a = u.local_addr().map_err(|e| io_os_error(ctx, e))?;
            Ok(inet_addr_tuple(ctx, a))
        }
        SocketState::ListenerTcp(l) => {
            let a = l.local_addr().map_err(|e| io_os_error(ctx, e))?;
            Ok(inet_addr_tuple(ctx, a))
        }
        SocketState::StreamTcp(s) => {
            let a = s.local_addr().map_err(|e| io_os_error(ctx, e))?;
            Ok(inet_addr_tuple(ctx, a))
        }
        #[cfg(unix)]
        SocketState::ListenerUnix(l) => {
            let a = l.local_addr().map_err(|e| io_os_error(ctx, e))?;
            match a.as_pathname() {
                Some(p) => Ok(new_str(ctx, types, p.as_os_str().as_encoded_bytes())),
                None => Err(raise_os_error(ctx, ENOTCONN)),
            }
        }
        #[cfg(unix)]
        SocketState::StreamUnix(s) => {
            let a = s.local_addr().map_err(|e| io_os_error(ctx, e))?;
            match a.as_pathname() {
                Some(p) => Ok(new_str(ctx, types, p.as_os_str().as_encoded_bytes())),
                None => Err(raise_os_error(ctx, ENOTCONN)),
            }
        }
        #[cfg(unix)]
        SocketState::DatagramUnix(d) => {
            let a = d.local_addr().map_err(|e| io_os_error(ctx, e))?;
            match a.as_pathname() {
                Some(p) => Ok(new_str(ctx, types, p.as_os_str().as_encoded_bytes())),
                None => Err(raise_os_error(ctx, ENOTCONN)),
            }
        }
    }
}

/// `sock.getpeername() -> address`: the connected
/// peer's address (same shapes as `getsockname`). An
/// unconnected socket (unbound, listener) is
/// `OSError(ENOTCONN)`. (Lane S2: added alongside
/// `getsockname`; see above.)
// Default build only: the pure-std `getpeername`.
// See `sock_getsockname` above — the feature build
// uses S1's FFI version instead.
#[cfg(not(feature = "unsafe-ffi"))]
pub(crate) fn sock_getpeername(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let table = sockets();
    let state = match table.get(&fd) {
        Some(s) => s,
        None => return Err(raise_os_error(ctx, EBADF)),
    };
    let types = ctx.types;
    match state {
        SocketState::StreamTcp(s) => {
            let a = s.peer_addr().map_err(|e| io_os_error(ctx, e))?;
            Ok(inet_addr_tuple(ctx, a))
        }
        SocketState::Datagram(u) => {
            let a = u.peer_addr().map_err(|e| io_os_error(ctx, e))?;
            Ok(inet_addr_tuple(ctx, a))
        }
        #[cfg(unix)]
        SocketState::StreamUnix(s) => {
            let a = s.peer_addr().map_err(|e| io_os_error(ctx, e))?;
            match a.as_pathname() {
                Some(p) => Ok(new_str(ctx, types, p.as_os_str().as_encoded_bytes())),
                None => Err(raise_os_error(ctx, ENOTCONN)),
            }
        }
        #[cfg(unix)]
        SocketState::DatagramUnix(d) => {
            let a = d.peer_addr().map_err(|e| io_os_error(ctx, e))?;
            match a.as_pathname() {
                Some(p) => Ok(new_str(ctx, types, p.as_os_str().as_encoded_bytes())),
                None => Err(raise_os_error(ctx, ENOTCONN)),
            }
        }
        // Unbound sockets and listeners are not connected.
        SocketState::Unbound(_) | SocketState::ListenerTcp(_) => {
            Err(raise_os_error(ctx, ENOTCONN))
        }
        #[cfg(unix)]
        SocketState::ListenerUnix(_) => Err(raise_os_error(ctx, ENOTCONN)),
    }
}
