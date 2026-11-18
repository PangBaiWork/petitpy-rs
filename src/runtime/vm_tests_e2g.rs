//! Lane E2g regression tests: descriptor / class-creation /
//! native-subclass dispatch (cluster (n)) — `object.__new__` /
//! `__setattr__` / `__delattr__` + the `__new__`
//! StaticClassMethod wrap, the namedtuple-base metaclass
//! resolution, `super().method()` as a comprehension iterable,
//! native-buffer subclass `__add__`, user exception subclass
//! `__new__` (+ `args` via the native-base tail and the
//! `real_load_maybe` attr slot), and the persisted builtins
//! override dict (`__build_class__` / `abs`).
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs` and `vm_tests_e2f.rs`).

use crate::objects::containers::objdict::{dict_get_value, dict_store};
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, MapData, Obj, Value};
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

// ── E2g (1): `object.__new__` / `__setattr__` / `__delattr__` ──────
// The three were `Err(PyRaise)` stubs (`objobject.rs`); the
// vendor allocates via `mp_obj_new_instance` and stores/deletes
// straight into the members map (`objobject.zig:89,111,129`).

#[test]
fn e2g_object_new_setattr_delattr() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class Foo:
    pass
o = object.__new__(Foo)
ok1 = isinstance(o, Foo)
object.__setattr__(o, "a", 5)
ok2 = o.a == 5
object.__delattr__(o, "a")
try:
    o.a
    ok3 = False
except AttributeError:
    ok3 = True
ok = ok1 and ok2 and ok3
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (2): `super().__new__` binds one arg ────────────────────────
// `object.__new__` is a StaticClassMethod (`objobject.zig:101-107`),
// so `convert_member_lookup` does NOT bind `self` for it; as a
// plain FunBuiltin the call took 2 args and raised a mismatch.

#[test]
fn e2g_super_new_binds_one_arg() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __new__(cls):
        return super(cls, A).__new__(cls)
    def __init__(self):
        self.v = 7
a = A()
ok = a.v == 7
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (3): class statement with a namedtuple base ─────────────────
// `call_build_class_ctx` resolved the metaclass via the header
// type, which is `None` for the namedtuple type — the payload
// check (`is_type_value`) resolves it to `type` itself.

#[test]
fn e2g_class_namedtuple_base() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
from collections import namedtuple
_Base = namedtuple("R", ["x", "y"])
class R(_Base):
    pass
r = R("a", "b")
ok = r.x == "a" and r.y == "b" and r[0] == "a"
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (4): `super().method()` as a comprehension iterable ─────────
// `LOAD_SUPER_METHOD` popped two stack slots but the vendor
// consumes three (`[super_fn, type, obj]` → `[method, self]`);
// the leftover `super_fn` became the CALL_FUNCTION callee when
// another stack consumer followed.

#[test]
fn e2g_super_method_comprehension_iterable() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def foo(self):
        return [0, 1, 2, 3]
class B(A):
    def foo(self):
        return [i for i in super().foo()]
ok = B().foo() == [0, 1, 2, 3]
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (5): native-buffer subclass `__add__` ──────────────────────
// `buffer_bytes` had no `Instance` arm, so a `my_bytes(bytes)`
// operand fell through to the "unsupported types" error;
// subobj[0] holds the native payload.

#[test]
fn e2g_subclass_native_buffer_add() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class my_bytes(bytes):
    pass
b1 = my_bytes(b"\x00\x01")
b2 = my_bytes(b"\x02\x03")
ok = (b1 + b2) == b"\x00\x01\x02\x03" and bytes(b1) == b"\x00\x01"
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (6): user exception subclass `__new__` ─────────────────────
// `call_type_ctx` sent every exception type down the direct
// `exception_make_new` path, so a Python `__new__` never ran;
// and `make_raise_obj` built the instance directly instead of
// calling the type. The corpus case, as globals:
// `GoodException.__new__` returns a `Dummy` (an exception
// instance, caught as such); `BadException.__new__` returns 1,
// so both the bare raise and `gen().throw(BadException)`
// surface the "exceptions must derive from BaseException"
// TypeError.

#[test]
fn e2g_user_exception_new_runs() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class Dummy(BaseException):
    pass
class GoodException(BaseException):
    def __new__(cls, *args, **kwargs):
        return Dummy(*args, **kwargs)
class BadException(BaseException):
    def __new__(cls, *args, **kwargs):
        return 1
try:
    raise GoodException("good message")
except BaseException as good:
    ok1 = type(good) is Dummy and good.args[0] == "good message"
try:
    raise BadException("bad message")
except Exception as bad:
    ok2 = type(bad) is TypeError and bad.args[0] == "exceptions must derive from BaseException"
def gen():
    yield
try:
    gen().throw(BadException)
except Exception as genbad:
    ok3 = type(genbad) is TypeError and genbad.args[0] == "exceptions must derive from BaseException"
ok = ok1 and ok2 and ok3
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (7): user exception subclass `args` ─────────────────────────
// The native-base tail of `instance_make_new` builds subobj[0]
// from the base's make_new (an exception object carrying the
// args); `real_load_maybe` consults the exception attr slot,
// so `lookup_in` step 3 finds `args`/`value` there.

#[test]
fn e2g_user_exception_args() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class E(Exception):
    pass
e = E("x")
ok = e.args == ("x",) and e.value == "x" and str(e) == "x"
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (9): `yield from` takes a user StopIteration's value ────────
// `exception_get_value` only read `Obj::Exception` payloads, so a
// `MyStopIteration(3)` raised by a sub-iterator (an `Instance`
// since E2g routes user subclasses through `instance_make_new`)
// yielded `None` instead of `3` — a regression vs master
// (`subclass_native3`). The native exception object lives in
// subobj[0] (the vendor's `getNativeException`).

#[test]
fn e2g_yield_from_user_stopiteration_value() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class MyStopIteration(StopIteration):
    pass
class Iter:
    def __iter__(self):
        return self
    def __next__(self):
        raise MyStopIteration(3)
result = []
def gen():
    v = yield from Iter()
    result.append(v)
    return 4
try:
    next(gen())
except StopIteration as er:
    ok_si = er.args == (4,)
ok = result == [3] and ok_si
"#,
    );
    assert!(fx.gb("ok"));
}

// ── E2g (8): the persisted builtins override dict ───────────────────
// `load_build_class_ctx` checks `state.builtins_override_dict` for
// `__build_class__` first (the vendor's
// `mp_module_builtins_override_dict` lookup), and
// `load_global_ctx` checks it between globals and the builtins
// globals. The dict is installed directly here (the store path
// that creates it needs the `builtins` module object, which is
// E2i's); the class statement must still pick the override up.

#[test]
fn e2g_build_class_override() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
bc_override = lambda x, y: ('class', y)
abs_override = lambda x: x + 1
"#,
    );
    let bc = fx.g("bc_override").expect("lambda in globals");
    let ab = fx.g("abs_override").expect("lambda in globals");
    {
        let (mut ctx, state) = fx.vm.split();
        let od = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
        ctx.roots.push(Value::Heap(od));
        let bc_q = static_qstr_id(b"__build_class__").expect("__build_class__ is static");
        let abs_q = static_qstr_id(b"abs").expect("abs is static");
        dict_store(&mut ctx, Value::Heap(od), Value::Qstr(bc_q), bc).unwrap();
        dict_store(&mut ctx, Value::Heap(od), Value::Qstr(abs_q), ab).unwrap();
        state.builtins_override_dict = Some(Value::Heap(od));
    }
    fx.run_ok(
        r#"
class A:
    pass
ok_bc = A == ('class', 'A')
ok_abs = abs(1) == 2
"#,
    );
    assert!(fx.gb("ok_bc"));
    assert!(fx.gb("ok_abs"));
}
