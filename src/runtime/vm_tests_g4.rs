//! Lane G4 regression tests: VM/dispatch hook family H1/H7/H8/H10.
//!
//! Root causes:
//! * H1 — `float(x)` on an instance never ran `__float__`: the
//!   FLOAT_MAYBE slot path uses stub seams, which cannot run a
//!   FunBc dunder, so every instance fell through to the
//!   native-base step and then to `TypeError: can't convert to
//!   float`. The fix intercepts the `float` TYPE object by GcRef
//!   identity (the `str` / `bool` shape in
//!   `vm_call_intercept.rs`) and runs `__float__` through the
//!   VM's real call hook; the original's
//!   `mp_obj_get_float_maybe` (`obj.zig:662-690`) requires the
//!   result to BE a float, and swallows a raising `__float__`
//!   (the unary op returns NULL) — both cases fall through to
//!   the type-call path, which raises exactly that TypeError.
//! * H7 — `str(KeyError(instance))` printed the ctx-only
//!   fallback (`<A object at …>`): the exception print renders
//!   its args through `obj_print_helper`, whose instance print
//!   uses stub seams. The fix renders exception args through
//!   the hook-aware `print_helper_hook` at every VM-level entry
//!   point that has state (`str` / `repr` / `print`, the
//!   uncaught-traceback printer, `sys.print_exception`).
//! * H8 — `type(iter(...))` raised a bare `TypeError`: the
//!   sequence iterators (`Obj::Iter` over str/list/tuple/set/
//!   frozenset/range/bytes/bytearray) have no type header. The
//!   fix gives them the real `iterator` type object.
//! * H10 — API `ppy_str` / `ppy_repr` with a raising `__str__`:
//!   the hook lived only in the VM dispatch, not in
//!   `obj_print_helper`. The fix threads the hook into the
//!   API print path.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs` and the other lane test files).

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

    fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
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

    /// Run a module that must raise; return the exception's
    /// (type name, message).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module did not raise"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
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

    fn gf(&mut self, name: &str) -> f64 {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Float(f) => *f,
                other => panic!("global {name} is not a float: {other:?}"),
            },
            other => panic!("global {name} is not a float: {other:?}"),
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
                _ => format!("{v:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            _ => format!("{v:?}"),
        }
    }
}

// ── G4/H1: `float()` runs `__float__` ───────────────────────────────
// Probed on the reference: `float(G(1.5))` is 1.5 when
// `G.__float__` returns 1.5; a non-float return (`5`, `"x"`,
// `None`) is `TypeError: can't convert to float`; a raising
// `__float__` is swallowed into the same TypeError; a float
// subclass without `__float__` converts as its native value
// (F3); plain values and arg errors are unchanged.

#[test]
fn g4_float_runs_dunder() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class G:
    def __init__(self, v):
        self.v = v
    def __float__(self):
        return self.v
a = float(G(1.5))
b = float(G(9.9))
c = float(1.5)
d = float("3.5")
e = float(2)
f = float()
"#,
    );
    assert_eq!(fx.gf("a"), 1.5);
    assert_eq!(fx.gf("b"), 9.9);
    assert_eq!(fx.gf("c"), 1.5);
    assert_eq!(fx.gf("d"), 3.5);
    assert_eq!(fx.gf("e"), 2.0);
    assert_eq!(fx.gf("f"), 0.0);
}

#[test]
fn g4_float_dunder_non_float_return_is_type_error() {
    let mut fx = Fx::new();
    for src in [
        "class GI:\n    def __float__(self):\n        return 5\nfloat(GI())\n",
        "class GS:\n    def __float__(self):\n        return \"x\"\nfloat(GS())\n",
        "class GN:\n    def __float__(self):\n        return None\nfloat(GN())\n",
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "can't convert to float".to_string()
            )
        );
    }
}

#[test]
fn g4_float_dunder_raise_is_swallowed_to_type_error() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(
            "class GR:\n    def __float__(self):\n        raise ValueError(\"boom\")\nfloat(GR())\n"
        ),
        (
            "TypeError".to_string(),
            "can't convert to float".to_string()
        )
    );
}

#[test]
fn g4_float_subclass_without_dunder_still_native() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class F(float): pass
a = float(F(2.5))
"#,
    );
    assert_eq!(fx.gf("a"), 2.5);
}

// ── G4/H8: `type(iter(...))` ────────────────────────────────────────
// Probed on the reference: every sequence iterator reports
// `<class 'iterator'>` and `repr(iter([1, 2]))` is `<iterator>`;
// the list/tuple/str/set iterators share ONE type object, but
// the range and dict iterators are DIFFERENT type objects with
// the same name (`type(iter([1])) is type(iter(range(3)))`
// False, `type(iter([1])) is type(iter({'a': 1}))` False);
// `iter(it) is it` holds.

