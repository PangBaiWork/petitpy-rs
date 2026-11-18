//! Lane E2c regression tests: `list.sort(key=)`, `pow(0, 0)`,
//! `bool(range(0))` + range attrs, `callable`, NotImplemented
//! binary-op fallback, `**` kwarg key interning, `map`/`filter`/
//! `zip` over a generator iterable, and the direct `str_join`
//! panic sweep.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs` and `vm_tests_e2b.rs`).

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

    /// The module raised an exception of this type (and nothing else).
    fn raised(&mut self, src: &str) -> String {
        match self.run(src) {
            Ok(_) => panic!("module did not raise"),
            Err(VmError::Raised(exc)) => self.exc_name(exc),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }
}

// ── E2c (e): `bool(range(0))` + range attrs ─────────────────────────
// `bool()` used the payload-only `Value::is_true` (range has no
// falsy payload), not `obj_is_true` (BOOL slot → `len > 0`);
// `range_attr` existed but was never wired into the type data.

#[test]
fn e2c_range_bool() {
    let mut fx = Fx::new();
    fx.run_ok("a = bool(range(0))\nb = bool(range(5))\nc = bool(range(0, 10))\nd = bool(range(10, 0, -1))\ne = bool(range(10, 0))\n");
    assert!(!fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(!fx.gb("e"));
}

#[test]
fn e2c_range_attrs() {
    let mut fx = Fx::new();
    fx.run_ok("r = range(1, 10, 2)\na = r.start\nb = r.stop\nc = r.step\ns = range(5)\nd = s.start\ne = s.stop\nf = s.step\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 10);
    assert_eq!(fx.gi("c"), 2);
    assert_eq!(fx.gi("d"), 0);
    assert_eq!(fx.gi("e"), 5);
    assert_eq!(fx.gi("f"), 1);
}

#[test]
fn e2c_range_bool_len_consistent() {
    // `if` (the VM truthiness path) already agreed with `len`;
    // `bool()` now agrees too.
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nif range(0):\n    x = 1\ny = 0\nif range(3):\n    y = 1\n");
    assert_eq!(fx.gi("x"), 0);
    assert_eq!(fx.gi("y"), 1);
}

// ── E2c (f): `callable` on instances ────────────────────────────────
// `mp_obj_is_callable` (obj.c:200-205): when the type's call slot
// is the instance-call slot, the instance is callable only if a
// `__call__` lookup hits — a plain instance is NOT callable.

#[test]
fn e2c_callable_plain_instance() {
    let mut fx = Fx::new();
    fx.run_ok("class A:\n    pass\na = callable(A())\nb = callable(A)\nc = callable(len)\nd = callable(1)\ne = callable([])\n");
    assert!(!fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(!fx.gb("d"));
    assert!(!fx.gb("e"));
}

#[test]
fn e2c_callable_with_dunder_call() {
    let mut fx = Fx::new();
    fx.run_ok("class B:\n    def __call__(self):\n        return 1\na = callable(B())\nb = B()()\n");
    assert!(fx.gb("a"));
    assert_eq!(fx.gi("b"), 1);
}

#[test]
fn e2c_callable_inherited_dunder_call() {
    let mut fx = Fx::new();
    fx.run_ok("class B:\n    def __call__(self):\n        return 1\nclass C(B):\n    pass\na = callable(C())\n");
    assert!(fx.gb("a"));
}

// ── E2c (7): map/filter/zip/enumerate over a generator ──────────────
// C11's `iternext` only knew `Obj::Iterator` — a generator
// sub-iterator raised "object not an iterator". The `next`
// seam (`next_sub`) resumes it through the VM's `next`
// builtin (and the C14 runtime's `resume_gen`).

#[test]
fn e2c_map_over_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\n    yield 3\na = (list(map(str, g())) == ['1', '2', '3'])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_map_over_genexpr() {
    let mut fx = Fx::new();
    fx.run_ok("a = (list(map(str, (x for x in [1, 2]))) == ['1', '2'])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_filter_over_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\n    yield 3\na = (list(filter(lambda x: x > 1, g())) == [2, 3])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_zip_over_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\na = (list(zip(g(), [3, 4])) == [(1, 3), (2, 4)])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_enumerate_over_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 'a'\n    yield 'b'\na = (list(enumerate(g())) == [(0, 'a'), (1, 'b')])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_map_over_generator_two_iters() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\na = (list(map(lambda x, y: x + y, g(), [10, 20])) == [11, 22])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_nested_map_over_generator() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\na = (list(map(str, map(lambda x: x + 1, g()))) == ['2', '3'])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_map_over_generator_unpack() {
    // The C14 runtime path (unpack goes through
    // `runtime_iter::iternext` + the `resume_gen` seam).
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\na, b = map(str, g())\nc = (a == '1' and b == '2')\n");
    assert!(fx.gb("c"));
}

#[test]
fn e2c_map_over_generator_raises() {
    // A raise inside the generator propagates (not swallowed
    // as exhaustion).
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised("def g():\n    yield 1\n    raise ValueError('boom')\nlist(map(str, g()))\n"),
        "ValueError"
    );
}

// ── E2c (i): NotImplemented binary-op fallback ────────────────────────
// A dunder returning `NotImplemented` means "try the other
// fallbacks (`__rop__`) and if nothing works, raise TypeError"
// (objtype.zig:1178-1181) — `instance_binary_op` returns NULL
// for it, so the VM's generic loop tries the reflected op and
// ends at `binop_unsupported`. And a tuple comparison against
// a tuple subclass takes the vendor's slow path
// (`tupleCmpHelper`: cast to the native tuple base,
// objtuple.zig:153-158) instead of failing on the iter-slot
// check.

#[test]
fn e2c_notimpl_add_raises() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __add__(self, rhs):\n        return NotImplemented\nc = C()\ntry:\n    c + 1\n    a = False\nexcept TypeError:\n    a = True\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_notimpl_sub_raises() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __sub__(self, rhs):\n        return NotImplemented\nc = C()\ntry:\n    c - 2\n    a = False\nexcept TypeError:\n    a = True\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_notimpl_lt_raises() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __lt__(self, rhs):\n        return NotImplemented\nc = C()\ntry:\n    c < 1\n    a = False\nexcept TypeError:\n    a = True\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_notimpl_reflected_wins() {
    // The reflected op IS tried: `__radd__` on the rhs runs
    // and its value wins.
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __add__(self, rhs):\n        return NotImplemented\nclass D:\n    def __radd__(self, lhs):\n        return 42\na = (C() + D() == 42)\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_notimpl_unary_untouched() {
    // Unary methods are NOT special-cased (class_notimpl):
    // `__neg__` returning NotImplemented yields the
    // NotImplemented object itself.
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    def __neg__(self):\n        return NotImplemented\nc = C()\na = (-c is NotImplemented)\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_tuple_subclass_cmp() {
    let mut fx = Fx::new();
    fx.run_ok("class mytuple(tuple):\n    pass\nt = mytuple((1, 2, 3))\na = ((1, 2, 3) <= t)\nb = ((1, 2, 4) < t)\nc = (t < (1, 2, 4))\nd = ((1, 2, 3) == t)\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
}

// ── E2c (o): `**kwargs` key interning ─────────────────────────────────
// A `**` key that is not an interned string is interned on
// demand (the Zig interns it; `RuntimeQstr::from_strn`, C15b
// interior mutability) instead of raising `TypeError("keyword
// argument name is not an interned string")`. The long key
// below is the `fun_calldblstar3` pin (a `Mapping.keys()`
// string that was never interned).

#[test]
fn e2c_kwargs_uninterned_key() {
    let mut fx = Fx::new();
    fx.run_ok("def foo(**kw):\n    return sorted(kw.items())\nclass Mapping:\n    def keys(self):\n        return ['a', 'b', 'c', 'abcdefghijklmnopqrst']\n    def __getitem__(self, key):\n        return 1 if key == 'a' else 2\na = (foo(**Mapping()) == [('a', 1), ('abcdefghijklmnopqrst', 2), ('b', 2), ('c', 2)])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_kwargs_star_dblstar() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b, c, d):\n    return (a, b, c, d)\na = (f(*(1, 2), **{'c': 3, 'd': 4}) == (1, 2, 3, 4))\nb = (f(*(1, 2), **{['c', 'd'][i]: (3 + i) for i in range(2)}) == (1, 2, 3, 4))\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
}

// ── E2c (c): `list.sort(key=)` ───────────────────────────────────────
// `list_sort` is ctx-only and returns `PyRaise` for a non-`None`
// key; the VM intercepts `list.sort` by GcRef identity and runs
// the key through the real call hook.

#[test]
fn e2c_list_sort_key() {
    let mut fx = Fx::new();
    fx.run_ok("l = [1, 3, 2, 5]\nl.sort(key=lambda x: -x)\n");
    assert_eq!(fx.glist("l"), vec![
        Value::SmallInt(5),
        Value::SmallInt(3),
        Value::SmallInt(2),
        Value::SmallInt(1),
    ]);
}

#[test]
fn e2c_list_sort_key_reverse() {
    let mut fx = Fx::new();
    fx.run_ok("l = [1, 3, 2, 5]\nl.sort(key=lambda x: -x, reverse=True)\n");
    assert_eq!(fx.glist("l"), vec![
        Value::SmallInt(1),
        Value::SmallInt(2),
        Value::SmallInt(3),
        Value::SmallInt(5),
    ]);
}

#[test]
fn e2c_list_sort_matches_sorted() {
    // The corpus `list_sort` test's shape: `l.sort(key=...)`
    // must equal `sorted(l, key=...)`.
    let mut fx = Fx::new();
    fx.run_ok("l = [1, 3, 2, 5]\nl.sort(key=lambda x: -x)\na = (l == sorted(l, key=lambda x: -x))\nl.sort(key=lambda x: -x, reverse=True)\nb = (l == sorted(l, key=lambda x: -x, reverse=True))\nl.sort(reverse=True)\nc = (l == sorted(l, reverse=True))\nl.sort(reverse=False)\nd = (l == sorted(l, reverse=False))\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(fx.gb("d"));
}

#[test]
fn e2c_list_sort_key_builtin() {
    let mut fx = Fx::new();
    fx.run_ok("l = ['bb', 'a', 'ccc']\nl.sort(key=len)\na = (l == ['a', 'bb', 'ccc'])\n");
    assert!(fx.gb("a"));
}

#[test]
fn e2c_list_sort_bad_args() {
    let mut fx = Fx::new();
    assert_eq!(fx.raised("l = [1]\nl.sort(1)\n"), "TypeError");
    assert_eq!(fx.raised("l = [1]\nl.sort(foo=1)\n"), "TypeError");
}

#[test]
fn e2c_list_sort_key_raises() {
    // The reference ABORTS here (NULL key result); Rust raises
    // instead and leaves the list unchanged.
    let mut fx = Fx::new();
    assert_eq!(fx.raised("l = [3, 1, 2]\nl.sort(key=len)\n"), "TypeError");
}

// ── E2c (d): `pow(0, 0)` ─────────────────────────────────────────────
// The builtin 2-arg `pow` went through `binary_op_plain` → the int
// slot → mpz semantics (`pow_inpl` returns 0 for a zero base),
// while `0**0` takes the small-int ans=1 loop and returns 1
// (the original: `mp_builtin_pow` IS `mp_binary_op(POWER)`).

#[test]
fn e2c_pow_zero_zero() {
    let mut fx = Fx::new();
    fx.run_ok("a = pow(0, 0)\nb = 0**0\nc = pow(2, 0)\nd = pow(0, 2)\ne = pow(2, 10)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 1);
    assert_eq!(fx.gi("c"), 1);
    assert_eq!(fx.gi("d"), 0);
    assert_eq!(fx.gi("e"), 1024);
}

#[test]
fn e2c_pow_zero_zero_heap_int() {
    let mut fx = Fx::new();
    fx.run_ok("z = 2**70 - 2**70\na = pow(z, 0)\nb = z**0\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 1);
}

#[test]
fn e2c_pow_mod_zero_zero() {
    // The 3-arg form keeps mpz semantics: `pow(0, 0, 5) == 0`.
    let mut fx = Fx::new();
    fx.run_ok("a = pow(0, 0, 5)\n");
    assert_eq!(fx.gi("a"), 0);
}

#[test]
fn e2c_pow_zero_negative() {
    // `pow(0, -1)` is still ZeroDivisionError on both paths.
    let mut fx = Fx::new();
    assert_eq!(fx.raised("pow(0, -1)\n"), "ZeroDivisionError");
}

#[test]
fn e2c_pow_overflow_goes_big() {
    // Overflow in the small-int loop falls back to the heap path.
    let mut fx = Fx::new();
    fx.run_ok("a = pow(2, 70)\nb = 2**70\nc = (a == b)\n");
    assert!(fx.gb("c"));
}
