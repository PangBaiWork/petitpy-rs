//! End-to-end builtin tests (D1), part 2: `iter` / `all` /
//! `any` / `sum`, `min` / `max`, `sorted`, the `io` module
//! registration, the `open` stub, and the builtins-table spot
//! check. The `Fx` harness is copied from
//! `builtin_vm_tests.rs` (that module's `Fx` is private — the
//! same duplication as `vm_tests.rs` / `vm_tests_b.rs`).


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

    /// Run a module that must raise; return (type name, message).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
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

    /// A global string (`Qstr` literal or heap `Str`), as a
    /// Rust `String`.
    fn gs(&mut self, name: &str) -> String {
        match self.g(name) {
            Some(Value::Qstr(q)) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("global {name} is not a str: {other:?}"),
            },
            other => panic!("global {name} is not a str: {other:?}"),
        }
    }

    /// A global list's items.
    fn glist(&mut self, name: &str) -> Vec<Value> {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::List(d) => d.items.clone(),
                other => panic!("global {name} is not a list: {other:?}"),
            },
            other => panic!("global {name} is not a list: {other:?}"),
        }
    }

    /// A global list of strings, as Rust `String`s.
    fn gstrlist(&mut self, name: &str) -> Vec<String> {
        self.glist(name)
            .into_iter()
            .map(|v| match v {
                Value::Qstr(q) => {
                    String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
                }
                Value::Heap(r) => match self.vm.arena.get(r) {
                    Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                    other => panic!("list item is not a str: {other:?}"),
                },
                other => panic!("list item is not a str: {other:?}"),
            })
            .collect()
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

    /// The exception's first arg (the message), as a string.
    fn exc_value(&mut self, exc: Value) -> String {
        let v = exception_get_value(&self.vm.arena, exc);
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                _ => format!("{v:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            _ => format!("{v:?}"),
        }
    }
}

// ── iter / all / any / sum ───────────────────────────────────────

#[test]
fn iter_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("it = iter([1, 2])\na = next(it)\nb = next(it)\nc = next(it, 'dflt')\nd = iter('ab')\ne = next(d)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gs("c"), "dflt");
    assert_eq!(fx.gs("e"), "a");
    assert_eq!(
        fx.raised_msg("iter(1)"),
        ("TypeError".to_string(), "object not iterable".to_string())
    );
}

#[test]
fn all_any() {
    let mut fx = Fx::new();
    fx.run_ok("a = all([True, True])\nb = all([True, False])\nc = all([])\nd = any([False, True])\ne = any([])\nf = all(x > 0 for x in [1, 2, 3])\ng = any(x > 2 for x in [1, 2, 3])\nh = any(x > 5 for x in [1, 2, 3])\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(!fx.gb("e"));
    assert!(fx.gb("f"));
    assert!(fx.gb("g"));
    assert!(!fx.gb("h"));
    assert_eq!(
        fx.raised_msg("all(1)"),
        ("TypeError".to_string(), "object not iterable".to_string())
    );
}

#[test]
fn sum_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = sum([1, 2, 3])\nb = sum([1, 2], 10)\nc = sum([])\nd = sum(['a', 'b'], '')\ne = sum([1, 2], start=10)\nf = sum(range(5))\n");
    assert_eq!(fx.gi("a"), 6);
    assert_eq!(fx.gi("b"), 13);
    assert_eq!(fx.gi("c"), 0);
    assert_eq!(fx.gs("d"), "ab");
    assert_eq!(fx.gi("e"), 13);
    assert_eq!(fx.gi("f"), 10);
}

#[test]
fn sum_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("sum(['a'])"),
        (
            "TypeError".to_string(),
            "unsupported types for __add__: 'int', 'str'".to_string()
        )
    );
    for src in ["sum([1], foo=1)", "sum([1], 2, start=3)"] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "argument num/types mismatch".to_string()
            )
        );
    }
    assert_eq!(
        fx.raised_msg("sum(1)"),
        ("TypeError".to_string(), "object not iterable".to_string())
    );
}

// ── min / max ────────────────────────────────────────────────────

