//! DNS / service / address FFI wrappers (`unsafe-ffi` feature
//! only). Lane S2 of the socket-FFI brief
//! (`scripts/socket_ffi_brief.md` §2.2). Zero dependencies, no
//! `libc` crate: the raw `extern "C"` declarations live in the
//! private `ffi` module below and the public wrappers are safe
//! functions returning `Result<_, i32>` (errno, or a gai
//! error code for `getaddrinfo` / `getnameinfo`), following
//! the `src/support/ffi.rs` pattern. When the feature is OFF
//! this file contributes nothing: every item is
//! `#[cfg(feature = "unsafe-ffi")]`.
//!
//! Merge notes (the main agent reconciles these at merge):
//! - The `#[repr(C)]` structs (`Sockaddr`, `SockaddrIn`,
//!   `SockaddrIn6`, `SockaddrStorage`, `Addrinfo`) used to
//!   be duplicated here (S1's file was not in this
//!   worktree yet when this lane was written); Build #23
//!   deleted the duplicates and uses S1's canonical
//!   definitions in `crate::support::ffi` — the local
//!   copies had the Linux `sockaddr` layout on every
//!   platform, so `getnameinfo` built a malformed
//!   `sockaddr_in` on macOS (no `sin_len`, family in the
//!   wrong place) and every reverse lookup failed. Only
//!   `Servent` is still defined here (`ffi.rs` has none).
//! - `errno()` duplicates the portable-errno helper S1 adds
//!   to `ffi.rs` (brief §1.3: Linux `__errno_location`,
//!   macOS `__error`, Android `__errno`, Windows
//!   `WSAGetLastError`). Unify at merge (one shared
//!   `pub(crate) fn errno()` in `ffi.rs`).
//! - `ensure_winsock()` (Windows `WSAStartup`, `OnceLock`)
//!   duplicates the helper S3 owns (brief §3 S3, "Windows
//!   平台收尾"). Unify at merge.
//! - `win32_to_errno` is reused from `modos.rs` (made
//!   `pub(crate)` by this lane); see the call sites.
//!
//! Pointer discipline (brief §2.2 / §6): `getaddrinfo`'s
//! linked list is freed by the `AddrInfoList` Drop guard
//! in `getaddrinfo_owned`; `servent` pointers are read
//! immediately after the call and never stored (the struct
//! lives in static storage owned by the C library —
//! `getservbyname` / `getservbyport` are not thread-safe
//! in the strict sense, but this VM runs Python code on a
//! single thread); `inet_ntoa`'s static-buffer pointer is
//! copied immediately. `htonl` / `htons` / `ntohl` /
//! `ntohs` are NOT here (they are macros, not syscalls):
//! they are pure-Rust byte swaps in `modsocket::dns`.

#[cfg(feature = "unsafe-ffi")]
use std::ffi::{CStr, CString};
#[cfg(feature = "unsafe-ffi")]
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
#[cfg(all(feature = "unsafe-ffi", windows))]
use std::sync::OnceLock;

/// The canonical `sockaddr` / `addrinfo` structs (S1's
/// `crate::support::ffi` — Build #23: the local copies
/// had the Linux layout on every platform, which broke
/// `getnameinfo` on macOS; these have the platform
/// layout, with `sin_len` on macOS, and the constructors
/// that build them). `Servent` stays local (see below).
#[cfg(feature = "unsafe-ffi")]
use crate::support::ffi::{Addrinfo, Sockaddr, SockaddrIn, SockaddrIn6};

/// The raw declarations (private mod).
#[cfg(feature = "unsafe-ffi")]
mod ffi {
    use super::{Addrinfo, Servent, Sockaddr};
    use std::os::raw::{c_char, c_int, c_uint};

    // FFI invariant: the errno accessors return a pointer to
    // the calling thread's errno cell; it is never null, and
    // the cell is only read (never written) by the caller.
    // Linux / macOS / Android each have their own name.
    #[cfg(target_os = "linux")]
    extern "C" {
        pub fn __errno_location() -> *mut c_int;
    }
    #[cfg(target_os = "macos")]
    extern "C" {
        pub fn __error() -> *mut c_int;
    }
    #[cfg(target_os = "android")]
    extern "C" {
        pub fn __errno() -> *mut c_int;
    }

