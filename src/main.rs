//! `petitpy` — the CLI entry point (`src/main.zig`, E1).
//!
//! Thin shell over `petitpy_rs::entry`: argument dispatch,
//! VM creation, and process exit codes. The printed program
//! name and version are the ORIGINAL's (`petitpy 0.9.0`)
//! even though this binary is `petitpy-rs`.
//!
//! Dispatch (`main.zig`, in order):
//!   `--version` / `-V` → `petitpy 0.9.0` on stdout, exit 0
//!   `--help` / `-h`    → usage on stderr, exit 0
//!   `--emit-mpy F.py`  → base64 `.mpy` on stdout (exit 0;
//!                        1 unreadable input, 2 compile failure
//!                        after re-running through the exec path
//!                        for the diagnostic; no file → usage
//!                        on stderr, exit 2)
//!   no args            → REPL (banner on stderr, `>>> ` prompt)
//!   `-c CODE [ARG...]` → run CODE as `__main__`, source name
//!                        `"<stdin>"`, sys.argv `['-c', ARG...]`
//!                        (no CODE → stderr + exit 2)
//!   `script.py [ARG...]` → run the script as `__main__`
//!                        (unreadable → stderr + exit 2)

use std::io::Write as _;
use std::process::ExitCode;

use petitpy_rs::entry::io_error_name;
use petitpy_rs::entry::embed_exec::{
    run_repl, run_source, wire_env_sys_path, wire_sys_argv,
};
use petitpy_rs::entry::VERSION;
use petitpy_rs::runtime::vm::Vm;

/// The usage text (`printUsage`, `main.zig`) — to STDERR,
/// exit 0. Byte-for-byte the original's (369 bytes): Zig
/// multiline literals join lines with `\n` and add NO trailing
/// newline, so the final empty `\\` line only supplies the
/// `\n` after "this message" — there is NO trailing blank line.
const USAGE: &str = "petitpy — a small Python runtime, MicroPython-compatible, written in Zig.\n\nUsage:\n  petitpy <script.py>       run a Python script (`ppy` is the same command)\n  petitpy -c CODE           run CODE (sys.argv[0] is '-c')\n  petitpy --emit-mpy F.py   compile only; print the .mpy as base64\n  petitpy --version         print version\n  petitpy --help            this message\n";

/// `emitMpy` (`main.zig`): compile `in_path` and print the
/// `.mpy` to stdout as base64. Returns a process exit status:
/// 0 compiled, 1 could not read the input, 2 the compiler
/// refused (re-run through the exec path for the diagnostic).
/// The VM is initialised first (the compiler interns qstrs
/// into the VM's pool).
///
/// G2 (P7a): `in_path` is an `OsStr` — a non-UTF-8 file name
/// must open, not panic in `std::env::args()`.
fn emit_mpy(vm: &mut Vm, in_path: &std::ffi::OsStr) -> u8 {
    let src = match std::fs::read(in_path) {
        Ok(s) => s,
        Err(_) => {
            let _ = std::io::stderr().write_all(b"petitpy: cannot read input\n");
            return 1;
        }
    };
    match petitpy_rs::compiler::bridge::compile_to_mpy_named(
        match std::str::from_utf8(&src) {
            Ok(s) => s,
            Err(_) => {
                // Non-UTF-8 source: the compiler refuses (the
                // Zig's lexer fails at the first undecodable
                // byte) — re-run through the exec path for the
                // diagnostic, as for any compile failure.
                let _ = run_source(vm, &src, None);
                return 2;
            }
        },
        petitpy_rs::compiler::parser::InputKind::FileInput,
        "<emit-mpy>",
    ) {
        Ok(mpy) => {
            let b64 = petitpy_rs::entry::base64::encode(&mpy);
            let mut stdout = std::io::stdout();
            if stdout.write_all(b64.as_bytes()).is_err()
                || stdout.write_all(b"\n").is_err()
            {
                return 1;
            }
            0
        }
        Err(_) => {
            // The bridge collapses the compiler's error into a
            // code, so the message is not reachable from here.
            // Re-running the source through the normal exec path
            // prints the diagnostic — and cannot execute
            // anything, because the compile that just failed
            // fails there too. The re-run's own status is
            // discarded: `emitMpy` returns 2 for any compile
            // failure.
            let _ = run_source(vm, &src, None);
            2
        }
    }
}

