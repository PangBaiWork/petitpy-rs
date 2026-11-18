//! End-to-end builtin tests (D1): run small Python snippets
//! through `Vm::load_module_from_source` and assert on the
//! module's globals or on the escaped exception. Every builtin
//! bound by this lane is called as a value here — the
//! `modbuiltins.zig` table (`abs` … `sum`, `super` /
//! `staticmethod` / `classmethod`, `NotImplemented`,
//! `__name__`), the `io` module registration, and the
//! `store_attr_ctx` rewiring (`c.x = 7`).
//!
//! Notes on what the snippets avoid (not this lane's scope):
//! - `print(file=...)` / `flush=`: `sys.stdout` is D2's
//!   (`modsys` + `file_io`); the current `print` still writes
//!   fd 1 and ignores `file=`.
//! - `input()`: reads fd 0 — tested at `CallCtx` level in
//!   `builtin_fns_tests.rs` (`input_read` over `Cursor`/`Vec`).
//! - `open()`: a `NotImplementedError` stub until D2 fills the
//!   body (asserted here).
//! - `dir()` on a user class whose `__dir__` raises, and
//!   `C.x` (class-attribute read — still ignored in
//!   `vm_tests_b.rs`, the `load_method_maybe_ctx` gap).

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

    /// A global float.
    fn gf(&mut self, name: &str) -> f64 {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Float(f) => *f,
                other => panic!("global {name} is not a float: {other:?}"),
            },
            other => panic!("global {name} is not a float: {other:?}"),
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

    /// A global tuple's items.
    fn gtuple(&mut self, name: &str) -> Vec<Value> {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Tuple(items) => items.clone(),
                other => panic!("global {name} is not a tuple: {other:?}"),
            },
            other => panic!("global {name} is not a tuple: {other:?}"),
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

// ── abs / hash / divmod ──────────────────────────────────────────

#[test]
fn abs_int_float() {
    let mut fx = Fx::new();
    fx.run_ok("a = abs(-5)\nb = abs(5)\nc = abs(-2.5)\nd = abs(0)\n");
    assert_eq!(fx.gi("a"), 5);
    assert_eq!(fx.gi("b"), 5);
    assert_eq!(fx.gf("c"), 2.5);
    assert_eq!(fx.gi("d"), 0);
}

#[test]
fn abs_errors() {
    let mut fx = Fx::new();
    for src in [
        "abs('x')",
        "abs([])",
        "abs(None)",
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "unsupported type for operator".to_string()
            )
        );
    }
}

#[test]
fn hash_values() {
    let mut fx = Fx::new();
    fx.run_ok("a = hash(5)\nb = hash(0)\nc = hash('a') == hash('a')\nd = hash(2**62) == -(2**62)\ne = hash((1, 2)) == hash((1, 2))\n");
    assert_eq!(fx.gi("a"), 5);
    assert_eq!(fx.gi("b"), 0);
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(fx.gb("e"));
}

#[test]
fn hash_unhashable() {
    let mut fx = Fx::new();
    for src in ["hash([])", "hash({})", "hash(set())"] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "unsupported type for operator".to_string()
            )
        );
    }
}

#[test]
fn divmod_ints() {
    let mut fx = Fx::new();
    fx.run_ok("a = divmod(7, 3)\nb = divmod(-7, 3)\nc = divmod(7, -3)\nd = divmod(7.5, 2) == (3.0, 1.5)\n");
    assert_eq!(fx.gtuple("a"), vec![Value::SmallInt(2), Value::SmallInt(1)]);
    assert_eq!(fx.gtuple("b"), vec![Value::SmallInt(-3), Value::SmallInt(2)]);
    assert_eq!(fx.gtuple("c"), vec![Value::SmallInt(-3), Value::SmallInt(-2)]);
    assert!(fx.gb("d"));
}

#[test]
fn divmod_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("divmod('a', 1)"),
        (
            "TypeError".to_string(),
            "unsupported types for __divmod__: 'str', 'int'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("divmod(1, 'a')"),
        (
            "TypeError".to_string(),
            "unsupported types for __divmod__: 'int', 'str'".to_string()
        )
    );
}

// ── bin / hex / oct / format ─────────────────────────────────────

