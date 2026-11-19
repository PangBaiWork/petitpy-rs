//! Shared `unsafe-ffi` syscall wrappers for the G6 lanes (os, socket,
//! subprocess). Zero dependencies, no `libc` crate (G0 precedent:
//! `src/modules/modselect.rs` keeps its own private `ffi` module).
//!
//! Every `extern "C"` declaration lives in the private `ffi` module
//! below; the public wrappers are safe functions returning
//! `Result<_, i32>` with the errno (or, for `umask`, the previous
//! mask — it cannot fail). When the feature is OFF this file
//! contributes nothing: every item is `#[cfg(feature =
//! "unsafe-ffi")]` and every call site keeps its default-path
//! behavior byte-identical.
//!
//! Types are plain Rust ints. `c_int` / `c_uint` are `i32` / `u32`
//! on both Linux x86_64 and Linux aarch64, so the layout is
//! identical on both arches (the same reasoning as G0's
//! fd_set/timeval). `lseek`'s `off_t` is `i64` on both.

/// The raw declarations (private mod, mirroring G0's
/// `modselect::ffi` — that module is untouched).
#[cfg(feature = "unsafe-ffi")]
mod ffi {
    use std::os::raw::{c_int, c_uint};

    // FFI invariant: glibc's `__errno_location` returns a pointer
    // to the calling thread's errno cell; it is never null, and the
    // cell is only read (never written) by the caller.
    //
    // NOTE: this duplicates the same 3-line declaration in G0's
    // `modselect::ffi` deliberately — that module is not touched.
    extern "C" {
        pub fn __errno_location() -> *mut c_int;
    }

    // FFI invariant: `umask(2)` takes a mode_t (`c_uint`) and
    // returns the previous mask; it cannot fail. The argument is
    // the caller's responsibility (the kernel masks it to 0777).
    extern "C" {
        pub fn umask(mask: c_uint) -> c_uint;
    }

    // FFI invariant: `close(2)` takes any `c_int` fd number; the
    // kernel fails EBADF on a bad fd, it cannot fault. On -1 the
    // thread errno cell holds the failure.
    extern "C" {
        pub fn close(fd: c_int) -> c_int;
    }

    // FFI invariant: `fsync(2)` / `lseek(2)` take an fd number
    // and plain integers; on -1 the thread errno cell holds the
    // failure. A bad fd fails EBADF, a bad whence fails EINVAL —
    // neither can fault.
    extern "C" {
        pub fn fsync(fd: c_int) -> c_int;
        pub fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    }
}

/// Read the calling thread's errno cell. Callers read it
/// immediately after a failed call, before any other call can
/// touch the cell.
#[cfg(feature = "unsafe-ffi")]
fn errno() -> i32 {
    // FFI invariant: `__errno_location` never returns null (see
    // above) and the cell is only read here, never written.
    unsafe { *ffi::__errno_location() }
}

/// `umask(2)` — set the process file-creation mask, return the
/// previous mask (`modos.zig:382-388`). Infallible: the kernel
/// masks the argument to 0777 and always succeeds. The caller
/// wraps its argument with `as u32` (the Zig's `@intCast` would
/// panic on a negative mask — the reference aborts rc=134 on
/// `os.umask(-1)`, N1, not reproduced; Rust wraps instead).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_umask(mask: u32) -> u32 {
    // FFI invariant: `umask` cannot fail and takes any u32 (the
    // kernel masks to 0777); no errno read is needed.
    unsafe { ffi::umask(mask) }
}

/// `close(2)` (`modos.zig:308-313`). No EINTR retry (the
/// original has none).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_close(fd: i32) -> Result<(), i32> {
    // FFI invariant: `close` takes any i32 fd number; the kernel
    // fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::close(fd) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `fsync(2)` (`modos.zig:352-356`). No EINTR retry (the
