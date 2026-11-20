//! The FFI socket backend (`unsafe-ffi` only): `SockAddr`
//! + `FfiSocket`. Active only when the `unsafe-ffi`
//! feature is on; the default build never compiles this
//! file.
//!
//! `FfiSocket` owns a REAL socket handle from construction
//! (`socket(2)` in `new`), so `fileno()` is stable for the
//! socket's whole lifetime — no placeholder swap like the
//! pure-`std` path. All methods return `Result<_, i32>`
//! (the errno / Winsock code); `modsocket.rs` turns the
//! error into `OSError`.
//!
//! The kernel is the state machine for most errors
//! (`EISCONN` on a second `connect`, `EOPNOTSUPP` on
//! `listen` of a datagram socket, ... — probed on Linux,
//! see `scripts/sockffi_s1_result.txt` §4). Two cases the
//! kernel cannot express are tracked here: `bound` (the
//! kernel auto-binds an unbound INET socket on `listen`,
//! and a second `bind` of a UNIX socket is `EADDRINUSE`
//! — the pinned behaviour is `EINVAL` for both, so
//! `bind` / `listen` check `bound` first).
//!
//! Numbering: the Python-facing numbers are
//! `modsocket.rs`'s Linux values (`AF_INET6` 10,
//! `SOL_SOCKET` 1, `SO_REUSEADDR` 2, ...). The kernel
//! numbers differ on macOS/Windows (`AF_INET6` 30/23,
//! `SOL_SOCKET` 0xffff, ...), so the FFI boundary
//! translates (`translate_family` / `translate_sockopt`).
//! `SockAddr::family()` returns the Python-facing family.
//!
//! Timeouts: `SO_RCVTIMEO` / `SO_SNDTIMEO` take a
//! `timeval` on Unix but a DWORD of milliseconds on
//! Windows — `setsockopt` converts (see below).
//!
//! Locking: `modsocket.rs` holds the socket-table lock
//! across FFI calls (including blocking ones). That is
//! safe in this VM: Python execution is single-threaded
//! and only the VM thread ever takes the table lock (no
//! background thread touches it — the readiness monitors
//! use their own table). The pure-`std` path clones
//! handles instead; the FFI path has no handle to clone
//! (the fd number IS the handle).

#![cfg(feature = "unsafe-ffi")]

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use crate::support::ffi;

/// Python-facing address families (`modsocket.rs`
/// constants): `AF_INET` 2, `AF_INET6` 10, `AF_UNIX` 1.
/// The kernel values differ on macOS/Windows (see
/// `translate_family`).
pub const AF_INET: i32 = 2;
/// Python-facing address families (`modsocket.rs`
/// constants): `AF_INET` 2, `AF_INET6` 10, `AF_UNIX` 1.
/// The kernel values differ on macOS/Windows (see
/// `translate_family`).
pub const AF_INET6: i32 = 10;
/// Python-facing address families (`modsocket.rs`
/// constants): `AF_INET` 2, `AF_INET6` 10, `AF_UNIX` 1.
/// The kernel values differ on macOS/Windows (see
/// `translate_family`).
pub const AF_UNIX: i32 = 1;

/// A socket address: INET / INET6 / UNIX. The family is
/// the Python-facing one (`AF_INET` 2 / `AF_INET6` 10 /
/// `AF_UNIX` 1); the kernel encoding is produced by
/// `to_raw` and read back by `from_raw`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SockAddr {
    Inet { ip: Ipv4Addr, port: u16 },
    Inet6 {
        ip: Ipv6Addr,
        port: u16,
        flowinfo: u32,
        scope_id: u32,
    },
    Unix { path: Vec<u8> },
}

impl SockAddr {
    /// The Python-facing family (`AF_INET` 2 / `AF_INET6`
    /// 10 / `AF_UNIX` 1).
    pub fn family(&self) -> i32 {
        match self {
            SockAddr::Inet { .. } => AF_INET,
            SockAddr::Inet6 { .. } => AF_INET6,
            SockAddr::Unix { .. } => AF_UNIX,
        }
    }

