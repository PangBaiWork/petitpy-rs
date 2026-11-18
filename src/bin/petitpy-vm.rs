//! `petitpy-vm` — the VM-only entry point (`src/main_vm.zig`, E1).
//!
//! Reads argv[1] as a `.mpy` file and executes it. The compiler
//! track is deliberately NOT linked here: `eval` / `exec` /
//! `compile` and `.py` imports raise
//! `NotImplementedError("compiler not linked in petitpy-vm")`
//! (`VmState.compiler_linked = false`); `.mpy` imports still
//! work. The printed program name and version are the
//! ORIGINAL's (`petitpy-vm 0.9.0`).
//!
//! Dispatch (`main_vm.zig`, in order):
//!   no args / `--help` / `-h` → usage on stderr, exit 0
//!   `--version` / `-V`         → `petitpy-vm 0.9.0` on stdout, exit 0
//!   `<script.mpy>`             → load + run as `__main__`
//!                               (unreadable → `error: <Name>`
//!                               on stderr, exit 1; extra args
//!                               ignored)

use std::io::Write as _;
use std::process::ExitCode;

use petitpy_rs::entry::embed_run_mpy::run_mpy;
use petitpy_rs::entry::io_error_name;
use petitpy_rs::entry::VERSION;
use petitpy_rs::runtime::vm::Vm;

/// The usage text (`printUsage`, `main_vm.zig`) — to STDERR,
/// exit 0. Byte-for-byte the original's, including the
/// trailing blank line and the em-dash.
/// Usage text (`main_vm.zig`) — same Zig multiline-literal
/// rule as the main binary: ends with a single `\n`
/// (212 bytes, verified against the reference).
const USAGE: &str = "petitpy-vm — VM-only runtime (loads pre-compiled .mpy files).\n\nUsage:\n  petitpy-vm <script.mpy>   run a compiled bytecode file\n  petitpy-vm --version      print version\n  petitpy-vm --help         this message\n";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let arg1 = argv.get(1).map(String::as_str);

    if argv.len() < 2 || matches!(arg1, Some("--help") | Some("-h")) {
        let _ = std::io::stderr().write_all(USAGE.as_bytes());
        return ExitCode::from(0);
    }
    if matches!(arg1, Some("--version") | Some("-V")) {
        let _ = std::io::stdout()
            .write_all(format!("petitpy-vm {VERSION}\n").as_bytes());
        return ExitCode::from(0);
    }

    let path = &argv[1];
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::io::stderr()
                .write_all(format!("error: {}\n", io_error_name(&e)).as_bytes());
            return ExitCode::from(1);
        }
    };

    let mut vm = Vm::new();
    vm.state.compiler_linked = false;
    ExitCode::from(run_mpy(&mut vm, &buf) as u8)
}
