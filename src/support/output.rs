//! Where Python's stdout (fd 1) and stderr (fd 2) go
//! (`support/output.zig`).
//!
//! The Zig writes to the file descriptor, or to the function an
//! embedding host gives in its configuration (ADR 022), which
//! then receives every write in order. The only in-tree caller
//! (`file_io.zig:169`) passes fd 1 or 2.

use std::cell::Cell;
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, OnceLock};

/// Host-provided output sink (`output.zig` `Sink`): a write
/// callback an embedding host supplies. The Zig's sink is a C
/// fn pointer + userdata; the Rust form is a plain trait object
/// (no C FFI, no `unsafe`). The sink itself is NOT stored in a
/// global — the Zig's `pub var sink` is interpreter state set
/// by `ppy_new` / cleared by `ppy_free`; in Rust the sink is
/// passed explicitly to `write` (a `VmState` field for it is
/// Phase F's, when the embedding API lands).
pub trait OutputSink {
    fn write(&mut self, fd: i32, data: &[u8]);
}

/// The embedding host's output sink for THIS thread (F1, ADR
/// 022): the C write callback + its userdata from the
/// interpreter's `ppy_config`. The Zig keeps `output.sink` as a
/// process-wide global that `run` saves/restores per
/// interpreter; the Rust form is a thread-local installed by
/// the embedding API bracket (`entry::api`) on entry and
/// restored on exit — one per thread, so concurrent calls into
/// different interpreters from different threads each see
/// their own sink. A `VmState` field was considered and
/// rejected: the write sites (`fileio_write`'s `StreamP` fns,
/// `print_impl_with`'s no-file path) are ctx-only and cannot
/// see `VmState`. `None` outside an API call (the CLI
/// default).
pub type PpyWriteFn = unsafe extern "C" fn(*mut std::ffi::c_void, i32, *const u8, usize);

/// A host output sink: the C write callback + its userdata.
/// Both halves are `Copy`, so the thread-local can hold it in
/// a `Cell`. The host guarantees the callback stays callable
/// and the userdata stays valid while the interpreter lives
/// (the `ppy_config` contract, `include/petitpy.h`).
#[derive(Clone, Copy)]
pub struct CSink {
    pub write: PpyWriteFn,
    pub userdata: *mut std::ffi::c_void,
}

thread_local! {
    static THREAD_SINK: Cell<Option<CSink>> = const { Cell::new(None) };
}

/// Install `sink` as this thread's sink, returning the previous
/// one (the API bracket restores it on drop).
pub fn set_thread_sink(sink: Option<CSink>) -> Option<CSink> {
    THREAD_SINK.with(|t| t.replace(sink))
}

/// Write `data` to this thread's sink as `fd`; true when a sink
/// consumed it, false when no sink is installed (the caller
/// falls back to the process stdout/stderr).
/// SAFETY: the sink was installed by the embedding API bracket
/// from a live interpreter's config; the host upholds the
/// `ppy_config` contract (callback + userdata valid while the
/// interpreter lives), and the bracket only installs it for the
/// duration of an API call on this thread.
pub fn write_sink(fd: i32, data: &[u8]) -> bool {
    THREAD_SINK.with(|t| match t.get() {
        Some(s) => {
            // FFI invariant: the sink was installed by the
            // embedding API bracket from a live interpreter's
            // config (callback + userdata valid, see above).
            unsafe { (s.write)(s.userdata, fd, data.as_ptr(), data.len()) };
            true
        }
        None => false,
    })
}

/// `output.write(fd, data)` — with a sink, route to it;
/// otherwise fd 1 → stdout, fd 2 → stderr (plain `write(1/2,
/// ...)`; errors ignored, exactly as the Zig discards the
/// `write` return). Any other fd is ignored: there is no safe
/// `std` API for an arbitrary fd, and no in-tree caller passes
/// one. This is the same operation as C8c's `MP_PLAT_PRINT`
/// (fd 1) / `MP_STDERR_PRINT` (fd 2) — those keep their own
/// paths (their tests pin them); the behaviour is identical.
///
/// NOTE (P14): fd 1 goes through the buffered stdout
/// (`buffered_stdout_write`), NOT a plain write — the
/// reference's stdout is libc's `stdout` FILE* (`fwrite`,
/// glibc stdio buffering). fd 2 stays a plain write: glibc
/// stderr is unbuffered by default, and the reference's is
/// unbuffered on both sides (G5 scope probes).
pub fn write(fd: i32, data: &[u8], sink: Option<&mut dyn OutputSink>) {
    if let Some(s) = sink {
        s.write(fd, data);
        return;
    }
    match fd {
        1 => {
            buffered_stdout_write(data);
        }
        2 => {
            let _ = std::io::stderr().write_all(data);
        }
        _ => {}
    }
}