    /// Write the kernel `sockaddr` into `storage`,
    /// returning its length for `bind` / `connect` /
    /// `sendto`.
    pub fn to_raw(&self, storage: &mut ffi::SockaddrStorage) -> u32 {
        match self {
            SockAddr::Inet { ip, port } => {
                let sa = ffi::SockaddrIn::new(port.to_be(), ip.octets());
                let n = std::mem::size_of::<ffi::SockaddrIn>();
                // SAFETY (invariant): `sa` is a live
                // `#[repr(C)]` value; reading its `n`
                // bytes as a byte slice is valid, and
                // `storage` is 128 bytes, so the copy
                // cannot overflow it.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &sa as *const ffi::SockaddrIn as *const u8,
                        n,
                    )
                };
                storage._inner[..n].copy_from_slice(bytes);
                n as u32
            }
            SockAddr::Inet6 {
                ip,
                port,
                flowinfo,
                scope_id,
            } => {
                let sa = ffi::SockaddrIn6::new(
                    port.to_be(),
                    *flowinfo,
                    ip.octets(),
                    *scope_id,
                );
                let n = std::mem::size_of::<ffi::SockaddrIn6>();
                // SAFETY (invariant): same as above — `sa`
                // is a live `#[repr(C)]` value read as
                // bytes, and `storage` is 128 bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &sa as *const ffi::SockaddrIn6 as *const u8,
                        n,
                    )
                };
                storage._inner[..n].copy_from_slice(bytes);
                n as u32
            }
            #[cfg(unix)]
            SockAddr::Unix { path } => {
                let sa = ffi::SockaddrUn::new(path);
                let n = std::mem::size_of::<ffi::SockaddrUn>();
                // SAFETY (invariant): same as above — `sa`
                // is a live `#[repr(C)]` value read as
                // bytes, and `storage` is 128 bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &sa as *const ffi::SockaddrUn as *const u8,
                        n,
                    )
                };
                storage._inner[..n].copy_from_slice(bytes);
                ffi::SockaddrUn::len_for(path)
            }
            // Build #20: `ffi::SockaddrUn` does not exist
            // on Windows (Unix-only in `ffi.rs`), and
            // there is no Winsock `sockaddr_un` mapping
            // in this port — return length 0, so the
            // `bind` / `connect` / `sendto` call fails
            // with the OS error instead of not compiling.
            #[cfg(windows)]
            SockAddr::Unix { .. } => 0,
        }
    }

    /// Read a kernel `sockaddr` back (`accept` /
    /// `getsockname` / `getpeername` / `recvfrom` fill
    /// `storage` and set `len`). `None` for an unknown
    /// family.
    pub fn from_raw(storage: &ffi::SockaddrStorage, len: u32) -> Option<SockAddr> {
        // Unnamed peer (a Unix datagram from an unbound
        // socket): the kernel sets `len` to 0 and writes
        // no address. That is an empty Unix address, not
        // an error (matches `std`, whose `SocketAddr` is
        // unnamed there — `as_pathname()` is `None`).
        if len == 0 {
            return Some(SockAddr::Unix { path: Vec::new() });
        }
        let fam = storage.family();
        if fam == ffi::AF_INET as u16 {
            // SAFETY (invariant): `storage` holds a
            // kernel-written `sockaddr_in` (16 bytes, the
            // family matched); `SockaddrStorage` is
            // 8-byte aligned, so the `u16`/`u32` reads
            // are aligned.
            let sa = unsafe { &*(storage._inner.as_ptr() as *const ffi::SockaddrIn) };
            Some(SockAddr::Inet {
                ip: Ipv4Addr::from(sa.sin_addr),
                port: u16::from_be(sa.sin_port),
            })
        } else if fam == ffi::AF_INET6 as u16 {
            // SAFETY (invariant): same as above — the
            // family matched, so `storage` holds a
            // kernel-written `sockaddr_in6` (28 bytes).
            let sa = unsafe { &*(storage._inner.as_ptr() as *const ffi::SockaddrIn6) };
            Some(SockAddr::Inet6 {
                ip: Ipv6Addr::from(sa.sin6_addr),
                port: u16::from_be(sa.sin6_port),
                flowinfo: sa.sin6_flowinfo,
                scope_id: sa.sin6_scope_id,
            })
        } else if fam == ffi::AF_UNIX as u16 {
            unix_sockaddr_un(storage, len)
        } else {
            None
        }
    }
}

/// Read a kernel `sockaddr_un` back (Unix only). Split
/// out of `SockAddr::from_raw` so the `#[cfg]` stays on
/// a whole function: `#[cfg]` on an `else if` arm is
/// fragile, and on Windows `ffi::SockaddrUn` does not
/// exist at all (Build #20).
/// An all-NUL path is an unnamed address (empty path):
/// Build #20, macOS `accept()` returns `addrlen > 2`
/// with a zeroed path for the unbound peer (Linux
/// returns len 2), and the old code treated a leading
/// NUL as an abstract address of full length — a path
/// of NUL bytes instead of empty. Safe: no filesystem
/// path is all NULs, and an abstract name with real
/// content (leading NUL + non-NUL bytes) is preserved.
#[cfg(unix)]
fn unix_sockaddr_un(storage: &ffi::SockaddrStorage, len: u32) -> Option<SockAddr> {
    // SAFETY (invariant): the family matched, so
    // `storage` holds a kernel-written `sockaddr_un`;
    // the path starts at offset 2 on every platform
    // (see `ffi.rs`). Abstract addresses start with NUL
    // and are NOT NUL-terminated — the name is the full
    // `len - 2` bytes; filesystem paths end at the
    // first NUL.
    let sa = unsafe { &*(storage._inner.as_ptr() as *const ffi::SockaddrUn) };
    let total = (len as usize).saturating_sub(2);
    let path_len = total.min(sa.sun_path.len());
    let raw = &sa.sun_path[..path_len];
    let end = if raw.first() == Some(&0) {
        if raw.iter().all(|b| *b == 0) {
            0
        } else {
            raw.len()
        }
    } else {
        raw.iter().position(|b| *b == 0).unwrap_or(raw.len())
    };
    Some(SockAddr::Unix {
        path: raw[..end].to_vec(),
    })
}

/// Windows has no `sockaddr_un` in this port (see
/// `to_raw`): an `AF_UNIX` family from the kernel has
/// no address to read back.
#[cfg(windows)]
fn unix_sockaddr_un(_storage: &ffi::SockaddrStorage, _len: u32) -> Option<SockAddr> {
    None
}

/// Translate a Python-facing family (`modsocket.rs`
/// numbering: `AF_INET6` 10) to the kernel family
/// (`AF_INET6` is 10 on Linux/Android, 30 on macOS, 23
/// on Windows; `AF_INET`/`AF_UNIX` agree everywhere).
pub fn translate_family(py_family: i32) -> i32 {
    if py_family == AF_INET6 {
        ffi::AF_INET6
    } else {
        py_family
    }
}

