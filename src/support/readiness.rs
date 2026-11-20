//! Readiness monitors for `select.select` (H-select).
//!
//! Pure-`std` readiness for fds that have no `select(2)` /
//! `poll(2)` API in `std`: one monitor thread per fd, sharing
//! the fd's `File` (`File::try_clone`, safe, shares the file
//! description and offset), doing blocking `read`s into a
//! shared, bounded buffer. `select` probes the buffer; the
//! file read path (`objfileio::fileio_read`) drains it first,
//! so no byte is ever read twice or lost.
//!
//! Mechanism (per fd, keyed by the fd number):
//! - `ensure_monitor(fd, file)` spawns the thread on first
//!   use (a second call for the same live fd returns the
//!   existing monitor — no thread per `select` call). The
//!   thread reads in 8 KiB chunks and pushes them into the
//!   buffer; when the buffer reaches `BUFFER_CAP` it parks
//!   on `space` until `take_buffered` frees room
//!   (backpressure, so a regular file can never fill memory).
//! - `take_buffered(fd, out)` drains up to `out.len()`
//!   bytes in order and wakes a parked monitor.
//! - `is_eof(fd)` is true once the monitor's read returned
//!   0: EOF counts as readable (CPython reports an EOF fd
//!   as ready), even when the buffer is empty.
//! - `note_closed(fd)` marks the monitor stale (called from
//!   the file close path). A stale monitor exits after its
//!   next read event — or at once if it is parked on a full
//!   buffer. A monitor blocked in `read` cannot be woken:
//!   it exits when the read returns (data, EOF, or error).
//!   On exit the entry stays while its buffer still holds
//!   bytes (removing it would strand them — `take_buffered`
//!   looks the monitor up by fd); `take_buffered` removes
//!   the entry once the last byte is drained, and the exit
//!   path removes it at once when the buffer is already
//!   empty. A later `ensure_monitor` for a reused fd number
//!   reuses a dead monitor's bytes only when the number
//!   still refers to the same description (the identity
//!   check — `(dev, ino)` on Unix, `(creation time, size)`
//!   on Windows) — otherwise it starts fresh.
//!
//! Bounds: at most one thread per distinct fd ever probed,
//! at most `BUFFER_CAP` bytes buffered per fd. A monitor
//! whose fd was closed out from under it stays blocked in
//! `read` until the next read event on its (dup'd)
//! description — the residual leak of this design, bounded
//! by one blocked thread per abandoned description whose
//! writer never writes or closes. (A monitor cannot poll
//! for liveness while blocked in `read`; polling the fd
//! number would need the fd tables, which a plain
//! `BuiltinFn` cannot see — see the note below.)
//!
//! Offset sharing: the monitor's `try_clone` shares the
//! file description — and therefore the offset — with the
//! original `File`. Read-ahead moves the shared offset, so
//! a later `write` through the same object would land past
//! the buffered bytes. That is safe for the cases `select`
//! monitors: pipes (the read end is never written) and
//! read-only files. Do not `select` on a read-write FIFO
//! object that you also write through.
//!
//! What feeds a monitor: a `File` the caller already holds.
//! A monitor is started from a file *object* (the arena's
//! `Obj::File` carries its own `Rc<File>`) or from a bare
//! fd int (the intercept in `call_function_ctx` resolves
//! it through `VmState.os_fds` — a plain `BuiltinFn` could
//! not see that table). Socket objects are NOT monitored:
//! pure `std` cannot probe a socket (`UnixStream::peek` is
//! unstable, and a monitor `recv` would consume bytes the
//! socket `recv` path does not drain) — see `modselect.rs`
//! and `scripts/h_select_result.txt` §4. There is no safe
//! `std` API for an arbitrary fd (`BorrowedFd::borrow_raw`
//! is `unsafe`), and `/proc/self/fd` is Linux-only and
//! refuses sockets, so no workaround exists within pure
//! `std`.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Bytes buffered per fd at most (`select` drains via
/// `take_buffered`; the monitor parks on `space` past it).
const BUFFER_CAP: usize = 64 * 1024;

