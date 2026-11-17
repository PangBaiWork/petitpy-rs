//! Lane C18b tests: `io.StringIO` / `io.BytesIO` / `io.IOBase`
//! (vendor `py/objstringio.c` + `py/modio.c`), end-to-end
//! snippets through `Vm` (the `io` module is built with
//! `build_io_module` and registered in
//! `state.builtin_modules`, the way D1 will register it —
//! `import io` then resolves). The `CallCtx`-level tests for
//! the protocol fns and the `IOBase` delegation live in
//! `objstringio_tests_ctx.rs`.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::{Vm, VmError};

/// The `open` entry of the `io` module table: D's `file_io`
/// (`mp_builtin_open_obj`). Never called by these tests — a
/// bare-`PyRaise` stub stands in for it.
fn open_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// Build the `io` module and register it the way D1 will
/// (`state.builtin_modules`), then pin it in `roots.extra`.
fn setup_io(vm: &mut Vm) {
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
}

/// A fresh VM with `io` registered, plus the read-back
/// helpers (the `vm_tests.rs` `Fx` / `import_tests.rs` `Ix`
/// pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_io(&mut vm);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let name = self.exc_name(exc);
                let args = self.exc_args(exc);
                let msg = match args.first() {
                    Some(Value::Qstr(q)) => {
                        String::from_utf8_lossy(self.vm.pool.qstr_str(*q as u16)).into_owned()
                    }
                    Some(Value::Heap(r)) => match self.vm.arena.get(*r) {
                        Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                        _ => format!("{args:?}"),
                    },
                    _ => format!("{args:?}"),
                };
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, args).
    fn raised(&mut self, src: &str) -> (String, Vec<Value>) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_args(exc)),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        let (name, args) = self.raised(src);
        let msg = match args.first() {
            Some(v) => self.val_str(*v),
            None => String::new(),
        };
        (name, msg)
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
            other => panic!("global {name} is not a small int: {other:?}"),
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

    /// The exception's args, as values.
    fn exc_args(&mut self, exc: Value) -> Vec<Value> {
        match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => d.args.clone(),
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        }
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String`.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("value is not a str: {other:?}"),
            },
            other => panic!("value is not a str: {other:?}"),
        }
    }
}

use crate::objects::obj::obj_get_type;

#[test]
fn stringio_read_write_getvalue() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("hello")
ok_r1 = s.read() == "hello"
s.seek(0)
ok_r2 = s.read(3) == "hel"
ok_v = s.getvalue() == "hello"
s.write("!")
s.seek(0)
ok_r3 = s.read() == "hel!o"
ok_tell = s.tell() == 5
"#,
    );
    assert!(fx.gb("ok_r1"));
    assert!(fx.gb("ok_r2"));
    assert!(fx.gb("ok_v"));
    assert!(fx.gb("ok_r3"));
    assert!(fx.gb("ok_tell"));
}

#[test]
fn stringio_ctor_forms() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
a = io.StringIO()
ok_empty = a.getvalue() == ""
b = io.StringIO(8)
b.write("xy")
ok_size = b.getvalue() == "xy"
c = io.StringIO(b"abc")
ok_bytes = c.getvalue() == "abc"
d = io.StringIO(bytearray(b"abc"))
ok_ba = d.getvalue() == "abc"
e = io.StringIO("a", "b")
ok_extra = e.getvalue() == "a"
f = io.StringIO(initial_value="x")
ok_kw = f.getvalue() == ""
g = io.StringIO("ab")
ok_type = type(g.getvalue()) is str
"#,
    );
    assert!(fx.gb("ok_empty"));
    assert!(fx.gb("ok_size"));
    assert!(fx.gb("ok_bytes"));
    assert!(fx.gb("ok_ba"));
    assert!(fx.gb("ok_extra"));
    assert!(fx.gb("ok_kw"));
    assert!(fx.gb("ok_type"));
}

#[test]
fn stringio_ctor_errors() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO(True)"),
        ("TypeError".to_string(), "object with buffer protocol required".to_string())
    );
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO(3.5)"),
        ("TypeError".to_string(), "object with buffer protocol required".to_string())
    );
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO([1])"),
        ("TypeError".to_string(), "object with buffer protocol required".to_string())
    );
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO(-5)"),
        ("MemoryError".to_string(), "memory allocation failed".to_string())
    );
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO(2**24 + 1)"),
        ("MemoryError".to_string(), "memory allocation failed".to_string())
    );
    assert_eq!(
        fx.raised_msg("import io\nio.StringIO(2**70)"),
        (
            "OverflowError".to_string(),
            "overflow converting long int to machine word".to_string()
        )
    );
}

#[test]
fn stringio_max_alloc_boundary() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO(2**24)
ok_size = s.tell() == 0
"#,
    );
    assert!(fx.gb("ok_size"));
}

#[test]
fn stringio_closed_split() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("ab")
s.seek(2)
s.close()
ok_seek = s.seek(0) == 0
ok_tell = s.tell() == 0
ok_flush = s.flush() is None
"#,
    );
    assert!(fx.gb("ok_seek"));
    assert!(fx.gb("ok_tell"));
    assert!(fx.gb("ok_flush"));
    // getvalue / read / write / readline raise; pos is NOT
    // reset by close (tell after close keeps the position —
    // checked with a fresh object that closed at pos 2).
    assert_eq!(
        fx.raised_msg(
            "import io\ns = io.StringIO('ab')\ns.getvalue()\ns.close()\ns.getvalue()"
        ),
        (
            "ValueError".to_string(),
            "I/O operation on closed file".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import io\ns = io.StringIO('ab')\ns.close()\ns.read()"),
        (
            "ValueError".to_string(),
            "I/O operation on closed file".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import io\ns = io.StringIO('ab')\ns.close()\ns.write('x')"),
        (
            "ValueError".to_string(),
            "I/O operation on closed file".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import io\ns = io.StringIO('ab')\ns.close()\ns.readline()"),
        (
            "ValueError".to_string(),
            "I/O operation on closed file".to_string()
        )
    );
    let mut fx2 = Fx::new();
    fx2.run_ok(
        r#"
import io
s = io.StringIO("ab")
s.seek(2)
s.close()
ok_tell2 = s.tell() == 2
"#,
    );
    assert!(fx2.gb("ok_tell2"));
}

#[test]
fn stringio_seek_edges() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("ab")
ok_cur = s.seek(-5, 1) == 0
ok_end = s.seek(-100, 2) == 0
ok_whence = s.seek(5, 9) == 5
ok_tell = s.tell() == 5
"#,
    );
    assert!(fx.gb("ok_cur"));
    assert!(fx.gb("ok_end"));
    assert!(fx.gb("ok_whence"));
    assert!(fx.gb("ok_tell"));
    let (name, args) = fx.raised("import io\ns = io.StringIO('ab')\ns.seek(-1)");
    assert_eq!(name, "OSError");
    assert_eq!(args, vec![Value::SmallInt(22)]);
}

#[test]
fn stringio_readinto() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("hello")
b = bytearray(3)
n1 = s.readinto(b)
ok_b1 = b == bytearray(b"hel")
b2 = bytearray(10)
n2 = s.readinto(b2)
ok_b2 = b2[0] == 108 and b2[1] == 111 and n2 == 2
"#,
    );
    assert_eq!(fx.gi("n1"), 3);
    assert!(fx.gb("ok_b1"));
    assert_eq!(fx.gi("n2"), 2);
    assert!(fx.gb("ok_b2"));
}

#[test]
fn stringio_readline_and_iter() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("ab\ncd\n")
ok_rl = s.readline(2) == "ab"
lines = []
for line in io.StringIO("a\nb\n"):
    lines.append(line)
ok_iter = lines == ["a\n", "b\n"]
"#,
    );
    assert!(fx.gb("ok_rl"));
    assert!(fx.gb("ok_iter"));
}

#[test]
fn stringio_with() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("x")
with s as t:
    r = t.read()
    ok_same = t is s
ok_r = r == "x"
closed = ""
try:
    s.getvalue()
except ValueError:
    closed = "closed"
ok_closed = closed == "closed"
"#,
    );
    assert!(fx.gb("ok_r"));
    assert!(fx.gb("ok_same"));
    assert!(fx.gb("ok_closed"));
}

#[test]
fn stringio_write_hole() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO("ab")
s.seek(5)
s.write("c")
ok_hole = s.getvalue() == "ab\x00\x00\x00c"
"#,
    );
    assert!(fx.gb("ok_hole"));
}

#[test]
fn stringio_repr() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
a = io.StringIO()
b = io.BytesIO()
ok_a = str(a).startswith("<io.StringIO 0x")
ok_b = str(b).startswith("<io.BytesIO 0x")
"#,
    );
    assert!(fx.gb("ok_a"));
    assert!(fx.gb("ok_b"));
}

#[test]
fn bytesio_basic_and_cow() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
a = io.BytesIO(b"ab")
ok_type = type(a.getvalue()) is bytes
ok_read = a.read() == b"ab"
orig = bytearray(b"abc")
b = io.BytesIO(orig)
b.write(b"X")
ok_cow = orig == bytearray(b"abc")
c = io.BytesIO("abc")
ok_str = c.getvalue() == b"abc"
"#,
    );
    assert!(fx.gb("ok_type"));
    assert!(fx.gb("ok_read"));
    assert!(fx.gb("ok_cow"));
    assert!(fx.gb("ok_str"));
}

#[test]
fn iobase_attrs_and_fresh() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
a = io.IOBase()
b = io.IOBase()
ok_fresh = a is not b
ok_repr = str(a) == "<IOBase>"
try:
    a.readinto
    ok_no_readinto = False
except AttributeError:
    ok_no_readinto = True
try:
    a.write
    ok_no_write = False
except AttributeError:
    ok_no_write = True
try:
    a.readline
    ok_no_readline = False
except AttributeError:
    ok_no_readline = True
try:
    a.closed
    ok_no_closed = False
except AttributeError:
    ok_no_closed = True
try:
    a.ioctl
    ok_no_ioctl = False
except AttributeError:
    ok_no_ioctl = True
"#,
    );
    assert!(fx.gb("ok_fresh"));
    assert!(fx.gb("ok_repr"));
    assert!(fx.gb("ok_no_readinto"));
    assert!(fx.gb("ok_no_write"));
    assert!(fx.gb("ok_no_readline"));
    assert!(fx.gb("ok_no_closed"));
    assert!(fx.gb("ok_no_ioctl"));
}

#[test]
fn stringio_method_table_shape() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
s = io.StringIO()
ok_has = True
try:
    s.read
    s.readinto
    s.readline
    s.write
    s.seek
    s.tell
    s.flush
    s.close
    s.getvalue
    s.__enter__
    s.__exit__
except AttributeError:
    ok_has = False
ok_no = True
try:
    s.read1
    ok_no = False
except AttributeError:
    pass
try:
    s.write1
    ok_no = False
except AttributeError:
    pass
try:
    s.readinto1
    ok_no = False
except AttributeError:
    pass
try:
    s.readlines
    ok_no = False
except AttributeError:
    pass
try:
    s.ioctl
    ok_no = False
except AttributeError:
    pass
try:
    s.closed
    ok_no = False
except AttributeError:
    pass
try:
    s.seekable
    ok_no = False
except AttributeError:
    pass
try:
    s.readable
    ok_no = False
except AttributeError:
    pass
try:
    s.writable
    ok_no = False
except AttributeError:
    pass
"#,
    );
    assert!(fx.gb("ok_has"));
    assert!(fx.gb("ok_no"));
}

#[test]
fn io_module_shape() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import io
ok_name = io.__name__ == "io"
ok_has = True
try:
    io.StringIO
    io.BytesIO
    io.IOBase
    io.open
except AttributeError:
    ok_has = False
try:
    io.BufferedWriter
    ok_no_bw = False
except AttributeError:
    ok_no_bw = True
"#,
    );
    assert!(fx.gb("ok_name"));
    assert!(fx.gb("ok_has"));
    assert!(fx.gb("ok_no_bw"));
}
