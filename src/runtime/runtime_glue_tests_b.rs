//! Tests for the `builtinevex` module (`builtinevex.zig`:
//! `eval` / `exec` / `compile`) — split out of
//! `runtime_glue_tests.rs` (file-size rule). Runs small Python
//! snippets through `Vm::load_module_from_source` (the
//! dispatch path, where `eval`/`exec`/`compile` are
//! intercepted by GcRef identity) and through `Vm::eval_exec`
//! directly (the `Vm`-level entry, with full interning).

use crate::compiler::parser::InputKind;
use crate::objects::containers::objdict::{dict_get_value, dict_store};
use crate::objects::containers::tests::empty_dict;
use crate::objects::obj::obj_get_type;
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::builtinevex::{call_compile_ctx, call_eval_exec_ctx};
use crate::runtime::vm::{Vm, VmError};

// ── builtinevex.zig ──────────────────────────────────────────────────

/// A fresh VM per test, plus the read-back helpers (the
/// `vm_tests.rs` `Fx` pattern, local to this section).
struct Vx {
    vm: Vm,
}

impl Vx {
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
                self.exc_args(exc)
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
    /// Rust `String` — for asserting exception messages.
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

    /// Run a module that must raise; return (type name, args).
    fn raised(&mut self, src: &str) -> (String, Vec<Value>) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_args(exc)),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }
}

#[test]
fn eval_returns_body_value() {
    let mut vx = Vx::new();
    vx.run_ok("r = eval(\"1+2\")");
    assert_eq!(vx.gi("r"), 3);
}

#[test]
fn eval_uses_module_globals() {
    let mut vx = Vx::new();
    vx.run_ok("x = 10\nr = eval(\"x + 5\")");
    assert_eq!(vx.gi("r"), 15);
}

#[test]
fn eval_distinct_locals_corpus() {
    // `builtin_eval.py`: `eval("foo", None, {"foo": 3})` reads
    // the locals dict (3); `eval("foo", None, {"bar": 3})`
    // falls through to the module globals (11).
    let mut vx = Vx::new();
    vx.run_ok(
        "foo = 11\nr1 = eval(\"foo\", None, {\"foo\": 3})\nr2 = eval(\"foo\", None, {\"bar\": 3})",
    );
    assert_eq!(vx.gi("r1"), 3);
    assert_eq!(vx.gi("r2"), 11);
}

#[test]
fn exec_distinct_locals_corpus() {
    // `builtin_exec.py`: `exec("def bar(): return 84", d)`
    // defines `bar` in `d`, then `d["bar"]()` returns 84.
    // (`bar = 0` first: the name must be interned by the outer
    // module load, or the dispatch-path read-only qstr resolve
    // cannot resolve it — see `eval_dispatch_new_name_fails_loud`.)
    let mut vx = Vx::new();
    vx.run_ok("bar = 0\nd = {}\nexec(\"def bar(): return 84\", d)\nr = d[\"bar\"]()");
    assert_eq!(vx.gi("r"), 84);
}

#[test]
fn exec_stores_into_globals() {
    // (`x = 0` first: interns `x` for the read-only resolve,
    // as above.)
    let mut vx = Vx::new();
    vx.run_ok("x = 0\nexec(\"x = 42\")");
    assert_eq!(vx.gi("x"), 42);
}

#[test]
fn exec_globals_none_means_current_locals() {
    // `builtin_exec.py`: `exec("foo", {"bar": 3}, None)`
    // raises `NameError` — globals `{"bar": 3}` has no `foo`,
    // and locals == globals there. (`foo = 11` first: interns
    // `foo` for the read-only resolve, as above.)
    let (name, args) = Vx::new().raised("foo = 11\nexec(\"foo\", {\"bar\": 3}, None)");
    assert_eq!(name, "NameError");
    assert_eq!(args.len(), 1);
}

#[test]
fn eval_syntax_error() {
    // `eval("[1,,]")` raises `SyntaxError("invalid syntax")`
    // (probed on the reference binary).
    let mut vx = Vx::new();
    let (name, args) = vx.raised("eval(\"[1,,]\")");
    assert_eq!(name, "SyntaxError");
    assert_eq!(args.len(), 1);
    assert_eq!(vx.val_str(args[0]), "invalid syntax");
}

#[test]
fn eval_non_dict_globals_is_bare_type_error() {
    // `eval("1", "foo")` raises the BARE `TypeError()`
    // (`raiseTypeErrorNull`, probed).
    let (name, args) = Vx::new().raised("eval(\"1\", \"foo\")");
    assert_eq!(name, "TypeError");
    assert!(args.is_empty(), "bare TypeError has no args: {args:?}");
}