/// original has none).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_fsync(fd: i32) -> Result<(), i32> {
    // FFI invariant: `fsync` takes any i32 fd number; the kernel
    // fails EBADF on a bad fd, it cannot fault.
    let r = unsafe { ffi::fsync(fd) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `lseek(2)` (`modos.zig:345-350`). No EINTR retry (the
/// original has none). The kernel is the authority on a bad
/// whence / negative offset — both fail EINVAL there.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> Result<i64, i32> {
    // FFI invariant: `lseek` takes an fd number and plain
    // integers; a bad fd fails EBADF, a bad whence fails EINVAL,
    // neither can fault.
    let r = unsafe { ffi::lseek(fd, offset, whence) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(r)
    }
}

// ── G6b: socket / subprocess ────────────────────────────────────────

/// `AF_UNIX` (the socket family; `sockaddr_un.sun_family`).
#[cfg(feature = "unsafe-ffi")]
pub const AF_UNIX: i32 = 1;

/// `SIGTERM` / `SIGKILL` (the subprocess timeout signals).
#[cfg(feature = "unsafe-ffi")]
pub const SIGTERM: i32 = 15;
#[cfg(feature = "unsafe-ffi")]
pub const SIGKILL: i32 = 9;

/// `struct sockaddr_un` (`sys/un.h`): `sun_family` (u16)
/// + `sun_path[108]` = 110 bytes, `#[repr(C)]` so the
/// layout matches the C struct exactly. The Zig passes
/// `sizeof(sockaddr_un)` (110) as the bind addrlen.
#[cfg(feature = "unsafe-ffi")]
#[repr(C)]
pub struct SockaddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

#[cfg(feature = "unsafe-ffi")]
impl SockaddrUn {
    /// Build a `sockaddr_un` for `bind`: family `AF_UNIX`,
    /// path bytes up to `plen` copied in, the rest zeroed.
    /// A zero `plen` is the all-zeros address — an ABSTRACT
    /// socket (Linux: leading NUL byte), as the Zig binds
    /// `""` (`modsocket.zig` `py_sock_bind`).
    pub fn new_unix(path: &[u8], plen: usize) -> SockaddrUn {
        let mut sun_path = [0u8; 108];
        sun_path[..plen].copy_from_slice(&path[..plen]);
        SockaddrUn {
            sun_family: AF_UNIX as u16,
            sun_path,
        }
    }
}

/// The G6b raw declarations (private mod, G6a pattern).
#[cfg(feature = "unsafe-ffi")]
mod ffi_b {
    use std::os::raw::{c_int, c_uint};

    use super::SockaddrUn;

    // FFI invariant: `socket(2)` takes three plain ints
    // and returns an fd or -1; a bad family/type fails
    // EAFNOSUPPORT in the kernel, it cannot fault.
    extern "C" {
        pub fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int;
    }

    // FFI invariant: `bind(2)` takes an fd, a pointer to
    // a caller-owned 110-byte `sockaddr_un`, and its size;
    // the kernel validates the address (bad family /
    // overlong path fail there), it cannot fault.
    extern "C" {
        pub fn bind(fd: c_int, addr: *const SockaddrUn, addrlen: c_uint) -> c_int;
    }

    // FFI invariant: `listen(2)` takes an fd and a plain
    // int backlog; the kernel decides (unbound → EINVAL,
    // already listening → Ok), it cannot fault.
    extern "C" {
        pub fn listen(fd: c_int, backlog: c_int) -> c_int;
    }

    // FFI invariant: `getsockopt(2)` takes an fd, two plain
    // ints, a caller-owned buffer and a pointer to its
    // length; the kernel writes at most `*optlen` bytes and
    // updates `*optlen` to the returned length, it cannot
    // fault. The buffer is a `Vec` owned by the wrapper.
    extern "C" {
        pub fn getsockopt(
            fd: c_int,
            level: c_int,
            optname: c_int,
            optval: *mut u8,
            optlen: *mut c_uint,
        ) -> c_int;
    }

    // FFI invariant: `kill(2)` takes a pid and a plain int
    // signal; a dead pid fails ESRCH in the kernel, it
    // cannot fault. Callers may ignore the error (the Zig
    // ignores kill errors — `_ = kill(...)`).
    extern "C" {
        pub fn kill(pid: c_int, sig: c_int) -> c_int;
    }

    // FFI invariant: `dup2(2)` takes two fd numbers; the
    // kernel validates them (bad fd → EBADF), it cannot
    // fault. Called from the `pre_exec` child hook, where
    // only async-signal-safe calls are allowed — `dup2`
    // is one.
    extern "C" {
        pub fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    }
}

/// `socket(2)` (`modsocket.zig` `py_socket_ctor`). No
/// EINTR retry (the original has none).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_socket(domain: i32, type_: i32, protocol: i32) -> Result<i32, i32> {
    // FFI invariant: `socket` takes three plain ints; a bad
    // family/type fails EAFNOSUPPORT in the kernel, it
    // cannot fault.
    let r = unsafe { ffi_b::socket(domain, type_, protocol) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(r)
    }
}

