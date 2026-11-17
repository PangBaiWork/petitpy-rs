//! zigpy-compile — port of `src/compiler/main.zig` (323 lines, 0 test blocks).
//!
//! The user-facing pure-Rust `.py` → `.mpy` compiler. CLI shape mirrors
//! `mpy-cross` (main.zig:1-11):
//!
//! ```text
//! zigpy-compile [opts] <input.py> [<input2.py> ...]
//!   -o <file>      explicit output (only valid with exactly one input)
//!   -s <name>      override embedded source-file name (default: "")
//!   -O3            optimisation level (default and only supported;
//!                  -O0/-O1/-O2 accepted but map to -O3 in nano)
//!   -v, --version  print version
//!   -h, --help     print usage
//! ```
//!
//! Exit codes (match mpy-cross convention, main.zig:13-17):
//!   0 — all inputs compiled and written cleanly
//!   1 — compile error (SyntaxError / IndentationError / ImportError)
//!   2 — infrastructure error (cannot read input, cannot write output,
//!                             OOM, bad flag, -o with multiple inputs)
//!
//! Compilation goes through Phase B's bridge
//! (`compile_to_mpy_named_debug`, lexer → parser → compile →
//! mpy.write); no second pipeline exists. The lexer `debug_mode` is
//! `true`, exactly as main.zig:238 passes it — that flag turns the
//! `__debug__` name into a `KW_TRUE` literal (lexer.zig:1023), and the
//! oracle zigpy-compile emits `True` where the bridge path (used by
//! `petitpy --emit-mpy`) emits `False`. The original's own two entry
//! points disagree here; this CLI matches main.zig, its own source.
//! See `scripts/cli_result.txt` for the evidence.
//!
//! Port notes (all deviations from main.zig are deliberate and listed):
//! - `VERSION` is inlined here, exactly as main.zig does (it deliberately
//!   does NOT import build_options; refresh at release time).
//! - argv is handled as raw bytes (`Vec<u8>` per argument), mirroring
//!   Zig's `[:0]const u8` argv. Paths are never lossy-converted.
//! - `parse_args` returns `Result<Args, ArgFlow>` instead of exiting
//!   directly (main.zig exits inside); `main` performs the prints and
//!   exits. Behaviour is identical; this only makes the parser unit-
//!   testable without spawning a process.
//! - `print_err_line` keeps Zig's 512-byte buffer cap: a formatted line
//!   longer than 512 bytes is silently dropped (main.zig: `bufPrint`
//!   fails with NoSpaceLeft and the function returns). `die_infra`
//!   writes directly and is uncapped, as in Zig.
//! - The bridge collapses the pipeline stages into one `CompileError`
//!   (and maps `ParseError::DedentMismatch` to `IndentationError`), so
//!   the CLI cannot tell a parse-stage failure from a compile-stage
//!   one. It prints the parse-stage form for both: `SyntaxError` ->
//!   `parse error: error.SyntaxError`, `IndentationError` ->
//!   `parse error: error.IndentationError`. That matches Zig for every
//!   parse-stage failure (the common case); it differs for compile-
//!   stage `SyntaxError` (e.g. `1 = 2`, multiple starred targets — Zig
//!   prints bare `SyntaxError`) and for `DedentMismatch` (Zig prints
//!   `parse error: error.DedentMismatch`). Exit codes match (1) in all
//!   cases. See `scripts/cli_result.txt` for the oracle evidence.
//! - `bridge::compile_to_mpy_named` takes `&str` source, but the Zig
//!   lexer works on raw bytes: a source file that is not valid UTF-8
//!   cannot go through the bridge. It is reported as a compile failure
//!   (`parse error: error.SyntaxError`, exit 1). The oracle compiles
//!   raw non-UTF-8 bytes in string literals fine (verified); no corpus
//!   file is affected (all 552 are valid UTF-8). Likewise a non-UTF-8
//!   `-s` value exits 2 with an explicit message. Both are documented
//!   in `scripts/cli_result.txt`.
//! - A directory given as input is read as an empty source. That is the
//!   oracle's observed behaviour (`fopen` succeeds, `ftell` returns 0,
//!   verified: directory input produces the 14-byte empty-module .mpy).
//! - Reading mirrors Zig's `fopen`/`fseek`/`ftell`/`fread` sequence via
//!   `File::open` + metadata length + `read_exact`, so `/proc`-style
//!   files (reported size 0) also read as empty, as in Zig.
//! - Rust aborts on allocation failure (`panic = "abort"`), so Zig's
//!   `out of memory ...` messages are not reproducible; there is no
//!   code path that can print them.

