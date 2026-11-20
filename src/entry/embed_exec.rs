//! The top-level exec HOST (`entry/embed_exec.zig` +
//! `main.zig`'s `runSource` / REPL branches).
//!
//! `run_source` is the `petitpy` binary's compile-then-exec
//! path (source name `"<stdin>"`, FILE_INPUT, run as
//! `__main__`); `run_repl` is the no-args interactive loop.
//! Both share the exit-code contract + traceback printing
//! with `embed_run_mpy` (see `exit_status_for`).

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::compile::CompileError;
use crate::compiler::parser::InputKind;
use crate::objects::value::{GcRef, Value};
use crate::runtime::vm::Vm;

/// `npy_embed_exec_str_zig` (`entry/embed_exec.zig`): compile
/// `src` as `__main__` (source name `"<stdin>"`, FILE_INPUT)
/// and run it. Returns the process exit code:
///   0  — clean exit (no exception, or SystemExit() /
///        SystemExit(None) / SystemExit(0))
///   1  — uncaught non-SystemExit exception (traceback
///        already printed) or SystemExit with a non-int arg
///   N  — SystemExit(N) where N is an int (lower 8 bits)
/// SystemExit NEVER prints a traceback (suppressed on both
/// nano and full — CPython convention).
///
/// `file` is the script path as raw bytes (`Some` for a script
/// file — sets `__main__.__file__` + `sys.path[0]`; `None` for
/// `-c` and the `--emit-mpy` diagnostic re-run). G2 (P7a):
/// bytes, not `&str` — a non-UTF-8 script name must reach
/// `__file__` byte-exact.
pub fn run_source(vm: &mut Vm, src: &[u8], file: Option<&[u8]>) -> i32 {
    match vm.run_main_source(src, b"<stdin>", file) {
        Ok(_) => 0,
        Err(exc) => exit_status_for(vm, exc, true),
    }
}

/// The shared uncaught-raise tail of both exec hosts: a
/// SystemExit maps to its exit code (never printed); anything
/// else prints its traceback to the host's stream and exits 1.
/// `to_stderr` is true for `petitpy` (stderr, after flushing
/// stdout) and false for `petitpy-vm` (stdout, `mp_plat_print`)
/// — the probed asymmetry between the two binaries.
pub(crate) fn exit_status_for(vm: &mut Vm, exc: Value, to_stderr: bool) -> i32 {
    {
        let (ctx, state) = vm.split();
        if crate::entry::systemexit::is_system_exit(&ctx, state, exc) {
            return crate::entry::systemexit::exit_code(&ctx, exc);
        }
    }
    print_traceback(vm, exc, to_stderr);
    1
}

/// Build the traceback text for `exc` (`print_exception_hook`,
/// the hook-aware printer — the same core `sys.print_exception`
/// uses) and write it to stderr (flushing stdout first, as the
/// Zig's `printExc` does — `print()` writes through libc's
/// buffered stdout, and a traceback written straight to fd 1
/// would come out before the program's earlier output when
/// stdout is a pipe) or stdout.
pub(crate) fn print_traceback(vm: &mut Vm, exc: Value, to_stderr: bool) {
    use std::io::Write as _;
    let out = {
        let (mut ctx, state) = vm.split();
        let mut out = String::new();
        let pool = ctx.pool;
        // G4/H7: the hook-aware printer — an uncaught
        // exception's instance args run their dunders
        // (`raise KeyError(A())` prints `KeyError: A-str`).
        crate::runtime::builtin_str::print_exception_hook(
            &mut ctx,
            state,
            exc,
            &|id| pool.qstr_str(id as u16),
            &mut out,
        );
        out
    };
    if to_stderr {
        // P14: flush the buffered stdout (the Zig's `printExc`
        // does `fflush(null)` before the stderr traceback).
        crate::support::output::flush_stdout_buffer();
        let _ = std::io::stderr().write_all(out.as_bytes());
    } else {
        // petitpy-vm: the traceback goes to stdout via a plain
        // write, BYPASSING the buffer with no flush first —
        // the Zig's `embed_run_mpy` calls
        // `mp_obj_print_exception(&mp_plat_print, raw)` with
        // no `fflush` (probed: REF prints the traceback
        // BEFORE earlier buffered `print()` output, which
        // surfaces at exit). The exit flush in `main` emits
        // the buffered output after the traceback.
        let _ = std::io::stdout().write_all(out.as_bytes());
        let _ = std::io::stdout().flush();
    }
}

