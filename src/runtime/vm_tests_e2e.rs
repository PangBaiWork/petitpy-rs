//! Lane E2e regression tests: bytes/bytearray buffer operands,
//! bytearray str methods, `str.format` kwargs with dynamic names,
//! and `%`-format `__int__` dispatch.
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

// ── E2e (item 3): `%`-format `__int__` dispatch ────────────────────────
// The slot's stub seams cannot run a FunBc `__int__`, so `"%d" %
// A()` raised "can't convert to int"; the VM's MODULO branch now
// threads its hook through `str_modulo_format_hook`.

#[test]
fn e2e_modulo_int_dispatch() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class A:\n    def __int__(self):\n        return 123\nassert '%d' % A() == '123'\nassert '%o' % A() == '173'\nassert '%x' % A() == '7b'\nassert '%5d' % A() == '  123'\nclass B:\n    def __int__(self):\n        return 5\nassert '%*d' % (B(), 42) == '   42'\nassert '%.*f' % (B(), 3.14159) == '3.14159'\n",
    );
}

#[test]
fn e2e_modulo_int_bad_return() {
    let mut fx = Fx::new();
    // A `__int__` returning a non-int is the vendor's BARE
    // `TypeError` (no message, empty args) — probed on the
    // reference.
    fx.run_ok(
        "class A:\n    def __int__(self):\n        return 'x'\ntry:\n    '%d' % A()\nexcept TypeError as e:\n    assert e.args == ()\nelse:\n    assert False\n",
    );
}

#[test]
fn e2e_modulo_int_raises() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg(
        "class A:\n    def __int__(self):\n        raise ValueError('boom')\n'%d' % A()\n",
    );
    assert_eq!(name, "ValueError");
    assert_eq!(msg, "boom");
}

// ── E2e (item 3): `str.format` kwargs end to end ───────────────────────

#[test]
fn e2e_format_kwargs() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert '{foo}'.format(foo='bar') == 'bar'\nassert '{text:{align}{width}}'.format(text='foo', align='<', width=20) == 'foo                 '\nassert '{:{}{}{}}'.format(123, '#', '>', '10') == '#######123'\n",
    );
}

// ── E2e (items 1, 2, 4): bytes/bytearray end to end ────────────────────

#[test]
fn e2e_bytes_array_ops() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import array\nassert b'ab' + array.array('b', [99, 100]) == b'abcd'\nassert b'ab' + bytearray(b'cd') == b'abcd'\nassert bytes(array.array('b', [97, 98])) == b'ab'\nassert bytes(bytearray(b'ab')) == b'ab'\nassert bytes(iter([128, 255])) == bytes([128, 255])\nassert array.array('b', [98]) in b'abc'\nassert b'ab' < bytearray(b'ac')\nassert bytearray(b'ab') + b'cd' == bytearray(b'abcd')\n",
    );
}

#[test]
fn e2e_bytearray_methods() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert bytearray(b'hello world').find(b'll') == 2\nassert bytearray(b'ab').center(6) == bytearray(b'  ab  ')\nassert bytearray(b'abab').count(b'ab') == 2\nassert bytearray(b'hello').decode() == 'hello'\nassert bytearray(b'aXb').partition(b'X') == (bytearray(b'a'), bytearray(b'X'), bytearray(b'b'))\nassert bytearray.fromhex('ff') == bytearray(b'\\xff')\nassert bytearray(b'a\\nb').splitlines() == [bytearray(b'a'), bytearray(b'b')]\nassert bytearray(b'{}').format(1) == bytearray(b'1')\n",
    );
}

#[test]
fn e2e_bytes_join_bytearray() {
    let mut fx = Fx::new();
    fx.run_ok(
        "assert b'-'.join([bytearray(b'a'), b'b']) == b'a-b'\nassert bytearray(b',').join([b'a', b'b']) == bytearray(b'a,b')\n",
    );
}

#[test]
fn e2e_bytearray_str_arg() {
    let mut fx = Fx::new();
    // `bytearray('1234')` (str without encoding) is a TypeError
    // on the reference; Rust used to accept it.
    let (name, _) = fx.raised_msg("bytearray('1234')\n");
    assert_eq!(name, "TypeError");
}
