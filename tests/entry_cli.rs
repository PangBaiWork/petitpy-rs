//! CLI end-to-end tests for Lane E1 (entry + CLI).
//!
//! Every test spawns the real built binaries (`petitpy-rs` and
//! `petitpy-vm`) as subprocesses and checks stdout, stderr and
//! the exit code byte-for-byte against the reference behaviour
//! probed in `scripts/e1_brief.md`. No test depends on the
//! reference binary being present: all expected strings are
//! pinned here.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const USAGE: &str = "petitpy — a small Python runtime, MicroPython-compatible, written in Zig.\n\nUsage:\n  petitpy <script.py>       run a Python script (`ppy` is the same command)\n  petitpy -c CODE           run CODE (sys.argv[0] is '-c')\n  petitpy --emit-mpy F.py   compile only; print the .mpy as base64\n  petitpy --version         print version\n  petitpy --help            this message\n";

const VM_USAGE: &str = "petitpy-vm — VM-only runtime (loads pre-compiled .mpy files).\n\nUsage:\n  petitpy-vm <script.mpy>   run a compiled bytecode file\n  petitpy-vm --version      print version\n  petitpy-vm --help         this message\n";

const BANNER: &str = "petitpy 0.9.0 — interactive REPL (Ctrl-D to exit)\n";

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn bin_vm() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-vm"))
}

/// A fresh scratch directory for one test (named, so parallel
/// tests never collide).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("e1-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).unwrap();
    path
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(bin: &Path, args: &[&str], stdin: &str, cwd: Option<&Path>) -> Run {
    run_env(bin, args, stdin, cwd, &[])
}

