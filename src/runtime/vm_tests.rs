//! End-to-end dispatch tests (C13 step 10): run small Python
//! snippets through `Vm::load_module_from_source` (compile →
//! load → execute the module body) and assert on the module's
//! globals or on the escaped exception. These are the first
//! tests that drive the whole VM — every opcode group in the
//! dispatch is covered by at least one snippet here.
//!
//! Notes on what the snippets avoid (not this lane's scope):
//! `class` definitions (C12a's `type_call`), `import` (C15),
//! `len` (not a builtin yet — use indexing / iteration
//! instead), and `with` (no context manager exists without
//! user classes; the unwind machinery it shares with
//! `try/finally`/`break`/`continue` is covered instead).

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM per test, plus the read-back helpers.
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

    fn is_none(&mut self, name: &str) -> bool {
        matches!(self.g(name), Some(Value::None))
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

/// The escaped exception's type name, or panics when the module
/// ran clean.
fn raised_name(fx: &mut Fx, src: &str) -> String {
    match fx.run(src) {
        Err(VmError::Raised(exc)) => fx.exc_name(exc),
        Ok(_) => panic!("expected the module to raise, but it ran clean"),
        Err(e) => panic!("expected a raise, got a load failure: {e:?}"),
    }
}

// ── constants / loads / stores ───────────────────────────────────────

#[test]
fn dispatch_const_ints() {
    let mut fx = Fx::new();
    fx.run_ok("a = 1\nb = -5\nc = 0\nd = 100\ne = -100\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), -5);
    assert_eq!(fx.gi("c"), 0);
    assert_eq!(fx.gi("d"), 100);
    assert_eq!(fx.gi("e"), -100);
}

#[test]
fn dispatch_const_bool_none() {
    let mut fx = Fx::new();
    fx.run_ok("a = True\nb = False\nc = None\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(fx.is_none("c"));
}

#[test]
fn dispatch_const_str() {
    let mut fx = Fx::new();
    fx.run_ok("a = \"hello\"\nb = 'world'\n");
    assert_eq!(fx.gs("a"), "hello");
    assert_eq!(fx.gs("b"), "world");
}

#[test]
fn dispatch_load_store_name() {
    let mut fx = Fx::new();
    fx.run_ok("x = 42\ny = x + 1\nz = y * 2\n");
    assert_eq!(fx.gi("x"), 42);
    assert_eq!(fx.gi("y"), 43);
    assert_eq!(fx.gi("z"), 86);
}

#[test]
fn dispatch_global_stmt() {
    let mut fx = Fx::new();
    fx.run_ok("x = 1\ndef f():\n    global x\n    x = 2\nf()\n");
    assert_eq!(fx.gi("x"), 2);
}

#[test]
fn dispatch_load_fast() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b):\n    return a + b\nx = f(3, 4)\n");
    assert_eq!(fx.gi("x"), 7);
}

#[test]
fn dispatch_load_fast_unbound() {
    // `y` is a local (assigned in the function) but unbound on
    // this path — `LOAD_FAST` must raise `NameError`.
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(
            &mut fx,
            "def f():\n    if False:\n        y = 1\n    return y\nx = f()\n"
        ),
        "NameError"
    );
}

#[test]
fn dispatch_delete_name() {
    let mut fx = Fx::new();
    fx.run_ok("x = 1\ndel x\n");
    assert!(fx.g("x").is_none());
}

// ── arithmetic / binary ops ──────────────────────────────────────────

#[test]
fn dispatch_arith() {
    let mut fx = Fx::new();
    fx.run_ok("a = 2 + 3 * 4\nb = 10 - 4\nc = 2 ** 8\nd = 17 // 5\ne = 17 % 5\nf = 7 / 2\ng = -3\nh = 2 << 3\ni = 16 >> 2\n");
    assert_eq!(fx.gi("a"), 14);
    assert_eq!(fx.gi("b"), 6);
    assert_eq!(fx.gi("c"), 256);
    assert_eq!(fx.gi("d"), 3);
    assert_eq!(fx.gi("e"), 2);
    assert!((fx.gf("f") - 3.5).abs() < 1e-12);
    assert_eq!(fx.gi("g"), -3);
    assert_eq!(fx.gi("h"), 16);
    assert_eq!(fx.gi("i"), 4);
}