#[test]
fn bin_hex_oct() {
    let mut fx = Fx::new();
    fx.run_ok("a = bin(10)\nb = bin(-10)\nc = bin(0)\nd = hex(255)\ne = hex(-255)\nf = oct(8)\ng = oct(0)\n");
    assert_eq!(fx.gs("a"), "0b1010");
    assert_eq!(fx.gs("b"), "-0b1010");
    assert_eq!(fx.gs("c"), "0b0");
    assert_eq!(fx.gs("d"), "0xff");
    assert_eq!(fx.gs("e"), "-0xff");
    assert_eq!(fx.gs("f"), "0o10");
    assert_eq!(fx.gs("g"), "0o0");
}

#[test]
fn format_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = format(3.14159, '.2f')\nb = format(42, 'x')\nc = format(255, '#x')\nd = format(5, 'b')\ne = format(42, '')\nf = format(42)\ng = format('hi', '')\n");
    assert_eq!(fx.gs("a"), "3.14");
    assert_eq!(fx.gs("b"), "2a");
    assert_eq!(fx.gs("c"), "0xff");
    assert_eq!(fx.gs("d"), "101");
    assert_eq!(fx.gs("e"), "42");
    assert_eq!(fx.gs("f"), "42");
    assert_eq!(fx.gs("g"), "hi");
}

// ── chr / ord ────────────────────────────────────────────────────

#[test]
fn chr_ord() {
    let mut fx = Fx::new();
    fx.run_ok("a = chr(65)\nb = chr(0x1F600)\nc = ord('A')\nd = ord('é')\ne = ord(b'a')\nf = ord('😀')\ng = chr(0xD800)\nh = len(g)\ni = ord(g)\n");
    assert_eq!(fx.gs("a"), "A");
    assert_eq!(fx.gs("b"), "😀");
    assert_eq!(fx.gi("c"), 65);
    assert_eq!(fx.gi("d"), 233);
    assert_eq!(fx.gi("e"), 97);
    assert_eq!(fx.gi("f"), 128512);
    assert_eq!(fx.gi("h"), 1);
    assert_eq!(fx.gi("i"), 0xD800);
}

#[test]
fn chr_ord_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("chr(0x110000)"),
        (
            "ValueError".to_string(),
            "chr() arg not in range(0x110000)".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("chr(-1)"),
        (
            "ValueError".to_string(),
            "chr() arg not in range(0x110000)".to_string()
        )
    );
    for src in ["ord('ab')", "ord(1)", "ord('')"] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "ord expects a character".to_string()
            )
        );
    }
    for src in ["chr('a')", "chr(3.5)"] {
        assert_eq!(
            fx.raised_msg(src),
            ("TypeError".to_string(), "can't convert to int".to_string())
        );
    }
}

// ── pow / round ──────────────────────────────────────────────────

#[test]
fn pow_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = pow(2, 3)\nb = pow(2, 3, 5)\nc = pow(2, -1, 5)\nd = pow(2.0, -1)\ne = pow(2, 3, mod=5)\nf = pow(10, 0)\ng = pow(2, 10)\n");
    assert_eq!(fx.gi("a"), 8);
    assert_eq!(fx.gi("b"), 3);
    assert_eq!(fx.gi("c"), 0);
    assert_eq!(fx.gf("d"), 0.5);
    assert_eq!(fx.gi("e"), 3);
    assert_eq!(fx.gi("f"), 1);
    assert_eq!(fx.gi("g"), 1024);
}

#[test]
fn pow_errors() {
    let mut fx = Fx::new();
    for src in ["pow(2, 3, mod=None)", "pow(2, 3, None)"] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "pow() with 3 arguments requires integers".to_string()
            )
        );
    }
    assert_eq!(
        fx.raised_msg("pow(2, 3, 5, mod=6)"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("pow(2, 3, 0)"),
        ("ValueError".to_string(), "divide by zero".to_string())
    );
}

#[test]
fn round_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = round(2.5)\nb = round(3.5)\nc = round(123.456, 2)\nd = round(1234, -2)\ne = round(2.675, 2)\nf = round(2.5, None)\ng = round(1.5, 0)\nh = round(123.456, -1)\ni = round(True)\nj = round(False)\nk = round(2.0)\nl = round(-2.5)\nm = round(1234, -1)\nn = round(1235, -1)\n");
    assert_eq!(fx.gi("a"), 2);
    assert_eq!(fx.gi("b"), 4);
    assert_eq!(fx.gf("c"), 123.46);
    assert_eq!(fx.gi("d"), 1200);
    assert_eq!(fx.gf("e"), 2.68);
    assert_eq!(fx.gi("f"), 2);
    assert_eq!(fx.gf("g"), 2.0);
    assert_eq!(fx.gf("h"), 120.0);
    assert_eq!(fx.gi("i"), 1);
    assert_eq!(fx.gi("j"), 0);
    assert_eq!(fx.gi("k"), 2);
    assert_eq!(fx.gi("l"), -2);
    assert_eq!(fx.gi("m"), 1230);
    assert_eq!(fx.gi("n"), 1240);
}

