//! Lane E2f regression tests: `__set_name__` (+ the descriptor
//! `__get__` scan it shares the loader with), slice attrs,
//! `reversed` over user classes (`__reversed__` delegation,
//! `__len__`/`__getitem__` via the hook, the delegation-result
//! header stamp), and user iterators in `tuple()`/`list()` plus
//! the `__next__` attribute on C11 iterators and generators.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs` and `vm_tests_e2c.rs`).

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

// ── E2f (1): `__set_name__` ─────────────────────────────────────────
// `type_make_new` used stub seams, whose `load_maybe` returns None
// always, so `call_set_name_methods` never captured a
// `__set_name__` (and the descriptor scan never saw `__get__`).

#[test]
fn e2f_set_name_called() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
log = []
class Desc:
    def __set_name__(self, owner, name):
        log.append(name)
class C:
    a = Desc()
    b = Desc()
ok = log == ["a", "b"]
"#,
    );
    assert!(fx.gb("ok"));
}

#[test]
fn e2f_set_name_raise_propagates() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class Bad:
    def __set_name__(self, owner, name):
        raise ValueError("boom")
try:
    class C2:
        x = Bad()
    raised = False
except ValueError:
    raised = True
"#,
    );
    assert!(fx.gb("raised"));
}

#[test]
fn e2f_descriptor_get_via_class_creation() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class Desc:
    def __get__(self, obj, owner=None):
        return 99
class C:
    x = Desc()
c = C()
result = c.x
"#,
    );
    assert_eq!(fx.gi("result"), 99);
}

// ── E2f (2a): slice attrs ───────────────────────────────────────────
// `slice_attr` existed but `slice_type_data` carried `attr: None`.

#[test]
fn e2f_slice_attrs() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
s = slice(1, 2)
ok = s.start == 1 and s.stop == 2 and s.step == None
ok2 = s.indices(10) == (1, 2, 1)
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

// ── E2f (2b): `reversed` over user classes ──────────────────────────
// `__reversed__` delegation (full lookup + hook call),
// `__len__`/`__getitem__` via the call seam, and the
// delegation-result header stamp (a delegated list must stay a
// list — `append` after `reversed(B())` pins it).

#[test]
fn e2f_reversed_user_len_getitem() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __len__(self):
        return 3
    def __getitem__(self, pos):
        return pos + 1
ok = list(reversed(A())) == [3, 2, 1]
"#,
    );
    assert!(fx.gb("ok"));
}

#[test]
fn e2f_reversed_dunder_reversed() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class B:
    def __reversed__(self):
        return [1, 2, 3]
r = reversed(B())
ok = r == [1, 2, 3]
r.append(4)
ok2 = r == [1, 2, 3, 4]
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

#[test]
fn e2f_reversed_getitem_raise_propagates() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class C:
    def __len__(self):
        return 2
    def __getitem__(self, pos):
        raise ValueError("x")
try:
    list(reversed(C()))
    raised = False
except ValueError:
    raised = True
"#,
    );
    assert!(fx.gb("raised"));
}

// ── E2f (2c): user iterators in `tuple()` / `list()` ────────────────
// `collect_iterable` had no `Instance` plan arm and its Protocol
// `next` seam only resumed generators (`VmHook::resume`); a user
// iterator's `__next__` now steps through `VmHook::next`
// (`iternext_vm_ctx` — a `StopIteration` subclass is exhaustion).

#[test]
fn e2f_iter_user_iterator_tuple() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class myiter:
    def __init__(self, i):
        self.i = i
    def __iter__(self):
        return self
    def __next__(self):
        if self.i <= 0:
            raise StopIteration
        elif self.i == 10:
            # stop with an argument
            raise StopIteration(42)
        self.i -= 1
        return self.i
ok = tuple(myiter(5)) == (4, 3, 2, 1, 0)
ok2 = tuple(myiter(12)) == (11, 10)
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

#[test]
fn e2f_iter_user_iterator_list_stop_forms() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class MyStopIteration(StopIteration):
    pass
class myiter:
    def __init__(self, i):
        self.i = i
    def __iter__(self):
        return self
    def __next__(self):
        if self.i == 0:
            raise StopIteration
        elif self.i == 1:
            raise StopIteration(1)
        elif self.i == 2:
            raise MyStopIteration
