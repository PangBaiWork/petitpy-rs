//! Tests for the `_subprocess` module (`modsubprocess.rs`).
//!
//! All tests run real commands (`/bin/true`, `/bin/false`,
//! `/bin/cat`, `/bin/echo`, `/bin/sh`, `/usr/bin/env`,
//! `/bin/pwd`, `/usr/bin/head`) — the same surface the
//! reference probes used. The timeout test pins
//! `returncode -9` (`SIGKILL` — the documented divergence
//! from the reference's `-15` `SIGTERM`).

use std::os::unix::io::AsRawFd;

use crate::modules::modsubprocess::build_subprocess_module;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM with `_subprocess` registered, plus the
/// read-back helpers (the `objstringio_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_subprocess(&mut vm);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => self.raised_parts(exc),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn raised_parts(&mut self, exc: Value) -> (String, String) {
        let (name, arg) = match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => {
                    let t = d.type_;
                    let name = match self.vm.arena.get(t) {
                        Obj::Type(td) => String::from_utf8_lossy(
                            self.vm.pool.qstr_str(td.name as u16),
                        )
                        .into_owned(),
                        _ => panic!("exception type is not a type"),
                    };
                    (name, d.args.first().copied())
                }
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        };
        let msg = match arg {
            Some(v) => self.val_str(v),
            None => String::new(),
        };
        (name, msg)
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    /// The `_spawn_and_wait` 4-tuple as
    /// `(returncode, stdout, stderr, timed_out)`, with the
    /// byte streams as `Option<Vec<u8>>`.
    fn g_result(&mut self, name: &str) -> (i64, Option<Vec<u8>>, Option<Vec<u8>>, bool) {
        let items = match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Tuple(t) => t.clone(),
                other => panic!("global {name} is not a tuple: {other:?}"),
            },
            other => panic!("global {name} is not a tuple: {other:?}"),
        };
        assert_eq!(items.len(), 4, "result tuple has 4 items");
        let rc = match items[0] {
            Value::SmallInt(n) => n,
            other => panic!("returncode is not an int: {other:?}"),
        };
        let bytes = |v: Value| -> Option<Vec<u8>> {
            match v {
                Value::None => None,
                Value::Heap(r) => match self.vm.arena.get(r) {
                    Obj::Bytes(b) => Some(b.bytes.clone()),
                    other => panic!("stream value is not bytes: {other:?}"),
                },
                Value::Qstr(q) => Some(self.vm.pool.qstr_str(q as u16).to_vec()),
                other => panic!("stream value is not bytes: {other:?}"),
            }
        };
        let out = bytes(items[1]);
        let err = bytes(items[2]);
        let timed_out = match items[3] {
            Value::Bool(b) => b,
            other => panic!("timed_out is not a bool: {other:?}"),
        };
        (rc, out, err, timed_out)
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String`.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                Obj::Bytes(b) => String::from_utf8_lossy(&b.bytes).into_owned(),
                other => panic!("value is not a str: {other:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::None => "None".to_string(),
        }
    }
}

fn setup_subprocess(vm: &mut Vm) {
    let fun_var = vm.state.vm_types.fun_builtin_var;
    let module_type = vm.state.vm_types.module;
    let m = {
        let (mut ctx, state) = vm.split();
        let m = build_subprocess_module(&mut ctx, fun_var, module_type)
            .expect("_subprocess builds");
        let id = static_qstr_id(b"_subprocess").expect("_subprocess qstr");
        state.builtin_modules.push((id, m));
        m
    };
    vm.pin(m);
}

#[test]
fn subprocess_true_false() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/true"], None, -1.0, None, None, -4, -4, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/false"], None, -1.0, None, None, -4, -4, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (1, None, None, false));
}

#[test]
fn subprocess_echo_pipe() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/echo", "hi"], None, -1.0, None, None, -4, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, Some(b"hi\n".to_vec()), None, false));
}

#[test]
fn subprocess_cat_input() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/cat"], b"abc", -1.0, None, None, -1, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, Some(b"abc".to_vec()), None, false));
    // A str input is converted the same way.
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/cat"], "abc", -1.0, None, None, -1, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, Some(b"abc".to_vec()), None, false));
}

#[test]
fn subprocess_stdout_and_stderr() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "echo out; echo err >&2"], None, -1.0, None, None, -4, -1, -1)
"#,
    );
    assert_eq!(
        fx.g_result("r"),
        (0, Some(b"out\n".to_vec()), Some(b"err\n".to_vec()), false)
    );
}

#[test]
fn subprocess_stderr_stdout_merged() {
    // `stderr=STDOUT` over a piped stdout: stderr joins the
    // child's stdout stream (the Zig's `dup2(1, 2)`), so
    // the drained stdout holds both lines in write order
    // and stderr is None — NOT the parent's stdout (F5b).
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "echo out; echo err >&2"], None, -1.0, None, None, -4, -1, -2)
"#,
    );
    assert_eq!(
        fx.g_result("r"),
        (0, Some(b"out\nerr\n".to_vec()), None, false)
    );
}

#[test]
fn subprocess_devnull() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/echo", "hi"], None, -1.0, None, None, -4, -3, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, None, None, false));
}

#[test]
fn subprocess_timeout() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sleep", "5"], None, 0.2, None, None, -4, -4, -4)
"#,
    );
    // `std` kills with SIGKILL, so the returncode is -9,
    // not the reference's -15 (SIGTERM) — see the module
    // docs.
    assert_eq!(fx.g_result("r"), (-9, None, None, true));
}

#[test]
fn subprocess_env_and_cwd() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/usr/bin/env"], None, -1.0, ["FOO=bar"], None, -4, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, Some(b"FOO=bar\n".to_vec()), None, false));
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/pwd"], None, -1.0, None, "/tmp", -4, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, Some(b"/tmp\n".to_vec()), None, false));
}

#[test]
fn subprocess_bad_command_and_cwd() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/nonexistent-cmd-xyz"], None, -1.0, None, None, -4, -4, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (127, None, None, false));
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/nonexistent-cmd-xyz"], None, -1.0, None, None, -4, -1, -1)
"#,
    );
    assert_eq!(
        fx.g_result("r"),
        (127, Some(Vec::new()), Some(Vec::new()), false)
    );
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/true"], None, -1.0, None, "/nonexistent-dir-xyz", -4, -1, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (127, Some(Vec::new()), None, false));
}

#[test]
fn subprocess_arg_errors() {
    let mut fx = Fx::new();
    for (src, want) in [
        (
            "import _subprocess\n_subprocess._spawn_and_wait([], None, -1.0, None, None, -4, -4, -4)",
            ("OSError", "22"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/true\", 123], None, -1.0, None, None, -4, -4, -4)",
            ("OSError", "22"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait(\"/bin/true\", None, -1.0, None, None, -4, -4, -4)",
            ("OSError", "22"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/true\"], None, None, None, None, -4, -4, -4)",
            ("TypeError", "can't convert to float"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/cat\"], 123, -1.0, None, None, -1, -1, -4)",
            ("TypeError", "can't convert to str implicitly"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/true\"], None, -1.0, \"FOO=1\", None, -4, -4, -4)",
            ("OSError", "22"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/true\"], None, -1.0, None, 123, -4, -4, -4)",
            ("OSError", "22"),
        ),
        (
            "import _subprocess\n_subprocess._spawn_and_wait([\"/bin/true\"], None, -1.0, None, None, \"x\", -4, -4)",
            ("TypeError", "can't convert to int"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

/// A tuple `argv` ABORTS the reference (`attempt to use
/// null value` panic in `cStrArray`, probed) — this port
/// returns `OSError(EINVAL)` instead.
#[test]
fn subprocess_argv_tuple() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(
            "import _subprocess\n_subprocess._spawn_and_wait((\"/bin/true\",), None, -1.0, None, None, -4, -4, -4)"
        ),
        ("OSError".to_string(), "22".to_string())
    );
}