#[test]
fn round_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("round(1.234, 2.0)"),
        ("TypeError".to_string(), "can't convert to int".to_string())
    );
    assert_eq!(
        fx.raised_msg("round('3.5')"),
        ("TypeError".to_string(), "can't convert to float".to_string())
    );
    assert_eq!(
        fx.raised_msg("round()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("round(1, 2, 3)"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

// ── repr / callable / id / len ───────────────────────────────────

#[test]
fn repr_callable_id() {
    let mut fx = Fx::new();
    fx.run_ok("a = repr(1)\nb = repr('a')\nc = repr([1, 2])\nd = repr(None)\ne = repr((1, 2))\nf = callable(len)\ng = callable(1)\nh = callable('x')\ni = callable(int)\nj = id(5)\nk = id(True)\nl = id(False)\nm = id(None)\nn = id(0)\no = id(-1)\n");
    assert_eq!(fx.gs("a"), "1");
    assert_eq!(fx.gs("b"), "'a'");
    assert_eq!(fx.gs("c"), "[1, 2]");
    assert_eq!(fx.gs("d"), "None");
    assert_eq!(fx.gs("e"), "(1, 2)");
    assert!(fx.gb("f"));
    assert!(!fx.gb("g"));
    assert!(!fx.gb("h"));
    assert!(fx.gb("i"));
    assert_eq!(fx.gi("j"), 11);
    assert_eq!(fx.gi("k"), 30);
    assert_eq!(fx.gi("l"), 14);
    assert_eq!(fx.gi("m"), 6);
    assert_eq!(fx.gi("n"), 1);
    assert_eq!(fx.gi("o"), -1);
}

#[test]
fn len_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = len('abc')\nb = len([1, 2])\nc = len((1,))\nd = len({})\ne = len(b'ab')\nf = len(range(3))\n");
    assert_eq!(fx.gi("a"), 3);
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gi("c"), 1);
    assert_eq!(fx.gi("d"), 0);
    assert_eq!(fx.gi("e"), 2);
    assert_eq!(fx.gi("f"), 3);
}

#[test]
fn len_errors() {
    let mut fx = Fx::new();
    for src in ["len(None)", "len(5)", "len(2.5)"] {
        assert_eq!(
            fx.raised_msg(src),
            ("TypeError".to_string(), "object has no len".to_string())
        );
    }
}

// ── NotImplemented / isinstance(object) / __name__ ───────────────

#[test]
fn not_implemented_singleton() {
    let mut fx = Fx::new();
    fx.run_ok("a = repr(NotImplemented)\nb = str(NotImplemented)\nc = type(NotImplemented).__name__\nd = isinstance(NotImplemented, object)\ne = isinstance(Ellipsis, object)\nf = bool(NotImplemented)\ng = NotImplemented == 1\nh = NotImplemented != 1\ni = hash(NotImplemented) == hash(NotImplemented)\nk = type(Ellipsis).__name__\n");
    assert_eq!(fx.gs("a"), "NotImplemented");
    assert_eq!(fx.gs("b"), "NotImplemented");
    assert_eq!(fx.gs("c"), "");
    assert!(fx.gb("d"));
    assert!(fx.gb("e"));
    assert!(fx.gb("f"));
    assert!(!fx.gb("g"));
    assert!(fx.gb("h"));
    assert!(fx.gb("i"));
    assert_eq!(fx.gs("k"), "");
    // The builtins dict's own `__name__` is `"builtins"` — it is
    // NOT visible as the module-level `__name__` (that is the
    // test module's `"<test>"`); read it out of the builtins
    // dict directly.
    let builtins = Value::Heap(fx.vm.state.builtins);
    let q = fx.vm.pool.from_strn(b"__name__").expect("static") as u32;
    let v = {
        let (mut ctx, _state) = fx.vm.split();
        dict_get_value(&mut ctx, builtins, Value::Qstr(q))
            .expect("__name__ is bound in builtins")
    };
    match v {
        Value::Qstr(id) => assert_eq!(fx.vm.pool.qstr_str(id as u16), b"builtins"),
        other => panic!("builtins __name__ is not a qstr: {other:?}"),
    }
}

