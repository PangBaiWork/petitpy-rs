//! Tests for Lane C15b: the import system (`builtinimport.zig` +
//! `import_search.zig` + `persistentcode.zig` load +
//! `zig_compile_bridge.zig` + `scheduler.zig` wiring).
//!
//! Runs small Python snippets through `Vm::load_module_from_source`
//! (the dispatch path, where `IMPORT_NAME` / `IMPORT_FROM` /
//! `IMPORT_STAR` and the `__import__` builtin are live) against
//! real files in a per-test temp dir (`sys.path`) and frozen
//! `.mpy` blobs compiled with the Rust compiler.

use crate::compiler::bridge::compile_to_mpy_named;
use crate::compiler::parser::InputKind;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::obj::obj_get_type;
use crate::objects::value::{Obj, Value};
use crate::runtime::frozen_modules::FrozenTable;
use crate::runtime::vm::{Vm, VmError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh VM per test, plus the read-back helpers (the
/// `vm_tests.rs` `Fx` / `runtime_glue_tests_b.rs` `Vx` pattern).
struct Ix {
    vm: Vm,
}

impl Ix {
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
                self.exc_args(exc)
            ),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, args).
    fn raised(&mut self, src: &str) -> (String, Vec<Value>) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_args(exc)),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, message).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        let (name, args) = self.raised(src);
        let msg = match args.first() {
            Some(v) => self.val_str(*v),
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

    fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not a small int: {other:?}"),
        }
    }

    fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    fn gs(&mut self, name: &str) -> String {
        match self.g(name) {
            Some(v) => self.val_str(v),
            None => panic!("global {name} is absent"),
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

    /// The exception's args, as values.
    fn exc_args(&mut self, exc: Value) -> Vec<Value> {
        match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => d.args.clone(),
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        }
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String` — for asserting exception messages.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("value is not a str: {other:?}"),
            },
            other => panic!("value is not a str: {other:?}"),
        }
    }

    /// Point `sys.path` at these dirs (only).
    fn set_path(&mut self, dirs: &[PathBuf]) {
        let paths: Vec<String> = dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        self.vm.set_sys_path(paths);
    }

    /// Install a frozen table from `(name, source)` pairs
    /// (each source compiled to `.mpy` with the Rust compiler).
    fn freeze(&mut self, entries: &[(&str, &str)]) {
        let compiled: Vec<(String, Vec<u8>)> = entries
            .iter()
            .map(|(name, src)| {
                let mpy = compile_to_mpy_named(src, InputKind::FileInput, name)
                    .expect("frozen source compiles");
                (name.to_string(), mpy)
            })
            .collect();
        self.vm
            .set_frozen_modules(FrozenTable::from_entries(compiled));
    }
}

/// A per-test temp dir, removed on drop.
struct TmpDir {
    path: PathBuf,
}

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

impl TmpDir {
    fn new() -> Self {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("petitpy-import-test-{}-{n}", std::process::id()));
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

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(p, bytes).expect("write test file");
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Compile source to `.mpy` bytes (for `.mpy`-file tests).
fn mpy_bytes(src: &str, name: &str) -> Vec<u8> {
    compile_to_mpy_named(src, InputKind::FileInput, name).expect("source compiles")
}

// ── frozen modules ───────────────────────────────────────────────────

#[test]
fn frozen_module_loads() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    ix.freeze(&[("fmod", "VAL = 7\n")]);
    ix.run_ok("import fmod\nx = fmod.VAL\n");
    assert_eq!(ix.gi("x"), 7);
}

#[test]
fn frozen_module_identity() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    ix.freeze(&[("fmod", "VAL = 7\n")]);
    ix.run_ok("import fmod\nimport fmod as f2\nx = fmod is f2\n");
    assert!(ix.gb("x"));
}

#[test]
fn frozen_package_path_is_empty() {
    // A frozen package (`fpkg` with a `fpkg.sub` entry) gets an
    // empty `__path__` — and its submodule still loads frozen.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    ix.freeze(&[("fpkg", "VAL = 1\n"), ("fpkg.sub", "VAL = 2\n")]);
    ix.run_ok("import fpkg\nx = fpkg.__path__\n");
    assert_eq!(ix.gs("x"), "");
    ix.run_ok("from fpkg import sub\ny = sub.VAL\n");
    assert_eq!(ix.gi("y"), 2);
}

#[test]
fn frozen_from_import_missing_name() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    ix.freeze(&[("fmod", "VAL = 7\n")]);
    let (name, msg) = ix.raised_msg("from fmod import NOPE\n");
    assert_eq!(name, "ImportError");
    assert_eq!(msg, "can't import name 'NOPE'");
}

#[test]
fn frozen_beats_syspath() {
    // Frozen lookup runs before the filesystem search.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("dup.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.freeze(&[("dup", "VAL = 1\n")]);
    ix.run_ok("import dup\nx = dup.VAL\n");
    assert_eq!(ix.gi("x"), 1);
}

#[test]
fn frozen_bad_mpy_is_value_error() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    ix.vm.set_frozen_modules(FrozenTable::from_entries(vec![(
        "bad".to_string(),
        b"garbage bytes, not an mpy".to_vec(),
    )]));
    let (name, msg) = ix.raised_msg("import bad\n");
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "incompatible .mpy file");
}

// ── sys.path .py files ───────────────────────────────────────────────

#[test]
fn py_module_loads() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import m1\nx = m1.VAL\n");
    assert_eq!(ix.gi("x"), 42);
}

#[test]
fn py_module_identity() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import m1\nimport m1 as m2\nx = m1 is m2\n");
    assert!(ix.gb("x"));
}

#[test]
fn py_from_import() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("from m1 import VAL\nx = VAL\n");
    assert_eq!(ix.gi("x"), 42);
}

#[test]
fn py_from_import_missing_name() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("from m1 import NOPE\n");
    assert_eq!(name, "ImportError");
    assert_eq!(msg, "can't import name 'NOPE'");
}

#[test]
fn py_dotted_import() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import pkg.sub\nx = pkg.sub.VAL\ny = pkg.VAL\n");
    assert_eq!(ix.gi("x"), 2);
    assert_eq!(ix.gi("y"), 1);
}

#[test]
fn py_dotted_from_import() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("from pkg.sub import VAL\nx = VAL\n");
    assert_eq!(ix.gi("x"), 2);
}

#[test]
fn py_package_path() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import pkg\nx = pkg.__path__\n");
    assert!(ix.gs("x").ends_with("pkg"));
}

#[test]
fn py_submodule_attr_hookup() {
    // `import pkg.sub` hooks `sub` onto `pkg` (vendor
    // `builtinimport.c:524`) — attribute access works after.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import pkg.sub\nx = pkg.sub.VAL\n");
    assert_eq!(ix.gi("x"), 2);
}

#[test]
fn py_not_a_package() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("import m1.sub\n");
    assert_eq!(name, "ModuleNotFoundError");
    assert_eq!(msg, "No module named 'm1.sub'; 'm1' is not a package");
}

#[test]
fn py_missing_module() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("import nope_xyz\n");
    assert_eq!(name, "ModuleNotFoundError");
    assert_eq!(msg, "No module named 'nope_xyz'");
}

#[test]
fn py_missing_dotted_top() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("import nope_xyz.sub\n");
    assert_eq!(name, "ModuleNotFoundError");
    assert_eq!(msg, "No module named 'nope_xyz'");
}

#[test]
fn py_namespace_package() {
    // A directory with no `__init__.py` still imports as a
    // (namespace) package; its submodules load from the dir.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("nsp/sub.py", "VAL = 3\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import nsp.sub\nx = nsp.sub.VAL\n");
    assert_eq!(ix.gi("x"), 3);
}

#[test]
fn py_syntax_error_in_module() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "x = 1+\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("import m1\n");
    assert_eq!(name, "SyntaxError");
    assert_eq!(msg, "invalid syntax");
}

#[test]
fn py_mpy_file_loads() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write_bytes("m2.mpy", &mpy_bytes("VAL = 9\n", "m2"));
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import m2\nx = m2.VAL\n");
    assert_eq!(ix.gi("x"), 9);
}

#[test]
fn py_prefers_py_over_mpy() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m3.py", "VAL = 1\n");
    dir.write_bytes("m3.mpy", &mpy_bytes("VAL = 2\n", "m3"));
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import m3\nx = m3.VAL\n");
    assert_eq!(ix.gi("x"), 1);
}

#[test]
fn py_module_body_runs_in_own_globals() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 1\ndef f():\n    return VAL\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("VAL = 99\nimport m1\nx = m1.f()\n");
    assert_eq!(ix.gi("x"), 1);
}

#[test]
fn py_import_star() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 5\n_HID = 6\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("from m1 import *\nx = VAL\n");
    assert_eq!(ix.gi("x"), 5);
    assert_eq!(ix.g("HID"), None);
    assert_eq!(ix.g("_HID"), None);
}

#[test]
fn py_circular_import() {
    // `a` imports `b`, `b` imports the partial `a` — the
    // module is registered BEFORE its body runs, so the cycle
    // completes instead of recursing forever.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("a.py", "import b\nVAL_A = 1\n");
    dir.write("b.py", "import a\nVAL_B = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("import a\nimport b\nx = a.VAL_A + b.VAL_B\n");
    assert_eq!(ix.gi("x"), 3);
}

#[test]
fn py_failed_import_removed_from_sys_modules() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "raise ValueError('boom')\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, _) = ix.raised_msg("import m1\n");
    assert_eq!(name, "ValueError");
    // Fix the file: the second import must load it (the failed
    // attempt left nothing registered).
    dir.write("m1.py", "VAL = 8\n");
    ix.run_ok("import m1\nx = m1.VAL\n");
    assert_eq!(ix.gi("x"), 8);
}

#[test]
fn py_import_in_function() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("def f():\n    import m1\n    return m1.VAL\nx = f()\n");
    assert_eq!(ix.gi("x"), 42);
}

#[test]
fn py_import_star_missing_module() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("from nope_xyz import *\n");
    assert_eq!(name, "ModuleNotFoundError");
    assert_eq!(msg, "No module named 'nope_xyz'");
}

#[test]
fn py_from_package_import_submodule() {
    // Attribute miss on a package falls back to a submodule
    // import (`import_from`'s `__path__` branch).
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("from pkg import sub\nx = sub.VAL\n");
    assert_eq!(ix.gi("x"), 2);
}

#[test]
fn py_from_package_import_attr() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("from pkg import VAL\nx = VAL\n");
    assert_eq!(ix.gi("x"), 1);
}

#[test]
fn py_relative_import_statement() {
    // `from . import x` compiles to a level-1 import, which the
    // original rejects with `NotImplementedError`.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("from . import x\n");
    assert_eq!(name, "NotImplementedError");
    assert_eq!(msg, "relative import");
}

// ── __import__ builtin ───────────────────────────────────────────────

#[test]
fn dunder_import_simple() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("x = __import__('m1').VAL\n");
    assert_eq!(ix.gi("x"), 42);
}

#[test]
fn dunder_import_dotted_returns_top() {
    // `fromlist` is `None` → the TOP module.
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("m = __import__('pkg.sub')\nx = m.VAL\n");
    assert_eq!(ix.gi("x"), 1);
}

#[test]
fn dunder_import_fromlist_returns_leaf() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("m = __import__('pkg.sub', {}, {}, ('VAL',))\nx = m.VAL\n");
    assert_eq!(ix.gi("x"), 2);
}

#[test]
fn dunder_import_empty_fromlist_returns_leaf() {
    // The Zig's rule is `isNone(fromtuple)` — an EMPTY tuple is
    // not `None`, so it returns the LEAF (diverges from
    // CPython, where `()` returns the top).
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("pkg/__init__.py", "VAL = 1\n");
    dir.write("pkg/sub.py", "VAL = 2\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("m = __import__('pkg.sub', {}, {}, ())\nx = m.VAL\n");
    assert_eq!(ix.gi("x"), 2);
}

#[test]
fn dunder_import_level_rejected() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("__import__('m1', {}, {}, [], 1)\n");
    assert_eq!(name, "NotImplementedError");
    assert_eq!(msg, "relative import");
}

#[test]
fn dunder_import_empty_name() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("__import__('')\n");
    assert_eq!(name, "ImportError");
    assert_eq!(msg, "module not found");
}

#[test]
fn dunder_import_nonstring_name() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("__import__(123)\n");
    assert_eq!(name, "ImportError");
    assert_eq!(msg, "module not found");
}

#[test]
fn dunder_import_zero_args() {
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("__import__()\n");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "argument num/types mismatch");
}

#[test]
fn dunder_import_kwargs_rejected() {
    // Intercepted builtins take no kwargs (the `eval` precedent).
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    let (name, msg) = ix.raised_msg("__import__('m1', level=0)\n");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "argument num/types mismatch");
}

#[test]
fn dunder_import_globals_ignored() {
    // `importImpl` never reads `globals`/`locals` — a non-dict
    // second arg is NOT an error (the Zig ignores it).
    let mut ix = Ix::new();
    let dir = TmpDir::new();
    dir.write("m1.py", "VAL = 42\n");
    ix.set_path(&[dir.path.clone()]);
    ix.run_ok("x = __import__('m1', 'not-a-dict').VAL\n");
    assert_eq!(ix.gi("x"), 42);
}

// ── compile bridge ───────────────────────────────────────────────────

#[test]
fn compile_syntax_beats_notimplemented() {
    // C15a's `compile_glue` raised `NotImplementedError` without
    // compiling; the bridge compiles first, so a syntax error
    // wins (as on the original).
    let mut ix = Ix::new();
    let (name, msg) = ix.raised_msg("compile('1+', 'f', 'eval')\n");
    assert_eq!(name, "SyntaxError");
    assert_eq!(msg, "invalid syntax");
}

#[test]
fn compile_valid_returns_code() {
    // E1: `compile()` returns a code object (the C15a
    // `NotImplementedError` gap is closed): `repr(c)` is
    // `<code>` (the reference prints `<>` — its stub type
    // has name qstr 0; see `objcode.rs`), `type(c).__name__`
    // is `"code"`, and `eval(c)` runs it.
    let mut ix = Ix::new();
    ix.run_ok("c = compile('1', 'f', 'eval')\nr = repr(c)\nn = type(c).__name__\nv = eval(c)\n");
    let r = ix.g("r").unwrap();
    assert_eq!(ix.val_str(r), "<code>");
    let n = ix.g("n").unwrap();
    assert_eq!(ix.val_str(n), "code");
    assert_eq!(ix.gi("v"), 1);
}

#[test]
fn compile_indent_error_says_invalid_syntax() {
    // C15a correction: the original says "invalid syntax" for
    // BOTH syntax and indent errors (verified on the reference
    // binary); the old mapping said "unexpected indent".
    let mut ix = Ix::new();
    let (name, msg) = ix.raised_msg("compile(' x = 1', 'f', 'exec')\n");
    assert_eq!(name, "IndentationError");
    assert_eq!(msg, "invalid syntax");
}

// ── persistentcode ───────────────────────────────────────────────────

#[test]
fn persistentcode_bad_arch_is_value_error() {
    // A `.mpy` with a non-NONE arch is rejected at load
    // (`persistentcode.zig:419-426`).
    let mut good = mpy_bytes("VAL = 1\n", "badarch");
    assert_eq!(good[0], b'M');
    good[2] = 0x04; // arch = (0x04 >> 2) & 0x2F = 1
    let mut ix = Ix::new();
    let res = ix.vm.load_module_from_mpy(&good, "badarch");
    match res {
        Err(VmError::Raised(exc)) => {
            assert_eq!(ix.exc_name(exc), "ValueError");
            let args = ix.exc_args(exc);
            assert_eq!(args.len(), 1);
            assert_eq!(ix.val_str(args[0]), "incompatible .mpy file");
        }
        other => panic!("expected a raised ValueError, got {other:?}"),
    }
}

#[test]
fn persistentcode_bad_small_int_bits_is_value_error() {
    let mut good = mpy_bytes("VAL = 1\n", "badbits");
    good[3] = 0xFF; // small-int bits 255 > 63
    let mut ix = Ix::new();
    let res = ix.vm.load_module_from_mpy(&good, "badbits");
    match res {
        Err(VmError::Raised(exc)) => {
            assert_eq!(ix.exc_name(exc), "ValueError");
            let args = ix.exc_args(exc);
            assert_eq!(ix.val_str(args[0]), "incompatible .mpy file");
        }
        other => panic!("expected a raised ValueError, got {other:?}"),
    }
}
