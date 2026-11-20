//! S3 — socket FFI, advanced + platform + `select` cooperation.
//! (Lane `socket-ffi-s3`, brief `scripts/socket_ffi_brief.md` §3 S3,
//! merged onto S1+S2.)
//!
//! Whole file is `#[cfg(feature = "unsafe-ffi")]`: the default
//! build sees nothing here and stays byte-identical.
//!
//! Contents:
//! - `sendmsg` / `recvmsg` (plain buffers only; no ancillary data
//!   — CPython's `sendmsg` has no ancillary parameter, and
//!   `recvmsg` with `ancbufsize != 0` is rejected here, never
//!   silently dropped).
//! - `socket.fromfd` (no dup, no socket-ness check — the
//!   integration test wraps a `/dev/null` fd and requires
//!   `fileno() == fd`) and `sock.detach`.
//! - `sock.makefile` (FFI arm: `dup` the fd and wrap the dup in
//!   a `std` stream — the socket keeps its own fd, so the file
//!   and the socket stay independently usable).
//! - `fromfd` validation (`check_fd_valid`) and the `makefile`
//!   cooperation primitive (`makefile_file`).
//! - AF_UNIX via FFI (`ffi_unix_bind` / `ffi_unix_connect` /
//!   `ffi_unix_accept`; Windows AF_UNIX is probed at runtime).
//! - Windows platform finish: `closesocket`,
//!   `ioctlsocket(FIONBIO)`, the loopback-TCP `socketpair`,
//!   `SOCKET`/`TableKey` conversion. Winsock error codes come
//!   back raw from `ffi.rs` and are mapped to POSIX errnos by
//!   the caller (`ffi_os_error` in `modsocket_ffi.rs`).
//! - `poll(2)` readiness (`poll_readable`), the `select`
//!   cooperation for sockets under the feature.
//!
//! All raw syscalls, `sockaddr` structs and the `SockAddr`
//! address type live in `crate::support::ffi` /
//! `crate::modules::modsocket_ffi` (S1, brief §2.3) — this
//! file holds no private duplicate. The Python-facing
//! family/type constants come from `modsocket.rs` and are
//! translated to kernel values by `translate_family` inside
//! `ffi_socket`.
//!
//! Known gaps / merge notes (unchanged from the standalone S3
//! result; see `scripts/sockffi_s3_result.txt` §4):
//! - `fromfd` double-ownership hazard: the wrapped fd stays
//!   in `os_fds`; closing the socket then dropping the VM
//!   aborts the DEBUG build (same class as H-select §4(9)).
//!   Release is harmless. A real fix (dup on `fromfd`, or
//!   removing the fd from `os_fds`) would break the pinned
//!   `fileno() == fd` behaviour — left for a later decision.
//! - Windows code (`winsock_socketpair`, `WSASendMsg` /
//!   `WSARecvMsg`, AF_UNIX probe) is NOT locally verifiable
//!   (no Windows host here; user constraint: no local
//!   cross-compilation) — it is written from the Winsock API
//!   and must be verified by CI.
//! - `poll_readable` is Unix-only: `socket_probe` is Unix-only
//!   in `modsocket.rs`, so Windows `select` on a socket still
//!   raises `EBADF` (pre-existing; out of S3 scope).
#![cfg(feature = "unsafe-ffi")]

use std::fs::File;
#[cfg(any(windows, test))]
use std::net::Ipv4Addr;
#[cfg(test)]
use std::net::Ipv6Addr;
#[cfg(unix)]
use std::os::unix::io::FromRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
#[cfg(windows)]
use std::os::windows::io::FromRawSocket;

use crate::memory::gc::CallCtx;
use crate::modules::modsocket::{
    alloc_socket, int_arg, io_os_error, self_socket, set_fd, settings, sock_key_of, sockets,
    str_arg, swap_in, SocketState, TableKey, AF_INET, AF_INET6, AF_UNIX, EAFNOSUPPORT, EBADF,
    EINVAL, ENOTCONN, MAX_RECV, SOCK_DGRAM, SOCK_STREAM,
};
use crate::modules::modsocket_ffi::{
    ffi_os_error, parse_target, sockaddr_value, translate_family, FfiSocket, SockAddr,
};
use crate::objects::containers::objlist::{list_items, new_list};
use crate::objects::containers::objtuple::{new_tuple, tuple_items};
use crate::objects::strings::objstr_a::new_bytes;
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::{malloc_fail, raise_os_error};
use crate::support::ffi;

/// The OS socket handle: `i32` fd on Unix, `SOCKET` (`usize`)
/// on Windows. S1's `FfiSocket.fd` is this type (brief §2.3).
#[cfg(unix)]
pub type RawFd = i32;
#[cfg(windows)]
pub type RawFd = usize;

/// `SOL_SOCKET` / `SO_TYPE` for the `fromfd` validation
/// (`getsockopt` on Windows; Unix uses `fcntl(F_GETFD)`).
/// Kernel values (Winsock: `SOL_SOCKET` 0xFFFF, `SO_TYPE`
/// 0x1008) — `sys_getsockopt` takes kernel values.
#[cfg(windows)]
const SOL_SOCKET: i32 = 0xFFFF;
#[cfg(windows)]
const SO_TYPE: i32 = 0x1008;