#[test]
fn isinstance_object_mro() {
    let mut fx = Fx::new();
    fx.run_ok("a = isinstance(1, object)\nb = isinstance('x', object)\nc = isinstance([], object)\nd = isinstance(True, object)\ne = isinstance(None, object)\nf = isinstance(True, int)\ng = issubclass(bool, int)\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(fx.gb("e"));
    assert!(!fx.gb("f"));
    assert!(!fx.gb("g"));
}

// ── globals / locals / dir ───────────────────────────────────────

#[test]
fn globals_locals() {
    let mut fx = Fx::new();
    fx.run_ok("a = globals() is locals()\nb = 'x' in globals()\nx = 1\nc = 'x' in globals()\nd = locals() is globals()\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
}

#[test]
fn dir_module_level() {
    let mut fx = Fx::new();
    fx.run_ok("x = 1\na = dir()\n");
    let names = fx.gstrlist("a");
    assert!(names.contains(&"__name__".to_string()));
    assert!(names.contains(&"x".to_string()));
}

#[test]
fn dir_int() {
    let mut fx = Fx::new();
    fx.run_ok("a = dir(1)\n");
    assert_eq!(
        fx.gstrlist("a"),
        vec!["__class__", "from_bytes", "to_bytes"]
    );
}

#[test]
fn dir_str_float() {
    let mut fx = Fx::new();
    fx.run_ok("a = dir('ab')\nb = dir(1.5)\n");
    let a = fx.gstrlist("a");
    assert_eq!(a[0], "__class__");
    assert_eq!(a[1], "count");
    assert_eq!(a[2], "endswith");
    assert_eq!(a[3], "find");
    assert_eq!(a[4], "format");
    assert_eq!(fx.gstrlist("b"), vec!["__class__"]);
}

#[test]
fn dir_dunder() {
    let mut fx = Fx::new();
    fx.run_ok("class D:\n def __dir__(self):\n  return ['z', 'a']\na = dir(D())\n");
    assert_eq!(fx.gstrlist("a"), vec!["z", "a"]);
}

#[test]
fn dir_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("dir(1, 2)"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

// ── getattr / setattr / hasattr / delattr ────────────────────────

#[test]
fn getattr_ok() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n pass\nc = C()\nc.x = 7\na = getattr(c, 'x')\nb = getattr(1, 'missing', 'dflt')\nc2 = callable(getattr(1, 'to_bytes'))\nd = getattr(c, 'missing', None)\n");
    assert_eq!(fx.gi("a"), 7);
    assert_eq!(fx.gs("b"), "dflt");
    assert!(fx.gb("c2"));
    assert!(matches!(fx.g("d"), Some(Value::None)));
}

#[test]
fn getattr_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("getattr(1, 'missing')"),
        (
            "AttributeError".to_string(),
            "no attribute 'missing'".to_string()
        )
    );
    for src in ["getattr(1, 2)", "getattr(1, b'real')", "hasattr(1, 2)"] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "can't convert to str implicitly".to_string()
            )
        );
    }
}

#[test]
fn hasattr_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("a = hasattr(1, 'real')\nb = hasattr(1, 'to_bytes')\nclass B:\n def __getattr__(self, name):\n  raise AttributeError(name)\nc = hasattr(B(), 'b')\nd = getattr(B(), 'b', 'default')\n");
    assert!(!fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(!fx.gb("c"));
    assert_eq!(fx.gs("d"), "default");
}

#[test]
fn setattr_delattr() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n pass\nc = C()\nsetattr(c, 'x', 7)\na = c.x\nb = getattr(c, 'x')\ndelattr(c, 'x')\nd = hasattr(c, 'x')\n");
    assert_eq!(fx.gi("a"), 7);
    assert_eq!(fx.gi("b"), 7);
    assert!(!fx.gb("d"));
}

#[test]
fn setattr_delattr_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("setattr(1, 'x', 2)"),
        ("AttributeError".to_string(), "no attribute 'x'".to_string())
    );
    assert_eq!(
        fx.raised_msg("class C:\n pass\nc = C()\ndelattr(c, 'missing')\n"),
        (
            "AttributeError".to_string(),
            "no attribute 'missing'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("setattr(1, 2, 3)"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
}

#[test]
fn store_attr_opcode() {
    let mut fx = Fx::new();
    fx.run_ok("class C: pass\nc = C()\nc.x = 7\na = c.x\n");
    assert_eq!(fx.gi("a"), 7);
}
