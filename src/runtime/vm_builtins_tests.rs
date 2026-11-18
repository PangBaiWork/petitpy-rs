//! Tests for Lane D2, step 5: `print(file=...)`
//! (`modbuiltins.zig:661-733`, full path).
//!
//! End-to-end through `Sx` (the dispatch path). Expected
//! values are the reference binary's, probed 2026-11-18 (see
//! `scripts/d2_result.txt` §2): `print("b", file=sys.stderr)`
//! works, `print("hello", file=io.StringIO())` lands in the
//! buffer, `print("x", file=None)` and `print("x", file=123)`
//! ABORT the reference (rc=134) — Rust raises the vendor-C
//! `OSError("stream operation not supported")` instead.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::obj::obj_get_type;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::{Vm, VmError};

#[test]
fn print_to_stringio() {
    let mut fx = Sx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO()
print("hello", file=s)
assert s.getvalue() == "hello\n"
print(1, 2, sep="-", end="!", file=s)
assert s.getvalue() == "hello\n1-2!"
print("a", "b", file=s, flush=True)
assert s.getvalue() == "hello\n1-2!a b\n"
print("c", file=s, flush=1)
assert s.getvalue() == "hello\n1-2!a b\nc\n"
"#,
    );
}

#[test]
fn print_to_sys_stdout_stderr() {
    let mut fx = Sx::new();
    // These write to the test process's own stdout/stderr
    // (cargo captures them); the assertion is that they do
    // not raise.
    fx.run_ok(
        r#"
import sys
print("to-stdout", file=sys.stdout)
print("to-stderr", file=sys.stderr)
"#,
    );
}

#[test]
fn print_file_nonstream_raises() {
    let mut fx = Sx::new();
    // The reference ABORTS on both (rc=134); Rust raises the
    // vendor-C OSError instead (documented divergence).
    let (name, value) = fx.run_err("print(\"x\", file=123)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "stream operation not supported");
    let (name, value) = fx.run_err("print(\"x\", file=None)\n");
    assert_eq!(name, "OSError");
    assert_eq!(value, "stream operation not supported");
}

/// A fresh VM with the `io` module registered (C18b's
/// `setup_io` pattern), plus the `Fx` read-back helpers.
/// Copied from `modsys_tests.rs` so the `print` tests are not
/// blocked on D1's `import io` registration.
struct Sx {
    vm: Vm,
}

fn open_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

impl Sx {
    fn new() -> Self {
        let mut vm = Vm::new();
        let fun_var = vm.state.vm_types.fun_builtin_var;
        let module_type = vm.state.vm_types.module;
        let io_module = {
            let (mut ctx, state) = vm.split();
            let open_id = static_qstr_id(b"open").expect("open is a static qstr");
            let open = ctx.alloc(
                Some(fun_var),
                Obj::FunBuiltin(FunBuiltinData {
                    name: open_id,
                    n_args: fun_n_args(1, 3),
                    func: open_stub,
                    kw_func: None,
                }),
            );
            let m = build_io_module(&mut ctx, fun_var, module_type, Value::Heap(open))
                .expect("io module builds");
            let io_id = static_qstr_id(b"io").expect("io is a static qstr");
            state.builtin_modules.push((io_id, m));
            m
        };
        vm.pin(io_module);
        Self { vm }
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

    fn run_err(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module unexpectedly succeeded: {src}"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Err(e) => panic!("module failed without raising: {e:?} ({src})"),
        }
    }

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
            Value::SmallInt(n) => n.to_string(),
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}