#[test]
fn dispatch_comparisons() {
    let mut fx = Fx::new();
    fx.run_ok("a = 1 < 2\nb = 2 <= 2\nc = 3 > 4\nd = 3 >= 3\ne = 1 == 1\nf = 1 != 1\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(!fx.gb("c"));
    assert!(fx.gb("d"));
    assert!(fx.gb("e"));
    assert!(!fx.gb("f"));
}

#[test]
fn dispatch_bool_ops() {
    let mut fx = Fx::new();
    fx.run_ok("a = True and False\nb = True or False\nc = not True\nd = False or 7\ne = True and 7\n");
    assert!(!fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(!fx.gb("c"));
    assert_eq!(fx.gi("d"), 7);
    assert_eq!(fx.gi("e"), 7);
}

#[test]
fn dispatch_is_op() {
    let mut fx = Fx::new();
    fx.run_ok("a = None is None\nb = 1 is 1\nc = 1 is not 2\n");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
}

#[test]
fn dispatch_in_op() {
    let mut fx = Fx::new();
    fx.run_ok("a = 2 in [1, 2, 3]\nb = 5 in [1, 2, 3]\nc = 5 not in [1, 2, 3]\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(fx.gb("c"));
}

#[test]
fn dispatch_div_zero() {
    let mut fx = Fx::new();
    assert_eq!(raised_name(&mut fx, "a = 1 // 0\n"), "ZeroDivisionError");
    let mut fx = Fx::new();
    assert_eq!(raised_name(&mut fx, "a = 1 % 0\n"), "ZeroDivisionError");
}

#[test]
fn dispatch_big_int() {
    // `2 ** 70` leaves the small-int window (a heap `Int`); the
    // arithmetic still agrees with itself.
    let mut fx = Fx::new();
    fx.run_ok("a = 2 ** 70\nb = a == 2 ** 70\nc = a - 1 == 2 ** 70 - 1\n");
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert!(matches!(fx.g("a"), Some(Value::Heap(_))));
}

#[test]
fn dispatch_bitwise() {
    let mut fx = Fx::new();
    fx.run_ok("a = 6 & 3\nb = 6 | 3\nc = 6 ^ 3\nd = ~6\n");
    assert_eq!(fx.gi("a"), 2);
    assert_eq!(fx.gi("b"), 7);
    assert_eq!(fx.gi("c"), 5);
    assert_eq!(fx.gi("d"), -7);
}

// ── control flow ─────────────────────────────────────────────────────

#[test]
fn dispatch_if_else() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nif 1 < 2:\n    x = 10\nelse:\n    x = 20\n");
    assert_eq!(fx.gi("x"), 10);
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nif 1 > 2:\n    x = 10\nelse:\n    x = 20\n");
    assert_eq!(fx.gi("x"), 20);
}

#[test]
fn dispatch_while_loop() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ni = 0\nwhile i < 5:\n    x += i\n    i += 1\n");
    assert_eq!(fx.gi("x"), 10);
    assert_eq!(fx.gi("i"), 5);
}

#[test]
fn dispatch_for_list() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor v in [1, 2, 3]:\n    x += v\n");
    assert_eq!(fx.gi("x"), 6);
}

#[test]
fn dispatch_for_range() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor i in range(5):\n    x += i\n");
    assert_eq!(fx.gi("x"), 10);
}

#[test]
fn dispatch_for_break() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor i in range(10):\n    if i == 3:\n        break\n    x += i\n");
    assert_eq!(fx.gi("x"), 3);
}

#[test]
fn dispatch_for_continue() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor i in range(5):\n    if i == 2:\n        continue\n    x += i\n");
    assert_eq!(fx.gi("x"), 8);
}

#[test]
fn dispatch_for_else() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor i in []:\n    x = 1\nelse:\n    x = 2\n");
    assert_eq!(fx.gi("x"), 2);
}

#[test]
fn dispatch_nested_loops() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\nfor i in range(3):\n    for j in range(3):\n        x += 1\n");
    assert_eq!(fx.gi("x"), 9);
}

#[test]
fn dispatch_while_break_continue() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ni = 0\nwhile i < 10:\n    i += 1\n    if i == 3:\n        continue\n    if i == 6:\n        break\n    x += i\n");
    assert_eq!(fx.gi("x"), 1 + 2 + 4 + 5);
}

// ── functions / closures / generators ───────────────────────────────

