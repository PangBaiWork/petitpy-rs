//! Lane D3 tests: the `struct` module
//! (`src/modules/modstruct.zig`), end-to-end snippets through
//! `Vm` (the `struct` module is built with
//! `build_struct_module` and registered in
//! `state.builtin_modules`, the way D1 will register it).
//! Expected values are the reference binary's, probed
//! 2026-11-18 (see `scripts/d3_result.txt` §2).

use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

use crate::modules::modstruct::build_struct_module;

/// Build the `struct` module and register it the way D1 will
/// (`state.builtin_modules`), then pin it in `roots.extra`.
fn setup_struct(vm: &mut Vm) {
    let fun_var = vm.state.vm_types.fun_builtin_var;
    let module_type = vm.state.vm_types.module;
    let struct_module = {
        let (mut ctx, state) = vm.split();
        let m = build_struct_module(&mut ctx, fun_var, module_type)
            .expect("struct module builds");
        let struct_id = static_qstr_id(b"struct").expect("struct is a static qstr");
        state.builtin_modules.push((struct_id, m));
        m
    };
    vm.pin(struct_module);
}

/// A fresh VM with `struct` registered, plus the read-back
/// helpers (the `objstringio_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_struct(&mut vm);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => self.raised_parts(exc),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn raised_parts(&mut self, exc: Value) -> (String, String) {
        let t = obj_get_type(&self.vm.arena, &self.vm.types, exc).expect("an exception has a type");
        let name = match self.vm.arena.get(t) {
            Obj::Type(d) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(d.name as u16)).into_owned()
            }
            _ => panic!("exception type is not a type object"),
        };
        let msg = match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => match d.args.first() {
                    Some(v) => self.val_str(*v),
                    None => String::new(),
                },
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        };
        (name, msg)
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

#[test]
fn struct_calcsize() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
assert struct.calcsize('') == 0
assert struct.calcsize('<i') == 4
assert struct.calcsize('>i') == 4
assert struct.calcsize('<hi') == 6
assert struct.calcsize('@hi') == 8
assert struct.calcsize('@i') == 4
assert struct.calcsize('<2i') == 8
assert struct.calcsize('<0i') == 0
assert struct.calcsize('<3x') == 3
assert struct.calcsize('<2s') == 2
assert struct.calcsize('<q') == 8
assert struct.calcsize('<l') == 4
assert struct.calcsize('@l') == 8
assert struct.calcsize('<P') == 8
assert struct.calcsize('<O') == 8
assert struct.calcsize('<S') == 8
assert struct.calcsize('<e') == 2
assert struct.calcsize('<f') == 4
assert struct.calcsize('<d') == 8
assert struct.calcsize('<b') == 1
assert struct.calcsize('<B') == 1
assert struct.calcsize('<h') == 2
assert struct.calcsize('<H') == 2
assert struct.calcsize('<I') == 4
assert struct.calcsize('<Q') == 8
assert struct.calcsize('!i') == 4
assert struct.calcsize('<10s') == 10
"#,
    );
    for fmt in [" ", "< i", "c", "?", "z", "=i", "2"] {
        assert_eq!(
            fx.raised_msg(&format!("import struct\nstruct.calcsize('{fmt}')")),
            ("ValueError".to_string(), "bad typecode".to_string()),
            "calcsize('{fmt}')"
        );
    }
}

#[test]
fn struct_pack() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
assert struct.pack('<i', 1) == b'\x01\x00\x00\x00'
assert struct.pack('<i', -1) == b'\xff\xff\xff\xff'
assert struct.pack('<i', 2**40) == b'\x00\x00\x00\x00'
assert struct.pack('<2i', 1) == b'\x01\x00\x00\x00\x00\x00\x00\x00'
assert struct.pack('<i', 1, 2, 3) == b'\x01\x00\x00\x00'
assert struct.pack('<s', 'ab') == b'a'
assert struct.pack('<2s', 'ab') == b'ab'
assert struct.pack('<2s', 'a') == b'a\x00'
assert struct.pack('<2s', 'abc') == b'ab'
assert struct.pack('<h', 0x1234) == b'\x34\x12'
assert struct.pack('>h', 0x1234) == b'\x12\x34'
assert struct.pack('<f', 1) == b'\x01\x00\x00\x00'
assert struct.pack('<q', -1) == b'\xff\xff\xff\xff\xff\xff\xff\xff'
assert struct.pack('<i', True) == b'\x01\x00\x00\x00'
assert struct.pack('@hi', 1, 2) == b'\x01\x00\x00\x00\x02\x00\x00\x00'
assert struct.pack('<3x') == b'\x00\x00\x00'
assert struct.pack('<O', 5) == b'\x0b\x00\x00\x00\x00\x00\x00\x00'
"#,
    );
    for (src, want) in [
        (
            "import struct\nstruct.pack('<f', 1.5)",
            ("TypeError", "can't convert to int"),
        ),
        (
            "import struct\nstruct.pack('<d', 1.5)",
            ("TypeError", "can't convert to int"),
        ),
        (
            "import struct\nstruct.pack('<e', 1.5)",
            ("TypeError", "can't convert to int"),
        ),
        (
            "import struct\nstruct.pack('<i', 'x')",
            ("TypeError", "can't convert to int"),
        ),
        (
            "import struct\nstruct.pack('=i', 1)",
            ("ValueError", "bad typecode"),
        ),
        (
            "import struct\nstruct.pack('c', 1)",
            ("ValueError", "bad typecode"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "{src}"
        );
    }
}