/// The reverse: a KERNEL family (e.g. `ai_family` from
/// `getaddrinfo`, 30 on macOS) back to the Python-facing
/// family (`AF_INET6` 10). Identity on Linux/Android;
/// `30 → 10` on macOS, `23 → 10` on Windows.
/// Build #20: `socket_getaddrinfo_ffi` put the raw C
/// family (30 on macOS) into the 5-tuple and compared it
/// against the requested Python family, so an
/// `AF_INET6` request filtered everything out.
pub fn translate_family_back(kernel_family: i32) -> i32 {
    if kernel_family == ffi::AF_INET6 {
        AF_INET6
    } else {
        kernel_family
    }
}

/// Translate a Python-facing `(level, opt)` pair
/// (`modsocket.rs` numbering: `SOL_SOCKET` 1, ...) to
/// the kernel pair. On Linux/Android this is the
/// identity; on macOS/Windows the known pairs map to the
/// kernel numbers. Unknown pairs pass through — the
/// kernel returns `ENOPROTOOPT` for them.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn translate_sockopt(level: i32, opt: i32) -> (i32, i32) {
    (level, opt)
}

/// Translate a Python-facing `(level, opt)` pair
/// (`modsocket.rs` numbering: `SOL_SOCKET` 1, ...) to
/// the kernel pair. On Linux/Android this is the
/// identity; on macOS/Windows the known pairs map to the
/// kernel numbers. Unknown pairs pass through — the
/// kernel returns `ENOPROTOOPT` for them.
#[cfg(target_os = "macos")]
pub fn translate_sockopt(level: i32, opt: i32) -> (i32, i32) {
    match (level, opt) {
        (1, 2) => (ffi::SOL_SOCKET, ffi::SO_REUSEADDR),
        (1, 4) => (ffi::SOL_SOCKET, ffi::SO_ERROR),
        (1, 6) => (ffi::SOL_SOCKET, ffi::SO_BROADCAST),
        (1, 7) => (ffi::SOL_SOCKET, ffi::SO_SNDBUF),
        (1, 8) => (ffi::SOL_SOCKET, ffi::SO_RCVBUF),
        (1, 9) => (ffi::SOL_SOCKET, ffi::SO_KEEPALIVE),
        (1, 15) => (ffi::SOL_SOCKET, ffi::SO_REUSEPORT),
        (1, 20) => (ffi::SOL_SOCKET, ffi::SO_RCVTIMEO),
        (1, 21) => (ffi::SOL_SOCKET, ffi::SO_SNDTIMEO),
        (0, 2) => (ffi::IPPROTO_IP, ffi::IP_TTL),
        _ => (level, opt),
    }
}

/// Translate a Python-facing `(level, opt)` pair
/// (`modsocket.rs` numbering: `SOL_SOCKET` 1, ...) to
/// the kernel pair. On Linux/Android this is the
/// identity; on macOS/Windows the known pairs map to the
/// kernel numbers. Unknown pairs pass through — the
/// kernel returns `ENOPROTOOPT` for them. (Windows has
/// no `SO_REUSEPORT`, so it passes through and the
/// kernel returns `ENOPROTOOPT`.)
#[cfg(windows)]
pub fn translate_sockopt(level: i32, opt: i32) -> (i32, i32) {
    match (level, opt) {
        (1, 2) => (ffi::SOL_SOCKET, ffi::SO_REUSEADDR),
        (1, 4) => (ffi::SOL_SOCKET, ffi::SO_ERROR),
        (1, 6) => (ffi::SOL_SOCKET, ffi::SO_BROADCAST),
        (1, 7) => (ffi::SOL_SOCKET, ffi::SO_SNDBUF),
        (1, 8) => (ffi::SOL_SOCKET, ffi::SO_RCVBUF),
        (1, 9) => (ffi::SOL_SOCKET, ffi::SO_KEEPALIVE),
        (1, 20) => (ffi::SOL_SOCKET, ffi::SO_RCVTIMEO),
        (1, 21) => (ffi::SOL_SOCKET, ffi::SO_SNDTIMEO),
        (0, 2) => (ffi::IPPROTO_IP, ffi::IP_TTL),
        _ => (level, opt),
    }
}

/// A `timeval` (16 bytes: `tv_sec` u64 + `tv_usec` u64,
/// little-endian) for a timeout option. `None` is 16
/// zero bytes (no timeout).
#[cfg(unix)]
fn timeval_bytes(d: Option<Duration>) -> [u8; 16] {
    let (sec, usec) = match d {
        Some(d) => (d.as_secs(), d.subsec_micros() as u64),
        None => (0, 0),
    };
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&sec.to_le_bytes());
    v[8..].copy_from_slice(&usec.to_le_bytes());
    v
}

/// A `Duration` as a Windows DWORD of milliseconds
/// (what Winsock wants for `SO_RCVTIMEO` /
/// `SO_SNDTIMEO`), 4 bytes little-endian. `None` is 0
/// (no timeout).
#[cfg(windows)]
fn timeout_dword_ms(d: Option<Duration>) -> [u8; 4] {
    let ms = match d {
        Some(d) => d.as_millis().min(u32::MAX as u128) as u32,
        None => 0,
    };
    ms.to_le_bytes()
}