#[test]
fn dispatch_closure() {
    let mut fx = Fx::new();
    fx.run_ok("def f():\n    x = 10\n    def g():\n        return x\n    return g()\ny = f()\n");
    assert_eq!(fx.gi("y"), 10);
}

#[test]
fn dispatch_closure_mutate() {
    let mut fx = Fx::new();
    fx.run_ok("def f():\n    x = 1\n    def g():\n        nonlocal x\n        x = 2\n        return x\n    g()\n    return x\ny = f()\n");
    assert_eq!(fx.gi("y"), 2);
}

#[test]
fn dispatch_default_args() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b=5):\n    return a + b\nx = f(1)\ny = f(1, 2)\n");
    assert_eq!(fx.gi("x"), 6);
    assert_eq!(fx.gi("y"), 3);
}

#[test]
fn dispatch_kwargs() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b=0):\n    return a - b\nx = f(10, b=3)\ny = f(a=1, b=2)\n");
    assert_eq!(fx.gi("x"), 7);
    assert_eq!(fx.gi("y"), -1);
}

#[test]
fn dispatch_varargs_tuple() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, *args):\n    return args\nx = f(1, 2, 3)\n");
    let items = fx.gtuple("x");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::SmallInt(2));
    assert_eq!(items[1], Value::SmallInt(3));
}

#[test]
fn dispatch_star_call() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b, c):\n    return a + b + c\nx = f(*[1, 2, 3])\n");
    assert_eq!(fx.gi("x"), 6);
}

#[test]
fn dispatch_kwargs_call() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, b):\n    return a + b\nx = f(**{\"a\": 1, \"b\": 2})\n");
    assert_eq!(fx.gi("x"), 3);
}

#[test]
fn dispatch_kwargs_call_kwonly() {
    let mut fx = Fx::new();
    fx.run_ok("def f(a, *, b):\n    return a + b\nx = f(1, b=2)\n");
    assert_eq!(fx.gi("x"), 3);
}

#[test]
fn dispatch_generator() {
    // (No `list()` call — that is C12a's `type_call`; collect
    // the yields with explicit `next()` calls instead.)
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    yield 2\n    yield 3\nit = g()\na = next(it)\nb = next(it)\nc = next(it)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gi("c"), 3);
}

#[test]
fn dispatch_generator_next() {
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 10\n    yield 20\ngen = g()\na = next(gen)\nb = next(gen)\n");
    assert_eq!(fx.gi("a"), 10);
    assert_eq!(fx.gi("b"), 20);
}

#[test]
fn dispatch_yield_from() {
    // (No `list()` call — that is C12a's `type_call`; collect
    // the yields with explicit `next()` calls instead.)
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield from [1, 2]\n    yield 3\nit = g()\na = next(it)\nb = next(it)\nc = next(it)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gi("c"), 3);
}

#[test]
fn dispatch_gen_throw() {
    // `gen.throw` injects the exception at the `YIELD_VALUE`
    // the generator is suspended on; the `except` catches it.
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    try:\n        yield 1\n    except ValueError:\n        yield 2\nit = g()\nnext(it)\nx = it.throw(ValueError)\n");
    assert_eq!(fx.gi("x"), 2);
}

#[test]
fn dispatch_gen_close_finally() {
    // `close()` injects `GeneratorExit`; the `finally` runs.
    let mut fx = Fx::new();
    fx.run_ok("y = 0\ndef g():\n    try:\n        yield 1\n    finally:\n        global y\n        y = 5\ngen = g()\nnext(gen)\ngen.close()\n");
    assert_eq!(fx.gi("y"), 5);
}

#[test]
fn dispatch_gen_return_value() {
    // The `return 42` becomes the `StopIteration` value on
    // exhaustion (the second `next()` raises it); the yield
    // itself is the first `next()`. (No `list()` call — that
    // is C12a's `type_call`.)
    let mut fx = Fx::new();
    fx.run_ok("def g():\n    yield 1\n    return 42\nit = g()\na = next(it)\ntry:\n    next(it)\n    b = 0\nexcept StopIteration:\n    b = 1\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 1);
}

#[test]
fn dispatch_recursion() {
    let mut fx = Fx::new();
    fx.run_ok("def fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nx = fib(10)\n");
    assert_eq!(fx.gi("x"), 55);
}

// ── exceptions ───────────────────────────────────────────────────────

#[test]
fn dispatch_try_except() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    1 // 0\nexcept ZeroDivisionError:\n    x = 1\n");
    assert_eq!(fx.gi("x"), 1);
}