use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process;

use petitpy_rs::compiler::bridge::compile_to_mpy_named_debug;
use petitpy_rs::compiler::compile::CompileError;
use petitpy_rs::compiler::parser::InputKind;

// NOTE: deliberately NOT read from build_options (main.zig header
// comment). The version string is small enough to inline; refresh it
// at release time.
const VERSION: &str = "0.1.0-dev";

/// Usage text, verbatim from main.zig:128-131. Raw string: a `\`
/// line-continuation would strip the two-space option indents.
const USAGE: &str = r#"usage: zigpy-compile [opts] <input.py> [<input2.py> ...]

options:
  -o <file>      explicit output path (only valid with one input)
  -s <name>      override embedded source-file name (default: "")
  -O<n>          optimisation level (only -O3 is meaningful; default 3)
  -v, --version  print version and exit
  -h, --help     print this help and exit

Default output: <input>.mpy alongside <input>.py.
"#;

/// Maximum path length the Zig side accepts (4096-byte NUL-terminated
/// buffer, room for the NUL: `path.len + 1 > buf.len` fails).
const MAX_PATH_LEN: usize = 4095;

/// `print_line` / `print_err_line` format into a 512-byte buffer in Zig;
/// overflow silently drops the whole line.
const PRINT_BUF_SIZE: usize = 512;

// ---------------------------------------------------------------------------
// I/O helpers (same convention as the other compiler CLIs).
// ---------------------------------------------------------------------------

fn write_stdout(bytes: &[u8]) {
    let _ = io::stdout().write_all(bytes);
    let _ = io::stdout().flush();
}

fn write_stderr(bytes: &[u8]) {
    let _ = io::stderr().write_all(bytes);
    let _ = io::stderr().flush();
}

/// Zig `print_err_line`: format into a 512-byte buffer; on overflow the
/// line is silently dropped.
fn print_err_line(line: &[u8]) {
    if line.len() <= PRINT_BUF_SIZE {
        write_stderr(line);
    }
}

fn print_usage() {
    write_stdout(USAGE.as_bytes());
}

fn print_version() {
    let line = format!("zigpy-compile (petitpy {VERSION}) -- emitting mpy v6\n");
    // Short fixed line; the 512-byte cap never triggers, but keep the
    // same helper shape as `print_line` in main.zig.
    if line.len() <= PRINT_BUF_SIZE {
        write_stdout(line.as_bytes());
    }
}

/// Zig `die_infra`: uncapped direct writes, exit 2.
fn die_infra(msg: &[u8]) -> ! {
    write_stderr(b"zigpy-compile: error: ");
    write_stderr(msg);
    write_stderr(b"\n");
    process::exit(2);
}

// ---------------------------------------------------------------------------
// CLI parsing.
// ---------------------------------------------------------------------------

/// Parsed arguments (main.zig `Args`).
struct Args {
    inputs: Vec<Vec<u8>>,
    output: Option<Vec<u8>>,
    source_name: Option<Vec<u8>>,
    // Recorded for completeness only: only -O3 affects codegen (see the
    // -O branch in `parse_args`), exactly as in main.zig.
    #[allow(dead_code)]
    opt_level: u8,
}

/// What `parse_args` wants the caller to do instead of returning `Args`.
/// `main` performs the print + exit; the parser itself never exits, so
/// it stays unit-testable.
#[derive(Debug)]
enum ArgFlow {
    /// Print usage to stdout, exit 0 (`-h` / `--help`).
    Help,
    /// Print version to stdout, exit 0 (`-v` / `--version`).
    Version,
    /// Print usage to stdout, exit 2 (no inputs).
    NoInputs,
    /// `print_err_line` (512-byte capped), exit 2 (unknown option).
    /// Carries the full line including the trailing `\n`.
    ErrorLine(Vec<u8>),
    /// `die_infra` message (uncapped, no prefix), exit 2.
    DieMsg(Vec<u8>),
}

