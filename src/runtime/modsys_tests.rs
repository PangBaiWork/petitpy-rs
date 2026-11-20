//! Tests for Lane D2: the `sys` module (`modsys.zig`).
//!
//! End-to-end through `Fx` (the dispatch path, where
//! `import sys` is live) + one `Vm`-level test for the
//! `set_sys_path` list-object sync. Expected values are the
//! reference binary's, probed 2026-11-18 (see
//! `scripts/d2_result.txt` §2).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::{Vm, VmError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn sys_constants() {
    let mut fx = Sx::new();
    // `sys.platform` is the OS tag (`modsys.rs`): "linux" on
    // Linux, "darwin" on macOS, "windows" on Windows.
    #[cfg(target_os = "linux")]
    let platform = "linux";
    #[cfg(target_os = "macos")]
    let platform = "darwin";
    #[cfg(target_os = "windows")]
    let platform = "windows";
    fx.run_ok(&format!(
        r#"
import sys
assert sys.version == "3.4.0; MicroPython (with v2.0 preview) v1.28.0-preview.80.gb14d129a16 on 2026-06-04"
assert sys.version_info == (3, 4, 0)
assert sys.platform == "{platform}"
assert sys.byteorder == "little"
assert sys.maxsize == 9223372036854775807
assert sys.implementation.name == "micropython"
assert sys.implementation.version == (1, 28, 0, "preview")
assert sys.implementation._machine == "petitpy [GCC 4.2.1 (Apple Inc. build 5658) (LLVM build)] version"
assert sys.implementation._mpy == 413773
assert type(sys.argv) is list
assert type(sys.modules) is dict
assert "sys" not in sys.modules
assert "os" not in sys.modules
assert type(sys.path) is list
"#,
    ));
}

#[test]
fn sys_stdio_objects() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
assert type(sys.stdin).__name__ == "TextIOWrapper"
assert type(sys.stdout).__name__ == "TextIOWrapper"
assert type(sys.stderr).__name__ == "TextIOWrapper"
assert sys.stdin.fileno() == 0
assert sys.stdout.fileno() == 1
assert sys.stderr.fileno() == 2
assert type(sys.stdin.buffer).__name__ == "FileIO"
assert sys.stdin.buffer.fileno() == 0
assert sys.stdin.buffer is sys.stdin.buffer
assert sys.stdout.buffer is sys.stdout.buffer
assert str(sys.stdin) == "<io.TextIOWrapper 0>"
assert str(sys.stdout) == "<io.TextIOWrapper 1>"
assert str(sys.stdin.buffer) == "<io.FileIO 0>"
"#,
    );
}

#[test]
fn sys_exc_info_always_none() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
assert sys.exc_info() == (None, None, None)
try:
    raise ValueError("x")
except ValueError:
    assert sys.exc_info() == (None, None, None)
    try:
        raise TypeError("y")
    except TypeError:
        assert sys.exc_info() == (None, None, None)
assert sys.exc_info() == (None, None, None)
"#,
    );
}

#[test]
fn sys_exit_forms() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
try:
    sys.exit()
    assert False
except SystemExit as e:
    assert e.args == ()
try:
    sys.exit(3)
    assert False
except SystemExit as e:
    assert e.args == (3,)
try:
    sys.exit("bye")
    assert False
except SystemExit as e:
    assert e.args == ("bye",)
"#,
    );
}

#[test]
fn sys_intern() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
assert sys.intern("micropython") == "micropython"
assert sys.intern("micropython") is "micropython"
assert sys.intern(b"abc") == "abc"
"#,
    );
    assert_eq!(
        fx.run_err("import sys\nsys.intern(0)"),
        ("TypeError".to_string(), "can't convert to str implicitly".to_string())
    );
    assert_eq!(
        fx.run_err("import sys\nsys.intern()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

#[test]
fn sys_recursionlimit() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
assert sys.getrecursionlimit() == 1000
assert sys.setrecursionlimit(500) is None
assert sys.getrecursionlimit() == 1000
assert sys.setrecursionlimit("not an int") is None
"#,
    );
}