/// An FFI socket: the owned OS handle plus its shadow
/// state. The handle is created by `new` and closed by
/// `close` / `Drop` — never shared (no `Clone`).
pub struct FfiSocket {
    pub fd: ffi::RawFd,
    /// Python-facing family (`AF_INET` 2 / `AF_INET6` 10
    /// / `AF_UNIX` 1).
    pub family: i32,
    pub kind: i32,
    pub proto: i32,
    /// Shadow: `setblocking` state (`std` has no getter;
    /// `modsocket.rs` keeps the same shadow in
    /// `SockSettings` — the two are updated together by
    /// the FFI arms there).
    pub blocking: bool,
    /// Shadow: applied read timeout (`settimeout` /
    /// `SO_RCVTIMEO`).
    pub read_timeout: Option<Duration>,
    /// Shadow: applied write timeout (`settimeout` /
    /// `SO_SNDTIMEO`).
    pub write_timeout: Option<Duration>,
    /// `bind` / `connect` set this. The kernel cannot
    /// express the two pinned checks: `listen` on an
    /// unbound INET socket auto-binds (the pinned
    /// behaviour is `EINVAL`), and a second `bind` of a
    /// UNIX socket is `EADDRINUSE` (pinned: `EINVAL`).
    pub bound: bool,
    /// Set by `listen`. `modsocket.rs`'s `is_listener`
    /// reads it (the kernel state is not tracked
    /// anywhere else).
    pub listening: bool,
    /// Set by `close`; `Drop` skips the close then.
    pub closed: bool,
}

impl Drop for FfiSocket {
    fn drop(&mut self) {
        if !self.closed {
            let _ = ffi::sys_closesocket(self.fd);
        }
    }
}

impl FfiSocket {
    /// `socket(2)`: create a socket with a REAL handle
    /// (no placeholder). `family` is Python-facing
    /// (`AF_INET6` 10); it is translated to the kernel
    /// family here.
    pub fn new(family: i32, kind: i32, proto: i32) -> Result<Self, i32> {
        let fd = ffi::sys_socket(translate_family(family), kind, proto)?;
        Ok(Self {
            fd,
            family,
            kind,
            proto,
            blocking: true,
            read_timeout: None,
            write_timeout: None,
            bound: false,
            listening: false,
            closed: false,
        })
    }

    /// Wrap an existing handle (S3's `fromfd`). The
    /// handle is assumed usable as-is (`bound` starts
    /// true — the kernel enforces the rest; a fromfd
    /// socket's real state is unknowable here).
    pub fn from_raw(fd: ffi::RawFd, family: i32, kind: i32, proto: i32) -> Self {
        Self {
            fd,
            family,
            kind,
            proto,
            blocking: true,
            read_timeout: None,
            write_timeout: None,
            bound: true,
            listening: false,
            closed: false,
        }
    }

    /// The table key / `fileno()`: the OS handle as
    /// `i64` (a Windows `SOCKET` fits — handle values
    /// are < 2^63, as in the pure-`std` path).
    pub fn fileno(&self) -> i64 {
        self.fd as i64
    }

    /// `bind(2)`: an already-bound socket is `EINVAL`
    /// (pinned — the kernel gives `EADDRINUSE` for a
    /// second UNIX `bind`).
    pub fn bind(&mut self, addr: &SockAddr) -> Result<(), i32> {
        if self.bound {
            return Err(ffi::EINVAL);
        }
        let mut storage = ffi::SockaddrStorage::new();
        let len = addr.to_raw(&mut storage);
        ffi::sys_bind(
            self.fd,
            storage._inner.as_ptr() as *const ffi::Sockaddr,
            len,
        )?;
        self.bound = true;
        Ok(())
    }

    /// `listen(2)`: an unbound socket is `EINVAL`
    /// (pinned — the kernel auto-binds an unbound INET
    /// socket). A datagram socket is `EOPNOTSUPP` from
    /// the kernel; a connected socket is `EINVAL` from
    /// the kernel.
    pub fn listen(&mut self, backlog: i32) -> Result<(), i32> {
        if !self.bound {
            return Err(ffi::EINVAL);
        }
        ffi::sys_listen(self.fd, backlog)?;
        self.listening = true;
        Ok(())
    }