fn unknown_option_line(a: &[u8]) -> Vec<u8> {
    let mut line = b"zigpy-compile: error: unknown option: ".to_vec();
    line.extend_from_slice(a);
    line.push(b'\n');
    line
}

/// Parse argv[1..]. Mirrors main.zig `parse_args` branch for branch,
/// including the order of checks (no-inputs before -o-with-multiple).
fn parse_args(argv: &[Vec<u8>]) -> Result<Args, ArgFlow> {
    let mut args = Args {
        inputs: Vec::new(),
        output: None,
        source_name: None,
        opt_level: 3,
    };

    let mut i: usize = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == b"-h" || a == b"--help" {
            return Err(ArgFlow::Help);
        } else if a == b"-v" || a == b"--version" {
            return Err(ArgFlow::Version);
        } else if a == b"-o" {
            i += 1;
            if i >= argv.len() {
                return Err(ArgFlow::DieMsg(b"-o requires an argument".to_vec()));
            }
            if args.output.is_some() {
                return Err(ArgFlow::DieMsg(b"-o specified twice".to_vec()));
            }
            args.output = Some(argv[i].clone());
        } else if a == b"-s" {
            i += 1;
            if i >= argv.len() {
                return Err(ArgFlow::DieMsg(b"-s requires an argument".to_vec()));
            }
            args.source_name = Some(argv[i].clone());
        } else if a.len() >= 2 && a[0] == b'-' && a[1] == b'O' {
            // -O, -O0, -O1, -O2, -O3 — all accepted, but only -O3 affects
            // codegen in nano. We record the level for completeness.
            if a.len() == 2 {
                args.opt_level = 3;
            } else if a.len() == 3 && (b'0'..=b'3').contains(&a[2]) {
                args.opt_level = a[2] - b'0';
            } else {
                return Err(ArgFlow::ErrorLine(unknown_option_line(a)));
            }
        } else if !a.is_empty() && a[0] == b'-' {
            return Err(ArgFlow::ErrorLine(unknown_option_line(a)));
        } else {
            args.inputs.push(a.clone());
        }
        i += 1;
    }

    if args.inputs.is_empty() {
        return Err(ArgFlow::NoInputs);
    }
    if args.output.is_some() && args.inputs.len() != 1 {
        return Err(ArgFlow::DieMsg(
            b"-o is only valid with exactly one input file".to_vec(),
        ));
    }
    Ok(args)
}

// ---------------------------------------------------------------------------
// Output path derivation: input.py → input.mpy.
// ---------------------------------------------------------------------------

/// Strip trailing ".py" if present; append ".mpy" (main.zig
/// `default_output_path`).
fn default_output_path(input: &[u8]) -> Vec<u8> {
    let stem_len = if input.ends_with(b".py") {
        input.len() - 3
    } else {
        input.len()
    };
    let mut out = Vec::with_capacity(stem_len + 4);
    out.extend_from_slice(&input[..stem_len]);
    out.extend_from_slice(b".mpy");
    out
}

// ---------------------------------------------------------------------------
// File I/O (mirrors main.zig read_file_libc / write_file_libc).
// ---------------------------------------------------------------------------