    // FFI invariant: the Unix (libc) DNS / service /
    // address functions take NUL-terminated strings (or
    // null) and plain integers / pointers; on failure they
    // return a gai code (`getaddrinfo` / `getnameinfo`),
    // -1 + errno (`gethostname`, `inet_pton` with a bad
    // family), 0 without setting errno (`inet_pton` with a
    // bad string, `inet_aton`), or null (`getservbyname` /
    // `getservbyport`, `inet_ntop`). None of them can fault
    // on valid inputs.
    #[cfg(unix)]
    extern "C" {
        pub fn getaddrinfo(
            node: *const c_char,
            service: *const c_char,
            hints: *const Addrinfo,
            res: *mut *mut Addrinfo,
        ) -> c_int;
        pub fn freeaddrinfo(res: *mut Addrinfo);
        pub fn getnameinfo(
            sa: *const Sockaddr,
            salen: c_uint,
            host: *mut c_char,
            hostlen: c_uint,
            serv: *mut c_char,
            servlen: c_uint,
            flags: c_int,
        ) -> c_int;
        pub fn gethostname(name: *mut c_char, len: usize) -> c_int;
        pub fn getservbyname(name: *const c_char, proto: *const c_char) -> *mut Servent;
        pub fn getservbyport(port: c_int, proto: *const c_char) -> *mut Servent;
        pub fn inet_pton(af: c_int, src: *const c_char, dst: *mut u8) -> c_int;
        pub fn inet_ntop(af: c_int, src: *const u8, dst: *mut c_char, size: c_uint)
            -> *const c_char;
        pub fn inet_aton(cp: *const c_char, inp: *mut u32) -> c_int;
        pub fn inet_ntoa(addr: u32) -> *const c_char;
    }

    // FFI invariant: the Windows (ws2_32) names are the same
    // C functions as on Unix (`getaddrinfo` is the ANSI
    // `ADDRINFOA` form). Every call needs `WSAStartup`
    // first (see `ensure_winsock`); `getaddrinfo` /
    // `getnameinfo` return a Winsock code (not a gai code)
    // on failure, and the other failures report through
    // `WSAGetLastError`.
    #[cfg(windows)]
    #[link(name = "ws2_32")]
    extern "C" {
        pub fn WSAStartup(version: u16, data: *mut u8) -> c_int;
        pub fn WSAGetLastError() -> c_int;
        pub fn getaddrinfo(
            node: *const c_char,
            service: *const c_char,
            hints: *const Addrinfo,
            res: *mut *mut Addrinfo,
        ) -> c_int;
        pub fn freeaddrinfo(res: *mut Addrinfo);
        pub fn getnameinfo(
            sa: *const Sockaddr,
            salen: c_uint,
            host: *mut c_char,
            hostlen: c_uint,
            serv: *mut c_char,
            servlen: c_uint,
            flags: c_int,
        ) -> c_int;
        pub fn gethostname(name: *mut c_char, len: usize) -> c_int;
        pub fn getservbyname(name: *const c_char, proto: *const c_char) -> *mut Servent;
        pub fn getservbyport(port: c_int, proto: *const c_char) -> *mut Servent;
        pub fn inet_pton(af: c_int, src: *const c_char, dst: *mut u8) -> c_int;
        pub fn inet_ntop(af: c_int, src: *const u8, dst: *mut c_char, size: c_uint)
            -> *const c_char;
        pub fn inet_ntoa(addr: u32) -> *const c_char;
    }
}

/// `struct servent`: `s_port` is in network byte order.
/// It is an `int` on Unix and a `short` on Windows
/// (Winsock's `servent`). Defined here — `ffi.rs` has no
/// canonical `Servent` (Build #23: the other structs
/// above moved to `crate::support::ffi`).
///
/// Build #34: on 64-bit Windows the field order is
/// SWAPPED — `winsock.h` puts `s_proto` before `s_port`
/// under `_WIN64` (`{ s_name, s_aliases, s_proto,
/// s_port }`; the winapi crate mirrors it with two
/// `#[cfg(target_pointer_width)]` variants). The old
/// order read `s_port` at offset 16, which on Win64 is
/// the `s_proto` pointer — `getservbyname("http")`
/// returned the pointer's low bytes as the port (Build
/// #33: 59935 instead of 80). 32-bit Windows and Unix
/// keep `s_port` before `s_proto`.
#[cfg(feature = "unsafe-ffi")]
#[repr(C)]
pub struct Servent {
    pub s_name: *mut std::os::raw::c_char,
    pub s_aliases: *mut *mut std::os::raw::c_char,
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub s_proto: *mut std::os::raw::c_char,
    #[cfg(unix)]
    pub s_port: i32,
    #[cfg(windows)]
    pub s_port: i16,
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    pub s_proto: *mut std::os::raw::c_char,
}

/// Address families, PYTHON-numbered (`AF_INET6` 10 —
/// the value `modsocket.rs` exposes). These are NOT the
/// kernel values on macOS (30) / Windows (23): anything
/// handed to C must use `kernel_family` (below), which
/// translates to `crate::support::ffi`'s platform-correct
/// constants, or compare against a kernel family from
/// `ai_family` with `crate::support::ffi`'s constants
/// (see `sockaddr_from_raw`). (`AF_INET` 2 agrees
/// everywhere.) These consts are only used for the
/// Python-facing length match in `inet_pton_owned`.
#[cfg(feature = "unsafe-ffi")]
pub const AF_INET: i32 = 2;
#[cfg(feature = "unsafe-ffi")]
pub const AF_INET6: i32 = 10;

