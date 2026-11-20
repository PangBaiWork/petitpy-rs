//! Shared `unsafe-ffi` syscall wrappers for the os and socket
//! modules. Zero dependencies, no `libc` crate.
//!
//! Every `extern "C"` declaration lives in the private `ffi`
//! module below; the public wrappers are safe functions
//! returning `Result<_, i32>` with the errno (Unix) or the
//! Winsock error code (Windows; the caller maps it to a POSIX
//! errno with `win32_to_errno`). When the feature is OFF this
//! file contributes nothing: every item is `#[cfg(feature =
//! "unsafe-ffi")]` and every call site keeps its default-path
//! behavior byte-identical.
//!
//! Types are plain Rust ints. `c_int` / `c_uint` are `i32` /
//! `u32` on Linux x86_64, Linux aarch64, macOS and Android,
//! so the layout is identical on those arches. `lseek`'s
//! `off_t` is `i64` on all of them. On Windows a socket
//! handle (`SOCKET`) is `usize` (`u64`), so `RawFd` is
//! `usize` there and `i32` on Unix.
//!
//! `errno()` is portable: Linux `__errno_location`,
//! macOS `__error`, Android `__errno`, Windows
//! `WSAGetLastError`. The os wrappers (`umask` / `close` /
//! `fsync` / `lseek`) are Unix-only — the symbols do not
//! exist in the MSVC CRT — and their call sites are gated
//! to `all(feature = "unsafe-ffi", unix)`.
//!
//! Windows needs `WSAStartup(MAKEWORD(2,2))` before any
//! Winsock call; `wsa_ensure()` runs it once via
//! `OnceLock`. Winsock functions live in `ws2_32` (linked
//! with `#[link(name = "ws2_32")]` on the extern block).
//!
//! History: this file once also held the G6b socket/subprocess
//! wrappers (`sys_socket`, `sys_bind`, `sys_listen`,
//! `sys_getsockopt`, `sys_kill`, `sys_dup2`, plus `SockaddrUn`).
//! Phase H replaced those modules with pure-Rust `std`
//! implementations, leaving the wrappers uncalled; they were
//! deleted 2026-11-20, leaving only the four os wrappers.
//! The socket FFI lane (S1, 2026-11-20) re-adds the socket
//! syscalls here, with portable errno and platform-correct
//! constants (see `scripts/socket_ffi_brief.md` §2.1).

#[cfg(feature = "unsafe-ffi")]
use std::os::raw::{c_char, c_void};

/// A socket handle: an fd number on Unix, a `SOCKET`
/// (`UINT_PTR`) on Windows.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub type RawFd = i32;

/// A socket handle: an fd number on Unix, a `SOCKET`
/// (`UINT_PTR`) on Windows.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub type RawFd = usize;

/// `INVALID_SOCKET` (Windows only): what `socket(2)` and
/// `accept(2)` return on failure.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const INVALID_SOCKET: RawFd = usize::MAX;

/// The raw declarations (private mod).
#[cfg(feature = "unsafe-ffi")]
mod ffi {
    use std::os::raw::{c_int, c_uint, c_void};

    use super::{Msghdr, Pollfd, Sockaddr};

    #[cfg(windows)]
    use super::{FdSet, Timeval, WsaMsg};

    /// `socklen_t` on Unix, `int` on Windows (all Winsock
    /// length parameters are `int`).
    #[cfg(unix)]
    pub type Socklen = u32;
    /// `socklen_t` on Unix, `int` on Windows (all Winsock
    /// length parameters are `int`).
    #[cfg(windows)]
    pub type Socklen = c_int;

    // FFI invariant: glibc's `__errno_location` returns a
    // pointer to the calling thread's errno cell; it is
    // never null, and the cell is only read (never written)
    // by the caller.
    #[cfg(target_os = "linux")]
    extern "C" {
        pub fn __errno_location() -> *mut c_int;
    }

    // FFI invariant: macOS's `__error()` is the same
    // contract as glibc's `__errno_location` (a per-thread
    // errno cell, never null, read-only here).
    #[cfg(target_os = "macos")]
    extern "C" {
        pub fn __error() -> *mut c_int;
    }

    // FFI invariant: bionic's `__errno()` is the same
    // contract as glibc's `__errno_location` (a per-thread
    // errno cell, never null, read-only here).
    #[cfg(target_os = "android")]
    extern "C" {
        pub fn __errno() -> *mut c_int;
    }

    // FFI invariant: `umask(2)` takes a mode_t (`c_uint`)
    // and returns the previous mask; it cannot fail. The
    // argument is the caller's responsibility (the kernel
    // masks it to 0777).
    //
    // Unix only for the plain names — on Windows the CRT
    // symbols are underscore-prefixed (`_umask` etc., see
    // the Windows block below).
    #[cfg(unix)]
    extern "C" {
        pub fn umask(mask: c_uint) -> c_uint;
    }

    // FFI invariant: `close(2)` takes any `c_int` fd
    // number; the kernel fails EBADF on a bad fd, it cannot
    // fault. On -1 the thread errno cell holds the failure.
    #[cfg(unix)]
    extern "C" {
        pub fn close(fd: c_int) -> c_int;
    }

    // FFI invariant: `fsync(2)` / `lseek(2)` take an fd
    // number and plain integers; on -1 the thread errno
    // cell holds the failure. A bad fd fails EBADF, a bad
    // whence fails EINVAL — neither can fault.
    #[cfg(unix)]
    extern "C" {
        pub fn fsync(fd: c_int) -> c_int;
        pub fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    }

    // FFI invariant (Windows CRT, RustPython `crt_fd` /
    // `host_env::nt` pattern): on Windows the fds are CRT
    // fds (`c_int`, the same `RawFd` Rust std uses), and
    // the CRT functions are the underscore-prefixed names:
    // `_close` for `close`, `_commit` for `fsync`
    // (RustPython aliases `libc::commit as fsync`), `_umask`
    // for `umask`. `_lseeki64` is the 64-bit `lseek`
    // (plain `_lseek` returns a 32-bit `long`); RustPython
    // instead uses `SetFilePointerEx` via `_get_osfhandle`,
    // but `_lseeki64` is the direct CRT equivalent with the
    // same observable behavior — including ESPIPE on pipes,
    // which `SetFilePointerEx` would report as
    // ERROR_INVALID_HANDLE (EBADF), the wrong errno here.
    // `_get_errno` reads the calling thread's CRT errno
    // cell (RustPython `host_env::os::get_errno`);
    // `WSAGetLastError` is Winsock-only and does NOT see
    // CRT failures. MSVC CRT errno values are the POSIX
    // numbers (EBADF 9, EINVAL 22, ESPIPE 29), so no
    // mapping is needed.
    #[cfg(windows)]
    extern "C" {
        pub fn _umask(mask: c_int) -> c_int;
        pub fn _close(fd: c_int) -> c_int;
        pub fn _commit(fd: c_int) -> c_int;
        pub fn _lseeki64(fd: c_int, offset: i64, whence: c_int) -> i64;
        pub fn _get_errno(p_value: *mut c_int) -> c_int;
    }

