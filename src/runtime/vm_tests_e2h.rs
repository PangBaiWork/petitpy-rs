//! Lane E2h regression tests: int / hash / bytes conversions.
//!
//! * `int_big_rshift` panic (mpz `shr_inpl` round-up path);
//! * `int_to_bytes` sign-bit check (positive heap ints);
//! * `int()` from bytearray / memoryview (buffer protocol);
//! * `memoryview.hex()` (the missing locals-dict entry);
//! * `hash()` / dict-store of user-class instances through the
//!   VM hook (`__hash__` member, truncation, the identity /
//!   `__eq__` rule, unhashable);
//! * `int()` of user-class instances through the VM hook
//!   (`__int__` member, big-int passthrough).
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs` and `vm_tests_e2f.rs`).

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

// ── Fix 1: `int_big_rshift` panic ────────────────────────────────────
// `mpn::shr`'s round-up path in `shr_inpl` truncated the spare
// digit the vendor's `mpz_need_dig` provides, so the carry write
// in the `+ 1` step panicked (`index out of bounds` at
// `mpn.rs:142`) for a shifted value of all 1s.

#[test]
fn e2h_int_big_rshift_no_panic() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert (-0xffffffffffffffff) >> 32 == -4294967296\nassert (-1) >> 100 == -1\nassert (1 << 100) >> 99 == 2\nassert (-(1 << 100)) >> 99 == -2\n",
    );
}

// ── Fix 2: `int_to_bytes` positive top-bit ───────────────────────────
// The sign-bit overflow check applies only to negative ints
// (the vendor passes `as_signed = mpz.neg`); a positive heap
// int with the top bit set no longer raises `OverflowError`.

#[test]
fn e2h_int_to_bytes_positive_top_bit() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert (2**64 - 1).to_bytes(8, 'little') == b'\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff'\nassert (2**64 - 1).to_bytes(8, 'big') == b'\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff'\nassert (255).to_bytes(1, 'little') == b'\\xff'\nassert (-1).to_bytes(1, 'little') == b'\\xff'\ntry:\n (2**64 - 1).to_bytes(7, 'little')\n assert False\nexcept OverflowError:\n pass\ntry:\n (-129).to_bytes(1, 'little')\n assert False\nexcept OverflowError:\n pass\n",
    );
}

// ── Fix 3: `int()` from bytearray / memoryview ───────────────────────
// `int_make_new_1` only tried `str_bytes_of`; the vendor tries
// `mp_get_buffer` after INT_MAYBE, so any buffer provider parses.

#[test]
fn e2h_int_parse_buffer() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert int(bytearray(b'123')) == 123\nassert int(memoryview(b'123')) == 123\nassert int(memoryview(bytearray(b'-42'))) == -42\nassert int(b'123') == 123\n",
    );
}

// ── Fix 4: `memoryview.hex()` ────────────────────────────────────────
// The memoryview type dict was empty; the vendor's
// `mp_obj_memoryview_locals_dict` is just the `hex` entry.

#[test]
fn e2h_memoryview_hex() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert memoryview(b'ab').hex() == '6162'\nassert memoryview(b'').hex() == b''\nassert memoryview(bytearray(b'\\xff')).hex() == 'ff'\n",
    );
}

// ── Fix 5: `hash()` / dict store through the VM hook ─────────────────

const HASH_CLASSES: &str = "class A:\n def __hash__(self):\n  return 123\n def __repr__(self):\n  return 'a instance'\nclass B:\n pass\nclass C:\n def __eq__(self, another):\n  return True\nclass D:\n def __hash__(self):\n  return None\nclass E:\n def __hash__(self):\n  return True\nclass F:\n def __hash__(self):\n  return (1 << 70) | 1\n";

#[test]
fn e2h_hash_user_class() {
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "{HASH_CLASSES}assert hash(A()) == 123\nassert repr({{A(): 1}}) == \"{{a instance: 1}}\"\nhash(B())\ntry:\n hash(C())\n assert False\nexcept TypeError as e:\n assert str(e) == 'unsupported type for operator'\ntry:\n hash(D())\n assert False\nexcept TypeError as e:\n assert str(e) == \"can't convert to int\"\nassert hash(E()) == 1\nassert hash(F()) == 1\n"
    ));
}

#[test]
fn e2h_hash_raising_propagates() {
    // The vendor's `mp_unary_op` HASH rule: a raising `__hash__`
    // is SWALLOWED — `hash()` and a dict-literal store both
    // report `TypeError("unsupported type for operator")`, and
    // the stashed raise must not leak into a later operation
    // (the `sorted` call below would surface a leaked
    // `pending_exc` through its own drain).
    let mut fx = Fx::new();
    fx.run_ok(
        "class R:\n def __hash__(self):\n  raise ValueError('boom')\ntry:\n hash(R())\n assert False\nexcept TypeError as e:\n assert str(e) == 'unsupported type for operator'\ntry:\n {R(): 1}\n assert False\nexcept TypeError as e:\n assert str(e) == 'unsupported type for operator'\nassert sorted([3, 1, 2], key=lambda x: -x) == [3, 2, 1]\n",
    );
}

#[test]
fn e2h_dict_store_instance_keys_growth() {
    // Twenty instance keys force several `compact_rehash`
    // replays; each replay re-hashes through the hook, so the
    // entries must survive growth. (Built with a dict
    // comprehension — STORE_COMP — and read back through
    // `len` / the values view: subscript stores and lookups
    // with an instance key are still ctx-only, out of scope.)
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n def __init__(self, v):\n  self.v = v\n def __hash__(self):\n  return self.v\n def __repr__(self):\n  return 'A%d' % self.v\nks = [A(i) for i in range(20)]\nd = {k: k.v * 10 for k in ks}\nassert len(d) == 20\nassert len(list(d.keys())) == 20\nassert list(d.values()) == [i * 10 for i in range(20)]\n",
    );
}

// ── Fix 5 (cont.): `int()` through the VM hook ───────────────────────
// `int_make_new_slot` dropped the `MakeNewFn` hook, so the
// vendor's INT_MAYBE step never ran for a user-class instance.

#[test]
fn e2h_int_user_class() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class Cud:\n def __int__(self):\n  return 42\nassert int(Cud()) == 42\nclass Big:\n def __int__(self):\n  return 1 << 100\nassert int(Big()) == 1 << 100\nclass BadInt:\n def __int__(self):\n  return None\ntry:\n int(BadInt())\n assert False\nexcept TypeError as e:\n assert str(e) == \"can't convert to int\"\nclass NoInt:\n pass\ntry:\n int(NoInt())\n assert False\nexcept TypeError as e:\n assert str(e) == \"can't convert to int\"\n",
    );
}

#[test]
fn e2h_int_user_class_raising_propagates() {
    // The vendor's INT_MAYBE NULL rule: a raising `__int__`
    // reports `TypeError("can't convert to int")`, the same as
    // a non-int return — the stashed raise is discarded.
    let mut fx = Fx::new();
    fx.run_ok(
        "class R:\n def __int__(self):\n  raise ValueError('boom')\ntry:\n int(R())\n assert False\nexcept TypeError as e:\n assert str(e) == \"can't convert to int\"\n",
    );
}
