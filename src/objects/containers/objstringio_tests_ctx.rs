//! Lane C18b tests (CallCtx level): the `io.StringIO` /
//! `io.BytesIO` / `io.IOBase` protocol fns and the `IOBase`
//! delegation (vendor `py/objstringio.c` + `py/modio.c`).
//!
//! These are the paths a snippet cannot reach: a closed
//! `SEEK_END`, a method returning `None` / a negative int / a
//! non-int, and a subclass resolving its protocol through the
//! MRO. The end-to-end snippets live in
//! `objstringio_tests.rs`; the harness here is a minimal copy
//! of its `Fx` (the tree's per-file harness pattern).
use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objstringio::{
    iobase_make_new, stringio_getvalue, stringio_ioctl, stringio_make_new, stringio_print,
};
use crate::objects::containers::objstringio_iobase::{iobase_ioctl, iobase_read, iobase_write};
use crate::objects::containers::objstringio_types::build_io_module;
use crate::objects::objtype::make_new::iobase_make_new_slot;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{IoctlArg, PrintKind, SeekState, StreamP};
use crate::objects::strings::objstr_a::new_str;
use crate::objects::value::{
    DictData, FunBuiltinData, GcRef, MapData, Obj, StringIoData, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::Vm;
use crate::support::stream::{MP_STREAM_CLOSE, MP_STREAM_SEEK, SEEK_END, SEEK_SET};

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
    fn io_type(&mut self, name: &str) -> GcRef {
        let io_id = static_qstr_id(b"io").expect("io is a static qstr");
        let module = self
            .vm
            .state
            .builtin_modules
            .iter()
            .find(|(q, _)| *q == io_id)
            .map(|(_, v)| *v)
            .expect("io registered");
        let dict = match module {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Module(m) => m.dict,
                _ => panic!("io is not a module"),
            },
            _ => panic!("io is not heap"),
        };
        let name_q = self.vm.pool.from_strn(name.as_bytes()).expect("intern") as u32;
        let (mut ctx, _state) = self.vm.split();
        let v = dict_get_value(&mut ctx, Value::Heap(dict), Value::Qstr(name_q))
            .expect("type present");
        match v {
            Value::Heap(r) => r,
            _ => panic!("type is not heap"),
        }
    }
}
/// The recorded raise after a `PyRaise` return: (name, message).
fn pending(ctx: &CallCtx) -> (String, String) {
    let pr = ctx.pending_raise.as_ref().expect("raise recorded");
    (
        pr.exc_name.to_string(),
        pr.message.clone().unwrap_or_default(),
    )
}

#[test]
fn closed_seek_end_is_value_error() {
    // The reference SEGFAULTS here (NULL `vstr` deref,
    // `objstringio.c:124`); Rust raises instead.
    let mut fx = Fx::new();
    let stringio_type = fx.io_type("StringIO");
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let arg = new_str(&mut ctx, types, b"ab");
    let obj = stringio_make_new(&mut ctx, stringio_type, &[arg], 0).expect("make_new");
    assert_eq!(
        stringio_ioctl(&mut ctx, obj, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    let mut state = SeekState::new(0, SEEK_END);
    let err = stringio_ioctl(
        &mut ctx,
        obj,
        MP_STREAM_SEEK,
        IoctlArg::Seek(&mut state),
    );
    assert!(matches!(err, Err(PyError::PyRaise)));
    assert_eq!(
        pending(&ctx),
        (
            "ValueError".to_string(),
            "I/O operation on closed file".to_string()
        )
    );
}

#[test]
fn iobase_missing_readinto_is_attribute_error() {
    let mut fx = Fx::new();
    let iobase_type = fx.io_type("IOBase");
    let (mut ctx, _state) = fx.vm.split();
    let obj = iobase_make_new(&mut ctx, iobase_type, &[], 0).expect("make_new");
    let mut buf = [0u8; 4];
    let err = iobase_read(&mut ctx, obj, &mut buf);
    assert!(matches!(err, Err(PyError::PyRaise)));
    assert_eq!(
        pending(&ctx),
        (
            "AttributeError".to_string(),
            "no attribute 'readinto'".to_string()
        )
    );
}

/// A `readinto` override that writes `b"xy"` into the
/// bytearray and returns 2. Args are `[self, bytearray]`
/// (the method lookup binds `self`).
fn readinto_write_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let Value::Heap(r) = args[1] else {
        return Err(PyError::PyRaise);
    };
    if let Obj::Array(a) = ctx.arena.get_mut(r) {
        a.data[..2].copy_from_slice(b"xy");
    }
    Ok(Value::SmallInt(2))
}

/// A `readinto` override returning `None` → `OSError(EAGAIN)`.
fn readinto_none_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::None)
}

