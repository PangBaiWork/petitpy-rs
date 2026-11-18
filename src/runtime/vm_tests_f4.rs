//! Lane F4 regression tests: PEP 487 `__init_subclass__`, the
//! C3 MRO-inconsistency error, and `dict.__missing__`.
//!
//! Root causes:
//! * `__init_subclass__` — `call_build_class_ctx` called the
//!   loader's return value with one arg, but the loader
//!   collapses the (fun, bound) pair into a `BoundMeth`
//!   (`type_attr` → `class_lookup` → `bind_pair`), so the
//!   bound base was prepended and the 1-param
//!   `__init_subclass__(cls)` saw two args ("argument
//!   num/types mismatch"). The Zig calls `dest[0]` (the raw
//!   function) with the new class alone
//!   (`modbuiltins.zig:574-588`); the fix unwraps the
//!   `BoundMeth` at the call site.
//! * MRO — `c3_mro` already detected the inconsistency
//!   (`ObjTypeError::MroConflict`), but `type_make_new`
//!   returned only the bare `PyRaise` signal, so the VM could
//!   only raise a message-less `TypeError`. The fix implements
//!   `TypedRaise` for `ObjTypeError` (the message formats the
//!   base names exactly as the Zig does,
//!   `objtype.zig:1025-1030`) and records it in
//!   `type_make_new`.
//! * `__missing__` — `instance_subscr` already had the
//!   protocol, but it was dead code: `stub_subscr` mapped
//!   every slot failure to `SubscrError::Raise`, so the
//!   `SubscrError::KeyError` arm never fired. The fix makes
//!   `stub_subscr` report a dict Get miss as `KeyError`
//!   (detected from the recorded pending raise, re-recorded
//!   verbatim), and `instance_subscr` discards that record
//!   when `__missing__` handles the miss (otherwise the
//!   dispatch loop's polled-raise check would fire the stale
//!   `KeyError` instead of a raise from `__missing__` itself).
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs`, `vm_tests_e2f.rs`,
//! `vm_tests_e2g.rs`, `vm_tests_e2h.rs`, `vm_tests_e2i.rs` and
//! `vm_tests_e2j.rs`).

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

    fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not a small int: {other:?}"),
        }
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

// ── F4 (1): `__init_subclass__`, plain form ──────────────────────────
// The first base with `__init_subclass__` is called with the new
// class as `cls` (`modbuiltins.zig:574-588`); a subclass of the
// subclass sees its own parent's hook again.

#[test]
fn f4_init_subclass_plain() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
seen = []
class B:
    def __init_subclass__(cls):
        seen.append(cls.__name__)
class C(B): pass
class D(C): pass
seen_str = ','.join(seen)
"#,
    );
    assert_eq!(fx.gs("seen_str"), "C,D");
}

// ── F4 (2): `__init_subclass__`, `@classmethod` form ────────────────
// The classmethod form records the new class the same way
// (`'k:' + cls.__name__` — the new class, not the base).

#[test]
fn f4_init_subclass_classmethod() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
seen = []
class K:
    @classmethod
    def __init_subclass__(cls):
        seen.append('k:' + cls.__name__)
class L(K): pass
seen_str = ','.join(seen)
"#,
    );
    assert_eq!(fx.gs("seen_str"), "k:L");
}

// ── F4 (3): `__init_subclass__` is NOT called via `type()` ────────────
// The vendor calls the hook from `__build_class__` only
// (`modbuiltins.zig:571`), not from `mp_obj_new_type` — so a
// class created through the 3-arg `type()` call does NOT run
// it, on the reference AND here (probed on both binaries).
// CPython differs (it calls the hook from `type.__new__`);
// petitpy follows the vendor, not CPython, here.

#[test]
fn f4_init_subclass_via_type() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
seen = []
class B:
    def __init_subclass__(cls):
        seen.append(cls.__name__)
T = type('T', (B,), {})
seen_str = ','.join(seen)
"#,
    );
    assert_eq!(fx.gs("seen_str"), "");
}

// ── F4 (4): a raising `__init_subclass__` propagates ────────────────

#[test]
fn f4_init_subclass_raises() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        r#"
class RB:
    def __init_subclass__(cls):
        raise RuntimeError('no-sub')
class RC(RB): pass
"#,
    );
    assert_eq!(name, "RuntimeError");
    assert_eq!(msg, "no-sub");
}

// ── F4 (5): the exact MRO-inconsistent diamond ───────────────────────
// `class Z(A, B)` where `B(A)`: C3 has no valid head (A is in B's
// tail, B is in the bases' tail), so `c3Mro` raises with the
// bases' names in the message (`objtype.zig:1025-1030`).

#[test]
fn f4_mro_conflict() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        r#"
class A: pass
class B(A): pass
class Z(A, B): pass
"#,
    );
    assert_eq!(name, "TypeError");
    assert_eq!(
        msg,
        "Cannot create a consistent method resolution order (MRO) for bases A, B"
    );
}

// ── F4 (6): the same MRO failure through `type()` ────────────────────

#[test]
fn f4_mro_conflict_via_type() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        r#"
class A: pass
class B(A): pass
Z = type('Z', (A, B), {})
"#,
    );
    assert_eq!(name, "TypeError");
    assert_eq!(
        msg,
        "Cannot create a consistent method resolution order (MRO) for bases A, B"
    );
}

// ── F4 (7): a consistent diamond still linearises ────────────────────
// The no-regression guard for the MRO fix: `D(B, C)` merges to
// (D, B, C, A, object) and `E(D)` extends it.

#[test]
fn f4_mro_consistent_diamond() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def f(self): return 'A'
    def g(self): return ['A']
class B(A):
    def g(self): return ['B'] + super().g()
class C(A):
    def f(self): return 'C'
    def g(self): return ['C'] + super().g()
class D(B, C):
    def g(self): return ['D'] + super().g()
class E(D): pass
mro_str = ','.join(k.__name__ for k in E.__mro__)
f_result = E().f()
g_result = ','.join(E().g())
"#,
    );
    assert_eq!(fx.gs("mro_str"), "E,D,B,C,A,object");
    assert_eq!(fx.gs("f_result"), "C");
    assert_eq!(fx.gs("g_result"), "D,B,C,A");
}

// ── F4 (8): `__missing__`, basic ─────────────────────────────────────
// A dict subclass defining `__missing__` returns its result for
// an absent key instead of raising `KeyError`
// (`objdict.zig:270-282` — the vendor documents the protocol
// but does not implement it; the Rust `instance_subscr` path
// does).

#[test]
fn f4_missing_basic() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class D(dict):
    def __missing__(self, k): return 'm-' + k
v = D()['x']
"#,
    );
    assert_eq!(fx.gs("v"), "m-x");
}

// ── F4 (9): `__missing__`, the Counter pattern ───────────────────────
// `c['a'] += 1` reads through `__missing__` (0) and stores the
// result; a later read of a still-absent key still goes through
// `__missing__` (the return value is NOT stored).

#[test]
fn f4_missing_counter() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class C(dict):
    def __missing__(self, k): return 0
c = C()
c['a'] += 1; c['a'] += 1; c['b'] += 5
a = c['a']
b = c['b']
cc = c['c']
a_in = 'a' in c
c_in = 'c' in c
"#,
    );
    assert_eq!(fx.gi("a"), 2);
    assert_eq!(fx.gi("b"), 5);
    assert_eq!(fx.gi("cc"), 0);
    assert!(fx.gb("a_in"));
    assert!(!fx.gb("c_in"));
}

// ── F4 (10): a raising `__missing__` propagates ──────────────────────
// The raise from `__missing__` itself must win over the miss's
// `KeyError` (it travels on the hook's `pending_exc`; the
// recorded `KeyError` is discarded when `__missing__` is
// consulted, so the dispatch loop's polled-raise check cannot
// fire it instead).

#[test]
fn f4_missing_raises() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        r#"
class R(dict):
    def __missing__(self, k): raise ValueError('boom-' + k)
print(R()['z'])
"#,
    );
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "boom-z");
}

// ── F4 (11): the `__missing__` result is not stored ──────────────────
// A hit through `__missing__` does not insert the key: `'x' in
// d` stays False and `.get` still returns its default (`.get`
// never calls `__missing__`).

