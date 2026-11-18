//! Lane E2 regression tests: run small Python snippets through
//! `Vm::load_module_from_source` and assert inside the snippet (or
//! on the escaped exception). Split out of `vm_tests_b.rs` by E2
//! (that file was 895 lines — over the 800-line cap).
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`).

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

// ── E2: the VM call hook (`CallHook`) ────────────────────────────────
// `type_call` carries the VM's real call hook into `MakeNewFn`, so a
// Python `__new__` / `__init__` actually executes (pre-E2 both
// raised the bare `TypeError` through the stub seams), and
// `collect_iterable` carries it into map/filter consumption, so
// `list(map(f, xs))` runs a `FunBc` `f`.

#[test]
fn e2_init_runs() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __init__(self, x):\n        self.x = x + 1\nc = C(41)\nn = c.x\n");
    assert_eq!(fx.gi("n"), 42);
}

#[test]
fn e2_init_raises_with_message() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __init__(self):\n        raise ValueError('boom')\ncaught = ''\ntry:\n    c = C()\nexcept ValueError as e:\n    caught = str(e)\n");
    assert_eq!(fx.gs("caught"), "boom");
}

#[test]
fn e2_new_returns_other_skips_init() {
    // Probed on the reference (instance_create.rs docs): a
    // `__new__` whose result's type is not the class SKIPS
    // `__init__` and returns the result.
    let mut fx = Fx::new();
    fx.run_ok("class N:\n    def __new__(cls):\n        return 42\n    def __init__(self):\n        raise ValueError('must not run')\nn = N()\n");
    assert_eq!(fx.gi("n"), 42);
}

#[test]
fn e2_init_super() {
    let mut fx = Fx::new();
    fx.run_ok("class A:\n    def __init__(self, x):\n        self.x = x\nclass B(A):\n    def __init__(self, x):\n        super().__init__(x + 1)\nb = B(41)\nn = b.x\n");
    assert_eq!(fx.gi("n"), 42);
}

#[test]
fn e2_list_map_funbc() {
    let mut fx = Fx::new();
    fx.run_ok("def f(x):\n    return x + 1\nxs = list(map(f, [1, 2, 3]))\n");
    assert_eq!(
        fx.glist("xs"),
        vec![Value::SmallInt(2), Value::SmallInt(3), Value::SmallInt(4)]
    );
}

#[test]
fn e2_tuple_set_dict_map_funbc() {
    let mut fx = Fx::new();
    fx.run_ok("def f(x):\n    return x * 2\nt = tuple(map(f, [1, 2]))\ns = set(map(f, [1, 2]))\nd = dict(map(lambda k: (k, k + 1), [1, 2]))\nok = len(s) == 2 and 2 in s and 4 in s and d[1] == 2 and d[2] == 3\n");
    assert_eq!(
        fx.gtuple("t"),
        vec![Value::SmallInt(2), Value::SmallInt(4)]
    );
    assert!(fx.gb("ok"));
}

#[test]
fn e2_sorted_map_funbc() {
    let mut fx = Fx::new();
    fx.run_ok("def f(x):\n    return -x\nxs = sorted(map(f, [1, 2, 3]))\n");
    assert_eq!(
        fx.glist("xs"),
        vec![
            Value::SmallInt(-3),
            Value::SmallInt(-2),
            Value::SmallInt(-1)
        ]
    );
}

/// E2 (Fix C follow-up): `str()` / `repr()` / `print()` run an
/// instance's `__str__` / `__repr__` through the VM's real call
/// hook (probed: the dunder result is RENDERED — `str(A())` is
/// `"1"` when `__str__` returns `1`; `repr` never falls back
/// to `__str__`).
#[test]
fn e2_str_repr_instance() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n    def __str__(self):\n        return 1\n    def __repr__(self):\n        return 2\nclass B:\n    def __str__(self):\n        return 'STR'\nclass C:\n    pass\na = str(A())\nb = repr(A())\nc = str(B())\nd = repr(B())\ne = str(C())\n",
    );
    assert_eq!(fx.gs("a"), "1");
    assert_eq!(fx.gs("b"), "2");
    assert_eq!(fx.gs("c"), "STR");
    // `repr()` never falls back to `__str__` (probed) — B has
    // no `__repr__`, so the default rendering applies.
    assert!(fx.gs("d").starts_with("<B object at "));
    assert!(fx.gs("e").starts_with("<C object at "));
}

/// E2 (Fix C follow-up): `print(instance)` renders through
/// `__str__` (via `io.StringIO` as the `file=` target, so the
/// test can read the bytes back).
#[test]
fn e2_print_instance() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import io\nclass A:\n    def __str__(self):\n        return 'A!'\nbuf = io.StringIO()\nprint(A(), 1, file=buf)\ns = buf.getvalue()\n",
    );
    assert_eq!(fx.gs("s"), "A! 1\n");
}

/// E2 (Fix C follow-up): `bool()` / `len()` run an instance's
/// `__bool__` / `__len__` through the VM's real call hook.
/// `__bool__` returning `1` is `False` (the identity rule,
/// probed); `len()` returns the `__len__` result raw.
#[test]
fn e2_bool_len_instance() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n    def __bool__(self):\n        return 1\nclass B:\n    def __len__(self):\n        return 'x'\nclass C:\n    def __bool__(self):\n        return True\nclass D:\n    def __len__(self):\n        return 3\nba = bool(A())\nbb = bool(C())\nlb = len(B())\nld = len(D())\nif A():\n    t1 = 'yes'\nelse:\n    t1 = 'no'\n",
    );
    assert_eq!(fx.gb("ba"), false);
    assert_eq!(fx.gb("bb"), true);
    assert_eq!(fx.gs("lb"), "x");
    assert_eq!(fx.gi("ld"), 3);
    assert_eq!(fx.gs("t1"), "no");
}

#[test]
fn e2_floordiv_var_neg() {
    let mut fx = Fx::new();
    fx.run_ok("a = -2\nb = -4\nc = a // b\nd = a % b\n");
    assert_eq!(fx.gi("a"), -2);
    assert_eq!(fx.gi("b"), -4);
    assert_eq!(fx.gi("c"), 0);
    assert_eq!(fx.gi("d"), -2);
    // Full sign matrix (E2: `div_euclid` is Euclidean, not floor
    // — wrong exactly when the divisor is negative and the
    // division is inexact).
    fx.run_ok(
        "def f(a, b):\n    assert a // b == divmod(a, b)[0]\n    assert a % b == divmod(a, b)[1]\n\
         f(-2, -4)\nf(2, -4)\nf(-2, 4)\nf(2, 4)\nf(-7, 2)\nf(7, -2)\n\
         f(-7, -2)\nf(7, 2)\nf(-8, -3)\nf(8, 3)\nf(8, -3)\nf(-8, 3)\n",
    );
    // Window edge: `-(2**62) // -1 == 2**62` is out of the
    // small-int window (heap int, correct value — the reference
    // agrees; only `divmod` there mistags, C14).
    fx.run_ok("e = -(2**62)\nassert e // -1 == 2**62\nassert e % -1 == 0\n");
}

