//! The VM's builtin functions — `print` / `next` /
//! `__build_class__` and the `str.format` kwargs wrapper
//! (C13). Split out of `vm.rs` (file-size cap).

use crate::memory::gc::CallCtx;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{ByteBuf, PrintKind};
use crate::objects::strings::mpprint::{Print, MP_PLAT_PRINT, MP_PLAT_PRINT_SINK_AWARE};
use crate::objects::strings::objstr_format::str_format;
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};

// ── The `print` / `next` / `__build_class__` builtins ────────────────

/// `mp_builtin_print` (`modbuiltins.zig:661-733`, full config):
/// `print(*args, sep=' ', end='\n', file=sys.stdout, flush=False)`.
/// The kwargs parse uses the terse form (probed: an unknown kw
/// raises `TypeError: argument num/types mismatch`). `sep`/`end`
/// go through `mp_obj_str_get_data` (probed: a non-str/bytes sep
/// raises `TypeError: can't convert to str implicitly`; a bytes
/// sep is accepted). An explicit `file=` routes through the
/// stream protocol (D2): `get_stream_raise(file, WRITE)` first,
/// then `stream_write_adaptor` per piece; `flush=True` calls
/// the file's `flush` method with 0 args (the Zig's
/// `mp_load_method` + `mp_call_method_n_kw`). A non-stream
/// file (or `file=None`) raises the vendor-C `OSError("stream
/// operation not supported")` — the reference ABORTS here
/// (rc=134, `mp_get_stream_raise` @panic), so Rust raising is
/// the documented divergence. With no `file` kwarg the output
/// goes to `MP_PLAT_PRINT_SINK_AWARE` (plain `write(1, …)` —
/// or the embedding host's sink when one is installed on this
/// thread, F1), exactly as the Zig's nano path does —
/// `sys.stdout` IS fd 1, so the bytes are identical. Args print via `obj_print_helper`
/// (`PRINT_STR`).
fn print_impl(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    use crate::objects::obj_ops::obj_print_helper;
    print_impl_with(ctx, args, kwargs, &mut |ctx: &mut CallCtx, v: Value, s: &mut ByteBuf| {
        obj_print_helper(ctx, ctx.types, v, PrintKind::Str, s);
        Ok(())
    })
}

/// `print_impl` with a caller-supplied arg renderer (E2): the
/// VM-intercepted `print` (`builtin_str::call_print_ctx`)
/// renders instance args through their `__str__` dunder, which
/// can raise — the render reports it as `PyError::PyRaise`
/// (the exception object travels on `state.pending_exc`, the
/// C14 side-channel idiom). The default render above is the
/// ctx-only path (infallible, as before).
/// The `print` kwargs, parsed (E2i — shared by
/// `print_impl_with` and the VM-intercepted
/// `print(file=<user instance>)` path in `builtin_str.rs`).
/// G2 (P7a): the render target is a `ByteBuf`, not a `String` —
/// a rendered non-UTF-8 `str` must reach fd 1 byte-exact.
pub(crate) struct PrintKwargs {
    pub(crate) sep: Option<Value>,
    pub(crate) end: Option<Value>,
    pub(crate) file: Option<Value>,
    pub(crate) flush: Option<Value>,
}

/// Parse the `print` kwargs (the terse form, probed: an
/// unknown kw raises `TypeError: argument num/types
/// mismatch`; a repeated kw likewise).
pub(crate) fn parse_print_kwargs(
    ctx: &mut CallCtx,
    kwargs: &[(u32, Value)],
) -> Result<PrintKwargs, PyError> {
    let terse = |ctx: &mut CallCtx| -> PyError {
        ctx.record_raise("TypeError", Some("argument num/types mismatch"));
        PyError::PyRaise
    };
    let sep_q = static_qstr_id(b"sep");
    let end_q = static_qstr_id(b"end");
    let file_q = static_qstr_id(b"file");
    let flush_q = static_qstr_id(b"flush");
    let mut sep: Option<Value> = None;
    let mut end: Option<Value> = None;
    let mut file: Option<Value> = None;
    let mut flush: Option<Value> = None;
    for (name, value) in kwargs {
        if Some(*name) == sep_q {
            if sep.is_some() {
                return Err(terse(ctx));
            }
            sep = Some(*value);
        } else if Some(*name) == end_q {
            if end.is_some() {
                return Err(terse(ctx));
            }
            end = Some(*value);
        } else if Some(*name) == file_q {
            if file.is_some() {
                return Err(terse(ctx));
            }
            file = Some(*value);
        } else if Some(*name) == flush_q {
            if flush.is_some() {
                return Err(terse(ctx));
            }
            flush = Some(*value);
        } else {
            return Err(terse(ctx));
        }
    }
    Ok(PrintKwargs {
        sep,
        end,
        file,
        flush,
    })
}

