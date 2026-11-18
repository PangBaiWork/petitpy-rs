//! Lane E2b regression tests: generator collection, yield-from
//! delegation, `next()` / `pend_throw` value handling, user
//! exception repr, set iterators, `bytes`/`bytearray` of
//! generators, and `join` of a genexpr.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs` and
//! `vm_tests_e2.rs`).

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

// ── E2b: generator collection ────────────────────────────────────────
// `collect_iterable` had no generator arm (the `VmHook` pair did
// not exist yet), so `list(gen())`, genexprs and `','.join(gen())`
// all raised "object not iterable".

#[test]
fn e2b_list_of_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\n    yield 3\nxs = list(g())\n");
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
    );
}

#[test]
fn e2b_collection_forms_of_generator() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 3\n    yield 1\n    yield 2\ns = sorted(g())\nt = tuple(g())\nn = len(set(g()))\nd = dict((x, x * x) for x in range(3))\nv = d[2]\n",
    );
    assert_eq!(
        fx.glist("s"),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
    );
    assert_eq!(
        fx.gtuple("t"),
        vec![Value::SmallInt(3), Value::SmallInt(1), Value::SmallInt(2)]
    );
    assert_eq!(fx.gi("n"), 3);
    assert_eq!(fx.gi("v"), 4);
}

#[test]
fn e2b_genexpr_collection() {
    let mut fx = Fx::new();
    fx.run_ok("xs = list(x * x for x in range(4))\nys = list(x for x in range(8) if x % 3 == 0)\n");
    assert_eq!(
        fx.glist("xs"),
        vec![
            Value::SmallInt(0),
            Value::SmallInt(1),
            Value::SmallInt(4),
            Value::SmallInt(9)
        ]
    );
    assert_eq!(
        fx.glist("ys"),
        vec![Value::SmallInt(0), Value::SmallInt(3), Value::SmallInt(6)]
    );
}

#[test]
fn e2b_generator_raises_through_list() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 1\n    raise ValueError('boom')\ncaught = ''\ntry:\n    xs = list(g())\nexcept ValueError as e:\n    caught = str(e)\n",
    );
    assert_eq!(fx.gs("caught"), "boom");
}

// ── E2b: the `send` frame-restore fix ────────────────────────────────
// `g.send(1)` on a new generator raised `TypeError` correctly but
// LOST the frame, so the next resume executed garbage
// (`NotImplementedError: opcode`).

#[test]
fn e2b_send_frame_survives() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 1\n    yield 2\ng = g()\ncaught = ''\ntry:\n    g.send(1)\nexcept TypeError:\n    caught = 'TypeError'\na = g.send(None)\nb = next(g)\n",
    );
    assert_eq!(fx.gs("caught"), "TypeError");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 2);
}

// ── E2b: `next()` keeps the `StopIteration` value ────────────────────
// The old `call_next_ctx` built a bare `StopIteration`, dropping
// the generator's return value.

#[test]
fn e2b_next_stopiteration_value() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 1\n    return 42\ng = g()\na = next(g)\nv = None\ntry:\n    next(g)\nexcept StopIteration as e:\n    v = e.args[0]\n",
    );
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("v"), 42);
}

// ── E2b: `pend_throw` ────────────────────────────────────────────────
// The old VM arm always returned `None`, stored the
// `make_raise_obj(None)` `TypeError` for `pend_throw(None)`,
// and never checked the Running state.

#[test]
fn e2b_pend_throw_returns_previous() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 1\n    yield 2\ng = g()\na = g.pend_throw(ValueError)\nb = g.pend_throw(TypeError)\nis_none = a is None\nis_value_error = b is ValueError\n",
    );
    assert!(fx.gb("is_none"));
    assert!(fx.gb("is_value_error"));
}

#[test]
fn e2b_pend_throw_none_clears() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def g():\n    yield 1\n    yield 2\ng = g()\ng.pend_throw(ValueError)\ng.pend_throw(None)\na = next(g)\n",
    );
    assert_eq!(fx.gi("a"), 1);
}

#[test]
fn e2b_pend_throw_running_raises() {
    let mut fx = Fx::new();
    fx.run_ok(
        "r = ''\ndef g():\n    global r\n    try:\n        g.pend_throw(ValueError)\n        r = 'no-raise'\n    except ValueError:\n        r = 'ValueError'\n    yield 1\ng = g()\na = next(g)\n",
    );
    assert_eq!(fx.gs("r"), "ValueError");
    assert_eq!(fx.gi("a"), 1);
}

// ── E2b: user-exception repr ──────────────────────────────────────────
// `repr(CancelledError())` printed `<CancelledError object at …>`
// because `call_type_ctx` shortcuts exception subclasses to a
// bare `Obj::Exception` with no subobj for `instance_print`.

#[test]
fn e2b_user_exception_repr() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class CancelledError(Exception):\n    pass\nr = repr(CancelledError())\ns = str(CancelledError('x'))\n",
    );
    assert_eq!(fx.gs("r"), "CancelledError()");
    assert_eq!(fx.gs("s"), "x");
}