#[test]
fn eval_arity_is_terse() {
    let mut vx = Vx::new();
    let (name, args) = vx.raised("eval()");
    assert_eq!(name, "TypeError");
    assert_eq!(args.len(), 1);
    assert_eq!(vx.val_str(args[0]), "argument num/types mismatch");
    let (name, args) = vx.raised("eval(\"1\", {}, {}, 1)");
    assert_eq!(name, "TypeError");
    assert_eq!(args.len(), 1);
    assert_eq!(vx.val_str(args[0]), "argument num/types mismatch");
}

#[test]
fn eval_non_buffer_source_is_oserror_enomem() {
    // `eval(123)` raises `OSError(12,)` on the reference
    // binary (the ignored buffer failure + the bridge's OOM
    // path — see the `builtinevex` module docs).
    let (name, args) = Vx::new().raised("eval(123)");
    assert_eq!(name, "OSError");
    assert_eq!(args, vec![Value::SmallInt(12)]);
}

#[test]
fn eval_bytes_source() {
    let mut vx = Vx::new();
    vx.run_ok("r = eval(b\"1+2\")");
    assert_eq!(vx.gi("r"), 3);
}

#[test]
fn eval_dispatch_new_name_interns() {
    // C15b: `from_strn` interns through `&self` now, so the
    // dispatch path interns a genuinely-new name instead of
    // failing with `MemoryError` (the old read-only resolve).
    // The name resolves — and the variable is unbound, so the
    // eval raises `NameError`, as on the original.
    let (name, _args) = Vx::new().raised("eval(\"zzz_new_name_xyz + 1\")");
    assert_eq!(name, "NameError");
}

#[test]
fn eval_dispatch_interned_name_works() {
    // The positive half of the read-only resolve: a name the
    // outer module load already interned resolves fine.
    let mut vx = Vx::new();
    vx.run_ok("foo = 11\nr = eval(\"foo\")");
    assert_eq!(vx.gi("r"), 11);
}

#[test]
fn compile_bad_mode_and_filename() {
    // `compile("1", "f", "zzz")` → `ValueError("bad compile
    // mode")`; `compile("1", 5, "eval")` → `TypeError("can't
    // convert to str implicitly")`; `compile("1", 5, 5)` →
    // the `ValueError` (the mode check overwrites the filename
    // error — all probed).
    let mut vx = Vx::new();
    let (name, args) = vx.raised("compile(\"1\", \"f\", \"zzz\")");
    assert_eq!(name, "ValueError");
    assert_eq!(args.len(), 1);
    assert_eq!(vx.val_str(args[0]), "bad compile mode");
    let (name, args) = vx.raised("compile(\"1\", 5, \"eval\")");
    assert_eq!(name, "TypeError");
    assert_eq!(args.len(), 1);
    assert_eq!(vx.val_str(args[0]), "can't convert to str implicitly");
    let (name, _args) = vx.raised("compile(\"1\", 5, 5)");
    assert_eq!(name, "ValueError");
}

#[test]
fn compile_valid_args_is_not_implemented() {
    // Validated args, but there is no `Obj::Code` yet — the
    // precise gap (see `compile_glue`).
    let (name, args) = Vx::new().raised("compile(\"1+2\", \"f\", \"eval\")");
    assert_eq!(name, "NotImplementedError");
    assert!(args.is_empty(), "bare NotImplementedError: {args:?}");
}

#[test]
fn compile_arity_is_terse() {
    let (name, _args) = Vx::new().raised("compile(\"1\", \"f\")");
    assert_eq!(name, "TypeError");
}