#[test]
fn sys_path_delegation() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys
assert "path" not in sys.__dict__
assert "ps1" not in sys.__dict__
assert "modules" in sys.__dict__
assert "argv" in sys.__dict__
sys.path = ["/tmp/xyz"]
assert sys.path == ["/tmp/xyz"]
"#,
    );
    assert_eq!(
        fx.run_err("import sys\nsys.foo = 42"),
        (
            "AttributeError".to_string(),
            "no attribute 'foo'".to_string()
        )
    );
    assert_eq!(
        fx.run_err("import sys\ndel sys.path"),
        (
            "AttributeError".to_string(),
            "no attribute 'path'".to_string()
        )
    );
    assert_eq!(
        fx.run_err("import sys\ndel sys.nonexistent"),
        (
            "AttributeError".to_string(),
            "no attribute 'nonexistent'".to_string()
        )
    );
    assert_eq!(
        fx.run_err("import sys\nsys.ps1"),
        (
            "AttributeError".to_string(),
            "no attribute 'ps1'".to_string()
        )
    );
}

/// A per-test temp dir, removed on drop (the `import_tests.rs`
/// pattern).
struct TmpDir {
    path: PathBuf,
}

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

impl TmpDir {
    fn new() -> Self {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("petitpy-sys-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn write(&self, rel: &str, contents: &str) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(p, contents).expect("write test file");
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn sys_modules_sync_and_path_append() {
    let dir = TmpDir::new();
    dir.write("mymod.py", "VALUE = 42\n");
    let mut fx = Sx::new();
    let path = dir.path.to_string_lossy().into_owned();
    fx.run_ok(&format!(
        r#"
import sys
assert "sys" not in sys.modules
assert "os" not in sys.modules
sys.path.append("{path}")
import mymod
assert mymod.VALUE == 42
assert "mymod" in sys.modules
assert sys.modules["mymod"] is mymod
"#,
        path = path.replace('\\', "\\\\").replace('"', "\\\"")
    ));
}

#[test]
fn sys_print_exception_to_stream() {
    // Needs `io.StringIO` as the target; `import io` is only
    // live in this test VM because the harness registers the
    // io module the way D1 will (C18b's `setup_io` pattern —
    // `import io` is D1's registration, not D2's).
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import sys, io
s = io.StringIO()
sys.print_exception(ValueError("boom"), s)
assert "boom" in s.getvalue()
assert "ValueError" in s.getvalue()
"#,
    );
    // The 1-arg form writes to the real stdout (one traceback
    // line in the test log) and returns None.
    fx.run_ok("import sys\nassert sys.print_exception(ValueError(\"quiet\")) is None\n");
    assert_eq!(
        fx.run_err("import sys\nsys.print_exception()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

/// A fresh VM with the `io` module registered (C18b's
/// `setup_io` pattern), plus the `Fx` read-back helpers.
/// `io` is registered here because `sys.print_exception`
/// needs a real stream target and `import io` is D1's
/// registration — this harness pre-registers it the way D1
/// will, so the `sys` tests are not blocked on D1.
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
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}

#[test]
fn set_sys_path_syncs_list_object() {
    let mut vm = Vm::new();
    vm.set_sys_path(vec![b"/tmp/abc".to_vec(), b"".to_vec()]);
    let (ctx, state) = vm.split();
    assert_eq!(state.sys_path, vec![b"/tmp/abc".to_vec(), b"".to_vec()]);
    let list_ref = state.sys_path_list.expect("D2 builds sys_path_list");
    let items = match ctx.arena.get(list_ref) {
        Obj::List(l) => l.items.clone(),
        other => panic!("sys_path_list is not a list: {other:?}"),
    };
    assert_eq!(items.len(), 2);
    // The items are the new paths (str values), in order.
    let first = match items[0] {
        Value::Qstr(q) => String::from_utf8_lossy(ctx.pool.qstr_str(q as u16)).into_owned(),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
            other => panic!("sys.path[0] is not a str: {other:?}"),
        },
        other => panic!("sys.path[0] is not a str: {other:?}"),
    };
    assert_eq!(first, "/tmp/abc");
    drop(ctx);
}
