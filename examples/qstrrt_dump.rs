//! Static qstr table dump, runtime track vs compiler track.
//!
//! Prints the whole static pool twice — once through the runtime
//! accessors (`RuntimeQstr`, ids 0..N_TOTAL) and once through the
//! compiler accessors (`QstrContext`) — one line per entry,
//! `id<TAB>len<TAB>hash<TAB>hexbytes`, under `runtime:` / `compiler:`
//! section headers. `scripts/check_qstrrt_table.py` diffs the two
//! sections against each other and against an independent parse of
//! `vendor/qstrdefs.generated.h`.
//!
//! Usage: `cargo run --release --example qstrrt_dump`

use petitpy_rs::compiler::qstr::QstrContext;
use petitpy_rs::qstr_table::N_TOTAL;
use petitpy_rs::runtime::qstr::RuntimeQstr;
use std::io::Write;
use std::process::ExitCode;

fn write_hex(out: &mut impl Write, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        let _ = out.write_all(&[HEX[(b >> 4) as usize], HEX[(b & 0x0f) as usize]]);
    }
}

fn main() -> ExitCode {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let rt = RuntimeQstr::new();
    let _ = writeln!(out, "runtime:");
    for id in 0..N_TOTAL {
        let _ = write!(out, "{}\t{}\t{}\t", id, rt.qstr_len(id), rt.qstr_hash(id));
        write_hex(&mut out, rt.qstr_str(id));
        let _ = writeln!(out);
    }

    let ctx = QstrContext::new();
    let _ = writeln!(out, "compiler:");
    for id in 0..N_TOTAL {
        let _ = write!(out, "{}\t{}\t{}\t", id, ctx.qstr_len(id), ctx.qstr_hash(id));
        write_hex(&mut out, ctx.qstr_str(id));
        let _ = writeln!(out);
    }

    ExitCode::SUCCESS
}
