//! End-to-end type-call tests (C13b): run small Python snippets
//! through `Vm::load_module_from_source` and assert on the
//! module's globals or on the escaped exception. Every builtin
//! type wired by this lane is called as a value here — the
//! `range(3)` / `list(...)` / `dict(...)` / `int(...)` calls the
//! C13 dispatch could not make (they raised "object not
//! callable").
//!
//! Notes on what the snippets avoid (not this lane's scope):
//! - `class C` WITH a Python `__init__` (`C(7)`): the
//!   `__init__` call goes through `with_stub_seams`, whose
//!   `call` is the builtin-live `call_via_type_slot` —
//!   `PyRaise` for a `FunBc` until the `VmSeams` runner
//!   replacement lands (the C12a-documented C13/C14
//!   follow-up). A class WITHOUT `__init__` (or with methods
//!   called via `LOAD_METHOD`, the VM's own path) works.
//! - `array(...)` / `deque(...)`: `array` and `deque` are NOT
//!   builtins (`from array import array`,
//!   `ucollections.deque`) — their `make_new` slots are wired
//!   but latent until the module lanes bind them.
//! - `float(...)` / `complex(...)`: C18a's adapters (see
//!   `makenew_float_make_new_landed`, which pins `float(1) == 1.0`
//!   — the C13b `type_call` "can't create instance" pin is
//!   retired).
//! - `len` / `iter`: D1's builtins-module bindings (the
//!   snippets use indexing / iteration instead).

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

    /// A global's type name (`"range"`, `"C"`, …).
    fn gt(&mut self, name: &str) -> String {
        let v = self
            .g(name)
            .unwrap_or_else(|| panic!("global {name} is missing"));
        let t = obj_get_type(&self.vm.arena, &self.vm.types, v)
            .expect("a value has a type");
        match self.vm.arena.get(t) {
            Obj::Type(d) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(d.name as u16)).into_owned()
            }
            _ => panic!("type is not a type object"),
        }
    }

    /// A global bytes' content.
    fn gbytes(&mut self, name: &str) -> Vec<u8> {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Bytes(b) => b.bytes.clone(),
                other => panic!("global {name} is not bytes: {other:?}"),
            },
            other => panic!("global {name} is not bytes: {other:?}"),
        }
    }

    /// A global array/bytearray's (type name, data bytes).
    fn garray(&mut self, name: &str) -> (String, Vec<u8>) {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Array(d) => {
                    let t = self
                        .vm
                        .arena
                        .header(r)
                        .type_
                        .expect("array has a header type");
                    let tname = match self.vm.arena.get(t) {
                        Obj::Type(td) => String::from_utf8_lossy(
                            self.vm.pool.qstr_str(td.name as u16),
                        )
                        .into_owned(),
                        _ => panic!("array header is not a type"),
                    };
                    (tname, d.data[..d.len].to_vec())
                }
                other => panic!("global {name} is not an array: {other:?}"),
            },
            other => panic!("global {name} is not an array: {other:?}"),
        }
    }

    /// A global dict's value for a string key (`None` when
    /// absent).
    fn gd(&mut self, name: &str, key: &str) -> Option<Value> {
        let d = self.g(name)?;
        let q = self.vm.pool.from_strn(key.as_bytes()).ok()? as u32;
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, d, Value::Qstr(q)).ok()
    }

    /// A `Str` / `Qstr` value as a Rust `String`.
    fn vs(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                _ => format!("{v:?}"),
            },
            _ => format!("{v:?}"),
        }
    }

    /// A 2-tuple of small ints, as a Rust pair.
    fn pair(&mut self, v: Value) -> (i64, i64) {
        match v {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Tuple(items) => match (items.first(), items.get(1)) {
                    (Some(Value::SmallInt(a)), Some(Value::SmallInt(b))) => (*a, *b),
                    _ => panic!("tuple item is not a small-int pair: {items:?}"),
                },
                other => panic!("value is not a tuple: {other:?}"),
            },
            _ => panic!("value is not a tuple: {v:?}"),
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

/// The escaped exception's (type name, message), or panics
/// when the module ran clean.
fn raised(fx: &mut Fx, src: &str) -> (String, String) {
    match fx.run(src) {
        Err(VmError::Raised(exc)) => (fx.exc_name(exc), fx.exc_value(exc)),
        Ok(_) => panic!("expected the module to raise, but it ran clean"),
        Err(e) => panic!("expected a raise, got a load failure: {e:?}"),
    }
}

// ── builtin type calls ───────────────────────────────────────────────

#[test]
fn makenew_range_call() {
    let mut fx = Fx::new();
    fx.run_ok("r = range(3)\nl = list(r)\n");
    assert_eq!(fx.gt("r"), "range");
    assert_eq!(fx.glist("l"), vec![Value::SmallInt(0), Value::SmallInt(1), Value::SmallInt(2)]);
}

#[test]
fn makenew_list_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = list([1, 2])\nb = list()\nc = list('ab')\n");
    assert_eq!(fx.glist("a"), vec![Value::SmallInt(1), Value::SmallInt(2)]);
    assert!(fx.glist("b").is_empty());
    assert_eq!(fx.gt("b"), "list");
    let c = fx.glist("c");
    assert_eq!(c.len(), 2);
    assert_eq!(fx.vs(c[0]), "a");
    assert_eq!(fx.vs(c[1]), "b");
}

