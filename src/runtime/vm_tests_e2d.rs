//! Lane E2d regression tests: user-class `__lt__` sorting
//! (via the VM hook), container comparison (`__lt__` /
//! `__eq__` through the hook), and container repr dispatching
//! user `__repr__`.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs` and `vm_tests_e2c.rs`).

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

const A_CLASS: &str = "class A:\n def __init__(self, v):\n  self.v = v\n def __lt__(self, other):\n  return self.v < other.v\n def __eq__(self, other):\n  return self.v == other.v\n def __repr__(self):\n  return \"A%d\" % self.v\n";

// ── E2d item 1: user-class `__lt__` sorting ──────────────────────────
// `sort_less` used the ctx-only `obj_binary_op` (stub seams),
// so a FunBc `__lt__` never ran and `list.sort()` raised
// `TypeError: unsupported types for __lt__: '', ''`. The sort
// now runs through the VM's `VmHook::binary_op`.

#[test]
fn e2d_sort_user_lt() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_CLASS}l = [A(5), A(2), A(8), A(1)]\nl.sort()\nassert repr(l) == '[A1, A2, A5, A8]'\nassert repr(sorted([A(3), A(1)])) == '[A1, A3]'\nl2 = [A(1), A(2), A(3)]\nl2.sort(reverse=True)\nassert repr(l2) == '[A3, A2, A1]'\n"
    ));
}

#[test]
fn e2d_sort_user_lt_key() {
    // `sort(key=)` + user `__lt__` together (the key path
    // already worked; only the repr failed before E2d).
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_CLASS}l = [A(1), A(2), A(3)]\nl.sort(key=lambda a: -a.v)\nassert repr(l) == '[A3, A2, A1]'\nassert repr(sorted([A(1), A(3)], key=lambda a: -a.v)) == '[A3, A1]'\n"
    ));
}

#[test]
fn e2d_sort_raises_value_error() {
    // A raising comparison surfaces the REAL exception (not a
    // synthesized TypeError) and the sort runs to completion
    // first (the list is left in its partially-sorted state:
    // the quicksort swaps the pivot into place before the
    // caller raises).
    let mut fx = Fx::new();
    fx.run_ok(
        "class B:\n def __init__(self, v):\n  self.v = v\n def __lt__(self, other):\n  raise ValueError('boom')\n def __repr__(self):\n  return 'B%d' % self.v\nl = [B(1), B(2)]\ntry:\n l.sort()\n assert False\nexcept ValueError as e:\n assert str(e) == 'boom'\nassert repr(l) == '[B2, B1]'\n",
    );
}

#[test]
fn e2d_sort_notimplemented_names() {
    // `__lt__` returning NotImplemented: no reflected fallback
    // exists for comparisons (the vendor has no REVERSE_LESS),
    // so the TypeError names the class on BOTH sides.
    let mut fx = Fx::new();
    fx.run_ok(
        "class B:\n def __init__(self, v):\n  self.v = v\n def __lt__(self, other):\n  return NotImplemented\n def __gt__(self, other):\n  return self.v > other.v\nl = [B(2), B(1)]\ntry:\n l.sort()\n assert False\nexcept TypeError as e:\n assert str(e) == \"unsupported types for __lt__: 'B', 'B'\"\n",
    );
}

// ── E2d item 3: container comparison ────────────────────────────────
// `seq_cmp_objs` used the ctx-only `obj_binary_op` /
// `obj_equal` (stub seams), so a FunBc `__lt__` / `__eq__`
// never ran inside a list/tuple comparison.