    // FFI invariant (Unix socket calls): every function
    // takes an fd number and plain integers / pointers to
    // caller-owned buffers (`sockaddr`, byte slices,
    // `msghdr`). A bad fd fails EBADF, a bad address fails
    // EFAULT — neither can fault on a valid pointer, and
    // the wrappers only pass pointers to live Rust values
    // (`sockaddr_*` structs, `&[u8]` / `&mut [u8]`
    // slices). On -1 the thread errno cell holds the
    // failure; `send` / `recv` return `isize` (-1 on
    // failure).
    #[cfg(unix)]
    extern "C" {
        pub fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        pub fn bind(fd: c_int, addr: *const Sockaddr, len: Socklen) -> c_int;
        pub fn listen(fd: c_int, backlog: c_int) -> c_int;
        pub fn accept(fd: c_int, addr: *mut Sockaddr, len: *mut Socklen) -> c_int;
        pub fn connect(fd: c_int, addr: *const Sockaddr, len: Socklen) -> c_int;
        pub fn send(fd: c_int, buf: *const u8, len: usize, flags: c_int) -> isize;
        pub fn recv(fd: c_int, buf: *mut u8, len: usize, flags: c_int) -> isize;
        pub fn sendto(
            fd: c_int,
            buf: *const u8,
            len: usize,
            flags: c_int,
            addr: *const Sockaddr,
            addrlen: Socklen,
        ) -> isize;
        pub fn recvfrom(
            fd: c_int,
            buf: *mut u8,
            len: usize,
            flags: c_int,
            addr: *mut Sockaddr,
            addrlen: *mut Socklen,
        ) -> isize;
        pub fn sendmsg(fd: c_int, msg: *const Msghdr, flags: c_int) -> isize;
        pub fn recvmsg(fd: c_int, msg: *mut Msghdr, flags: c_int) -> isize;
        pub fn shutdown(fd: c_int, how: c_int) -> c_int;
        pub fn getsockname(fd: c_int, addr: *mut Sockaddr, len: *mut Socklen) -> c_int;
        pub fn getpeername(fd: c_int, addr: *mut Sockaddr, len: *mut Socklen) -> c_int;
        pub fn setsockopt(
            fd: c_int,
            level: c_int,
            opt: c_int,
            val: *const c_void,
            len: Socklen,
        ) -> c_int;
        pub fn getsockopt(
            fd: c_int,
            level: c_int,
            opt: c_int,
            val: *mut c_void,
            len: *mut Socklen,
        ) -> c_int;
        pub fn dup(fd: c_int) -> c_int;
        pub fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int;
        pub fn socketpair(
            domain: c_int,
            ty: c_int,
            protocol: c_int,
            fds: *mut [c_int; 2],
        ) -> c_int;
        pub fn poll(fds: *mut Pollfd, nfds: u32, timeout: c_int) -> c_int;
    }

    // FFI invariant (Winsock calls): the signatures are
    // ws2_32's. `SOCKET` is `usize`; length parameters are
    // `int` (declared as `Socklen` = `c_int`); `send` /
    // `recv` / `sendto` / `recvfrom` return `int`
    // (`SOCKET_ERROR` = -1), NOT `ssize_t` — declaring
    // them as `isize` would read garbage in the upper 32
    // bits of `rax`. `socket` / `accept` return `SOCKET`
    // (`INVALID_SOCKET` = `usize::MAX` on failure).
    // `WSAStartup` takes a version word and a
    // caller-owned `WSADATA` buffer; the wrapper passes a
    // 512-byte zeroed buffer (larger than `WSADATA`), so
    // the write cannot overflow it.
    #[cfg(windows)]
    #[link(name = "ws2_32")]
    extern "C" {
        pub fn WSAStartup(version: u16, data: *mut u8) -> c_int;
        pub fn WSAGetLastError() -> c_int;
        pub fn socket(af: c_int, ty: c_int, protocol: c_int) -> usize;
        pub fn bind(s: usize, addr: *const Sockaddr, len: Socklen) -> c_int;
        pub fn listen(s: usize, backlog: c_int) -> c_int;
        pub fn accept(s: usize, addr: *mut Sockaddr, len: *mut Socklen) -> usize;
        pub fn connect(s: usize, addr: *const Sockaddr, len: Socklen) -> c_int;
        pub fn send(s: usize, buf: *const u8, len: c_int, flags: c_int) -> c_int;
        pub fn recv(s: usize, buf: *mut u8, len: c_int, flags: c_int) -> c_int;
        pub fn sendto(
            s: usize,
            buf: *const u8,
            len: c_int,
            flags: c_int,
            addr: *const Sockaddr,
            addrlen: Socklen,
        ) -> c_int;
        pub fn recvfrom(
            s: usize,
            buf: *mut u8,
            len: c_int,
            flags: c_int,
            addr: *mut Sockaddr,
            addrlen: *mut Socklen,
        ) -> c_int;
        pub fn shutdown(s: usize, how: c_int) -> c_int;
        pub fn getsockname(s: usize, addr: *mut Sockaddr, len: *mut Socklen) -> c_int;
        pub fn getpeername(s: usize, addr: *mut Sockaddr, len: *mut Socklen) -> c_int;
        pub fn setsockopt(
            s: usize,
            level: c_int,
            opt: c_int,
            val: *const c_void,
            len: Socklen,
        ) -> c_int;
        pub fn getsockopt(
            s: usize,
            level: c_int,
            opt: c_int,
            val: *mut c_void,
            len: *mut Socklen,
        ) -> c_int;
        pub fn closesocket(s: usize) -> c_int;
        pub fn ioctlsocket(s: usize, cmd: c_int, argp: *mut u32) -> c_int;
        pub fn WSAPoll(fds: *mut Pollfd, nfds: u32, timeout: c_int) -> c_int;
        // FFI invariant: `WSASendMsg` / `WSARecvMsg` take a
        // socket, a caller-owned `WSAMSG` (whose buffers
        // point to caller-owned slices) and a pointer to a
        // caller-owned `DWORD` for the byte count; the last
        // two parameters are always null here (no
        // overlapped I/O). `SOCKET_ERROR` (-1) on failure,
        // 0 on success.
        pub fn WSASendMsg(
            s: usize,
            msg: *const WsaMsg,
            flags: u32,
            sent: *mut u32,
            overlapped: *mut c_void,
            completion: *mut c_void,
        ) -> c_int;
        pub fn WSARecvMsg(
            s: usize,
            msg: *mut WsaMsg,
            received: *mut u32,
            overlapped: *mut c_void,
            completion: *mut c_void,
        ) -> c_int;
        // FFI invariant: `select` takes a count (ignored on
        // Windows — the sets carry their own counts) and
        // pointers to caller-owned `fd_set`s (null = no set
        // for that direction) plus a caller-owned `timeval`
        // (null = block forever). Returns the number of
        // ready sockets, 0 on timeout, `SOCKET_ERROR` on
        // failure.
        pub fn select(
            nfds: c_int,
            readfds: *mut FdSet,
            writefds: *mut FdSet,
            exceptfds: *mut FdSet,
            timeout: *const Timeval,
        ) -> c_int;
        pub fn WSADuplicateSocket(s: usize, pid: u32, info: *mut u8) -> c_int;
        pub fn WSASocket(
            af: c_int,
            ty: c_int,
            protocol: c_int,
            info: *mut u8,
            group: u32,
            flags: u32,
        ) -> usize;
    }

    // FFI invariant: `GetCurrentProcessId` takes no
    // argument and cannot fail; it is only used to
    // duplicate a socket into the current process
    // (`WSADuplicateSocket`).
    #[cfg(windows)]
    #[link(name = "kernel32")]
    extern "C" {
        pub fn GetCurrentProcessId() -> u32;
    }
}

/// Read the calling thread's errno cell (Unix) or the last
/// Winsock error (Windows). Callers read it immediately
/// after a failed call, before any other call can touch
/// the cell.
#[cfg(feature = "unsafe-ffi")]
pub(crate) fn errno() -> i32 {
    #[cfg(target_os = "linux")]
    {
        // FFI invariant: `__errno_location` never returns
        // null (see above) and the cell is only read here,
        // never written.
        unsafe { *ffi::__errno_location() }
    }
    #[cfg(target_os = "macos")]
    {
        // FFI invariant: `__error()` never returns null
        // (see above) and the cell is only read here, never
        // written.
        unsafe { *ffi::__error() }
    }
    #[cfg(target_os = "android")]
    {
        // FFI invariant: `__errno()` never returns null
        // (see above) and the cell is only read here, never
        // written.
        unsafe { *ffi::__errno() }
    }
    #[cfg(windows)]
    {
        // FFI invariant: `WSAGetLastError` returns the last
        // Winsock error for this thread; it is only read
        // immediately after a failed Winsock call.
        unsafe { ffi::WSAGetLastError() }
    }
}