/// Bytes per monitor `read` call.
const CHUNK: usize = 8 * 1024;

/// How long `ensure_monitor` waits for a fresh monitor's
/// first read to settle (see `wait_settled`): a thread
/// spawn + one immediately-returning read is microseconds,
/// so 10ms is generous even on a loaded 1.5-core VM — and
/// it is paid only once per fd (a reused monitor is already
/// settled). A select on an empty pipe with timeout 0 takes
/// up to this long instead of ~0; that is the price of a
/// correct first probe.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_millis(10);

/// One fd's shared monitor state (see the module docs).
pub struct Monitor {
    /// Bytes read ahead, in order; drained by `take_buffered`.
    buffer: Mutex<VecDeque<u8>>,
    /// Signalled when `take_buffered` frees room (the
    /// monitor waits here while the buffer is full).
    space: Condvar,
    /// Signalled when the monitor pushes bytes or reaches a
    /// terminal state (EOF / read error / stale exit): a
    /// `fileio_read` blocked in `wait_buffered` wakes here.
    /// (`space` is the monitor's own wait channel; `data` is
    /// the readers'.)
    data: Condvar,
    /// The read side hit EOF (`read` returned 0). EOF is
    /// readable, so `select` reports the fd ready even with
    /// an empty buffer.
    eof: AtomicBool,
    /// The thread exited (EOF / read error / stale). A dead
    /// entry is replaced, never reused.
    dead: AtomicBool,
    /// Set by `note_closed`: exit after the next read event
    /// (or at once when parked on a full buffer).
    stale: AtomicBool,
    /// Identity of the description the monitor reads
    /// (`(dev, ino)` on Unix, `file_index` on Windows):
    /// a later `ensure_monitor` for the same fd number
    /// reuses a dead monitor's buffered bytes only when
    /// the number still refers to this description (an
    /// fd number reused for another file must not inherit
    /// the old bytes). `None` where no identity is
    /// available — such entries are never reused dead.
    identity: Option<(u64, u64)>,
}

impl Monitor {
    fn new(identity: Option<(u64, u64)>) -> Self {
        Monitor {
            buffer: Mutex::new(VecDeque::new()),
            space: Condvar::new(),
            data: Condvar::new(),
            eof: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            stale: AtomicBool::new(false),
            identity,
        }
    }

    /// Bytes currently buffered (drainable by `take_buffered`).
    pub fn buffered_len(&self) -> usize {
        self.buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The read side hit EOF (readable even with an empty buffer).
    pub fn is_eof(&self) -> bool {
        self.eof.load(Ordering::Acquire)
    }

    /// The thread exited; a fresh monitor must be started.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }
}

/// The process-global monitor table: fd → live monitor.
fn table() -> &'static Mutex<HashMap<i32, Arc<Monitor>>> {
    static TABLE: OnceLock<Mutex<HashMap<i32, Arc<Monitor>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `mon` may be reused for a file with
/// `identity`: same description (when both identities are
/// known), not stale, and either live or dead-with-bytes.
/// A stale monitor is never reused (its fd was closed or
/// seeked); a live monitor for a DIFFERENT description
/// (the fd number was reused after `close`) is never
/// reused either — the kernel reuses the lowest free fd
/// number, so a lingering live monitor for a closed fd is
/// the common case, not a corner. When either identity is
/// unknown the check cannot run and the old behaviour
/// (reuse a live monitor) stands.
fn reusable(mon: &Monitor, identity: Option<(u64, u64)>) -> bool {
    if mon.stale.load(Ordering::Acquire) {
        return false;
    }
    if let (Some(a), Some(b)) = (mon.identity, identity) {
        if a != b {
            return false;
        }
    }
    if !mon.is_dead() {
        return true;
    }
    mon.buffered_len() > 0
}

/// Mark `mon` stale and wake it if it is parked on a full
/// buffer (it exits after its next read event; a monitor
/// blocked in `read` cannot be woken — see the module docs).
fn mark_stale(mon: &Monitor) {
    mon.stale.store(true, Ordering::Release);
    mon.space.notify_all();
    mon.data.notify_all();
}

/// Remove `fd`'s entry, but only if it is still `mon` (a
/// fresh monitor for a reused fd number must not be
/// removed by the old thread's exit).
fn remove_if_same(fd: i32, mon: &Arc<Monitor>) {
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cur) = t.get(&fd) {
        if Arc::ptr_eq(cur, mon) {
            t.remove(&fd);
        }
    }
}

