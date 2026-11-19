//! VM-intercepted `str` / `repr` / `bool` / `len` / `print`
//! (E2, Fix C follow-up).
//!
//! These five builtins are ctx-only `FunBuiltin`s (or, for
//! `str` / `bool`, the type objects themselves), so an
//! instance's `__str__` / `__repr__` / `__bool__` / `__len__`
//! never runs through them — the slot adapters use stub seams.
//! They are intercepted by GcRef identity in
//! `vm_call_intercept` (the D1 pattern) and routed here, where
//! `state` is live for the real call hook.
//!
//! Fidelity notes (all probed on the reference binary):
//! - `str(x)` calls `__str__`, falling back to `__repr__` when
//!   `__str__` is missing; `repr(x)` calls `__repr__` only.
//!   The dunder's RESULT is printed as STR into the return
//!   value (`str(A())` is `"1"` when `__str__` returns `1` —
//!   the result is rendered, not returned raw).
//! - `len(x)` returns the `__len__` result RAW (`len(C())` is
//!   `"x"` when `__len__` returns `"x"`).
//! - `bool(x)` uses the `__bool__` identity rule (`__bool__`
//!   returning `1` is `False` — see `vm_seams`).
//! - A dunder that raises PROPAGATES here. (The reference
//!   ABORTS on a raising `__str__` inside `str()` — a Zig
//!   null-pointer panic in `instance_print` — so Rust raising
//!   the exception is a deliberate divergence, same class as
//!   the other abort-vs-raise cases. The print SLOT swallows
//!   the raise and falls back to the default rendering — that
//!   is the slot's infallible signature, not the builtin's.)

use crate::memory::gc::CallCtx;
use crate::objects::containers::map::map_of;
use crate::objects::containers::map_set::set_items;
use crate::objects::containers::objdict::is_ordered_dict_value;
use crate::objects::containers::objlist::list_items_vec;
use crate::objects::containers::objset::is_frozenset;
use crate::objects::containers::objtuple::tuple_items;
use crate::objects::obj::obj_is_true;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::objtype::lookup::{class_lookup, LookupHit, SlotKind};
use crate::objects::objtype::seams::with_stub_seams;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{ByteBuf, ByteSink, PrintKind};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_call::call_type_ctx;
use crate::runtime::vm_seams::{is_instance, try_instance_bool, try_instance_unary_op};

/// Look a print dunder up the way `instance_print` does
/// (`lookup_print_method`): `class_lookup` with the PRINT slot
/// (slot 0 for the `__repr__` fallback after a missed
/// `__str__`), stub seams. Returns the BOUND method value
/// (`bind_pair`), or `None` when there is no such dunder.
fn lookup_print_dunder(
    ctx: &mut CallCtx,
    value: Value,
    type_: crate::objects::value::GcRef,
    attr: u32,
    slot: Option<SlotKind>,
) -> Option<Value> {
    use crate::objects::objtype::instance::bind_pair;
    let hit = with_stub_seams(|seams| class_lookup(ctx, Some(value), attr, slot, false, type_, seams));
    match hit {
        Some(LookupHit::Member(fun, bound)) => Some(bind_pair(ctx, fun, bound)),
        _ => None,
    }
}

