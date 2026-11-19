//! Tests for Lane D2: the `os` module (`modos.zig`) +
//! the fd builtins (`modos_fd.rs`).
//!
//! End-to-end through `Sx` (the dispatch path, where
//! `import os` is live) + one ctx-level test for the
//! `urandom` length. Expected values are the reference
//! binary's, probed 2026-11-18 (see `scripts/d2_result.txt`
//! §2). `len` is not a builtin on this base (D1 binds it),
//! so the tests compare whole values instead of lengths.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::{Vm, VmError};

#[test]
fn os_constants() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
assert os.name == "posix"
assert os.sep == "/"
assert os.altsep == ""
assert os.extsep == "."
assert os.linesep == "\n"
assert os.curdir == "."
assert os.pardir == ".."
assert os.F_OK == 0
assert os.R_OK == 4
assert os.W_OK == 2
assert os.X_OK == 1
assert os.O_RDONLY == 0
assert os.O_WRONLY == 1
assert os.O_RDWR == 2
assert os.O_CREAT == 64
assert os.O_TRUNC == 512
assert os.O_APPEND == 1024
assert os.O_EXCL == 128
assert os.SEEK_SET == 0
assert os.SEEK_CUR == 1
assert os.SEEK_END == 2
"#,
    );
}

#[test]
fn os_getcwd_listdir() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
assert type(os.getcwd()) is str
assert os.getcwd() != ""
assert os.getcwd() == os.getcwd()
assert "Cargo.toml" in os.listdir(os.getcwd())
assert os.listdir(".") == os.listdir(os.getcwd())
"#,
    );
}

#[test]
fn os_stat_is_attrtuple() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
import sys
assert type(os.stat(".")) is type(sys.implementation)
assert type(os.stat(".")) is not tuple
assert type(sys.implementation) is not tuple
assert os.stat(".") == os.stat(".")
assert os.stat(".") == os.lstat(".")
"#,
    );
    let (name, value) = fx.run_err("import os\nos.stat(\"nonexistent_xyz\")\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "2");
}

#[test]
fn os_urandom_limits() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
assert os.urandom(0) == b""
assert type(os.urandom(16)) is bytes
assert os.urandom(16) != b""
"#,
    );
    let (name, value) = fx.run_err("import os\nos.urandom(-1)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "22");
    let (name, value) = fx.run_err("import os\nos.urandom(5000)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "27");
}

#[test]
fn os_urandom_len_ctx() {
    // `len` is not a builtin on this base (D1 binds it), so
    // the length is checked at the ctx level: `urandom(16)`
    // returns a 16-byte `bytes`.
    let mut fx = Sx::new();
    let (mut ctx, state) = fx.vm.split();
    let os_id = static_qstr_id(b"os").expect("os is a static qstr");
    let os_mod = state
        .builtin_modules
        .iter()
        .find(|(q, _)| *q == os_id)
        .map(|(_, v)| *v)
        .expect("D2 registers os");
    let urandom_id = static_qstr_id(b"urandom").expect("urandom is a static qstr");
    let (fun, bound) = crate::runtime::vm_rt_name::load_method_maybe_ctx(
        &mut ctx, state, os_mod, urandom_id,
    )
    .expect("load urandom");
    assert!(bound.is_none());
    let fun = fun.expect("urandom found");
    let out = crate::runtime::vm_call::call_function_ctx(
        &mut ctx,
        state,
        fun,
        1,
        0,
        &[Value::SmallInt(16)],
    )
    .expect("urandom(16) succeeds");
    match out {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Bytes(b) => assert_eq!(b.bytes.len(), 16),
            other => panic!("urandom(16) is not bytes: {other:?}"),
        },
        other => panic!("urandom(16) is not a heap value: {other:?}"),
    }
    drop(ctx);
}

#[test]
fn os_environ_getenv() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
assert os.getenv("PETITPY_D2_TEST_MISSING") is None
assert os.getenv("PETITPY_D2_TEST_MISSING", "dflt") == "dflt"
assert os.getenv("PETITPY_D2_TEST_MISSING", default="dflt2") == "dflt2"
os.putenv("PETITPY_D2_TEST", "hello")
assert os.getenv("PETITPY_D2_TEST") == "hello"
e1 = os._environ()
e2 = os._environ()
assert e1 == e2
assert e1 is not e2
assert e1["PETITPY_D2_TEST"] == "hello"
os.unsetenv("PETITPY_D2_TEST")
assert os.getenv("PETITPY_D2_TEST") is None
"#,
    );
    let (name, value) = fx.run_err("import os\nos.putenv(\"\", \"x\")\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "22");
}

#[test]
fn os_access_bool() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
assert os.access("/tmp", os.F_OK) == True
assert os.access("/tmp", os.R_OK) == True
assert os.access("/tmp", os.W_OK) == True
assert os.access("/nonexistent_xyz", os.F_OK) == False
assert os.access("/tmp", 8) == False
assert os.access("/tmp", -1) == False
"#,
    );
}