#[test]
fn struct_unpack() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
assert struct.unpack('<i', b'\x01\x00\x00\x00') == (1,)
assert struct.unpack('<f', b'\x00\x00\xc0\x3f') == (1069547520,)
assert struct.unpack('<d', b'\x00\x00\x00\x00\x00\x00\xf8\x3f') == (4609434218613702656,)
assert struct.unpack('<2i', b'\x01\x00\x00\x00\x02\x00\x00\x00') == (1, 2)
assert struct.unpack('<s', b'ab') == (b'a',)
assert struct.unpack('<2s', b'ab') == (b'ab',)
assert struct.unpack('<x', b'\x00') == ()
assert struct.unpack('', b'') == ()
assert struct.unpack('@hi', b'\x01\x00\x00\x00\x02\x00\x00\x00') == (1, 2)
assert struct.unpack('<O', b'\x0b\x00\x00\x00\x00\x00\x00\x00') == (5,)
assert struct.unpack_from('<i', b'\x00\x01\x00\x00\x00', 1) == (1,)
assert struct.unpack_from('<i', b'\x01\x00\x00\x00\x00', -5) == (1,)
assert struct.unpack is struct.unpack_from
"#,
    );
    for (src, want) in [
        (
            "import struct\nstruct.unpack('<i', 123)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nstruct.unpack('<i', b'\x01\x00')",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nstruct.unpack_from('<i', b'\x01\x00\x00\x00', -5)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nstruct.unpack_from('<i', b'\x01\x00\x00\x00', 1)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nstruct.unpack('=i', b'\x01\x00\x00\x00')",
            ("ValueError", "bad typecode"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "{src}"
        );
    }
}

#[test]
fn struct_pack_into() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
b = bytearray(b'\x00\x00\x00\x00\x00\x00\x00\x00')
struct.pack_into('<i', b, 0, 1)
assert b[0] == 1 and b[1] == 0 and b[2] == 0 and b[3] == 0
struct.pack_into('<i', b, 4, 2)
assert b[4] == 2 and b[5] == 0
struct.pack_into('<h', b, -2, 0x1234)
assert b[6] == 0x34 and b[7] == 0x12
"#,
    );
    for (src, want) in [
        (
            "import struct\nstruct.pack_into('<i', b'\\x00\\x00\\x00\\x00', 0, 1)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nstruct.pack_into('<i', 'xxxx', 0, 1)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nb = bytearray(4)\nstruct.pack_into('<i', b, 1, 1)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nb = bytearray(4)\nstruct.pack_into('<i', b, -5, 1)",
            ("ValueError", "buffer too small"),
        ),
        (
            "import struct\nb = bytearray(4)\nstruct.pack_into('=i', b, 0, 1)",
            ("ValueError", "bad typecode"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "{src}"
        );
    }
}

#[test]
fn struct_no_error_type() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
try:
    struct.error
    assert False
except AttributeError:
    assert True
"#,
    );
}

/// C13 VM BUG (not a D3 bug): `and` with a False left
/// followed by a call raises `TypeError("object not
/// callable")` in the Rust VM — the `JUMP_IF_FALSE_OR_POP`
/// path leaves the stack misaligned when the jump is
/// taken, so the following call pops a wrong callee. The
/// reference prints `False` cleanly. Minimal repro; the
/// fixing lane removes the `#[ignore]`.
/// (Found by Lane D3 while bisecting `struct_pack`:
/// every pack line passes in isolation, the chain failed
/// only once an intermediate link went False.)
#[test]
#[ignore = "C13 VM and-short-circuit bug: jump-taken path misaligns the stack"]
fn and_false_shortcircuit_vm_bug() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import struct
ok = False
ok = ok and struct.pack('<i', 1) == b'\x01\x00\x00\x00'
assert ok == False
"#,
    );
}