/// Render an instance through its `__str__` / `__repr__`
/// dunder, the way `instance_print` does: call the dunder with
/// 0 args, print the RESULT as STR. `Ok(Some(s))` = a dunder
/// ran; `Ok(None)` = no dunder (fall back to the default
/// rendering); `Err(exc)` = the dunder raised (propagated —
/// see the module docs).
fn render_instance(
    ctx: &mut CallCtx,
    state: &mut VmState,
    value: Value,
    kind: PrintKind,
) -> Result<Option<String>, Value> {
    let Some(type_) = crate::objects::obj::obj_get_type(ctx.arena, ctx.types, value) else {
        return Ok(None);
    };
    let mut fun = None;
    if kind == PrintKind::Str {
        if let Some(q) = static_qstr_id(b"__str__") {
            fun = lookup_print_dunder(ctx, value, type_, q, Some(SlotKind::Print));
        }
        if fun.is_none() {
            if let Some(q) = static_qstr_id(b"__repr__") {
                fun = lookup_print_dunder(ctx, value, type_, q, None);
            }
        }
    } else if let Some(q) = static_qstr_id(b"__repr__") {
        fun = lookup_print_dunder(ctx, value, type_, q, Some(SlotKind::Print));
    }
    let Some(fun) = fun else {
        return Ok(None);
    };
    let ret = call_function_ctx(ctx, state, fun, 0, 0, &[])?;
    let mut s = String::new();
    obj_print_helper(ctx, ctx.types, ret, PrintKind::Str, &mut s);
    Ok(Some(s))
}

/// Allocate a fresh heap `Str` from bytes.
fn new_str_value(ctx: &mut CallCtx, bytes: &[u8]) -> Value {
    use crate::objects::strings::objstr_a::new_str;
    new_str(ctx, ctx.types, bytes)
}

/// Is `value` a container payload whose print recurses
/// elements (exact list / tuple / dict / set — the four
/// `*_print` slots `print_helper_hook` mirrors)? The tuple
/// arm requires an EXACT tuple: a namedtuple is an
/// `Obj::Tuple` headed by its class, and its repr comes from
/// the class's own print slot (`TupEmpty()`), not `"()"`.
fn is_container(ctx: &CallCtx, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    use crate::objects::containers::objlist::is_exact_list;
    use crate::objects::containers::objtuple::is_exact_tuple;
    if is_exact_list(ctx, ctx.types.list, value)
        || is_exact_tuple(ctx, ctx.types.tuple, value)
    {
        return true;
    }
    matches!(ctx.arena.get(r), Obj::Dict(_) | Obj::Set(_))
}