#[test]
fn makenew_tuple_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = tuple((1, 2))\nb = tuple()\nc = tuple([3])\n");
    assert_eq!(fx.gtuple("a"), vec![Value::SmallInt(1), Value::SmallInt(2)]);
    assert!(fx.gtuple("b").is_empty());
    assert_eq!(fx.gtuple("c"), vec![Value::SmallInt(3)]);
}

#[test]
fn makenew_dict_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = dict()\nb = dict(a=1)\nc = dict([('k', 9)])\nd = dict(a=1, b=2)\n");
    assert_eq!(fx.gt("a"), "dict");
    assert_eq!(fx.gd("b", "a"), Some(Value::SmallInt(1)));
    assert_eq!(fx.gd("c", "k"), Some(Value::SmallInt(9)));
    assert_eq!(fx.gd("d", "a"), Some(Value::SmallInt(1)));
    assert_eq!(fx.gd("d", "b"), Some(Value::SmallInt(2)));
}

#[test]
fn makenew_set_frozenset_call() {
    let mut fx = Fx::new();
    fx.run_ok("s = set([1, 2])\nf = frozenset([1, 2])\na = 1 in s\nb = 2 in f\n");
    assert_eq!(fx.gt("s"), "set");
    assert_eq!(fx.gt("f"), "frozenset");
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
}

#[test]
fn makenew_int_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = int('42')\nb = int()\nc = int('42', 16)\nd = int(3.7)\n");
    assert_eq!(fx.gi("a"), 42);
    assert_eq!(fx.gi("b"), 0);
    assert_eq!(fx.gi("c"), 66);
    assert_eq!(fx.gi("d"), 3);
}

#[test]
fn makenew_str_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = str(1)\nb = str()\nc = str('x')\n");
    assert_eq!(fx.gs("a"), "1");
    assert_eq!(fx.gs("b"), "");
    assert_eq!(fx.gs("c"), "x");
}

#[test]
fn makenew_bool_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = bool(1)\nb = bool()\nc = bool(0)\n");
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    assert!(!fx.gb("c"));
}

#[test]
fn makenew_bytes_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = bytes(b'a')\nb = bytes()\n");
    assert_eq!(fx.gbytes("a"), b"a");
    assert_eq!(fx.gt("a"), "bytes");
    assert!(fx.gbytes("b").is_empty());
}

#[test]
fn makenew_bytearray_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = bytearray(3)\nb = bytearray(b'ab')\n");
    assert_eq!(fx.garray("a"), ("bytearray".to_string(), vec![0, 0, 0]));
    assert_eq!(fx.garray("b"), ("bytearray".to_string(), vec![b'a', b'b']));
}

#[test]
fn makenew_slice_call() {
    let mut fx = Fx::new();
    fx.run_ok("s = slice(1, 2)\nt = slice(1, 2).indices(5)\n");
    assert_eq!(fx.gt("s"), "slice");
    assert_eq!(
        fx.gtuple("t"),
        vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(1)]
    );
}

