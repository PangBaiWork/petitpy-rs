//! The VM's builtin functions — `print` / `next` /
//! `__build_class__` and the `str.format` kwargs wrapper
//! (C13). Split out of `vm.rs` (file-size cap).

use crate::memory::gc::CallCtx;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::strings::mpprint::{Print, MP_PLAT_PRINT};
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
/// sep is accepted). `file=`/`flush=` are IGNORED — there are no
/// stream objects yet (D lane's `sys.stdout`); output goes to
/// `MP_PLAT_PRINT` (plain `write(1, …)`), exactly as the Zig's
/// nano path does. Args print via `obj_print_helper` (`PRINT_STR`).
fn print_impl(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    use crate::objects::obj_ops::obj_print_helper;
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
        } else if Some(*name) == file_q || Some(*name) == flush_q {
            // Ignored (no streams yet — D lane).
        } else {
            return Err(terse(ctx));
        }
    }
    let sep_bytes = match sep {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b" ".to_vec(),
    };
    let end_bytes = match end {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b"\n".to_vec(),
    };
    let mut sink = MP_PLAT_PRINT;
    let mut print = Print::new(&mut sink);
    let types = ctx.types;
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            let _ = mp_print_str(&mut print, &sep_bytes);
        }
        let mut s = String::new();
        obj_print_helper(ctx, types, *arg, PrintKind::Str, &mut s);
        let _ = mp_print_str(&mut print, s.as_bytes());
    }
    let _ = mp_print_str(&mut print, &end_bytes);
    Ok(Value::None)
}

/// `mp_obj_str_get_data` for a `print` `sep`/`end` value: a qstr
/// immediate, a heap `Str`, or a heap `Bytes` gives its bytes;
/// anything else raises `TypeError: can't convert to str
/// implicitly` (probed on the reference binary).
fn sep_end_bytes(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
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
/// `Ellipsis` (probed on the reference binary).
pub(crate) fn ellipsis_print(
    _ctx: &mut CallCtx,
    _v: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let _ = sink.write_str("Ellipsis");
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