#[test]
fn g4_type_of_sequence_iterators() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
it_l = iter([1, 2])
it_t = iter((1, 2))
it_s = iter('ab')
it_set = iter({1, 2})
it_r = iter(range(3))
it_d = iter({'a': 1})
it_k = iter({'a': 1}.keys())
n_l = type(it_l).__name__
n_t = type(it_t).__name__
n_s = type(it_s).__name__
n_set = type(it_set).__name__
n_r = type(it_r).__name__
n_d = type(it_d).__name__
n_k = type(it_k).__name__
r_l = repr(it_l)
i_lt = type(it_l) is type(it_t)
i_ls = type(it_l) is type(it_s)
i_lset = type(it_l) is type(it_set)
i_lr = type(it_l) is type(it_r)
i_ld = type(it_l) is type(it_d)
i_lk = type(it_l) is type(it_k)
i_dk = type(it_d) is type(it_k)
i_rk = type(it_r) is type(it_k)
s_l = iter(it_l) is it_l
s_r = iter(it_r) is it_r
s_d = iter(it_d) is it_d
"#,
    );
    for n in ["n_l", "n_t", "n_s", "n_set", "n_r", "n_d", "n_k"] {
        assert_eq!(fx.gs(n), "iterator", "{n}");
    }
    assert_eq!(fx.gs("r_l"), "<iterator>");
    // The polymorph family shares ONE type object ...
    assert!(fx.gb("i_lt"));
    assert!(fx.gb("i_ls"));
    assert!(fx.gb("i_lset"));
    // ... but range and dict iterators are DIFFERENT type objects
    // with the same name (probed on REF). The dict family shares
    // ONE type object: `iter(d)`, `iter(d.keys())`,
    // `iter(d.values())`, `iter(d.items())` are all
    // `mp_type_dict_view_it` (objdict.zig:527, :602 — probed on REF).
    assert!(!fx.gb("i_lr"));
    assert!(!fx.gb("i_ld"));
    assert!(!fx.gb("i_lk"));
    assert!(fx.gb("i_dk"));
    assert!(!fx.gb("i_rk"));
    // iter(it) is it.
    assert!(fx.gb("s_l"));
    assert!(fx.gb("s_r"));
    assert!(fx.gb("s_d"));
}

#[test]
fn g4_type_of_getitem_iterator() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __getitem__(self, i):
        if i >= 2:
            raise IndexError(i)
        return i * 10
it_g = iter(A())
n_g = type(it_g).__name__
i_g = type(it_g) is type(iter([1]))
xs = list(it_g)
r_xs = repr(xs)
"#,
    );
    assert_eq!(fx.gs("n_g"), "iterator");
    assert!(fx.gb("i_g"));
    assert_eq!(fx.gs("r_xs"), "[0, 10]");
}

// ── G4/H7: exception args render through the hook ────────────────
// Probed on the reference: `str(KeyError(A()))` is `A-str`,
// `repr(KeyError(A()))` is `KeyError(A-repr,)`,
// `str(KeyError(A(), "b"))` is `(A-repr, 'b')`,
// `str(KeyError())` is empty, and a user exception subclass
// renders its args the same way (`str(E(A()))` is `A-str`,
// `repr(E(A()))` is `E(A-repr,)`). `print` and the uncaught
// traceback take the same path (dual probes h7b/h7c/h7d).

#[test]
fn g4_str_repr_of_exception_with_instance_arg() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __repr__(self):
        return "A-repr"
    def __str__(self):
        return "A-str"
s1 = str(KeyError(A()))
r1 = repr(KeyError(A()))
s2 = str(ValueError(A()))
s3 = str(KeyError(A(), "b"))
s4 = str(KeyError())
class E(Exception):
    pass
s5 = str(E(A()))
r5 = repr(E(A()))
"#,
    );
    assert_eq!(fx.gs("s1"), "A-str");
    assert_eq!(fx.gs("r1"), "KeyError(A-repr,)");
    assert_eq!(fx.gs("s2"), "A-str");
    assert_eq!(fx.gs("s3"), "(A-repr, 'b')");
    assert_eq!(fx.gs("s4"), "");
    assert_eq!(fx.gs("s5"), "A-str");
    assert_eq!(fx.gs("r5"), "E(A-repr,)");
}

#[test]
fn g4_exception_subclass_dunder_wins() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class E(Exception):
    def __str__(self):
        return "custom-str"
    def __repr__(self):
        return "custom-repr"
s = str(E("x"))
r = repr(E("x"))
"#,
    );
    assert_eq!(fx.gs("s"), "custom-str");
    assert_eq!(fx.gs("r"), "custom-repr");
}

#[test]
fn g4_exception_subclass_raising_dunder_propagates_via_str() {
    // NOTE: REF ABORTS here (rc=134, null-pointer panic at
    // `mp_mirrors.zig:745` via `instance_print`, objtype.zig:1679
    // — the fallback path calls `mp_obj_print_helper` with a
    // null print fn), and likewise on `print(E("x"))`. Rust
    // keeps the fallback / propagate behavior instead: the
    // documented abort-vs-fallback divergence class (same as a
    // raising `__repr__` inside a container — see
    // `print_helper_hook`); do NOT reproduce the abort. The
    // `str` / `repr` builtins propagate a dunder raise, so
    // `str(E("x"))` raises the `ValueError` here.
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(
            "class E(Exception):\n    def __str__(self):\n        raise ValueError(\"boom\")\nstr(E(\"x\"))\n"
        ),
        ("ValueError".to_string(), "boom".to_string())
    );
}
