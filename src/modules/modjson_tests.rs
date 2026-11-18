//! Lane D3 tests: the `_json` module (`src/modules/modjson.zig`),
//! end-to-end snippets through `Vm` (the `_json` module is built
//! with `build_json_module` and registered in
//! `state.builtin_modules`, the way D1 will register it).
//! Expected values are the reference binary's, probed
//! 2026-11-18 (see `scripts/d3_result.txt` §2).

use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

use crate::modules::modjson::build_json_module;

/// Build the `_json` module and register it the way D1 will
/// (`state.builtin_modules`), then pin it in `roots.extra`.
fn setup_json(vm: &mut Vm) {
    let fun_var = vm.state.vm_types.fun_builtin_var;
    let module_type = vm.state.vm_types.module;
    let json_module = {
        let (mut ctx, state) = vm.split();
        let m = build_json_module(&mut ctx, fun_var, module_type)
            .expect("_json module builds");
        let json_id = static_qstr_id(b"_json").expect("_json is a static qstr");
        state.builtin_modules.push((json_id, m));
        m
    };
    vm.pin(json_module);
}

/// A fresh VM with `_json` registered, plus the read-back
/// helpers (the `objstringio_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_json(&mut vm);
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
fn json_encode_basics() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _json
assert _json.encode(None, ",", ":", True) == "null"
assert _json.encode(True, ",", ":", True) == "true"
assert _json.encode(False, ",", ":", True) == "false"
assert _json.encode(0, ",", ":", True) == "0"
assert _json.encode(-42, ",", ":", True) == "-42"
assert _json.encode(2**70, ",", ":", True) == "1180591620717411303424"
assert _json.encode(1.5, ",", ":", True) == "1.5"
assert _json.encode(1.0, ",", ":", True) == "1.0"
assert _json.encode(1e30, ",", ":", True) == "1e+30"
assert _json.encode("abc", ",", ":", True) == '"abc"'
assert _json.encode([1, "a", None, True, 2.5], ",", ":", True) == '[1,"a",null,true,2.5]'
assert _json.encode((1, 2), ",", ":", True) == "[1,2]"
assert _json.encode({"b": 1, "a": 2}, ",", ":", True) == '{"b":1,"a":2}'
assert _json.encode({"a": 1}, ", ", ": ", True) == '{"a": 1}'
assert _json.encode([], ",", ":", True) == "[]"
assert _json.encode({}, ",", ":", True) == "{}"
assert _json.encode("a\"b\\c\nd", ",", ":", True) == '"a\\"b\\\\c\\nd"'
assert _json.encode("\x01\x1f", ",", ":", True) == '"\\u0001\\u001f"'
"#,
    );
}

#[test]
fn json_encode_ascii() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _json
assert _json.encode("é", ",", ":", True) == '"\\u00e9"'
assert _json.encode("é", ",", ":", False) == '"é"'
assert _json.encode("😀", ",", ":", True) == '"\\ud83d\\ude00"'
assert _json.encode("😀", ",", ":", False) == '"😀"'
assert _json.encode("aéb", ",", ":", 1) == '"aéb"'
"#,
    );
}