fn run_env(
    bin: &Path,
    args: &[&str],
    stdin: &str,
    cwd: Option<&Path>,
    env: &[(&str, Option<&str>)],
) -> Run {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Host PYTHONPATH / PETITPY_LIB must not leak into the
        // child's sys.path; the env tests set them explicitly.
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB");
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        match v {
            Some(v) => {
                cmd.env(k, v);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().unwrap();
    Run {
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
        code: status.code().unwrap(),
    }
}

/// Minimal base64 decoder (standard alphabet, `=` padding) —
/// enough to turn `--emit-mpy` output back into `.mpy` bytes.
fn b64_decode(s: &str) -> Vec<u8> {
    let mut vals = Vec::with_capacity(s.len());
    for c in s.chars() {
        if c == '=' {
            break;
        }
        let v = match c {
            'A'..='Z' => c as u8 - b'A',
            'a'..='z' => c as u8 - b'a' + 26,
            '0'..='9' => c as u8 - b'0' + 52,
            '+' => 62,
            '/' => 63,
            _ => continue,
        };
        vals.push(v);
    }
    // Pad the value stream to a multiple of 4 with zeros (the
    // bits the `=` padding stood for), decode each quartet
    // into 3 bytes, then truncate to the real byte count.
    while vals.len() % 4 != 0 {
        vals.push(0);
    }
    let nbytes_total = (vals.len() * 6 / 8).saturating_sub(
        // Each `=` in the original input removes one output
        // byte; count them from the tail of the input.
        s.chars().rev().take_while(|&c| c == '=').count(),
    );
    let mut out = Vec::with_capacity(nbytes_total);
    for chunk in vals.chunks(4) {
        let mut n: u32 = 0;
        for &v in chunk {
            n = (n << 6) | v as u32;
        }
        for i in 0..3 {
            out.push(((n >> (8 * (2 - i))) & 0xff) as u8);
        }
    }
    out.truncate(nbytes_total);
    out
}

/// Compile `src` with `--emit-mpy` and write the `.mpy` bytes
/// to `name` in `dir`. Panics unless the emit succeeds.
fn emit_mpy(dir: &Path, src: &str, name: &str) -> PathBuf {
    let py = write_file(dir, "prog.py", src.as_bytes());
    let r = run(&bin_petitpy(), &["--emit-mpy", py.to_str().unwrap()], "", Some(dir));
    assert_eq!(r.code, 0, "emit-mpy failed: {}", r.stderr);
    assert_eq!(r.stderr, "");
    let mpy = b64_decode(r.stdout.trim());
    write_file(dir, name, &mpy)
}

// ----------------------------------------------------------------
// `petitpy` — version / help
// ----------------------------------------------------------------

#[test]
fn version_long() {
    let r = run(&bin_petitpy(), &["--version"], "", None);
    assert_eq!(r.stdout, "petitpy 0.9.0\n");
    assert_eq!(r.stderr, "");
    assert_eq!(r.code, 0);
}

#[test]
fn version_short() {
    let r = run(&bin_petitpy(), &["-V"], "", None);
    assert_eq!(r.stdout, "petitpy 0.9.0\n");
    assert_eq!(r.stderr, "");
    assert_eq!(r.code, 0);
}

#[test]
fn version_extra_arg_ignored() {
    // Only argv[1] is consulted.
    let r = run(&bin_petitpy(), &["--version", "extra"], "", None);
    assert_eq!(r.stdout, "petitpy 0.9.0\n");
    assert_eq!(r.code, 0);
}

#[test]
fn help_long() {
    let r = run(&bin_petitpy(), &["--help"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, USAGE);
    assert_eq!(r.code, 0);
}

#[test]
fn help_short() {
    let r = run(&bin_petitpy(), &["-h"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, USAGE);
    assert_eq!(r.code, 0);
}

// ----------------------------------------------------------------
// `petitpy` — --emit-mpy
// ----------------------------------------------------------------

#[test]
fn emit_mpy_byte_pinned() {
    // `print(1+1)\n` compiled with source name `<emit-mpy>`:
    // byte-identical to the reference binary's output.
    let dir = scratch("emit_mpy_byte_pinned");
    let py = write_file(&dir, "em.py", b"print(1+1)\n");
    let r = run(
        &bin_petitpy(),
        &["--emit-mpy", py.to_str().unwrap()],
        "",
        Some(&dir),
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stderr, "");
    assert_eq!(
        r.stdout,
        "TQYAHwMAFDxlbWl0LW1weT4AD4F3WAgCARECgjQBWVFj\n"
    );
}

#[test]
fn emit_mpy_missing_file() {
    let r = run(&bin_petitpy(), &["--emit-mpy", "/nonexistent.py"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "petitpy: cannot read input\n");
    assert_eq!(r.code, 1);
}

#[test]
fn emit_mpy_no_arg() {
    let r = run(&bin_petitpy(), &["--emit-mpy"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "usage: petitpy --emit-mpy IN.py\n");
    assert_eq!(r.code, 2);
}

#[test]
fn emit_mpy_syntax_error() {
    let dir = scratch("emit_mpy_syntax_error");
    let py = write_file(&dir, "bad.py", b"1+\n");
    let r = run(
        &bin_petitpy(),
        &["--emit-mpy", py.to_str().unwrap()],
        "",
        Some(&dir),
    );
    // Compile failure re-runs through the exec path for the
    // diagnostic; the status is 2.
    assert_eq!(r.code, 2);
    assert_eq!(r.stdout, "");
    assert_eq!(
        r.stderr,
        "Traceback (most recent call last):\n  File \"<stdin>\"\nSyntaxError: invalid syntax\n"
    );
}

// ----------------------------------------------------------------
// `petitpy` — -c
// ----------------------------------------------------------------

#[test]
fn dash_c_print() {
    let r = run(&bin_petitpy(), &["-c", "print(1+1)"], "", None);
    assert_eq!(r.stdout, "2\n");
    assert_eq!(r.stderr, "");
    assert_eq!(r.code, 0);
}

#[test]
fn dash_c_argv() {
    let r = run(
        &bin_petitpy(),
        &["-c", "import sys; print(sys.argv)", "a", "b"],
        "",
        None,
    );
    assert_eq!(r.stdout, "['-c', 'a', 'b']\n");
    assert_eq!(r.code, 0);
}

#[test]
fn dash_c_missing_code() {
    let r = run(&bin_petitpy(), &["-c"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "petitpy: argument expected for the -c option\n");
    assert_eq!(r.code, 2);
}

// ----------------------------------------------------------------
// `petitpy` — script files
// ----------------------------------------------------------------

#[test]
fn script_missing() {
    let r = run(&bin_petitpy(), &["/nonexistent.py"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(
        r.stderr,
        "petitpy: can't open file '/nonexistent.py': FileNotFound\n"
    );
    assert_eq!(r.code, 2);
}

#[test]
fn script_is_dir() {
    let dir = scratch("script_is_dir");
    let r = run(&bin_petitpy(), &[dir.to_str().unwrap()], "", None);
    assert_eq!(
        r.stderr,
        format!("petitpy: can't open file '{}': IsDir\n", dir.display())
    );
    assert_eq!(r.code, 2);
}

#[test]
fn script_unknown_flag_is_a_filename() {
    let r = run(&bin_petitpy(), &["--foo"], "", None);
    assert_eq!(
        r.stderr,
        "petitpy: can't open file '--foo': FileNotFound\n"
    );
    assert_eq!(r.code, 2);
}

#[test]
fn script_file_argv_path() {
    let dir = scratch("script_file_argv_path");
    write_file(
        &dir,
        "sub/s.py",
        b"import sys\nprint(__file__)\nprint(sys.path[0])\nprint(sys.argv)\n",
    );
    let r = run(&bin_petitpy(), &["sub/s.py", "a", "b"], "", Some(&dir));
    assert_eq!(r.stdout, "sub/s.py\nsub\n['sub/s.py', 'a', 'b']\n");
    assert_eq!(r.stderr, "");
    assert_eq!(r.code, 0);
}

// ----------------------------------------------------------------
// `petitpy` — exit codes (SystemExit)
// ----------------------------------------------------------------

fn exit_code_for(code_src: &str) -> Run {
    run(&bin_petitpy(), &["-c", code_src], "", None)
}

#[test]
fn exit_systemexit_bare() {
    let r = exit_code_for("raise SystemExit");
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_0() {
    let r = exit_code_for("raise SystemExit(0)");
    assert_eq!(r.code, 0);
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_3() {
    let r = exit_code_for("raise SystemExit(3)");
    assert_eq!(r.code, 3);
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_neg1() {
    // Low 8 bits: -1 -> 255.
    let r = exit_code_for("raise SystemExit(-1)");
    assert_eq!(r.code, 255);
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_huge() {
    // Heap ints are not small ints: 2**70 -> 1.
    let r = exit_code_for("raise SystemExit(2**70)");
    assert_eq!(r.code, 1);
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_true() {
    // bool is not a small int: True -> 1.
    let r = exit_code_for("raise SystemExit(True)");
    assert_eq!(r.code, 1);
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_str() {
    let r = exit_code_for("raise SystemExit(\"x\")");
    assert_eq!(r.code, 1);
    // SystemExit NEVER prints a traceback.
    assert_eq!(r.stderr, "");
}

#[test]
fn exit_systemexit_none() {
    let r = exit_code_for("raise SystemExit(None)");
    assert_eq!(r.code, 0);
    assert_eq!(r.stderr, "");
}

// ----------------------------------------------------------------
// `petitpy` — tracebacks
// ----------------------------------------------------------------

#[test]
fn uncaught_traceback_stderr() {
    let r = run(&bin_petitpy(), &["-c", "raise ValueError(\"boom\")"], "", None);
    assert_eq!(r.code, 1);
    assert_eq!(r.stdout, "");
    assert_eq!(
        r.stderr,
        "Traceback (most recent call last):\n  File \"<stdin>\", in <module>\nValueError: boom\n"
    );
}

#[test]
fn compile_error_traceback() {
    // The SyntaxError frame has no block: no ", in <module>".
    let r = run(&bin_petitpy(), &["-c", "1+"], "", None);
    assert_eq!(r.code, 1);
    assert_eq!(r.stdout, "");
    assert_eq!(
        r.stderr,
        "Traceback (most recent call last):\n  File \"<stdin>\"\nSyntaxError: invalid syntax\n"
    );
}

// ----------------------------------------------------------------
// `petitpy` — REPL
// ----------------------------------------------------------------

#[test]
fn repl_pipe_basic() {
    let r = run(&bin_petitpy(), &[], "x = 5\nx\n1+1\n", None);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "5\n2\n");
    // 3 lines + the final EOF read = 4 prompts, then "\n".
    assert_eq!(r.stderr, format!("{BANNER}>>> >>> >>> >>> \n"));
}

#[test]
fn repl_pipe_multiline_failure() {
    // No continuation: `def f():` alone is a SyntaxError, the
    // loop continues, and later lines still run.
    let input = "x = 5\nx\n1+1\ndef f():\nprint(\"hi\")\nraise ValueError(\"boom\")\n2+2\n";
    let r = run(&bin_petitpy(), &[], input, None);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "5\n2\nhi\n4\n");
    assert_eq!(
        r.stderr,
        format!(
            "{BANNER}>>> >>> >>> >>> Traceback (most recent call last):\n  File \"<stdin>\"\nSyntaxError: invalid syntax\n>>> >>> Traceback (most recent call last):\n  File \"<stdin>\", in <module>\nValueError: boom\n>>> >>> \n"
        )
    );
}

#[test]
fn repl_eof_empty() {
    let r = run(&bin_petitpy(), &[], "", None);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, format!("{BANNER}>>> \n"));
}

// ----------------------------------------------------------------
// `petitpy` — env sys.path wiring
// ----------------------------------------------------------------

#[test]
fn env_pythonpath() {
    let dir = scratch("env_pythonpath");
    let home = dir.to_str().unwrap().to_string();
    let lib = format!("{home}/.petitpy/lib");
    // `print(sys.path)` shows each entry via `repr`, which
    // doubles backslashes — on Windows the temp path's `\`
    // become `\\` in the output (CI Build #14).
    #[cfg(windows)]
    let lib = lib.replace('\\', "\\\\");
    // PYTHONPATH's list separator is platform-specific
    // (`;` on Windows, `:` on Unix) — the entries must be
    // split by `append_path_entries` on every platform.
    #[cfg(windows)]
    let py_path = "/foo;/bar";
    #[cfg(not(windows))]
    let py_path = "/foo:/bar";
    let r = run_env(
        &bin_petitpy(),
        &["-c", "import sys; print(sys.path)"],
        "",
        None,
        &[("HOME", Some(&home)), ("PYTHONPATH", Some(py_path))],
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, format!("['', '/foo', '/bar', '{lib}']\n"));
}

#[test]
fn env_petitpy_lib() {
    let dir = scratch("env_petitpy_lib");
    let home = dir.to_str().unwrap().to_string();
    let r = run_env(
        &bin_petitpy(),
        &["-c", "import sys; print(sys.path)"],
        "",
        None,
        &[("HOME", Some(&home)), ("PETITPY_LIB", Some("/baz"))],
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "['', '/baz']\n");
}

// ----------------------------------------------------------------
// `petitpy` — Obj::Code + D3 registration through the CLI
// ----------------------------------------------------------------

#[test]
fn compile_code_object_cli() {
    let src = "c = compile(\"1+1\", \"f\", \"eval\")\nprint(type(c).__name__)\nprint(repr(c))\nprint(eval(c))\nprint(exec(compile(\"x = 41\", \"f\", \"exec\")))\nprint(x)\n";
    let r = run(&bin_petitpy(), &["-c", src], "", None);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "code\n<code>\n2\nNone\n41\n");
}

#[test]
fn import_d3_modules_cli() {
    let src = "import math, struct, socket\nprint(math.sqrt(4))\nprint(struct.calcsize('i'))\nprint(\"socket ok\")\ntry:\n    import umath\n    print(\"umath ok\")\nexcept ModuleNotFoundError:\n    print(\"umath missing\")\n";
    let r = run(&bin_petitpy(), &["-c", src], "", None);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "2.0\n4\nsocket ok\numath missing\n");
}

// ----------------------------------------------------------------
// `petitpy-vm`
// ----------------------------------------------------------------

#[test]
fn vm_version() {
    let r = run(&bin_vm(), &["--version"], "", None);
    assert_eq!(r.stdout, "petitpy-vm 0.9.0\n");
    assert_eq!(r.stderr, "");
    assert_eq!(r.code, 0);
}

#[test]
fn vm_version_short() {
    let r = run(&bin_vm(), &["-V"], "", None);
    assert_eq!(r.stdout, "petitpy-vm 0.9.0\n");
    assert_eq!(r.code, 0);
}

#[test]
fn vm_help() {
    let r = run(&bin_vm(), &["--help"], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, VM_USAGE);
    assert_eq!(r.code, 0);
}

#[test]
fn vm_no_args() {
    let r = run(&bin_vm(), &[], "", None);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, VM_USAGE);
    assert_eq!(r.code, 0);
}

#[test]
fn vm_missing_file() {
    let r = run(&bin_vm(), &["/nonexistent.mpy"], "", None);
    // The Zig prints a stack trace after this line; the Rust
    // binary reproduces only the first line.
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "error: FileNotFound\n");
    assert_eq!(r.code, 1);
}

#[test]
fn vm_run_mpy() {
    let dir = scratch("vm_run_mpy");
    let mpy = emit_mpy(
        &dir,
        "print(\"from mpy\")\nimport sys\nprint(sys.argv)\nprint(sys.path)\n",
        "prog.mpy",
    );
    let r = run(&bin_vm(), &[mpy.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    // The vm binary wires no argv / env path: [] and [''].
    assert_eq!(r.stdout, "from mpy\n[]\n['']\n");
    assert_eq!(r.stderr, "");
}

#[test]
fn vm_traceback_stdout() {
    // The uncaught traceback goes to STDOUT, not stderr —
    // the probed petitpy / petitpy-vm asymmetry.
    let dir = scratch("vm_traceback_stdout");
    let mpy = emit_mpy(&dir, "raise ValueError(\"boom\")\n", "bad.mpy");
    let r = run(&bin_vm(), &[mpy.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 1);
    assert_eq!(
        r.stdout,
        "Traceback (most recent call last):\n  File \"<emit-mpy>\", in <module>\nValueError: boom\n"
    );
    assert_eq!(r.stderr, "");
}

#[test]
fn vm_systemexit() {
    let dir = scratch("vm_systemexit");
    let mpy = emit_mpy(&dir, "raise SystemExit(3)\n", "exit.mpy");
    let r = run(&bin_vm(), &[mpy.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 3);
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "");
}

#[test]
fn vm_eval_linked() {
    // The compiler separation was removed (it saved only ~14KB
    // of ~2.1MB): `eval` works under `petitpy-vm` now.
    let dir = scratch("vm_eval_linked");
    let mpy = emit_mpy(&dir, "print(eval(\"1+1\"))\n", "eval.mpy");
    let r = run(&bin_vm(), &[mpy.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "2\n");
    assert_eq!(r.stderr, "");
}

#[test]
fn vm_import_math() {
    let dir = scratch("vm_import_math");
    let mpy = emit_mpy(&dir, "import math\nprint(math.sqrt(4))\n", "imp.mpy");
    let r = run(&bin_vm(), &[mpy.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, "2.0\n");
}

#[test]
fn vm_corrupt_mpy() {
    let dir = scratch("vm_corrupt_mpy");
    let bad = write_file(&dir, "bad.mpy", b"not a real mpy file at all");
    let r = run(&bin_vm(), &[bad.to_str().unwrap()], "", Some(&dir));
    assert_eq!(r.code, 1);
    assert_eq!(r.stdout, "ValueError: incompatible .mpy file\n");
    assert_eq!(r.stderr, "");
}

#[test]
fn vm_extra_args_ignored() {
    let dir = scratch("vm_extra_args_ignored");
    let mpy = emit_mpy(&dir, "print(\"ok\")\n", "prog.mpy");
    let r = run(
        &bin_vm(),
        &[mpy.to_str().unwrap(), "junk", "args"],
        "",
        Some(&dir),
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "ok\n");
    assert_eq!(r.stderr, "");
}