/// `mp_obj_str_get_data` for a `print` `sep`/`end` value: a qstr
/// immediate, a heap `Str`, or a heap `Bytes` gives its bytes;
/// anything else raises `TypeError: can't convert to str
/// implicitly` (probed on the reference binary).
pub(crate) fn sep_end_bytes(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
    match v {
        Value::Qstr(id) => Ok(ctx.pool.qstr_str(id as u16).to_vec()),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => Ok(s.bytes.clone()),
            Obj::Bytes(b) => Ok(b.bytes.clone()),
            _ => {
                ctx.record_raise("TypeError", Some("can't convert to str implicitly"));
                Err(PyError::PyRaise)
            }
        },
        _ => {
            ctx.record_raise("TypeError", Some("can't convert to str implicitly"));
            Err(PyError::PyRaise)
        }
    }
}

pub(crate) fn print_impl_with(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
    render: &mut dyn FnMut(&mut CallCtx, Value, &mut ByteBuf) -> PyResult<()>,
) -> PyResult<Value> {
    use crate::objects::obj::obj_is_true;
    use crate::runtime::runtime_call::call_method_n_kw;
    use crate::runtime::runtime_method::load_method;
    use crate::support::stream::{get_stream_raise, stream_write_adaptor, MP_STREAM_OP_WRITE};
    let kw = parse_print_kwargs(ctx, kwargs)?;
    let sep = kw.sep;
    let end = kw.end;
    let file = kw.file;
    let flush = kw.flush;
    let sep_bytes = match sep {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b" ".to_vec(),
    };
    let end_bytes = match end {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b"\n".to_vec(),
    };
    let flush = match flush {
        Some(v) => obj_is_true(ctx, ctx.types, v)?,
        None => false,
    };
    match file {
        Some(f) => {
            get_stream_raise(ctx, f, MP_STREAM_OP_WRITE)?;
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    stream_write_adaptor(ctx, f, &sep_bytes)?;
                }
                let mut s = ByteBuf::new();
                render(ctx, *arg, &mut s)?;
                stream_write_adaptor(ctx, f, &s.0)?;
            }
            stream_write_adaptor(ctx, f, &end_bytes)?;
            if flush {
                let flush_id =
                    static_qstr_id(b"flush").expect("flush is a static qstr");
                let dest = load_method(ctx, f, flush_id, Value::None)
                    .map_err(|e| rt_to_pyerror(ctx, e))?;
                call_method_n_kw(ctx, 0, 0, &dest, &[])
                    .map_err(|e| rt_to_pyerror(ctx, e))?;
            }
            Ok(Value::None)
        }
        None => {
            let mut sink = MP_PLAT_PRINT_SINK_AWARE;
            let mut print = Print::new(&mut sink);
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    let _ = mp_print_str(&mut print, &sep_bytes);
                }
                let mut s = ByteBuf::new();
                render(ctx, *arg, &mut s)?;
                let _ = mp_print_str(&mut print, &s.0);
            }
            let _ = mp_print_str(&mut print, &end_bytes);
            // P14: `flush=True` flushes the buffered stdout
            // (the `file=` path flushes the file object; the
            // no-file path ignored the flag until now).
            if flush {
                crate::support::output::flush_stdout_buffer();
            }
            Ok(Value::None)
        }
    }
}

/// Convert an `RtError` from the `flush` method call into the
/// `PyError` a builtin slot returns: named variants are
/// recorded on `ctx` (first record wins — the `load_method`
/// `AttributeError("no attribute 'flush'")` is the common one);
/// `PyRaise` is already recorded; `OutOfMemory` maps directly;
/// `Raised(v)` cannot be carried through the slot convention
/// (the pending slot holds name+arg, not an object) and falls
/// back to the codebase's `to_pyerror()` — the caller builds a
/// generic `TypeError` for it (see `scripts/d2_result.txt` §4).
fn rt_to_pyerror(ctx: &mut CallCtx, e: crate::runtime::runtime::RtError) -> PyError {
    match e {
        crate::runtime::runtime::RtError::PyRaise => PyError::PyRaise,
        crate::runtime::runtime::RtError::OutOfMemory => PyError::OutOfMemory,
        crate::runtime::runtime::RtError::Raised(_) => PyError::PyRaise,
        _ => {
            let name = e.exc_name();
            let msg = e.message();
            ctx.record_raise(name, Some(&msg));
            PyError::PyRaise
        }
    }
}