/// Render `value` into `sink` with the VM's real call hook
/// (E2d): the hook-aware counterpart of `obj_print_helper`.
/// The ctx-only helper cannot run a FunBc `__repr__` (its
/// print slot uses stub seams), so a container holding an
/// instance falls back to `<A object at …>` — this mirrors
/// `instance_print` (`objtype.zig:1638-1687`) + the container
/// prints (`list_print` / `tuple_print` / `dict_print` /
/// `set_print`): an instance renders through its dunder, a
/// container recurses its elements with `PrintKind::Repr`
/// (dict keys AND values; `OrderedDict({...})` for ordered
/// dicts), anything else delegates to `obj_print_helper`.
/// The `kind` is honoured only at the top level (a
/// container's elements are always Repr, like the Zig);
/// nested instances always render through `__repr__`.
/// SLOT SEMANTICS: a dunder raise is SWALLOWED here and the
/// default rendering is used — the print slot is infallible,
/// and the reference ABORTS (rc=134, null-pointer panic at
/// `mp_mirrors.zig:745`) on a raising `__repr__` inside a
/// container, so Rust keeping the fallback is a documented
/// divergence (same class as the other abort-vs-fallback
/// cases). Only the `str` / `repr` BUILTINS propagate a
/// dunder raise (see the module docs).
pub fn print_helper_hook(
    ctx: &mut CallCtx,
    state: &mut VmState,
    value: Value,
    kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    if is_instance(ctx.arena, value) {
        match render_instance(ctx, state, value, kind) {
            Ok(Some(text)) => {
                let _ = sink.write_str(&text);
            }
            _ => obj_print_helper(ctx, ctx.types, value, kind, sink),
        }
        return;
    }
    let Value::Heap(r) = value else {
        obj_print_helper(ctx, ctx.types, value, kind, sink);
        return;
    };
    // The container kind as an owned tag: the arena borrow
    // ends here, so the recursive calls below can take
    // `&mut ctx`. The tuple arm requires an EXACT tuple — a
    // namedtuple (`Obj::Tuple` headed by its class) falls
    // through to `obj_print_helper`, whose type-slot dispatch
    // renders `TupEmpty()`.
    use crate::objects::containers::objtuple::is_exact_tuple;
    let tag = match ctx.arena.get(r) {
        Obj::List(_) => 1,
        Obj::Tuple(_) | Obj::AttrTuple(_)
            if is_exact_tuple(ctx, ctx.types.tuple, value) =>
        {
            2
        }
        Obj::Dict(_) => 3,
        Obj::Set(_) => 4,
        _ => 0,
    };
    match tag {
        1 => {
            let items = list_items_vec(ctx, value);
            let _ = sink.write_str("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    let _ = sink.write_str(", ");
                }
                print_helper_hook(ctx, state, *item, PrintKind::Repr, sink);
            }
            let _ = sink.write_str("]");
        }
        2 => {
            let items: Vec<Value> = tuple_items(ctx, value).unwrap_or(&[]).to_vec();
            let _ = sink.write_str("(");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    let _ = sink.write_str(", ");
                }
                print_helper_hook(ctx, state, *item, PrintKind::Repr, sink);
            }
            if items.len() == 1 {
                let _ = sink.write_str(",");
            }
            let _ = sink.write_str(")");
        }
        3 => {
            let ordered = is_ordered_dict_value(ctx, value);
            if ordered {
                let _ = sink.write_str("OrderedDict(");
            }
            let _ = sink.write_str("{");
            let mut cur = 0;
            let mut first = true;
            while let Some(i) = map_of(ctx.arena, r).next_live(&mut cur) {
                if !first {
                    let _ = sink.write_str(", ");
                }
                first = false;
                let (k, v) = {
                    let m = map_of(ctx.arena, r);
                    (
                        m.entries[i].key.expect("live entry has a key"),
                        m.entries[i].value,
                    )
                };
                print_helper_hook(ctx, state, k, PrintKind::Repr, sink);
                let _ = sink.write_str(": ");
                print_helper_hook(ctx, state, v, PrintKind::Repr, sink);
            }
            let _ = sink.write_str("}");
            if ordered {
                let _ = sink.write_str(")");
            }
        }
        4 => {
            let frozen = is_frozenset(ctx.arena, ctx.types, value);
            let items = match ctx.arena.get(r) {
                Obj::Set(s) => set_items(&s.table),
                _ => Vec::new(),
            };
            if items.is_empty() {
                let _ = sink.write_str(if frozen { "frozenset()" } else { "set()" });
                return;
            }
            let _ = sink.write_str(if frozen { "frozenset({" } else { "{" });
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    let _ = sink.write_str(", ");
                }
                print_helper_hook(ctx, state, *item, PrintKind::Repr, sink);
            }
            let _ = sink.write_str(if frozen { "})" } else { "}" });
        }
        _ => obj_print_helper(ctx, ctx.types, value, kind, sink),
    }
}

/// `str(x)` through the VM (E2): a 1-arg instance call runs
/// the dunder path; anything else delegates to the type-call
/// path (`str_make_new` — `str()`, `str(b, encoding)`, ...).
pub(crate) fn call_str_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                if let Some(s) = render_instance(ctx, state, arg, PrintKind::Str)? {
                    return Ok(new_str_value(ctx, s.as_bytes()));
                }
            }
            // E2d: a container renders through the hook-aware
            // helper, so a nested instance's `__repr__` runs
            // (the ctx-only path falls back to `<A object …>`).
            if is_container(ctx, arg) {
                let mut s = String::new();
                print_helper_hook(ctx, state, arg, PrintKind::Str, &mut s);
                return Ok(new_str_value(ctx, s.as_bytes()));
            }
        }
    }
    let r = ctx.types.str_;
    call_type_ctx(ctx, state, r, n_args, n_kw, args)
}

