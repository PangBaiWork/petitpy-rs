//! Tests for the Lane D4 small modules (`modtime`, `modselect`,
//! `modgc`, `moderrno`, `modsignal`, `modpwd`, `modgrp`,
//! `modmicropython`, `modcollections`, `modarray`, `modmain`).
//!
//! All tests are end-to-end through a real `Vm` (`Vm::new`
//! registers the D4 modules — the `modsocket_tests.rs`
//! `Fx` pattern): run a script, read its globals back, or
//! assert the raised exception. Reference behaviour for
//! every assertion was probed on the reference binary
//! 2026-11-18 (see `scripts/d4_result.txt` §2).

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::time::Duration;

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::parser::InputKind;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM (D4 modules registered by `Vm::new`) plus
/// the read-back helpers (the `modsocket_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        Self { vm: Vm::new() }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    /// Load + run `src` as the entry (`__main__`) module —
    /// the Phase E entry shape (`load_module_from_mpy` with
    /// name `"__main__"` sets `state.d4.main_module`).
    fn run_main(&mut self, src: &str) -> Result<Value, VmError> {
        let mpy = compile_to_mpy_named(src, InputKind::FileInput, b"__main__")
            .map_err(VmError::Compile)?;
        self.vm.load_module_from_mpy(&mpy, "__main__")
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

    fn run_main_ok(&mut self, src: &str) -> Value {
        match self.run_main(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("entry module raised {name}: {msg}")
            }
            Err(e) => panic!("entry module failed to run: {e:?}"),
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
                Obj::Float(f) => f.to_string(),
                Obj::Tuple(t) => format!("({})", t.len()),
                Obj::List(l) => format!("[{}]", l.items.len()),
                other => panic!("value is not a str: {other:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::None => "None".to_string(),
        }
    }

    fn g_int(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Int(i) => i.as_int_checked().expect("test int fits i64"),
                other => panic!("global {name} is not an int: {other:?}"),
            },
            other => panic!("global {name} is not an int: {other:?}"),
        }
    }

    fn g_float(&mut self, name: &str) -> f64 {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Float(f) => *f,
                other => panic!("global {name} is not a float: {other:?}"),
            },
            Some(Value::SmallInt(n)) => n as f64,
            other => panic!("global {name} is not a float: {other:?}"),
        }
    }

    fn g_bool(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    fn g_str(&mut self, name: &str) -> String {
        match self.g(name) {
            Some(v) => self.val_str(v),
            None => panic!("global {name} is absent"),
        }
    }

    fn g_is_none(&mut self, name: &str) -> bool {
        matches!(self.g(name), Some(Value::None))
    }
}

// ── time ──────────────────────────────────────────────────────────

#[test]
fn d4_time_values() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import time\n\
         t = time.time()\n\
         tns = time.time_ns()\n\
         m = time.monotonic()\n\
         mns = time.monotonic_ns()\n\
         tk = time.ticks_ms()\n",
    );
    assert!(fx.g_float("t") > 0.0);
    assert!(fx.g_int("tns") > 0);
    assert!(fx.g_float("m") >= 0.0);
    assert!(fx.g_int("mns") >= 0);
    assert!(fx.g_int("tk") >= 0);
    // `time_ns` and `time` agree to within a second.
    let tns = fx.g_int("tns");
    let t = fx.g_float("t");
    assert!(((tns as f64) / 1e9 - t).abs() < 1.0);
}

#[test]
fn d4_time_ticks_diff() {
    let mut fx = Fx::new();
    fx.run_ok("import time\na = time.ticks_diff(10, 3)\nb = time.ticks_diff(3, 10)\n");
    assert_eq!(fx.g_int("a"), 7);
    assert_eq!(fx.g_int("b"), -7);
}

#[test]
fn d4_time_sleep_ms_nonpositive() {
    let mut fx = Fx::new();
    fx.run_ok("import time\na = time.sleep_ms(-5)\nb = time.sleep_ms(0)\n");
    assert!(fx.g_is_none("a"));
    assert!(fx.g_is_none("b"));
}

#[test]
fn d4_time_sleep_negative_raises() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import time\ntime.sleep(-1)\n"),
        ("OSError".to_string(), "22".to_string())
    );
}

