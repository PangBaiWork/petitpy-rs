//! Lane E2j regression tests: EQUAL / NOT_EQUAL 2-pass protocol
//! for instance operands — the array1 lines (`X('b') == b'ab'`
//! / `!=`) and the L/S/D family (`L([1,2]) == L([1,2])`,
//! `S('ab') == S('ab')`, `D(a=1) == D(a=1)`).
//!
//! Root cause (E2i §4(a)): the vendor's SENTINEL branch calls
//! the FULL `mp_binary_op` (objtype.zig:1167-1168), and for
//! EQUAL/NOT_EQUAL that is `mp_obj_equal_not_equal` — the
//! 2-pass loop (obj.zig:268-368). The Rust SENTINEL arm called
//! only the lhs type's slot (`seams.binary`), so `X == b'ab'`
//! came back True (the array slot compares contents) where the
//! vendor returns False (both passes skip — array/bytes flags
//! are 0 — and the pointers differ), and `L == L` came back
//! False (the list slot refuses a non-list rhs) where the
//! vendor returns True (pass 1 reaches the instance slot,
//! which delegates to subobj[0]). The fix routes EQUAL /
//! NOT_EQUAL in the SENTINEL arm through
//! `obj_equal_not_equal`, with the seam's `call` wrapped in
//! `SeamHook` so a user `__eq__` on the rhs still runs.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs`, `vm_tests_e2f.rs`,
//! `vm_tests_e2g.rs`, `vm_tests_e2h.rs` and `vm_tests_e2i.rs`).

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

// ── E2j (1): array subclass vs bytes (the array1 lines) ─────────────
// `X('b',[97,98]) == b'ab'` is False on the reference (both
// 2-pass attempts skip — array/bytes flags are 0 — and the
// pointers differ); the slot-level path returned True because
// the array slot compares contents.

#[test]
fn e2j_array_subclass_vs_bytes() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import array
class X(array.array):
    pass
eq = X('b', [97, 98]) == b'ab'
ne = X('b', [97, 98]) != b'ab'
"#,
    );
    assert!(!fx.gb("eq"));
    assert!(fx.gb("ne"));
}

// ── E2j (2): array subclass vs array ────────────────────────────────
// Same-typecode contents compare True (already passed before
// the fix — the no-regression guard for the `==` side).

#[test]
fn e2j_array_subclass_vs_array() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import array
class X(array.array):
    pass
eq = X('b', [97, 98]) == array.array('b', [97, 98])
ne = X('b', [97, 98]) != array.array('b', [97, 98])
eq2 = X('b', [97, 98]) == array.array('b', [97, 99])
"#,
    );
    assert!(fx.gb("eq"));
    assert!(!fx.gb("ne"));
    assert!(!fx.gb("eq2"));
}

// ── E2j (3): list subclass vs list ──────────────────────────────────
// `L([1,2]) == L([1,2])` is True on the reference (pass 1
// reaches the instance slot, which delegates to subobj[0]);
// the slot-level path returned False (the list slot refuses
// a non-list rhs).

#[test]
fn e2j_list_subclass_vs_list() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class L(list):
    pass
eq = L([1, 2]) == L([1, 2])
ne = L([1, 2]) != L([1, 2])
eq_plain = L([1, 2]) == [1, 2]
ne_plain = L([1, 2]) != [1, 2]
eq_diff = L([1, 2]) == L([1, 3])
"#,
    );
    assert!(fx.gb("eq"));
    assert!(!fx.gb("ne"));
    assert!(fx.gb("eq_plain"));
    assert!(!fx.gb("ne_plain"));
    assert!(!fx.gb("eq_diff"));
}

// ── E2j (4): str subclass vs str ────────────────────────────────────

#[test]
fn e2j_str_subclass_vs_str() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class S(str):
    pass
eq = S('ab') == S('ab')
ne = S('ab') != S('ab')
eq_plain = S('ab') == 'ab'
eq_diff = S('ab') == S('ac')
"#,
    );
    assert!(fx.gb("eq"));
    assert!(!fx.gb("ne"));
    assert!(fx.gb("eq_plain"));
    assert!(!fx.gb("eq_diff"));
}

// ── E2j (5): dict subclass vs dict ──────────────────────────────────

#[test]
fn e2j_dict_subclass_vs_dict() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class D(dict):
    pass
eq = D(a=1) == D(a=1)
ne = D(a=1) != D(a=1)
eq_plain = D(a=1) == {'a': 1}
eq_diff = D(a=1) == D(a=2)
"#,
    );
    assert!(fx.gb("eq"));
    assert!(!fx.gb("ne"));
    assert!(fx.gb("eq_plain"));
    assert!(!fx.gb("eq_diff"));
}

// ── E2j (6): bytes subclass vs bytes (no-regression guard) ──────────

#[test]
fn e2j_bytes_subclass_vs_bytes() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class B(bytes):
    pass
eq = B(b'ab') == b'ab'
eq2 = B(b'ab') == B(b'ab')
ne = B(b'ab') != B(b'ab')
"#,
    );
    assert!(fx.gb("eq"));
    assert!(fx.gb("eq2"));
    assert!(!fx.gb("ne"));
}

// ── E2j (7): a user `__eq__` on the rhs still runs ──────────────────
// The inner protocol must carry the hook (the seam's `call`
// wrapped in `SeamHook`): with a bare `None` hook the inner
// loop's instance pass would run the stub slot, `L2.__eq__`
// would raise instead of running, and the inner protocol
// would answer a definitive False before the outer loop ever
// tried the rhs's type. `L3.__eq__` returns NotImplemented,
// which the protocol treats as "not implemented" (False).

#[test]
fn e2j_user_eq_on_rhs_runs() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class L1(list):
    pass
class L2(list):
    def __eq__(self, o):
        return True
class L3(list):
    def __eq__(self, o):
        return NotImplemented
eq = L1([1, 2]) == L2([9, 9])
eq_ni = L1([1, 2]) == L3([1, 2])
eq_lhs = L2([9, 9]) == L1([1, 2])
"#,
    );
    assert!(fx.gb("eq"));
    assert!(!fx.gb("eq_ni"));
    assert!(fx.gb("eq_lhs"));
}

// ── E2j (8): `__ne__` first for NOT_EQUAL ───────────────────────────
// The vendor tries `__ne__` first when the type has a neq test
// (the CPython asymmetry in the 2-pass loop).

#[test]
fn e2j_ne_with_user_neq() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class N(list):
    def __ne__(self, o):
        return False
    def __eq__(self, o):
        return True
class M(list):
    pass
ne = N([1]) != [1]
eq = N([1]) == [1]
ne_diff = M([1]) != M([2])
ne_same = M([1]) != M([1])
ne_plain = M([1]) != [1]
"#,
    );
    assert!(!fx.gb("ne"));
    assert!(fx.gb("eq"));
    assert!(fx.gb("ne_diff"));
    assert!(!fx.gb("ne_same"));
    assert!(!fx.gb("ne_plain"));
}

// ── E2j (9): ordering / containment still take the old path ─────────
// Only EQUAL/NOT_EQUAL route through the full protocol; `<` and
// `in` keep the slot-level seam (the E2i `<` behaviour).

#[test]
fn e2j_ordering_containment_unchanged() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class L(list):
    pass
class S(str):
    pass
lt = L([1, 2]) < [1, 3]
lt2 = S('ab') < 'ac'
contains = 1 in L([1, 2])
contains_s = 'a' in S('ab')
"#,
    );
    assert!(fx.gb("lt"));
    assert!(fx.gb("lt2"));
    assert!(fx.gb("contains"));
    assert!(fx.gb("contains_s"));
}
