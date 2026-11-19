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