#[test]
fn d4_time_sleep_bad_type() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import time\ntime.sleep('x')\n"),
        (
            "TypeError".to_string(),
            "can't convert to float".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import time\ntime.sleep_ms('x')\n"),
        ("TypeError".to_string(), "can't convert to int".to_string())
    );
    assert_eq!(
        fx.raised_msg("import time\ntime.sleep_ms(1.5)\n"),
        ("TypeError".to_string(), "can't convert to int".to_string())
    );
}

#[test]
fn d4_time_perf_counter_is_monotonic() {
    let mut fx = Fx::new();
    fx.run_ok("import time\nsame = time.perf_counter is time.monotonic\n");
    assert!(fx.g_bool("same"));
}

// ── gc ────────────────────────────────────────────────────────────

#[test]
fn d4_gc_collect_zero() {
    let mut fx = Fx::new();
    fx.run_ok("import gc\nc = gc.collect()\n");
    assert_eq!(fx.g_int("c"), 0);
}

#[test]
fn d4_gc_mem() {
    let mut fx = Fx::new();
    fx.run_ok("import gc\na = gc.mem_alloc()\nf = gc.mem_free()\n");
    assert!(fx.g_int("a") >= 0);
    assert!(fx.g_int("f") >= 0);
}

#[test]
fn d4_gc_enable_disable() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import gc\n\
         a = gc.isenabled()\n\
         gc.disable()\n\
         b = gc.isenabled()\n\
         gc.enable()\n\
         c = gc.isenabled()\n",
    );
    assert!(fx.g_bool("a"));
    assert!(!fx.g_bool("b"));
    assert!(fx.g_bool("c"));
}

#[test]
fn d4_gc_no_threshold() {
    let mut fx = Fx::new();
    fx.run_ok("import gc\nhas = hasattr(gc, 'threshold')\n");
    assert!(!fx.g_bool("has"));
}

// ── errno ─────────────────────────────────────────────────────────

#[test]
fn d4_errno_constants() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import errno\n\
         a = errno.ENOENT\n\
         b = errno.EPERM\n\
         c = errno.EINVAL\n\
         d = errno.EINPROGRESS\n",
    );
    assert_eq!(fx.g_int("a"), 2);
    assert_eq!(fx.g_int("b"), 1);
    assert_eq!(fx.g_int("c"), 22);
    assert_eq!(fx.g_int("d"), 115);
}

#[test]
fn d4_errno_errorcode() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import errno\n\
         n = errno.errorcode[2]\n\
         ln = len(errno.errorcode)\n",
    );
    assert_eq!(fx.g_str("n"), "ENOENT");
    assert_eq!(fx.g_int("ln"), 22);
}

#[test]
fn d4_errno_errorcode_missing() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import errno\nerrno.errorcode[999]\n"),
        ("KeyError".to_string(), "999".to_string())
    );
}

#[test]
fn d4_uerrno_import() {
    let mut fx = Fx::new();
    fx.run_ok("import uerrno\na = uerrno.ENOENT\n");
    assert_eq!(fx.g_int("a"), 2);
}

// ── select ────────────────────────────────────────────────────────

#[test]
fn d4_select_empty() {
    let mut fx = Fx::new();
    fx.run_ok("import select\nr = select.select([], [], [], 0)\n");
    assert_eq!(fx.g_str("r"), "(3)");
}

#[test]
fn d4_select_empty_timeout() {
    let mut fx = Fx::new();
    fx.run_ok("import select\nr = select.select([], [], [], 0.01)\n");
    assert_eq!(fx.g_str("r"), "(3)");
}

#[test]
fn d4_select_negative_timeout() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import select\nselect.select([], [], [], -1)\n"),
        ("OSError".to_string(), "22".to_string())
    );
}

#[test]
fn h_select_stdio_fd_ebadf() {
    // H-select: fd 0 (stdin) is not resolvable in pure `std`
    // (no safe `File` for stdio) — `EBADF`, on both builds.
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import select\nselect.select([0], [], [], 0)\n"),
        ("OSError".to_string(), "9".to_string())
    );
}

// ── select, pure-`std` probes (H-select; both builds) ───────────────