#[test]
fn min_max() {
    let mut fx = Fx::new();
    fx.run_ok("a = min(3, 1, 2)\nb = max(3, 1, 2)\nc = min([3, 1, 2])\nd = max([3, 1, 2])\ne = min('cab')\nf = max('cab')\ng = min(3, 1, 2, key=lambda x: -x)\nh = max([1, 2, 3], key=lambda x: -x)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 3);
    assert_eq!(fx.gi("c"), 1);
    assert_eq!(fx.gi("d"), 3);
    assert_eq!(fx.gs("e"), "a");
    assert_eq!(fx.gs("f"), "c");
    assert_eq!(fx.gi("g"), 3);
    assert_eq!(fx.gi("h"), 1);
}

#[test]
fn min_max_default() {
    let mut fx = Fx::new();
    fx.run_ok("a = min([], default=-1)\nb = max([], default='x')\nc = min([1], foo=1)\n");
    assert_eq!(fx.gi("a"), -1);
    assert_eq!(fx.gs("b"), "x");
    assert_eq!(fx.gi("c"), 1);
    assert_eq!(
        fx.raised_msg("min([])"),
        ("ValueError".to_string(), "arg is an empty sequence".to_string())
    );
    assert_eq!(
        fx.raised_msg("min([3, 1, 2], key=None)"),
        ("TypeError".to_string(), "object not callable".to_string())
    );
    assert_eq!(
        fx.raised_msg("min()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

#[test]
fn min_max_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("min('a', 1)"),
        (
            "TypeError".to_string(),
            "unsupported types for __lt__: 'int', 'str'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("min(1, 'a')"),
        (
            "TypeError".to_string(),
            "unsupported types for __lt__: 'str', 'int'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("max(1, 'a')"),
        (
            "TypeError".to_string(),
            "unsupported types for __gt__: 'str', 'int'".to_string()
        )
    );
}

// ── sorted ───────────────────────────────────────────────────────

#[test]
fn sorted_basic() {
    let mut fx = Fx::new();
    fx.run_ok("a = sorted([3, 1, 2])\nb = sorted([3, 1, 2], reverse=True)\nc = sorted('cab')\nd = sorted([])\ne = sorted([1])\n");
    assert_eq!(fx.glist("a"), vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    assert_eq!(fx.glist("b"), vec![Value::SmallInt(3), Value::SmallInt(2), Value::SmallInt(1)]);
    assert_eq!(fx.gstrlist("c"), vec!["a", "b", "c"]);
    assert!(fx.glist("d").is_empty());
    assert_eq!(fx.glist("e"), vec![Value::SmallInt(1)]);
}

#[test]
fn sorted_key() {
    let mut fx = Fx::new();
    fx.run_ok("a = sorted([3, 1, 2], key=lambda x: -x) == [3, 2, 1]\nb = sorted([(1, 'a'), (1, 'b'), (0, 'c')], key=lambda t: t[0]) == [(0, 'c'), (1, 'a'), (1, 'b')]\nc = sorted([(1, 'a'), (1, 'b'), (0, 'c')], key=lambda t: t[0], reverse=True) == [(1, 'a'), (1, 'b'), (0, 'c')]\nd = sorted(['bb', 'a', 'ccc'], key=len) == ['a', 'bb', 'ccc']\ne = sorted([3, 1, 2], key=None) == [1, 2, 3]\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(fx.gb("e"));
}

#[test]
fn sorted_key_ties() {
    // Ties keep their original order, with and without
    // `reverse=True` (probed on the reference binary).
    let mut fx = Fx::new();
    fx.run_ok("b = sorted([(1, 'a'), (1, 'b'), (0, 'c')], key=lambda t: t[0]) == [(0, 'c'), (1, 'a'), (1, 'b')]\nc = sorted([(1, 'a'), (1, 'b'), (0, 'c')], key=lambda t: t[0], reverse=True) == [(1, 'a'), (1, 'b'), (0, 'c')]\n");
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
}

#[test]
fn sorted_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("sorted([1, 'a'])"),
        (
            "TypeError".to_string(),
            "unsupported types for __lt__: 'int', 'str'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("sorted([], None)"),
        (
            "TypeError".to_string(),
            "must use keyword argument for key function".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("sorted()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("sorted([1], foo=1)"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
    // The key call itself fails: the reference ABORTS (rc=134)
    // here (`sort(key=len)` on ints — the NULL key result is
    // passed to the comparison); Rust raises instead.
    assert_eq!(
        fx.raised_msg("sorted([1], key=len)"),
        ("TypeError".to_string(), "object has no len".to_string())
    );
}

// ── io registration / open stub / table completeness ─────────────

#[test]
fn import_io() {
    let mut fx = Fx::new();
    fx.run_ok("import io\ns = io.StringIO()\ns.write('hi')\na = s.getvalue()\nb = io.BytesIO\nc = io.IOBase\nfrom io import StringIO\nd = StringIO()\n");
    assert_eq!(fx.gs("a"), "hi");
}

#[test]
fn open_stub() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("open('x')"),
        (
            "NotImplementedError".to_string(),
            "open() not implemented yet (D2)".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("open('x', 'w')"),
        (
            "NotImplementedError".to_string(),
            "open() not implemented yet (D2)".to_string()
        )
    );
}

#[test]
fn builtins_table_spot() {
    // Every name D1 binds resolves (no `NameError`): the whole
    // `modbuiltins.zig` delta in one snippet.
    let mut fx = Fx::new();
    fx.run_ok("a = [abs, all, any, bin, callable, chr, divmod, format, getattr, setattr, hasattr, delattr, globals, hash, hex, id, iter, len, locals, max, min, next, oct, open, ord, pow, print, repr, round, sorted, sum, super, staticmethod, classmethod, NotImplemented, Ellipsis, dir, input]\nn = len(a)\n");
    assert_eq!(fx.gi("n"), 38);
}