#[test]
fn makenew_map_call() {
    let mut fx = Fx::new();
    fx.run_ok("m = map(str, [1, 2])\nl = list(m)\n");
    assert_eq!(fx.gt("m"), "map");
    let l = fx.glist("l");
    assert_eq!(l.len(), 2);
    assert_eq!(fx.vs(l[0]), "1");
    assert_eq!(fx.vs(l[1]), "2");
}

#[test]
fn makenew_filter_call() {
    let mut fx = Fx::new();
    fx.run_ok("f = filter(None, [0, 1])\nl = list(f)\n");
    assert_eq!(fx.gt("f"), "filter");
    assert_eq!(fx.glist("l"), vec![Value::SmallInt(1)]);
}

#[test]
fn makenew_zip_call() {
    let mut fx = Fx::new();
    fx.run_ok("z = zip([1], [2])\nl = list(z)\n");
    assert_eq!(fx.gt("z"), "zip");
    let l = fx.glist("l");
    assert_eq!(l.len(), 1);
    assert_eq!(fx.pair(l[0]), (1, 2));
}

#[test]
fn makenew_enumerate_call() {
    let mut fx = Fx::new();
    fx.run_ok("e = enumerate([7])\nl = list(e)\n");
    assert_eq!(fx.gt("e"), "enumerate");
    let l = fx.glist("l");
    assert_eq!(l.len(), 1);
    assert_eq!(fx.pair(l[0]), (0, 7));
}

#[test]
fn makenew_property_call() {
    let mut fx = Fx::new();
    fx.run_ok("p = property()\n");
    assert_eq!(fx.gt("p"), "property");
}

#[test]
fn makenew_type_call() {
    let mut fx = Fx::new();
    fx.run_ok("a = type(1) is int\nb = type('X', (), {})\n");
    assert!(fx.gb("a"));
    assert_eq!(fx.gt("b"), "type");
}

#[test]
fn makenew_object_call() {
    let mut fx = Fx::new();
    fx.run_ok("o = object()\n");
    assert_eq!(fx.gt("o"), "object");
}

#[test]
fn makenew_bytes_encoding_kwarg() {
    let mut fx = Fx::new();
    fx.run_ok("b = bytes('ab', encoding='utf-8')\n");
    assert_eq!(fx.gbytes("b"), b"ab");
}

// ── user classes ─────────────────────────────────────────────────────

#[test]
fn makenew_class_empty() {
    let mut fx = Fx::new();
    fx.run_ok("class C: pass\nc = C()\n");
    assert_eq!(fx.gt("c"), "C");
}

#[test]
fn makenew_class_attr() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n x = 1\nc = C()\nb = c.x\n");
    assert_eq!(fx.gi("b"), 1);
}

/// `C.x` (attribute read ON the class object) — ignored until
/// C14 rewrites `load_method_maybe_ctx` (its doc: "C14 subsumes
/// this whole function"): the interim looks `x` up in `type`'s
/// locals dict instead of `C`'s own dict, so it raises
/// `AttributeError("no attribute 'x'")`. The instance read
/// (`c.x`, above) already proves the class dict is wired.
#[test]
#[ignore]
fn makenew_class_attr_on_class() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n x = 1\na = C.x\n");
    assert_eq!(fx.gi("a"), 1);
}

/// `C.x = 7` (instance attribute store through the STORE_ATTR
/// opcode) — un-ignored by D1: `store_attr_ctx` is now the real
/// C14 `runtime_method::store_attr` (the `Instance` members map
/// + the attr slot), not the stub that always raised
/// `AttributeError`.
#[test]
fn makenew_class_member_store() {
    let mut fx = Fx::new();
    fx.run_ok("class C: pass\nc = C()\nc.x = 7\na = c.x\n");
    assert_eq!(fx.gi("a"), 7);
}

#[test]
fn makenew_class_method() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n def m(self):\n  return 42\nc = C()\na = c.m()\n");
    assert_eq!(fx.gi("a"), 42);
}

#[test]
fn makenew_class_int_base() {
    let mut fx = Fx::new();
    fx.run_ok("class CI(int): pass\na = CI(5) + 1\n");
    assert_eq!(fx.gi("a"), 6);
}

// ── error mapping ────────────────────────────────────────────────────