/// A readable temp file for select probes: /dev/null does not
/// exist on Windows. A regular file is equally "ready" for
/// select (files are always ready; EOF counts as ready).
fn probe_path() -> String {
    let path = std::env::temp_dir().join(format!("petitpy_probe_{}", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn h_select_devnull_readable() {
    let mut fx = Fx::new();
    let path = probe_path();
    fx.run_ok(&format!(
        "import os, select\n\
         fd = os.open({path:?}, os.O_RDONLY)\n\
         r, w, x = select.select([fd], [], [], 0)\n\
         a = len(r)\n\
         b = r[0]\n\
         os.close(fd)\n",
    ));
    assert_eq!(fx.g_int("a"), 1);
    // The result keeps the original fd object.
    assert!(fx.g_int("b") >= 0);
}

#[cfg(unix)]
#[test]
fn h_select_listener_never_ready() {
    // H-select: a listener is never ready — pure `std` has no
    // non-destructive probe for it (`UnixStream::peek` is
    // unstable; `accept` would consume the pending
    // connection). The connect thread still runs, so this
    // pins the limitation even WITH a pending connection
    // (the old G0 test asserted ready here via real
    // `select(2)`). Timeout 0.2s, not 5s — nothing will ever
    // make it ready.
    let path = format!("/tmp/h_sel_listen_{}", std::process::id());
    let _ = std::fs::remove_file(&path);
    let client_path = path.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = UnixStream::connect(&client_path);
    });
    let mut fx = Fx::new();
    // No `accept()` here: the listener is non-blocking and the
    // client may not have connected yet when `select` returns,
    // so `accept()` would raise EAGAIN (35) on a slow runner
    // — a race, not a select result. Closing the listener
    // with a pending connection just resets it; the client
    // thread ignores its connect result.
    let src = format!(
        "import socket, select, os\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({path:?}); s.listen(1); s.setblocking(False)\n\
         r, _, _ = select.select([s.fileno()], [], [], 0.2)\n\
         a = len(r)\n\
         s.close(); os.remove({path:?})\n"
    );
    fx.run_ok(&src);
    let _ = handle.join();
    let _ = std::fs::remove_file(&path);
    assert_eq!(fx.g_int("a"), 0);
}

#[test]
fn h_select_write_ready() {
    let mut fx = Fx::new();
    let path = probe_path();
    fx.run_ok(&format!(
        "import os, select\n\
         fd = os.open({path:?}, os.O_WRONLY)\n\
         _, w, _ = select.select([], [fd], [], 0)\n\
         a = len(w)\n\
         os.close(fd)\n",
    ));
    assert_eq!(fx.g_int("a"), 1);
}

#[test]
fn h_select_ebadf() {
    let mut fx = Fx::new();
    let path = probe_path();
    assert_eq!(
        fx.raised_msg(&format!(
            "import os, select\n\
             fd = os.open({path:?}, os.O_RDONLY)\n\
             os.close(fd)\n\
             select.select([fd], [], [], 0)\n",
        )),
        ("OSError".to_string(), "9".to_string())
    );
}

#[test]
fn h_select_high_fd_ebadf() {
    // H-select: Python has no FD_SETSIZE cap, and an unknown
    // fd is `EBADF` — the old feature path silently skipped
    // fd >= 1024 (and returned three empty lists).
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import select\nselect.select([2000], [], [], 0)\n"),
        ("OSError".to_string(), "9".to_string())
    );
}

#[test]
fn h_select_dup_fd() {
    let mut fx = Fx::new();
    let path = probe_path();
    fx.run_ok(&format!(
        "import os, select\n\
         fd = os.open({path:?}, os.O_RDONLY)\n\
         r, _, _ = select.select([fd, fd], [], [], 0)\n\
         a = len(r)\n\
         os.close(fd)\n",
    ));
    // filterReady appends per input item, like the Zig.
    assert_eq!(fx.g_int("a"), 2);
}

#[test]
fn h_select_xlist_not_ready() {
    let mut fx = Fx::new();
    let path = probe_path();
    fx.run_ok(&format!(
        "import os, select\n\
         fd = os.open({path:?}, os.O_RDONLY)\n\
         _, _, x = select.select([], [], [fd], 0)\n\
         a = len(x)\n\
         os.close(fd)\n",
    ));
    assert_eq!(fx.g_int("a"), 0);
}