#[test]
fn e2_fun_name_qstr_table() {
    // E2: `make_function_ctx` used the prelude name varint raw as
    // the qstr id; it is a TABLE INDEX into the child code's qstr
    // table (`mp_obj_fun_get_name`, objfun.zig).
    let mut fx = Fx::new();
    fx.run_ok("def Fun():\n    pass\nassert Fun.__name__ == 'Fun'\n");
    fx.run_ok(
        "def outer():\n    x = 1\n    def inner():\n        return x\n    return inner\nassert outer().__name__ == 'inner'\n",
    );
    fx.run_ok("def Gen():\n    yield\nassert Gen.__name__ == 'Gen'\n");
}

#[test]
fn e2_namedtuple_empty_repr() {
    // E2: an empty namedtuple must be a fresh tuple headed by
    // its class (`TupEmpty()`), not the shared `()` singleton.
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections import namedtuple\nE = namedtuple('TupEmpty', [])\nassert repr(E()) == 'TupEmpty()'\nassert E() == ()\nassert E() is not ()\n",
    );
}

#[test]
fn e2_splitlines_keepends_kwarg() {
    // E2: `splitlines(keepends=True)` needs the kw wrapper
    // (positional wins, unknown kwargs ignored — probed).
    let mut fx = Fx::new();
    fx.run_ok(
        "assert 'a\\nb'.splitlines(keepends=True) == ['a\\n', 'b']\nassert 'a\\nb'.splitlines(True, keepends=False) == ['a\\n', 'b']\nassert 'a\\nb'.splitlines(foo=1) == ['a', 'b']\nassert b'a\\nb'.splitlines(keepends=True) == [b'a\\n', b'b']\n",
    );
}