/// Read the calling thread's CRT errno cell (Windows
/// only). The CRT functions (`_close` / `_commit` /
/// `_umask` / `_lseeki64`) set CRT errno, NOT the Winsock
/// error — `errno()` above would read the wrong cell.
/// RustPython `host_env::os::get_errno` pattern: `_get_errno`
/// fills the caller's cell. The value is already a POSIX
/// errno number, no mapping needed.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub(crate) fn crt_errno() -> i32 {
    // FFI invariant: `_get_errno` writes one `c_int`
    // through the caller's pointer; it cannot fail in a
    // way that matters here (it returns 0 on success).
    let mut value: i32 = 0;
    unsafe {
        ffi::_get_errno(&mut value);
    }
    value
}

/// Run `WSAStartup(MAKEWORD(2,2))` once (Windows only).
/// Every Windows wrapper calls this first; on Unix it does
/// not exist (no startup is needed).
#[cfg(all(feature = "unsafe-ffi", windows))]
pub(crate) fn wsa_ensure() -> Result<(), i32> {
    use std::sync::OnceLock;
    static WSA: OnceLock<Result<(), i32>> = OnceLock::new();
    *WSA.get_or_init(|| {
        // FFI invariant: `WSAStartup` takes a version word
        // and a caller-owned `WSADATA` buffer; the 512-byte
        // zeroed buffer is larger than `WSADATA`, so the
        // write cannot overflow it. The return value IS the
        // error code (not `WSAGetLastError`).
        let mut data = [0u8; 512];
        let r = unsafe { ffi::WSAStartup(0x0202, data.as_mut_ptr()) };
        if r == 0 {
            Ok(())
        } else {
            Err(r)
        }
    })
}

// ── `sockaddr` structs ──────────────────────────────────────────────
// `#[repr(C)]`, platform layout. macOS inserts a `sa_len` /
// `sin_len` / `sin6_len` / `sun_len` byte before the family,
// so every struct has a macOS variant. The constructors
// below hide the layout: callers pass semantic values.

/// Generic socket address (16 bytes). Only used as a cast
/// target for `sockaddr_in` / `sockaddr_in6` /
/// `sockaddr_un` / `sockaddr_storage`.
#[cfg(feature = "unsafe-ffi")]
#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
pub struct Sockaddr {
    pub sa_family: u16,
    pub sa_data: [u8; 14],
}

/// Generic socket address (16 bytes). Only used as a cast
/// target for `sockaddr_in` / `sockaddr_in6` /
/// `sockaddr_un` / `sockaddr_storage`.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
#[repr(C)]
pub struct Sockaddr {
    pub sa_len: u8,
    pub sa_family: u8,
    pub sa_data: [u8; 14],
}

/// Generic socket address (16 bytes). Only used as a cast
/// target for `sockaddr_in` / `sockaddr_in6` /
/// `sockaddr_storage` (Windows has no `sockaddr_un` path
/// in S1).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct Sockaddr {
    pub sa_family: u16,
    pub sa_data: [u8; 14],
}

/// `sockaddr_in` (16 bytes). `sin_port` is network byte
/// order; `sin_addr` is 4 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

/// `sockaddr_in` (16 bytes). `sin_port` is network byte
/// order; `sin_addr` is 4 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn {
    pub sin_len: u8,
    pub sin_family: u8,
    pub sin_port: u16,
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

/// `sockaddr_in` (16 bytes). `sin_port` is network byte
/// order; `sin_addr` is 4 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

#[cfg(feature = "unsafe-ffi")]
impl SockaddrIn {
    /// Build an `AF_INET` address. `port` and `addr` are
    /// already in network byte order (`port.to_be()`,
    /// octets).
    pub fn new(port_be: u16, addr: [u8; 4]) -> Self {
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                sin_family: AF_INET as u16,
                sin_port: port_be,
                sin_addr: addr,
                sin_zero: [0; 8],
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self {
                sin_len: 16,
                sin_family: AF_INET as u8,
                sin_port: port_be,
                sin_addr: addr,
                sin_zero: [0; 8],
            }
        }
    }
}

/// `sockaddr_in6` (28 bytes). `sin6_port` is network byte
/// order; `sin6_addr` is 16 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn6 {
    pub sin6_family: u16,
    pub sin6_port: u16,
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

/// `sockaddr_in6` (28 bytes). `sin6_port` is network byte
/// order; `sin6_addr` is 16 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn6 {
    pub sin6_len: u8,
    pub sin6_family: u8,
    pub sin6_port: u16,
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

/// `sockaddr_in6` (28 bytes). `sin6_port` is network byte
/// order; `sin6_addr` is 16 bytes in network order.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn6 {
    pub sin6_family: u16,
    pub sin6_port: u16,
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

#[cfg(feature = "unsafe-ffi")]
impl SockaddrIn6 {
    /// Build an `AF_INET6` address. `port` is already in
    /// network byte order; `addr` is 16 bytes in network
    /// order.
    pub fn new(port_be: u16, flowinfo: u32, addr: [u8; 16], scope_id: u32) -> Self {
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                sin6_family: AF_INET6 as u16,
                sin6_port: port_be,
                sin6_flowinfo: flowinfo,
                sin6_addr: addr,
                sin6_scope_id: scope_id,
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self {
                sin6_len: 28,
                sin6_family: AF_INET6 as u8,
                sin6_port: port_be,
                sin6_flowinfo: flowinfo,
                sin6_addr: addr,
                sin6_scope_id: scope_id,
            }
        }
    }
}

/// `sockaddr_un` (Unix only). Linux: 110 bytes (family +
/// 108-byte path); macOS: 106 bytes (len + family +
/// 104-byte path).
#[cfg(feature = "unsafe-ffi")]
#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

/// `sockaddr_un` (Unix only). Linux: 110 bytes (family +
/// 108-byte path); macOS: 106 bytes (len + family +
/// 104-byte path).
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrUn {
    pub sun_len: u8,
    pub sun_family: u8,
    pub sun_path: [u8; 104],
}

#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
impl SockaddrUn {
    /// Build an `AF_UNIX` address from a path (already
    /// truncated to the `sun_path` limit by the caller;
    /// longer paths are silently truncated here — the
    /// caller validates the length first).
    pub fn new(path: &[u8]) -> Self {
        #[cfg(not(target_os = "macos"))]
        {
            let mut sun_path = [0u8; 108];
            let n = path.len().min(108);
            sun_path[..n].copy_from_slice(&path[..n]);
            Self {
                sun_family: AF_UNIX as u16,
                sun_path,
            }
        }
        #[cfg(target_os = "macos")]
        {
            let mut sun_path = [0u8; 104];
            let n = path.len().min(104);
            sun_path[..n].copy_from_slice(&path[..n]);
            Self {
                sun_len: (2 + n) as u8,
                sun_family: AF_UNIX as u8,
                sun_path,
            }
        }
    }

    /// The `sockaddr_un` length for `bind` / `connect` /
    /// `sendto`: header + path, plus a NUL terminator for
    /// filesystem paths. Abstract addresses (leading NUL)
    /// are NOT NUL-terminated — the name is exactly the
    /// bytes, so no `+ 1` (an extra byte would be a
    /// different abstract address).
    pub fn len_for(path: &[u8]) -> u32 {
        if path.first() == Some(&0) {
            (2 + path.len()) as u32
        } else {
            (2 + path.len() + 1) as u32
        }
    }
}