#[test]
fn os_mkdir_rmdir_rename_remove() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
d = "/tmp/petitpy_d2_os_" + str(os.getpid())
os.mkdir(d)
assert os.listdir(d) == []
os.mkdir(d + "/sub")
assert os.listdir(d) == ["sub"]
os.rename(d + "/sub", d + "/sub2")
assert os.listdir(d) == ["sub2"]
fd = os.open(d + "/f", os.O_WRONLY | os.O_CREAT, 0o644)
os.close(fd)
l = os.listdir(d)
assert l == ["sub2", "f"] or l == ["f", "sub2"]
os.remove(d + "/f")
assert os.listdir(d) == ["sub2"]
assert os.remove is os.unlink
os.rmdir(d + "/sub2")
os.rmdir(d)
"#,
    );
    let (name, value) = fx.run_err("import os\nos.mkdir(\"/tmp\")\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "17");
    let (name, value) = fx.run_err("import os\nos.rmdir(\"/nonexistent_xyz\")\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "2");
}

#[test]
fn os_getattr_and_fixed_dict() {
    let mut fx = Sx::new();
    // `os.path` → import `os_path`, whose failure propagates
    // as ModuleNotFoundError (probed); `os.environ` goes
    // through `os_ext` (missing here) → AttributeError.
    let (name, value) = fx.run_err("import os\nos.path\n");
    assert_eq!(name, "ModuleNotFoundError");
    assert_eq!(value, "No module named 'os_path'");
    let (name, _) = fx.run_err("import os\nos.environ\n");
    assert_eq!(name, "AttributeError");
    let (name, _) = fx.run_err("import os\nos.nonexistent_xyz\n");
    assert_eq!(name, "AttributeError");
    // The os dict is fixed: store and delete both raise.
    let (name, _) = fx.run_err("import os\nos.foo = 42\n");
    assert_eq!(name, "AttributeError");
    let (name, _) = fx.run_err("import os\ndel os.sep\n");
    assert_eq!(name, "AttributeError");
}

#[test]
fn os_fd_roundtrip() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
d = "/tmp/petitpy_d2_fd_" + str(os.getpid())
os.mkdir(d)
fd = os.open(d + "/f", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
assert os.write(fd, b"hello") == 5
os.close(fd)
fd = os.open(d + "/f", os.O_RDONLY)
assert os.read(fd, 5) == b"hello"
assert os.read(fd, 5) == b""
assert os.lseek(fd, 0, os.SEEK_SET) == 0
assert os.read(fd, 2) == b"he"
assert os.lseek(fd, -1, os.SEEK_END) == 4
assert os.read(fd, 1) == b"o"
os.fsync(fd)
os.close(fd)
os.remove(d + "/f")
os.rmdir(d)
"#,
    );
    // A closed stdio fd raises EBADF on later use. On the
    // default build the fd is only recorded in
    // `os_closed_stdio` (the real fd is never closed, so the
    // test process itself is unaffected). Under
    // `--features unsafe-ffi` (G6a) the real `close(2)` runs
    // first — the test process's stdin fd IS really closed
    // afterwards, which is fine: no unit test reads real
    // stdin (`input()` is tested over Cursor/Vec in
    // builtin_fns_tests.rs, and `sys.stdin.fileno()` is a
    // VM-level constant).
    let (name, value) = fx.run_err("import os\nos.close(0)\nos.read(0, 5)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "9");
    let (name, value) = fx.run_err("import os\nos.read(999, 5)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "9");
    let (name, value) = fx.run_err("import os\nos.close(999)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "9");
    let (name, value) = fx.run_err("import os\nos.read(0, 5000)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "27");
    let (name, value) = fx.run_err("import os\nos.open(\"/tmp\", \"x\")\n");
    assert_eq!(name, "TypeError");
    assert_eq!(value, "can't convert to int");
}

/// `os.close` on an `open()` file's fd (G6a U3): the fd is
/// not in `os_fds`, so the default build raises EBADF; under
/// `--features unsafe-ffi` the real `close(2)` runs and the
/// close succeeds.
#[test]
fn os_close_open_file_fd() {
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let (name, value) =
            fx_run_err_new("f = open('/dev/null', 'r')\nimport os\nos.close(f.fileno())\n");
        assert_eq!(name, "OSError");
        assert_eq!(value, "9");
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        let mut fx = Sx::new();
        fx.run_ok(
            "f = open('/dev/null', 'r')\nimport os\nos.close(f.fileno())\nprint('os.close done')\n",
        );
    }
}

#[test]
fn os_fd_lseek_whence() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import os
d = "/tmp/petitpy_d2_lseek_" + str(os.getpid())
os.mkdir(d)
fd = os.open(d + "/f", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
assert os.write(fd, b"hello") == 5
assert os.lseek(fd, 0, os.SEEK_SET) == 0
assert os.lseek(fd, 2, os.SEEK_CUR) == 2
assert os.lseek(fd, 0, os.SEEK_END) == 5
os.close(fd)
os.remove(d + "/f")
os.rmdir(d)
"#,
    );
    let (name, value) = fx.run_err(
        "import os\nfd = os.open(\"/tmp\", os.O_RDONLY)\nos.lseek(fd, 0, 99)\n",
    );
    assert_eq!(name, "OSError");
    assert_eq!(value, "22");
    // `os.lseek(1, 0, 0)` on stdio: on the default build this
    // is always ESPIPE(29) (no `Seek` on stdio). Under
    // `--features unsafe-ffi` (G6a) the real `lseek(2)` runs,
    // so the outcome depends on what fd 1 is in the test
    // process — libtest's output capture makes it a regular
    // temp file (seek succeeds, returns 0), while a pipe/tty
    // gives ESPIPE. Both are correct kernel answers; the
    // piped case is pinned by the subprocess test
    // `g6a_lseek_stdio_file`/`lseek_stdio_pipe` probe instead.
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let (name, value) = fx.run_err("import os\nos.lseek(1, 0, 0)\n");
        assert_eq!(name, "OSError");
        assert_eq!(value, "29");
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        match fx.run("import os\nx = os.lseek(1, 0, 0)\n") {
            Ok(_) => {}
            Err(VmError::Raised(exc)) => {
                assert_eq!(fx.exc_name(exc), "OSError");
                assert_eq!(fx.exc_value(exc), "29");
            }
            Err(e) => panic!("module failed without raising: {e:?}"),
        }
    }
}

#[test]
fn os_unimplemented_stubs() {
    // The reference implements these; the port has no std API
    // for `umask` on the default build (see §4) — the stub raises
    // NotImplementedError so the gap is loud, not silent.
    // Under `--features unsafe-ffi` (G6a) the real `umask(2)`
    // runs instead: round-trip it and restore the mask in the
    // same program (no test asserts file modes, so the brief
    // process-global window is unobservable).
    // `kill`/`chown` shell out to `/bin` helpers instead
    // (F5b): `kill(1, 0)` succeeds (pid 1 exists), `chown`
    // of /tmp to 0:0 succeeds as root.
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        let (name, _) = fx_run_err_new("import os\nos.umask(0o022)\n");
        assert_eq!(name, "NotImplementedError");
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        let mut fx = Sx::new();
        fx.run_ok(
            "import os\nprev = os.umask(0o022)\ncur = os.umask(prev)\nassert cur == 0o022\n",
        );
        // `os.umask(-1)` wraps (`as u32` -> kernel masks to
        // 0777) and returns the previous mask; the reference
        // ABORTS there (Zig @intCast panic, rc=134), N1.
        let mut fx = Sx::new();
        fx.run_ok("import os\nprev = os.umask(0o022)\ncur = os.umask(-1)\nassert cur == 0o022\nos.umask(prev)\n");
    }
    let mut fx = Sx::new();
    // `run_ok` panics if either call raises.
    fx.run_ok("import os\nos.kill(1, 0)\nos.chown(\"/tmp\", 0, 0)\n");
}