#[test]
fn h_select_file_object_ready() {
    // H-select: a file object is accepted (its own `File`),
    // a regular file is ready at once (EOF counts as ready),
    // and the result keeps the ORIGINAL object (identity).
    let mut fx = Fx::new();
    let path = probe_path();
    fx.run_ok(&format!(
        "import select\n\
         f = open({path:?}, 'rb')\n\
         r, w, x = select.select([f], [], [], 0)\n\
         a = len(r)\n\
         b = r[0] is f\n\
         f.close()\n",
    ));
    assert_eq!(fx.g_int("a"), 1);
    assert!(fx.g_bool("b"));
}

#[cfg(unix)]
#[test]
fn h_select_socket_object_never_ready() {
    // H-select: a socket object resolves through its
    // `fileno()` (the `self_socket` shape) — and, like any
    // socket fd, is never ready in pure `std` (see the
    // module docs). Both the object and the bare fd.
    let mut fx = Fx::new();
    fx.run_ok(
        "import socket, select\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         r, _, _ = select.select([s], [], [], 0)\n\
         a = len(r)\n\
         r, _, _ = select.select([s.fileno()], [], [], 0)\n\
         b = len(r)\n\
         s.close()\n",
    );
    assert_eq!(fx.g_int("a"), 0);
    assert_eq!(fx.g_int("b"), 0);
}

#[test]
fn d4_select_bad_arg() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import select\nselect.select(1, [], [], 0)\n"),
        (
            "TypeError".to_string(),
            "select() argument must be a list".to_string()
        )
    );
}

#[test]
fn d4_select_too_few_args() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import select\nselect.select([], [])\n"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

// ── signal ────────────────────────────────────────────────────────

#[test]
fn d4_signal_constants() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import signal\n\
         a = signal.SIGTERM\n\
         b = signal.SIGINT\n\
         c = signal.SIG_DFL\n\
         d = signal.SIG_IGN\n",
    );
    assert_eq!(fx.g_int("a"), 15);
    assert_eq!(fx.g_int("b"), 2);
    assert_eq!(fx.g_int("c"), 0);
    assert_eq!(fx.g_int("d"), 1);
}

#[test]
fn d4_signal_roundtrip() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import signal\n\
         def h(signum, frame):\n\
         \x20   pass\n\
         a = signal.getsignal(15)\n\
         b = signal.signal(15, h)\n\
         c = signal.getsignal(15)\n\
         d = signal.signal(15, signal.SIG_DFL)\n",
    );
    assert_eq!(fx.g_int("a"), 0);
    assert_eq!(fx.g_int("b"), 0);
    // `c` is the handler function — read back as the stored
    // value: it must be the same object as the `h` global.
    let c = fx.g("c").expect("c");
    let h = fx.g("h").expect("h");
    assert_eq!(c, h);
    let d = fx.g("d").expect("d");
    assert_eq!(d, h);
}

#[test]
fn d4_signal_out_of_range() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import signal\n\
         def h(signum, frame):\n\
         \x20   pass\n\
         a = signal.getsignal(999)\n\
         b = signal.getsignal(-1)\n\
         c = signal.signal(999, h)\n\
         d = signal.getsignal(999)\n",
    );
    assert_eq!(fx.g_int("a"), 0);
    assert_eq!(fx.g_int("b"), 0);
    assert_eq!(fx.g_int("c"), 0);
    assert_eq!(fx.g_int("d"), 0);
}

#[test]
fn d4_signal_bad_type() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import signal\nsignal.signal('x', 0)\n"),
        ("TypeError".to_string(), "can't convert to int".to_string())
    );
}

// ── pwd / grp ─────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn d4_pwd_root() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import pwd\n\
         pw = pwd.getpwnam('root')\n\
         n = pw[0]\n\
         u = pw[2]\n\
         g = pw[3]\n\
         d = pw[5]\n\
         s = pw[6]\n",
    );
    assert_eq!(fx.g_str("n"), "root");
    assert_eq!(fx.g_int("u"), 0);
    assert_eq!(fx.g_int("g"), 0);
    // The home directory and shell are OS-specific
    // (`/etc/passwd`): Linux `/root` + `/bin/bash`, macOS
    // `/var/root` + `/bin/sh`.
    #[cfg(target_os = "macos")]
    let (home, shell) = ("/var/root", "/bin/sh");
    #[cfg(not(target_os = "macos"))]
    let (home, shell) = ("/root", "/bin/bash");
    assert_eq!(fx.g_str("d"), home);
    assert_eq!(fx.g_str("s"), shell);
}