/// Identity of the description `file` refers to, for the
/// dead-monitor reuse check in `ensure_monitor` (see
/// `Monitor::identity`). `None` where no identity is
/// available or `metadata` fails.
fn file_identity(file: &File) -> Option<(u64, u64)> {
    let md = file.metadata().ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.dev(), md.ino()))
    }
    #[cfg(windows)]
    {
        // `MetadataExt::file_index` is UNSTABLE (`windows_by_handle`,
        // rust-lang/rust#63010) and returns `Option<u64>` — it does
        // not compile on stable rustc (broke the CI Windows job,
        // 2026-11-20). Stable proxy: (creation time, size), both
        // inherent `Metadata` methods (`created` + `len` — the
        // Windows-only `creation_time`/`file_size` are trait
        // methods, not inherent). A false match needs two files
        // created in the same 100ns tick with the same size on the
        // same reused fd — far narrower than no check at all
        // (`None` skips the check entirely).
        let created = md
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Some((created, md.len()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = md;
        None
    }
}

/// Wait (bounded) for a fresh monitor's first read to
/// settle: the monitor thread notifies `data` whenever a
/// read completes (bytes pushed, EOF, or an error — see
/// `monitor_loop`), so this returns as soon as the first
/// read has returned. A blocking read on an empty pipe
/// never settles — the deadline bounds the wait. Without
/// this, a `select` with timeout 0 would race the monitor
/// thread and miss an fd that is already at EOF (its first
/// probe runs before the monitor's first read). Only called
/// for a freshly spawned monitor (a reused one is already
/// settled — its first read happened long ago).
fn wait_settled(mon: &Monitor, deadline: std::time::Duration) {
    let mut buf = mon.buffer.lock().unwrap_or_else(|e| e.into_inner());
    let start = std::time::Instant::now();
    while buf.is_empty() && !mon.is_eof() && !mon.is_dead() {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        let (b, r) = mon
            .data
            .wait_timeout(buf, remaining)
            .unwrap_or_else(|e| e.into_inner());
        buf = b;
        if r.timed_out() {
            break;
        }
    }
}

/// Start (or reuse) the monitor for `fd`, reading ahead
/// from `file` (`File::try_clone`, safe — the clone shares
/// the description, so the monitor's reads advance the same
/// offset the file object's reads use; `fileio_read` drains
/// the buffer first, keeping the byte order). Returns the
/// shared monitor; a second call for the same live fd
/// returns the same `Arc` (no new thread). A dead monitor
/// is reused only when it still holds undrained bytes AND
/// the fd number still refers to the same description
/// (identity check — an fd number reused for another file
/// gets a fresh monitor, never the old bytes); otherwise a
/// fresh monitor replaces it. The identity check also
/// applies to LIVE monitors: an fd number reused after
/// `close` (the kernel reuses the lowest free number) must
/// not inherit the old monitor — the old one is marked
/// stale and replaced.
pub fn ensure_monitor(fd: i32, file: &File) -> io::Result<Arc<Monitor>> {
    let identity = file_identity(file);
    {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mon) = t.get(&fd) {
            if reusable(mon, identity) {
                return Ok(mon.clone());
            }
            // Not reusable (stale, or the fd number was
            // reused for another description): mark it so it
            // exits, and fall through to a fresh monitor.
            mark_stale(mon);
        }
    }
    let dup = file.try_clone()?;
    let mon = Arc::new(Monitor::new(identity));
    {
        let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
        // A live monitor may have appeared between the check
        // above and this insert (two threads probing one fd):
        // keep the first, drop ours (and its dup). A dead
        // entry with bytes for the SAME description is also
        // kept (its bytes are still drainable).
        if let Some(cur) = t.get(&fd) {
            if reusable(cur, identity) {
                return Ok(cur.clone());
            }
            mark_stale(cur);
        }
        t.insert(fd, mon.clone());
    }
    let thread_mon = mon.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("petitpy-readiness-{fd}"))
        .spawn(move || monitor_loop(fd, dup, thread_mon));
    if let Err(e) = spawn {
        // The thread did not start: drop the table entry so
        // a later call retries instead of returning a monitor
        // that will never fill.
        remove_if_same(fd, &mon);
        return Err(e);
    }
    // A fresh monitor's first read is awaited (bounded) so
    // the caller sees an accurate first probe — see
    // `wait_settled`. Paid once per fd (reuse returns above).
    wait_settled(&mon, SETTLE_DEADLINE);
    Ok(mon)
}