/// `run_err` on a fresh `Sx` (each stub test gets its own VM
/// so a raised `NotImplementedError` cannot leak state).
fn fx_run_err_new(src: &str) -> (String, String) {
    let mut fx = Sx::new();
    fx.run_err(src)
}

/// A fresh VM with the `io` module registered (C18b's
/// `setup_io` pattern), plus the `Fx` read-back helpers.
/// Copied from `modsys_tests.rs` so the `os` tests are not
/// blocked on D1's `import io` registration.
struct Sx {
    vm: Vm,
}

fn open_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

impl Sx {
    fn new() -> Self {
        let mut vm = Vm::new();
        let fun_var = vm.state.vm_types.fun_builtin_var;
        let module_type = vm.state.vm_types.module;
        let io_module = {
            let (mut ctx, state) = vm.split();
            let open_id = static_qstr_id(b"open").expect("open is a static qstr");
            let open = ctx.alloc(
                Some(fun_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name: open_id,
                    n_args: fun_n_args(1, 3),
                    func: open_stub,
                    kw_func: None,
                }),
            );
            let m = build_io_module(&mut ctx, fun_var, module_type, Value::Heap(open))
                .expect("io module builds");
            let io_id = static_qstr_id(b"io").expect("io is a static qstr");
            state.builtin_modules.push((io_id, m));
            m
        };
        vm.pin(io_module);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => panic!(
                "module raised {}: {:?}",
                self.exc_name(exc),
                self.exc_value(exc)
            ),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn run_err(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module unexpectedly succeeded: {src}"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Err(e) => panic!("module failed without raising: {e:?} ({src})"),
        }
    }

    fn exc_name(&mut self, exc: Value) -> String {
        let t = obj_get_type(&self.vm.arena, &self.vm.types, exc)
            .expect("an exception has a type");
        match self.vm.arena.get(t) {
            Obj::Type(d) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(d.name as u16)).into_owned()
            }
            _ => panic!("exception type is not a type object"),
        }
    }

    fn exc_value(&mut self, exc: Value) -> String {
        let v = exception_get_value(&self.vm.arena, exc);
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("exception value is not a str: {other:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}

