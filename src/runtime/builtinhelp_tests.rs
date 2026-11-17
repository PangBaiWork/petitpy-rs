//! Lane C18b tests: `help()` (vendor `py/builtinhelp.c`).
//!
//! The pure string builders (`print_modules`, `print_obj`)
//! are asserted byte-for-byte against the reference binary's
//! output (probed 2026-11-18); the interception is covered
//! end-to-end through `Vm` snippets (the stdout bytes
//! themselves cannot be captured in-process — the builders
//! are the same code `call_help_ctx` prints).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::objects::primitives::objint::fun_n_args;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::builtinhelp::{
    call_help_ctx, is_modules_arg, print_modules, print_obj, HELP_DEFAULT_TEXT,
};
use crate::runtime::frozen_modules::FrozenTable;
use crate::runtime::vm::{Vm, VmError};

/// The `open` stub for the `io` module table (see
/// `objstringio_tests.rs` — never called here).
fn open_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        // Register `io` (for the module help test) the way
        // D1 will — see `objstringio_tests.rs::setup_io`.
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

    fn run_ok(&mut self, src: &str) {
        match self.run(src) {
            Ok(_) => {}
            Err(VmError::Raised(exc)) => {
                let name = self.exc_name(exc);
                let args = self.exc_args(exc);
                let msg = match args.first() {
                    Some(v) => self.val_str(*v),
                    None => String::new(),
                };
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
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

    fn exc_args(&mut self, exc: Value) -> Vec<Value> {
        match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => d.args.clone(),
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        }
    }

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

    fn gs(&mut self, name: &str) -> String {
        match self.g(name) {
            Some(v) => self.val_str(v),
            None => panic!("global {name} is absent"),
        }
    }
}

/// (type name, first arg as a string) for an `Err(Value)` from
/// a `call_*_ctx` helper.
fn err_name_msg(fx: &mut Fx, exc: Value) -> (String, String) {
    let name = fx.exc_name(exc);
    let args = fx.exc_args(exc);
    let msg = match args.first() {
        Some(v) => fx.val_str(*v),
        None => String::new(),
    };
    (name, msg)
}

#[test]
fn help_default_text_verbatim() {
    assert_eq!(
        HELP_DEFAULT_TEXT,
        "Welcome to MicroPython!\n\nFor online docs please visit http://docs.micropython.org/\n\nControl commands:\n  CTRL-A        -- on a blank line, enter raw REPL mode\n  CTRL-B        -- on a blank line, enter normal REPL mode\n  CTRL-C        -- interrupt a running program\n  CTRL-D        -- on a blank line, exit or do a soft reset\n  CTRL-E        -- on a blank line, enter paste mode\n\nFor further help on a specific object, type help(obj)\n"
    );
}

#[test]
fn help_no_args_returns_none() {
    let mut fx = Fx::new();
    let (mut ctx, mut state) = fx.vm.split();
    let r = call_help_ctx(&mut ctx, &mut state, 0, 0, &[]);
    assert_eq!(r, Ok(Value::None));
}

#[test]
fn help_two_args_type_error() {
    let mut fx = Fx::new();
    let (mut ctx, mut state) = fx.vm.split();
    let arg = Value::SmallInt(1);
    let r = call_help_ctx(&mut ctx, &mut state, 2, 0, &[arg, arg]);
    let err = r.expect_err("help(1, 2) must raise");
    assert_eq!(
        err_name_msg(&mut fx, err),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}

#[test]
fn help_modules_list_columns() {
    let mut fx = Fx::new();
    let (mut ctx, state) = fx.vm.split();
    // Fake registrations: two builtin, one extensible, two
    // frozen — the output is the sorted union, column-first.
    let io_id = static_qstr_id(b"io").expect("io is a static qstr");
    let sys_id = static_qstr_id(b"sys").expect("sys is a static qstr");
    let math_id = static_qstr_id(b"math").expect("math is a static qstr");
    state.builtin_modules.retain(|(q, _)| *q != io_id);
    state.builtin_modules.push((sys_id, Value::None));
    state.builtin_modules.push((math_id, Value::None));
    state.extensible_modules.push((io_id, Value::None));
    state.frozen_modules = FrozenTable::from_entries(vec![
        ("zebra".to_string(), Vec::new()),
        ("apple".to_string(), Vec::new()),
    ]);
    let mut out = String::new();
    print_modules(&mut ctx, &state, &mut out);
    // Sorted: apple, io, math, sys, zebra. Rows = ceil(5/4)
    // = 2; row 0: apple, math, zebra (j = 0, 2, 4); row 1:
    // io, sys (j = 1, 3).
    assert_eq!(
        out,
        "apple             math              zebra\nio                sys\n"
    );
}

#[test]
fn help_modules_empty() {
    let mut fx = Fx::new();
    let (mut ctx, state) = fx.vm.split();
    state.builtin_modules.clear();
    state.extensible_modules.clear();
    let mut out = String::new();
    print_modules(&mut ctx, &state, &mut out);
    assert_eq!(out, "");
}

#[test]
fn help_obj_int() {
    let mut fx = Fx::new();
    let (mut ctx, _state) = fx.vm.split();
    let mut out = String::new();
    print_obj(&mut ctx, Value::SmallInt(1), &mut out);
    assert_eq!(
        out,
        "object 1 is of type int\n  from_bytes -- <classmethod>\n  to_bytes -- <function>\n"
    );
}

#[test]
fn help_obj_str() {
    let mut fx = Fx::new();
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let s = crate::objects::strings::objstr_a::new_str(&mut ctx, types, b"");
    let mut out = String::new();
    print_obj(&mut ctx, s, &mut out);
    // The empty string prints as nothing, so the C's
    // `"object " + obj + " is of type …"` has TWO spaces
    // (verified against the reference binary with `cat -A`).
    assert!(out.starts_with("object  is of type str\n"), "got: {out}");
    assert!(out.contains("  find -- <function>\n"), "got: {out}");
    assert!(out.contains("  zfill -- <function>\n"), "got: {out}");
}

#[test]
fn help_obj_function() {
    let mut fx = Fx::new();
    let (mut ctx, state) = fx.vm.split();
    let builtins_v = Value::Heap(state.builtins);
    let print_q = static_qstr_id(b"print").expect("print is a static qstr");
    let print_v = dict_get_value(&mut ctx, builtins_v, Value::Qstr(print_q))
        .expect("print is bound");
    let mut out = String::new();
    print_obj(&mut ctx, print_v, &mut out);
    assert_eq!(out, "object <function> is of type function\n");
}

#[test]
fn help_obj_type() {
    let mut fx = Fx::new();
    let (mut ctx, _state) = fx.vm.split();
    let int_v = Value::Heap(ctx.types.int);
    let mut out = String::new();
    print_obj(&mut ctx, int_v, &mut out);
    assert_eq!(
        out,
        "object <class 'int'> is of type type\n  from_bytes -- <classmethod>\n  to_bytes -- <function>\n"
    );
}

#[test]
fn help_obj_io_module() {
    let mut fx = Fx::new();
    let io_id = static_qstr_id(b"io").expect("io is a static qstr");
    let module = fx
        .vm
        .state
        .builtin_modules
        .iter()
        .find(|(q, _)| *q == io_id)
        .map(|(_, v)| *v)
        .expect("io registered");
    let (mut ctx, _state) = fx.vm.split();
    let mut out = String::new();
    print_obj(&mut ctx, module, &mut out);
    assert_eq!(
        out,
        "object <module 'io'> is of type module\n  __name__ -- io\n  open -- <function>\n  IOBase -- <class 'IOBase'>\n  StringIO -- <class 'StringIO'>\n  BytesIO -- <class 'BytesIO'>\n"
    );
}

#[test]
fn help_modules_arg_forms() {
    let mut fx = Fx::new();
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let q = static_qstr_id(b"modules").expect("modules is a static qstr");
    assert!(is_modules_arg(&mut ctx, Value::Qstr(q)));
    let heap = crate::objects::strings::objstr_a::new_str(&mut ctx, types, b"modules");
    assert!(is_modules_arg(&mut ctx, heap));
    let heap_sp =
        crate::objects::strings::objstr_a::new_str(&mut ctx, types, b"modules ");
    assert!(!is_modules_arg(&mut ctx, heap_sp));
    let bytes =
        crate::objects::strings::objstr_a::new_bytes(&mut ctx, types, b"modules");
    assert!(!is_modules_arg(&mut ctx, bytes));
    assert!(!is_modules_arg(&mut ctx, Value::SmallInt(1)));
}

#[test]
fn help_e2e() {
    let mut fx = Fx::new();
    fx.run_ok("help(1)\n");
    fx.run_ok("help()\n");
    fx.run_ok("help('modules')\n");
    fx.run_ok("help('modules ')\n");
}

#[test]
fn int_to_bytes_e2e_after_classmethod_wrap() {
    // The passing half of the `int_locals_dict` fix: `to_bytes`
    // is a plain builtin, looked up on an int INSTANCE.
    let mut fx = Fx::new();
    fx.run_ok(
        "a2 = (255).to_bytes(2, 'big')\n\
         a3 = a2 == b'\\x00\\xff'\n",
    );
    assert!(fx.gb("a3"));
}

#[test]
#[ignore = "blocked on the type-object attribute lookup gap (same class as C13b's ignored `makenew_class_attr_on_class`): the VM's LOAD_METHOD uses `load_method_maybe_ctx`, which looks the attr up in `type`'s dict instead of the type's OWN dict and never consults the generic `TypeData.attr` slot. The original routes this through the `type` type's attr slot (`mp_obj_type_attr`); fixing it needs that port + switching VM dispatch to C14's loader. The wrap itself is verified at CallCtx level by `int_from_bytes_classmethod_binding` in `tests_ops.rs`."]
fn int_from_bytes_e2e_after_classmethod_wrap() {
    // The blocked half: `int.from_bytes` is a classmethod
    // object (the original's `MpObjStaticClassMethod`) — the
    // call and the prints must match the reference
    // (`<classmethod>`), once type-object lookup lands.
    let mut fx = Fx::new();
    fx.run_ok(
        "a0 = int.from_bytes(b'\\x01\\x00', 'big')\n\
         a1 = int.from_bytes(b'\\x01\\x00', 'little')\n\
         a4 = str(type(int.from_bytes))\n\
         a5 = str(int.from_bytes)\n\
         a6 = str(int.to_bytes)\n",
    );
    assert_eq!(fx.gi("a0"), 256);
    assert_eq!(fx.gi("a1"), 1);
    assert_eq!(fx.gs("a4"), "<class 'classmethod'>");
    assert_eq!(fx.gs("a5"), "<classmethod>");
    assert_eq!(fx.gs("a6"), "<function>");
}