/// Translate a Python-facing family (`AF_INET` 2 /
/// `AF_INET6` 10) to the kernel family for a C call.
/// Build #23: `inet_pton` / `inet_ntop` passed the
/// Python family raw, and Darwin family 10 is
/// `AF_CCITT`, not `AF_INET6` (30) — so every IPv6
/// `inet_pton` / `inet_ntop` on macOS failed with
/// `EAFNOSUPPORT` 47. (`AF_INET` 2 is the identity.)
#[cfg(feature = "unsafe-ffi")]
fn kernel_family(py_family: i32) -> i32 {
    if py_family == AF_INET6 {
        crate::support::ffi::AF_INET6
    } else {
        py_family
    }
}

/// `EAI_SYSTEM` as the C library returns it from
/// `getaddrinfo` / `getnameinfo`: glibc / bionic -11,
/// Darwin 11 (the BSD values — cf. Build #22's
/// `EAI_*_RAW` pattern in `modsocket::dns`). The
/// `EAI_SYSTEM` const there is the glibc value on every
/// platform, so this file defines its own raw value.
#[cfg(all(feature = "unsafe-ffi", target_os = "macos"))]
const EAI_SYSTEM_RAW: i32 = 11;
#[cfg(all(feature = "unsafe-ffi", unix, not(target_os = "macos")))]
const EAI_SYSTEM_RAW: i32 = -11;

/// `getnameinfo` buffer sizes (brief §6: `NI_MAXHOST`
/// 1025, `NI_MAXSERV` 32 — the same on every platform).
#[cfg(feature = "unsafe-ffi")]
pub const NI_MAXHOST: usize = 1025;
#[cfg(feature = "unsafe-ffi")]
pub const NI_MAXSERV: usize = 32;

/// `EINVAL` (22 on every platform in this table's use).
#[cfg(feature = "unsafe-ffi")]
pub const EINVAL: i32 = 22;

/// Read the calling thread's errno cell (Unix) or the
/// Winsock error (Windows). Callers read it immediately
/// after a failed call, before any other call can touch
/// the cell. (Duplicates S1's `ffi.rs` helper — unify at
/// merge; see the file header.)
#[cfg(feature = "unsafe-ffi")]
fn errno() -> i32 {
    #[cfg(target_os = "linux")]
    // FFI invariant: `__errno_location` never returns null
    // and the cell is only read here, never written.
    unsafe {
        *ffi::__errno_location()
    }
    #[cfg(target_os = "macos")]
    // FFI invariant: `__error` never returns null and the
    // cell is only read here, never written.
    unsafe {
        *ffi::__error()
    }
    #[cfg(target_os = "android")]
    // FFI invariant: `__errno` never returns null and the
    // cell is only read here, never written.
    unsafe {
        *ffi::__errno()
    }
    #[cfg(windows)]
    // FFI invariant: `WSAGetLastError` returns the calling
    // thread's last Winsock error (0 when none).
    unsafe {
        ffi::WSAGetLastError()
    }
}

/// Ensure Winsock is initialised (Windows only):
/// `WSAStartup(MAKEWORD(2, 2))`, called once via
/// `OnceLock`. Returns the Winsock error code on failure.
/// (Duplicates S3's helper — unify at merge; see the file
/// header.)
#[cfg(all(feature = "unsafe-ffi", windows))]
fn ensure_winsock() -> Result<(), i32> {
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    *STARTED.get_or_init(|| {
        // FFI invariant: `WSAStartup` takes a version word
        // and a writable `WSADATA` buffer; the buffer is
        // 512 bytes (the real `WSADATA` is 400 bytes on
        // Win64), 8-aligned, and lives on the stack for the
        // call only. No `WSACleanup` (the process exit
        // cleans up, as in CPython).
        let mut data = [0u64; 64];
        let r = unsafe { ffi::WSAStartup(0x0202, data.as_mut_ptr() as *mut u8) };
        if r == 0 {
            Ok(())
        } else {
            Err(r)
        }
    })
}

/// Build a NUL-terminated C string from a Rust string,
/// truncating at the first NUL (the codebase's C-string
/// convention — `path_arg` does the same; the C call can
/// only see up to the NUL anyway).
#[cfg(feature = "unsafe-ffi")]
fn truncate_cstring(s: &str) -> CString {
    let end = s.as_bytes().iter().position(|&b| b == 0).unwrap_or(s.len());
    CString::new(&s.as_bytes()[..end]).expect("truncated string has no NUL")
}

/// Read a NUL-terminated byte buffer as a Rust `String`
/// (lossy — hostnames / service names are ASCII in
/// practice, and a lossy read beats failing the call).
#[cfg(feature = "unsafe-ffi")]
fn cstr_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

// ── Raw wrappers (§2.2) ─────────────────────────────────────────────

/// `getaddrinfo(3)` — returns the gai error code (0 on
/// success; `EAI_*` on Unix, a Winsock code on Windows —
/// never -1 + errno). The caller frees the list with
/// `sys_freeaddrinfo` (use `getaddrinfo_owned` instead,
/// which does it with a Drop guard).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getaddrinfo(
    node: *const std::os::raw::c_char,
    service: *const std::os::raw::c_char,
    hints: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> i32 {
    #[cfg(windows)]
    if let Err(e) = ensure_winsock() {
        return e;
    }
    // FFI invariant: node/service are null or point at
    // valid NUL-terminated strings, hints is null or a
    // valid `addrinfo`, res is a valid out-pointer. The C
    // call cannot fault on valid inputs.
    unsafe { ffi::getaddrinfo(node, service, hints, res) }
}