/// 200KB of output must not deadlock: the drain runs while
/// the child runs (the Zig's poll loop; here reader
/// threads). Without the concurrent drain the child would
/// block once the pipe filled (64KB).
#[test]
fn subprocess_big_output() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "head -c 200000 /dev/zero"], None, -1.0, None, None, -4, -1, -4)
"#,
    );
    let (rc, out, err, timed_out) = fx.g_result("r");
    assert_eq!(rc, 0);
    assert_eq!(out.as_ref().map(|v| v.len()), Some(200000));
    assert!(out.map(|v| v.iter().all(|b| *b == 0)).unwrap_or(false));
    assert_eq!(err, None);
    assert!(!timed_out);
}

/// File-descriptor stream modes (`/proc/self/fd`
/// re-open): stdin from a file, stdout to a file.
#[test]
fn subprocess_fd_modes() {
    let in_path = "/tmp/petitpy-d3-sub-in.txt";
    let out_path = "/tmp/petitpy-d3-sub-out.txt";
    std::fs::write(in_path, b"data").expect("write input file");
    std::fs::write(out_path, b"").expect("write output file");
    // The `File`s stay alive for the whole test: dropping
    // one would close its fd before the child re-opens it
    // via `/proc/self/fd`.
    let in_file = std::fs::File::open(in_path).expect("open input");
    let in_fd = in_file.as_raw_fd();
    let out_file = std::fs::OpenOptions::new()
        .write(true)
        .open(out_path)
        .expect("open output");
    let out_fd = out_file.as_raw_fd();
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/cat\"], None, -1.0, None, None, {in_fd}, -1, -4)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, Some(b"data".to_vec()), None, false));
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/echo\", \"hi\"], None, -1.0, None, None, -4, {out_fd}, -4)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    assert_eq!(
        std::fs::read(out_path).expect("read output"),
        b"hi\n",
        "stdout went to the file"
    );
    drop(in_file);
    drop(out_file);
    let _ = std::fs::remove_file(in_path);
    let _ = std::fs::remove_file(out_path);
}
