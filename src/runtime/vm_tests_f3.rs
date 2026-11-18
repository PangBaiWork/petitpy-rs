//! Lane F3 regression tests: native-subclass operands + enum/hash.
//!
//! Eight mechanisms (see `scripts/f3_result.txt`):
//! 1. `nativeSubclassOp` in `binary_op_ctx` — an int/float/str
//!    subclass on the right of a native scalar works as its value
//!    (`1 + R(2)`, `1 < R(2)`, `'a' + S('b')`, `1 + V(2)` via the
//!    reflected `__radd__`, `1 + W(2)` returning `NotImplemented`
//!    as-is).
//! 2. The SENTINEL pre-check in `try_instance_binary_op` — a
//!    subclass instance on the LEFT routes through the FULL
//!    `binary_op_ctx` on `(subobj[0], rhs)`, so `S('ab') < S('ac')`
//!    is True and `L([1,2]) + L([3])` raises `('list', 'list')`
//!    (the vendor's inner `mp_binary_op` reverse-swaps and unwraps
//!    the rhs too — E2j §4(b), corrected: `L < L` raises
//!    `('list', 'L')`, it does NOT return True).
//! 3. The `HASH` arm in `str_unary_op` — `hash(S('ab'))` is
//!    `hash('ab')` (E2j §4(a)).
//! 4. Hook-aware dict subscript load/store/delete in the VM
//!    dispatch — `@enum` members store twice (E2g §4, test_enum).
//! 5. `nativeBase` in `str_join` — `''.join([S('x'), 'y'])`.
//! 6. The hook-aware brace engine — `'{:03d}'.format(R(7))`,
//!    `f'{Q():z}'`, `format(Q(), 'y')`, `f'{N.TWO:03d}'`,
//!    `f'{Code.KERNEL}'`.
//! 7. `nativeBase` in the `%` engine — `'%d' % F(3.9)`.
//! 8. The deferred `nativeBase` step in `obj_get_float_maybe` —
//!    `float(F(2.5))` (the Zig's `mp_obj_get_float_maybe`,
//!    `obj.zig:676-684`).
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

    /// The module raised an exception of this type with this
    /// message (and nothing else).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module did not raise"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }
}

// ── F3 (1): `nativeSubclassOp` — native scalar on the left ──────────
// `1 + R(2)` / `1 < R(2)` / `2 > R(1)` / `3 // R(2)` /
// `1.5 + F(1.0)` / `'a' + S('b')` / `'a' < S('b')` all raised
// `TypeError: unsupported types ...` on base (the vendor's
// `nativeSubclassOp` was a stub in `binary_op_ctx`).

#[test]
fn f3_native_subclass_rhs() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class R(int): pass
class F(float): pass
class S(str): pass
a = 1 + R(2)
b = 1 < R(2)
c = 2 > R(1)
d = 3 // R(2)
e = 1.5 + F(1.0)
f = 'a' + S('b')
g = 'a' < S('b')
h = 1.0 + R(2)
"#,
    );
    assert_eq!(fx.gi("a"), 3);
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert_eq!(fx.gi("d"), 1);
    assert_eq!(fx.gf("e"), 2.5);
    assert_eq!(fx.gs("f"), "ab");
    assert!(fx.gb("g"));
    assert_eq!(fx.gf("h"), 3.0);
}

// ── F3 (1b): reflected method first ──────────────────────────────────
// `1 + V(2)` returns `'radd'` (the reflected `__radd__` runs);
// `1 + W(2)` returns `NotImplemented` AS-IS (the vendor returns
// the reflected result without converting it to an error).

#[test]
fn f3_native_reflected_method() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class V(int):
    def __radd__(self, o): return 'radd'
class W(int):
    def __radd__(self, o): return NotImplemented
a = 1 + V(2)
b = 1 + W(2)
c = b is NotImplemented
"#,
    );
    assert_eq!(fx.gs("a"), "radd");
    assert!(fx.gb("c"));
}

// ── F3 (1c): `sorted` / `sum` over int-subclass values ───────────────
// `sorted([R(3), 1, R(2)])` raised `('R', 'R')` on base (the
// sort's comparison goes through `binary_op_ctx`, whose
// instance-lhs path is Fix 2); `sum` goes through Fix 1.

#[test]
fn f3_native_sorted_sum() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class R(int): pass
s = sorted([R(3), 1, R(2)])
s0 = int(s[0])
s1 = int(s[1])
s2 = int(s[2])
t = sum([R(1), R(2)])
"#,
    );
    assert_eq!(fx.gi("s0"), 1);
    assert_eq!(fx.gi("s1"), 2);
    assert_eq!(fx.gi("s2"), 3);
    assert_eq!(fx.gi("t"), 3);
}