/// `freeaddrinfo(3)` — frees a list returned by
/// `getaddrinfo` (null is tolerated).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_freeaddrinfo(res: *mut Addrinfo) {
    // FFI invariant: res is null or a list head returned by
    // `getaddrinfo`; `freeaddrinfo` tolerates null.
    unsafe { ffi::freeaddrinfo(res) }
}

/// `getnameinfo(3)` — returns the gai error code (0 on
/// success). `host` / `serv` are filled with
/// NUL-terminated strings (either may be null when the
/// caller does not need it — this wrapper always passes
/// both buffers; see `getnameinfo_owned`).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getnameinfo(
    sa: *const Sockaddr,
    salen: u32,
    host: *mut std::os::raw::c_char,
    hostlen: u32,
    serv: *mut std::os::raw::c_char,
    servlen: u32,
    flags: i32,
) -> i32 {
    #[cfg(windows)]
    if let Err(e) = ensure_winsock() {
        return e;
    }
    // FFI invariant: sa points at a `sockaddr` of `salen`
    // bytes, host/serv point at writable buffers of
    // `hostlen` / `servlen` bytes. The C call cannot fault
    // on valid inputs.
    unsafe { ffi::getnameinfo(sa, salen, host, hostlen, serv, servlen, flags) }
}

/// `gethostname(2)` — fills `name` (up to `len` bytes,
/// NUL-terminated). Returns `Ok` on success, the errno /
/// Winsock code on failure.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_gethostname(name: *mut std::os::raw::c_char, len: usize) -> Result<(), i32> {
    #[cfg(windows)]
    if let Err(e) = ensure_winsock() {
        return Err(e);
    }
    // FFI invariant: name points at a writable buffer of
    // `len` bytes; the call fails with -1 + errno /
    // `WSAGetLastError` on error, never faults.
    let r = unsafe { ffi::gethostname(name, len) };
    if r == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

/// `getservbyname(3)` — returns the `servent` pointer, or
/// null when the service is unknown. The pointer is only
/// read immediately after the call, never stored (see
/// `getservbyname_owned`).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getservbyname(
    name: *const std::os::raw::c_char,
    proto: *const std::os::raw::c_char,
) -> *mut Servent {
    // On Windows a failed `WSAStartup` just makes the call
    // return null (→ "not found" → the built-in table
    // fallback), so the result is ignored here.
    #[cfg(windows)]
    let _ = ensure_winsock();
    // FFI invariant: name/proto are null or point at valid
    // NUL-terminated strings; the call returns null or a
    // pointer to static storage, never faults.
    unsafe { ffi::getservbyname(name, proto) }
}

/// `getservbyport(3)` — returns the `servent` pointer, or
/// null when the port is unknown. Same pointer discipline
/// as `sys_getservbyname`. `port` is in NETWORK byte order
/// (POSIX: the argument "shall be a value obtained by
/// converting a `uint16_t` in network byte order to
/// `int`"); the caller (`getservbyport_owned`) converts
/// from host order.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getservbyport(
    port: i32,
    proto: *const std::os::raw::c_char,
) -> *mut Servent {
    #[cfg(windows)]
    let _ = ensure_winsock();
    // FFI invariant: proto is null or a valid
    // NUL-terminated string; the call returns null or a
    // pointer to static storage, never faults.
    unsafe { ffi::getservbyport(port, proto) }
}

/// `inet_pton(3)` — parses a presentation-format address
/// into `dst` (4 bytes for `AF_INET`, 16 for `AF_INET6`).
/// `af` is the KERNEL family (callers translate from the
/// Python-facing family first — see `kernel_family`).
/// Returns `Ok` on success. On failure: `Err(0)` when the
/// string is not a valid address for the family
/// (`inet_pton` returns 0 and does NOT set errno — the 0
/// is a sentinel, not an errno; the caller maps it to
/// `OSError("illegal IP address string passed to
/// inet_pton")`), `Err(errno)` when the family is bad
/// (`inet_pton` returns -1 + `EAFNOSUPPORT`).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_inet_pton(af: i32, src: *const std::os::raw::c_char, dst: *mut u8) -> Result<(), i32> {
    #[cfg(windows)]
    if let Err(e) = ensure_winsock() {
        return Err(e);
    }
    // FFI invariant: src is a valid NUL-terminated string,
    // dst points at a writable buffer of 4 (`AF_INET`) or
    // 16 (`AF_INET6`) bytes. The call cannot fault.
    let r = unsafe { ffi::inet_pton(af, src, dst) };
    if r == 1 {
        Ok(())
    } else if r == 0 {
        Err(0)
    } else {
        Err(errno())
    }
}