#[test]
fn e2_str_join_dynamic_qstr() {
    // E2: `iter_items` used static-only `qstr_bytes`, so
    // `''.join('abc')` joined an empty source.
    let mut fx = Fx::new();
    fx.run_ok("assert ''.join('abc') == 'abc'\nassert ','.join('abc') == 'a,b,c'\n");
}

#[test]
fn e2_dict_view_type() {
    // E2: views allocated with a `None` header — no slots, so
    // `type(d.keys())`, `print`, `len` and `in` all failed.
    let mut fx = Fx::new();
    fx.run_ok(
        "d = {1: 2, 3: 4}\nassert type(d.keys()).__name__ == 'dict_view'\nassert repr(d.keys()) == 'dict_keys([1, 3])'\nassert repr(d.values()) == 'dict_values([2, 4])'\nassert repr(d.items()) == 'dict_items([(1, 2), (3, 4)])'\nassert len(d.keys()) == 2\nassert bool(d.keys())\nassert not bool({}.keys())\nassert 1 in d.keys()\nassert 9 not in d.keys()\nassert type(hash({}.values())) is int\n",
    );
}

#[test]
fn e2_getattr_hasattr_dunder() {
    // E2: `getattr`/`hasattr` went through the ctx-only
    // `load_method_maybe`, which has no `__getattr__` fallback.
    let mut fx = Fx::new();
    fx.run_ok(
        "class B:\n    def __getattr__(self, attr):\n        if attr == 'a':\n            return attr\n        raise AttributeError\nb = B()\nassert getattr(b, 'a') == 'a'\nassert getattr(b, 'a', 'default') == 'a'\nassert getattr(b, 'b', 'default') == 'default'\nassert hasattr(b, 'a')\nassert not hasattr(b, 'b')\n",
    );
    fx.run_ok(
        "class C:\n    def __getattr__(self, attr):\n        if attr == 'exists':\n            return attr\n        elif attr == 'raise':\n            raise Exception(123)\n        raise AttributeError\nc = C()\nassert hasattr(c, 'exists')\nassert not hasattr(c, 'doesnt_exist')\ntry:\n    hasattr(c, 'raise')\nexcept Exception as e:\n    assert str(e) == '123'\nelse:\n    raise AssertionError('no raise')\n",
    );
}

#[test]
fn e2_property_no_getter() {
    // E2: a getter-less property returned the property object
    // itself instead of raising (objtype.zig:1320 — NULL without
    // the SENTINEL, so the caller raises).
    let mut fx = Fx::new();
    fx.run_ok(
        "class D:\n    prop = property()\nd = D()\ntry:\n    d.prop\nexcept AttributeError as e:\n    assert str(e) == \"no attribute 'prop'\"\nelse:\n    raise AssertionError('no raise')\nassert getattr(d, 'prop', 'default') == 'default'\nassert not hasattr(d, 'prop')\n",
    );
}

#[test]
fn e2_bytes_fromhex_classmethod() {
    // E2: `bytes.fromhex` was registered as a plain `FunBuiltin`,
    // so `bytes.fromhex('001122')` called the body with one arg
    // and panicked indexing `args[1]` (Rust panic, SIGABRT). The
    // original wraps it in a `StaticClassMethod`
    // (objstr.zig:3517-3520, sig (2, 3) counting `cls` — the
    // `int.from_bytes` pattern).
    let mut fx = Fx::new();
    fx.run_ok(
        "assert bytes.fromhex('001122') == bytes([0, 17, 34])\nassert bytes.fromhex('00:11:22', ':') == bytes([0, 17, 34])\nassert bytes.fromhex('aa-bb-cc', '-') == bytes([170, 187, 204])\nassert str(bytes.fromhex) == '<bound_method>'\ntry:\n    bytes.fromhex()\nexcept TypeError:\n    pass\nelse:\n    raise AssertionError('no raise')\ntry:\n    bytes.fromhex('0')\nexcept ValueError:\n    pass\nelse:\n    raise AssertionError('no raise')\ntry:\n    bytes.fromhex('zz')\nexcept ValueError:\n    pass\nelse:\n    raise AssertionError('no raise')\n",
    );
}