#[test]
fn e2d_container_order_eq() {
    // Ordering needs BOTH `__lt__` and `__gt__`: the vendor
    // swaps Less→More (`seq_cmp_objs`), so an `__lt__`-only
    // class raises `TypeError ... for __gt__` — pinned below.
    // Equality needs only `__eq__`.
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_CLASS}class G:\n def __init__(self, v):\n  self.v = v\n def __lt__(self, other):\n  return self.v < other.v\n def __gt__(self, other):\n  return self.v > other.v\n def __eq__(self, other):\n  return self.v == other.v\n def __repr__(self):\n  return \"G%d\" % self.v\nassert (G(1),) < (G(2),)\nassert not (G(2),) < (G(1),)\nassert [G(1), G(2)] < [G(1), G(3)]\nassert (G(1), G(2)) <= (G(1), G(2))\nassert [G(1)] >= [G(1)]\nassert (A(1),) == (A(1),)\nassert [A(1), A(2)] == [A(1), A(2)]\nassert [A(1)] != [A(2)]\nassert [[A(1)]] == [[A(1)]]\nassert (A(1), A(2)) != (A(1),)\ntry:\n (A(1),) < (A(2),)\n assert False\nexcept TypeError as e:\n assert str(e) == \"unsupported types for __gt__: 'A', 'A'\"\n"
    ));
}

#[test]
fn e2d_max_type_error_names() {
    // `max` uses `__gt__`; a class with only `__lt__` raises
    // with the class name on both sides (was `''`, `''`).
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n def __init__(self, v):\n  self.v = v\n def __lt__(self, other):\n  return self.v < other.v\ntry:\n max([A(3), A(1)])\n assert False\nexcept TypeError as e:\n assert str(e) == \"unsupported types for __gt__: 'A', 'A'\"\n",
    );
}

// ── E2d item 2: container repr ──────────────────────────────────────
// `list_print` / `tuple_print` / `dict_print` / `set_print`
// render elements through the ctx-only `obj_print_helper`
// (stub seams), so an instance inside a container fell back
// to `<A object at …>` while top-level `repr(A())` worked.

#[test]
fn e2d_container_repr() {
    // `B` has no `__eq__` on purpose: defining `__eq__`
    // makes instances unhashable (like CPython), so `A`
    // cannot be a dict key or a set element — `B` covers
    // those two lines.
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_CLASS}class B:\n def __init__(self, v):\n  self.v = v\n def __repr__(self):\n  return \"B%d\" % self.v\nassert repr([A(7)]) == '[A7]'\nassert repr([[A(7)]]) == '[[A7]]'\nassert repr((A(7),)) == '(A7,)'\nassert str([A(1)]) == '[A1]'\nassert repr([A(1)]) == '[A1]'\nassert str((A(1),)) == '(A1,)'\nassert repr([A(1), [A(2), (A(3),)]]) == '[A1, [A2, (A3,)]]'\nassert repr({{B(1): 10}}) == '{{B1: 10}}'\nassert repr({{'k': A(7)}}) == \"{{'k': A7}}\"\nassert repr({{B(1)}}) == '{{B1}}'\nassert repr((A(1), A(2))) == '(A1, A2)'\n"
    ));
}

#[test]
fn e2d_container_repr_namedtuple() {
    // A namedtuple is an `Obj::Tuple` headed by its class:
    // it must NOT take the container branch (which would
    // render `"()"`) — its repr comes from the class's own
    // print slot, nested or not.
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections import namedtuple\nE = namedtuple('TupEmpty', [])\nassert repr(E()) == 'TupEmpty()'\nassert repr([E()]) == '[TupEmpty()]'\n",
    );
}

#[test]
fn e2d_container_repr_raising_fallback() {
    // A raising `__repr__` inside a container: the reference
    // ABORTS (rc=134, null-pointer panic); Rust keeps the
    // print slot's swallow-to-fallback (documented
    // divergence) and continues. The address varies (arena
    // slot), so only the shape is pinned.
    let mut fx = Fx::new();
    fx.run_ok(
        "class R:\n def __repr__(self):\n  raise ValueError('boom')\ns = repr([R()])\nassert s.startswith('[<R object at ')\nassert s.endswith('>]')\n",
    );
}