ok = list(myiter(0)) == [] and list(myiter(1)) == [] and list(myiter(2)) == []
"#,
    );
    assert!(fx.gb("ok"));
}

#[test]
fn e2f_iter_missing_next_typeerror() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class NotIterable:
    def __iter__(self):
        return self
try:
    all(NotIterable())
    raised = False
except TypeError:
    raised = True
"#,
    );
    assert!(fx.gb("raised"));
}

// ── E2f (2d): the `__next__` attribute ──────────────────────────────
// C11 iterators and generators carry header `None`, so the
// `__next__` fast path in `load_method_maybe_ctx` (gated on the
// header type's `FLAG_ITER_IS_ITERNEXT`) never fired.

#[test]
fn e2f_iter_next_dunder() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
ok = iter(range(4)).__next__() == 0
ok2 = iter([10, 20]).__next__() == 10
g = (x for x in [7, 8])
ok3 = g.__next__() == 7
ok4 = g.__next__() == 8
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}

// ── E2f (4): builtin-method kwargs ───────────────────────────────────
// The `kwVar` wrappers (`objstr.zig:3368-3371, 3437, 3495-3498`):
// `split`/`rsplit` take `sep`/`maxsplit`, `replace` takes `count`,
// `encode`/`decode` take `encoding`/`errors` — unknown or duplicate
// kwargs raise the terse TypeError.

#[test]
fn e2f_method_kwargs_split() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
ok1 = 'a b  c'.split(maxsplit=1) == ['a', 'b  c']
ok2 = 'a,b,c'.rsplit(',', maxsplit=1) == ['a,b', 'c']
ok3 = 'a,b'.split(sep=',') == ['a', 'b']
ok4 = 'a b'.split() == ['a', 'b']
ok5 = 'a,b'.split(',', 1) == ['a', 'b']
ok6 = b'a,b'.split(sep=b',') == [b'a', b'b']
ok7 = b'a,b,c'.rsplit(b',', maxsplit=1) == [b'a,b', b'c']
"#,
    );
    for n in ["ok1", "ok2", "ok3", "ok4", "ok5", "ok6", "ok7"] {
        assert!(fx.gb(n), "{n} failed");
    }
}

#[test]
fn e2f_method_kwargs_replace_encode_decode() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
ok1 = 'aaa'.replace('a', 'b', count=2) == 'bba'
ok2 = 'aaa'.replace('a', 'b') == 'bbb'
ok3 = 'x'.encode(encoding='utf-8') == b'x'
ok4 = b'ab'.decode(errors='strict') == 'ab'
ok5 = b'ab'.decode(encoding='utf-8') == 'ab'
ok6 = b'ab'.decode() == 'ab'
"#,
    );
    for n in ["ok1", "ok2", "ok3", "ok4", "ok5", "ok6"] {
        assert!(fx.gb(n), "{n} failed");
    }
}

#[test]
fn e2f_method_kwargs_bad() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
def check(f):
    try:
        f()
        return 'no-raise'
    except TypeError:
        return 'TypeError'
    except Exception:
        return 'other'
bad1 = check(lambda: 'a'.split(bogus=1))
bad2 = check(lambda: 'a'.split(',', sep=','))
bad3 = check(lambda: 'a'.replace('a', 'b', old='a'))
bad4 = check(lambda: 'a'.split(',', ',', ',', 1))
bad5 = check(lambda: 'a'.replace('a'))
"#,
    );
    // `g()` reads a module global; compare the stored strings via
    // the exception-value reader shape (a str global decodes the
    // same way).
    for (n, want) in [
        ("bad1", "TypeError"),
        ("bad2", "TypeError"),
        ("bad3", "TypeError"),
        ("bad4", "TypeError"),
        ("bad5", "TypeError"),
    ] {
        let got = match fx.g(n) {
            Some(Value::Qstr(q)) => {
                String::from_utf8_lossy(fx.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Some(Value::Heap(r)) => match fx.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                _ => panic!("{n} is not a str"),
            },
            other => panic!("{n} missing: {other:?}"),
        };
        assert_eq!(got, want, "{n}");
    }
}