/// `inet_ntop(3)` — formats the packed address at `src`
/// (4 / 16 bytes) into `dst` (a writable buffer of `size`
/// bytes, NUL-terminated). `af` is the KERNEL family
/// (callers translate from the Python-facing family
/// first — see `kernel_family`). Returns `Ok` on success,
/// the errno on failure (`EAFNOSUPPORT` for a bad family,
/// `ENOSPC` for a too-small buffer).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_inet_ntop(
    af: i32,
    src: *const u8,
    dst: *mut std::os::raw::c_char,
    size: u32,
) -> Result<(), i32> {
    #[cfg(windows)]
    if let Err(e) = ensure_winsock() {
        return Err(e);
    }
    // FFI invariant: src points at 4 (`AF_INET`) or 16
    // (`AF_INET6`) packed bytes, dst at a writable buffer
    // of `size` bytes. The call cannot fault.
    let r = unsafe { ffi::inet_ntop(af, src, dst, size) };
    if r.is_null() {
        Err(errno())
    } else {
        Ok(())
    }
}

/// `inet_aton(3)` (Unix only — Windows has no
/// `inet_aton`; the module layer uses the pure-Rust
/// parser there). Parses an IPv4 address, including the
/// short forms (`"127.1"`, `"0x7f.1"` — see
/// `parse_inet_aton` in `modsocket::dns`). Returns `Ok`
/// on success. On failure: `Err(0)` — `inet_aton`
/// returns 0 and does NOT set errno (the 0 is a sentinel,
/// not an errno; the caller maps it to `OSError("illegal
/// IP address string passed to inet_aton")`).
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn sys_inet_aton(cp: *const std::os::raw::c_char, inp: *mut u32) -> Result<(), i32> {
    // FFI invariant: cp is a valid NUL-terminated string,
    // inp is a valid out-pointer to a `u32` (the `in_addr`
    // is written in network byte order). The call cannot
    // fault.
    let r = unsafe { ffi::inet_aton(cp, inp) };
    if r == 1 {
        Ok(())
    } else {
        Err(0)
    }
}

/// `inet_ntoa(3)` — formats the `in_addr` (passed by
/// value; its memory holds the 4 octets in network
/// order) into a pointer to a static buffer. The pointer
/// is copied immediately by the caller and never held
/// (see `inet_ntoa_owned`).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_inet_ntoa(addr: u32) -> *const std::os::raw::c_char {
    #[cfg(windows)]
    let _ = ensure_winsock();
    // FFI invariant: `addr` is any `u32` (the `in_addr`
    // value); the call returns a pointer to a static
    // buffer, never null for a valid input.
    unsafe { ffi::inet_ntoa(addr) }
}

// ── Owned safe helpers (what `modsocket::dns` calls) ─────────────────

/// One resolved address from `getaddrinfo(3)`, fully
/// owned (no pointers into the C list survive the call).
#[cfg(feature = "unsafe-ffi")]
pub struct AddrInfoEntry {
    pub family: i32,
    pub socktype: i32,
    pub proto: i32,
    pub canonname: String,
    pub sockaddr: ResolvedAddr,
}

/// An address from `getaddrinfo(3)`, owned. Only
/// INET / INET6 occur for host lookups; UNIX is carried
/// defensively (a `sockaddr_un` in the list).
#[cfg(feature = "unsafe-ffi")]
pub enum ResolvedAddr {
    Inet(SocketAddr),
    Unix(Vec<u8>),
}

/// RAII guard for a `getaddrinfo(3)` result list: frees
/// it on drop, so an early return cannot leak it
/// (brief §6).
#[cfg(feature = "unsafe-ffi")]
struct AddrInfoList(*mut Addrinfo);

#[cfg(feature = "unsafe-ffi")]
impl Drop for AddrInfoList {
    fn drop(&mut self) {
        sys_freeaddrinfo(self.0);
    }
}