fn main() -> ExitCode {
    // G2 (P7a): `args_os`, not `args` — `std::env::args()`
    // PANICS (unwrap in env.rs) on a non-UTF-8 argument,
    // while the reference runs a `0xff` script name fine.
    // Everything downstream is byte-based (`OsStr` /
    // `as_encoded_bytes`), so no lossy conversion happens
    // anywhere on the path from argv to `__file__` /
    // `sys.argv` / `sys.path[0]`.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let arg1 = argv.get(1).map(|s| s.as_os_str());
    let is = |s: &str| arg1 == Some(std::ffi::OsStr::new(s));

    if is("--version") || is("-V") {
        let _ = std::io::stdout()
            .write_all(format!("petitpy {VERSION}\n").as_bytes());
        return ExitCode::from(0);
    }
    if is("--help") || is("-h") {
        let _ = std::io::stderr().write_all(USAGE.as_bytes());
        return ExitCode::from(0);
    }

    // `--emit-mpy IN.py` — compile and print the `.mpy` as
    // base64, without running the program.
    if is("--emit-mpy") {
        let Some(in_path) = argv.get(2) else {
            let _ = std::io::stderr()
                .write_all(b"usage: petitpy --emit-mpy IN.py\n");
            return ExitCode::from(2);
        };
        let mut vm = Vm::new();
        return ExitCode::from(emit_mpy(&mut vm, in_path.as_os_str()));
    }

    // No args → interactive REPL.
    if argv.len() < 2 {
        let mut vm = Vm::new();
        wire_env_sys_path(&mut vm);
        return ExitCode::from(run_repl(&mut vm) as u8);
    }

    // `-c CODE [ARG...]`: sys.argv is ['-c', ARG...].
    if is("-c") {
        let Some(code) = argv.get(2) else {
            let _ = std::io::stderr().write_all(
                b"petitpy: argument expected for the -c option\n",
            );
            return ExitCode::from(2);
        };
        let mut vm = Vm::new();
        wire_env_sys_path(&mut vm);
        let sys_argv: Vec<Vec<u8>> = std::iter::once(b"-c".to_vec())
            .chain(
                argv.iter()
                    .skip(3)
                    .map(|a| a.as_encoded_bytes().to_vec()),
            )
            .collect();
        wire_sys_argv(&mut vm, &sys_argv);
        return ExitCode::from(run_source(&mut vm, code.as_encoded_bytes(), None) as u8);
    }

    // Read the script as a buffer and run it as `__main__`.
    let script = &argv[1];
    let src = match std::fs::read(script) {
        Ok(s) => s,
        Err(e) => {
            // Byte-exact error line: the file name is raw
            // argv bytes (the reference prints them as-is).
            let mut msg = b"petitpy: can't open file '".to_vec();
            msg.extend_from_slice(script.as_encoded_bytes());
            msg.extend_from_slice(b"': ");
            msg.extend_from_slice(io_error_name(&e).as_bytes());
            msg.push(b'\n');
            let _ = std::io::stderr().write_all(&msg);
            return ExitCode::from(2);
        }
    };
    let mut vm = Vm::new();
    wire_env_sys_path(&mut vm);
    let sys_argv: Vec<Vec<u8>> = argv
        .iter()
        .skip(1)
        .map(|a| a.as_encoded_bytes().to_vec())
        .collect();
    wire_sys_argv(&mut vm, &sys_argv);
    ExitCode::from(run_source(&mut vm, &src, Some(script.as_encoded_bytes())) as u8)
}
