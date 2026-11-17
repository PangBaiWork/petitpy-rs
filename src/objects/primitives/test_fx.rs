//! Shared VM test fixture for the C18a float/complex tests —
//! the `vm_tests.rs` `Fx` shape (a fresh `Vm` per test plus the
//! read-back helpers), local to `primitives` so both
//! `objfloat_tests.rs` and `objcomplex_tests.rs` use it without
//! duplicating ~200 lines.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::ops::{PrimOutcome, PrimResult};
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM per test, plus the read-back helpers (the
/// `vm_tests.rs` `Fx` shape, local to this module).
pub(crate) struct Fx {
    vm: Vm,
}

impl Fx {
    pub(crate) fn new() -> Self {
        Self { vm: Vm::new() }
    }

    pub(crate) fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    pub(crate) fn run_ok(&mut self, src: &str) -> Value {
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

    /// The `(name, message)` of the raised exception, or a panic
    /// naming what happened instead.
    pub(crate) fn run_err(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module unexpectedly succeeded: {src}"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Err(e) => panic!("module failed without raising: {e:?} ({src})"),
        }
    }

    /// Read a module global by name (`None` when absent).
    pub(crate) fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    pub(crate) fn gf(&mut self, name: &str) -> f64 {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Float(f) => *f,
                other => panic!("global {name} is not a float: {other:?}"),
            },
            other => panic!("global {name} is not a float: {other:?}"),
        }
    }

    pub(crate) fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not a small int: {other:?}"),
        }
    }

    pub(crate) fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    pub(crate) fn gs(&mut self, name: &str) -> String {
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

    /// A global complex, as `(re, im)`.
    pub(crate) fn gc(&mut self, name: &str) -> (f64, f64) {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Complex { re, im } => (*re, *im),
                other => panic!("global {name} is not a complex: {other:?}"),
            },
            other => panic!("global {name} is not a complex: {other:?}"),
        }
    }

    /// The exception's type name (`"ValueError"`, …).
    pub(crate) fn exc_name(&mut self, exc: Value) -> String {
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
    pub(crate) fn exc_value(&mut self, exc: Value) -> String {
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

/// Run a slot-level check with a `CallCtx` from a fresh VM.
pub(crate) fn slot_ctx<R>(f: impl FnOnce(&mut CallCtx<'_>, &crate::objects::obj::BuiltinTypes) -> R) -> R {
    let mut vm = Vm::new();
    let types = vm.types;
    let (mut ctx, _state) = vm.split();
    f(&mut ctx, &types)
}

// ── C14 case list, end to end ────────────────────────────────────────


/// The `PrimOutcome` of a rich float/complex op call.
pub(crate) fn outcome(r: PrimResult) -> PrimOutcome {
    match r {
        Ok(o) => o,
        Err(e) => panic!("slot raised unexpectedly: {e:?}"),
    }
}

/// Extract the float from a `PrimOutcome::Value`.
pub(crate) fn out_f64(arena: &crate::memory::gc::GcArena, o: PrimOutcome) -> f64 {
    match o {
        PrimOutcome::Value(Value::Heap(r)) => match arena.get(r) {
            Obj::Float(x) => *x,
            other => panic!("expected float, got {other:?}"),
        },
        other => panic!("expected float value, got {other:?}"),
    }
}

/// Extract the two floats from a `PrimOutcome::Value` tuple.
pub(crate) fn out_f64_pair(arena: &crate::memory::gc::GcArena, o: PrimOutcome) -> (f64, f64) {
    match o {
        PrimOutcome::Value(Value::Heap(r)) => match arena.get(r) {
            Obj::Tuple(items) => {
                assert_eq!(items.len(), 2, "expected a 2-tuple");
                (out_f64(arena, PrimOutcome::Value(items[0])), out_f64(arena, PrimOutcome::Value(items[1])))
            }
            other => panic!("expected tuple, got {other:?}"),
        },
        other => panic!("expected tuple value, got {other:?}"),
    }
}

