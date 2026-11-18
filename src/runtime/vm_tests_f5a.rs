//! Lane F5a regression tests: typing generics (`runtime_annotations`).
//!
//! Vendor hooks (all read from the original's Zig):
//! * Type subscript → `typing._class_getitem` (`obj.zig:600-614`):
//!   after the type's own subscr slot declines, an exact-type
//!   base routes to the frozen `typing._class_getitem`. The VM
//!   path runs it in `try_instance_subscr` (`vm_seams.rs`) — the
//!   LOAD_SUBSCR call site is in `vm_dispatch.rs` (the F3 lane's
//!   file) and the slot path (`obj_subscr`) has no import/call
//!   access, so the VM-hook path is the only place it can live.
//! * Type `|` → `typing._union` (`runtime.zig:575-578`): both
//!   sides a type or None (not both None) route to the frozen
//!   `typing._union`, before the small-int path and before
//!   `nativeSubclassOp` (`binary_op_ctx`, `vm_rt.rs`).
//! * `isinstance` / `issubclass` with a UnionType
//!   (`objtype.zig:514-549`): `union_args` reads `__args__`
//!   (and rejects a classinfo WITH `__origin__`, i.e. a generic
//!   alias). The builtin adapters now use the REAL `load_maybe`
//!   (`subclass.rs`), and `real_load_maybe` (`seams.rs`) reads
//!   the instance members map first — the vendor's
//!   `mp_load_method_maybe` calls the attr slot before the
//!   locals_dict search, so a UnionType's `__args__` tuple (an
//!   instance member) is visible. The adapters also
//!   `record_typed` the `ObjTypeError`, so the arg-1/arg-2
//!   messages are no longer bare `TypeError`s (pre-existing
//!   gap on master: `isinstance(1, 5)` raised with an empty
//!   message).
//! * `get_type_hints` is frozen Python; it needs `__annotations__`
//!   in the class dict, `__bases__`, `__module__`, `sys.modules.get`,
//!   `__import__`, and `eval` with explicit globals/locals — all
//!   verified by probe, no Rust change needed for them.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied (same as
//! `vm_tests_f4.rs` and the E2 series).

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
                other => panic!("exception value is not a str: {other:?}"),
            },
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}

#[test]
fn f5a_builtin_type_subscript_reprs() {
    let mut fx = Fx::new();
    fx.run_ok(
        "a = str(list[str])\n\
         b = str(dict[str, tuple[int, ...]])\n\
         c = str(list[int]([1]))\n\
         d = str(type(list[int]))\n",
    );
    assert_eq!(fx.gs("a"), "list[str]");
    assert_eq!(fx.gs("b"), "dict[str, tuple[int, ...]]");
    assert_eq!(fx.gs("c"), "[1]");
    assert_eq!(fx.gs("d"), "<class 'GenericAlias'>");
}

#[test]
fn f5a_list_of_user_class_repr() {
    let mut fx = Fx::new();
    fx.run_ok("class A: pass\na = str(list[A])\n");
    // The lib harness loads the module as "<test>", so the
    // class's `__module__` is "<test>" — the binary probe
    // (`f5a_probe/subscr.py`, run as a script) verifies the
    // `__main__` form, `list[__main__.A]`.
    assert_eq!(fx.gs("a"), "list[<test>.A]");
}

#[test]
fn f5a_user_class_getitem() {
    let mut fx = Fx::new();
    fx.run_ok(
        "class G:\n\
         \x20   def __class_getitem__(cls, item): return (cls.__name__, item)\n\
         a = str(G[int])\n",
    );
    assert_eq!(fx.gs("a"), "('G', <class 'int'>)");
}

#[test]
fn f5a_type_or_union() {
    let mut fx = Fx::new();
    fx.run_ok(
        "a = str(int | str)\n\
         b = str(int | int)\n\
         c = str(None | int)\n\
         d = str(int | None | str)\n\
         e = str(list[str] | None)\n",
    );
    assert_eq!(fx.gs("a"), "int | str");
    assert_eq!(fx.gs("b"), "<class 'int'>");
    assert_eq!(fx.gs("c"), "None | int");
    assert_eq!(fx.gs("d"), "int | None | str");
    assert_eq!(fx.gs("e"), "list[str] | None");
}

#[test]
fn f5a_union_eq() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from typing import Optional, Union\n\
         a = int | str == str | int\n\
         b = Union[int, None] == Optional[int]\n",
    );
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
}

#[test]
fn f5a_isinstance_union() {
    let mut fx = Fx::new();
    fx.run_ok(
        "a = isinstance(1, int | None)\n\
         b = isinstance('a', int | None)\n\
         c = isinstance(1.5, int | str)\n\
         d = isinstance(None, int | None)\n",
    );
    assert!(fx.gb("a"));
    assert!(!fx.gb("b"));
    // 1.5 is a float — an instance of neither `int` nor `str`.
    assert!(!fx.gb("c"));
    assert!(fx.gb("d"));
}

#[test]
fn f5a_issubclass_union() {
    let mut fx = Fx::new();
    fx.run_ok(
        "a = issubclass(int, int | str)\n\
         b = issubclass(str, int | str)\n\
         c = issubclass(bool, int | str)\n",
    );
    assert!(fx.gb("a"));
    assert!(fx.gb("b"));
    assert!(!fx.gb("c"));
}

#[test]
fn f5a_isinstance_generic_alias_raises() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg("isinstance(1, list[int])");
    assert_eq!(name, "TypeError");
    assert_eq!(
        msg,
        "issubclass() arg 2 must be a class or a tuple of classes"
    );
}

#[test]
fn f5a_issubclass_arg_errors_have_messages() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg("issubclass(1, int)");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "issubclass() arg 1 must be a class");
    let (name, msg) = fx.raised_msg("isinstance(1, 5)");
    assert_eq!(name, "TypeError");
    assert_eq!(
        msg,
        "issubclass() arg 2 must be a class or a tuple of classes"
    );
}

#[test]
fn f5a_type_or_non_type_raises() {
    let mut fx = Fx::new();
    let (name, msg) = fx.raised_msg("int | 5");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "unsupported types for __or__: 'type', 'int'");
    let (name, msg) = fx.raised_msg("None | None");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "unsupported types for __or__: 'NoneType', 'NoneType'");
}

#[test]
fn f5a_get_type_hints() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from typing import Optional, get_type_hints\n\
         class A: pass\n\
         Names = list[str] | None\n\
         class B:\n\
         \x20   x: int\n\
         \x20   y: \"list[str]\"\n\
         class C(B):\n\
         \x20   z: Optional[A] = None\n\
         \x20   w: 'Names'\n\
         h = str(get_type_hints(C))\n",
    );
    // Same "<test>" module-name note as
    // `f5a_list_of_user_class_repr`: the binary probe of the
    // integration case verifies the `__main__.A` form.
    assert_eq!(
        fx.gs("h"),
        "{'x': <class 'int'>, 'y': list[str], 'z': <test>.A | None, 'w': list[str] | None}"
    );
}