fn path_from_bytes(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

fn read_err_line(path: &[u8], err: &[u8]) -> Vec<u8> {
    let mut line = b"zigpy-compile: ".to_vec();
    line.extend_from_slice(path);
    line.extend_from_slice(b": cannot read: ");
    line.extend_from_slice(err);
    line.push(b'\n');
    line
}

fn write_err_line(path: &[u8], err: &[u8]) -> Vec<u8> {
    let mut line = b"zigpy-compile: ".to_vec();
    line.extend_from_slice(path);
    line.extend_from_slice(b": cannot write: ");
    line.extend_from_slice(err);
    line.push(b'\n');
    line
}

/// Read a whole file. Mirrors `read_file_libc` (`fopen` → `fseek END` →
/// `ftell` → `fread`): open failure is `FopenFailed`, a short read is
/// `FreadShort`. A directory reads as empty (the oracle's `ftell` on a
/// directory stream returns 0 — verified against the reference binary).
fn read_file(path: &[u8]) -> Vec<u8> {
    let p = path_from_bytes(path);
    if p.is_dir() {
        return Vec::new();
    }
    let mut f = match File::open(p) {
        Ok(f) => f,
        Err(_) => {
            print_err_line(&read_err_line(path, b"error.FopenFailed"));
            process::exit(2);
        }
    };
    let len = match f.metadata() {
        Ok(m) => m.len() as usize,
        Err(_) => {
            print_err_line(&read_err_line(path, b"error.FopenFailed"));
            process::exit(2);
        }
    };
    let mut buf = vec![0u8; len];
    if len == 0 {
        return buf;
    }
    match f.read_exact(&mut buf) {
        Ok(()) => buf,
        Err(_) => {
            print_err_line(&read_err_line(path, b"error.FreadShort"));
            process::exit(2);
        }
    }
}

/// Write a whole file. Mirrors `write_file_libc` (`fopen "wb"` →
/// `fwrite`): open failure is `FopenFailed`, a short write is
/// `FwriteShort`. Empty content still creates/truncates the file.
fn write_file(path: &[u8], bytes: &[u8]) {
    let mut f = match File::create(path_from_bytes(path)) {
        Ok(f) => f,
        Err(_) => {
            print_err_line(&write_err_line(path, b"error.FopenFailed"));
            process::exit(2);
        }
    };
    if bytes.is_empty() {
        return;
    }
    if f.write_all(bytes).is_err() {
        print_err_line(&write_err_line(path, b"error.FwriteShort"));
        process::exit(2);
    }
}

// ---------------------------------------------------------------------------
// Compile a single file. Returns true on success, false on compile error.
// Infrastructure errors exit(2) and don't return (main.zig `compile_one`).
// ---------------------------------------------------------------------------

fn compile_err_line(path: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut line = b"zigpy-compile: ".to_vec();
    line.extend_from_slice(path);
    line.extend_from_slice(b": ");
    line.extend_from_slice(msg);
    line.push(b'\n');
    line
}

fn compile_one(input_path: &[u8], output_path: &[u8], source_name: &str) -> bool {
    // NUL-terminate the input path into a 4096-byte buffer (main.zig
    // `nul_terminate`); over-long paths exit 2 before any reading.
    if input_path.len() > MAX_PATH_LEN {
        print_err_line(&compile_err_line(input_path, b"input path too long"));
        process::exit(2);
    }
    let py_src = read_file(input_path);

    // The bridge takes `&str` source; see the header notes for the
    // non-UTF-8 limitation. Reported as a parse failure (exit 1).
    let src = match std::str::from_utf8(&py_src) {
        Ok(s) => s,
        Err(_) => {
            print_err_line(&compile_err_line(
                input_path,
                b"parse error: error.SyntaxError",
            ));
            return false;
        }
    };

    // Pipeline: lexer → parser → compile → mpy.write, via the Phase B
    // bridge. `FileInput` matches main.zig's `.file_input`; lexer
    // `debug_mode` is `true`, matching main.zig:238 (see header notes).
    let bytes = match compile_to_mpy_named_debug(src, InputKind::FileInput, source_name, true) {
        Ok(bytes) => bytes,
        Err(CompileError::OutOfMemory) => {
            print_err_line(&compile_err_line(
                input_path,
                b"out of memory during compile",
            ));
            process::exit(2);
        }
        // The bridge collapses parse/compile stages; the parse-stage
        // form is printed (see header notes).
        Err(CompileError::SyntaxError) => {
            print_err_line(&compile_err_line(
                input_path,
                b"parse error: error.SyntaxError",
            ));
            return false;
        }
        Err(CompileError::IndentationError) => {
            print_err_line(&compile_err_line(
                input_path,
                b"parse error: error.IndentationError",
            ));
            return false;
        }
    };

    // NUL-terminate the output path (same 4096-byte buffer rule).
    if output_path.len() > MAX_PATH_LEN {
        print_err_line(&compile_err_line(output_path, b"output path too long"));
        process::exit(2);
    }
    write_file(output_path, &bytes);
    true
}

// ---------------------------------------------------------------------------
// Entry.
// ---------------------------------------------------------------------------

fn main() {
    // argv as raw bytes (Zig `[:0]const u8` argv, argv[0] = program name).
    let argv: Vec<Vec<u8>> = env::args_os()
        .map(|a| a.as_bytes().to_vec())
        .collect();

    let args = match parse_args(&argv) {
        Ok(args) => args,
        Err(ArgFlow::Help) => {
            print_usage();
            process::exit(0);
        }
        Err(ArgFlow::Version) => {
            print_version();
            process::exit(0);
        }
        Err(ArgFlow::NoInputs) => {
            print_usage();
            process::exit(2);
        }
        Err(ArgFlow::ErrorLine(line)) => {
            print_err_line(&line);
            process::exit(2);
        }
        Err(ArgFlow::DieMsg(msg)) => die_infra(&msg),
    };

    // Default source name = "" (matches mpy-cross -s '' so byte-diffs
    // against compile_test stay clean even when invoking zigpy-compile
    // directly). Users can override with -s.
    let source_name: &str = match &args.source_name {
        Some(name) => match std::str::from_utf8(name) {
            Ok(s) => s,
            Err(_) => die_infra(b"-s value is not valid UTF-8"),
        },
        None => "",
    };

    let mut any_failed = false;
    for input in &args.inputs {
        let output_owned: Option<Vec<u8>> = if args.output.is_none() {
            Some(default_output_path(input))
        } else {
            None
        };
        let output_path: &[u8] = match &args.output {
            Some(o) => o,
            None => output_owned.as_ref().unwrap(),
        };
        if !compile_one(input, output_path, source_name) {
            any_failed = true;
        }
    }

    process::exit(if any_failed { 1 } else { 0 });
}

// ---------------------------------------------------------------------------
// Tests (main.zig has 0 test blocks; these pin the CLI contract — usage
// text, version line, arg parsing, output-path derivation — against the
// original's observable behaviour, checked by scripts/check_cli.py).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(strs: &[&str]) -> Vec<Vec<u8>> {
        let mut v = vec![b"zigpy-compile".to_vec()];
        v.extend(strs.iter().map(|s| s.as_bytes().to_vec()));
        v
    }

    #[test]
    fn usage_text_is_verbatim() {
        // Raw string on both sides: a `\` continuation would strip the
        // two-space option indents (that bug was caught by check_cli.py
        // against the oracle, not by this test — keep the differential
        // as the real guard).
        assert_eq!(
            USAGE,
            r#"usage: zigpy-compile [opts] <input.py> [<input2.py> ...]

options:
  -o <file>      explicit output path (only valid with one input)
  -s <name>      override embedded source-file name (default: "")
  -O<n>          optimisation level (only -O3 is meaningful; default 3)
  -v, --version  print version and exit
  -h, --help     print this help and exit

Default output: <input>.mpy alongside <input>.py.
"#
        );
    }

    #[test]
    fn version_line_is_verbatim() {
        assert_eq!(
            format!("zigpy-compile (petitpy {VERSION}) -- emitting mpy v6\n"),
            "zigpy-compile (petitpy 0.1.0-dev) -- emitting mpy v6\n"
        );
    }

    #[test]
    fn parse_help_and_version() {
        assert!(matches!(parse_args(&argv(&["-h"])), Err(ArgFlow::Help)));
        assert!(matches!(
            parse_args(&argv(&["--help"])),
            Err(ArgFlow::Help)
        ));
        assert!(matches!(
            parse_args(&argv(&["-v"])),
            Err(ArgFlow::Version)
        ));
        assert!(matches!(
            parse_args(&argv(&["--version"])),
            Err(ArgFlow::Version)
        ));
        // -h / -v win as soon as they are seen, whatever follows.
        assert!(matches!(
            parse_args(&argv(&["-v", "a.py"])),
            Err(ArgFlow::Version)
        ));
        // ... but an earlier unknown option errors first.
        assert!(matches!(
            parse_args(&argv(&["--bogus", "-v"])),
            Err(ArgFlow::ErrorLine(_))
        ));
    }

    #[test]
    fn parse_no_inputs_prints_usage_exit_2() {
        assert!(matches!(parse_args(&argv(&[])), Err(ArgFlow::NoInputs)));
        // -o alone still ends at NoInputs (the -o/multi check comes after).
        assert!(matches!(
            parse_args(&argv(&["-o", "out.mpy"])),
            Err(ArgFlow::NoInputs)
        ));
    }

    #[test]
    fn parse_o_and_s() {
        let args = parse_args(&argv(&["-o", "out.mpy", "a.py"])).unwrap();
        assert_eq!(args.output, Some(b"out.mpy".to_vec()));
        assert_eq!(args.inputs, vec![b"a.py".to_vec()]);

        let args = parse_args(&argv(&["a.py", "-s", "src.py"])).unwrap();
        assert_eq!(args.source_name, Some(b"src.py".to_vec()));
        assert_eq!(args.inputs, vec![b"a.py".to_vec()]);

        // -s twice: last wins (no "specified twice" check in main.zig).
        let args = parse_args(&argv(&["-s", "one", "-s", "two", "a.py"])).unwrap();
        assert_eq!(args.source_name, Some(b"two".to_vec()));

        // -o twice: error (and the missing-argument check comes first).
        assert!(matches!(
            parse_args(&argv(&["-o", "a.mpy", "-o", "b.mpy", "x.py"])),
            Err(ArgFlow::DieMsg(m)) if m == b"-o specified twice"
        ));
        assert!(matches!(
            parse_args(&argv(&["-o", "a.mpy", "-o"])),
            Err(ArgFlow::DieMsg(m)) if m == b"-o requires an argument"
        ));
        assert!(matches!(
            parse_args(&argv(&["-o"])),
            Err(ArgFlow::DieMsg(m)) if m == b"-o requires an argument"
        ));
        assert!(matches!(
            parse_args(&argv(&["-s"])),
            Err(ArgFlow::DieMsg(m)) if m == b"-s requires an argument"
        ));

        // -o with multiple inputs: error.
        assert!(matches!(
            parse_args(&argv(&["-o", "out.mpy", "a.py", "b.py"])),
            Err(ArgFlow::DieMsg(m)) if m == b"-o is only valid with exactly one input file"
        ));
    }

    #[test]
    fn parse_opt_levels() {
        // -O / -O0..-O3 accepted; all map to a recorded level.
        for (flag, level) in [
            ("-O", 3u8),
            ("-O0", 0),
            ("-O1", 1),
            ("-O2", 2),
            ("-O3", 3),
        ] {
            let args = parse_args(&argv(&[flag, "a.py"])).unwrap();
            assert_eq!(args.opt_level, level, "flag {flag}");
        }
        // -O4 / -O30 / -Ox are unknown options.
        for flag in ["-O4", "-O30", "-Ox"] {
            match parse_args(&argv(&[flag, "a.py"])) {
                Err(ArgFlow::ErrorLine(line)) => {
                    assert_eq!(
                        line,
                        format!("zigpy-compile: error: unknown option: {flag}\n").into_bytes(),
                        "flag {flag}"
                    );
                }
                other => panic!("flag {flag}: expected ErrorLine, got {}", other.is_ok()),
            }
        }
    }

    #[test]
    fn parse_unknown_options() {
        for flag in ["--bogus", "-x", "-", "--", "-ox"] {
            match parse_args(&argv(&[flag, "a.py"])) {
                Err(ArgFlow::ErrorLine(line)) => {
                    assert_eq!(
                        line,
                        format!("zigpy-compile: error: unknown option: {flag}\n").into_bytes(),
                        "flag {flag}"
                    );
                }
                _ => panic!("flag {flag}: expected ErrorLine"),
            }
        }
        // Empty-string arg is an input, not an option.
        let args = parse_args(&argv(&[""])).unwrap();
        assert_eq!(args.inputs, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn parse_inputs_in_order() {
        let args = parse_args(&argv(&["a.py", "b.py", "c.py"])).unwrap();
        assert_eq!(
            args.inputs,
            vec![b"a.py".to_vec(), b"b.py".to_vec(), b"c.py".to_vec()]
        );
        assert_eq!(args.output, None);
        assert_eq!(args.source_name, None);
        assert_eq!(args.opt_level, 3);
    }

    #[test]
    fn default_output_paths() {
        assert_eq!(default_output_path(b"a.py"), b"a.mpy".to_vec());
        assert_eq!(default_output_path(b"a"), b"a.mpy".to_vec());
        assert_eq!(default_output_path(b"a.py.py"), b"a.py.mpy".to_vec());
        assert_eq!(default_output_path(b".py"), b".mpy".to_vec());
        assert_eq!(default_output_path(b"dir/b.py"), b"dir/b.mpy".to_vec());
        assert_eq!(default_output_path(b"a.PY"), b"a.PY.mpy".to_vec());
        assert_eq!(default_output_path(b""), b".mpy".to_vec());
    }
}