// ── F3 (2): SENTINEL full protocol — str subclass ordering ──────────
// `S('ab') < S('ac')` raised `('S', 'S')` on base (the slot-level
// seam); the full protocol delegates to the str slot on the
// unwrapped values and returns True.

#[test]
fn f3_subclass_str_ordering() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class S(str): pass
a = S('ab') < S('ac')
b = S('ab') <= S('ab')
c = S('ac') > S('ab')
d = S('ab') + S('c')
"#,
    );
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(fx.gb("c"));
    assert_eq!(fx.gs("d"), "abc");
}

// ── F3 (2b): SENTINEL full protocol — list subclass error names ─────
// E2j §4(b), CORRECTED: `L([1,2]) < L([1,3])` does NOT return True
// on the reference — it raises `('list', 'L')` (the list slot
// refuses a non-list rhs). `L + L` raises `('list', 'list')` (the
// vendor's inner `mp_binary_op` reverse-swaps and unwraps the rhs
// too). Both named `('L', 'L')` on base.

#[test]
fn f3_subclass_list_error_names() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(
            r#"
class L(list): pass
L([1, 2]) < L([1, 3])
"#,
        ),
        (
            "TypeError".to_string(),
            "unsupported types for __lt__: 'list', 'L'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg(
            r#"
class L(list): pass
L([1, 2]) + L([3])
"#,
        ),
        (
            "TypeError".to_string(),
            "unsupported types for __add__: 'list', 'list'".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg(
            r#"
class L(list): pass
[1, 2] + L([3])
"#,
        ),
        (
            "TypeError".to_string(),
            "unsupported types for __add__: 'list', 'list'".to_string()
        )
    );
}

// ── F3 (2c): instance on the left, plain arithmetic ──────────────────
// `R(2) + 1` already passed on base (the int slot answers); the
// no-regression guard for the SENTINEL recursion.

#[test]
fn f3_subclass_instance_lhs_arith() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class R(int): pass
a = R(2) + 1
b = R(2) - 1
c = R(2) * 3
d = R(7) % 3
e = R(7) / 2
f = R(2) ** 3
g = -R(2)
h = abs(R(-2))
"#,
    );
    assert_eq!(fx.gi("a"), 3);
    assert_eq!(fx.gi("b"), 1);
    assert_eq!(fx.gi("c"), 6);
    assert_eq!(fx.gi("d"), 1);
    assert_eq!(fx.gf("e"), 3.5);
    assert_eq!(fx.gi("f"), 8);
    assert_eq!(fx.gi("g"), -2);
    assert_eq!(fx.gi("h"), 2);
}

// ── F3 (3): `hash(S('ab'))` ──────────────────────────────────────────
// E2j §4(a): `hash(S('ab'))` raised "unsupported type for
// operator" (the str slot had no HASH arm); str-subclass dict
// keys failed the same way. `hash('ab')` is 28198.

#[test]
fn f3_hash_str_subclass() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class S(str): pass
a = hash(S('ab'))
b = hash('ab')
c = {S('ab'): 1}[S('ab')]
d = {S('ab'): 1}['ab']
"#,
    );
    assert_eq!(fx.gi("a"), 28198);
    assert_eq!(fx.gi("b"), 28198);
    assert_eq!(fx.gi("c"), 1);
    assert_eq!(fx.gi("d"), 1);
}

// ── F3 (4): @enum dict subscript store / load / delete ───────────────
// E2g §4 / test_enum `test_members_are_hashable`: the dict
// LITERAL stored both keys (STORE_MAP used `dict_store_hook`),
// but a subscript store raised on the FIRST store and a
// subscript load raised on the second key (LOAD/STORE_SUBSCR
// were ctx-only).

#[test]
fn f3_enum_dict_subscr() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
from enum import Enum
class Color(Enum):
    RED = 1
    GREEN = 2
d = {}
d[Color.RED] = "one"
d[Color.GREEN] = "two"
a = d[Color.RED]
b = d[Color.GREEN]
n = len(d)
del d[Color.RED]
m = len(d)
c = d[Color.GREEN]
"#,
    );
    assert_eq!(fx.gs("a"), "one");
    assert_eq!(fx.gs("b"), "two");
    assert_eq!(fx.gi("n"), 2);
    assert_eq!(fx.gi("m"), 1);
    assert_eq!(fx.gs("c"), "two");
}