#[test]
fn dispatch_try_finally() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    x = 1\nfinally:\n    x = 2\n");
    assert_eq!(fx.gi("x"), 2);
}

#[test]
fn dispatch_try_finally_exc() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    try:\n        raise ValueError\n    finally:\n        x = 3\nexcept ValueError:\n    x = 4\n");
    assert_eq!(fx.gi("x"), 4);
}

#[test]
fn dispatch_try_finally_return() {
    // `return` unwinds through the `finally` (the `-1`
    // sentinel), which runs before the value is returned.
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ndef f():\n    try:\n        return 1\n    finally:\n        global x\n        x = 5\ny = f()\n");
    assert_eq!(fx.gi("y"), 1);
    assert_eq!(fx.gi("x"), 5);
}

#[test]
fn dispatch_raise_obj() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    raise ValueError(\"oops\")\nexcept ValueError:\n    x = 1\n");
    assert_eq!(fx.gi("x"), 1);
}

#[test]
fn dispatch_raise_msg() {
    let mut fx = Fx::new();
    match fx.run("raise ValueError(\"oops\")\n") {
        Err(VmError::Raised(exc)) => {
            assert_eq!(fx.exc_name(exc), "ValueError");
            assert_eq!(fx.exc_value(exc), "oops");
        }
        other => panic!("expected a raise, got {other:?}"),
    }
}

#[test]
fn dispatch_raise_from() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    try:\n        raise ValueError(\"a\")\n    except ValueError:\n        raise TypeError(\"b\") from None\nexcept TypeError:\n    x = 1\n");
    assert_eq!(fx.gi("x"), 1);
}

#[test]
fn dispatch_reraise() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    try:\n        raise ValueError\n    except ValueError:\n        raise\nexcept ValueError:\n    x = 1\n");
    assert_eq!(fx.gi("x"), 1);
}

#[test]
fn dispatch_unhandled_exc() {
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(&mut fx, "raise ValueError(\"boom\")\n"),
        "ValueError"
    );
}

#[test]
fn dispatch_name_error() {
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(&mut fx, "x = undefined_name\n"),
        "NameError"
    );
}

#[test]
fn dispatch_type_error_call() {
    let mut fx = Fx::new();
    assert_eq!(raised_name(&mut fx, "x = 1(2)\n"), "TypeError");
}

#[test]
fn dispatch_import_error() {
    // The import system is live (C15b) — with no module of
    // that name registered and no file on `sys.path`, `import
    // nosuchmodule_xyz` raises `ModuleNotFoundError` (an
    // `ImportError` subclass; the C13 stub raised a plain
    // `ImportError`). (D2 registers the real `os` module, so
    // the old `import os` form of this test no longer raises.)
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(&mut fx, "import nosuchmodule_xyz\n"),
        "ModuleNotFoundError"
    );
}

#[test]
fn dispatch_except_tuple() {
    let mut fx = Fx::new();
    fx.run_ok("x = 0\ntry:\n    raise KeyError(\"k\")\nexcept (ValueError, KeyError):\n    x = 1\n");
    assert_eq!(fx.gi("x"), 1);
}

#[test]
fn dispatch_except_nomatch() {
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(
            &mut fx,
            "try:\n    raise ValueError\nexcept TypeError:\n    x = 1\n"
        ),
        "ValueError"
    );
}

// ── collections ──────────────────────────────────────────────────────

#[test]
fn dispatch_list_ops() {
    let mut fx = Fx::new();
    fx.run_ok("a = [1, 2, 3]\na.append(4)\nb = a[1]\nc = a[-1]\n");
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gi("c"), 4);
    let items = fx.glist("a");
    assert_eq!(items.len(), 4);
}

#[test]
fn dispatch_list_comp() {
    // (No `range()` call — that is C12a's `type_call`; the
    // comprehension iterates a list literal instead.)
    let mut fx = Fx::new();
    fx.run_ok("a = [x * 2 for x in [0, 1, 2, 3]]\n");
    let items = fx.glist("a");
    assert_eq!(items.len(), 4);
    assert_eq!(items[0], Value::SmallInt(0));
    assert_eq!(items[1], Value::SmallInt(2));
    assert_eq!(items[3], Value::SmallInt(6));
}