    /// `accept(2)`: returns the accepted socket (same
    /// family/kind/proto, the listener's blocking and
    /// timeouts — CPython: the accepted socket takes the
    /// listener's timeout) and the peer address.
    pub fn accept(&mut self) -> Result<(Self, SockAddr), i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let mut len = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        let fd = ffi::sys_accept(
            self.fd,
            storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut len,
        )?;
        let addr = SockAddr::from_raw(&storage, len).ok_or(ffi::EINVAL)?;
        let mut s = Self {
            fd,
            family: self.family,
            kind: self.kind,
            proto: self.proto,
            blocking: self.blocking,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            bound: true,
            listening: false,
            closed: false,
        };
        // Apply the inherited timeouts to the new handle
        // (the kernel starts with none).
        let (r, w) = (s.read_timeout, s.write_timeout);
        s.settimeout(r, w)?;
        if !s.blocking {
            s.setblocking(false)?;
        }
        Ok((s, addr))
    }

    /// `connect(2)`. A listener is `EINVAL` and an
    /// already-connected stream socket is `EISCONN`,
    /// both from the kernel. On success the socket is
    /// bound (a `connect` implicitly binds).
    pub fn connect(&mut self, addr: &SockAddr) -> Result<(), i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let len = addr.to_raw(&mut storage);
        ffi::sys_connect(
            self.fd,
            storage._inner.as_ptr() as *const ffi::Sockaddr,
            len,
        )?;
        self.bound = true;
        Ok(())
    }

    /// `connect(2)` with a timeout: nonblocking connect
    /// + `poll` for writability + `SO_ERROR` check.
    /// `EINPROGRESS` is expected, not an error. On
    /// timeout the error is `ETIMEDOUT`. The blocking
    /// flag is restored on every path.
    pub fn connect_timeout(
        &mut self,
        addr: &SockAddr,
        timeout: Duration,
    ) -> Result<(), i32> {
        let was_blocking = self.blocking;
        self.setblocking(false)?;
        let r = self.connect_timeout_inner(addr, timeout);
        // Restore the blocking flag on every path (the
        // socket was blocking when called — a timeout is
        // only set on a blocking socket).
        let _ = self.setblocking(was_blocking);
        r
    }

    fn connect_timeout_inner(
        &mut self,
        addr: &SockAddr,
        timeout: Duration,
    ) -> Result<(), i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let len = addr.to_raw(&mut storage);
        match ffi::sys_connect(
            self.fd,
            storage._inner.as_ptr() as *const ffi::Sockaddr,
            len,
        ) {
            Ok(()) => {
                self.bound = true;
                return Ok(());
            }
            Err(e) if e == ffi::EINPROGRESS => {}
            Err(e) => return Err(e),
        }
        // `poll` for writability until the deadline,
        // retrying `EINTR` (CPython retries too).
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let mut pfd = ffi::Pollfd {
                fd: self.fd,
                events: ffi::POLLOUT,
                revents: 0,
            };
            match ffi::sys_poll(&mut pfd, 1, ms) {
                Ok(0) => return Err(ffi::ETIMEDOUT),
                Ok(_) => break,
                Err(e) if e == ffi::EINTR => {
                    if Instant::now() >= deadline {
                        return Err(ffi::ETIMEDOUT);
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        // Writable: the connect result is in `SO_ERROR`
        // (0 = connected).
        let mut buf = [0u8; 4];
        let mut buflen = 4u32;
        let n = ffi::sys_getsockopt(
            self.fd,
            ffi::SOL_SOCKET,
            ffi::SO_ERROR,
            buf.as_mut_ptr(),
            &mut buflen,
        )?;
        let _ = n;
        let err = i32::from_ne_bytes(buf);
        if err == 0 {
            self.bound = true;
            Ok(())
        } else {
            Err(err)
        }
    }

    /// `send(2)`: one send, returning the bytes sent.
    pub fn send(&self, buf: &[u8], flags: i32) -> Result<usize, i32> {
        ffi::sys_send(self.fd, buf.as_ptr(), buf.len(), flags)
    }

    /// `recv(2)`: one receive, returning the bytes read
    /// (0 = the peer closed, TCP).
    pub fn recv(&self, buf: &mut [u8], flags: i32) -> Result<usize, i32> {
        ffi::sys_recv(self.fd, buf.as_mut_ptr(), buf.len(), flags)
    }

    /// `sendto(2)`: one datagram send, returning the
    /// bytes sent.
    pub fn sendto(&self, buf: &[u8], flags: i32, addr: &SockAddr) -> Result<usize, i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let len = addr.to_raw(&mut storage);
        ffi::sys_sendto(
            self.fd,
            buf.as_ptr(),
            buf.len(),
            flags,
            storage._inner.as_ptr() as *const ffi::Sockaddr,
            len,
        )
    }

    /// `recvfrom(2)`: one datagram receive, returning
    /// the bytes read and the peer address.
    pub fn recvfrom(&self, buf: &mut [u8], flags: i32) -> Result<(usize, SockAddr), i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let mut len = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        let n = ffi::sys_recvfrom(
            self.fd,
            buf.as_mut_ptr(),
            buf.len(),
            flags,
            storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut len,
        )?;
        let addr = SockAddr::from_raw(&storage, len).ok_or(ffi::EINVAL)?;
        Ok((n, addr))
    }

    /// `shutdown(2)`.
    pub fn shutdown(&self, how: i32) -> Result<(), i32> {
        ffi::sys_shutdown(self.fd, how)
    }

    /// `getsockname(2)`: the local address.
    pub fn getsockname(&self) -> Result<SockAddr, i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let mut len = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        ffi::sys_getsockname(
            self.fd,
            storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut len,
        )?;
        SockAddr::from_raw(&storage, len).ok_or(ffi::EINVAL)
    }

    /// `getpeername(2)`: the peer address (`ENOTCONN`
    /// when unconnected, from the kernel).
    pub fn getpeername(&self) -> Result<SockAddr, i32> {
        let mut storage = ffi::SockaddrStorage::new();
        let mut len = std::mem::size_of::<ffi::SockaddrStorage>() as u32;
        ffi::sys_getpeername(
            self.fd,
            storage._inner.as_mut_ptr() as *mut ffi::Sockaddr,
            &mut len,
        )?;
        SockAddr::from_raw(&storage, len).ok_or(ffi::EINVAL)
    }

    /// `setsockopt(2)`: `val` is the canonical option
    /// value (an int as 4 little-endian bytes, a
    /// `timeval` as 16 bytes, ... — the encoding is the
    /// caller's, as in the pure-`std` path). The
    /// `(level, opt)` pair is translated to kernel
    /// numbers first. On Windows a `timeval` for
    /// `SO_RCVTIMEO` / `SO_SNDTIMEO` becomes a DWORD of
    /// milliseconds.
    pub fn setsockopt(&self, level: i32, opt: i32, val: &[u8]) -> Result<(), i32> {
        let (klevel, kopt) = translate_sockopt(level, opt);
        #[cfg(windows)]
        {
            if klevel == ffi::SOL_SOCKET
                && (kopt == ffi::SO_RCVTIMEO || kopt == ffi::SO_SNDTIMEO)
                && val.len() == 16
            {
                // `timeval` (sec u64 + usec u64, LE) →
                // DWORD milliseconds.
                let sec = u64::from_le_bytes(val[0..8].try_into().unwrap());
                let usec = u64::from_le_bytes(val[8..16].try_into().unwrap());
                let ms = sec
                    .saturating_mul(1000)
                    .saturating_add(usec / 1000)
                    .min(u32::MAX as u64) as u32;
                let dw = ms.to_le_bytes();
                return ffi::sys_setsockopt(
                    self.fd,
                    klevel,
                    kopt,
                    dw.as_ptr(),
                    dw.len() as u32,
                );
            }
        }
        ffi::sys_setsockopt(self.fd, klevel, kopt, val.as_ptr(), val.len() as u32)
    }

    /// `getsockopt(2)`: fills `buf` with up to
    /// `buf.len()` bytes, returning the bytes written.
    /// The `(level, opt)` pair is translated to kernel
    /// numbers first. (The `modsocket.rs` arm reads the
    /// shadow instead — this is for internal use, e.g.
    /// `SO_ERROR` in `connect_timeout`.)
    pub fn getsockopt(&self, level: i32, opt: i32, buf: &mut [u8]) -> Result<usize, i32> {
        let (klevel, kopt) = translate_sockopt(level, opt);
        let mut len = buf.len() as u32;
        let n = ffi::sys_getsockopt(self.fd, klevel, kopt, buf.as_mut_ptr(), &mut len)?;
        Ok(n)
    }

    /// Set/clear nonblocking: `fcntl(F_SETFL,
    /// O_NONBLOCK)` on Unix, `ioctlsocket(FIONBIO)` on
    /// Windows. Updates the `blocking` shadow.
    pub fn setblocking(&mut self, blocking: bool) -> Result<(), i32> {
        #[cfg(unix)]
        {
            let flags = ffi::sys_fcntl(self.fd, ffi::F_GETFL, 0)?;
            let new = if blocking {
                flags & !ffi::O_NONBLOCK
            } else {
                flags | ffi::O_NONBLOCK
            };
            ffi::sys_fcntl(self.fd, ffi::F_SETFL, new)?;
        }
        #[cfg(windows)]
        {
            ffi::sys_ioctlsocket(self.fd, ffi::FIONBIO, if blocking { 0 } else { 1 })?;
        }
        self.blocking = blocking;
        Ok(())
    }

    /// Apply read/write timeouts (`SO_RCVTIMEO` /
    /// `SO_SNDTIMEO`): a `timeval` on Unix, a DWORD of
    /// milliseconds on Windows. `None` clears the
    /// timeout (16 zero bytes / 0). Updates the shadow
    /// fields. Does NOT touch the blocking flag (the
    /// caller sets that separately).
    pub fn settimeout(
        &mut self,
        r: Option<Duration>,
        w: Option<Duration>,
    ) -> Result<(), i32> {
        #[cfg(unix)]
        {
            let tv_r = timeval_bytes(r);
            let tv_w = timeval_bytes(w);
            ffi::sys_setsockopt(
                self.fd,
                ffi::SOL_SOCKET,
                ffi::SO_RCVTIMEO,
                tv_r.as_ptr(),
                tv_r.len() as u32,
            )?;
            ffi::sys_setsockopt(
                self.fd,
                ffi::SOL_SOCKET,
                ffi::SO_SNDTIMEO,
                tv_w.as_ptr(),
                tv_w.len() as u32,
            )?;
        }
        #[cfg(windows)]
        {
            let ms_r = timeout_dword_ms(r);
            let ms_w = timeout_dword_ms(w);
            ffi::sys_setsockopt(
                self.fd,
                ffi::SOL_SOCKET,
                ffi::SO_RCVTIMEO,
                ms_r.as_ptr(),
                ms_r.len() as u32,
            )?;
            ffi::sys_setsockopt(
                self.fd,
                ffi::SOL_SOCKET,
                ffi::SO_SNDTIMEO,
                ms_w.as_ptr(),
                ms_w.len() as u32,
            )?;
        }
        self.read_timeout = r;
        self.write_timeout = w;
        Ok(())
    }

    /// Close the handle. Idempotent (`Drop` skips a
    /// second close). Consumes `self`.
    pub fn close(mut self) -> Result<(), i32> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        ffi::sys_closesocket(self.fd)
    }

    /// `dup(2)`: a new handle for the same file
    /// description, with the same shadow state.
    pub fn dup(&self) -> Result<Self, i32> {
        let fd = ffi::sys_dup(self.fd, self.family, self.kind, self.proto)?;
        Ok(Self {
            fd,
            family: self.family,
            kind: self.kind,
            proto: self.proto,
            blocking: self.blocking,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            bound: self.bound,
            listening: self.listening,
            closed: false,
        })
    }
}