// ── E2b: set iterators ────────────────────────────────────────────────
// `iter(iter({1, 2, 3}))` raised "object not iterable" and
// `list(iter({1, 2, 3}))` panicked in `dict_ref_of` — a set
// iterator was misclassified as a dict view.

#[test]
fn e2b_set_iter_of_iter() {
    let mut fx = Fx::new();
    fx.run_ok("xs = sorted(list(iter({3, 1, 2})))\ntotal = 0\nfor x in {1, 2, 3}:\n    total += x\n");
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]
    );
    assert_eq!(fx.gi("total"), 6);
}

// ── E2b: `bytes` / `bytearray` of a generator ─────────────────────────
// Both constructors called `collect_iterable` / `iter_items`
// with a `None` hook, so a generator argument raised "object
// not iterable".

#[test]
fn e2b_bytes_of_generator() {
    let mut fx = Fx::new();
    fx.run_ok("b = bytes(i for i in range(4))\nxs = list(b)\n");
    assert_eq!(
        fx.glist("xs"),
        vec![
            Value::SmallInt(0),
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3)
        ]
    );
}

#[test]
fn e2b_bytearray_of_genexpr() {
    let mut fx = Fx::new();
    fx.run_ok("a = bytearray(i for i in range(50))\nn = len(a)\nfifth = a[5]\n");
    assert_eq!(fx.gi("n"), 50);
    assert_eq!(fx.gi("fifth"), 5);
}

// ── E2b: yield-from ───────────────────────────────────────────────────
// `getiter` fell through to a getitem iterator for user
// instances (`bound_meth_call_slot` cannot run a `FunBc`
// through stub seams), so `yield from Iter()` raised "object
// isn't subscriptable"; the map call seam dropped a
// `StopIteration` value, so `yield from map(gen7, ...)`
// lost the `444`; `throw` was converted eagerly, so a
// sub-iterator's `throw` method received a `TypeError`
// object instead of the raw value.

#[test]
fn e2b_yield_from_list_and_map() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def gen7(x):\n    if x < 3:\n        return x\n    raise StopIteration(444)\ndef gen8():\n    return (yield from map(gen7, range(100)))\ng = gen8()\nxs = list(g)\n",
    );
    // `list(g)` yields 0, 1, 2; the `444` is the delegation
    // result, read back via a second run below.
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(0), Value::SmallInt(1), Value::SmallInt(2)]
    );
}

#[test]
fn e2b_yield_from_map_stopiteration_value() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def gen7(x):\n    if x < 3:\n        return x\n    raise StopIteration(444)\ndef gen8():\n    return (yield from map(gen7, range(100)))\nret = None\ng = gen8()\ntry:\n    while True:\n        next(g)\nexcept StopIteration as e:\n    ret = e.args[0] if e.args else None\n",
    );
    assert_eq!(fx.gi("ret"), 444);
}

#[test]
fn e2b_yield_from_user_iterable() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class MyGen:\n    def __init__(self):\n        self.v = 0\n    def __iter__(self):\n        return self\n    def __next__(self):\n        self.v += 1\n        if self.v > 2:\n            raise StopIteration\n        return self.v\ndef gen():\n    yield from MyGen()\nxs = list(gen())\ntotal = 0\nfor x in MyGen():\n    total += x\n",
    );
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(1), Value::SmallInt(2)]
    );
    assert_eq!(fx.gi("total"), 3);
}

#[test]
fn e2b_yield_from_throw_raw_value() {
    let mut fx = Fx::new();
    fx.run_ok(
        "seen = []\nclass Iter:\n    def __iter__(self):\n        return self\n    def __next__(self):\n        return 1\n    def throw(self, x):\n        seen.append(x)\n        return 456\ndef gen():\n    yield from Iter()\ng = gen()\na = next(g)\nb = g.throw(123)\ng2 = gen()\nnext(g2)\nc = g2.throw(ZeroDivisionError)\nfirst_is_123 = seen[0] == 123\nsecond_is_class = seen[1] is ZeroDivisionError\n",
    );
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 456);
    assert_eq!(fx.gi("c"), 456);
    assert!(fx.gb("first_is_123"));
    assert!(fx.gb("second_is_class"));
}

// ── E2b: `join` of a genexpr ──────────────────────────────────────────
// `str_join` is ctx-only; the VM now intercepts `join` by GcRef
// identity and collects the iterable through the real hook first.

#[test]
fn e2b_join_of_genexpr() {
    let mut fx = Fx::new();
    fx.run_ok(
        "a = ','.join(str(x) for x in range(3))\ndef g():\n    yield 'a'\n    yield 'b'\nb = ','.join(g())\nc = b','.join(bytes([x]) for x in [97, 98])\nxs = list(c)\n",
    );
    assert_eq!(fx.gs("a"), "0,1,2");
    assert_eq!(fx.gs("b"), "a,b");
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(97), Value::SmallInt(44), Value::SmallInt(98)]
    );
}

#[test]
fn e2b_join_arity_mismatch() {
    let mut fx = Fx::new();
    fx.run_ok(
        "caught = ''\ntry:\n    ','.join()\nexcept TypeError as e:\n    caught = str(e)\n",
    );
    assert_eq!(fx.gs("caught"), "argument num/types mismatch");
}