/// The monitor thread: blocking reads into `mon`'s buffer
/// until EOF, a read error, or `stale` (see the module docs
/// for the exit conditions and the residual leak).
fn monitor_loop(fd: i32, mut file: File, mon: Arc<Monitor>) {
    let mut chunk = [0u8; CHUNK];
    loop {
        if mon.stale.load(Ordering::Acquire) {
            break;
        }
        match file.read(&mut chunk) {
            Ok(0) => {
                mon.eof.store(true, Ordering::Release);
                break;
            }
            Ok(n) => {
                // Push with backpressure: park while the
                // buffer is full (woken by `take_buffered`).
                // `wait_while` re-checks `stale`, so a
                // close wakes a parked monitor at once.
                let mut buf = mon
                    .buffer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                buf = mon
                    .space
                    .wait_while(buf, |b| {
                        b.len() >= BUFFER_CAP && !mon.stale.load(Ordering::Acquire)
                    })
                    .unwrap_or_else(|e| e.into_inner());
                if mon.stale.load(Ordering::Acquire) {
                    break;
                }
                buf.extend(chunk[..n].iter().copied());
                // Wake a `fileio_read` waiting for data.
                mon.data.notify_all();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // The fd is nonblocking (opened with
                // `O_NONBLOCK` — file status flags live on the
                // shared description, so the monitor's dup
                // inherits them): no data right now, not an
                // error. Poll again after a short sleep — there
                // is no blocking read to wait on. A writer
                // arriving later is picked up within ~2ms.
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            Err(_) => break,
        }
    }
    mon.dead.store(true, Ordering::Release);
    // Wake any `wait_buffered` waiter: the monitor reached a
    // terminal state (EOF / read error / stale exit).
    mon.data.notify_all();
    // The entry stays while the buffer still holds bytes:
    // removing it would strand them (`take_buffered`
    // looks the monitor up by fd). It is removed by
    // `take_buffered` once the last byte is drained (or
    // here when the buffer is already empty).
    let empty = mon
        .buffer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty();
    if empty {
        remove_if_same(fd, &mon);
    }
}

/// Drain up to `out.len()` buffered bytes for `fd`, in
/// order; returns the number of bytes copied (0 when there
/// is no monitor or its buffer is empty). Wakes a monitor
/// parked on a full buffer. Never touches the OS file.
pub fn take_buffered(fd: i32, out: &mut [u8]) -> usize {
    let mon = {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&fd) {
            Some(mon) => mon.clone(),
            None => return 0,
        }
    };
    let mut buf = mon.buffer.lock().unwrap_or_else(|e| e.into_inner());
    let n = out.len().min(buf.len());
    for (i, slot) in out.iter_mut().take(n).enumerate() {
        *slot = buf[i];
    }
    buf.drain(..n);
    let drained_empty = buf.is_empty();
    let dead = mon.is_dead();
    if n > 0 {
        // Freeing room may unpark the monitor.
        mon.space.notify_all();
    }
    drop(buf);
    // The last byte of a dead monitor drains the entry
    // itself (a live monitor keeps its entry; a dead one
    // with bytes left stays until fully drained).
    if n > 0 && drained_empty && dead {
        remove_if_same(fd, &mon);
    }
    n
}