/// Read a `sockaddr` from a `getaddrinfo` result into an
/// owned address. `sa` points at `len` bytes of a
/// `sockaddr` of the given family (owned by the C list —
/// alive until the guard drops). Returns `None` for a
/// family we do not carry, or a too-short sockaddr.
///
/// `len` is `usize`: on Windows `ai_addrlen` is
/// `size_t`, so the `u32` parameter broke the Windows
/// build (Build #20, E0308 at the call site).
/// `family` is the KERNEL family straight from
/// `ai_family`, so it is matched against the
/// platform-correct constants in `crate::support::ffi`
/// (`AF_INET6` is 10 on Linux, 30 on macOS, 23 on
/// Windows) — NOT this file's Python-numbered
/// `AF_INET6` (10): Build #20, matching 10 skipped
/// every macOS IPv6 entry, so a family=`AF_INET6`
/// lookup came back empty.
#[cfg(feature = "unsafe-ffi")]
fn sockaddr_from_raw(sa: *mut Sockaddr, len: usize, family: i32) -> Option<ResolvedAddr> {
    if sa.is_null() {
        return None;
    }
    match family {
        crate::support::ffi::AF_INET if len >= std::mem::size_of::<SockaddrIn>() => {
            // FFI invariant: `sa` points at a valid
            // `sockaddr_in` (the list guarantees
            // `ai_addrlen` bytes); reading it cannot fault.
            let sin = unsafe { &*(sa as *const SockaddrIn) };
            let ip = Ipv4Addr::from(sin.sin_addr);
            let port = u16::from_be(sin.sin_port);
            Some(ResolvedAddr::Inet(SocketAddr::new(ip.into(), port)))
        }
        crate::support::ffi::AF_INET6 if len >= std::mem::size_of::<SockaddrIn6>() => {
            // FFI invariant: `sa` points at a valid
            // `sockaddr_in6`; reading it cannot fault.
            // `flowinfo` / `scope_id` are dropped — the
            // module returns 2-tuples for IPv6 (as the
            // pure-`std` path does), so they have nowhere
            // to go; a link-local address with a scope id
            // therefore loses the scope (noted in the
            // result file).
            let sin6 = unsafe { &*(sa as *const SockaddrIn6) };
            let ip = Ipv6Addr::from(sin6.sin6_addr);
            let port = u16::from_be(sin6.sin6_port);
            Some(ResolvedAddr::Inet(SocketAddr::new(ip.into(), port)))
        }
        1 if len >= std::mem::size_of::<Sockaddr>() => {
            // AF_UNIX: the path is the bytes after
            // `sun_family`, up to the first NUL (or the
            // end of the sockaddr). Defensive only.
            // (No all-NUL special case is needed here,
            // unlike `SockAddr::from_raw`: the first-NUL
            // rule already yields an empty path when the
            // whole path is NULs.)
            let raw = unsafe { std::slice::from_raw_parts(sa as *const u8, len) };
            let path = &raw[2..];
            let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
            Some(ResolvedAddr::Unix(path[..end].to_vec()))
        }
        _ => None,
    }
}

/// `getaddrinfo(3)` with owned results. `node` is `None`
/// for a null node (the local host); `service` is a port
/// number or a service name, as a string; `flags` are the
/// `AI_*` flags (e.g. `AI_PASSIVE` for a wildcard
/// lookup). On failure returns the gai error code (Unix)
/// or Winsock code (Windows) — the caller reports it as
/// the `OSError` errno (CPython's `gaierror` errno).
/// Entries whose sockaddr cannot be parsed are skipped
/// (defensive; INET / INET6 always parse).
#[cfg(feature = "unsafe-ffi")]
pub fn getaddrinfo_owned(
    node: Option<&str>,
    service: &str,
    family: i32,
    socktype: i32,
    proto: i32,
    flags: i32,
) -> Result<Vec<AddrInfoEntry>, i32> {
    let node_c = node.map(truncate_cstring);
    let service_c = truncate_cstring(service);
    let hints = Addrinfo {
        ai_flags: flags,
        ai_family: family,
        ai_socktype: socktype,
        ai_protocol: proto,
        ai_addrlen: 0,
        ai_addr: std::ptr::null_mut(),
        ai_canonname: std::ptr::null_mut(),
        ai_next: std::ptr::null_mut(),
    };
    let mut res: *mut Addrinfo = std::ptr::null_mut();
    let rc = sys_getaddrinfo(
        node_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null()),
        service_c.as_ptr(),
        &hints,
        &mut res,
    );
    if rc != 0 {
        return Err(rc);
    }
    let _guard = AddrInfoList(res);
    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        // FFI invariant: `cur` points into the
        // `getaddrinfo` list (alive until the guard
        // drops); `ai_addr` points at a `sockaddr` of
        // `ai_addrlen` bytes; `ai_canonname` is null or a
        // NUL-terminated string.
        let ai = unsafe { &*cur };
        let canonname = if ai.ai_canonname.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(ai.ai_canonname) }.to_string_lossy().into_owned()
        };
        if let Some(sockaddr) =
            sockaddr_from_raw(ai.ai_addr, ai.ai_addrlen as usize, ai.ai_family)
        {
            out.push(AddrInfoEntry {
                family: ai.ai_family,
                socktype: ai.ai_socktype,
                proto: ai.ai_protocol,
                canonname,
                sockaddr,
            });
        }
        cur = ai.ai_next;
    }
    Ok(out)
}

/// The address for `getnameinfo(3)`: an IP + port, with
/// the IPv6 flowinfo / scope id carried so a 4-tuple
/// round-trips.
#[cfg(feature = "unsafe-ffi")]
pub enum NameInfoAddr {
    Inet(Ipv4Addr, u16),
    Inet6(Ipv6Addr, u16, u32, u32),
}

