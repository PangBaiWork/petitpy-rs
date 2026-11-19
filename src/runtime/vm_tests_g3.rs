//! Lane G3 regression tests: container hook family (H2–H6).
//!
//! Vendor hooks (all read from the original's Zig / probed on the
//! reference binary 2026-11-19):
//! * H2 (`map.zig` `compactLookup` / `compactRehash`): the probe
//!   comparison is `mp_obj_equal` — a FunBc `__eq__` on the key
//!   runs. The Rust probe was ctx-only (`obj_equal`), so
//!   `d[A()]` raised `KeyError` even though the hash half (F3)
//!   worked. The rehash replay replays through the same probe.
//! * H3 (`objdict.zig` `dictGetHelperImpl`): `.get` / `.pop` /
//!   `.setdefault` share the map lookup — same probe gap, via
//!   the method table (intercepted by GcRef identity, the
//!   `list.sort` / `str.join` precedent).
//! * H4 (`objtype.zig` `instance_subscr` SENTINEL): a dict
//!   subclass instance subscripts through the subobj's dict
//!   slot — the hook has to reach the stub subscr seam.
//! * H5 (`map.zig` `mp_set_lookup`): the set probe is
//!   `mp_obj_equal` too, and the unhashable-insert quirk
//!   (`s.add([2])` raises AND `s == {[2]}`) must survive.
//! * H6 (`objlist.zig` `npy_lst_extend`): `extend` iterates
//!   via `mp_getiter` / `mp_iternext` — a generator arg needs
//!   the VM's real call hook (`collect_iterable`).
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied (same as
//! `vm_tests_f6.rs`).

use crate::objects::containers::objdict::dict_get_value;
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

    fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not an int: {other:?}"),
        }
    }

    /// The exception's type name (`"ValueError"`, …).
    fn exc_name(&mut self, exc: Value) -> String {
        use crate::objects::obj::obj_get_type;
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
        use crate::objects::objtype::objexcept::exception_get_value;
        let v = exception_get_value(&self.vm.arena, exc);
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("exception value is not a str: {other:?}"),
            },
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}

/// The H2–H5 key type: constant hash, type-based equality.
const A_DEF: &str = "class A:\n    def __hash__(self):\n        return 7\n    def __eq__(self, o):\n        return isinstance(o, A)\n";

// ── H2: dict probe runs FunBc `__eq__` ─────────────────────────────

#[test]
fn g3_h2_dict_subscr_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}d = {{}}\nd[A()] = 'x'\nok = (d[A()] == 'x')\n"
    ));
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h2_dict_subscr_after_rehash() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}d = {{}}\nd[A()] = 'x'\nfor i in range(30):\n    d[i] = i\nok = (d[A()] == 'x')\nn = len(d)\n"
    ));
    assert!(fx.gb("ok"));
    assert_eq!(fx.gi("n"), 31);
}

#[test]
fn g3_h2_dict_del_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}d = {{}}\nd[A()] = 'x'\ndel d[A()]\nok = (len(d) == 0)\n"
    ));
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h2_dict_eq_truthy_int_counts() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n    def __hash__(self):\n        return 7\n    def __eq__(self, o):\n        return 1\nd = {}\nd[A()] = 'x'\nok = (d[A()] == 'x')\n",
    );
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h2_dict_eq_raise_reads_not_equal() {
    // Vendor `mp_obj_is_true` pending guard (obj.zig:470): a
    // raise inside the probe comparison reads as not-equal —
    // the load reports `KeyError`, the `ValueError` swallowed.
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n    def __hash__(self):\n        return 7\n    def __eq__(self, o):\n        raise ValueError('eq boom')\nd = {}\nd[A()] = 'x'\nok = False\ntry:\n    x = d[A()]\nexcept KeyError:\n    ok = True\n",
    );
    assert!(fx.gb("ok"));
}

// ── H3: dict.get / .pop / .setdefault run FunBc `__eq__` ──────────

#[test]
fn g3_h3_dict_get_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}class B:\n    def __hash__(self):\n        return 7\n    def __eq__(self, o):\n        return isinstance(o, B)\nd = {{}}\nd[A()] = 'x'\nok1 = (d.get(A()) == 'x')\nok2 = (d.get(A(), 'dflt') == 'x')\nok3 = (d.get(B()) is None)\nok4 = (d.get(B(), 'dflt') == 'dflt')\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}

