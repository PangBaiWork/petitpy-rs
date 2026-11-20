//! S3 — socket FFI, advanced + platform + `select` cooperation.
//! (Lane `socket-ffi-s3`, brief `scripts/socket_ffi_brief.md` §3 S3.)
//!
//! Whole file is `#[cfg(feature = "unsafe-ffi")]`: the default
//! build sees nothing here and stays byte-identical.
//!
//! Contents:
//! - `sendmsg` / `recvmsg` (plain buffers only; no ancillary data
//!   — CPython's `sendmsg` has no ancillary parameter, and
//!   `recvmsg` with `ancbufsize != 0` is rejected by the
//!   `modsocket.rs` layer, never silently dropped).
//! - `fromfd` validation (`check_fd_valid`) and the `makefile`
//!   cooperation primitive (`makefile_file`).
//! - AF_UNIX via FFI (`ffi_unix_bind` / `ffi_unix_connect` /
//!   `ffi_unix_accept`; Windows AF_UNIX is probed at runtime).
//! - Windows platform finish: `WSAStartup` (`OnceLock`),
//!   `closesocket`, `ioctlsocket(FIONBIO)`, the loopback-TCP
//!   `socketpair`, `SOCKET`/`TableKey` conversion, and WSA
//!   error codes → errno via `win32_to_errno`.
//! - `poll(2)` readiness (`poll_readable`), the `select`
//!   cooperation: non-destructive, so `select` on a socket
//!   with pending data reports readable (H-select §4(1)).
//!
//! MERGE NOTES (read before merging S1/S2/S3):
//! - S1's `ffi.rs` (§2.1) will provide these same wrappers
//!   (`sys_socket`, `sys_bind`, ... `sys_sendmsg`, `sys_recvmsg`,
//!   `msghdr`, `iovec`, `addrinfo`, portable `errno()`). At merge,
//!   DELETE the private `mod ffi` below and rewire the call
//!   sites to `crate::support::ffi::sys_*` — the signatures are
//!   the frozen §2.1 ones, so the rewire is mechanical (only the
//!   path changes). `last_error` (this file's `ffi::errno`) is
//!   replaced by `ffi::errno()` the same way.
//! - `AdvAddr` is this lane's local address type (INET/INET6/
//!   UNIX). S1's `SockAddr` (§2.3) replaces it at merge; the
//!   variants and the `to_sockaddr` / `from_sockaddr` shapes
//!   match, so the call sites (`ffi_sendmsg`, `ffi_recvmsg`,
//!   `ffi_unix_*`) only change the type name.
//! - `RawFd` (`i32` Unix, `usize` Windows) is S1's
//!   `FfiSocket.fd` type; `sock_key` / `raw_fd_from_key` are
//!   the `TableKey = i64` conversions for it.
//! - The free functions here map to `FfiSocket` methods (§2.3)
//!   at merge: `ffi_sendmsg` → `FfiSocket::sendmsg`,
//!   `ffi_recvmsg` → `FfiSocket::recvmsg`,
//!   `ffi_unix_bind/connect/accept` → the AF_UNIX arms of
//!   `FfiSocket::bind/connect/accept`, `makefile_file` → the
//!   `makefile` FFI arm, `check_fd_valid` → the `fromfd`
//!   validation, `poll_readable` → the `select` probe.
//! - `detach` needs NO FFI primitive (`IntoRawFd` is safe `std`;
//!   for `FfiSocket` the fd is already a plain int). It is
//!   implemented as `sock_detach` in `modsocket.rs`; the FFI
//!   arm at merge is one line (`SocketState::Ffi(f) => f.fd`).
//!
//! INTERFACE GAPS (no S1 interface to build on yet — this file
//! is self-contained so it compiles standalone):
//! - The private `mod ffi` below duplicates the frozen §2.1
//!   wrappers S1 will put in `ffi.rs` (see merge note). It is
//!   temporary by design; it is NOT a second implementation to
//!   keep.
//! - `SockaddrStorage` / `AdvAddr` duplicate the frozen §2.1/§2.3
//!   address types for the same reason.
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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
#[cfg(unix)]
use std::os::unix::io::FromRawFd;
#[cfg(windows)]
use std::os::windows::io::FromRawSocket;

/// The OS socket handle: `i32` fd on Unix, `SOCKET` (`usize`)
/// on Windows. S1's `FfiSocket.fd` is this type (brief §2.3).
#[cfg(unix)]
pub type RawFd = i32;
#[cfg(windows)]
pub type RawFd = usize;

/// OS-level address families (the values the syscalls see —
/// note `AF_INET6` is 10 on Unix but 23 on Winsock; the
/// Python-facing constants in `modsocket.rs` are separate).
const AF_INET: i32 = 2;
#[cfg(unix)]
const AF_INET6: i32 = 10;
#[cfg(windows)]
const AF_INET6: i32 = 23;
#[cfg(unix)]
const AF_UNIX: i32 = 1;
#[cfg(windows)]
const AF_UNIX: i32 = 29;
/// `SOCK_STREAM` / `SOCK_DGRAM`: only the Windows
/// `socketpair` path and the unit tests use them (Unix
/// production code passes the type through `FfiSocket`),
/// so they exist on Windows and in test builds only.
#[cfg(any(windows, test))]
const SOCK_STREAM: i32 = 1;
#[cfg(any(windows, test))]
const SOCK_DGRAM: i32 = 2;