/// `fcntl` command: get the close-on-exec flag (validity probe).
#[cfg(unix)]
const F_GETFD: i32 = 1;

/// `poll(2)` events (Unix).
#[cfg(unix)]
const POLLIN: i16 = 1;
#[cfg(unix)]
const POLLERR: i16 = 8;
#[cfg(unix)]
const POLLHUP: i16 = 16;

/// `ioctlsocket` command: set non-blocking mode (Windows).
#[cfg(windows)]
const FIONBIO: i32 = 0x8004_667E_u32 as i32;

/// A small int, infallible for socket fields (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket field fits the small-int window")
}

/// The `SOCKET` / fd value as a table key (a `SOCKET` value
/// is < 2^63 in practice, so the truncating cast is safe).
#[allow(dead_code)] // Merge API: called by the unit tests here.
pub(crate) fn sock_key(fd: RawFd) -> i64 {
    fd as i64
}

/// The reverse conversion (a key that came from a script
/// is validated by `check_fd_valid` afterwards, so a
/// truncated value fails there with `EBADF`).
pub(crate) fn raw_fd_from_key(key: i64) -> RawFd {
    key as RawFd
}

/// Create an FFI socket fd (used by the unit tests, the
/// Windows AF_UNIX probe, and the Winsock `socketpair`;
/// S1's `FfiSocket::new` is the production path).
/// `domain` is Python-facing (`AF_INET6` 10); it is
/// translated to the kernel value here.
#[allow(dead_code)] // Merge API: production use is Windows-only (`winsock_socketpair`); Linux use is the unit tests here.
pub(crate) fn ffi_socket(domain: i32, ty: i32, proto: i32) -> Result<RawFd, i32> {
    ffi::sys_socket(translate_family(domain), ty, proto)
}

/// Close a raw fd (`close(2)` on Unix, `closesocket` on
/// Windows — never the other one).
#[allow(dead_code)] // Merge API: production use is Windows-only (`winsock_socketpair`); Linux use is the unit tests here.
pub(crate) fn close_raw(fd: RawFd) -> Result<(), i32> {
    #[cfg(unix)]
    {
        ffi::sys_close(fd)
    }
    #[cfg(windows)]
    {
        ffi::sys_closesocket(fd)
    }
}