// ── Python-level methods (called from `modsocket.rs`) ────────────────
// These mirror the pure-`std` arms in `modsocket.rs`, but
// operate on the `FfiSocket` in the table. They use
// `modsocket.rs` internals (`pub(crate)` there):
// `self_socket` / `parse_inet_addr` / `parse_unix_path` /
// `inet_addr_tuple` / `sockets` / `settings` /
// `get_settings` / `alloc_socket` / `apply_settings` /
// `int_arg` / `str_arg` / `EINVAL` / `EBADF` /
// `EAFNOSUPPORT` / `MAX_RECV` / `SocketState`.
// Validation order matches the `std` arms (argument
// conversion before any fd check — e.g. `sendall` with a
// bytearray is the `TypeError` even on a closed socket).

use std::net::{IpAddr, SocketAddr as StdSocketAddr, SocketAddrV6};

use crate::memory::gc::CallCtx;
use crate::modules::modsocket::{
    alloc_socket, apply_settings, get_settings, inet_addr_tuple, int_arg, parse_inet_addr,
    parse_unix_path, self_socket, settings, sockets, str_arg, SocketState, EAFNOSUPPORT, EBADF,
    EINVAL, MAX_RECV,
};
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::strings::objstr_a::new_bytes;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::{malloc_fail, raise_os_error};