/// `repr(x)` through the VM (E2): a 1-arg instance call runs
/// the dunder path; anything else delegates to the existing
/// `repr_builtin`.
pub(crate) fn call_repr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                if let Some(s) = render_instance(ctx, state, arg, PrintKind::Repr)? {
                    return Ok(new_str_value(ctx, s.as_bytes()));
                }
            }
            // E2d: a container renders through the hook-aware
            // helper (see `call_str_ctx`).
            if is_container(ctx, arg) {
                let mut s = String::new();
                print_helper_hook(ctx, state, arg, PrintKind::Repr, &mut s);
                return Ok(new_str_value(ctx, s.as_bytes()));
            }
        }
    }
    crate::runtime::builtin_fns::repr_builtin(ctx, &args[..n_args])
        .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e))
}

/// `bool(x)` through the VM (E2): a 1-arg instance call runs
/// `__bool__` / `__len__` with the real hook; anything else
/// delegates to the type-call path (`bool()`, `bool(x)` for
/// non-instances, arg errors).
pub(crate) fn call_bool_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                match try_instance_bool(ctx, state, arg)? {
                    Some(b) => return Ok(Value::Bool(b)),
                    None => {}
                }
                let types = ctx.types;
                return obj_is_true(ctx, types, arg)
                    .map(Value::Bool)
                    .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e));
            }
        }
    }
    let r = ctx.types.bool_;
    call_type_ctx(ctx, state, r, n_args, n_kw, args)
}

/// `len(x)` through the VM (E2): a 1-arg instance call runs
/// `__len__` with the real hook and returns the result RAW;
/// anything else delegates to the existing `len_builtin`.
pub(crate) fn call_len_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args == 1 && n_kw == 0 {
        if let Some(arg) = args.first().copied() {
            if is_instance(ctx.arena, arg) {
                use crate::objects::slots::UnaryOp;
                if let Some(v) = try_instance_unary_op(ctx, state, UnaryOp::Len, arg)? {
                    return Ok(v);
                }
            }
        }
    }
    crate::runtime::builtin_fns::len_builtin(ctx, &args[..n_args])
        .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e))
}

/// `print(...)` through the VM (E2): when any positional arg
/// is an instance, render through `print_impl_with` with the
/// instance-aware render (a dunder raise is stashed on
/// `state.pending_exc` and drained below); otherwise delegate
/// to the existing `print_builtin` (bit-identical behaviour
/// for non-instance args).
/// Render one `print` argument into `s` (E2i — extracted
/// from `call_print_ctx`'s render closure so the
/// `print(file=<user instance>)` path renders identically):
/// an instance through its `__str__` dunder (a raise stashes
/// the exception object on `state.pending_exc` and reports
/// `PyRaise`, the C14 side-channel idiom), a container
/// through `print_helper_hook`, anything else through the
/// default `obj_print_helper`.
fn render_print_arg(
    ctx: &mut CallCtx,
    state: &mut VmState,
    v: Value,
    s: &mut ByteBuf,
) -> PyResult<()> {
    if is_instance(ctx.arena, v) {
        match render_instance(ctx, state, v, PrintKind::Str) {
            Ok(Some(text)) => {
                s.write_bytes(text.as_bytes());
                Ok(())
            }
            Ok(None) => {
                obj_print_helper(ctx, ctx.types, v, PrintKind::Str, s);
                Ok(())
            }
            Err(exc) => {
                state.pending_exc = Some(exc);
                Err(PyError::PyRaise)
            }
        }
    } else if is_container(ctx, v) {
        // E2d: a nested instance's `__repr__` runs through the
        // hook (a raise inside is swallowed by the helper —
        // slot semantics, see `print_helper_hook`).
        print_helper_hook(ctx, state, v, PrintKind::Str, s);
        Ok(())
    } else {
        obj_print_helper(ctx, ctx.types, v, PrintKind::Str, s);
        Ok(())
    }
}

