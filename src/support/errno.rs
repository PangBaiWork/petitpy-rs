//! Portable `errno` access (`support/errno_compat.zig`).
//!
//! The Zig centralises the libc `errno` lvalue (`__error` on
//! Darwin, `__errno_location` on glibc/musl/wasi-libc) so the
//! per-module bodies don't each carry their own copy. In Rust
//! the same value is `std::io::Error::last_os_error()` — no
//! libc, no raw pointers. The Zig's `errnoRef()` returns a
//! `*c_int` (a mutable lvalue); Rust code only ever READS the
//! code after a failed call, so the thin reader below is the
//! whole port. (File-IO lanes use this after a failed `std::fs`
//! operation; the `std::io::Error` itself also carries
//! `raw_os_error()`.)

/// The calling thread's last OS error code, or 0 if none is
/// recorded (`errno_compat.zig` `errnoRef().*` read).
pub fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0)
}