/// `print` as a `BuiltinKwFn` (the `kw_func` of the `print`
/// `FunBuiltin`).
pub fn print_builtin(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    print_impl(ctx, args, kwargs)
}

/// `print` as a plain `BuiltinFn` (the no-kw path —
/// `call_fun_builtin` always routes through `kw_func` when one
/// is present, so this is the documented no-kw form).
pub fn print_builtin_nokw(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    print_impl(ctx, args, &[])
}

/// `__repl_print__` (`modbuiltins.zig:355`, sig (1, 1)): the
/// REPL's echo — if the argument is not `None`, print its
/// `repr` followed by `\n` to stdout (`MP_PLAT_PRINT`, the
/// same fd-1 path `print` uses with no `file` kwarg), and
/// return `None`. (The `_ = value` assignment in the Zig is
/// dead under nano — `CAN_OVERRIDE_BUILTINS=0` — do not
/// implement it.) Bound in `bind_d1_builtins`; the compiler's
/// REPL branch resolves the name on demand
/// (`compReplPrintQstr`, `compile.zig:2716-2718`).
pub fn repl_print_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    use crate::objects::obj_ops::obj_print_helper;
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    if *arg != Value::None {
        let types = ctx.types;
        let mut s = ByteBuf::new();
        obj_print_helper(ctx, types, *arg, PrintKind::Repr, &mut s);
        let mut sink = MP_PLAT_PRINT;
        let mut print = Print::new(&mut sink);
        let _ = mp_print_str(&mut print, &s.0);
        let _ = mp_print_str(&mut print, b"\n");
    }
    Ok(Value::None)
}

/// `str.format` as a `BuiltinKwFn` (C13 — C8b's deferred item):
/// the kwargs pass through to `str_format` instead of `&[]`, so
/// `"{x}".format(x=1)` works.
pub fn format_kw_wrapper(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    use crate::objects::strings::objstr_methods_c::KwArg;
    let types = ctx.types;
    let kws: Vec<KwArg> = kwargs
        .iter()
        .map(|(name, value)| KwArg {
            name: *name,
            value: *value,
        })
        .collect();
    str_format(ctx, types, args, &kws).map_err(|e| ctx.record_typed(e))
}

/// The ellipsis print slot: `repr(...)` / `print(...)` is
/// `Ellipsis` (probed on the reference binary). The ellipsis
/// type is shared by the `Ellipsis` singleton and the
/// `NotImplemented` singleton (D1 — the vendor's
/// `mp_type_singleton` carries both, `objsingleton.c:47-53`),
/// so the payload decides the spelling.
pub(crate) fn ellipsis_print(
    ctx: &mut CallCtx,
    v: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let is_not_implemented = match v {
        Value::Heap(r) => matches!(ctx.arena.get(r), Obj::Instance(_)),
        _ => false,
    };
    let _ = sink.write_str(if is_not_implemented {
        "NotImplemented"
    } else {
        "Ellipsis"
    });
}

/// `none_print` (`mp_type_roms.zig:191-201`, vendor `objnone.c`):
/// the `NoneType` print slot — `None` for every kind (the JSON
/// `"null"` branch is dead under full config, as the Zig's
/// `_ = kind` notes). Until D1 the VM's `none_type` was built by
/// `plain_type` with NO print slot, so `obj_print_helper` fell
/// back to the default `"<NoneType>"` — `repr(None)` and
/// `print(None)` were both wrong in master.
pub(crate) fn none_print(
    _ctx: &mut CallCtx,
    _v: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let _ = sink.write_str("None");
}

/// `next` as a `BuiltinFn` — a stub: the VM intercepts `next` by
/// GcRef identity in `call_function` (it needs the VM's
/// `iternext` seams, which a bare `BuiltinFn` cannot carry), so
/// this body is unreachable through the VM.
pub(crate) fn next_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `__build_class__` as a `BuiltinFn` — a stub: the VM intercepts
/// `__build_class__` by GcRef identity in `call_function` (it
/// needs the VM's frame execution + the locals override, which a
/// bare `BuiltinFn` cannot carry), so this body is unreachable
/// through the VM (see `call_build_class_ctx` in `vm_call.rs`).
pub(crate) fn build_class_builtin_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

// ── Re-exports ───────────────────────────────────────────────────────

/// The Print-based `mp_print_*` exports, re-exported for the VM's
/// default print paths (C8c's `Print` API — `mp_print_str` /
/// `mp_print_int` / `mp_print_float` over a `Print` sink).
pub use crate::objects::strings::mpprint::{mp_print_float, mp_print_int, mp_print_str};