/// `bind(2)` (`modsocket.zig` `py_sock_bind`): the WHOLE
/// `sockaddr_un` size (110) as addrlen, as the Zig passes
/// `sizeof(sockaddr_un)`. No EINTR retry.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_bind(fd: i32, addr: &SockaddrUn) -> Result<(), i32> {
    // FFI invariant: `bind` takes an fd, a pointer to a
    // caller-owned 110-byte `sockaddr_un`, and its size;
    // the kernel validates the address, it cannot fault.
    let r = unsafe { ffi_b::bind(fd, addr, size_of::<SockaddrUn>() as u32) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `listen(2)` (`modsocket.zig` `py_sock_listen`). The
/// kernel is the authority: an unbound socket fails
/// EINVAL there (probed on the reference), a bound one
/// succeeds, a listening one succeeds again.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_listen(fd: i32, backlog: i32) -> Result<(), i32> {
    // FFI invariant: `listen` takes an fd and a plain int
    // backlog; the kernel decides, it cannot fault.
    let r = unsafe { ffi_b::listen(fd, backlog) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `getsockopt(2)` (`modsocket.zig` `py_sock_getsockopt`):
/// a 4096-byte stack buffer in the Zig; here a `Vec` of
/// `buflen` bytes. Returns the bytes of the RETURNED
/// `optlen` (the kernel may return fewer bytes than
/// asked for).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_getsockopt(fd: i32, level: i32, opt: i32, buflen: i32) -> Result<Vec<u8>, i32> {
    // The buffer is owned by this wrapper and outlives the
    // call; the kernel writes at most `buflen` bytes.
    let mut buf = vec![0u8; buflen as usize];
    let mut len = buflen as u32;
    // FFI invariant: `getsockopt` takes an fd, two plain
    // ints, a caller-owned buffer and a pointer to its
    // length; the kernel writes at most `*optlen` bytes,
    // it cannot fault.
    let r = unsafe { ffi_b::getsockopt(fd, level, opt, buf.as_mut_ptr(), &mut len) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        buf.truncate(len as usize);
        Ok(buf)
    }
}

/// `kill(2)` (`mod_subprocess.zig` `py_spawn_and_wait`):
/// the Zig ignores kill errors (`_ = kill(...)`), so
/// callers may ignore `Err` too.
#[cfg(feature = "unsafe-ffi")]
pub fn sys_kill(pid: i32, sig: i32) -> Result<(), i32> {
    // FFI invariant: `kill` takes a pid and a plain int
    // signal; a dead pid fails ESRCH in the kernel, it
    // cannot fault.
    let r = unsafe { ffi_b::kill(pid, sig) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}

/// `dup2(2)` (`mod_subprocess.zig` `py_spawn_and_wait`):
/// the child dup2's each stream mode onto its fd. The Zig
/// ignores dup2 errors (`_ = dup2(...)`) — the `pre_exec`
/// caller does the same. Async-signal-safe (called from
/// the `pre_exec` child hook).
#[cfg(feature = "unsafe-ffi")]
pub fn sys_dup2(oldfd: i32, newfd: i32) -> Result<(), i32> {
    // FFI invariant: `dup2` takes two fd numbers; the
    // kernel validates them (bad fd → EBADF), it cannot
    // fault.
    let r = unsafe { ffi_b::dup2(oldfd, newfd) };
    if r < 0 {
        // FFI invariant: errno is read immediately after the
        // failed call, before any other call can touch this
        // thread's errno cell.
        Err(errno())
    } else {
        Ok(())
    }
}
