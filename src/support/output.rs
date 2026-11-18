//! Where Python's stdout (fd 1) and stderr (fd 2) go
//! (`support/output.zig`).
//!
//! The Zig writes to the file descriptor, or to the function an
//! embedding host gives in its configuration (ADR 022), which
//! then receives every write in order. The only in-tree caller
//! (`file_io.zig:169`) passes fd 1 or 2.

use std::cell::Cell;
use std::io::Write;

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
pub fn write(fd: i32, data: &[u8], sink: Option<&mut dyn OutputSink>) {
    if let Some(s) = sink {
        s.write(fd, data);
        return;
    }
    match fd {
        1 => {
            let _ = std::io::stdout().write_all(data);
        }
        2 => {
            let _ = std::io::stderr().write_all(data);
        }
        _ => {}
    }
}