/// Translate Python-facing (`glibc`) `NI_*` flags to the
/// kernel values for `getnameinfo(3)`. The Python-visible
/// constants (`dns.rs`) stay glibc values on every Unix;
/// only the FFI boundary translates. Darwin (BSD)
/// `netdb.h`: `NI_NOFQDN 0x1, NI_NUMERICHOST 0x2,
/// NI_NAMEREQD 0x4, NI_NUMERICSERV 0x8, NI_DGRAM 0x10` —
/// glibc swaps the first four (`NUMERICHOST 1,
/// NUMERICSERV 2, NOFQDN 4, NAMEREQD 8`), so passing the
/// Python flags straight through left `NI_NUMERICSERV`
/// unset on macOS and the service resolved to its name
/// (`"http"` instead of `"80"` — Build #24,
/// `sockffi_dns_reverse`). Unknown bits pass through
/// unchanged. Identity on Linux/Android/Windows (glibc
/// and Winsock values already match the Python ones).
#[cfg(all(feature = "unsafe-ffi", target_os = "macos"))]
fn translate_ni_flags(py_flags: i32) -> i32 {
    // glibc bit → Darwin bit: NUMERICHOST 1→2,
    // NUMERICSERV 2→8, NOFQDN 4→1, NAMEREQD 8→4,
    // DGRAM 16→16.
    let mut kernel = py_flags & !(1 | 2 | 4 | 8 | 16);
    if py_flags & 1 != 0 {
        kernel |= 2;
    }
    if py_flags & 2 != 0 {
        kernel |= 8;
    }
    if py_flags & 4 != 0 {
        kernel |= 1;
    }
    if py_flags & 8 != 0 {
        kernel |= 4;
    }
    if py_flags & 16 != 0 {
        kernel |= 16;
    }
    kernel
}

/// Translate Python-facing (`glibc`) `NI_*` flags to the
/// kernel values — identity everywhere except macOS (see
/// the macOS arm above).
#[cfg(all(feature = "unsafe-ffi", not(target_os = "macos")))]
fn translate_ni_flags(py_flags: i32) -> i32 {
    py_flags
}

/// `getnameinfo(3)` — reverse lookup. Returns
/// `(hostname, service)`; on failure the gai error code.
/// `flags` are the Python-facing `NI_*` flags
/// (`NI_NUMERICHOST` etc.); they are translated to the
/// kernel values on macOS (`translate_ni_flags`).
///
/// Build #23: the `sockaddr` is built with
/// `crate::support::ffi`'s constructors, which set the
/// platform layout (`sin_len` + the kernel family on
/// macOS). The old code built the Linux layout inline
/// (`sin_family` as a `u16` at offset 0), so on Darwin
/// the call saw `sin_len = 2`, `sin_family = 0
/// (AF_UNSPEC)` and failed — `gethostbyaddr` /
/// `getnameinfo` / `getfqdn` all raised the raw Darwin
/// gai code `EAI_NODATA` (5, displayed as `EIO`).
/// An `EAI_SYSTEM` return is mapped to the thread
/// errno (the gai code itself is not an errno — raising
/// it would report e.g. `EDEADLK` on Darwin, where
/// `EAI_SYSTEM` is 11); every other gai code is raised
/// as-is, as before.
#[cfg(feature = "unsafe-ffi")]
pub fn getnameinfo_owned(addr: &NameInfoAddr, flags: i32) -> Result<(String, String), i32> {
    // Build #24: `flags` arrive Python-facing (glibc
    // values); translate to the kernel values on macOS —
    // the Darwin `NI_*` bits differ (see
    // `translate_ni_flags`).
    let flags = translate_ni_flags(flags);
    let mut host = [0u8; NI_MAXHOST];
    let mut serv = [0u8; NI_MAXSERV];
    let rc = match addr {
        NameInfoAddr::Inet(ip, port) => {
            let sa = SockaddrIn::new(port.to_be(), ip.octets());
            sys_getnameinfo(
                &sa as *const SockaddrIn as *const Sockaddr,
                std::mem::size_of::<SockaddrIn>() as u32,
                host.as_mut_ptr() as *mut std::os::raw::c_char,
                host.len() as u32,
                serv.as_mut_ptr() as *mut std::os::raw::c_char,
                serv.len() as u32,
                flags,
            )
        }
        NameInfoAddr::Inet6(ip, port, flow, scope) => {
            let sa = SockaddrIn6::new(port.to_be(), *flow, ip.octets(), *scope);
            sys_getnameinfo(
                &sa as *const SockaddrIn6 as *const Sockaddr,
                std::mem::size_of::<SockaddrIn6>() as u32,
                host.as_mut_ptr() as *mut std::os::raw::c_char,
                host.len() as u32,
                serv.as_mut_ptr() as *mut std::os::raw::c_char,
                serv.len() as u32,
                flags,
            )
        }
    };
    if rc != 0 {
        #[cfg(unix)]
        if rc == EAI_SYSTEM_RAW {
            return Err(errno());
        }
        return Err(rc);
    }
    Ok((cstr_to_string(&host), cstr_to_string(&serv)))
}

/// `gethostname(2)` — the machine's hostname. Returns
/// the errno / Winsock code on failure.
#[cfg(feature = "unsafe-ffi")]
pub fn gethostname_owned() -> Result<String, i32> {
    let mut buf = [0u8; 256];
    sys_gethostname(buf.as_mut_ptr() as *mut std::os::raw::c_char, buf.len())?;
    Ok(cstr_to_string(&buf))
}