/// Map an FFI errno (Unix errno / Windows WSA code) to
/// `OSError` (Windows codes go through
/// `win32_to_errno`, so Python sees one errno numbering
/// on every platform).
pub(crate) fn ffi_os_error(ctx: &mut CallCtx, e: i32) -> PyError {
    #[cfg(windows)]
    let e = crate::runtime::modos::win32_to_errno(e);
    raise_os_error(ctx, e)
}

/// A small int, infallible for socket fields (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket field fits the small-int window")
}

/// A `SockAddr` as a CPython address value: an
/// `(host, port)` tuple for INET, pathname bytes for
/// UNIX. `pub(crate)`: the FFI-advanced module
/// (`modsocket_ffi_adv.rs`) uses it for `recvmsg`.
pub(crate) fn sockaddr_value(ctx: &mut CallCtx, a: &SockAddr) -> Value {
    match a {
        SockAddr::Inet { ip, port } => {
            inet_addr_tuple(ctx, StdSocketAddr::new(IpAddr::V4(*ip), *port))
        }
        SockAddr::Inet6 {
            ip,
            port,
            flowinfo,
            scope_id,
        } => inet_addr_tuple(
            ctx,
            StdSocketAddr::V6(SocketAddrV6::new(*ip, *port, *flowinfo, *scope_id)),
        ),
        SockAddr::Unix { path } => new_bytes(ctx, ctx.types, path),
    }
}

/// Parse a `connect` / `sendto` target address into a
/// `SockAddr` (INET `(host, port)` tuple / UNIX path).
/// `pub(crate)`: the FFI-advanced module
/// (`modsocket_ffi_adv.rs`) uses it for `sendmsg`.
pub(crate) fn parse_target(ctx: &mut CallCtx, v: Value, family: i32) -> PyResult<SockAddr> {
    match family {
        AF_UNIX => Ok(SockAddr::Unix {
            path: parse_unix_path(ctx, v)?,
        }),
        AF_INET | AF_INET6 => {
            let sa = parse_inet_addr(ctx, v, family)?;
            Ok(match sa {
                StdSocketAddr::V4(a) => SockAddr::Inet {
                    ip: *a.ip(),
                    port: a.port(),
                },
                StdSocketAddr::V6(a) => SockAddr::Inet6 {
                    ip: *a.ip(),
                    port: a.port(),
                    flowinfo: a.flowinfo(),
                    scope_id: a.scope_id(),
                },
            })
        }
        _ => Err(raise_os_error(ctx, EAFNOSUPPORT)),
    }
}

/// `sock.bind(address)` (FFI): only an unbound socket
/// can bind (rebinding is `EINVAL`, checked before the
/// address is parsed — same order as the `std` arm).
pub(crate) fn sock_bind_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let family = fields[0];
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    {
        let table = sockets();
        match table.get(&fd) {
            Some(SocketState::Ffi(f)) if f.bound => {
                return Err(raise_os_error(ctx, EINVAL));
            }
            Some(SocketState::Ffi(_)) => {}
            _ => return Err(raise_os_error(ctx, EBADF)),
        }
    }
    let addr = parse_target(ctx, args[1], family)?;
    let mut table = sockets();
    let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
        return Err(raise_os_error(ctx, EBADF));
    };
    f.bind(&addr).map_err(|e| ffi_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `sock.listen(backlog)` (FFI).
pub(crate) fn sock_listen_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let backlog = match args.get(1) {
        Some(v) => int_arg(ctx, *v)? as i32,
        None => 0,
    };
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let mut table = sockets();
    let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
        return Err(raise_os_error(ctx, EBADF));
    };
    f.listen(backlog).map_err(|e| ffi_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `sock.accept() -> (sock, addr)` (FFI): the client
/// inherits family/type/proto and the listener's
/// settings (CPython: the accepted socket takes the
/// listener's timeout).
pub(crate) fn sock_accept_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let (new_sock, addr) = {
        let mut table = sockets();
        let Some(SocketState::Ffi(l)) = table.get_mut(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        l.accept().map_err(|e| ffi_os_error(ctx, e))?
    };
    let new_key = new_sock.fileno();
    let parent_settings = get_settings(fd);
    sockets().insert(new_key, SocketState::Ffi(new_sock));
    if let Some(st) = sockets().get_mut(&new_key) {
        apply_settings(st, &parent_settings);
    }
    settings().insert(new_key, parent_settings);
    let client = alloc_socket(ctx, new_key, fields[0], fields[1], fields[2])?;
    let addr_v = sockaddr_value(ctx, &addr);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[client, addr_v]);
    Ok(Value::Heap(tuple))
}