/// Block until `fd`'s monitor has buffered bytes, hit EOF,
/// or died. Returns `false` when there is no monitor
/// (nothing to wait for — the caller does a plain OS read).
/// A `fileio_read` with a live monitor must wait here when
/// the buffer is empty: returning 0 would be read as EOF by
/// the `stream_rw_loop` (`out_sz == 0` means EOF), while a
/// live monitor may still deliver bytes.
pub fn wait_buffered(fd: i32) -> bool {
    let mon = {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&fd) {
            Some(mon) => mon.clone(),
            None => return false,
        }
    };
    let mut buf = mon.buffer.lock().unwrap_or_else(|e| e.into_inner());
    while buf.is_empty() && !mon.is_eof() && !mon.is_dead() {
        buf = mon
            .data
            .wait(buf)
            .unwrap_or_else(|e| e.into_inner());
    }
    true
}

/// Drain the monitor's read-ahead bytes for `fd` into
/// `buf`, blocking while a live monitor may still deliver
/// more. Returns `(n, need_os)`: `n` bytes drained;
/// `need_os` says whether the caller should read the
/// remainder from the OS object (true only when no live
/// monitor is left to feed the buffer: no monitor, or a
/// dead one). When `need_os` is false the buffer is full or
/// EOF was reached. A live monitor with an empty buffer
/// BLOCKS here until it delivers bytes — a blocking read
/// must wait for data, not report EOF (`stream_rw_loop`
/// reads a 0 return as EOF, and `os.read` returns whatever
/// one read yields), and the monitor is the only reader of
/// the shared offset while it lives (an OS read now would
/// skip the bytes it still holds). Used by both read paths
/// (`fileio_read` and `os.read`).
pub fn drain_into(fd: i32, buf: &mut [u8]) -> (usize, bool) {
    let Some(mon) = monitor_for(fd) else {
        return (0, true);
    };
    let mut n = take_buffered(fd, buf);
    if n == buf.len() {
        return (n, false);
    }
    loop {
        // EOF is terminal: no more bytes will ever arrive.
        if mon.is_eof() {
            return (n, false);
        }
        // Dead (read error / stale): the OS read surfaces
        // the error, or EOF, for the remainder.
        if mon.is_dead() {
            return (n, true);
        }
        // Live monitor, empty buffer: wait for data / EOF /
        // death (blocks — this is a blocking read).
        wait_buffered(fd);
        let m = take_buffered(fd, &mut buf[n..]);
        n += m;
        if n == buf.len() {
            return (n, false);
        }
    }
}

/// The monitor for `fd`, if one is live (dead entries are
/// still returned — the caller checks `is_dead` — so a
/// just-finished monitor's buffered bytes are not lost).
pub fn monitor_for(fd: i32) -> Option<Arc<Monitor>> {
    let t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.get(&fd).cloned()
}

/// Mark `fd`'s monitor stale (the file was closed): it
/// exits after its next read event, or at once when parked
/// on a full buffer. The buffer is cleared: its bytes were
/// read ahead from a description that is now closed, and
/// the file object's read path raises `EBADF` once closed,
/// so no reader can ever drain them — leaving them would
/// let a later file reusing this fd number inherit stale
/// bytes (the `ensure_monitor` identity check cannot tell
/// two opens of the SAME file apart). No-op when there is
/// no monitor. A monitor blocked in `read` cannot be woken
/// — see the module docs.
pub fn note_closed(fd: i32) {
    let mon = {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&fd) {
            Some(mon) => mon.clone(),
            None => return,
        }
    };
    mon.stale.store(true, Ordering::Release);
    {
        let mut buf = mon.buffer.lock().unwrap_or_else(|e| e.into_inner());
        buf.clear();
    }
    mon.space.notify_all();
    mon.data.notify_all();
}