// ── F3 (4b): dict literal with @enum keys + subscript load ───────────
// The literal path already worked; the load did not (same E2g §4
// decomposition).

#[test]
fn f3_enum_dict_literal_load() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
from enum import Enum
class Color(Enum):
    RED = 1
    GREEN = 2
d = {Color.RED: "one", Color.GREEN: "two"}
a = d[Color.RED]
b = d[Color.GREEN]
"#,
    );
    assert_eq!(fx.gs("a"), "one");
    assert_eq!(fx.gs("b"), "two");
}

// ── F3 (5): `str.join` of a str subclass ─────────────────────────────
// `''.join([S('x'), 'y'])` raised "join expects a list of
// str/bytes objects consistent with self object" (`joinItem` is
// `nativeBase` on full).

#[test]
fn f3_join_str_subclass() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class S(str): pass
a = ''.join([S('x'), 'y'])
b = '-'.join([S('a'), S('b')])
"#,
    );
    assert_eq!(fx.gs("a"), "xy");
    assert_eq!(fx.gs("b"), "a-b");
}

// ── F3 (6): brace format — int subclass + `__format__` ───────────────
// `'{:03d}'.format(R(7))` raised "bad format string" (the spec
// step was an identity stub); `f'{Q():z}'` / `format(Q(), 'y')`
// printed the instance fallback (`<Q object at N>`).

#[test]
fn f3_format_subclass_and_dunder() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class R(int): pass
class Q:
    def __format__(self, spec): return 'Q' + spec
a = '{:03d}'.format(R(7))
b = f'{Q():z}'
c = format(Q(), 'y')
d = '{:>5}'.format(R(7))
e = format(7, '03d')
"#,
    );
    assert_eq!(fx.gs("a"), "007");
    assert_eq!(fx.gs("b"), "Qz");
    assert_eq!(fx.gs("c"), "Qy");
    assert_eq!(fx.gs("d"), "    7");
    assert_eq!(fx.gs("e"), "007");
}

// ── F3 (6b): brace format — IntEnum / StrEnum ────────────────────────
// `f'{N.TWO:03d}'` is `'002'` (via `IntEnum.__format__` →
/// `format(self._value_, spec)`); `f'{Code.KERNEL}'` is
// `Code.KERNEL` (via `Enum.__str__` through the conversion
// print). Both printed the instance fallback on base.

#[test]
fn f3_format_enum_members() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
from enum import Enum, IntEnum, auto
class N(IntEnum):
    ONE = 1
    TWO = auto()
class Code(str, Enum):
    KERNEL = 'kernel_error'
a = f'{N.TWO:03d}'
b = f'{Code.KERNEL}'
c = N.ONE < N.TWO
d = N.TWO + 1
"#,
    );
    assert_eq!(fx.gs("a"), "002");
    assert_eq!(fx.gs("b"), "Code.KERNEL");
    assert!(fx.gb("c"));
    assert_eq!(fx.gi("d"), 3);
}

// ── F3 (7): `%`-format of float / int subclasses ─────────────────────
// `'%d' % F(3.9)` raised "can't convert to int" (the
// float-subclass instance never reached `arg_as_int`).

#[test]
fn f3_percent_subclass() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class R(int): pass
class F(float): pass
a = '%d' % F(3.9)
b = '%x' % R(255)
c = '%d' % R(7)
d = '%f' % F(1.5)
e = '%c' % R(65)
"#,
    );
    assert_eq!(fx.gs("a"), "3");
    assert_eq!(fx.gs("b"), "ff");
    assert_eq!(fx.gs("c"), "7");
    assert_eq!(fx.gs("d"), "1.500000");
    assert_eq!(fx.gs("e"), "A");
}

// ── F3 (8): `float()` of a float subclass ────────────────────────────
// `float(F(2.5))` raised "can't convert to float" (the FLOAT_MAYBE
// slot answers NULL for the plain float; the Zig's
// `mp_obj_get_float_maybe` falls through to `nativeBase` —
// `obj.zig:676-684` — a step the Rust `obj_get_float_maybe` had
// marked deferred). `float(R(2))` still raises on both (an int
// base is not a float), as does `int(F(2.5))` (no `nativeBase`
// step on the int path).

#[test]
fn f3_float_of_float_subclass() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class F(float): pass
a = float(F(2.5))
"#,
    );
    assert_eq!(fx.gf("a"), 2.5);
    assert_eq!(
        fx.raised_msg("class R(int): pass\nfloat(R(2))\n"),
        (
            "TypeError".to_string(),
            "can't convert to float".to_string()
        )
    );
}