/// `sockaddr_storage` (128 bytes, 8-byte aligned): the
/// receive buffer for `accept` / `getsockname` /
/// `getpeername` / `recvfrom`. Big enough for every
/// `sockaddr_*` on every platform.
#[cfg(feature = "unsafe-ffi")]
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct SockaddrStorage {
    pub _inner: [u8; 128],
}

#[cfg(feature = "unsafe-ffi")]
impl SockaddrStorage {
    pub fn new() -> Self {
        Self { _inner: [0; 128] }
    }

    /// The address family of the stored `sockaddr`: on
    /// macOS the family byte sits at offset 1 (after
    /// `sa_len`); everywhere else it is the `u16` at
    /// offset 0.
    pub fn family(&self) -> u16 {
        #[cfg(target_os = "macos")]
        {
            self._inner[1] as u16
        }
        #[cfg(not(target_os = "macos"))]
        {
            u16::from_ne_bytes([self._inner[0], self._inner[1]])
        }
    }
}

/// `addrinfo` (S2's `getaddrinfo` input/output). The field
/// order differs per platform: macOS puts `ai_canonname`
/// before `ai_addr`; Windows' `ai_addrlen` is `size_t`.
#[cfg(feature = "unsafe-ffi")]
#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
pub struct Addrinfo {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: u32,
    pub ai_addr: *mut Sockaddr,
    pub ai_canonname: *mut c_char,
    pub ai_next: *mut Addrinfo,
}

/// `addrinfo` (S2's `getaddrinfo` input/output). The field
/// order differs per platform: macOS puts `ai_canonname`
/// before `ai_addr`; Windows' `ai_addrlen` is `size_t`.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
#[repr(C)]
pub struct Addrinfo {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: u32,
    pub ai_canonname: *mut c_char,
    pub ai_addr: *mut Sockaddr,
    pub ai_next: *mut Addrinfo,
}

/// `addrinfo` (S2's `getaddrinfo` input/output). The field
/// order differs per platform: macOS puts `ai_canonname`
/// before `ai_addr`; Windows' `ai_addrlen` is `size_t`.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct Addrinfo {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: usize,
    pub ai_canonname: *mut c_char,
    pub ai_addr: *mut Sockaddr,
    pub ai_next: *mut Addrinfo,
}

/// `iovec` (S3's `sendmsg` / `recvmsg`, Unix only).
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
#[repr(C)]
pub struct Iovec {
    pub iov_base: *mut c_void,
    pub iov_len: usize,
}

/// `msghdr` (S3's `sendmsg` / `recvmsg`, Unix only).
/// Same layout on Linux, macOS and Android.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
#[repr(C)]
pub struct Msghdr {
    pub msg_name: *mut c_void,
    pub msg_namelen: u32,
    pub msg_iov: *mut Iovec,
    pub msg_iovlen: usize,
    pub msg_control: *mut c_void,
    pub msg_controllen: usize,
    pub msg_flags: i32,
}

/// `pollfd` for `poll(2)` / `WSAPoll`. Unix: `fd` is
/// `i32`; Windows: `fd` is `SOCKET` (`usize`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
#[repr(C)]
pub struct Pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// `pollfd` for `poll(2)` / `WSAPoll`. Unix: `fd` is
/// `i32`; Windows: `fd` is `SOCKET` (`usize`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct Pollfd {
    pub fd: usize,
    pub events: i16,
    pub revents: i16,
}

/// `WSABUF` (S3's `WSASendMsg` / `WSARecvMsg`, Windows
/// only): a length + pointer pair.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct WsaBuf {
    pub len: u32,
    pub buf: *mut u8,
}

/// `WSAMSG` (S3's `WSASendMsg` / `WSARecvMsg`, Windows
/// only). `name` is `NULL` when no address is given
/// (connected send / receive without the peer); `control`
/// is always the empty buffer (no ancillary data).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct WsaMsg {
    pub name: *mut Sockaddr,
    pub namelen: i32,
    pub buffers: *mut WsaBuf,
    pub buffer_count: u32,
    pub control: WsaBuf,
    pub flags: u32,
}

/// `fd_set` (S3's `select`, Windows only): a count plus
/// room for 64 sockets (`FD_SETSIZE`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct FdSet {
    pub count: u32,
    pub sockets: [usize; 64],
}

/// `timeval` (S3's `select`, Windows only).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
#[repr(C)]
pub struct Timeval {
    pub sec: i32,
    pub usec: i32,
}

// ── Platform-correct constants ──────────────────────────────────────
// Kernel values, NOT the Linux values hardcoded in
// `modsocket.rs` (the Python-facing module constants stay
// Linux-numbered; the FFI boundary translates — see
// `modsocket_ffi.rs::translate_family` /
// `translate_sockopt`). `AF_INET` / `AF_UNIX` /
// `SOCK_STREAM` / `SOCK_DGRAM` / `IPPROTO_*` agree on every
// platform; the rest differ.

/// Address families.
#[cfg(feature = "unsafe-ffi")]
pub const AF_UNSPEC: i32 = 0;
/// Address families.
#[cfg(feature = "unsafe-ffi")]
pub const AF_UNIX: i32 = 1;
/// Address families.
#[cfg(feature = "unsafe-ffi")]
pub const AF_INET: i32 = 2;
/// Address families: Linux/Android 10, macOS 30, Windows 23.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const AF_INET6: i32 = 10;
/// Address families: Linux/Android 10, macOS 30, Windows 23.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const AF_INET6: i32 = 30;
/// Address families: Linux/Android 10, macOS 30, Windows 23.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const AF_INET6: i32 = 23;

/// Socket types.
#[cfg(feature = "unsafe-ffi")]
pub const SOCK_STREAM: i32 = 1;
/// Socket types.
#[cfg(feature = "unsafe-ffi")]
pub const SOCK_DGRAM: i32 = 2;
/// Socket types.
#[cfg(feature = "unsafe-ffi")]
pub const SOCK_RAW: i32 = 3;

/// Protocols.
#[cfg(feature = "unsafe-ffi")]
pub const IPPROTO_IP: i32 = 0;
/// Protocols.
#[cfg(feature = "unsafe-ffi")]
pub const IPPROTO_TCP: i32 = 6;
/// Protocols.
#[cfg(feature = "unsafe-ffi")]
pub const IPPROTO_UDP: i32 = 17;

/// `SOL_SOCKET`: Linux/Android 1, macOS/Windows 0xffff.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SOL_SOCKET: i32 = 1;
/// `SOL_SOCKET`: Linux/Android 1, macOS/Windows 0xffff.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SOL_SOCKET: i32 = 0xffff;

/// `SO_REUSEADDR`: Linux/Android 2, macOS/Windows 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_REUSEADDR: i32 = 2;
/// `SO_REUSEADDR`: Linux/Android 2, macOS/Windows 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_REUSEADDR: i32 = 4;

/// `SO_BROADCAST`: Linux/Android 6, macOS/Windows 32.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_BROADCAST: i32 = 6;
/// `SO_BROADCAST`: Linux/Android 6, macOS/Windows 32.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_BROADCAST: i32 = 32;

/// `SO_SNDBUF`: Linux/Android 7, macOS/Windows 0x1001.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_SNDBUF: i32 = 7;
/// `SO_SNDBUF`: Linux/Android 7, macOS/Windows 0x1001.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_SNDBUF: i32 = 0x1001;

/// `SO_RCVBUF`: Linux/Android 8, macOS/Windows 0x1002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_RCVBUF: i32 = 8;
/// `SO_RCVBUF`: Linux/Android 8, macOS/Windows 0x1002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_RCVBUF: i32 = 0x1002;

/// `SO_RCVTIMEO`: Linux/Android 20, macOS/Windows 0x1006.
/// Unix takes a `timeval`; Windows takes a DWORD of
/// milliseconds (see `modsocket_ffi.rs`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_RCVTIMEO: i32 = 20;
/// `SO_RCVTIMEO`: Linux/Android 20, macOS/Windows 0x1006.
/// Unix takes a `timeval`; Windows takes a DWORD of
/// milliseconds (see `modsocket_ffi.rs`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_RCVTIMEO: i32 = 0x1006;