#[test]
fn f4_missing_not_stored() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class D(dict):
    def __missing__(self, k): return 'm-' + k
d = D()
v = d['x']
x_in = 'x' in d
g = d.get('x', 'DFLT')
"#,
    );
    assert_eq!(fx.gs("v"), "m-x");
    assert!(!fx.gb("x_in"));
    assert_eq!(fx.gs("g"), "DFLT");
}

// ── F4 (12): exact-dict misses still raise `KeyError` ────────────────
// `__missing__` applies to dict SUBCLASSES only; a plain dict
// miss and a subclass without `__missing__` keep raising.

#[test]
fn f4_exact_dict_miss_still_keyerror() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg("{}['x']\n");
    assert_eq!(name, "KeyError");
    assert_eq!(msg, "x");
    let (name, msg) = fx.raised_msg(
        r#"
class N(dict): pass
print(N()['x'])
"#,
    );
    assert_eq!(name, "KeyError");
    assert_eq!(msg, "x");
}

// ── F4 (13): `del` of a missing key still raises `KeyError` ──────────
// Deletion never consults `__missing__` (Get only).

#[test]
fn f4_missing_del_still_keyerror() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        r#"
class D(dict): pass
d = D()
del d['q']
"#,
    );
    assert_eq!(name, "KeyError");
    assert_eq!(msg, "q");
}