/// `wireEnvSysPath` (`main.zig`): PYTHONPATH entries, then
/// PETITPY_LIB (or `$HOME/.petitpy/lib`), then
/// `<exe dir>/../share/petitpy/lib` if that directory exists.
/// Appends to the VM's `sys.path` (whose `[0]` is `""`).
/// Runs for script, `-c`, and REPL alike, before argv/file
/// wiring.
///
/// G2 (P7a): paths are raw bytes (`Vec<Vec<u8>>`) — an env
/// entry is UTF-8 here (`std::env::var`), but the stored form
/// is bytes so a non-UTF-8 script dirname can overwrite
/// `[0]` without a lossy round-trip.
pub fn wire_env_sys_path(vm: &mut Vm) {
    let mut paths = vm.state.sys_path.clone();
    if let Ok(pp) = std::env::var("PYTHONPATH") {
        append_path_entries(&mut paths, &pp);
    }
    // PETITPY_LIB overrides; falls back to `~/.petitpy/lib`.
    if let Ok(lib) = std::env::var("PETITPY_LIB") {
        append_path_entries(&mut paths, &lib);
    } else if let Ok(home) = std::env::var("HOME") {
        append_path_entries(&mut paths, &format!("{home}/.petitpy/lib"));
    }
    // `<exe dir>/../share/petitpy/lib` (ADR 016 §3) — only
    // when the directory exists. The literal string (with the
    // `/../` component, as the Zig builds it) is what lands in
    // `sys.path`.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let lib = format!("{}/../share/petitpy/lib", dir.display());
            if std::path::Path::new(&lib).is_dir() {
                paths.push(lib.into_bytes());
            }
        }
    }
    vm.set_sys_path(paths);
}

/// `appendPathEntries` (`main.zig`): append each
/// path-list entry, skipping empties (CPython behavior
/// — an empty entry would mean CWD, which `sys.path[0]`
/// already covers). The separator is platform-specific:
/// `:` on Unix, `;` on Windows — a `:` on Windows is the
/// drive letter, so splitting on it would break `C:\...`
/// into `C` + `\...` (CI Build #13 `env_pythonpath`).
fn append_path_entries(paths: &mut Vec<Vec<u8>>, value: &str) {
    #[cfg(windows)]
    const SEP: char = ';';
    #[cfg(not(windows))]
    const SEP: char = ':';
    for entry in value.split(SEP) {
        if !entry.is_empty() {
            paths.push(entry.as_bytes().to_vec());
        }
    }
}

/// `wireSysArgv` (`main.zig`): populate `sys.argv` with the
/// given args (the script path + script args, or `['-c',
/// ARG...]` — the caller builds the list; argv[0], the
/// interpreter name, is NOT included). Raw bytes, G2 (P7a).
pub fn wire_sys_argv(vm: &mut Vm, argv: &[Vec<u8>]) {
    vm.set_sys_argv(argv.to_vec());
}