/// A `readinto` override returning `-5` → `OSError(5)`.
fn readinto_neg_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(-5))
}

/// A `readinto` override returning a `str` → `TypeError`.
fn readinto_str_fn(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(new_str(ctx, ctx.types, b"x"))
}

/// A `write` override returning the byte count when the
/// content is exactly `b"hello"`, else 0 — so the test
/// observes the bytes the method received (the raw return is
/// clamped to the buffer size by `iobase_read_write`, so a
/// content-derived count is the observable channel).
fn write_check_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let ok = match args[1] {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Array(a) => a.data[..a.len] == *b"hello",
            _ => false,
        },
        _ => false,
    };
    Ok(Value::SmallInt(if ok { 5 } else { 0 }))
}

/// An `ioctl` override returning the request int it was
/// given. Args are `[self, request, arg]`.
fn ioctl_req_fn(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[1])
}

/// A fake `IOBase`-protocol type whose locals dict holds one
/// override method.
fn fake_type(
    ctx: &mut CallCtx,
    fun_var: GcRef,
    name: &str,
    method: &[u8],
    n: u16,
    f: crate::objects::slots::BuiltinFn,
) -> GcRef {
    let id = static_qstr_id(method).expect("method name is a static qstr");
    let fun = ctx.alloc(
        Some(fun_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(n, n),
            func: f,
            kw_func: None,
        }),
    );
    let dict = ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![(Value::Qstr(id), Value::Heap(fun))]),
        }),
    );
    let name_id = ctx.pool.from_strn(name.as_bytes()).expect("intern") as u32;
    ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(TypeData {
            name: name_id,
            flags: 0,
            base: None,
            mro: Vec::new(),
            dict,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
            attr: None,
            make_new: Some(iobase_make_new_slot),
            bases: None,
            namedtuple_fields: None,
            protocol: Some(StreamP::new(
                Some(iobase_read),
                Some(iobase_write),
                Some(iobase_ioctl),
                false,
            )),
        }),
    )
}

fn fake_instance(ctx: &mut CallCtx, fake: GcRef) -> Value {
    Value::Heap(ctx.alloc(
        Some(fake),
        Obj::StringIo(StringIoData {
            buf: Vec::new(),
            pos: 0,
            closed: false,
        }),
    ))
}

#[test]
fn iobase_readinto_delegates_to_method() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"readinto", 2, readinto_write_fn);
    let obj = fake_instance(&mut ctx, fake);
    let mut buf = [0u8; 4];
    assert_eq!(iobase_read(&mut ctx, obj, &mut buf), Ok(2));
    assert_eq!(&buf[..2], b"xy");
}

#[test]
fn iobase_readinto_none_is_eagain() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"readinto", 2, readinto_none_fn);
    let obj = fake_instance(&mut ctx, fake);
    let mut buf = [0u8; 4];
    let err = iobase_read(&mut ctx, obj, &mut buf);
    assert!(matches!(err, Err(PyError::PyRaise)));
    // `OSError(11)` — recorded as a value raise, so the
    // pending slot holds the name with NO message; the arg
    // (the errno) is what the VM builds the exception from.
    let pr = ctx.pending_raise.as_ref().expect("raise recorded");
    assert_eq!(pr.exc_name, "OSError");
    assert_eq!(pr.arg, Some(Value::SmallInt(11)));
}

#[test]
fn iobase_readinto_negative_is_oserror() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"readinto", 2, readinto_neg_fn);
    let obj = fake_instance(&mut ctx, fake);
    let mut buf = [0u8; 4];
    let err = iobase_read(&mut ctx, obj, &mut buf);
    assert!(matches!(err, Err(PyError::PyRaise)));
    let pr = ctx.pending_raise.as_ref().expect("raise recorded");
    assert_eq!(pr.exc_name, "OSError");
    assert_eq!(pr.arg, Some(Value::SmallInt(5)));
}

#[test]
fn iobase_readinto_str_is_type_error() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"readinto", 2, readinto_str_fn);
    let obj = fake_instance(&mut ctx, fake);
    let mut buf = [0u8; 4];
    let err = iobase_read(&mut ctx, obj, &mut buf);
    assert!(matches!(err, Err(PyError::PyRaise)));
    assert_eq!(
        pending(&ctx),
        (
            "TypeError".to_string(),
            "can't convert to int".to_string()
        )
    );
}

#[test]
fn iobase_write_delegates_to_method() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"write", 2, write_check_fn);
    let obj = fake_instance(&mut ctx, fake);
    assert_eq!(iobase_write(&mut ctx, obj, b"hello"), Ok(5));
    assert_eq!(iobase_write(&mut ctx, obj, b"world"), Ok(0));
}