/// Does `file`'s `write` method need the VM (E2i)? Only a
/// `FunBc` / `Closure` does — a native `write` (a `StringIO`,
/// a `File`) runs through the ctx-only stream protocol, and
/// a missing `write` keeps the protocol path's
/// `AttributeError`. The lookup is ctx-only (no call), so it
/// cannot raise.
fn file_needs_hook_write(ctx: &mut CallCtx, file: Value) -> bool {
    if !is_instance(ctx.arena, file) {
        return false;
    }
    let Some(write_q) = static_qstr_id(b"write") else {
        return false;
    };
    match crate::objects::objtype::seams::real_load_maybe(ctx, file, write_q) {
        Some((fun, _)) => matches!(
            fun,
            Value::Heap(r)
                if matches!(ctx.arena.get(r), Obj::FunBc(_) | Obj::Closure(_))
        ),
        None => false,
    }
}

/// Call `file`'s `write` method with a bytearray over `piece`
/// (E2i — one `print` piece; the hook is created per call
/// because a hook and the render both borrow `state`, so they
/// cannot be live at once). The return converts the way
/// `iobase_read_write` does: `None` → `OSError(EAGAIN)`,
/// int `< 0` → `OSError(-ret)`, anything else →
/// `TypeError` (`mp_obj_get_int`); the count itself is
/// discarded (the adaptor's contract).
fn write_file_piece(
    ctx: &mut CallCtx,
    state: &mut VmState,
    file: Value,
    piece: &[u8],
) -> PyResult<()> {
    use crate::objects::containers::objarray::new_bytearray;
    use crate::objects::obj::{obj_get_int, ObjError};
    use crate::objects::objtype::seams::real_load_maybe;
    use crate::objects::slots::VmHook;
    use crate::runtime::raise::raise_os_error;
    use crate::support::stream::MP_EAGAIN;
    let Some(write_q) = static_qstr_id(b"write") else {
        ctx.record_raise("AttributeError", Some("no attribute 'write'"));
        return Err(PyError::PyRaise);
    };
    let Some((fun, bound)) = real_load_maybe(ctx, file, write_q) else {
        ctx.record_raise("AttributeError", Some("no attribute 'write'"));
        return Err(PyError::PyRaise);
    };
    let ba = new_bytearray(ctx, ctx.types.bytearray, piece);
    // The bytearray must be rooted across the method call: the
    // call allocates, a collection could trigger (the GC stress
    // rule), and `ba` is otherwise reachable only from this
    // stack slot (D6 — the `iobase_read_write` idiom).
    ctx.roots.push(Value::Heap(ba));
    let mut hook = crate::runtime::vm_seams::make_hook(state);
    let ret_obj = match bound {
        Some(self_) => hook.call(ctx, fun, 2, 0, &[self_, Value::Heap(ba)]),
        None => hook.call(ctx, fun, 1, 0, &[Value::Heap(ba)]),
    }?;
    if ret_obj == Value::None {
        return Err(raise_os_error(ctx, MP_EAGAIN));
    }
    let ret = obj_get_int(ctx, ctx.types, ret_obj).map_err(|e| match e {
        ObjError::OutOfMemory => PyError::OutOfMemory,
        other => ctx.record_typed(other),
    })?;
    if ret < 0 {
        let errno = (-ret) as i32;
        return Err(raise_os_error(ctx, errno));
    }
    Ok(())
}