#[cfg(unix)]
#[test]
fn d4_pwd_missing() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import pwd\npwd.getpwnam('nosuchuser123')\n"),
        ("KeyError".to_string(), "nosuchuser123".to_string())
    );
    assert_eq!(
        fx.raised_msg("import pwd\npwd.getpwuid(99999)\n"),
        ("KeyError".to_string(), "99999".to_string())
    );
    assert_eq!(
        fx.raised_msg("import pwd\npwd.getpwnam(123)\n"),
        ("KeyError".to_string(), "123".to_string())
    );
}

#[cfg(unix)]
#[test]
fn d4_pwd_getpwuid_root() {
    let mut fx = Fx::new();
    fx.run_ok("import pwd\npw = pwd.getpwuid(0)\nn = pw[0]\n");
    assert_eq!(fx.g_str("n"), "root");
}

#[cfg(unix)]
#[test]
fn d4_grp_root() {
    let mut fx = Fx::new();
    // The gid-0 group is OS-specific (`/etc/group`): "root"
    // on Linux, "wheel" on macOS (there is no "root" group
    // there — `getgrnam('root')` raises KeyError). The member
    // list is only asserted on Linux (the macOS wheel members
    // depend on the machine's `/etc/group`).
    #[cfg(target_os = "macos")]
    let group = "wheel";
    #[cfg(not(target_os = "macos"))]
    let group = "root";
    fx.run_ok(&format!(
        "import grp\n\
         gr = grp.getgrnam('{group}')\n\
         n = gr[0]\n\
         g = gr[2]\n\
         m = gr[3]\n",
    ));
    assert_eq!(fx.g_str("n"), group);
    assert_eq!(fx.g_int("g"), 0);
    #[cfg(not(target_os = "macos"))]
    assert_eq!(fx.g_str("m"), "[0]");
}

#[cfg(unix)]
#[test]
fn d4_grp_missing() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import grp\ngrp.getgrnam('nosuchgroup123')\n"),
        ("KeyError".to_string(), "nosuchgroup123".to_string())
    );
    assert_eq!(
        fx.raised_msg("import grp\ngrp.getgrgid(99999)\n"),
        ("KeyError".to_string(), "99999".to_string())
    );
}

// ── micropython ───────────────────────────────────────────────────

#[test]
fn d4_micropython_const() {
    let mut fx = Fx::new();
    fx.run_ok("import micropython\na = micropython.const(5)\nb = micropython.const('x')\n");
    assert_eq!(fx.g_int("a"), 5);
    assert_eq!(fx.g_str("b"), "x");
}

#[test]
fn d4_micropython_opt_level() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import micropython\n\
         a = micropython.opt_level()\n\
         micropython.opt_level(1)\n\
         b = micropython.opt_level()\n\
         micropython.opt_level(0)\n\
         c = micropython.opt_level()\n",
    );
    assert_eq!(fx.g_int("a"), 0);
    assert_eq!(fx.g_int("b"), 1);
    assert_eq!(fx.g_int("c"), 0);
}

#[test]
fn d4_micropython_heap() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import micropython\n\
         a = micropython.heap_lock()\n\
         b = micropython.heap_unlock()\n",
    );
    assert!(fx.g_is_none("a"));
    assert_eq!(fx.g_int("b"), 0);
}

// ── collections ───────────────────────────────────────────────────

#[test]
fn d4_collections_deque() {
    // NOTE (D5): `import collections` now loads the FROZEN
    // petitpy-lib package (list-backed Python `deque`), so the
    // NATIVE deque is reached via `ucollections` — on the
    // reference `ucollections.deque is collections.deque` is
    // False. The frozen deque's `__init__` is a Python function,
    // which hits the pre-existing FunBc `__init__` stub-seam
    // gap (see d5_result.txt §4).
    let mut fx = Fx::new();
    fx.run_ok(
        "from ucollections import deque\n\
         d = deque([1, 2, 3], 10)\n\
         a = len(d)\n\
         d.append(4)\n\
         b = len(d)\n\
         c = d.popleft()\n\
         d.appendleft(0)\n\
         e = d[0]\n",
    );
    assert_eq!(fx.g_int("a"), 3);
    assert_eq!(fx.g_int("b"), 4);
    assert_eq!(fx.g_int("c"), 1);
    assert_eq!(fx.g_int("e"), 0);
}

