//! Where Python's stdout (fd 1) and stderr (fd 2) go
//! (`support/output.zig`).
//!
//! The Zig writes to the file descriptor, or to the function an
//! embedding host gives in its configuration (ADR 022), which
//! then receives every write in order. The only in-tree caller
//! (`file_io.zig:169`) passes fd 1 or 2.

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