/// `print(..., file=<user instance>)` (E2i): the file's
/// `write` is a `FunBc` — it cannot run through the ctx-only
/// stream protocol (`iobase_write` → `call_method_n_kw` →
/// the `fun_bc_call_slot` stub → a bare `TypeError`), so
/// `print` to a user `io.IOBase` subclass failed
/// (`io_iobase.py`: REF prints `write 4` / `write 1`). This
/// path renders each piece exactly like `print_impl_with`
/// does (arg, sep between args, end last) and writes each
/// through the VM hook; `flush=True` calls the file's `flush`
/// method with 0 args through the hook (missing → the
/// `AttributeError` the ctx-only path raises).
fn print_to_instance_file(
    ctx: &mut CallCtx,
    state: &mut VmState,
    args: &[Value],
    kwargs: &[(u32, Value)],
    file: Value,
) -> PyResult<Value> {
    use crate::objects::obj::obj_is_true;
    use crate::runtime::vm_builtins::{parse_print_kwargs, sep_end_bytes};
    let kw = parse_print_kwargs(ctx, kwargs)?;
    let sep_bytes = match kw.sep {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b" ".to_vec(),
    };
    let end_bytes = match kw.end {
        Some(v) => sep_end_bytes(ctx, v)?,
        None => b"\n".to_vec(),
    };
    let flush = match kw.flush {
        Some(v) => obj_is_true(ctx, ctx.types, v)?,
        None => false,
    };
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            write_file_piece(ctx, state, file, &sep_bytes)?;
        }
        let mut s = ByteBuf::new();
        render_print_arg(ctx, state, *arg, &mut s)?;
        write_file_piece(ctx, state, file, &s.0)?;
    }
    write_file_piece(ctx, state, file, &end_bytes)?;
    if flush {
        use crate::objects::objtype::seams::real_load_maybe;
        use crate::objects::slots::VmHook;
        let Some(flush_q) = static_qstr_id(b"flush") else {
            ctx.record_raise("AttributeError", Some("no attribute 'flush'"));
            return Err(PyError::PyRaise);
        };
        let Some((fun, bound)) = real_load_maybe(ctx, file, flush_q) else {
            ctx.record_raise("AttributeError", Some("no attribute 'flush'"));
            return Err(PyError::PyRaise);
        };
        let mut hook = crate::runtime::vm_seams::make_hook(state);
        match bound {
            Some(self_) => hook.call(ctx, fun, 1, 0, &[self_])?,
            None => hook.call(ctx, fun, 0, 0, &[])?,
        };
    }
    Ok(Value::None)
}

pub(crate) fn call_print_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    // The kwargs pairs, built once: the instance-file check
    // below needs them before the hook-arg branch.
    let pairs: Vec<(u32, Value)> = (0..n_kw)
        .map(|i| {
            let name = args[n_args + 2 * i].as_qstr().unwrap_or(0);
            (name, args[n_args + 2 * i + 1])
        })
        .collect();
    // E2i: a `file=` that is a user instance whose `write` is
    // a `FunBc` takes the hook path (`print_to_instance_file`)
    // — the ctx-only stream protocol cannot run it
    // (`io_iobase.py`). Any other file (a native stream, a
    // missing `write`, `file=None`) keeps the paths below.
    if let Some(file_q) = static_qstr_id(b"file") {
        if let Some((_, f)) = pairs.iter().find(|(name, _)| *name == file_q) {
            if file_needs_hook_write(ctx, *f) {
                return print_to_instance_file(ctx, state, &args[..n_args], &pairs, *f)
                    .map_err(|e| crate::runtime::vm_seams::drain_hook_raise(ctx, state, e));
            }
        }
    }
    // E2d: the hook path also runs when an arg is a container
    // (it may hold an instance — the render closure recurses
    // it through `print_helper_hook`); plain scalars still
    // take the `print_builtin` path below.
    let has_hook_arg = args[..n_args]
        .iter()
        .any(|a| is_instance(ctx.arena, *a) || is_container(ctx, *a));
    if !has_hook_arg {
        return crate::runtime::vm_builtins::print_builtin(ctx, &args[..n_args], &pairs)
            .map_err(|e| crate::runtime::vm_ops::exc_from_pyerror(ctx, state, e));
    }
    // The render closure borrows `state` for the dunder calls;
    // it is dropped when `print_impl_with` returns, before the
    // error drain below borrows `state` again.
    let result = {
        let mut render =
            |ctx: &mut CallCtx, v: Value, s: &mut ByteBuf| -> PyResult<()> {
                render_print_arg(ctx, state, v, s)
            };
        crate::runtime::vm_builtins::print_impl_with(
            ctx,
            &args[..n_args],
            &pairs,
            &mut render,
        )
    };
    result.map_err(|e| crate::runtime::vm_seams::drain_hook_raise(ctx, state, e))
}