// ── Buffered stdout (P14) ────────────────────────────────────────────

/// glibc stdio's default buffer size for a non-tty stream
/// (also the `BufWriter` capacity G5's P3 used for files).
const STDOUT_BUF_CAP: usize = 4096;

/// The process-global stdout buffer: bytes reach `std`'s
/// `Stdout` only on flush. `std::io::stdout()` is a
/// `LineWriter` — it flushes on every `\n` even when piped,
/// which is exactly the write-through this fixes; the buffer
/// must sit ABOVE it. `tty` is cached once (fd 1 does not
/// change target mid-process).
struct StdoutBuffer {
    buf: Vec<u8>,
    tty: bool,
}

static STDOUT_BUFFER: OnceLock<Mutex<StdoutBuffer>> = OnceLock::new();

fn stdout_buffer() -> &'static Mutex<StdoutBuffer> {
    STDOUT_BUFFER.get_or_init(|| {
        Mutex::new(StdoutBuffer {
            buf: Vec::new(),
            tty: std::io::stdout().is_terminal(),
        })
    })
}

/// Write one chunk out through `std`'s stdout, errors
/// ignored (the Zig's `output.write` discards the write
/// return). Always called WITHOUT the buffer lock held (the
/// chunk was drained first), so no lock ordering issue.
fn emit_stdout(chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(chunk);
    let _ = lock.flush();
}

/// Append `data` to the stdout buffer, flushing per glibc
/// stdio rules: when fd 1 is a terminal, flush through the
/// last `\n` (line-buffered); otherwise flush 4096-byte
/// chunks as the buffer fills (fully-buffered). Bytes left
/// in the buffer surface at the next flush — an explicit
/// `flush_stdout_buffer()` or process exit (`main` /
/// `petitpy-vm` flush at the end).
pub fn buffered_stdout_write(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    // Append once, then drain what glibc stdio would flush:
    // on a tty (line-buffered) everything through the last
    // `\n`, then any full 4096-byte chunks left over (a
    // newline-less write may overflow the capacity); on a
    // pipe/file (fully-buffered) full 4096-byte chunks as the
    // buffer fills. The remainder stays buffered until an
    // explicit flush or process exit.
    let mut pending: Option<Vec<u8>> = Some(data.to_vec());
    loop {
        let chunk = {
            let mut guard = stdout_buffer().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(d) = pending.take() {
                guard.buf.extend_from_slice(&d);
            }
            if guard.tty {
                if let Some(pos) = guard.buf.iter().rposition(|b| *b == b'\n') {
                    guard.buf.drain(..=pos).collect::<Vec<u8>>()
                } else if guard.buf.len() >= STDOUT_BUF_CAP {
                    guard.buf.drain(..STDOUT_BUF_CAP).collect::<Vec<u8>>()
                } else {
                    return;
                }
            } else if guard.buf.len() >= STDOUT_BUF_CAP {
                guard.buf.drain(..STDOUT_BUF_CAP).collect::<Vec<u8>>()
            } else {
                return;
            }
        };
        emit_stdout(&chunk);
        // A huge write may have left further full chunks (or,
        // on a tty, further complete lines) behind — loop until
        // nothing more is flushable. `pending` is None now, so
        // the next pass only drains.
        let more = {
            let guard = stdout_buffer().lock().unwrap_or_else(|e| e.into_inner());
            if guard.tty {
                guard.buf.iter().any(|b| *b == b'\n') || guard.buf.len() >= STDOUT_BUF_CAP
            } else {
                guard.buf.len() >= STDOUT_BUF_CAP
            }
        };
        if !more {
            return;
        }
    }
}

/// Flush whatever the stdout buffer holds. Called by
/// `sys.stdout.flush()`, `print(flush=True)` (no-file path),
/// the pre-traceback flush (`embed_exec.rs`), and once at
/// process exit (`main` / `petitpy-vm`). The embedding API
/// does NOT flush per call — the reference's FILE* buffer
/// persists across API calls.
pub fn flush_stdout_buffer() {
    let chunk = {
        let mut guard = stdout_buffer().lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut guard.buf)
    };
    emit_stdout(&chunk);
}