#[test]
fn dispatch_dict_comp() {
    // (No `range()` call — that is C12a's `type_call`.)
    let mut fx = Fx::new();
    fx.run_ok("a = {k: k * k for k in [0, 1, 2]}\nb = a[2]\n");
    assert_eq!(fx.gi("b"), 4);
}

#[test]
fn dispatch_set_comp() {
    // (No `range()` call — that is C12a's `type_call`.)
    let mut fx = Fx::new();
    fx.run_ok("a = {x for x in [0, 1, 2]}\nb = 2 in a\nc = 9 in a\n");
    assert!(fx.gb("b"));
    assert!(!fx.gb("c"));
}

#[test]
fn dispatch_dict_ops() {
    let mut fx = Fx::new();
    fx.run_ok("a = {}\na[\"k\"] = 5\nb = a[\"k\"]\n");
    assert_eq!(fx.gi("b"), 5);
}

#[test]
fn dispatch_tuple_build() {
    let mut fx = Fx::new();
    fx.run_ok("a = (1, 2, 3)\nb = a[0]\nc = a[1:]\n");
    assert_eq!(fx.gi("b"), 1);
    let items = fx.gtuple("c");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::SmallInt(2));
}

#[test]
fn dispatch_unpack() {
    let mut fx = Fx::new();
    fx.run_ok("a, b = (1, 2)\nc, *d = (1, 2, 3)\n");
    assert_eq!(fx.gi("a"), 1);
    assert_eq!(fx.gi("b"), 2);
    assert_eq!(fx.gi("c"), 1);
    let items = fx.glist("d");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::SmallInt(2));
    assert_eq!(items[1], Value::SmallInt(3));
}

#[test]
fn dispatch_unpack_error() {
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(&mut fx, "a, b = (1, 2, 3)\n"),
        "ValueError"
    );
}

#[test]
fn dispatch_slice() {
    let mut fx = Fx::new();
    fx.run_ok("a = [1, 2, 3, 4]\nb = a[1:3]\nc = a[::2]\n");
    let items = fx.glist("b");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::SmallInt(2));
    assert_eq!(items[1], Value::SmallInt(3));
    let items = fx.glist("c");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::SmallInt(1));
    assert_eq!(items[1], Value::SmallInt(3));
}

#[test]
fn dispatch_str_ops() {
    let mut fx = Fx::new();
    fx.run_ok("a = \"hello\"\nb = a[1]\nc = a + \" world\"\nd = a * 2\n");
    assert_eq!(fx.gs("b"), "e");
    assert_eq!(fx.gs("c"), "hello world");
    assert_eq!(fx.gs("d"), "hellohello");
}

#[test]
fn dispatch_str_format() {
    let mut fx = Fx::new();
    fx.run_ok("a = \"%d-%s\" % (1, \"x\")\n");
    assert_eq!(fx.gs("a"), "1-x");
}

#[test]
fn dispatch_index_error() {
    let mut fx = Fx::new();
    assert_eq!(raised_name(&mut fx, "a = [1, 2]\nb = a[5]\n"), "IndexError");
}

#[test]
fn dispatch_key_error() {
    let mut fx = Fx::new();
    assert_eq!(raised_name(&mut fx, "a = {}\nb = a[\"k\"]\n"), "KeyError");
}

#[test]
fn dispatch_stop_iteration() {
    // Exhausting a generator raises `StopIteration` out of
    // `next()` (the `for` loop swallows it; a bare `next`
    // does not). (`iter` is not a builtin — use a generator.)
    let mut fx = Fx::new();
    assert_eq!(
        raised_name(
            &mut fx,
            "def g():\n    yield 1\nit = g()\nnext(it)\nnext(it)\n"
        ),
        "StopIteration"
    );
}

#[test]
fn dispatch_nested_call_chain() {
    // A longer program exercising many arms together.
    // (No `range()` call — that is C12a's `type_call`.)
    let mut fx = Fx::new();
    fx.run_ok("def add(a, b):\n    return a + b\ndef total(xs):\n    s = 0\n    for x in xs:\n        s = add(s, x)\n    return s\nx = total([1, 2, 3, 4])\ny = total([0, 1, 2, 3, 4])\n");
    assert_eq!(fx.gi("x"), 10);
    assert_eq!(fx.gi("y"), 10);
}
