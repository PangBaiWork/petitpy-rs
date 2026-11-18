//! Lane E2i regression tests: array / import / misc singles —
//! the array-subclass buffer delegation, the array comparison
//! `NotImplementedError`, `array.extend` over a generator, the
//! getitem-iterator hook (`getitem.py` / `stopiteration.py`),
//! `print(file=)` over a user `write`, the list `MemoryError`
//! cap, the ordered-dict popitem/add tombstone, the `builtins`
//! override dict (`builtins.abs`, `builtins.__import__`), and
//! a class instance registered as a module in `sys.modules`.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied — that
//! module's `Fx` is private (same copy as `vm_tests_b.rs`,
//! `vm_tests_e2.rs`, `vm_tests_e2b.rs`, `vm_tests_e2c.rs`,
//! `vm_tests_e2d.rs`, `vm_tests_e2e.rs` and `vm_tests_e2f.rs`).

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

// ── E2i (1): array-subclass buffer ───────────────────────────────────
// `bytes(X('b', ...))` failed "object not iterable": `buffer_bytes`
// only matched `Obj::Array`/`Obj::Bytes`/`Obj::Str`, never an
// `Obj::Instance` whose `subobj[0]` is the array (the vendor's
// `instance_get_buffer` delegates the buffer slot to `subobj[0]`).

#[test]
fn e2i_array_subclass_bytes() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import array
class X(array.array):
    pass
ok = bytes(X('b', [0x61, 0x62, 0x63])) == b'abc'
ok2 = bytes(X('b', [97, 98])) == b'ab'
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

// ── E2i (2): array comparison `NotImplementedError` ──────────────────
// The vendor's `arrayBinaryOpImpl` comparison arm raises
// `NotImplementedError` for mismatched layouts / a signed `<`,
// but the Rust slot returned a bare `PyRaise` with nothing
// recorded, so `==` swallowed it to `False`, `<` became a
// `TypeError`, and `98 in bytearray(b'abc')` became `True`.

#[test]
fn e2i_array_cmp_mismatch_nie() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import array
try:
    array.array('b') == array.array('h')
    raised_eq = False
except NotImplementedError:
    raised_eq = True
try:
    array.array('b', [1]) < array.array('b', [2])
    raised_lt = False
except NotImplementedError:
    raised_lt = True
try:
    98 in bytearray(b'abc')
    raised_in = False
except NotImplementedError:
    raised_in = True
# ... while the non-raising paths keep their values.
eq_bytes = array.array('b', [97, 98]) == b'ab'
eq_same = array.array('b', [1]) == array.array('b', [1])
"#,
    );
    assert!(fx.gb("raised_eq"));
    assert!(fx.gb("raised_lt"));
    assert!(fx.gb("raised_in"));
    assert!(!fx.gb("eq_bytes"));
    assert!(fx.gb("eq_same"));
}

// ── E2i (3): `array.extend` over a generator ─────────────────────────
// `array_extend` hardcoded its hook to `None`, so
// `collect_iterable`'s Generator arm returned "object not
// iterable" (the constructor already worked — only the method
// failed). The `str.join` precedent: intercept by GcRef identity.

#[test]
fn e2i_array_extend_generator() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import array
a1 = array.array('I', [1, 3])
a1.extend(i for i in (4, 5))
ok = list(a1) == [1, 3, 4, 5]
b = bytearray(b'ab')
b.extend(x for x in b'cd')
ok2 = bytes(b) == b'abcd'
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

// ── E2i (4a): getitem iterator over a user instance ──────────────────
// `for x in A()` / `list(A())` failed "object isn't
// subscriptable": both getitem seams used the ctx-only
// `obj_subscr`, whose stub seams cannot run a FunBc
// `__getitem__`. The fix threads the VM hook through them
// (and drops the `StopIteration`/`IndexError` args, the
// vendor's `excIsStopOrIndex` rule).

#[test]
fn e2i_getitem_iter_user_class() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __getitem__(self, i):
        if i >= 3:
            raise IndexError(i)
        return i * 10
ok = list(A()) == [0, 10, 20]
class B:
    def __getitem__(self, i):
        if i == 0:
            raise StopIteration(42)
        return i
try:
    next(iter(B()))
    raised = False
except StopIteration as e:
    raised = e.args == ()
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("raised"));
}

// ── E2i (4b): `next(enumerate/map/zip(...))` over `__iter__` ─────────
// The ctx-only `getiter` resolved a FunBc `__iter__` through
// stub seams (unrecorded `PyRaise`) and fell through to a
// GETITEM iterator, which then failed with a bare `TypeError`.
// The fix resolves `__iter__` through the hook when one is
// available (`getiter_hook`).

#[test]
fn e2i_next_enumerate_map_zip_user_iter() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
class A:
    def __iter__(self):
        return self
    def __next__(self):
        raise StopIteration(42)
try:
    next(enumerate(A()))
    raised_e = False
except StopIteration as e:
    raised_e = e.args == (42,)
try:
    next(map(lambda x: x, A()))
    raised_m = False
except StopIteration as e:
    raised_m = e.args == (42,)
try:
    next(zip(A()))
    raised_z = False
except StopIteration as e:
    raised_z = e.args == (42,)
