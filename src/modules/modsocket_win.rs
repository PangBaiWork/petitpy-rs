//! The `socket` module on Windows: a stub.
//!
//! The real module (`modsocket.rs`, Unix-only) is built on
//! `std::os::unix::net::{UnixListener, UnixStream}` and the
//! raw-fd socket table — none of which exists on Windows.
//! A Windows AF_UNIX port would need raw Winsock FFI
//! (`AF_UNIX` exists since Windows 10 1803, but `std` has
//! no safe wrapper for it), which is out of scope for this
//! port. The module still exists so `import socket` and the
//! constants (`AF_UNIX`, `SOCK_STREAM`, ...) resolve, but
//! `socket.socket(...)` and `socket.fromfd(...)` raise
//! `NotImplementedError`. The type is registered in
//! `BuiltinTypes.socket` like the real one, so the VM's
//! socket-type slot stays populated.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::BuiltinFn;
use crate::objects::value::{FunBuiltinData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm_types::locals_dict_from_entries;

/// A small int, infallible for socket constants (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket constant fits the small-int window")
}

/// `socket.socket(...)` / `socket.fromfd(...)` on Windows.
fn socket_not_impl(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.record_raise(
        "NotImplementedError",
        Some("socket is not implemented on Windows"),
    );
    Err(PyError::PyRaise)
}

/// Build the `socket` module object (same shape as the real
/// `build_socket_module`: module + socket type; the type has
/// an empty locals dict — no instances can exist to call
/// methods on — and the module carries the constructor stubs
/// plus the same constants).
pub fn build_socket_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, GcRef)> {
    let socket_id = static_qstr_id(b"socket").expect("socket is a static qstr");
    let module = new_module(ctx, socket_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let locals = locals_dict_from_entries(ctx, fun_builtin_var, None, &[], &[], &[]);
    let socket_type = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(TypeData {
            name: socket_id,
            flags: 0,
            base: None,
            mro: Vec::new(),
            dict: locals,
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
    ctx.roots.push(Value::Heap(socket_type));
    let ctor = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: socket_id,
            n_args: fun_n_args(2, 3),
            func: socket_not_impl as BuiltinFn,
            kw_func: None,
        }),
    );
    let fromfd_id = static_qstr_id(b"fromfd").expect("fromfd is a static qstr");
    let fromfd = ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: fromfd_id,
            n_args: fun_n_args(3, 4),
            func: socket_not_impl as BuiltinFn,
            kw_func: None,
        }),
    );
    ctx.roots.push(Value::Heap(ctor));
    ctx.roots.push(Value::Heap(fromfd));
    dict_store(ctx, dict_v, Value::Qstr(socket_id), Value::Heap(ctor))
        .map_err(|e| ctx.record_typed(e))?;
    dict_store(ctx, dict_v, Value::Qstr(fromfd_id), Value::Heap(fromfd))
        .map_err(|e| ctx.record_typed(e))?;
    for (name, v) in [
        (b"AF_UNIX".as_slice(), 1i64),
        (b"AF_INET".as_slice(), 2),
        (b"AF_INET6".as_slice(), 10),
        (b"SOCK_STREAM".as_slice(), 1),
        (b"SOCK_DGRAM".as_slice(), 2),
        (b"SOL_SOCKET".as_slice(), 1),
        (b"SO_REUSEADDR".as_slice(), 2),
        (b"SO_PEERCRED".as_slice(), 17),
    ] {
        let id = static_qstr_id(name).expect("socket constant name is a static qstr");
        dict_store(ctx, dict_v, Value::Qstr(id), small(v)).map_err(|e| ctx.record_typed(e))?;
    }
    Ok((module, socket_type))
}