/// The interactive REPL (`main.zig`, no-args branch): banner
/// to stderr, then read lines with the `>>> ` prompt (to
/// stderr, before each read — including the final EOF read)
/// and execute each as SINGLE_INPUT in the persistent
/// `__main__` globals. EOF prints `\n` to stderr and exits 0.
/// Raises print their traceback to stderr and the loop
/// continues. NO continuation: `mp_repl_continue_with_input`
/// is a stub returning false unconditionally (probed: a lone
/// `def f():` is a `SyntaxError`, not a continuation prompt —
/// do NOT "fix" multi-line input).
pub fn run_repl(vm: &mut Vm) -> i32 {
    use std::io::Write as _;
    let _ = std::io::stderr().write_all(
        format!(
            "petitpy {} — interactive REPL (Ctrl-D to exit)\n",
            crate::entry::VERSION
        )
        .as_bytes(),
    );
    // The persistent `__main__` globals: an empty module run
    // registers `__main__` (so `import __main__` resolves) and
    // sets `state.globals` to its dict.
    let _ = vm.run_main_source(b"", b"<stdin>", None);
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut stderr = std::io::stderr();
    let mut line: Vec<u8> = Vec::new();
    loop {
        let n = crate::support::readline::repl_read_line(
            &mut reader,
            &mut stderr,
            Some(b">>> "),
            4096,
            &mut line,
        );
        if n == 0 {
            let _ = stderr.write_all(b"\n");
            return 0;
        }
        let _ = repl_exec_line(vm, &line);
    }
}

/// `npy_repl_exec_line` (`entry/embed_exec.zig`): compile +
/// call one REPL line in SINGLE_INPUT mode (bare expressions
/// echo via `__repl_print__`), source name `"<stdin>"`, in
/// the persistent `__main__` globals. Returns 0 on clean
/// completion, 1 on raise (traceback printed to stderr).
/// The caller (`run_repl`, like `main.zig`) ignores the
/// return value.
fn repl_exec_line(vm: &mut Vm, line: &[u8]) -> i32 {
    let globals = vm.state.globals;
    match repl_exec_line_inner(vm, line, globals) {
        Ok(()) => 0,
        Err(exc) => {
            print_traceback(vm, exc, true);
            1
        }
    }
}

/// The compile + run half of `repl_exec_line` (mirrors
/// `Vm::eval_exec`, but with SINGLE_INPUT + source name
/// `"<stdin>"` — `eval_exec` hardcodes `"<string>"`, which
/// would put the wrong file in REPL tracebacks).
fn repl_exec_line_inner(vm: &mut Vm, line: &[u8], globals: GcRef) -> Result<(), Value> {
    let src_str = match std::str::from_utf8(line) {
        Ok(s) => s,
        // Non-UTF-8 source is a `SyntaxError` (the Zig's
        // lexer fails at the first undecodable byte).
        Err(_) => {
            return Err(vm.compile_error_value_named(CompileError::SyntaxError, b"<stdin>"));
        }
    };
    let mpy = match compile_to_mpy_named(src_str, InputKind::SingleInput, b"<stdin>") {
        Ok(m) => m,
        Err(ce) => return Err(vm.compile_error_value_named(ce, b"<stdin>")),
    };
    let (module_val, code_idx) = match vm.register_mpy_code(&mpy, b"<stdin>", Some(globals), false) {
        Ok(v) => v,
        Err(e) => {
            let (mut ctx, state) = vm.split();
            return Err(crate::runtime::compile_bridge::vm_error_to_value(
                &mut ctx, state, e,
            ));
        }
    };
    // Pin the module object, as a module load does (its dict
    // is the caller's globals, so it stays alive through the
    // caller; the pin keeps the module itself alive for the
    // body call).
    vm.pin(module_val);
    let old_globals = vm.state.globals;
    let old_locals = vm.state.locals;
    vm.state.globals = globals;
    vm.state.locals = None;
    let fun = Value::Heap(vm.call_module_fun(code_idx, module_val));
    let result = crate::runtime::vm_ops::call_function(vm, fun, 0, 0, &[]);
    vm.state.globals = old_globals;
    vm.state.locals = old_locals;
    match result {
        Ok(_) => Ok(()),
        Err(exc) => Err(exc),
    }
}
