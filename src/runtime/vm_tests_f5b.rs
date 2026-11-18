//! Lane F5b regression tests: generator globals in imported
//! modules + the misc single points (socket fromfd, subprocess
//! streams, os kill/chown).
//!
//! 1. Generator resume switches globals to the generator
//!    function's module globals (objgenerator.zig:221-227) —
//!    without it a generator defined in an imported module
//!    resolves NO module globals (`NameError`), even before the
//!    first yield, while plain functions in the same module work
//!    (`itertools.zip_longest` calling module-global `repeat`).
//! 2. `socket.fromfd` registers the given fd in the socket table.
//! 3. `subprocess.run` honours `stderr=STDOUT` and text mode.
//! 4. `os.kill` / `os.chown` raise the reference's errors instead
//!    of `NotImplementedError`.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs`, `vm_tests_e2f.rs`,
//! `vm_tests_e2g.rs`, `vm_tests_e2h.rs`, `vm_tests_e2i.rs`,
//! `vm_tests_e2j.rs`, `vm_tests_f3.rs` and `vm_tests_f4.rs`).

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM per test, plus the read-back helpers (the
/// `vm_tests.rs` `Fx` pattern, copied — that module's `Fx`
/// is private).
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

    /// Load a synthetic module under `name` (registered in
    /// `state.modules`, so `import name` finds it), then run
    /// `src` as the main module.
    fn run_with_mod(&mut self, name: &str, mod_src: &str, src: &str) -> Value {
        self.vm
            .load_module_from_source(mod_src, name)
            .unwrap_or_else(|e| panic!("module {name} failed to load: {e:?}"));
        self.run_ok(src)
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not a small int: {other:?}"),
        }
    }

    /// The exception's type name (`"ValueError"`, …).
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

    /// The exception's first arg (the message), as a string.
    fn exc_value(&mut self, exc: Value) -> String {
        let v = exception_get_value(&self.vm.arena, exc);
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => format!("{other:?}"),
            },
            other => format!("{other:?}"),
        }
    }
}

const MYMOD2: &str = r#"
def helper(v):
    return v * 10

def gen_first():
    yield helper(1)

def gen_plain():
    x = helper(2)
    yield x
"#;

const MYMOD: &str = r#"
def helper(v):
    return v * 10

def gen():
    it = iter([1])
    try:
        v = next(it)
    except StopIteration:
        v = helper(5)
    yield v
    try:
        v = next(it)
    except StopIteration:
        v = helper(7)
    yield v

def plain():
    return helper(3)

def inner():
    yield helper(1)

def outer():
    yield from inner()
"#;

/// A generator in an imported module resolves its module's
/// globals on the FIRST lookup, before any yield.
#[test]
fn f5b_gen_globals_before_first_yield() {
    let mut fx = Fx::new();
    fx.run_with_mod(
        "mymod2",
        MYMOD2,
        "import mymod2\na = sum(list(mymod2.gen_first()))\nb = sum(list(mymod2.gen_plain()))",
    );
    assert_eq!(fx.gi("a"), 10);
    assert_eq!(fx.gi("b"), 20);
}

/// … and after a yield, inside a try/except StopIteration, and
/// through `yield from` (nested resume re-derives the globals).
#[test]
fn f5b_gen_globals_after_yield_and_yield_from() {
    let mut fx = Fx::new();
    fx.run_with_mod(
        "mymod",
        MYMOD,
        "import mymod\nc = sum(list(mymod.gen()))\nd = mymod.plain()\ne = sum(list(mymod.outer()))",
    );
    assert_eq!(fx.gi("c"), 71);
    assert_eq!(fx.gi("d"), 30);
    assert_eq!(fx.gi("e"), 10);
}

/// The case that found the bug: `itertools.zip_longest` calls
/// module-global `repeat`.
#[test]
fn f5b_itertools_zip_longest_repeat() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import itertools\nf = sum(t[0] for t in itertools.zip_longest([1,2],[3],fillvalue=0))\ng = sum(t[1] for t in itertools.zip_longest([1,2],[3],fillvalue=0))",
    );
    assert_eq!(fx.gi("f"), 3);
    assert_eq!(fx.gi("g"), 3);
}

/// `socket.fromfd` wraps the given fd; `fileno()` returns it.
#[test]
fn f5b_socket_fromfd_fileno() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import socket, os\nfd = os.open('/dev/null', os.O_RDONLY)\ns = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\na = (s.fileno() == fd)\ns.close()\n",
    );
    assert_eq!(fx.g("a"), Some(Value::Bool(true)));
}

/// `subprocess.run` with `stderr=STDOUT` merges stderr into the
/// captured stdout, and `text=True` decodes it.
#[test]
fn f5b_subprocess_stderr_stdout_text() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import subprocess\nr = subprocess.run(['/bin/sh','-c','echo $NPY_E; echo err >&2'], env={'NPY_E':'e1'}, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)\nok = (r.stdout == 'e1\\nerr\\n' and r.stderr is None)",
    );
    assert_eq!(fx.g("ok"), Some(Value::Bool(true)));
}

/// `os.kill` of a nonexistent pid raises OSError (the reference
/// is a direct libc `kill` + `raiseOSError(errno)` — errno 3
/// ESRCH), not NotImplementedError. The integration case uses
/// pid -99999.
#[test]
fn f5b_os_kill_esrch_raises() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import os\ntry:\n    os.kill(-99999, 0)\n    ok = False\nexcept OSError:\n    ok = True\n",
    );
    assert_eq!(fx.g("ok"), Some(Value::Bool(true)));
}

/// `os.chown` of a path to its own uid/gid is a no-op (the
/// integration case creates its own file first).
#[test]
fn f5b_os_chown_self_noop() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import os\np = '/tmp/nanopy_chown_f5b'\nfd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)\nos.close(fd)\nst = os.stat(p)\nos.chown(p, st.st_uid, st.st_gid)\nok = True\nos.remove(p)\n",
    );
    assert_eq!(fx.g("ok"), Some(Value::Bool(true)));
}