#[test]
fn eval_exec_direct_with_explicit_dicts() {
    // `Vm::eval_exec` directly (the `Vm`-level entry, with
    // full interning): distinct locals, and `exec` storing
    // into the globals dict.
    let mut vm = Vm::new();
    // Intern the names BEFORE the split (`CallCtx.pool` is a
    // shared ref and cannot intern).
    let foo_id = vm.pool.from_strn(b"foo").unwrap() as u32;
    let x_id = vm.pool.from_strn(b"x").unwrap() as u32;
    let (mut ctx, _state) = vm.split();
    let globals = empty_dict(&mut ctx.arena, &mut ctx.roots);
    let locals = empty_dict(&mut ctx.arena, &mut ctx.roots);
    dict_store(&mut ctx, Value::Heap(globals), Value::Qstr(foo_id), Value::SmallInt(11)).unwrap();
    dict_store(&mut ctx, Value::Heap(locals), Value::Qstr(foo_id), Value::SmallInt(3)).unwrap();
    // Pin both dicts (they are not reachable from any root yet
    // — `eval_exec` pins them again, but the const-table
    // conversion inside registration allocates first).
    ctx.roots.extra.push(Value::Heap(globals));
    ctx.roots.extra.push(Value::Heap(locals));
    drop(ctx);
    let r = vm
        .eval_exec(b"foo", InputKind::EvalInput, globals, locals)
        .expect("eval with distinct locals");
    assert_eq!(r, Value::SmallInt(3));
    let r = vm
        .eval_exec(b"x = 42", InputKind::FileInput, globals, globals)
        .expect("exec stores into globals");
    assert_eq!(r, Value::None);
    let (mut ctx, _state) = vm.split();
    let x = dict_get_value(&mut ctx, Value::Heap(globals), Value::Qstr(x_id)).unwrap();
    assert_eq!(x, Value::SmallInt(42));
}

#[test]
fn eval_buffer_source_via_ctx_path() {
    // `source_bytes` through the dispatch path, with a
    // hand-built `bytearray` value (no type call needed — the
    // `bytearray` MakeNew adapter is C13b's): `eval` of a
    // bytearray source returns the body value.
    use crate::objects::value::{ArrayData, Obj};
    let mut vm = Vm::new();
    let (mut ctx, state) = vm.split();
    let bytes_val = {
        // Header `None`: `buffer_bytes` reads the payload, not
        // the header type (`BuiltinTypes` has no `bytearray`
        // field — C9b).
        let r: GcRef = ctx.arena.alloc(
            None,
            Obj::Array(ArrayData {
                typecode: b'B',
                free: 0,
                len: 3,
                data: b"3+4".to_vec(),
                source: None,
            }),
            &ctx.roots,
        );
        Value::Heap(r)
    };
    let r = call_eval_exec_ctx(
        &mut ctx,
        state,
        InputKind::EvalInput,
        1,
        0,
        &[bytes_val],
    )
    .expect("eval of a bytearray source");
    assert_eq!(r, Value::SmallInt(7));
    // A non-buffer source through the same path raises
    // `OSError(12)` (the `Vm`-level module test covers the
    // same case through dispatch).
    let r = call_eval_exec_ctx(
        &mut ctx,
        state,
        InputKind::EvalInput,
        1,
        0,
        &[Value::SmallInt(123)],
    );
    match r {
        Err(exc) => {
            let t = obj_get_type(&ctx.arena, &ctx.types, exc).expect("exc type");
            match ctx.arena.get(t) {
                Obj::Type(d) => {
                    let name = String::from_utf8_lossy(ctx.pool.qstr_str(d.name as u16));
                    assert_eq!(name, "OSError");
                }
                _ => panic!("not a type"),
            }
        }
        Ok(v) => panic!("expected OSError, got {v:?}"),
    }
}

#[test]
fn compile_ctx_path_validates() {
    // `call_compile_ctx` directly: valid args reach the
    // `Obj::Code` gap (`NotImplementedError`), a bad mode the
    // `ValueError`, too few args the terse `TypeError`.
    let mut vm = Vm::new();
    // Intern the names BEFORE the split (`CallCtx.pool` is a
    // shared ref and cannot intern).
    let f_id = vm.pool.from_strn(b"f").unwrap() as u32;
    let eval_id = vm.pool.from_strn(b"eval").unwrap() as u32;
    let (mut ctx, state) = vm.split();
    let src = {
        let r: GcRef = ctx.arena.alloc(
            Some(ctx.types.str_),
            Obj::Str(crate::objects::value::StrData { bytes: b"1+2".to_vec() }),
            &ctx.roots,
        );
        Value::Heap(r)
    };
    let fname = Value::Qstr(f_id);
    let mode = Value::Qstr(eval_id);
    let r = call_compile_ctx(&mut ctx, state, 3, 0, &[src, fname, mode]);
    match r {
        Err(exc) => {
            let t = obj_get_type(&ctx.arena, &ctx.types, exc).expect("exc type");
            match ctx.arena.get(t) {
                Obj::Type(d) => {
                    let name = String::from_utf8_lossy(ctx.pool.qstr_str(d.name as u16));
                    assert_eq!(name, "NotImplementedError");
                }
                _ => panic!("not a type"),
            }
        }
        Ok(v) => panic!("expected NotImplementedError, got {v:?}"),
    }
}