#[test]
fn g3_h3_dict_pop_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}d = {{}}\nd[A()] = 'x'\nv = d.pop(A())\nok1 = (v == 'x')\nok2 = (len(d) == 0)\nok3 = (d.pop(A(), 'dflt') == 'dflt')\nok4 = False\ntry:\n    d.pop(A())\nexcept KeyError:\n    ok4 = True\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}

#[test]
fn g3_h3_dict_setdefault_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}d = {{}}\nd[A()] = 'x'\nok1 = (d.setdefault(A()) == 'x')\nok2 = (len(d) == 1)\nd2 = {{}}\nok3 = (d2.setdefault(A(), 'y') == 'y')\nok4 = (d2[A()] == 'y')\nok5 = (d2.setdefault(A()) == 'y')\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
    assert!(fx.gb("ok5"));
}

// ── H6: list.extend / `+=` collect a generator ─────────────────────

#[test]
fn g3_h6_list_extend_genexpr() {
    let mut fx = Fx::new();
    fx.run_ok("l = []\nl.extend(x for x in [1, 2, 3])\nok = (l == [1, 2, 3])\n");
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h6_list_extend_genfn() {
    let mut fx = Fx::new();
    fx.run_ok(
        "def gen():\n    yield 1\n    yield 2\nl = [0]\nl.extend(gen())\nok = (l == [0, 1, 2])\n",
    );
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h6_list_iadd_genexpr() {
    let mut fx = Fx::new();
    fx.run_ok("l = [0]\nl += (x for x in [1, 2])\nok = (l == [0, 1, 2])\n");
    assert!(fx.gb("ok"));
}

#[test]
fn g3_h6_list_extend_list_unchanged() {
    // The plain-list fast path still works (no hook needed).
    let mut fx = Fx::new();
    fx.run_ok("l = [1]\nl.extend([2, 3])\nok1 = (l == [1, 2, 3])\nl += [4]\nok2 = (l == [1, 2, 3, 4])\n");
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
}

// ── H5: set probe runs FunBc `__eq__` ──────────────────────────────

#[test]
fn g3_h5_set_literal_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}s = {{A()}}\nok1 = (A() in s)\nok2 = (len(s) == 1)\ns2 = {{A(), A()}}\nok3 = (len(s2) == 1)\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
}

#[test]
fn g3_h5_set_methods_instance_key() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}s = set()\ns.add(A())\ns.add(A())\nok1 = (len(s) == 1)\ns.discard(A())\nok2 = (len(s) == 0)\ns.add(A())\ns.remove(A())\nok3 = (len(s) == 0)\nok4 = False\ntry:\n    s.remove(A())\nexcept KeyError:\n    ok4 = True\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}

#[test]
fn g3_h5_set_ctor_instance_keys() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}s = set([A(), A()])\nok1 = (len(s) == 1)\ns2 = set(A() for _ in range(2))\nok2 = (len(s2) == 1)\ns3 = {{A() for _ in range(2)}}\nok3 = (len(s3) == 1)\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
}

#[test]
fn g3_h5_set_add_unhashable_quirk() {
    // The quirk must survive the hook threading: `s.add([2])`
    // raises TypeError AND the element is still in the set
    // (observable via len / iteration — `s == {[2]}` itself
    // raises TypeError on the reference too, since the
    // comparison re-hashes the element).
    let mut fx = Fx::new();
    fx.run_ok(
        "s = set()\nok1 = False\ntry:\n    s.add([2])\nexcept TypeError:\n    ok1 = True\nok2 = (len(s) == 1)\nok3 = (list(s) == [[2]])\n",
    );
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
}

#[test]
fn g3_h5_dict_in_instance_key() {
    // The dict-`in` gap adjacent to H2 (the Contains branch).
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{A_DEF}class B:\n    def __hash__(self):\n        return 7\n    def __eq__(self, o):\n        return isinstance(o, B)\nd = {{}}\nd[A()] = 'x'\nok1 = (A() in d)\nok2 = (B() not in d)\n"
    ));
    assert!(fx.gb("ok1"));
    assert!(fx.gb("ok2"));
}