#[test]
fn json_encode_unsupported() {
    let mut fx = Fx::new();
    for src in [
        "import _json\n_json.encode(float('inf'), ',', ':', True)",
        "import _json\n_json.encode(float('nan'), ',', ':', True)",
        "import _json\n_json.encode(b'hi', ',', ':', True)",
        "import _json\n_json.encode({1: 2}, ',', ':', True)",
        "import _json\n_json.encode({1, 2}, ',', ':', True)",
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "TypeError".to_string(),
                "not handled by the native JSON encoder".to_string()
            ),
            "{src}"
        );
    }
    // Deeper than 256 levels → the same TypeError (the
    // Python layer turns it into RecursionError). The value
    // must be built and encoded in ONE module (each run has
    // fresh globals).
    assert_eq!(
        fx.raised_msg(
            "import _json\nx = []\nfor i in range(300):\n    x = [x]\n_json.encode(x, ',', ':', True)",
        ),
        (
            "TypeError".to_string(),
            "not handled by the native JSON encoder".to_string()
        )
    );
    // A non-str separator is a different TypeError.
    assert_eq!(
        fx.raised_msg("import _json\n_json.encode(1, 2, ',', True)"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
}

#[test]
fn json_string_fn() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _json
assert _json.string("a\"b", True) == '"a\\"b"'
assert _json.string("é", True) == '"\\u00e9"'
assert _json.string("é", False) == '"é"'
assert _json.string("\n", True) == '"\\n"'
assert _json.string("\x01", True) == '"\\u0001"'
assert _json.string("", True) == '""'
"#,
    );
    assert_eq!(
        fx.raised_msg("import _json\n_json.string(123, True)"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
}

#[test]
fn json_decode_basics() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import _json
assert _json.decode("null") is None
assert _json.decode("true") is True
assert _json.decode("false") is False
assert _json.decode("123") == 123
assert _json.decode("-45") == -45
assert _json.decode("0") == 0
assert _json.decode("1.5") == 1.5
assert _json.decode("1e3") == 1000.0
assert _json.decode("-2.5e-2") == -0.025
assert _json.decode('"abc"') == "abc"
assert _json.decode('[1, 2, "a"]') == [1, 2, "a"]
assert _json.decode('{"a": 1}') == {"a": 1}
assert _json.decode('  [1,2]  ') == [1, 2]
assert _json.decode('{"a": {"b": [true, null]}}') == {"a": {"b": [True, None]}}
assert _json.decode('[]') == []
assert _json.decode('{}') == {}
assert _json.decode('123456789012345678') == 123456789012345678
assert _json.decode('1234567890123456789') == 1234567890123456789
assert _json.decode('"a\\nb"') == "a\nb"
assert _json.decode('"\\u00e9"') == "é"
assert _json.decode('"\\/"') == "/"
assert _json.decode('"\\u0041"') == "A"
"#,
    );
}

#[test]
fn json_decode_errors() {
    let mut fx = Fx::new();
    for src in [
        "import _json\n_json.decode('01')",
        "import _json\n_json.decode('1.')",
        "import _json\n_json.decode('1e')",
        "import _json\n_json.decode('1e+')",
        "import _json\n_json.decode('[1,2] x')",
        "import _json\n_json.decode('')",
        "import _json\n_json.decode('{')",
        "import _json\n_json.decode('\"\\\\ud83d\\\\ude00\"')",
        "import _json\n_json.decode('\"\\\\ud800\"')",
        "import _json\n_json.decode('\"a\x01b\"')",
        "import _json\n_json.decode(\"{'a': 1}\")",
        "import _json\n_json.decode('[1,]')",
        "import _json\n_json.decode('{\"a\":1,}')",
        "import _json\n_json.decode('tru')",
        "import _json\n_json.decode('nul')",
        "import _json\n_json.decode('NaN')",
        "import _json\n_json.decode('Infinity')",
        "import _json\n_json.decode('-')",
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (
                "ValueError".to_string(),
                "not handled by the native JSON decoder".to_string()
            ),
            "{src}"
        );
    }
    // A non-str input is the decoder ValueError too (probed).
    assert_eq!(
        fx.raised_msg("import _json\n_json.decode(123)"),
        (
            "ValueError".to_string(),
            "not handled by the native JSON decoder".to_string()
        )
    );
    // Deeper than 256 levels → ValueError (built and decoded
    // in one module).
    assert_eq!(
        fx.raised_msg("import _json\ns = '[' * 300 + ']' * 300\n_json.decode(s)"),
        (
            "ValueError".to_string(),
            "not handled by the native JSON decoder".to_string()
        )
    );
}