/// `SOL_SOCKET` / `SO_TYPE` for the `fromfd` validation
/// (`getsockopt` on Windows; Unix uses `fcntl(F_GETFD)`).
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

/// Winsock `INVALID_SOCKET` (Windows).
#[cfg(windows)]
const INVALID_SOCKET: usize = usize::MAX;

/// Raw `extern "C"` declarations + safe wrappers, following
/// the `src/support/ffi.rs` pattern: every wrapper returns
/// `Result<_, i32>` (POSIX errno; on Windows the Winsock code
/// is converted through `win32_to_errno` inside the wrapper,
/// so callers only ever see POSIX errnos). See the module
/// docs for the S1 merge note — this `mod ffi` is temporary.
#[allow(dead_code)]
// Allowed: this module is the frozen §2.1 wrapper layer S1
// will provide in `ffi.rs`; on Unix several wrappers are only
// reached via the merge-API functions below (unit tests +
// S1's `FfiSocket` at merge), so they read as dead until the
// merge. The whole module is deleted at merge.
mod ffi {
    use std::os::raw::c_int;
    #[cfg(windows)]
    use std::sync::OnceLock;

    // ── errno (Unix, per-platform) ──
    #[cfg(target_os = "linux")]
    extern "C" {
        fn __errno_location() -> *mut c_int;
    }
    #[cfg(target_os = "macos")]
    extern "C" {
        fn __error() -> *mut c_int;
    }
    #[cfg(target_os = "android")]
    extern "C" {
        fn __errno() -> *mut c_int;
    }
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "macos", target_os = "android"))
    ))]
    compile_error!(
        "socket FFI errno: unsupported Unix target — add the platform errno accessor (__errno_location / __error / __errno) here"
    );

    /// The calling thread's errno, read immediately after a
    /// failed call (Unix). FFI invariant: the accessor never
    /// returns null and the cell is only read here.
    #[cfg(unix)]
    pub(crate) fn errno() -> i32 {
        #[cfg(target_os = "linux")]
        unsafe {
            *__errno_location()
        }
        #[cfg(target_os = "macos")]
        unsafe {
            *__error()
        }
        #[cfg(target_os = "android")]
        unsafe {
            *__errno()
        }
    }

    /// The last Winsock error as a POSIX errno (Windows).
    /// FFI invariant: `WSAGetLastError` is infallible and
    /// returns the calling thread's last Winsock code.
    #[cfg(windows)]
    pub(crate) fn wsa_error() -> i32 {
        crate::runtime::modos::win32_to_errno(unsafe { WSAGetLastError() })
    }

    /// `WSAStartup(MAKEWORD(2,2))`, once per process
    /// (Windows). Must run before any Winsock call.
    #[cfg(windows)]
    pub(crate) fn ensure_winsock() -> Result<(), i32> {
        static STARTUP: OnceLock<Result<(), i32>> = OnceLock::new();
        *STARTUP.get_or_init(|| {
            // WSADATA is 408 bytes on 64-bit Windows; it is
            // only written by WSAStartup, never read here —
            // an opaque zeroed buffer is enough.
            let mut data = [0u8; 408];
            // FFI invariant: WSAStartup takes a version word
            // and a caller-provided buffer; it returns 0 on
            // success, else the Winsock error code directly
            // (not via WSAGetLastError).
            let r = unsafe { WSAStartup(0x0202, data.as_mut_ptr()) };
            if r == 0 {
                Ok(())
            } else {
                Err(crate::runtime::modos::win32_to_errno(r))
            }
        })
    }

    // ── FFI structs (private to this module) ──
    /// `struct iovec` (Unix): one scatter/gather element.
    #[repr(C)]
    #[cfg(unix)]
    pub(crate) struct Iovec {
        pub iov_base: *mut u8,
        pub iov_len: usize,
    }

    /// `struct msghdr` (Unix, x86_64/aarch64: 56 bytes).
    #[repr(C)]
    #[cfg(unix)]
    pub(crate) struct Msghdr {
        pub msg_name: *mut u8,
        pub msg_namelen: u32,
        pub msg_iov: *mut Iovec,
        pub msg_iovlen: usize,
        pub msg_control: *mut u8,
        pub msg_controllen: usize,
        pub msg_flags: c_int,
    }

    /// `struct pollfd` (Unix): one fd to probe.
    #[repr(C)]
    #[cfg(unix)]
    pub(crate) struct Pollfd {
        pub fd: c_int,
        pub events: i16,
        pub revents: i16,
    }

    /// `WSABUF` (Windows): one scatter/gather element.
    #[repr(C)]
    #[cfg(windows)]
    pub(crate) struct WsaBuf {
        pub len: u32,
        pub buf: *mut u8,
    }

    /// `WSAMSG` (Windows, 64-bit: 56 bytes).
    #[repr(C)]
    #[cfg(windows)]
    pub(crate) struct WsaMsg {
        pub name: *mut u8,
        pub namelen: c_int,
        pub lp_buffers: *mut WsaBuf,
        pub buffer_count: u32,
        pub control: WsaBuf,
        pub flags: u32,
    }

    /// `fd_set` (Windows): `fd_count` + up to 64 sockets.
    #[repr(C)]
    #[cfg(windows)]
    pub(crate) struct FdSet {
        pub fd_count: u32,
        pub fd_array: [usize; 64],
    }

    /// `struct timeval` (Windows): seconds + microseconds
    /// (`long` is `i32` on Windows).
    #[repr(C)]
    #[cfg(windows)]
    pub(crate) struct Timeval {
        pub tv_sec: i32,
        pub tv_usec: i32,
    }

    // ── Unix socket calls ──
    #[cfg(unix)]
    extern "C" {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        fn bind(fd: c_int, addr: *const u8, len: u32) -> c_int;
        fn listen(fd: c_int, backlog: c_int) -> c_int;
        fn accept(fd: c_int, addr: *mut u8, len: *mut u32) -> c_int;
        fn connect(fd: c_int, addr: *const u8, len: u32) -> c_int;
        fn send(fd: c_int, buf: *const u8, len: usize, flags: c_int) -> isize;
        fn recv(fd: c_int, buf: *mut u8, len: usize, flags: c_int) -> isize;
        fn sendmsg(fd: c_int, msg: *const Msghdr, flags: c_int) -> isize;
        fn recvmsg(fd: c_int, msg: *mut Msghdr, flags: c_int) -> isize;
        fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int;
        fn poll(fds: *mut Pollfd, nfds: u32, timeout: c_int) -> c_int;
    }

    /// `socket(2)`. FFI invariant: three plain ints; on -1
    /// the thread errno holds the failure.
    #[cfg(unix)]
    pub(crate) fn sys_socket(domain: i32, ty: i32, proto: i32) -> Result<super::RawFd, i32> {
        let r = unsafe { socket(domain, ty, proto) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }

    /// `bind(2)`. FFI invariant: the address bytes are a
    /// caller-built `sockaddr` of `len` bytes; on -1 errno
    /// holds the failure.
    #[cfg(unix)]
    pub(crate) fn sys_bind(fd: super::RawFd, addr: *const u8, len: u32) -> Result<(), i32> {
        let r = unsafe { bind(fd, addr, len) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }

    /// `listen(2)`. FFI invariant: plain ints; on -1 errno
    /// holds the failure.
    #[cfg(unix)]
    pub(crate) fn sys_listen(fd: super::RawFd, backlog: i32) -> Result<(), i32> {
        let r = unsafe { listen(fd, backlog) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }

    /// `accept(2)`. FFI invariant: `addr`/`len` are a
    /// caller-provided buffer; on -1 errno holds the
    /// failure (EAGAIN on a non-blocking listener is a
    /// normal outcome, not a fault).
    #[cfg(unix)]
    pub(crate) fn sys_accept(
        fd: super::RawFd,
        addr: *mut u8,
        len: *mut u32,
    ) -> Result<super::RawFd, i32> {
        let r = unsafe { accept(fd, addr, len) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }

    /// `connect(2)`. FFI invariant: the address bytes are a
    /// caller-built `sockaddr` of `len` bytes; on -1 errno
    /// holds the failure.
    #[cfg(unix)]
    pub(crate) fn sys_connect(fd: super::RawFd, addr: *const u8, len: u32) -> Result<(), i32> {
        let r = unsafe { connect(fd, addr, len) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }

    /// `send(2)`. FFI invariant: `buf` points at `len`
    /// readable bytes for the call; -1 is the only error
    /// return (0 is a valid short write).
    #[cfg(unix)]
    pub(crate) fn sys_send(
        fd: super::RawFd,
        buf: *const u8,
        len: usize,
        flags: i32,
    ) -> Result<usize, i32> {
        let r = unsafe { send(fd, buf, len, flags) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r as usize)
        }
    }

    /// `recv(2)`. FFI invariant: `buf` points at `len`
    /// writable bytes for the call; -1 is the only error
    /// return (0 is EOF, not an error).
    #[cfg(unix)]
    pub(crate) fn sys_recv(
        fd: super::RawFd,
        buf: *mut u8,
        len: usize,
        flags: i32,
    ) -> Result<usize, i32> {
        let r = unsafe { recv(fd, buf, len, flags) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r as usize)
        }
    }

    /// `sendmsg(2)`. FFI invariant: `msg` points at a
    /// caller-built `Msghdr` whose iovecs point at live
    /// buffers; -1 is the only error return.
    #[cfg(unix)]
    pub(crate) fn sys_sendmsg(
        fd: super::RawFd,
        msg: *const Msghdr,
        flags: i32,
    ) -> Result<usize, i32> {
        let r = unsafe { sendmsg(fd, msg, flags) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r as usize)
        }
    }

    /// `recvmsg(2)`. FFI invariant: `msg` points at a
    /// caller-built `Msghdr` whose iovecs point at live
    /// writable buffers; -1 is the only error return.
    #[cfg(unix)]
    pub(crate) fn sys_recvmsg(
        fd: super::RawFd,
        msg: *mut Msghdr,
        flags: i32,
    ) -> Result<usize, i32> {
        let r = unsafe { recvmsg(fd, msg, flags) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r as usize)
        }
    }

    /// `fcntl(2)`. FFI invariant: plain ints; on -1 errno
    /// holds the failure.
    #[cfg(unix)]
    pub(crate) fn sys_fcntl(fd: super::RawFd, cmd: i32, arg: i32) -> Result<i32, i32> {
        let r = unsafe { fcntl(fd, cmd, arg) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }

    /// `poll(2)`. FFI invariant: `fds` points at `nfds`
    /// caller-built `Pollfd`s; -1 is the only error return
    /// (0 is a timeout, not an error).
    #[cfg(unix)]
    pub(crate) fn sys_poll(fds: *mut Pollfd, nfds: u32, timeout_ms: i32) -> Result<i32, i32> {
        let r = unsafe { poll(fds, nfds, timeout_ms) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }

    // ── Winsock calls (ws2_32) ──
    #[cfg(windows)]
    #[link(name = "ws2_32")]
    extern "C" {
        fn WSAStartup(w_version_requested: u16, lp_wsa_data: *mut u8) -> c_int;
        fn WSAGetLastError() -> c_int;
        fn socket(af: c_int, kind: c_int, protocol: c_int) -> usize;
        fn bind(s: usize, addr: *const u8, namelen: c_int) -> c_int;
        fn listen(s: usize, backlog: c_int) -> c_int;
        fn accept(s: usize, addr: *mut u8, addrlen: *mut c_int) -> usize;
        fn connect(s: usize, addr: *const u8, namelen: c_int) -> c_int;
        fn send(s: usize, buf: *const u8, len: c_int, flags: c_int) -> c_int;
        fn recv(s: usize, buf: *mut u8, len: c_int, flags: c_int) -> c_int;
        fn getsockname(s: usize, addr: *mut u8, addrlen: *mut c_int) -> c_int;
        fn getsockopt(
            s: usize,
            level: c_int,
            optname: c_int,
            optval: *mut u8,
            optlen: *mut c_int,
        ) -> c_int;
        fn closesocket(s: usize) -> c_int;
        fn ioctlsocket(s: usize, cmd: c_int, argp: *mut u32) -> c_int;
        fn select(
            nfds: c_int,
            readfds: *mut FdSet,
            writefds: *mut FdSet,
            exceptfds: *mut FdSet,
            timeout: *const Timeval,
        ) -> c_int;
        fn WSASendMsg(
            s: usize,
            msg: *const WsaMsg,
            flags: u32,
            sent: *mut u32,
            overlapped: *mut u8,
            completion: *mut u8,
        ) -> c_int;
        fn WSARecvMsg(
            s: usize,
            msg: *mut WsaMsg,
            received: *mut u32,
            overlapped: *mut u8,
            completion: *mut u8,
        ) -> c_int;
    }

    /// Winsock `socket`. FFI invariant: three plain ints;
    /// on `INVALID_SOCKET` the Winsock error holds the
    /// failure. Every Winsock wrapper below calls
    /// `ensure_winsock()` first, so no caller can forget
    /// `WSAStartup`.
    #[cfg(windows)]
    pub(crate) fn sys_socket(domain: i32, ty: i32, proto: i32) -> Result<super::RawFd, i32> {
        ensure_winsock()?;
        let r = unsafe { socket(domain, ty, proto) };
        if r == super::INVALID_SOCKET {
            Err(wsa_error())
        } else {
            Ok(r)
        }
    }

    /// Winsock `bind` (`namelen` is `int` on Winsock,
    /// unlike the Unix `socklen_t`).
    #[cfg(windows)]
    pub(crate) fn sys_bind(fd: super::RawFd, addr: *const u8, len: i32) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { bind(fd, addr, len) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// Winsock `listen`.
    #[cfg(windows)]
    pub(crate) fn sys_listen(fd: super::RawFd, backlog: i32) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { listen(fd, backlog) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// Winsock `accept` (`addrlen` is `int*` on Winsock).
    #[cfg(windows)]
    pub(crate) fn sys_accept(
        fd: super::RawFd,
        addr: *mut u8,
        len: *mut c_int,
    ) -> Result<super::RawFd, i32> {
        ensure_winsock()?;
        let r = unsafe { accept(fd, addr, len) };
        if r == super::INVALID_SOCKET {
            Err(wsa_error())
        } else {
            Ok(r)
        }
    }

    /// Winsock `connect`.
    #[cfg(windows)]
    pub(crate) fn sys_connect(fd: super::RawFd, addr: *const u8, len: i32) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { connect(fd, addr, len) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// Winsock `send` (`len` is `int`; clamped, a buffer
    /// over 2 GiB cannot be sent in one call anyway).
    #[cfg(windows)]
    pub(crate) fn sys_send(
        fd: super::RawFd,
        buf: *const u8,
        len: usize,
        flags: i32,
    ) -> Result<usize, i32> {
        ensure_winsock()?;
        let len = len.min(i32::MAX as usize) as c_int;
        let r = unsafe { send(fd, buf, len, flags) };
        if r < 0 {
            Err(wsa_error())
        } else {
            Ok(r as usize)
        }
    }

    /// Winsock `recv`.
    #[cfg(windows)]
    pub(crate) fn sys_recv(
        fd: super::RawFd,
        buf: *mut u8,
        len: usize,
        flags: i32,
    ) -> Result<usize, i32> {
        ensure_winsock()?;
        let len = len.min(i32::MAX as usize) as c_int;
        let r = unsafe { recv(fd, buf, len, flags) };
        if r < 0 {
            Err(wsa_error())
        } else {
            Ok(r as usize)
        }
    }

    /// Winsock `getsockname`.
    #[cfg(windows)]
    pub(crate) fn sys_getsockname(
        fd: super::RawFd,
        addr: *mut u8,
        len: *mut c_int,
    ) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { getsockname(fd, addr, len) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// Winsock `getsockopt` (used by the `fromfd`
    /// validation on Windows).
    #[cfg(windows)]
    pub(crate) fn sys_getsockopt(
        fd: super::RawFd,
        level: i32,
        opt: i32,
        val: *mut u8,
        len: *mut c_int,
    ) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { getsockopt(fd, level, opt, val, len) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// `closesocket` — Windows closes a `SOCKET` with this,
    /// NOT `close` (which would corrupt the CRT fd table).
    #[cfg(windows)]
    pub(crate) fn sys_closesocket(fd: super::RawFd) -> Result<(), i32> {
        ensure_winsock()?;
        // FFI invariant: `closesocket` takes any SOCKET value;
        // an invalid one fails with WSAENOTSOCK, it cannot fault.
        let r = unsafe { closesocket(fd) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// `ioctlsocket` (Windows non-blocking switch).
    #[cfg(windows)]
    pub(crate) fn sys_ioctlsocket(fd: super::RawFd, cmd: i32, argp: *mut u32) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe { ioctlsocket(fd, cmd, argp) };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// Winsock `select` (the `nfds` argument is ignored on
    /// Windows; pass 0).
    #[cfg(windows)]
    pub(crate) fn sys_select(
        readfds: *mut FdSet,
        writefds: *mut FdSet,
        exceptfds: *mut FdSet,
        timeout: *const Timeval,
    ) -> Result<i32, i32> {
        ensure_winsock()?;
        let r = unsafe { select(0, readfds, writefds, exceptfds, timeout) };
        if r < 0 {
            Err(wsa_error())
        } else {
            Ok(r)
        }
    }

    /// `WSASendMsg` (Windows). `sent` receives the byte
    /// count; the overlapped/completion args are null
    /// (synchronous call). Returns 0 on success.
    #[cfg(windows)]
    pub(crate) fn sys_sendmsg(
        fd: super::RawFd,
        msg: *const WsaMsg,
        flags: u32,
        sent: *mut u32,
    ) -> Result<(), i32> {
        ensure_winsock()?;
        // FFI invariant: `msg` points at a caller-built
        // `WsaMsg` whose buffers point at live memory.
        let r = unsafe {
            WSASendMsg(
                fd,
                msg,
                flags,
                sent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }

    /// `WSARecvMsg` (Windows). `received` gets the byte
    /// count; the message flags come back in
    /// `msg.flags` (there is no separate flags out-param —
    /// unlike `WSASendMsg`, `WSARecvMsg` takes 5 args).
    #[cfg(windows)]
    pub(crate) fn sys_recvmsg(
        fd: super::RawFd,
        msg: *mut WsaMsg,
        received: *mut u32,
    ) -> Result<(), i32> {
        ensure_winsock()?;
        let r = unsafe {
            WSARecvMsg(fd, msg, received, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if r != 0 {
            Err(wsa_error())
        } else {
            Ok(())
        }
    }
}

/// Raw address buffer (128 bytes, 8-aligned —
/// `sockaddr_storage` on Linux/macOS/Windows). Addresses
/// are read/written as raw bytes (family at offset 0,
/// port/address at the `sockaddr_in` / `sockaddr_in6` /
/// `sockaddr_un` offsets), so no per-family struct is
/// needed — the offsets are identical on all three
/// platforms, and the macOS `u8` family + pad byte reads
/// back as the same `u16` value on little-endian.
#[repr(C, align(8))]
pub struct SockaddrStorage {
    pub data: [u8; 128],
}

impl SockaddrStorage {
    pub fn zeroed() -> Self {
        Self { data: [0u8; 128] }
    }
}

/// A socket address (this lane's local type — S1's
/// `SockAddr` (§2.3) replaces it at merge; the variants
/// and the conversion shapes match).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvAddr {
    /// IPv4 (`SocketAddr::V4`).
    Inet(SocketAddr),
    /// IPv6 (`SocketAddr::V6`).
    Inet6(SocketAddr),
    /// `AF_UNIX` path bytes: empty = unnamed, a leading
    /// NUL byte = abstract-namespace name.
    Unix(Vec<u8>),
}

/// `sockaddr_un` length for a path: family (2) + the path
/// + its NUL terminator for filesystem paths; an abstract
/// name (leading NUL) has no terminator; empty is unnamed.
fn unix_sockaddr_len(path: &[u8]) -> u32 {
    if path.is_empty() {
        2
    } else if path[0] == 0 {
        2 + path.len() as u32
    } else {
        2 + path.len() as u32 + 1
    }
}

impl AdvAddr {
    /// Serialise to raw `sockaddr` bytes + length.
    pub fn to_sockaddr(&self) -> (SockaddrStorage, u32) {
        let mut st = SockaddrStorage::zeroed();
        match self {
            AdvAddr::Inet(a) => {
                // sockaddr_in: family u16 @0, port BE u16 @2,
                // addr 4 bytes @4.
                let ip = match a.ip() {
                    IpAddr::V4(v4) => v4,
                    _ => unreachable!("AdvAddr::Inet holds an IPv4 address"),
                };
                st.data[0..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
                st.data[2..4].copy_from_slice(&a.port().to_be_bytes());
                st.data[4..8].copy_from_slice(&ip.octets());
                (st, 16)
            }
            AdvAddr::Inet6(a) => {
                // sockaddr_in6: family u16 @0, port BE u16 @2,
                // flowinfo u32 @4 (0), addr 16 bytes @8,
                // scope_id u32 @24 (0).
                let ip = match a.ip() {
                    IpAddr::V6(v6) => v6,
                    _ => unreachable!("AdvAddr::Inet6 holds an IPv6 address"),
                };
                st.data[0..2].copy_from_slice(&(AF_INET6 as u16).to_ne_bytes());
                st.data[2..4].copy_from_slice(&a.port().to_be_bytes());
                st.data[8..24].copy_from_slice(&ip.octets());
                (st, 28)
            }
            AdvAddr::Unix(path) => {
                // sockaddr_un: family u16 @0, path @2.
                st.data[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
                let len = unix_sockaddr_len(path);
                st.data[2..2 + path.len()].copy_from_slice(path);
                (st, len)
            }
        }
    }

    /// Parse raw `sockaddr` bytes (as returned by
    /// `accept` / `recvmsg`). `None` for an unknown family
    /// or a truncated address.
    pub fn from_sockaddr(data: &[u8]) -> Option<AdvAddr> {
        if data.len() < 2 {
            return None;
        }
        let family = u16::from_ne_bytes([data[0], data[1]]) as i32;
        match family {
            AF_INET => {
                if data.len() < 8 {
                    return None;
                }
                let port = u16::from_be_bytes([data[2], data[3]]);
                let ip = Ipv4Addr::new(data[4], data[5], data[6], data[7]);
                Some(AdvAddr::Inet(SocketAddr::new(IpAddr::V4(ip), port)))
            }
            AF_INET6 => {
                if data.len() < 24 {
                    return None;
                }
                let port = u16::from_be_bytes([data[2], data[3]]);
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&data[8..24]);
                Some(AdvAddr::Inet6(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::from(octets)),
                    port,
                )))
            }
            AF_UNIX => {
                let raw = &data[2..];
                if raw.first() == Some(&0) {
                    // Abstract name: the leading NUL is part
                    // of the name, not a terminator.
                    Some(AdvAddr::Unix(raw.to_vec()))
                } else {
                    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                    Some(AdvAddr::Unix(raw[..end].to_vec()))
                }
            }
            _ => None,
        }
    }
}

/// Table-key conversions (`TableKey = i64`; S1's
/// `FfiSocket` uses these at merge). On Unix an fd is a
/// small non-negative `i32`; on Windows a `SOCKET` value
/// is < 2^63 in practice, so the truncating cast is safe.
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
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
/// S1's `FfiSocket::new` is the production path at merge).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
pub(crate) fn ffi_socket(domain: i32, ty: i32, proto: i32) -> Result<RawFd, i32> {
    ffi::sys_socket(domain, ty, proto)
}

/// Close a raw fd (`close(2)` on Unix, `closesocket` on
/// Windows — never the other one).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
pub(crate) fn close_raw(fd: RawFd) -> Result<(), i32> {
    #[cfg(unix)]
    {
        crate::support::ffi::sys_close(fd)
    }
    #[cfg(windows)]
    {
        ffi::sys_closesocket(fd)
    }
}

/// `sendmsg` (`FfiSocket::sendmsg` at merge): gather the
/// plain buffers into one message. There is no ancillary
/// data — the control buffer is always empty (CPython's
/// `sendmsg` takes no ancillary parameter; the `modsocket`
/// layer rejects `recvmsg` ancillary requests). `addr` is
/// the destination for unconnected sockets (`None` sends
/// to the connected peer). No `EINTR` retry (the existing
/// `ffi.rs` wrappers do none either).
pub(crate) fn ffi_sendmsg(
    fd: RawFd,
    bufs: &[&[u8]],
    flags: i32,
    addr: Option<&AdvAddr>,
) -> Result<usize, i32> {
    let (mut storage, addrlen) = match addr {
        Some(a) => {
            let (s, l) = a.to_sockaddr();
            (Some(s), l)
        }
        None => (None, 0),
    };
    #[cfg(unix)]
    {
        let mut iov: Vec<ffi::Iovec> = bufs
            .iter()
            .map(|b| ffi::Iovec {
                iov_base: b.as_ptr() as *mut u8,
                iov_len: b.len(),
            })
            .collect();
        let (name_ptr, name_len) = match &mut storage {
            Some(s) => (s.data.as_mut_ptr(), addrlen),
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
        let (name_ptr, name_len) = match &mut storage {
            Some(s) => (s.data.as_mut_ptr(), addrlen as i32),
            None => (std::ptr::null_mut(), 0),
        };
        // FFI invariant: the `WsaMsg` borrows `wsa_bufs`
        // and `storage`, which both outlive the call.
        let msg = ffi::WsaMsg {
            name: name_ptr,
            namelen: name_len,
            lp_buffers: wsa_bufs.as_mut_ptr(),
            buffer_count: wsa_bufs.len() as u32,
            control: ffi::WsaBuf {
                len: 0,
                buf: std::ptr::null_mut(),
            },
            flags: 0,
        };
        let mut sent: u32 = 0;
        ffi::sys_sendmsg(fd, &msg, flags as u32, &mut sent)?;
        Ok(sent as usize)
    }
}

/// `recvmsg` (`FfiSocket::recvmsg` at merge): scatter one
/// message into the buffers. Returns `(bytes, address,
/// msg_flags)`; the address is `None` when the kernel
/// left the name buffer empty. No ancillary buffer is
/// requested, so none is ever returned.
pub(crate) fn ffi_recvmsg(
    fd: RawFd,
    bufs: &mut [&mut [u8]],
    flags: i32,
) -> Result<(usize, Option<AdvAddr>, i32), i32> {
    #[cfg(unix)]
    {
        let mut iov: Vec<ffi::Iovec> = bufs
            .iter_mut()
            .map(|b| ffi::Iovec {
                iov_base: b.as_mut_ptr(),
                iov_len: b.len(),
            })
            .collect();
        let mut storage = SockaddrStorage::zeroed();
        let mut namelen: u32 = storage.data.len() as u32;
        // FFI invariant: the `Msghdr` borrows `iov` and
        // `storage`, which both outlive the call.
        let mut hdr = ffi::Msghdr {
            msg_name: storage.data.as_mut_ptr(),
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
            AdvAddr::from_sockaddr(&storage.data[..namelen as usize])
        } else {
            None
        };
        Ok((n, addr, hdr.msg_flags))
    }
    #[cfg(windows)]
    {
        let mut wsa_bufs: Vec<ffi::WsaBuf> = bufs
            .iter_mut()
            .map(|b| ffi::WsaBuf {
                len: b.len().min(u32::MAX as usize) as u32,
                buf: b.as_mut_ptr(),
            })
            .collect();
        let mut storage = SockaddrStorage::zeroed();
        // FFI invariant: the `WsaMsg` borrows `wsa_bufs`
        // and `storage`, which both outlive the call; the
        // flags come back in `msg.flags` (WSARecvMsg has
        // no separate flags out-param).
        let mut msg = ffi::WsaMsg {
            name: storage.data.as_mut_ptr(),
            namelen: storage.data.len() as i32,
            lp_buffers: wsa_bufs.as_mut_ptr(),
            buffer_count: wsa_bufs.len() as u32,
            control: ffi::WsaBuf {
                len: 0,
                buf: std::ptr::null_mut(),
            },
            flags: 0,
        };
        let mut received: u32 = 0;
        ffi::sys_recvmsg(fd, &mut msg, &mut received)?;
        let addr = if msg.namelen > 0 {
            AdvAddr::from_sockaddr(&storage.data[..msg.namelen as usize])
        } else {
            None
        };
        Ok((received as usize, addr, msg.flags as i32))
    }
}

/// Validate an fd for `fromfd` (the `fromfd` validation
/// in `modsocket.rs`). Unix: `fcntl(F_GETFD)` — the fd
/// must be open; deliberately NO `SO_TYPE` check, so a
/// non-socket fd (the integration test wraps `/dev/null`)
/// is accepted. Windows: `getsockopt(SO_TYPE)` — a real
/// socket is required there (no `/dev/null` equivalent
/// in the test suite); a non-socket handle fails with
/// `WSAENOTSOCK` → `ENOTSOCK`.
pub(crate) fn check_fd_valid(fd: RawFd) -> Result<(), i32> {
    #[cfg(unix)]
    {
        ffi::sys_fcntl(fd, F_GETFD, 0).map(|_| ())
    }
    #[cfg(windows)]
    {
        let mut val: i32 = 0;
        let mut len: i32 = std::mem::size_of::<i32>() as i32;
        ffi::sys_getsockopt(
            fd,
            SOL_SOCKET,
            SO_TYPE,
            &mut val as *mut i32 as *mut u8,
            &mut len,
        )
    }
}

/// Wrap a raw fd in a `File` for the `makefile` FFI
/// cooperation (the `makefile` FFI arm at merge).
///
/// # Safety
/// The caller must have removed the fd from the sockets
/// table first and must own it: this call transfers
/// ownership to the returned `File` (closing the `File`
/// closes the fd; the socket must NOT close it afterwards
/// — otherwise the fd number is closed twice).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
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

/// AF_UNIX `bind(2)` on an FFI fd (the AF_UNIX arm of
/// `FfiSocket::bind` at merge). `path` is the filesystem
/// path bytes (empty = unnamed; a leading NUL = abstract
/// namespace, Linux only).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
pub(crate) fn ffi_unix_bind(fd: RawFd, path: &[u8]) -> Result<(), i32> {
    let addr = AdvAddr::Unix(path.to_vec());
    let (storage, len) = addr.to_sockaddr();
    #[cfg(unix)]
    {
        ffi::sys_bind(fd, storage.data.as_ptr(), len)
    }
    #[cfg(windows)]
    {
        ffi::sys_bind(fd, storage.data.as_ptr(), len as i32)
    }
}

/// AF_UNIX `connect(2)` on an FFI fd (the AF_UNIX arm of
/// `FfiSocket::connect` at merge).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
pub(crate) fn ffi_unix_connect(fd: RawFd, path: &[u8]) -> Result<(), i32> {
    let addr = AdvAddr::Unix(path.to_vec());
    let (storage, len) = addr.to_sockaddr();
    #[cfg(unix)]
    {
        ffi::sys_connect(fd, storage.data.as_ptr(), len)
    }
    #[cfg(windows)]
    {
        ffi::sys_connect(fd, storage.data.as_ptr(), len as i32)
    }
}

/// AF_UNIX `accept(2)` on an FFI fd (the AF_UNIX arm of
/// `FfiSocket::accept` at merge). Returns the new fd and
/// the peer address (`Unix(vec![])` when unnamed).
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
pub(crate) fn ffi_unix_accept(fd: RawFd) -> Result<(RawFd, AdvAddr), i32> {
    let mut storage = SockaddrStorage::zeroed();
    #[cfg(unix)]
    {
        let mut len: u32 = storage.data.len() as u32;
        let new_fd = ffi::sys_accept(fd, storage.data.as_mut_ptr(), &mut len)?;
        let addr = AdvAddr::from_sockaddr(&storage.data[..len as usize])
            .unwrap_or(AdvAddr::Unix(Vec::new()));
        Ok((new_fd, addr))
    }
    #[cfg(windows)]
    {
        let mut len: i32 = storage.data.len() as i32;
        let new_fd = ffi::sys_accept(fd, storage.data.as_mut_ptr(), &mut len)?;
        let addr = AdvAddr::from_sockaddr(&storage.data[..len as usize])
            .unwrap_or(AdvAddr::Unix(Vec::new()));
        Ok((new_fd, addr))
    }
}

/// AF_UNIX support probe. Unix: always true (`AF_UNIX`
/// is universal there). Windows: creates a throwaway
/// `AF_UNIX` socket — success (Win10 1803+) means the
/// Winsock AF_UNIX path works, failure means keep the
/// existing `NotImplementedError`.
#[allow(dead_code)] // Merge API (see module docs): called by the unit tests here and by S1's `FfiSocket` at merge.
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
        let addr = AdvAddr::Inet(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            0,
        ));
        let (storage, len) = addr.to_sockaddr();
        ffi::sys_bind(listener, storage.data.as_ptr(), len as i32)?;
        let mut bound = SockaddrStorage::zeroed();
        let mut bound_len: i32 = bound.data.len() as i32;
        ffi::sys_getsockname(listener, bound.data.as_mut_ptr(), &mut bound_len)?;
        let bound_addr = AdvAddr::from_sockaddr(&bound.data[..bound_len as usize])
            .ok_or(22 /* EINVAL */)?;
        match bound_addr {
            AdvAddr::Inet(a) => Ok(a.port()),
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
    let peer = AdvAddr::Inet(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        port,
    ));
    let (storage, len) = peer.to_sockaddr();
    if let Err(e) = ffi::sys_connect(client, storage.data.as_ptr(), len as i32) {
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    // Accept with a deadline: listener non-blocking, poll
    // with Winsock `select` until readable or 5 s pass.
    let mut one: u32 = 1;
    if let Err(e) = ffi::sys_ioctlsocket(listener, FIONBIO, &mut one) {
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let accepted = loop {
        let mut set = ffi::FdSet {
            fd_count: 1,
            fd_array: [0usize; 64],
        };
        set.fd_array[0] = listener;
        let tv = ffi::Timeval {
            tv_sec: 0,
            tv_usec: 100_000,
        };
        match ffi::sys_select(&mut set, std::ptr::null_mut(), std::ptr::null_mut(), &tv) {
            Ok(n) if n > 0 => break,
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
        let mut storage = SockaddrStorage::zeroed();
        let mut len: i32 = storage.data.len() as i32;
        match ffi::sys_accept(listener, storage.data.as_mut_ptr(), &mut len) {
            Ok(fd) => break fd,
            Err(e) => {
                // EAGAIN while the connection is still
                // being queued: keep polling until the
                // deadline; any other error fails.
                if e == 11 /* EAGAIN */ && Instant::now() < deadline {
                    continue;
                }
                let _ = close_raw(client);
                let _ = close_raw(listener);
                return Err(e);
            }
        }
    };
    // Both ends back to blocking.
    let mut zero: u32 = 0;
    if let Err(e) = ffi::sys_ioctlsocket(accepted, FIONBIO, &mut zero)
        .and_then(|_| ffi::sys_ioctlsocket(client, FIONBIO, &mut zero))
    {
        let _ = close_raw(accepted);
        let _ = close_raw(client);
        let _ = close_raw(listener);
        return Err(e);
    }
    let _ = close_raw(listener);
    Ok((client, accepted))
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
        let dest = AdvAddr::Unix(path_bytes.to_vec());
        let n = ffi_sendmsg(client, &[b"hi"], 0, Some(&dest)).expect("sendmsg");
        assert_eq!(n, 2);
        let mut buf = [0u8; 2];
        let (got, addr, _flags) =
            ffi_recvmsg(server, &mut [&mut buf[..]], 0).expect("recvmsg");
        assert_eq!(got, 2);
        assert_eq!(&buf, b"hi");
        // The sender is the bound client datagram socket.
        assert_eq!(addr, Some(AdvAddr::Unix(client_path_bytes.to_vec())));
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

    /// `AdvAddr` round-trips through raw `sockaddr` bytes.
    #[test]
    fn adv_addr_roundtrip() {
        let v4 = AdvAddr::Inet(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));
        let (st, len) = v4.to_sockaddr();
        assert_eq!(AdvAddr::from_sockaddr(&st.data[..len as usize]), Some(v4));
        let v6 = AdvAddr::Inet6(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)),
            443,
        ));
        let (st, len) = v6.to_sockaddr();
        assert_eq!(AdvAddr::from_sockaddr(&st.data[..len as usize]), Some(v6));
        let un = AdvAddr::Unix(b"/tmp/x.sock".to_vec());
        let (st, len) = un.to_sockaddr();
        assert_eq!(AdvAddr::from_sockaddr(&st.data[..len as usize]), Some(un));
        let unnamed = AdvAddr::Unix(Vec::new());
        let (st, len) = unnamed.to_sockaddr();
        assert_eq!(len, 2);
        assert_eq!(
            AdvAddr::from_sockaddr(&st.data[..len as usize]),
            Some(unnamed)
        );
    }

    /// `sock_key` / `raw_fd_from_key` round-trip.
    #[test]
    fn sock_key_roundtrip() {
        assert_eq!(raw_fd_from_key(sock_key(3 as RawFd)), 3 as RawFd);
        assert_eq!(sock_key(raw_fd_from_key(7)), 7);
    }
}