#[test]
fn makenew_error_list_args() {
    let mut fx = Fx::new();
    // The polled sig error is overwritten by the `getiter`
    // failure (the C9b precedent) — the reference raises
    // `TypeError("object not iterable")`, not the mismatch.
    assert_eq!(
        raised(&mut fx, "x = list(1, 2)\n"),
        ("TypeError".to_string(), "object not iterable".to_string())
    );
}

#[test]
fn makenew_error_range_no_args() {
    let mut fx = Fx::new();
    assert_eq!(
        raised(&mut fx, "x = range()\n"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

#[test]
fn makenew_error_int_bad_str() {
    let mut fx = Fx::new();
    assert_eq!(
        raised(&mut fx, "x = int('x')\n"),
        (
            "ValueError".to_string(),
            "invalid syntax for integer".to_string()
        )
    );
}

#[test]
fn makenew_float_make_new_landed() {
    let mut fx = Fx::new();
    // C18a: `float` has a `make_new` adapter now — `float(1)`
    // is `1.0`, not the C13b `type_call` "can't create instance"
    // (that pin, `makenew_error_float_no_make_new`, is retired
    // with this test — the `float(1)` raise it pinned no longer
    // happens).
    fx.run_ok("x = float(1)\n");
    match fx.g("x") {
        Some(Value::Heap(r)) => match fx.vm.arena.get(r) {
            Obj::Float(f) => assert_eq!(*f, 1.0),
            other => panic!("float(1) is not a float: {other:?}"),
        },
        other => panic!("float(1) global missing: {other:?}"),
    }
}

#[test]
fn makenew_error_type_bad_name() {
    let mut fx = Fx::new();
    // `type(1, (), {})`: the name is not a string. Only the
    // type is asserted — `type_make_new` maps its
    // `ObjTypeError` through a bare `to_pyerror`, so the
    // "can't convert to str implicitly" message is lost on
    // the VM path (a known C12a gap, not this lane's).
    assert_eq!(raised(&mut fx, "x = type(1, (), {})\n").0, "TypeError");
}

// ── `and`/`or` short-circuit with a variable LHS (D3 merge) ────────
// C13 decoded the `JUMP_IF_*_OR_POP` label with the SIGNED decoder
// (`vm.zig` uses the UNSIGNED one), so a taken jump landed 64 bytes
// early and the next fetch decoded garbage (`NotImplementedError`
// "opcode"). A constant LHS never took the jump (the compiler folds
// it), so only the variable-LHS taken path failed. Found by Lane D3.

#[test]
fn and_false_var_shortcircuit() {
    let mut fx = Fx::new();
    fx.run_ok("ok = False\nok = ok and len(\"x\") == 1\nassert ok == False\n");
    let mut fx = Fx::new();
    fx.run_ok("ok = False\nok = ok and len(\"x\")\nassert ok == False\n");
}

#[test]
fn or_true_var_shortcircuit() {
    let mut fx = Fx::new();
    fx.run_ok("ok = True\nok = ok or len(\"x\") == 1\nassert ok == True\n");
    let mut fx = Fx::new();
    fx.run_ok("ok = True\nok = ok or len(\"x\")\nassert ok == True\n");
}

#[test]
fn and_true_var_continues() {
    let mut fx = Fx::new();
    fx.run_ok("ok = True\nok = ok and len(\"x\") == 1\nassert ok == True\n");
    let mut fx = Fx::new();
    fx.run_ok("ok = False\nok = ok or len(\"x\") == 1\nassert ok == True\n");
}

// ── `del x.attr` through STORE_ATTR (D2 merge) ───────────────────────
// The value slot holds NULL for `del x.attr` (LOAD_NULL;
// STORE_ATTR) — the dispatch must pass the `None` through to
// `store_attr_ctx` (delete), not coerce it to the `None` object
// (which would STORE None instead of deleting). Probed on the
// reference 2026-11-18: hasattr is False after `del c.x`.
#[test]
fn del_instance_attr_opcode() {
    let mut fx = Fx::new();
    fx.run_ok("class C:\n    pass\nc = C()\nc.x = 7\nassert hasattr(c, 'x')\ndel c.x\nassert not hasattr(c, 'x')\n");
}