#[test]
fn d4_collections_deque_one_arg_fails() {
    // NOTE (D5): the native deque still requires exactly 2
    // args; the FROZEN `collections.deque` (list-backed Python
    // class) accepts 1 — so this pins the native module via
    // `ucollections`, as on the reference.
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("from ucollections import deque\ndeque([1, 2, 3])\n"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

#[test]
fn d4_collections_namedtuple() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections import namedtuple\n\
         T = namedtuple('Tup', ['foo', 'bar'])\n\
         t = T(1, 2)\n\
         a = t.foo\n\
         b = t[1]\n\
         u = T(bar=1, foo=2)\n\
         c = u.foo\n",
    );
    assert_eq!(fx.g_int("a"), 1);
    assert_eq!(fx.g_int("b"), 2);
    assert_eq!(fx.g_int("c"), 2);
}

#[test]
fn d4_collections_ordereddict() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections import OrderedDict\n\
         d = OrderedDict()\n\
         d['a'] = 1\n\
         a = d['a']\n\
         is_dict = isinstance(d, dict)\n",
    );
    assert_eq!(fx.g_int("a"), 1);
    assert!(fx.g_bool("is_dict"));
}

#[test]
fn d4_collections_uimport_identity() {
    // NOTE (D5): with the frozen bundle loaded, `import
    // collections` is the FROZEN package, so `deque` is the
    // list-backed Python class — NOT the native deque
    // (probed on the reference: `ucollections.deque is
    // collections.deque` is False). `namedtuple` and
    // `OrderedDict` are re-exported from the native module
    // by the frozen package, so they stay identical.
    let mut fx = Fx::new();
    fx.run_ok(
        "import collections\n\
         import ucollections\n\
         a = ucollections.deque is collections.deque\n\
         b = ucollections.namedtuple is collections.namedtuple\n\
         c = ucollections.OrderedDict is collections.OrderedDict\n",
    );
    assert!(!fx.g_bool("a"));
    assert!(fx.g_bool("b"));
    assert!(fx.g_bool("c"));
}

// ── array ─────────────────────────────────────────────────────────

#[test]
fn d4_array_import() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import array\n\
         import uarray\n\
         same = uarray.array is array.array\n\
         a = array.array('i', [1, 2])\n\
         ln = len(a)\n\
         first = a[0]\n",
    );
    assert!(fx.g_bool("same"));
    assert_eq!(fx.g_int("ln"), 2);
    assert_eq!(fx.g_int("first"), 1);
}

// ── usys ──────────────────────────────────────────────────────────

#[test]
fn d4_usys_import() {
    let mut fx = Fx::new();
    fx.run_ok("import sys\nimport usys\nsame = usys is sys\n");
    assert!(fx.g_bool("same"));
}

// ── __main__ ──────────────────────────────────────────────────────

#[test]
fn d4_main_import() {
    let mut fx = Fx::new();
    fx.run_main_ok("x = 42\n");
    fx.run_ok("import __main__\ny = __main__.x\nn = __main__.__name__\n");
    assert_eq!(fx.g_int("y"), 42);
    assert_eq!(fx.g_str("n"), "__main__");
}

#[test]
fn d4_main_not_in_sys_modules() {
    let mut fx = Fx::new();
    fx.run_main_ok("x = 42\n");
    fx.run_ok(
        "import sys\n\
         try:\n\
         \x20   sys.modules['__main__']\n\
         \x20   found = True\n\
         except KeyError:\n\
         \x20   found = False\n",
    );
    assert!(!fx.g_bool("found"));
}

#[test]
fn d4_main_missing_before_load() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import __main__\n"),
        (
            "ModuleNotFoundError".to_string(),
            "No module named '__main__'".to_string()
        )
    );
}