/// `getservbyname(3)` — the port for a service name
/// (and optional protocol, e.g. `"tcp"`). Returns `None`
/// when the service is unknown (the caller falls back to
/// the built-in table, then errors). The `servent` is
/// read immediately and never stored.
#[cfg(feature = "unsafe-ffi")]
pub fn getservbyname_owned(name: &str, proto: Option<&str>) -> Option<u16> {
    let name_c = truncate_cstring(name);
    let proto_c = proto.map(truncate_cstring);
    let sp = sys_getservbyname(
        name_c.as_ptr(),
        proto_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null()),
    );
    if sp.is_null() {
        return None;
    }
    // FFI invariant: `sp` points at a valid `servent`
    // (static storage); `s_port` is in network byte
    // order. Read once, here, never stored.
    let port = unsafe { (*sp).s_port };
    Some(u16::from_be(port as u16))
}

/// `getservbyport(3)` — the service name for a port (and
/// optional protocol). Returns `None` when the port is
/// unknown (the caller falls back to the built-in table,
/// then errors). The `servent` is read immediately and
/// never stored.
///
/// Build #23: `port` is passed in NETWORK byte order —
/// `getservbyport(3)` compares it directly against the
/// network-order `s_port`, so the old host-order call
/// matched the wrong service on macOS: `htons(20480) ==
/// 80`, and macOS's full `/etc/services` has `emwavemsg
/// 20480/tcp+udp`, so `getservbyport(80)` returned
/// `"emwavemsg"` instead of falling through to the
/// built-in table (Linux's minimal `/etc/services` has
/// no 20480 entry, so the bug was masked there; `22`
/// would likewise have matched `pcanywherestat`
/// 5632/udp).
#[cfg(feature = "unsafe-ffi")]
pub fn getservbyport_owned(port: u16, proto: Option<&str>) -> Option<String> {
    let proto_c = proto.map(truncate_cstring);
    let sp = sys_getservbyport(
        port.to_be() as i32,
        proto_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null()),
    );
    if sp.is_null() {
        return None;
    }
    // FFI invariant: `sp` points at a valid `servent`
    // (static storage); `s_name` is a NUL-terminated
    // string (null defensively → unknown). Read once,
    // here, never stored.
    let name = unsafe { (*sp).s_name };
    if name.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned())
}

/// `inet_pton(3)` — parses a presentation-format address
/// into packed bytes (4 for `AF_INET`, 16 for
/// `AF_INET6`). On failure: `Err(0)` for an invalid
/// string (see `sys_inet_pton`), `Err(errno)` for a bad
/// family. `af` is the PYTHON-facing family; it is
/// translated to the kernel family for the C call
/// (Build #23 — see `kernel_family`).
#[cfg(feature = "unsafe-ffi")]
pub fn inet_pton_owned(af: i32, ip: &str) -> Result<Vec<u8>, i32> {
    let src = truncate_cstring(ip);
    let mut dst = [0u8; 16];
    sys_inet_pton(kernel_family(af), src.as_ptr(), dst.as_mut_ptr())?;
    // The family was validated by the caller (only
    // `AF_INET` / `AF_INET6` reach here); the fallback
    // is unreachable defence.
    let n = match af {
        AF_INET => 4,
        AF_INET6 => 16,
        _ => return Err(EINVAL),
    };
    Ok(dst[..n].to_vec())
}

/// `inet_ntop(3)` — formats packed bytes (4 / 16) as a
/// presentation-format string. The caller validates the
/// length against the family first. On failure the errno.
/// `af` is the PYTHON-facing family; it is translated
/// to the kernel family for the C call (Build #23 — see
/// `kernel_family`).
#[cfg(feature = "unsafe-ffi")]
pub fn inet_ntop_owned(af: i32, packed: &[u8]) -> Result<String, i32> {
    let mut dst = [0u8; 64];
    sys_inet_ntop(
        kernel_family(af),
        packed.as_ptr(),
        dst.as_mut_ptr() as *mut std::os::raw::c_char,
        dst.len() as u32,
    )?;
    Ok(cstr_to_string(&dst))
}

/// `inet_aton(3)` (Unix only — Windows has no
/// `inet_aton`; the module layer uses the pure-Rust
/// parser there). Parses an IPv4 address, including the
/// short forms, into 4 octets in network order. On
/// failure: `Err(0)` (see `sys_inet_aton`).
#[cfg(all(feature = "unsafe-ffi", unix))]
pub fn inet_aton_owned(ip: &str) -> Result<[u8; 4], i32> {
    let src = truncate_cstring(ip);
    let mut inp: u32 = 0;
    sys_inet_aton(src.as_ptr(), &mut inp)?;
    // `inp`'s memory holds the 4 octets in network order.
    Ok(inp.to_ne_bytes())
}

/// `inet_ntoa(3)` — formats 4 packed octets as a
/// dotted-quad string. The static-buffer pointer is
/// copied immediately, never held.
#[cfg(feature = "unsafe-ffi")]
pub fn inet_ntoa_owned(packed: &[u8; 4]) -> Result<String, i32> {
    let addr = u32::from_ne_bytes(*packed);
    let p = sys_inet_ntoa(addr);
    if p.is_null() {
        // Cannot happen for a valid input (`inet_ntoa`
        // always returns its static buffer); defence only.
        return Err(EINVAL);
    }
    // FFI invariant: `p` points at a NUL-terminated
    // static buffer; copied here, never stored.
    Ok(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}