#[test]
fn iobase_ioctl_delegates_to_method() {
    let mut fx = Fx::new();
    let fun_var = fx.vm.state.vm_types.fun_builtin_var;
    let (mut ctx, _state) = fx.vm.split();
    let fake = fake_type(&mut ctx, fun_var, "Fake", b"ioctl", 3, ioctl_req_fn);
    let obj = fake_instance(&mut ctx, fake);
    let mut state = SeekState::new(0, SEEK_SET);
    assert_eq!(
        iobase_ioctl(&mut ctx, obj, MP_STREAM_SEEK, IoctlArg::Seek(&mut state)),
        Ok(2)
    );
}

#[test]
fn iobase_ioctl_missing_is_attribute_error() {
    let mut fx = Fx::new();
    let iobase_type = fx.io_type("IOBase");
    let (mut ctx, _state) = fx.vm.split();
    let obj = iobase_make_new(&mut ctx, iobase_type, &[], 0).expect("make_new");
    let mut state = SeekState::new(0, SEEK_SET);
    let err = iobase_ioctl(
        &mut ctx,
        obj,
        MP_STREAM_SEEK,
        IoctlArg::Seek(&mut state),
    );
    assert!(matches!(err, Err(PyError::PyRaise)));
    assert_eq!(
        pending(&ctx),
        (
            "AttributeError".to_string(),
            "no attribute 'ioctl'".to_string()
        )
    );
}

/// A hand-built subclass type of `base`: `protocol None`,
/// `mro [sub, base, object]` — the shape a user class gets,
/// minus the `make_new` inheritance this lane does not own
/// (C12a's `typecreate` always installs `instance_make_new`).
fn subclass_type(
    ctx: &mut CallCtx,
    base: GcRef,
    name: &str,
) -> GcRef {
    let dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
    let name_id = ctx.pool.from_strn(name.as_bytes()).expect("intern") as u32;
    let sub = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(TypeData {
            name: name_id,
            flags: 0,
            base: Some(base),
            mro: Vec::new(),
            dict,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
            attr: None,
            make_new: None,
            bases: None,
            namedtuple_fields: None,
            protocol: None,
        }),
    );
    if let Obj::Type(d) = ctx.arena.get_mut(sub) {
        d.mro = vec![sub, base, ctx.types.object];
    }
    sub
}

#[test]
fn stringio_subclass_mro_print_getvalue() {
    // A subclass instance prints / getvalues as its base
    // (probed on the reference: `class S(io.StringIO)` →
    // `<io.StringIO …>` / `str`). The protocol resolves
    // through the MRO (`get_stream`'s walk).
    let mut fx = Fx::new();
    let stringio_type = fx.io_type("StringIO");
    let bytesio_type = fx.io_type("BytesIO");
    let (mut ctx, _state) = fx.vm.split();
    let sub = subclass_type(&mut ctx, stringio_type, "S");
    let types = ctx.types;
    let arg = new_str(&mut ctx, types, b"hi");
    let obj = stringio_make_new(&mut ctx, sub, &[arg], 0).expect("subclass payload");
    let v = stringio_getvalue(&mut ctx, &[obj]).expect("getvalue");
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => assert_eq!(s.bytes, b"hi"),
            other => panic!("getvalue is not str: {other:?}"),
        },
        other => panic!("getvalue is not heap: {other:?}"),
    }
    let mut s = String::new();
    stringio_print(&mut ctx, obj, PrintKind::Str, &mut s);
    assert!(s.starts_with("<io.StringIO 0x"), "print was {s}");
    // The BytesIO side: a subclass prints as BytesIO and its
    // getvalue is bytes.
    let sub_b = subclass_type(&mut ctx, bytesio_type, "B");
    let types_b = ctx.types;
    let arg_b = new_str(&mut ctx, types_b, b"hi");
    let obj_b = stringio_make_new(&mut ctx, sub_b, &[arg_b], 0).expect("subclass payload");
    let v_b = stringio_getvalue(&mut ctx, &[obj_b]).expect("getvalue");
    match v_b {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Bytes(b) => assert_eq!(b.bytes, b"hi"),
            other => panic!("getvalue is not bytes: {other:?}"),
        },
        other => panic!("getvalue is not heap: {other:?}"),
    }
    let mut s_b = String::new();
    stringio_print(&mut ctx, obj_b, PrintKind::Str, &mut s_b);
    assert!(s_b.starts_with("<io.BytesIO 0x"), "print was {s_b}");
}



