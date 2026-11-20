//! Shared `unsafe-ffi` syscall wrappers for the os module only.
//! Zero dependencies, no `libc` crate.
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
//! identical on both arches. `lseek`'s `off_t` is `i64` on both.
//!
//! History: this file once also held the G6b socket/subprocess
//! wrappers (`sys_socket`, `sys_bind`, `sys_listen`,
//! `sys_getsockopt`, `sys_kill`, `sys_dup2`, plus `SockaddrUn`).
//! Phase H replaced those modules with pure-Rust `std`
//! implementations, leaving the wrappers uncalled; they were
//! deleted 2026-11-20, leaving only the four os wrappers below
//! (`umask`, `close`, `fsync`, `lseek`).

/// The raw declarations (private mod).
#[cfg(feature = "unsafe-ffi")]
mod ffi {
    use std::os::raw::{c_int, c_uint};

    // FFI invariant: glibc's `__errno_location` returns a pointer
    // to the calling thread's errno cell; it is never null, and the
    // cell is only read (never written) by the caller.
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