/// `SO_SNDTIMEO`: Linux/Android 21, macOS/Windows 0x1005.
/// Unix takes a `timeval`; Windows takes a DWORD of
/// milliseconds (see `modsocket_ffi.rs`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_SNDTIMEO: i32 = 21;
/// `SO_SNDTIMEO`: Linux/Android 21, macOS/Windows 0x1005.
/// Unix takes a `timeval`; Windows takes a DWORD of
/// milliseconds (see `modsocket_ffi.rs`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_SNDTIMEO: i32 = 0x1005;

/// `SO_REUSEPORT`: Linux/Android/macOS 15. Winsock has no
/// equivalent (Unix only).
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const SO_REUSEPORT: i32 = 15;

/// `SO_KEEPALIVE`: Linux/Android 9, macOS/Windows 8.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_KEEPALIVE: i32 = 9;
/// `SO_KEEPALIVE`: Linux/Android 9, macOS/Windows 8.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_KEEPALIVE: i32 = 8;

/// `SO_ERROR`: Linux/Android 4, macOS/Windows 0x1007
/// (used by `connect_timeout`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SO_ERROR: i32 = 4;
/// `SO_ERROR`: Linux/Android 4, macOS/Windows 0x1007
/// (used by `connect_timeout`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const SO_ERROR: i32 = 0x1007;

/// `TCP_NODELAY`: 1 on every platform.
#[cfg(feature = "unsafe-ffi")]
pub const TCP_NODELAY: i32 = 1;

/// `IP_TTL`: Linux/Android 2, macOS/Windows 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const IP_TTL: i32 = 2;
/// `IP_TTL`: Linux/Android 2, macOS/Windows 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "macos", windows))]
pub const IP_TTL: i32 = 4;

/// `shutdown()` directions: 0/1/2 on every platform.
#[cfg(feature = "unsafe-ffi")]
pub const SHUT_RD: i32 = 0;
/// `shutdown()` directions: 0/1/2 on every platform.
#[cfg(feature = "unsafe-ffi")]
pub const SHUT_WR: i32 = 1;
/// `shutdown()` directions: 0/1/2 on every platform.
#[cfg(feature = "unsafe-ffi")]
pub const SHUT_RDWR: i32 = 2;

/// `MSG_OOB` / `MSG_PEEK` / `MSG_DONTROUTE`: 1/2/4 on
/// every platform.
#[cfg(feature = "unsafe-ffi")]
pub const MSG_OOB: i32 = 1;
/// `MSG_OOB` / `MSG_PEEK` / `MSG_DONTROUTE`: 1/2/4 on
/// every platform.
#[cfg(feature = "unsafe-ffi")]
pub const MSG_PEEK: i32 = 2;
/// `MSG_OOB` / `MSG_PEEK` / `MSG_DONTROUTE`: 1/2/4 on
/// every platform.
#[cfg(feature = "unsafe-ffi")]
pub const MSG_DONTROUTE: i32 = 4;

/// `MSG_DONTWAIT`: Linux/Android 64, macOS 0x80. Winsock
/// has no equivalent (use `FIONBIO`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MSG_DONTWAIT: i32 = 64;
/// `MSG_DONTWAIT`: Linux/Android 64, macOS 0x80. Winsock
/// has no equivalent (use `FIONBIO`).
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const MSG_DONTWAIT: i32 = 0x80;

/// `MSG_WAITALL`: Linux/Android 256, macOS 0x40, Windows 8.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MSG_WAITALL: i32 = 256;
/// `MSG_WAITALL`: Linux/Android 256, macOS 0x40, Windows 8.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const MSG_WAITALL: i32 = 0x40;
/// `MSG_WAITALL`: Linux/Android 256, macOS 0x40, Windows 8.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const MSG_WAITALL: i32 = 8;

/// `O_NONBLOCK` (Unix): Linux/Android 2048, macOS 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const O_NONBLOCK: i32 = 2048;
/// `O_NONBLOCK` (Unix): Linux/Android 2048, macOS 4.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const O_NONBLOCK: i32 = 4;

/// `F_GETFL` / `F_SETFL` (Unix): 3/4 on every platform.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const F_GETFL: i32 = 3;
/// `F_GETFL` / `F_SETFL` (Unix): 3/4 on every platform.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const F_SETFL: i32 = 4;

/// `FIONBIO` for `ioctlsocket` (Windows only).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const FIONBIO: i32 = 0x8004667e_u32 as i32;

/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const EINTR: i32 = 4;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const EAGAIN: i32 = 11;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const EWOULDBLOCK: i32 = 11;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const EINPROGRESS: i32 = 115;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const EINTR: i32 = 4;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const EAGAIN: i32 = 35;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const EWOULDBLOCK: i32 = 35;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const EINPROGRESS: i32 = 36;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const EINTR: i32 = 10004;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const EAGAIN: i32 = 10035;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const EWOULDBLOCK: i32 = 10035;
/// Errnos (kernel values): Linux/Android `EINTR` 4,
/// `EAGAIN`/`EWOULDBLOCK` 11, `EINPROGRESS` 115;
/// macOS `EINTR` 4, `EAGAIN`/`EWOULDBLOCK` 35,
/// `EINPROGRESS` 36; Windows `WSAEINTR` 10004,
/// `WSAEWOULDBLOCK` 10035, `WSAEINPROGRESS` 10036.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const EINPROGRESS: i32 = 10036;

/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const POLLIN: i16 = 1;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const POLLOUT: i16 = 4;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const POLLERR: i16 = 8;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const POLLHUP: i16 = 16;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const POLLIN: i16 = 0x0100;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const POLLOUT: i16 = 0x0010;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const POLLERR: i16 = 0x0001;
/// `poll(2)` / `WSAPoll` events. Unix: `POLLIN` 1,
/// `POLLOUT` 4, `POLLERR` 8, `POLLHUP` 16. Windows:
/// `POLLRDNORM` 0x0100, `POLLWRNORM` 0x0010,
/// `POLLERR` 0x0001, `POLLHUP` 0x0002.
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const POLLHUP: i16 = 0x0002;

/// `EINVAL` (kernel value): Linux/macOS 22, Windows
/// `WSAEINVAL` 10022. Used for errors the FFI layer
/// constructs itself (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(unix)]
pub const EINVAL: i32 = 22;
/// `EINVAL` (kernel value): Linux/macOS 22, Windows
/// `WSAEINVAL` 10022. Used for errors the FFI layer
/// constructs itself (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const EINVAL: i32 = 10022;

/// `ETIMEDOUT` (kernel value): Linux 110, macOS 60,
/// Windows `WSAETIMEDOUT` 10060. Used by
/// `connect_timeout` (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "linux")]
pub const ETIMEDOUT: i32 = 110;
/// `ETIMEDOUT` (kernel value): Linux 110, macOS 60,
/// Windows `WSAETIMEDOUT` 10060. Used by
/// `connect_timeout` (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "macos")]
pub const ETIMEDOUT: i32 = 60;
/// `ETIMEDOUT` (kernel value): Linux 110, macOS 60,
/// Windows `WSAETIMEDOUT` 10060. Used by
/// `connect_timeout` (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(target_os = "android")]
pub const ETIMEDOUT: i32 = 110;
/// `ETIMEDOUT` (kernel value): Linux 110, macOS 60,
/// Windows `WSAETIMEDOUT` 10060. Used by
/// `connect_timeout` (not from the kernel).
#[cfg(feature = "unsafe-ffi")]
#[cfg(windows)]
pub const ETIMEDOUT: i32 = 10060;

// ── os wrappers (Unix only) ─────────────────────────────────────────