/// `sendmsg`: gather the plain buffers into one message.
/// There is no ancillary data — the control buffer is
/// always empty (CPython's `sendmsg` takes no ancillary
/// parameter; the `modsocket` layer rejects `recvmsg`
/// ancillary requests). `addr` is the destination for
/// unconnected sockets (`None` sends to the connected
/// peer). No `EINTR` retry (the existing `ffi.rs` wrappers
/// do none either).
pub(crate) fn ffi_sendmsg(
    fd: RawFd,
    bufs: &[&[u8]],
    flags: i32,
    addr: Option<&SockAddr>,
) -> Result<usize, i32> {
    #[cfg(unix)]
    {
        let mut iov: Vec<ffi::Iovec> = bufs
            .iter()
            .map(|b| ffi::Iovec {
                iov_base: b.as_ptr() as *mut std::os::raw::c_void,
                iov_len: b.len(),
            })
            .collect();
        let mut storage = ffi::SockaddrStorage::new();
        let (name_ptr, name_len) = match addr {
            Some(a) => {
                let len = a.to_raw(&mut storage);
                (
                    storage._inner.as_mut_ptr() as *mut std::os::raw::c_void,
                    len,
                )
            }
            None => (std::ptr::null_mut(), 0),
        };
        // FFI invariant: the `Msghdr` borrows `iov` and
        // `storage`, which both outlive the call; the
        // buffers are borrowed from the caller for the call.
        let hdr = ffi::Msghdr {
            msg_name: name_ptr,
            msg_namelen: name_len,
            msg_iov: iov.as_mut_ptr(),
            msg_iovlen: iov.len(),
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        ffi::sys_sendmsg(fd, &hdr, flags)
    }
    #[cfg(windows)]
    {
        let mut wsa_bufs: Vec<ffi::WsaBuf> = bufs
            .iter()
            .map(|b| ffi::WsaBuf {
                len: b.len().min(u32::MAX as usize) as u32,
                buf: b.as_ptr() as *mut u8,
            })
            .collect();
        let mut storage = ffi::SockaddrStorage::new();
        let (name_ptr, name_len) = match addr {
            Some(a) => {
                let len = a.to_raw(&mut storage);
                (
                    storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
                    len as i32,
                )
            }
            None => (std::ptr::null_mut(), 0),
        };
        // FFI invariant: the `WsaMsg` borrows `wsa_bufs`
        // and `storage`, which both outlive the call.
        let msg = ffi::WsaMsg {
            name: name_ptr,
            namelen: name_len,
            buffers: wsa_bufs.as_mut_ptr(),
            buffer_count: wsa_bufs.len() as u32,
            control: ffi::WsaBuf {
                len: 0,
                buf: std::ptr::null_mut(),
            },
            flags: 0,
        };
        ffi::sys_sendmsg(fd, &msg, flags as u32)
    }
}

/// `recvmsg`: scatter one message into the buffers.
/// Returns `(bytes, address, msg_flags)`; the address is
/// `None` when the kernel left the name buffer empty. No
/// ancillary buffer is requested, so none is ever returned.
/// (`flags` is the `recvmsg` input flags on Unix;
/// `WSARecvMsg` takes no input flags — the message flags
/// come back in the `WSAMSG` — so it is ignored on
/// Windows.)
pub(crate) fn ffi_recvmsg(
    fd: RawFd,
    bufs: &mut [&mut [u8]],
    flags: i32,
) -> Result<(usize, Option<SockAddr>, i32), i32> {
    #[cfg(unix)]
    {
        let mut iov: Vec<ffi::Iovec> = bufs
            .iter_mut()
            .map(|b| ffi::Iovec {
                iov_base: b.as_mut_ptr() as *mut std::os::raw::c_void,
                iov_len: b.len(),
            })
            .collect();
        let mut storage = ffi::SockaddrStorage::new();
        let mut namelen: u32 = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        // FFI invariant: the `Msghdr` borrows `iov` and
        // `storage`, which both outlive the call.
        let mut hdr = ffi::Msghdr {
            msg_name: storage._inner.as_mut_ptr() as *mut std::os::raw::c_void,
            msg_namelen: namelen,
            msg_iov: iov.as_mut_ptr(),
            msg_iovlen: iov.len(),
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        let n = ffi::sys_recvmsg(fd, &mut hdr, flags)?;
        namelen = hdr.msg_namelen;
        let addr = if namelen > 0 {
            SockAddr::from_raw(&storage, namelen)
        } else {
            None
        };
        Ok((n, addr, hdr.msg_flags))
    }
    #[cfg(windows)]
    {
        let _ = flags;
        let mut wsa_bufs: Vec<ffi::WsaBuf> = bufs
            .iter_mut()
            .map(|b| ffi::WsaBuf {
                len: b.len().min(u32::MAX as usize) as u32,
                buf: b.as_mut_ptr(),
            })
            .collect();
        let mut storage = ffi::SockaddrStorage::new();
        // FFI invariant: the `WsaMsg` borrows `wsa_bufs`
        // and `storage`, which both outlive the call; the
        // flags come back in `msg.flags` (WSARecvMsg has
        // no separate flags out-param).
        let mut msg = ffi::WsaMsg {
            name: storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            namelen: std::mem::size_of::<ffi::SockaddrStorage>() as i32,
            buffers: wsa_bufs.as_mut_ptr(),
            buffer_count: wsa_bufs.len() as u32,
            control: ffi::WsaBuf {
                len: 0,
                buf: std::ptr::null_mut(),
            },
            flags: 0,
        };
        let n = ffi::sys_recvmsg(fd, &mut msg)?;
        let addr = if msg.namelen > 0 {
            SockAddr::from_raw(&storage, msg.namelen as u32)
        } else {
            None
        };
        Ok((n, addr, msg.flags as i32))
    }
}

/// Validate an fd for `fromfd` (the `fromfd` validation
/// in `modsocket.rs`). Unix: `fcntl(F_GETFD)` — the fd
/// must be open; deliberately NO `SO_TYPE` check, so a
/// non-socket fd (the integration test wraps `/dev/null`)
/// is accepted. Windows: `getsockopt(SO_TYPE)` — a real
/// socket is required there (no `/dev/null` equivalent in
/// the test suite); a non-socket handle fails with
/// `WSAENOTSOCK` → `ENOTSOCK`.
pub(crate) fn check_fd_valid(fd: RawFd) -> Result<(), i32> {
    #[cfg(unix)]
    {
        ffi::sys_fcntl(fd, F_GETFD, 0).map(|_| ())
    }
    #[cfg(windows)]
    {
        let mut val: i32 = 0;
        let mut len: u32 = std::mem::size_of::<i32>() as u32;
        ffi::sys_getsockopt(
            fd,
            SOL_SOCKET,
            SO_TYPE,
            &mut val as *mut i32 as *mut u8,
            &mut len,
        )
        .map(|_| ())
    }
}

/// Wrap a raw fd in a `File` for the `makefile` FFI arm
/// (called by `sock_makefile` in this file).
///
/// # Safety
/// The caller must own the fd: this call transfers
/// ownership to the returned `File` (closing the `File`
/// closes the fd; the socket must NOT close it afterwards
/// — otherwise the fd number is closed twice).
#[allow(dead_code)] // Merge API: the tested ownership primitive for the `makefile` FFI arm (`sock_makefile` dups instead, so the socket keeps its fd).
pub(crate) unsafe fn makefile_file(fd: RawFd) -> File {
    #[cfg(unix)]
    {
        File::from_raw_fd(fd)
    }
    #[cfg(windows)]
    {
        File::from_raw_socket(fd)
    }
}

/// AF_UNIX `bind(2)` on an FFI fd. `path` is the
/// filesystem path bytes (empty = unnamed; a leading NUL =
/// abstract namespace, Linux only). (S1's `FfiSocket::bind`
/// covers this for production sockets via the generic
/// `SockAddr` machinery; this helper is the direct
/// address-form used by the unit tests.)
#[allow(dead_code)] // Merge API: unit tests only on Linux; the Windows `socketpair` probe path.
pub(crate) fn ffi_unix_bind(fd: RawFd, path: &[u8]) -> Result<(), i32> {
    let addr = SockAddr::Unix {
        path: path.to_vec(),
    };
    let mut storage = ffi::SockaddrStorage::new();
    let len = addr.to_raw(&mut storage);
    ffi::sys_bind(
        fd,
        storage._inner.as_ptr() as *const ffi::Sockaddr,
        len,
    )
}

/// AF_UNIX `connect(2)` on an FFI fd.
#[allow(dead_code)] // Merge API: unit tests only on Linux.
pub(crate) fn ffi_unix_connect(fd: RawFd, path: &[u8]) -> Result<(), i32> {
    let addr = SockAddr::Unix {
        path: path.to_vec(),
    };
    let mut storage = ffi::SockaddrStorage::new();
    let len = addr.to_raw(&mut storage);
    ffi::sys_connect(
        fd,
        storage._inner.as_ptr() as *const ffi::Sockaddr,
        len,
    )
}

/// AF_UNIX `accept(2)` on an FFI fd. Returns the new fd
/// and the peer address (`Unix { path: [] }` when unnamed).
#[allow(dead_code)] // Merge API: unit tests only on Linux.
pub(crate) fn ffi_unix_accept(fd: RawFd) -> Result<(RawFd, SockAddr), i32> {
    let mut storage = ffi::SockaddrStorage::new();
    let mut len: u32 = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
    let new_fd = ffi::sys_accept(
        fd,
        storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
        &mut len,
    )?;
    let addr = SockAddr::from_raw(&storage, len).unwrap_or(SockAddr::Unix { path: Vec::new() });
    Ok((new_fd, addr))
}

/// AF_UNIX support probe. Unix: always true (`AF_UNIX`
/// is universal there). Windows: creates a throwaway
/// `AF_UNIX` socket — success (Win10 1803+) means the
/// Winsock AF_UNIX path works, failure means keep the
/// existing `NotImplementedError`.
#[allow(dead_code)] // Merge API: called by the unit tests here.
pub(crate) fn af_unix_supported() -> bool {
    #[cfg(unix)]
    {
        true
    }
    #[cfg(windows)]
    {
        match ffi_socket(AF_UNIX, SOCK_STREAM, 0) {
            Ok(fd) => {
                let _ = close_raw(fd);
                true
            }
            Err(_) => false,
        }
    }
}

/// `poll(2)` readiness probe (Unix): true when the fd is
/// readable NOW — data waiting, EOF, an error, or a
/// pending connection on a listener. Non-destructive: no
/// byte is consumed, so the socket's `recv` still sees
/// the data afterwards. Any error (including `EINTR` and
/// an invalid fd) reports not-ready; the `select` loop
/// re-probes on the next iteration.
#[cfg(unix)]
pub(crate) fn poll_readable(fd: RawFd) -> bool {
    let mut pfd = ffi::Pollfd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    match ffi::sys_poll(&mut pfd, 1, 0) {
        Ok(n) => n > 0 && (pfd.revents & (POLLIN | POLLERR | POLLHUP)) != 0,
        Err(_) => false,
    }
}

/// Winsock `socketpair` (Windows): there is no
/// `socketpair(2)` on Windows, so build a loopback TCP
/// pair (CPython does the same). Returns
/// `(client, accepted)`, both blocking. The listener is
/// set non-blocking and polled with a 5 s deadline, so a
/// failed loopback connect fails cleanly instead of
/// hanging in `accept` forever.
#[cfg(windows)]
pub(crate) fn winsock_socketpair() -> Result<(RawFd, RawFd), i32> {
    use std::time::{Duration, Instant};

    // Listener on 127.0.0.1:0.
    let listener = ffi_socket(AF_INET, SOCK_STREAM, 0)?;
    let bind_res = (|| -> Result<u16, i32> {
        let addr = SockAddr::Inet {
            ip: Ipv4Addr::new(127, 0, 0, 1),
            port: 0,
        };
        let mut storage = ffi::SockaddrStorage::new();
        let len = addr.to_raw(&mut storage);
        ffi::sys_bind(
            listener,
            storage._inner.as_ptr() as *const ffi::Sockaddr,
            len,
        )?;
        let mut bound = ffi::SockaddrStorage::new();
        let mut bound_len: u32 = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        ffi::sys_getsockname(
            listener,
            bound._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut bound_len,
        )?;
        let bound_addr = SockAddr::from_raw(&bound, bound_len).ok_or(22 /* EINVAL */)?;
        match bound_addr {
            SockAddr::Inet { port, .. } => Ok(port),
            _ => Err(22 /* EINVAL */),
        }
    })();
    let port = match bind_res {
        Ok(p) => p,
        Err(e) => {
            let _ = close_raw(listener);
            return Err(e);
        }
    };
    if let Err(e) = ffi::sys_listen(listener, 1) {
        let _ = close_raw(listener);
        return Err(e);
    }
    // Client connect (loopback; completes while we accept).
    let client = match ffi_socket(AF_INET, SOCK_STREAM, 0) {
        Ok(c) => c,
        Err(e) => {
            let _ = close_raw(listener);
            return Err(e);
        }
    };
    let peer = SockAddr::Inet {
        ip: Ipv4Addr::new(127, 0, 0, 1),
        port,
    };
    let mut storage = ffi::SockaddrStorage::new();
    let len = peer.to_raw(&mut storage);
    if let Err(e) = ffi::sys_connect(
        client,
        storage._inner.as_ptr() as *const ffi::Sockaddr,
        len,
    ) {
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    // Accept with a deadline: listener non-blocking, poll
    // with Winsock `select` until readable or 5 s pass.
    let one: u32 = 1;
    if let Err(e) = ffi::sys_ioctlsocket(listener, FIONBIO, one) {
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let accepted = loop {
        let mut set = ffi::FdSet {
            count: 1,
            sockets: [0usize; 64],
        };
        set.sockets[0] = listener;
        match ffi::sys_select(Some(&mut set), None, None, Some(100)) {
            // Readable: fall through to the `accept` below.
            Ok(n) if n > 0 => {}
            Ok(_) => {
                if Instant::now() >= deadline {
                    let _ = close_raw(client);
                    let _ = close_raw(listener);
                    return Err(110 /* ETIMEDOUT */);
                }
            }
            Err(e) => {
                let _ = close_raw(client);
                let _ = close_raw(listener);
                return Err(e);
            }
        }
        let mut storage = ffi::SockaddrStorage::new();
        let mut len: u32 = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        match ffi::sys_accept(
            listener,
            storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut len,
        ) {
            Ok(fd) => break fd,
            Err(e) => {
                // `EAGAIN` while the connection is still
                // being queued: keep polling until the
                // deadline; any other error fails. (The
                // raw Winsock code is mapped first —
                // `WSAEWOULDBLOCK` 10035 → `EAGAIN` 11.)
                let e_posix = crate::runtime::modos::win32_to_errno(e);
                if e_posix == 11 /* EAGAIN */ && Instant::now() < deadline {
                    continue;
                }
                let _ = close_raw(client);
                let _ = close_raw(listener);
                return Err(e);
            }
        }
    };
    // Both ends back to blocking.
    let zero: u32 = 0;
    if let Err(e) = ffi::sys_ioctlsocket(accepted, FIONBIO, zero)
        .and_then(|_| ffi::sys_ioctlsocket(client, FIONBIO, zero))
    {
        let _ = close_raw(accepted);
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    let _ = close_raw(listener);
    Ok((client, accepted))
}

// ── Moved from `modsocket.rs` (merge split) ──────────────────────────
// These live here so `modsocket.rs` stays under the 2500-line cap.
// They use `modsocket.rs` / `modsocket_ffi.rs` internals (`pub(crate)`
// there) and S1's `SockAddr` / `FfiSocket`.

/// `socket.fromfd` under `unsafe-ffi` (S3). The fd is taken
/// as-is — NOT dup'd (the integration test
/// `socket_fromfd_wraps_fd` requires `fileno() == fd`) —
/// and is NOT checked for socket-ness (the test wraps a
/// `/dev/null` fd); only liveness is checked (`EBADF` for
/// an invalid fd, via `check_fd_valid`). The fd is wrapped
/// in S1's `FfiSocket` (real fd at construction, fileno
/// stable), so the FFI method arms apply to it.
pub(crate) fn socket_fromfd_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let fd = int_arg(ctx, args[0])?;
    let family = int_arg(ctx, args[1])? as i32;
    let type_ = int_arg(ctx, args[2])? as i32;
    let proto = match args.get(3) {
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
    #[cfg(windows)]
    if family == AF_UNIX {
        // The Winsock AF_UNIX path is probed at runtime
        // (`af_unix_supported`); a fromfd AF_UNIX socket
        // stays unimplemented there.
        ctx.record_raise(
            "NotImplementedError",
            Some("socket.fromfd(AF_UNIX) is not implemented on Windows"),
        );
        return Err(PyError::PyRaise);
    }
    let raw = raw_fd_from_key(fd);
    check_fd_valid(raw).map_err(|e| ffi_os_error(ctx, e))?;
    let state = SocketState::Ffi(FfiSocket::from_raw(raw, family, type_, proto));
    let key = sock_key_of(&state);
    sockets().insert(key, state);
    alloc_socket(ctx, key, family, type_, proto)
}

/// Resolve the real fd for `sendmsg`/`recvmsg` (FFI lane,
/// S3). Bound/connected sockets use their stored fd; an
/// unbound INET datagram uses the placeholder (CPython
/// auto-binds an unbound UDP socket the same way); an
/// unbound AF_UNIX datagram swaps in a real unbound
/// `UnixDatagram` first — the placeholder is an AF_INET UDP
/// socket, and sending a `sockaddr_un` to it is
/// `EAFNOSUPPORT` (the kernel rejects the family). An
/// unbound stream socket is `EINVAL`, a listener is
/// `listener_err` (`ENOTCONN` for sends, `EINVAL` for
/// receives — the `sock_send` / `sock_recv` shapes).
fn msg_fd(
    ctx: &mut CallCtx,
    r: GcRef,
    fd: TableKey,
    fields: [i32; 3],
    listener_err: i32,
) -> PyResult<RawFd> {
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let (family, type_) = (fields[0], fields[1]);
    // Classify the stored state. The lock is released
    // before any swap — `swap_in` re-locks the table.
    enum Action {
        Use(RawFd),
        SwapUnixDgram,
        Fail(i32),
    }
    let action = {
        let table = sockets();
        match table.get(&fd) {
            None => Action::Fail(EBADF),
            Some(s) if s.is_listener() => Action::Fail(listener_err),
            Some(SocketState::Unbound(_)) if type_ == SOCK_STREAM => Action::Fail(EINVAL),
            Some(SocketState::Unbound(_)) if family == AF_UNIX => {
                #[cfg(unix)]
                {
                    Action::SwapUnixDgram
                }
                #[cfg(not(unix))]
                {
                    Action::Fail(EAFNOSUPPORT)
                }
            }
            Some(SocketState::Unbound(_)) => Action::Use(raw_fd_from_key(fd)),
            Some(s) => Action::Use(raw_fd_from_key(sock_key_of(s))),
        }
    };
    match action {
        Action::Fail(e) => Err(raise_os_error(ctx, e)),
        Action::Use(raw) => Ok(raw),
        Action::SwapUnixDgram => {
            #[cfg(unix)]
            {
                let d = UnixDatagram::unbound().map_err(|e| io_os_error(ctx, e))?;
                let new_key = swap_in(ctx, r, fd, SocketState::DatagramUnix(d));
                Ok(raw_fd_from_key(new_key))
            }
            #[cfg(not(unix))]
            {
                Err(raise_os_error(ctx, EAFNOSUPPORT))
            }
        }
    }
}

/// `sock.sendmsg(buffers[, flags[, address]]) -> int` (FFI
/// lane, S3): gather-send via `sendmsg(2)`. `buffers` is a
/// list/tuple of bytes-like; `flags` defaults to 0;
/// `address` is an `(host, port)` tuple for INET or a path
/// for UNIX. No ancillary data (CPython's `sendmsg` takes
/// none — the control buffer is always empty).
pub(crate) fn sock_sendmsg(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    // Copy the buffer values out first: `list_items` /
    // `tuple_items` borrow `ctx` and `str_arg` needs `&mut ctx`.
    let items: Vec<Value> = match list_items(ctx, args[1]) {
        Some(items) => items.to_vec(),
        None => match tuple_items(ctx, args[1]) {
            Some(items) => items.to_vec(),
            None => {
                ctx.record_raise(
                    "TypeError",
                    Some("sendmsg buffers must be a list or tuple"),
                );
                return Err(PyError::PyRaise);
            }
        },
    };
    let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(items.len());
    for v in items {
        bufs.push(str_arg(ctx, v)?);
    }
    let bufs_ref: Vec<&[u8]> = bufs.iter().map(|b| b.as_slice()).collect();
    let flags = match args.get(2) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    let addr = match args.get(3) {
        Some(v) if *v != Value::None => Some(parse_target(ctx, *v, fields[0])?),
        _ => None,
    };
    let raw = msg_fd(ctx, r, fd, fields, ENOTCONN)?;
    let n = ffi_sendmsg(raw, &bufs_ref, flags, addr.as_ref())
        .map_err(|e| ffi_os_error(ctx, e))?;
    Ok(small(n as i64))
}

/// `sock.recvmsg(bufsize[, ancbufsize[, flags]]) ->
/// (bytes, ancdata, msg_flags, address)` (FFI lane, S3).
/// `ancbufsize != 0` is `NotImplementedError` (no ancillary
/// data support — never silently dropped); the returned
/// ancillary list is always empty.
pub(crate) fn sock_recvmsg(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, fields) = self_socket(ctx, args)?;
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
    let ancbufsize = match args.get(2) {
        Some(v) => int_arg(ctx, *v)?,
        None => 0,
    };
    if ancbufsize != 0 {
        ctx.record_raise(
            "NotImplementedError",
            Some("recvmsg ancillary data is not implemented"),
        );
        return Err(PyError::PyRaise);
    }
    let flags = match args.get(3) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    let mut buf = vec![0u8; n_signed as usize];
    let raw = msg_fd(ctx, r, fd, fields, EINVAL)?;
    let (got, addr, msg_flags) =
        ffi_recvmsg(raw, &mut [&mut buf[..]], flags).map_err(|e| ffi_os_error(ctx, e))?;
    buf.truncate(got);
    let data = new_bytes(ctx, ctx.types, &buf);
    let ancdata = Value::Heap(new_list(ctx, ctx.types.list, &[]));
    let addr_v = match addr {
        Some(a) => sockaddr_value(ctx, &a),
        None => Value::None,
    };
    let tuple = new_tuple(
        ctx,
        ctx.types.tuple,
        &[data, ancdata, small(msg_flags as i64), addr_v],
    );
    Ok(Value::Heap(tuple))
}

/// `sock.detach() -> int` (FFI lane, S3): remove the socket
/// from the table WITHOUT closing it and return the raw fd;
/// the instance's fd becomes -1 (CPython). The caller owns
/// the fd afterwards.
pub(crate) fn sock_detach(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    // `IntoRawFd` / `IntoRawSocket` are safe: they transfer
    // ownership of the number out of the owned object without
    // closing it. (Dropping the state instead would CLOSE the
    // fd — exactly what `detach` must not do.) The FFI arm
    // sets `closed` so the `FfiSocket` drop skips its close.
    let raw: TableKey = {
        let mut table = sockets();
        let Some(state) = table.remove(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        #[cfg(unix)]
        {
            use std::os::unix::io::IntoRawFd;
            match state {
                SocketState::Unbound(u) | SocketState::Datagram(u) => {
                    u.into_raw_fd() as TableKey
                }
                SocketState::ListenerTcp(l) => l.into_raw_fd() as TableKey,
                SocketState::ListenerUnix(l) => l.into_raw_fd() as TableKey,
                SocketState::StreamTcp(s) => s.into_raw_fd() as TableKey,
                SocketState::StreamUnix(s) => s.into_raw_fd() as TableKey,
                SocketState::DatagramUnix(d) => d.into_raw_fd() as TableKey,
                SocketState::Ffi(mut f) => {
                    let raw = f.fd as TableKey;
                    f.closed = true;
                    raw
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::IntoRawSocket;
            match state {
                SocketState::Unbound(u) | SocketState::Datagram(u) => {
                    u.into_raw_socket() as TableKey
                }
                SocketState::ListenerTcp(l) => l.into_raw_socket() as TableKey,
                SocketState::StreamTcp(s) => s.into_raw_socket() as TableKey,
                SocketState::Ffi(mut f) => {
                    let raw = f.fd as TableKey;
                    f.closed = true;
                    raw
                }
            }
        }
    };
    settings().remove(&fd);
    set_fd(ctx, r, -1);
    Ok(small(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn unix_pair() -> (RawFd, RawFd) {
        use std::os::unix::io::AsRawFd;
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().expect("socketpair for test");
        let (fa, fb) = (a.as_raw_fd(), b.as_raw_fd());
        std::mem::forget(a);
        std::mem::forget(b);
        (fa, fb)
    }

    /// `sendmsg` gathers several buffers; `recvmsg`
    /// scatters them back in order.
    #[test]
    #[cfg(unix)]
    fn sendmsg_recvmsg_unix_pair() {
        let (a, b) = unix_pair();
        let n = ffi_sendmsg(a, &[b"he", b"llo"], 0, None).expect("sendmsg");
        assert_eq!(n, 5);
        let mut buf = [0u8; 5];
        let (got, _addr, _flags) = ffi_recvmsg(b, &mut [&mut buf[..]], 0).expect("recvmsg");
        assert_eq!(got, 5);
        assert_eq!(&buf, b"hello");
        close_raw(a).expect("close a");
        close_raw(b).expect("close b");
    }

    /// `sendmsg` with an explicit destination address, and
    /// `recvmsg` reporting the sender, on `AF_UNIX` datagrams.
    #[test]
    #[cfg(unix)]
    fn sendmsg_recvmsg_unix_datagram() {
        let path = std::env::temp_dir().join(format!(
            "sockffi-adv-{}-dgram.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let server = ffi_socket(AF_UNIX, SOCK_DGRAM, 0).expect("socket");
        ffi_unix_bind(server, path_bytes).expect("bind");
        let client_path = std::env::temp_dir().join(format!(
            "sockffi-adv-{}-dgram-client.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&client_path);
        let client_path_bytes = client_path.as_os_str().as_encoded_bytes();
        let client = ffi_socket(AF_UNIX, SOCK_DGRAM, 0).expect("socket");
        ffi_unix_bind(client, client_path_bytes).expect("bind client");
        let dest = SockAddr::Unix {
            path: path_bytes.to_vec(),
        };
        let n = ffi_sendmsg(client, &[b"hi"], 0, Some(&dest)).expect("sendmsg");
        assert_eq!(n, 2);
        let mut buf = [0u8; 2];
        let (got, addr, _flags) =
            ffi_recvmsg(server, &mut [&mut buf[..]], 0).expect("recvmsg");
        assert_eq!(got, 2);
        assert_eq!(&buf, b"hi");
        // The sender is the bound client datagram socket.
        assert_eq!(
            addr,
            Some(SockAddr::Unix {
                path: client_path_bytes.to_vec()
            })
        );
        close_raw(client).expect("close client");
        close_raw(server).expect("close server");
        std::fs::remove_file(&path).expect("remove socket file");
        std::fs::remove_file(&client_path).expect("remove client socket file");
    }

    /// AF_UNIX via FFI: `socket` → `bind` → `listen` →
    /// `connect` → `accept` → `send`/`recv`.
    #[test]
    #[cfg(unix)]
    fn af_unix_ffi_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "sockffi-adv-{}-unix.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let listener = ffi_socket(AF_UNIX, SOCK_STREAM, 0).expect("socket");
        ffi_unix_bind(listener, path_bytes).expect("bind");
        ffi::sys_listen(listener, 1).expect("listen");
        let client = ffi_socket(AF_UNIX, SOCK_STREAM, 0).expect("socket");
        ffi_unix_connect(client, path_bytes).expect("connect");
        let (accepted, _addr) = ffi_unix_accept(listener).expect("accept");
        let n = ffi::sys_send(client, b"ping".as_ptr(), 4, 0).expect("send");
        assert_eq!(n, 4);
        let mut buf = [0u8; 4];
        let got = ffi::sys_recv(accepted, buf.as_mut_ptr(), 4, 0).expect("recv");
        assert_eq!(got, 4);
        assert_eq!(&buf, b"ping");
        close_raw(client).expect("close client");
        close_raw(accepted).expect("close accepted");
        close_raw(listener).expect("close listener");
        std::fs::remove_file(&path).expect("remove socket file");
    }

    /// `poll_readable`: false with no data, true once data
    /// is waiting, false again after draining.
    #[test]
    #[cfg(unix)]
    fn poll_readable_reports_pending_data() {
        let (a, b) = unix_pair();
        assert!(!poll_readable(a));
        ffi::sys_send(b, b"x".as_ptr(), 1, 0).expect("send");
        assert!(poll_readable(a));
        let mut buf = [0u8; 1];
        ffi::sys_recv(a, buf.as_mut_ptr(), 1, 0).expect("recv");
        assert!(!poll_readable(a));
        close_raw(a).expect("close a");
        close_raw(b).expect("close b");
    }

    /// A listener reports readable only once a connection
    /// is pending.
    #[test]
    #[cfg(unix)]
    fn poll_readable_listener_pending_connection() {
        let path = std::env::temp_dir().join(format!(
            "sockffi-adv-{}-poll.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let listener = ffi_socket(AF_UNIX, SOCK_STREAM, 0).expect("socket");
        ffi_unix_bind(listener, path_bytes).expect("bind");
        ffi::sys_listen(listener, 1).expect("listen");
        assert!(!poll_readable(listener));
        let client = ffi_socket(AF_UNIX, SOCK_STREAM, 0).expect("socket");
        ffi_unix_connect(client, path_bytes).expect("connect");
        assert!(poll_readable(listener));
        let (accepted, _) = ffi_unix_accept(listener).expect("accept");
        close_raw(client).expect("close client");
        close_raw(accepted).expect("close accepted");
        close_raw(listener).expect("close listener");
        std::fs::remove_file(&path).expect("remove socket file");
    }

    /// `check_fd_valid`: a live fd passes, a closed fd
    /// fails `EBADF`.
    #[test]
    #[cfg(unix)]
    fn check_fd_valid_rejects_closed_fd() {
        let (a, b) = unix_pair();
        check_fd_valid(a).expect("live fd is valid");
        close_raw(a).expect("close a");
        assert_eq!(check_fd_valid(a), Err(9 /* EBADF */));
        close_raw(b).expect("close b");
    }

    /// `makefile_file` takes ownership: the `File` closes
    /// the fd when dropped.
    #[test]
    #[cfg(unix)]
    fn makefile_file_takes_ownership() {
        let fd = ffi_socket(AF_UNIX, SOCK_STREAM, 0).expect("socket");
        check_fd_valid(fd).expect("live fd is valid");
        let f = unsafe { makefile_file(fd) };
        drop(f);
        assert_eq!(check_fd_valid(fd), Err(9 /* EBADF */));
    }

    /// `SockAddr` round-trips through raw `sockaddr` bytes.
    #[test]
    fn sockaddr_roundtrip() {
        let v4 = SockAddr::Inet {
            ip: Ipv4Addr::new(127, 0, 0, 1),
            port: 8080,
        };
        let mut st = ffi::SockaddrStorage::new();
        let len = v4.to_raw(&mut st);
        assert_eq!(SockAddr::from_raw(&st, len), Some(v4));
        let v6 = SockAddr::Inet6 {
            ip: Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1),
            port: 443,
            flowinfo: 0,
            scope_id: 0,
        };
        let mut st = ffi::SockaddrStorage::new();
        let len = v6.to_raw(&mut st);
        assert_eq!(SockAddr::from_raw(&st, len), Some(v6));
        let un = SockAddr::Unix {
            path: b"/tmp/x.sock".to_vec(),
        };
        let mut st = ffi::SockaddrStorage::new();
        let len = un.to_raw(&mut st);
        assert_eq!(SockAddr::from_raw(&st, len), Some(un));
        let unnamed = SockAddr::Unix { path: Vec::new() };
        let mut st = ffi::SockaddrStorage::new();
        let len = unnamed.to_raw(&mut st);
        // S1's `len_for` counts family + path + NUL
        // terminator, so an unnamed address serialises to
        // 3 bytes (it is never passed to `bind` / `connect`
        // in production — there is nothing to bind to).
        assert_eq!(len, 3);
        // The kernel reports an unnamed peer with `len` 0.
        assert_eq!(SockAddr::from_raw(&st, 0), Some(unnamed));
    }

    /// `sock_key` / `raw_fd_from_key` round-trip.
    #[test]
    fn sock_key_roundtrip() {
        assert_eq!(raw_fd_from_key(sock_key(3 as RawFd)), 3 as RawFd);
        assert_eq!(sock_key(raw_fd_from_key(7)), 7);
    }
}
