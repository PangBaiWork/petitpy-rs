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
//!   still refers to the same description (the `(dev, ino)`
//!   identity check) — otherwise it starts fresh.
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
//! What feeds a monitor: only a `File` the caller already
//! holds. A monitor is started from a file *object* (the
//! arena's `Obj::File` carries its own `Rc<File>`). Bare fd
//! ints (`os.pipe`, `os.open`) and socket objects cannot be
//! resolved to a handle from a plain `BuiltinFn` — the os
//! fd table lives in `VmState` (reachable only through the
//! `call_function_ctx` interception) and the socket table
//! is private to `modsocket` — so `select` cannot start a
//! monitor for them; see `scripts/h_select_result.txt` §4
//! for the exact unblock proposal. There is no safe `std`
//! API for an arbitrary fd (`BorrowedFd::borrow_raw` is
//! `unsafe`, and `UnixStream::peek` is unstable), and
//! `/proc/self/fd` is Linux-only and refuses sockets, so
//! no workaround exists within pure `std`.

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

/// One fd's shared monitor state (see the module docs).
pub struct Monitor {
    /// Bytes read ahead, in order; drained by `take_buffered`.
    buffer: Mutex<VecDeque<u8>>,
    /// Signalled when `take_buffered` frees room (the
    /// monitor waits here while the buffer is full).
    space: Condvar,
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
        use std::os::windows::fs::MetadataExt;
        Some((md.file_index(), 0))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = md;
        None
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
/// fresh monitor replaces it.
pub fn ensure_monitor(fd: i32, file: &File) -> io::Result<Arc<Monitor>> {
    let identity = file_identity(file);
    {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mon) = t.get(&fd) {
            if !mon.is_dead() {
                return Ok(mon.clone());
            }
            if mon.buffered_len() > 0
                && mon.identity.is_some()
                && mon.identity == identity
            {
                return Ok(mon.clone());
            }
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
            if !cur.is_dead() {
                return Ok(cur.clone());
            }
            if cur.buffered_len() > 0
                && cur.identity.is_some()
                && cur.identity == identity
            {
                return Ok(cur.clone());
            }
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
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    mon.dead.store(true, Ordering::Release);
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

/// The monitor for `fd`, if one is live (dead entries are
/// still returned — the caller checks `is_dead` — so a
/// just-finished monitor's buffered bytes are not lost).
pub fn monitor_for(fd: i32) -> Option<Arc<Monitor>> {
    let t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.get(&fd).cloned()
}

/// Mark `fd`'s monitor stale (the file was closed): it
/// exits after its next read event, or at once when parked
/// on a full buffer. No-op when there is no monitor.
/// A monitor blocked in `read` cannot be woken — see the
/// module docs.
pub fn note_closed(fd: i32) {
    let mon = {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&fd) {
            Some(mon) => mon.clone(),
            None => return,
        }
    };
    mon.stale.store(true, Ordering::Release);
    mon.space.notify_all();
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
}