/// `umask(2)` — set the process file-creation mask, return
/// the previous mask (`modos.zig:382-388`). Infallible: the
/// kernel masks the argument to 0777 and always succeeds.
/// Unix variant (plain `umask` symbol); the Windows
/// variant below uses the CRT `_umask`.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_umask(mask: u32) -> u32 {
    // FFI invariant: `umask` cannot fail and takes any u32
    // (the kernel masks to 0777); no errno read is needed.
    unsafe { ffi::umask(mask) }
}

/// `umask` on Windows — the MSVC CRT `_umask`
/// (RustPython `host_env::nt::umask`). Infallible like the
/// Unix call: the CRT masks the argument to 0777 and
/// always succeeds, returning the previous mask.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_umask(mask: u32) -> u32 {
    // FFI invariant: `_umask` cannot fail and takes any
    // `c_int` (the CRT masks to 0777); no errno read is
    // needed.
    unsafe { ffi::_umask(mask as i32) as u32 }
}

/// `close(2)` (`modos.zig:308-313`). No EINTR retry (the
/// original has none). Unix variant (plain `close`
/// symbol); the Windows variant below uses the CRT
/// `_close` — NOT `closesocket`, which is only for
/// SOCKET handles (see `sys_closesocket`).
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_close(fd: i32) -> Result<(), i32> {
    // FFI invariant: `close` takes any i32 fd number; the
    // kernel fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::close(fd) };
    if r < 0 {
        // FFI invariant: errno is read immediately after
        // the failed call, before any other call can touch
        // this thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `close` on Windows — the MSVC CRT `_close`
/// (RustPython `crt_fd::_close`). Same contract as the
/// Unix variant: any i32 CRT fd number, EBADF on a bad
/// fd. The error comes from the CRT errno cell
/// (`crt_errno`), not `WSAGetLastError`.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_close(fd: i32) -> Result<(), i32> {
    // FFI invariant: `_close` takes any i32 fd number;
    // the CRT fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::_close(fd) };
    if r < 0 {
        // FFI invariant: CRT errno is read immediately
        // after the failed call, before any other call can
        // touch this thread's CRT errno cell.
        Err(crt_errno())
    } else {
        Ok(())
    }
}

/// `fsync(2)` (`modos.zig:352-356`). No EINTR retry (the
/// original has none). Unix variant (plain `fsync`
/// symbol); the Windows variant below uses the CRT
/// `_commit`.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_fsync(fd: i32) -> Result<(), i32> {
    // FFI invariant: `fsync` takes any i32 fd number; the
    // kernel fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::fsync(fd) };
    if r < 0 {
        // FFI invariant: errno is read immediately after
        // the failed call, before any other call can touch
        // this thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `fsync` on Windows — the MSVC CRT `_commit`
/// (RustPython `crt_fd::fsync`, aliased there as
/// `libc::commit`). Same contract as the Unix variant:
/// any i32 CRT fd number, EBADF on a bad fd. The error
/// comes from the CRT errno cell (`crt_errno`).
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_fsync(fd: i32) -> Result<(), i32> {
    // FFI invariant: `_commit` takes any i32 fd number;
    // the CRT fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::_commit(fd) };
    if r < 0 {
        // FFI invariant: CRT errno is read immediately
        // after the failed call, before any other call can
        // touch this thread's CRT errno cell.
        Err(crt_errno())
    } else {
        Ok(())
    }
}

/// `lseek(2)` (`modos.zig:345-350`). No EINTR retry (the
/// original has none). The kernel is the authority on a bad
/// whence / negative offset — both fail EINVAL there.
/// Unix variant (plain `lseek` symbol); the Windows
/// variant below uses the CRT `_lseeki64`.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> Result<i64, i32> {
    // FFI invariant: `lseek` takes an fd number and plain
    // integers; a bad fd fails EBADF, a bad whence fails
    // EINVAL, neither can fault.
    let r = unsafe { ffi::lseek(fd, offset, whence) };
    if r < 0 {
        // FFI invariant: errno is read immediately after
        // the failed call, before any other call can touch
        // this thread's errno cell.
        Err(errno())
    } else {
        Ok(r)
    }
}

/// `lseek` on Windows — the MSVC CRT `_lseeki64`
/// (64-bit offsets; plain `_lseek` returns a 32-bit
/// `long`). Same contract as the Unix variant: a bad fd
/// fails EBADF, a bad whence fails EINVAL, a pipe fails
/// ESPIPE. RustPython instead seeks via `SetFilePointerEx`
/// on the `_get_osfhandle` HANDLE, but `_lseeki64` is the
/// direct CRT equivalent — and the better match here, since
/// `SetFilePointerEx` on a pipe reports
/// ERROR_INVALID_HANDLE (EBADF) while the tests expect
/// ESPIPE. The error comes from the CRT errno cell
/// (`crt_errno`).
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> Result<i64, i32> {
    // FFI invariant: `_lseeki64` takes an fd number and
    // plain integers; a bad fd fails EBADF, a bad whence
    // fails EINVAL, neither can fault.
    let r = unsafe { ffi::_lseeki64(fd, offset, whence) };
    if r < 0 {
        // FFI invariant: CRT errno is read immediately
        // after the failed call, before any other call can
        // touch this thread's CRT errno cell.
        Err(crt_errno())
    } else {
        Ok(r)
    }
}

// ── socket wrappers ─────────────────────────────────────────────────

/// `socket(2)`: create a socket, returning its handle
/// (`RawFd`: fd on Unix, `SOCKET` on Windows). The caller
/// passes the KERNEL family (see `modsocket_ffi.rs`'s
/// family translation — `AF_INET6` differs per platform).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_socket(domain: i32, ty: i32, proto: i32) -> Result<RawFd, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `socket` takes plain ints; on failure
    // the return is -1 (Unix) or `INVALID_SOCKET`
    // (Windows) and the errno cell holds the failure.
    #[cfg(unix)]
    {
        let r = unsafe { ffi::socket(domain, ty, proto) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }
    #[cfg(windows)]
    {
        let r = unsafe { ffi::socket(domain, ty, proto) };
        if r == INVALID_SOCKET {
            Err(errno())
        } else {
            Ok(r)
        }
    }
}

/// `bind(2)`.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_bind(fd: RawFd, addr: *const Sockaddr, len: u32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `bind` takes a handle and a pointer
    // to a caller-owned `sockaddr` of `len` bytes; a bad
    // handle fails EBADF, a bad address fails EFAULT —
    // neither can fault on a valid pointer.
    let r = unsafe { ffi::bind(fd, addr, len as ffi::Socklen) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `listen(2)`.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_listen(fd: RawFd, backlog: i32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `listen` takes a handle and a plain
    // int; a bad handle fails EBADF, it cannot fault.
    let r = unsafe { ffi::listen(fd, backlog) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `accept(2)`: returns the new handle and fills `addr` /
/// `len` with the peer address (`len` is in/out).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_accept(
    fd: RawFd,
    addr: *mut Sockaddr,
    len: *mut u32,
) -> Result<RawFd, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `accept` takes a handle and pointers
    // to a caller-owned `sockaddr` buffer and its length;
    // a bad handle fails EBADF, it cannot fault on valid
    // pointers. The return is -1 (Unix) or
    // `INVALID_SOCKET` (Windows) on failure.
    let r = unsafe { ffi::accept(fd, addr, len as *mut ffi::Socklen) };
    #[cfg(unix)]
    {
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }
    #[cfg(windows)]
    {
        if r == INVALID_SOCKET {
            Err(errno())
        } else {
            Ok(r)
        }
    }
}

/// `connect(2)`.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_connect(fd: RawFd, addr: *const Sockaddr, len: u32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `connect` takes a handle and a
    // pointer to a caller-owned `sockaddr` of `len` bytes;
    // a bad handle fails EBADF, it cannot fault on a valid
    // pointer. A nonblocking socket returns `EINPROGRESS`
    // (not an error for `connect_timeout`).
    let r = unsafe { ffi::connect(fd, addr, len as ffi::Socklen) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `send(2)`: one send, returning the bytes sent.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_send(fd: RawFd, buf: *const u8, len: usize, flags: i32) -> Result<usize, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `send` takes a handle and a pointer
    // to a caller-owned slice of `len` bytes; a bad handle
    // fails EBADF, it cannot fault on a valid pointer.
    // Windows returns `int` (cast to `isize`); Unix
    // returns `isize` directly.
    #[cfg(unix)]
    let r = unsafe { ffi::send(fd, buf, len, flags) };
    #[cfg(windows)]
    let r = unsafe { ffi::send(fd, buf, len as c_int, flags) } as isize;
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `recv(2)`: one receive, returning the bytes read (0 =
/// the peer closed, TCP).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_recv(fd: RawFd, buf: *mut u8, len: usize, flags: i32) -> Result<usize, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `recv` takes a handle and a pointer
    // to a caller-owned buffer of `len` bytes; a bad handle
    // fails EBADF, it cannot fault on a valid pointer.
    // Windows returns `int` (cast to `isize`); Unix
    // returns `isize` directly.
    #[cfg(unix)]
    let r = unsafe { ffi::recv(fd, buf, len, flags) };
    #[cfg(windows)]
    let r = unsafe { ffi::recv(fd, buf, len as c_int, flags) } as isize;
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `sendto(2)`: one datagram send, returning the bytes
/// sent.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_sendto(
    fd: RawFd,
    buf: *const u8,
    len: usize,
    flags: i32,
    addr: *const Sockaddr,
    addrlen: u32,
) -> Result<usize, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `sendto` takes a handle, a pointer to
    // a caller-owned slice of `len` bytes, and a pointer to
    // a caller-owned `sockaddr` of `addrlen` bytes; a bad
    // handle fails EBADF, it cannot fault on valid pointers.
    #[cfg(unix)]
    let r = unsafe { ffi::sendto(fd, buf, len, flags, addr, addrlen) };
    #[cfg(windows)]
    let r = unsafe { ffi::sendto(fd, buf, len as c_int, flags, addr, addrlen as ffi::Socklen) }
        as isize;
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `recvfrom(2)`: one datagram receive, returning the
/// bytes read; `addr` / `addrlen` are filled with the
/// peer address (`addrlen` is in/out).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_recvfrom(
    fd: RawFd,
    buf: *mut u8,
    len: usize,
    flags: i32,
    addr: *mut Sockaddr,
    addrlen: *mut u32,
) -> Result<usize, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `recvfrom` takes a handle, a pointer
    // to a caller-owned buffer of `len` bytes, and pointers
    // to a caller-owned `sockaddr` buffer and its length;
    // a bad handle fails EBADF, it cannot fault on valid
    // pointers.
    #[cfg(unix)]
    let r = unsafe { ffi::recvfrom(fd, buf, len, flags, addr, addrlen as *mut ffi::Socklen) };
    #[cfg(windows)]
    let r = unsafe {
        ffi::recvfrom(fd, buf, len as c_int, flags, addr, addrlen as *mut ffi::Socklen)
    } as isize;
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `sendmsg(2)` (S3 uses it; S1 only declares it).
/// Unix only — Windows uses `WSASendMsg` below.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_sendmsg(fd: RawFd, msg: *const Msghdr, flags: i32) -> Result<usize, i32> {
    // FFI invariant: `sendmsg` takes an fd and a pointer
    // to a caller-owned `msghdr` (whose `iov` entries point
    // to caller-owned slices); a bad fd fails EBADF, it
    // cannot fault on valid pointers.
    let r = unsafe { ffi::sendmsg(fd, msg, flags) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `recvmsg(2)` (S3 uses it; S1 only declares it).
/// Unix only — Windows uses `WSARecvMsg` below.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_recvmsg(fd: RawFd, msg: *mut Msghdr, flags: i32) -> Result<usize, i32> {
    // FFI invariant: `recvmsg` takes an fd and a pointer
    // to a caller-owned `msghdr` (whose `iov` entries point
    // to caller-owned buffers); a bad fd fails EBADF, it
    // cannot fault on valid pointers.
    let r = unsafe { ffi::recvmsg(fd, msg, flags) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as usize)
    }
}

/// `WSASendMsg` (S3 uses it): the Windows counterpart of
/// `sys_sendmsg`. Same return shape (bytes sent) as the
/// Unix wrapper; the `sent` out-parameter is managed here
/// so callers use one signature.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_sendmsg(fd: RawFd, msg: *const WsaMsg, flags: u32) -> Result<usize, i32> {
    wsa_ensure()?;
    // FFI invariant: `WSASendMsg` takes a socket, a
    // pointer to a caller-owned `WSAMSG` (whose buffers
    // point to caller-owned slices) and a pointer to a
    // caller-owned `DWORD` for the byte count; the last two
    // parameters are always null (no overlapped I/O). A bad
    // socket fails with `SOCKET_ERROR`, it cannot fault on
    // valid pointers.
    let mut sent: u32 = 0;
    let r = unsafe {
        ffi::WSASendMsg(
            fd,
            msg,
            flags,
            &mut sent,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        Ok(sent as usize)
    }
}

/// `WSARecvMsg` (S3 uses it): the Windows counterpart of
/// `sys_recvmsg`. Same return shape (bytes received) as
/// the Unix wrapper; the `received` out-parameter is
/// managed here. The caller reads the message flags back
/// from the `WSAMSG` it owns.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_recvmsg(fd: RawFd, msg: *mut WsaMsg) -> Result<usize, i32> {
    wsa_ensure()?;
    // FFI invariant: `WSARecvMsg` takes a socket, a
    // pointer to a caller-owned `WSAMSG` (whose buffers
    // point to caller-owned slices) and a pointer to a
    // caller-owned `DWORD` for the byte count; the last two
    // parameters are always null (no overlapped I/O). A bad
    // socket fails with `SOCKET_ERROR`, it cannot fault on
    // valid pointers.
    let mut received: u32 = 0;
    let r = unsafe {
        ffi::WSARecvMsg(
            fd,
            msg,
            &mut received,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        Ok(received as usize)
    }
}

/// `select` (S3 uses it; Windows only — Unix uses
/// `poll`). `read` / `write` / `except` are `Some` for a
/// set to test, `None` for no set; `timeout_ms` is `None`
/// for "block forever", `Some(ms)` for a bounded wait.
/// Returns the number of ready sockets (0 = timeout).
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_select(
    read: Option<&mut FdSet>,
    write: Option<&mut FdSet>,
    except: Option<&mut FdSet>,
    timeout_ms: Option<i32>,
) -> Result<i32, i32> {
    wsa_ensure()?;
    // FFI invariant: `select` takes pointers to
    // caller-owned `fd_set`s (null = no set for that
    // direction) and a pointer to a caller-owned `timeval`
    // (null = block forever); a bad socket fails with
    // `SOCKET_ERROR`, it cannot fault on valid pointers.
    let tv = timeout_ms.map(|ms| Timeval {
        sec: ms / 1000,
        usec: (ms % 1000) * 1000,
    });
    let r = unsafe {
        ffi::select(
            0,
            read.map_or(std::ptr::null_mut(), |s| s as *mut FdSet),
            write.map_or(std::ptr::null_mut(), |s| s as *mut FdSet),
            except.map_or(std::ptr::null_mut(), |s| s as *mut FdSet),
            tv.as_ref().map_or(std::ptr::null(), |t| t as *const Timeval),
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r)
    }
}

/// `shutdown(2)`.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_shutdown(fd: RawFd, how: i32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `shutdown` takes a handle and a plain
    // int; a bad handle fails EBADF, it cannot fault.
    let r = unsafe { ffi::shutdown(fd, how) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `getsockname(2)`: fills `addr` / `len` with the local
/// address (`len` is in/out).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getsockname(fd: RawFd, addr: *mut Sockaddr, len: *mut u32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `getsockname` takes a handle and
    // pointers to a caller-owned `sockaddr` buffer and its
    // length; a bad handle fails EBADF, it cannot fault on
    // valid pointers.
    let r = unsafe { ffi::getsockname(fd, addr, len as *mut ffi::Socklen) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `getpeername(2)`: fills `addr` / `len` with the peer
/// address (`len` is in/out).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getpeername(fd: RawFd, addr: *mut Sockaddr, len: *mut u32) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `getpeername` takes a handle and
    // pointers to a caller-owned `sockaddr` buffer and its
    // length; a bad handle fails EBADF (unconnected), it
    // cannot fault on valid pointers.
    let r = unsafe { ffi::getpeername(fd, addr, len as *mut ffi::Socklen) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `setsockopt(2)`: `val` points to `len` bytes of option
/// value (an int, a `timeval`, ... — the caller owns the
/// encoding; see `modsocket_ffi.rs` for the Windows
/// `timeval` → DWORD conversion).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_setsockopt(
    fd: RawFd,
    level: i32,
    opt: i32,
    val: *const u8,
    len: u32,
) -> Result<(), i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `setsockopt` takes a handle, plain
    // ints, and a pointer to a caller-owned value of `len`
    // bytes; a bad handle fails EBADF, it cannot fault on
    // a valid pointer.
    let r = unsafe {
        ffi::setsockopt(
            fd,
            level,
            opt,
            val as *const c_void,
            len as ffi::Socklen,
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `getsockopt(2)`: fills `val` with up to `*len` bytes
/// and updates `*len` to the bytes written (`len` is
/// in/out).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getsockopt(
    fd: RawFd,
    level: i32,
    opt: i32,
    val: *mut u8,
    len: *mut u32,
) -> Result<usize, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `getsockopt` takes a handle, plain
    // ints, and pointers to a caller-owned buffer and its
    // length; a bad handle fails EBADF, it cannot fault on
    // valid pointers.
    let r = unsafe {
        ffi::getsockopt(
            fd,
            level,
            opt,
            val as *mut c_void,
            len as *mut ffi::Socklen,
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        // FFI invariant: on success the kernel wrote
        // `*len` bytes into `val`; reading `*len` back is
        // safe (the kernel only writes that many).
        Ok(unsafe { *len } as usize)
    }
}

/// Close a socket handle: `close(2)` on Unix,
/// `closesocket` on Windows (a `SOCKET` is not an fd —
/// `close` would be wrong there).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_closesocket(fd: RawFd) -> Result<(), i32> {
    #[cfg(unix)]
    {
        // FFI invariant: `close` takes any i32 fd number;
        // the kernel fails EBADF on a bad fd, it cannot
        // fault.
        let r = unsafe { ffi::close(fd) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
    #[cfg(windows)]
    {
        wsa_ensure()?;
        // FFI invariant: `closesocket` takes any `SOCKET`;
        // it fails `WSAENOTSOCK` on a bad handle, it cannot
        // fault.
        let r = unsafe { ffi::closesocket(fd) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
}

/// `dup(2)`: duplicate a handle (new handle, same file
/// description). The `family` / `kind` / `proto`
/// arguments are ignored on Unix; on Windows they are
/// needed for `WSASocket` (see below).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_dup(fd: RawFd, family: i32, kind: i32, proto: i32) -> Result<RawFd, i32> {
    #[cfg(unix)]
    {
        let _ = (family, kind, proto);
        // FFI invariant: `dup` takes any i32 fd number; the
        // kernel fails EBADF on a bad fd, it cannot fault.
        let r = unsafe { ffi::dup(fd) };
        if r < 0 {
            Err(errno())
        } else {
            Ok(r)
        }
    }
    #[cfg(windows)]
    {
        wsa_ensure()?;
        // FFI invariant: `WSADuplicateSocket` takes a
        // `SOCKET`, the current process id, and a
        // caller-owned `WSAPROTOCOL_INFO` buffer; it fills
        // the buffer (373 bytes — the 512-byte buffer is
        // larger, so the write cannot overflow it).
        // `WSASocket` then creates a new `SOCKET` from that
        // buffer. Both fail with `INVALID_SOCKET` / -1 and
        // the WSA error cell holds the failure.
        let mut info = [0u8; 512];
        let r = unsafe {
            ffi::WSADuplicateSocket(fd, ffi::GetCurrentProcessId(), info.as_mut_ptr())
        };
        if r < 0 {
            return Err(errno());
        }
        let s = unsafe { ffi::WSASocket(family, kind, proto, info.as_mut_ptr(), 0, 0) };
        if s == INVALID_SOCKET {
            Err(errno())
        } else {
            Ok(s)
        }
    }
}

/// `fcntl(2)` (Unix only): `F_GETFL` / `F_SETFL` for the
/// nonblocking flag. Windows uses `ioctlsocket` (see
/// `sys_ioctlsocket`).
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_fcntl(fd: RawFd, cmd: i32, arg: i32) -> Result<i32, i32> {
    // FFI invariant: `fcntl` takes an fd number and plain
    // ints; a bad fd fails EBADF, a bad cmd fails EINVAL —
    // neither can fault.
    let r = unsafe { ffi::fcntl(fd, cmd, arg) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r)
    }
}

/// `ioctlsocket(FIONBIO)` (Windows only): set/clear the
/// nonblocking flag. `arg` is 1 (nonblocking) or 0.
#[cfg(all(feature = "unsafe-ffi", windows))]
pub fn sys_ioctlsocket(fd: RawFd, cmd: i32, arg: u32) -> Result<(), i32> {
    wsa_ensure()?;
    // FFI invariant: `ioctlsocket` takes a `SOCKET`, a
    // command, and a pointer to a caller-owned `u32`; a bad
    // handle fails `WSAENOTSOCK`, it cannot fault on a
    // valid pointer.
    let mut v = arg;
    let r = unsafe { ffi::ioctlsocket(fd, cmd, &mut v) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `socketpair(2)` (Unix only): create a connected pair,
/// returning both handles. Windows has no `socketpair` —
/// S2/S3 use a loopback TCP pair there.
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_socketpair(domain: i32, ty: i32, proto: i32) -> Result<[RawFd; 2], i32> {
    // FFI invariant: `socketpair` takes plain ints and a
    // pointer to a caller-owned `[c_int; 2]`; it fills both
    // entries on success, it cannot fault.
    let mut fds = [0i32; 2];
    let r = unsafe { ffi::socketpair(domain, ty, proto, &mut fds) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(fds)
    }
}

/// `poll(2)` (Unix) / `WSAPoll` (Windows): wait for
/// readiness on `fds` (`nfds` entries) for up to
/// `timeout_ms` milliseconds (-1 = infinite, 0 = return
/// immediately). Returns the number of ready entries (0 =
/// timeout). S1 uses it for `connect_timeout`; S3 reuses
/// it for the select-on-socket probe.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_poll(fds: *mut Pollfd, nfds: u32, timeout_ms: i32) -> Result<i32, i32> {
    #[cfg(windows)]
    wsa_ensure()?;
    // FFI invariant: `poll` / `WSAPoll` take a pointer to
    // a caller-owned `pollfd` array of `nfds` entries and a
    // plain int; a bad pointer fails EFAULT, it cannot
    // fault on a valid pointer. `EINTR` is returned (the
    // caller decides whether to retry).
    #[cfg(unix)]
    let r = unsafe { ffi::poll(fds, nfds, timeout_ms) };
    #[cfg(windows)]
    let r = unsafe { ffi::WSAPoll(fds, nfds, timeout_ms) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r)
    }
}