/// `sock.connect(address)` (FFI): with a timeout set it
/// uses `connect_timeout`, else a plain `connect`. The
/// kernel enforces the state machine (a listener is
/// `EINVAL`, a second `connect` on a stream socket is
/// `EISCONN`); a failed connect leaves the socket
/// unbound.
pub(crate) fn sock_connect_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let family = fields[0];
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let addr = parse_target(ctx, args[1], family)?;
    let timeout = get_settings(fd).timeout;
    let mut table = sockets();
    let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
        return Err(raise_os_error(ctx, EBADF));
    };
    match timeout {
        Some(d) => f
            .connect_timeout(&addr, d)
            .map_err(|e| ffi_os_error(ctx, e))?,
        None => f.connect(&addr).map_err(|e| ffi_os_error(ctx, e))?,
    }
    Ok(Value::None)
}

/// `sock.recv(bufsize) -> bytes` (FFI).
pub(crate) fn sock_recv_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
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
    let mut buf = vec![0u8; n_signed as usize];
    let got = {
        let mut table = sockets();
        let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.recv(&mut buf, 0).map_err(|e| ffi_os_error(ctx, e))?
    };
    Ok(new_bytes(ctx, ctx.types, &buf[..got]))
}

/// `sock.send(bytes) -> int` (FFI): one write, returning
/// the bytes sent. The data conversion happens before
/// any fd check (a bytearray is the `TypeError` even on
/// a closed socket — same order as the `std` arm).
pub(crate) fn sock_send_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let n = {
        let mut table = sockets();
        let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.send(&data, 0).map_err(|e| ffi_os_error(ctx, e))?
    };
    Ok(small(n as i64))
}

/// `sock.sendall(bytes) -> None` (FFI): loops `send`
/// until all bytes are written (a partial `send` is
/// normal on a stream socket). Datagram `sendall` is a
/// single `send` (datagrams are atomic).
pub(crate) fn sock_sendall_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let mut rest = &data[..];
    let mut table = sockets();
    let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
        return Err(raise_os_error(ctx, EBADF));
    };
    while !rest.is_empty() {
        let n = f.send(rest, 0).map_err(|e| ffi_os_error(ctx, e))?;
        if n == 0 {
            break;
        }
        rest = &rest[n..];
    }
    Ok(Value::None)
}

/// `sock.sendto(bytes, address) -> int` (FFI):
/// datagram sockets only (a stream socket is `EISCONN`,
/// from the kernel).
pub(crate) fn sock_sendto_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, fields) = self_socket(ctx, args)?;
    let family = fields[0];
    let data = str_arg(ctx, args[1])?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let addr = parse_target(ctx, args[2], family)?;
    let n = {
        let mut table = sockets();
        let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.sendto(&data, 0, &addr)
            .map_err(|e| ffi_os_error(ctx, e))?
    };
    Ok(small(n as i64))
}

/// `sock.recvfrom(bufsize) -> (bytes, address)` (FFI).
pub(crate) fn sock_recvfrom_ffi(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
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
    let mut buf = vec![0u8; n_signed as usize];
    let (got, addr) = {
        let mut table = sockets();
        let Some(SocketState::Ffi(f)) = table.get_mut(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.recvfrom(&mut buf, 0).map_err(|e| ffi_os_error(ctx, e))?
    };
    let data = new_bytes(ctx, ctx.types, &buf[..got]);
    let addr_v = sockaddr_value(ctx, &addr);
    let tuple = new_tuple(ctx, ctx.types.tuple, &[data, addr_v]);
    Ok(Value::Heap(tuple))
}

/// `sock.getpeername() -> address` (FFI; new method —
/// the pure-`std` path has none). `ENOTCONN` when
/// unconnected, from the kernel.
pub(crate) fn sock_getpeername(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let addr = {
        let table = sockets();
        let Some(SocketState::Ffi(f)) = table.get(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.getpeername().map_err(|e| ffi_os_error(ctx, e))?
    };
    Ok(sockaddr_value(ctx, &addr))
}

/// `sock.getsockname() -> address` (FFI; new method —
/// the pure-`std` path has none).
pub(crate) fn sock_getsockname(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    let addr = {
        let table = sockets();
        let Some(SocketState::Ffi(f)) = table.get(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.getsockname().map_err(|e| ffi_os_error(ctx, e))?
    };
    Ok(sockaddr_value(ctx, &addr))
}

/// `sock.shutdown(how) -> None` (FFI; new method — the
/// pure-`std` path has none). `how` is `SHUT_RD` (0) /
/// `SHUT_WR` (1) / `SHUT_RDWR` (2); anything else is
/// `EINVAL` from the kernel.
pub(crate) fn sock_shutdown(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, fd, _fields) = self_socket(ctx, args)?;
    let how = int_arg(ctx, args[1])? as i32;
    if fd < 0 {
        return Err(raise_os_error(ctx, EBADF));
    }
    {
        let table = sockets();
        let Some(SocketState::Ffi(f)) = table.get(&fd) else {
            return Err(raise_os_error(ctx, EBADF));
        };
        f.shutdown(how).map_err(|e| ffi_os_error(ctx, e))?;
    }
    Ok(Value::None)
}
