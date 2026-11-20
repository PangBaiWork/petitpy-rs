//! Tests for the `_subprocess` module (`modsubprocess.rs`).
//!
//! All tests run real commands (`/bin/true`, `/bin/false`,
//! `/bin/cat`, `/bin/echo`, `/bin/sh`, `/usr/bin/env`,
//! `/bin/pwd`, `/usr/bin/head`) — the same surface the
//! reference probes used. The timeout test pins
//! `returncode -9` (`SIGKILL`, no grace) on BOTH builds:
//! Phase H deletes the `unsafe-ffi` SIGTERM+grace path
//! (U5), so the default and feature builds behave the
//! same. The fd tests pin the shared-offset / shared-
//! append / socket-fd dup semantics on BOTH builds too
//! (Phase H deletes the `pre_exec` dup2 (U8)).

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
    // Exit codes 0 and 1 via `/bin/sh -c "exit N"`:
    // `/bin/true` and `/bin/false` are not spawnable on the
    // macOS runners (the spawn fails → returncode 127), and
    // `sh` is the portable way to produce a chosen exit code.
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "exit 0"], None, -1.0, None, None, -4, -4, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "exit 1"], None, -1.0, None, None, -4, -4, -4)
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
    // Unified (Phase H): the timeout arm always sends
    // `SIGKILL` (no `SIGTERM`, no grace), so the
    // returncode is -9 on BOTH builds.
    assert_eq!(fx.g_result("r"), (-9, None, None, true));
}

/// A child that IGNORES `SIGTERM` is `SIGKILL`ed at once
/// (Phase H: no 1.0 s grace — the U5 path is deleted):
/// returncode -9, timed out, and the whole call takes
/// well under a second (the old feature build spent the
/// 1.0 s grace first, so this also pins "no grace").
#[test]
fn subprocess_timeout_sigterm_ignored() {
    use std::time::Instant;
    let mut fx = Fx::new();
    let start = Instant::now();
    fx.run_ok(
        r#"
import _subprocess
r = _subprocess._spawn_and_wait(["/bin/sh", "-c", "trap \"\" TERM; sleep 5"], None, 0.2, None, None, -4, -4, -4)
"#,
    );
    assert_eq!(fx.g_result("r"), (-9, None, None, true));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "timed-out kill took {:?}, expected < 1s (no grace)",
        start.elapsed()
    );
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
    // `/bin/pwd` prints the canonical working directory:
    // on macOS `/tmp` is a symlink to `/private/tmp`, so
    // the child reports `/private/tmp`.
    #[cfg(target_os = "macos")]
    let expected_pwd = b"/private/tmp\n".to_vec();
    #[cfg(not(target_os = "macos"))]
    let expected_pwd = b"/tmp\n".to_vec();
    assert_eq!(
        fx.g_result("r"),
        (0, Some(expected_pwd), None, false)
    );
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

/// File-descriptor stream modes: stdin from a file,
/// stdout to a file (Phase H: the modes are served by
/// duping the fd, so this runs on both builds).
#[test]
fn subprocess_fd_modes() {
    let in_path = "/tmp/petitpy-d3-sub-in.txt";
    let out_path = "/tmp/petitpy-d3-sub-out.txt";
    std::fs::write(in_path, b"data").expect("write input file");
    std::fs::write(out_path, b"").expect("write output file");
    // The `File`s stay alive for the whole test: dropping
    // one would close its fd before the child dups it.
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

/// The child reads from the SHARED offset: the parent
/// seeks to 3 first, the child gets the rest — the dup
/// shares the file offset (Phase H: the dup replaces the
/// `pre_exec` dup2 (U8), so this runs on both builds).
#[test]
fn subprocess_fd_shared_offset() {
    use std::io::Seek;
    let in_path = "/tmp/petitpy-d3-sub-off-in.txt";
    std::fs::write(in_path, b"0123456789").expect("write input file");
    let mut in_file = std::fs::File::open(in_path).expect("open input");
    in_file
        .seek(std::io::SeekFrom::Start(3))
        .expect("seek");
    let in_fd = in_file.as_raw_fd();
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/cat\"], None, -1.0, None, None, {in_fd}, -1, -4)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, Some(b"3456789".to_vec()), None, false));
    drop(in_file);
    let _ = std::fs::remove_file(in_path);
}

/// The child appends after existing content: the fd was
/// opened `O_APPEND` and the dup shares the append mode
/// (Phase H: runs on both builds).
#[test]
fn subprocess_fd_append() {
    use std::io::Write;
    let out_path = "/tmp/petitpy-d3-sub-append-out.txt";
    let mut out_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(true)
        .open(out_path)
        .expect("open output");
    out_file.write_all(b"AAA").expect("write AAA");
    let out_fd = out_file.as_raw_fd();
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/sh\", \"-c\", \"echo BBB\"], None, -1.0, None, None, -4, {out_fd}, -4)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    drop(out_file);
    assert_eq!(std::fs::read(out_path).expect("read output"), b"AAABBB\n");
    let _ = std::fs::remove_file(out_path);
}

/// stdout to a SOCKET fd: the child output arrives on
/// the pair's peer (Phase H: the dup covers socket fds,
/// so this runs on both builds).
#[test]
fn subprocess_fd_socket() {
    use std::io::Read;
    let (mut peer, child_end) = std::os::unix::net::UnixStream::pair().expect("pair");
    let fd = child_end.as_raw_fd();
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/echo\", \"hi\"], None, -1.0, None, None, -4, {fd}, -4)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    drop(child_end);
    let mut buf = Vec::new();
    peer.read_to_end(&mut buf).expect("read peer");
    assert_eq!(buf, b"hi\n");
}

/// `stderr=STDOUT` with `stdout=fd`: both streams land
/// in the same file, in write order — the second dup of
/// the stdout fd (Phase H: runs on both builds).
#[test]
fn subprocess_fd_stderr_stdout() {
    let out_path = "/tmp/petitpy-d3-sub-merge-out.txt";
    let out_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(out_path)
        .expect("open output");
    let out_fd = out_file.as_raw_fd();
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/sh\", \"-c\", \"echo out; echo err >&2\"], None, -1.0, None, None, -4, {out_fd}, -2)\n"
    ));
    assert_eq!(fx.g_result("r"), (0, None, None, false));
    drop(out_file);
    assert_eq!(
        std::fs::read(out_path).expect("read output"),
        b"out\nerr\n"
    );
    let _ = std::fs::remove_file(out_path);
}