/// Invalidate `fd`'s monitor after a seek: the buffered
/// bytes were read ahead from the OLD offset and are stale
/// now (a seek discards read-ahead). The buffer is cleared
/// under its lock and the monitor is marked stale, so it
/// exits and a later `select` starts a fresh monitor from
/// the new offset. The clear and the stale mark are ordered
/// against the monitor's push (both under the buffer lock,
/// and the monitor re-checks `stale` after acquiring the
/// lock and before extending), so no stale byte can survive
/// the seek. No-op when there is no monitor.
pub fn note_seek(fd: i32) {
    let mon = {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&fd) {
            Some(mon) => mon.clone(),
            None => return,
        }
    };
    mon.stale.store(true, Ordering::Release);
    {
        let mut buf = mon.buffer.lock().unwrap_or_else(|e| e.into_inner());
        buf.clear();
    }
    mon.space.notify_all();
    mon.data.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp file with `data` in it, opened for reading.
    fn temp_file(data: &[u8]) -> (std::path::PathBuf, File) {
        let path = std::env::temp_dir().join(format!(
            "h-select-readiness-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, data).unwrap();
        (path.clone(), File::open(&path).unwrap())
    }

    /// Poll `f` until it is true or `deadline_ms` passes.
    fn poll_until(deadline_ms: u64, mut f: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < u128::from(deadline_ms) {
            if f() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        f()
    }

    /// The fd number keying the monitor: the real fd on
    /// Unix; a synthetic key elsewhere (the monitor only
    /// keys on it — the `File` itself is what it reads).
    #[cfg(unix)]
    fn fd_of(f: &File) -> i32 {
        use std::os::unix::io::AsRawFd;
        f.as_raw_fd()
    }
    #[cfg(not(unix))]
    fn fd_of(_f: &File) -> i32 {
        static NEXT: std::sync::atomic::AtomicI32 =
            std::sync::atomic::AtomicI32::new(1000);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn monitor_reads_file_in_order_then_eof() {
        let (path, file) = temp_file(b"hello readiness");
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        assert!(poll_until(2000, || mon.is_eof()));
        assert_eq!(mon.buffered_len(), 15);
        let mut out = [0u8; 15];
        assert_eq!(take_buffered(fd, &mut out), 15);
        assert_eq!(&out, b"hello readiness");
        assert_eq!(take_buffered(fd, &mut out), 0);
        assert!(mon.is_dead());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn take_buffered_drains_partially_in_order() {
        let (path, file) = temp_file(b"0123456789");
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        assert!(poll_until(2000, || mon.buffered_len() == 10));
        let mut out = [0u8; 4];
        assert_eq!(take_buffered(fd, &mut out), 4);
        assert_eq!(&out, b"0123");
        let mut out = [0u8; 6];
        assert_eq!(take_buffered(fd, &mut out), 6);
        assert_eq!(&out, b"456789");
        assert_eq!(take_buffered(fd, &mut out), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ensure_monitor_reuses_live_monitor() {
        let (path, file) = temp_file(b"shared");
        let fd = fd_of(&file);
        let a = ensure_monitor(fd, &file).unwrap();
        let b = ensure_monitor(fd, &file).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(poll_until(2000, || a.is_eof()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[cfg(unix)]
    fn fresh_monitor_settled_on_eof() {
        // `/dev/null` is at EOF at once: `ensure_monitor`
        // must not return before the monitor's first read
        // has settled, or a timeout-0 `select` would miss
        // it (its first probe runs before the monitor
        // thread's first read). This is the regression test
        // for the `wait_settled` handshake.
        let file = File::open("/dev/null").unwrap();
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        assert!(mon.is_eof() || mon.is_dead());
        assert_eq!(mon.buffered_len(), 0);
    }

    #[test]
    fn buffer_never_exceeds_cap() {
        let data = vec![7u8; 200 * 1024];
        let (path, file) = temp_file(&data);
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        // While the file drains, the buffered length must
        // never pass the cap (the monitor parks when full).
        let start = std::time::Instant::now();
        let mut max_seen = 0usize;
        while start.elapsed().as_millis() < 500 {
            max_seen = max_seen.max(mon.buffered_len());
            if mon.is_eof() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(max_seen <= BUFFER_CAP, "max buffered {max_seen}");
        // Drain everything: the full 200 KiB arrives in order.
        // Bounded: if the monitor stalls, fail fast with the
        // counts instead of spinning forever (this loop hung
        // the suite for 35 minutes before the take_buffered
        // fix — a drain that makes no progress is a bug,
        // not a slow test).
        let mut got = Vec::new();
        let mut out = [0u8; 8192];
        let drain_start = std::time::Instant::now();
        while got.len() < data.len() {
            let n = take_buffered(fd, &mut out);
            if n == 0 {
                assert!(
                    drain_start.elapsed().as_millis() < 10_000,
                    "drain stalled: got {} of {} bytes, buffered {}, eof {}, dead {}",
                    got.len(),
                    data.len(),
                    mon.buffered_len(),
                    mon.is_eof(),
                    mon.is_dead()
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            got.extend_from_slice(&out[..n]);
        }
        assert_eq!(got, data);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn note_closed_on_unknown_fd_is_noop() {
        // Must not panic or create an entry.
        note_closed(-12345);
        assert!(monitor_for(-12345).is_none());
        assert_eq!(take_buffered(-12345, &mut [0u8; 4]), 0);
    }

    #[test]
    fn wait_buffered_returns_when_data_arrives() {
        let (path, file) = temp_file(b"wait for me");
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        // Blocks until the monitor buffers (or EOFs), then
        // returns true — never a false EOF.
        assert!(wait_buffered(fd));
        assert!(mon.buffered_len() > 0 || mon.is_eof());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn note_seek_clears_buffer_and_stales_monitor() {
        let (path, file) = temp_file(b"0123456789");
        let fd = fd_of(&file);
        let mon = ensure_monitor(fd, &file).unwrap();
        assert!(poll_until(2000, || mon.buffered_len() == 10));
        note_seek(fd);
        // Bytes read ahead from the old offset are stale.
        assert_eq!(mon.buffered_len(), 0);
        assert_eq!(take_buffered(fd, &mut [0u8; 10]), 0);
        let _ = std::fs::remove_file(path);
    }

    /// `wait_buffered` blocks until a FIFO writer arrives
    /// (the blocking-read case): a `fileio_read` with a live
    /// monitor and an empty buffer must wait, not report EOF.
    /// Unix-only (needs `mkfifo`). The FIFO is opened
    /// `O_RDWR` — a read-only open would block until a writer
    /// appears, and a nonblocking read-only open reads EOF
    /// while there is no writer at all, so neither exercises
    /// the wait; `O_RDWR` never blocks on open and the pipe
    /// always has a writer (itself), so the monitor's read
    /// genuinely blocks until the writer thread writes.
    #[test]
    #[cfg(unix)]
    fn wait_buffered_blocks_until_fifo_writer() {
        let dir = std::env::temp_dir().join(format!(
            "h-select-fifo-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let fifo = dir.join("fifo");
        let _ = std::fs::remove_file(&fifo);
        let ok = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "mkfifo failed");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .unwrap();
        let fd = fd_of(&file);
        let _mon = ensure_monitor(fd, &file).unwrap();
        // The writer arrives after 50ms.
        let writer = file.try_clone().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut w = writer;
            use std::io::Write;
            let _ = w.write_all(b"fifo-data");
        });
        // Bound the wait: a broken `data` signal would hang
        // the test forever, so wait on a channel with a 2s
        // deadline instead of calling `wait_buffered` inline.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(wait_buffered(fd));
        });
        let waited = rx
            .recv_timeout(std::time::Duration::from_millis(2000))
            .expect("wait_buffered hung: no data signal");
        assert!(waited);
        let mut out = [0u8; 9];
        assert!(poll_until(2000, || take_buffered(fd, &mut out) == 9));
        assert_eq!(&out, b"fifo-data");
        let _ = std::fs::remove_file(&fifo);
        let _ = std::fs::remove_dir(&dir);
    }
}