"#,
    );
    assert!(fx.gb("raised_e"));
    assert!(fx.gb("raised_m"));
    assert!(fx.gb("raised_z"));
}

// ── E2i (5): `print(file=)` over a user `write` ──────────────────────
// The ctx-only stream protocol (`get_stream_raise` +
// `iobase_write`) cannot call a FunBc `write` (stub seam →
// bare `TypeError`). The VM-intercepted print now renders
// each piece and writes it through a per-call hook when the
// file is an instance with a FunBc `write`.

#[test]
fn e2i_print_file_user_write() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
log = []
class MyIO(io.IOBase):
    def write(self, buf):
        log.append(bytes(buf))
        return len(buf)
print('test', file=MyIO())
ok = log == [b'test', b'\n']
sio = io.StringIO()
print('x', file=sio)
ok2 = sio.getvalue() == 'x\n'
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

// ── E2i (6): the list `MemoryError` cap ──────────────────────────────
// A list cannot hold more than 2,097,152 elements (16 MiB of
// 8-byte pointers — the vendor's `MAX_ALLOC` at the allocator
// level). Two halves: `exc_from_typed` mapped a slot
// `OutOfMemory` to a bare `TypeError` (empty raise name), and
// no explicit cap existed.

#[test]
fn e2i_list_repeat_memoryerror() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
ok = len([1] * 2097152) == 2097152
try:
    [1] * 2097153
    raised = False
except MemoryError:
    raised = True
try:
    [].extend(range(2097153))
    raised2 = False
except MemoryError:
    raised2 = True
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("raised"));
    assert!(fx.gb("raised2"));
}

#[test]
fn e2i_list_append_past_cap_memoryerror() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
l = [1] * 2097152
try:
    l.append(1)
    raised = False
except MemoryError:
    raised = True
"#,
    );
    assert!(fx.gb("raised"));
}

// ── E2i (7): OrderedDict popitem then add ────────────────────────────
// `dict_popitem` tombstones the last live slot, but the
// ordered ADD always appended at `entries.len()` while
// reporting `Added(used)` — after a popitem the value was
// stored into the tombstone and the key became unreachable
// (`d["xyz"] = 321` read back as `None` / `KeyError`). The
// vendor's ordered ADD writes at `tbl[used]`, overwriting
// the tail tombstone.

#[test]
fn e2i_ordereddict_popitem_then_add() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
from collections import OrderedDict
d = OrderedDict([(10, 20), ("b", 100), (1, 2)])
del d["b"]
d["abc"] = 123
popped = d.popitem()
d["xyz"] = 321
ok = popped == ('abc', 123)
ok2 = list(d.values()) == [20, 2, 321]
ok3 = d["xyz"] == 321
ok4 = d.popitem() == ('xyz', 321)
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}

// ── E2i (8a): the `builtins` override dict ───────────────────────────
// The `Builtins::override_dict` seams existed since C14/D1
// but the VM never persisted the dict, so every override was
// dropped at the end of its own `STORE_ATTR`. The VM now
// keeps `mp_module_builtins_override_dict` on `VmState`, the
// builtins dict is fixed (the ROM table), and `import
// builtins` exposes it as a module.

#[test]
fn e2i_builtins_override_abs() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import builtins
builtins.abs = lambda x: x + 1
ok = abs(1) == 2
# ... while the module attribute still reads the original.
ok2 = builtins.abs(1) == 1
ok3 = builtins.__name__ == 'builtins'
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
}

// ── E2i (8b): `builtins.__import__` ──────────────────────────────────
// IMPORT_NAME dispatches through the override dict (the
// vendor's `mp_import_name` checks it first), and a direct
// `__import__(...)` call resolves the replacement through
// `load_global`. The replacement runs through the VM's real
// call — the ctx-only `call_function_n_kw` cannot run a
// FunBc.

#[test]
fn e2i_builtins_override_import() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import builtins
calls = []
def custom_import(name, globals, locals, fromlist, level):
    calls.append((name, fromlist, level))
    class M:
        a = 1
        b = 2
    return M
builtins.__import__ = custom_import
__import__('A', None, None, None, 0)
import a
from a import a as a1
ok = calls == [('A', None, 0), ('a', None, 0), ('a', ('a',), 0)]
ok2 = a1 == 1
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
}

// ── E2i (9): a class instance registered as a module ─────────────────
// `import mod` never consulted `sys.modules` (only the
// internal `state.modules` registry), so an instance stored
// there raised `ModuleNotFoundError`. The import path now
// consults the live `sys.modules` dict and returns the value
// as-is, module or not.

#[test]
fn e2i_sys_modules_instance() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import sys
class A:
    def __init__(self, value):
        self.value = value
    def meth(self):
        return self.value
    def meth_with_arg(self, a):
        return [self.value, a]
sys.modules["mod"] = A(1)
import mod
ok = mod.meth() == 1
ok2 = mod.meth_with_arg(2) == [1, 2]
sys.modules["mod"] = A(3)
from mod import meth, meth_with_arg
ok3 = meth() == 3
ok4 = meth_with_arg(4) == [3, 4]
"#,
    );
    assert!(fx.gb("ok"));
    assert!(fx.gb("ok2"));
    assert!(fx.gb("ok3"));
    assert!(fx.gb("ok4"));
}