#[test]
fn e2_gen_wrap_defaults() {
    // E2: calling a generator function with missing defaulted
    // args panicked in `setup_code_state` (`extra_args` index out
    // of bounds) — the gen-wrap path passed `&[]` instead of the
    // `FunBc`'s `def_pos` (`itertools.count(10)`,
    // `islice([..], 2)` in the frozen itertools module).
    let mut fx = Fx::new();
    fx.run_ok(
        "def g(a=5, b=7):\n    yield a\n    yield b\nit = g()\nassert next(it) == 5\nassert next(it) == 7\ndef h(a, b=2):\n    yield a + b\nassert next(h(10)) == 12\nimport itertools\nc = itertools.count(10)\nassert next(c) == 10\nassert next(c) == 11\nassert next(itertools.islice([1, 2, 3, 4], 2)) == 1\n",
    );
}

#[test]
fn e2_list_enumerate_str() {
    // E2: `list(enumerate('ab'))` raised `IndexError` — the
    // collect protocol arm's `getitem` seam only mapped
    // `ObjError::IndexOutOfRange` to exhaustion, but a str
    // subscript at OOB records an `IndexError` raise instead
    // (`call_subscr` turns a slot `PyRaise` into
    // `NotImplemented`); the VM seam (`vm_rt_iter.rs`) already
    // classified on the recorded raise per
    // `objgetitemiter.zig:excIsStopOrIndex`, the collect seam
    // did not.
    let mut fx = Fx::new();
    fx.run_ok(
        "assert list(enumerate('ab')) == [(0, 'a'), (1, 'b')]\nassert list(zip([1, 2], 'ab')) == [(1, 'a'), (2, 'b')]\nassert list(map(str, 'ab')) == ['a', 'b']\n",
    );
}

#[test]
fn e2_str_caseconv_literal() {
    // E2: `' Ab '.lower()` raised "can't convert to str
    // implicitly" — the case-conversion / predicate methods in
    // `objstr_methods_b.rs` read self data with arena-only
    // `str_data`, which returns `None` for dynamic-qstr literals
    // (D5's `str_data_pool` fix class; the last 6 sites in that
    // file, plus the `bytes.hex` sep arg).
    let mut fx = Fx::new();
    fx.run_ok(
        "assert ' Ab '.lower() == ' ab '\nassert ' Ab '.upper() == ' AB '\nassert 'hello world'.title() == 'Hello World'\nassert 'hello'.capitalize() == 'Hello'\nassert '42'.zfill(5) == '00042'\nassert 'ABC'.isupper()\nassert 'abc'.islower()\nassert 'a1'.isalnum()\nassert bytes([0, 17]).hex(':') == '00:11'\n",
    );
}

#[test]
fn e2_dict_fromkeys_classmethod() {
    // E2: `dict.fromkeys([1, 2, 3, 4])` raised "argument num/types
    // mismatch" — `fromkeys` was registered as a plain `FunBuiltin`
    // but the original wraps it in a `StaticClassMethod`
    // (objdict.zig:741-743, sig (2, 3) counting `cls` — the
    // `bytes.fromhex` pattern, the third and last classmethod in
    // the locals tables).
    let mut fx = Fx::new();
    fx.run_ok(
        "assert dict.fromkeys([1, 2, 3, 4]) == {1: None, 2: None, 3: None, 4: None}\nassert dict.fromkeys('ab', 0) == {'a': 0, 'b': 0}\nassert str(dict.fromkeys) == '<bound_method>'\n",
    );
}

#[test]
fn e2_dict_update_kwargs() {
    // E2: `d.update(a=5)` raised "argument num/types mismatch" —
    // the core `dict_update` already took a kwargs `MapData` (the
    // C15 VM path used it), but the `BuiltinFn` wrapper passed an
    // empty map; the `BuiltinKwFn` wrapper now builds the map
    // from the `(qstr, value)` pairs (kwargs applied after the
    // positional argument, in order).
    let mut fx = Fx::new();
    fx.run_ok(
        "d = {1: 2}\nd.update(a=5)\nassert d == {1: 2, 'a': 5}\nd.update([(1, 5)], b=6)\nassert d[1] == 5\nassert d['b'] == 6\n",
    );
}
