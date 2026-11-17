//! CLI port of `src/compiler/mpy_roundtrip.zig`.
//!
//! Usage: mpy-roundtrip <input.mpy> [output.mpy]
//! Exit 0 on match, 1 on mismatch, 2 on read/infra error — the same
//! codes and messages as the Zig CLI. The optional second argument is
//! NOT in the Zig CLI: it writes the re-serialized bytes to a file, so
//! the write check can feed Rust-written .mpy files to the original
//! binary (`petitpy-vm`). With one argument the behavior is exactly the
//! Zig CLI's.

use petitpy_rs::compiler::mpy;
use petitpy_rs::compiler::mpy_roundtrip::{self, Roundtrip};
use std::io::Write;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 {
        let _ = writeln!(std::io::stderr(), "usage: mpy-roundtrip <input.mpy>");
        std::process::exit(2);
    }
    let data = match std::fs::read(&argv[1]) {
        Ok(d) => d,
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "fopen failed");
            std::process::exit(2);
        }
    };
    match mpy_roundtrip::check(&data) {
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "mpy.read failed: {e}");
            std::process::exit(2);
        }
        Ok(Roundtrip::Match { bytes }) => {
            if argv.len() > 2 {
                // Re-derive the re-serialized bytes for the output file.
                let m = mpy::read(&data).expect("read succeeded in check()");
                let out = mpy::write(&m);
                if std::fs::write(&argv[2], &out).is_err() {
                    let _ = writeln!(std::io::stderr(), "fopen failed");
                    std::process::exit(2);
                }
            }
            println!("OK round-trip: {} bytes, {}", bytes, argv[1]);
            std::process::exit(0);
        }
        Ok(Roundtrip::Mismatch {
            in_len,
            out_len,
            first_diff,
        }) => {
            let _ = writeln!(
                std::io::stderr(),
                "MISMATCH: in={in_len} bytes, out={out_len} bytes, {}",
                argv[1]
            );
            if let Some(i) = first_diff {
                let m = mpy::read(&data).expect("read succeeded in check()");
                let out = mpy::write(&m);
                let _ = writeln!(
                    std::io::stderr(),
                    "  first byte diff at offset {i}: in=0x{:0>2x} out=0x{:0>2x}",
                    data[i], out[i]
                );
            }
            std::process::exit(1);
        }
    }
}
