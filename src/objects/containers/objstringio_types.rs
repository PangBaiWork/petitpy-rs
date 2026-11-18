//! The `io` stream types and the `io` module object
//! (vendor `py/objstringio.c` + `py/modio.c`): the 11-entry
//! `STRINGIO_LOCALS` table, the three `TypeData` builders and
//! `build_io_module` (see `objstringio.rs` for the protocol
//! fns and `objstringio_iobase.rs` for the `IOBase`
//! delegation).

use super::objstringio::{
    identity_builtin, stringio_getvalue, stringio_ioctl, stringio_print, stringio_read,
    stringio_write,
};
use super::objstringio_iobase::{iobase_ioctl, iobase_read, iobase_write};
use crate::memory::gc::CallCtx;
use crate::objects::obj::FLAG_ITER_IS_STREAM;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{MethodEntry, PrintKind, StreamP};
use crate::objects::value::{DictData, GcRef, MapData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm_types::locals_dict_from_entries;
use crate::support::stream_methods::{
    stream_close_builtin, stream_exit_builtin, stream_flush_builtin, stream_read_builtin,
    stream_readinto_builtin, stream_seek_builtin, stream_tell_builtin,
    stream_unbuffered_readline_builtin, stream_write_builtin,
};
// ── types + module ──────────────────────────────────────────────────

/// The 11-entry `stringio_locals_dict_table`
/// (`objstringio.c:217-229`), in the C's order. The eight
/// stream methods are C16's `STREAM_METHODS` bodies (shared
/// with every stream type); `getvalue` is this lane's;
/// `__enter__` is `mp_identity_obj`; `__exit__` is C16's
/// `mp_stream___exit___obj`. NOTE the absences vs the
/// 14-entry `STREAM_METHODS`: no `read1` / `write1` /
/// `readinto1` / `readlines` / `ioctl` — probed 2026-11-18,
/// all `AttributeError` on a `StringIO`.
pub const STRINGIO_LOCALS: &[MethodEntry] = &[
    MethodEntry {
        name: "read",
        func: stream_read_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "readinto",
        func: stream_readinto_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    MethodEntry {
        name: "readline",
        func: stream_unbuffered_readline_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    MethodEntry {
        name: "write",
        func: stream_write_builtin,
        min: 2,
        max: 4,
        kw_func: None,
    },
    MethodEntry {
        name: "seek",
        func: stream_seek_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    MethodEntry {
        name: "tell",
        func: stream_tell_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "flush",
        func: stream_flush_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "close",
        func: stream_close_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "getvalue",
        func: stringio_getvalue,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "__enter__",
        func: identity_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    MethodEntry {
        name: "__exit__",
        func: stream_exit_builtin,
        min: 4,
        max: 4,
        kw_func: None,
    },
];

fn stringio_type_data_inner(
    name: &[u8],
    dict: GcRef,
    is_text: bool,
    make_new: crate::objects::slots::MakeNewFn,
) -> TypeData {
    TypeData {
        name: static_qstr_id(name).expect("StringIO/BytesIO name is a static qstr"),
        flags: FLAG_ITER_IS_STREAM,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        // No `getiter` slot: `FLAG_ITER_IS_STREAM` makes the
        // getiter dispatch return the stream itself (C11/C16).
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx,
             value: Value,
             kind: PrintKind,
             sink: &mut dyn std::fmt::Write| {
                stringio_print(ctx, value, kind, sink)
            },
        ),
        attr: None,
        make_new: Some(make_new),
        bases: None,
        namedtuple_fields: None,
        protocol: Some(StreamP::new(
            Some(stringio_read),
            Some(stringio_write),
            Some(stringio_ioctl),
            is_text,
        )),
    }
}

/// `mp_type_stringio` (`objstringio.c:241-250`): flags
/// `FLAG_ITER_IS_STREAM`; slots make_new/print/protocol/
/// locals_dict; `is_text = true`.
pub fn stringio_type_data(dict: GcRef) -> TypeData {
    stringio_type_data_inner(
        b"StringIO",
        dict,
        true,
        crate::objects::objtype::make_new::stringio_make_new_slot,
    )
}

/// `mp_type_bytesio` (`objstringio.c:252-264`): same slots as
/// `StringIO`, `is_text = false`.
pub fn bytesio_type_data(dict: GcRef) -> TypeData {
    stringio_type_data_inner(
        b"BytesIO",
        dict,
        false,
        crate::objects::objtype::make_new::stringio_make_new_slot,
    )
}

/// `mp_type_iobase` (`modio.c:99-107`): flags `NONE` (no
/// `FLAG_ITER_IS_STREAM`), NO print slot (default print
/// `<IOBase>`), NO locals dict (an empty one — every method
/// lookup is `AttributeError`, probed), slots make_new/
/// protocol only; `is_text = false`.
pub fn iobase_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"IOBase").expect("IOBase is a static qstr"),
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
        make_new: Some(crate::objects::objtype::make_new::iobase_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: Some(StreamP::new(
            Some(iobase_read),
            Some(iobase_write),
            Some(iobase_ioctl),
            false,
        )),
    }
}

/// Build the `io` module object (`modio.c:206-230`):
/// globals `{__name__: "io", open, IOBase, StringIO,
/// BytesIO}` in the C's table order. `open` is D's `file_io`
/// (`mp_builtin_open_obj`); the caller passes it in (tests
/// pass a stub). The `BufferedWriter` entry is dead under
/// full (`MICROPY_PY_IO_BUFFEREDWRITER` is off,
/// `mpconfigport.h:375` — probed: no such attribute).
/// Registration into `VmState.builtin_modules` is D1's, not
/// this builder's. The module and its types are pushed to
/// `ctx.roots` by `new_module` / the allocations below
/// (the `new_module` pattern).
pub fn build_io_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
    open_builtin: Value,
) -> PyResult<Value> {
    use crate::objects::containers::objdict::dict_store;
    use crate::objects::objtype::objmodule::new_module;

    let io_name = static_qstr_id(b"io").expect("io is a static qstr");
    let module = new_module(ctx, io_name, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    // One shared locals dict for both stream types (the C
    // uses the same `stringio_locals_dict` for both).
    let locals = locals_dict_from_entries(ctx, fun_builtin_var, None, STRINGIO_LOCALS, &[], &[]);
    let stringio = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(stringio_type_data(locals)),
    );
    let bytesio = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(bytesio_type_data(locals)),
    );
    let iobase_dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
    let iobase = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(iobase_type_data(iobase_dict)),
    );
    ctx.roots.push(Value::Heap(stringio));
    ctx.roots.push(Value::Heap(bytesio));
    ctx.roots.push(Value::Heap(iobase));
    let dict_v = Value::Heap(dict);
    for (name, value) in [
        (b"open".as_slice(), open_builtin),
        (b"IOBase".as_slice(), Value::Heap(iobase)),
        (b"StringIO".as_slice(), Value::Heap(stringio)),
        (b"BytesIO".as_slice(), Value::Heap(bytesio)),
    ] {
        let id = static_qstr_id(name).expect("io member name is a static qstr");
        dict_store(ctx, dict_v, Value::Qstr(id), value)
            .map_err(|e| ctx.record_typed(e))?;
    }
    Ok(module)
}
