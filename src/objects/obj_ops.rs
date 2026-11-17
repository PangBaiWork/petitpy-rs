//! Object operations — equality, printing, indexing, exceptions.
//!
//! The second half of the `obj.zig` port (the other half is
//! `obj.rs`: slot dispatch + int/float/complex extraction + len +
//! truth). Everything here dispatches through the `obj.rs` helpers
//! (`call_binary`, `call_subscr`, `obj_get_type`, …); the dependency
//! is one-way (`obj_ops` → `obj`).

use std::fmt;

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{
    buffer_slot, call_binary, call_subscr, call_unary, is_bytes, is_str, obj_get_int_maybe,
    obj_get_type, obj_is_true, print_slot, qstr_bytes, type_data, BuiltinTypes, ObjError,
    SlotOutcome, BUFFER_RAISE_IF_UNSUPPORTED, BUFFER_RW, FLAG_EQ_CHECKS_OTHER_TYPE,
    FLAG_EQ_HAS_NEQ_TEST, FLAG_EQ_NOT_REFLEXIVE,
};
use crate::objects::slots::{BinaryOp, BufferInfo, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};

// ── equality ──────────────────────────────────────────────────────────

/// `mp_obj_str_equal` (vendor `py/objstr.c`): string content
/// equality. qstr/qstr compares by content when both ids are
/// statically resolvable (the static table could in principle hold
/// one string under two ids — the C1 interleave finding — so id
/// equality alone is not proof); a dynamic id compares by id only
/// (the dynamic pool dedups by content, and a dynamic id's content
/// never equals a static id's content, since `from_strn` searches
/// the static table first).
fn str_equal(arena: &GcArena, o1: Value, o2: Value) -> bool {
    match (o1, o2) {
        (Value::Qstr(a), Value::Qstr(b)) => {
            if a == b {
                true
            } else {
                match (qstr_bytes(a), qstr_bytes(b)) {
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                }
            }
        }
        (Value::Qstr(id), Value::Heap(r)) | (Value::Heap(r), Value::Qstr(id)) => {
            match (qstr_bytes(id), arena.get(r)) {
                (Some(bytes), Obj::Str(s)) => bytes == s.bytes.as_slice(),
                _ => false,
            }
        }
        (Value::Heap(a), Value::Heap(b)) => match (arena.get(a), arena.get(b)) {
            (Obj::Str(x), Obj::Str(y)) => x.bytes == y.bytes,
            _ => false,
        },
        _ => false,
    }
}

/// The `str` vs `bytes` comparison warning (`obj.zig`
/// equalNotEqualImpl, full + feature-warning): the original prints
/// it to stderr (verified on the reference binary:
/// `Warning: Comparison between bytes and str`). Until the print
/// machinery (C13/C14) centralises stderr, this writes there
/// directly.
fn warn_str_bytes_cmp() {
    eprintln!("Warning: Comparison between bytes and str");
}

/// `mp_obj_equal_not_equal` (`obj.zig:309-368`): the general
/// equality dispatch. `op` must be EQUAL or NOT_EQUAL (anything
/// else is a caller bug — the original takes a `c_int` op and only
/// these two reach here).
pub fn obj_equal_not_equal(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    op: BinaryOp,
    o1_in: Value,
    o2_in: Value,
) -> PyResult<Value> {
    debug_assert!(op == BinaryOp::Equal || op == BinaryOp::NotEqual);
    let mut o1 = o1_in;
    let mut o2 = o2_in;
    let local_true = if op == BinaryOp::NotEqual {
        Value::Bool(false)
    } else {
        Value::Bool(true)
    };
    let local_false = if op == BinaryOp::NotEqual {
        Value::Bool(true)
    } else {
        Value::Bool(false)
    };

    // Reflexive shortcut (`obj.zig:323-333`).
    if o1 == o2 {
        if o1.is_small_int() {
            return Ok(local_true);
        }
        if let Some(t) = obj_get_type(ctx.arena, types, o1) {
            if let Some(d) = type_data(ctx.arena, t) {
                if d.flags & FLAG_EQ_NOT_REFLEXIVE == 0 {
                    return Ok(local_true);
                }
            }
        }
    }

    // Decide whether to skip the first generic pass.
    let mut skip_first = false;
    if is_str(ctx.arena, types, o1) {
        if is_str(ctx.arena, types, o2) {
            return Ok(if str_equal(ctx.arena, o1, o2) {
                local_true
            } else {
                local_false
            });
        }
        if is_bytes(ctx.arena, types, o2) {
            warn_str_bytes_cmp();
            return Ok(local_false);
        }
        skip_first = true;
    } else {
        if is_bytes(ctx.arena, types, o1) && is_str(ctx.arena, types, o2) {
            warn_str_bytes_cmp();
            return Ok(local_false);
        }
        if o1.is_small_int() {
            if o2.is_small_int() {
                return Ok(local_false);
            }
            skip_first = true;
        }
    }

    // Generic 2-pass loop (`obj.zig:349-367`): try o1's type, then
    // swap and try o2's type. A slot "not implemented" (`NULL` in
    // the original) falls through; a slot OutOfMemory propagates.
    for _pass in 0..2 {
        if skip_first {
            skip_first = false;
        } else {
            let t = obj_get_type(ctx.arena, types, o1);
            // Copy the slot + flags out first: the borrow of the
            // arena ends here, so the slot can be called with
            // `&mut ctx` below.
            let (bin, flags) = match t.and_then(|t| type_data(ctx.arena, t)) {
                Some(d) => (d.binary_op, d.flags),
                None => (None, 0),
            };
            if let Some(bin) = bin {
                let cross_ok = flags & FLAG_EQ_CHECKS_OTHER_TYPE != 0;
                let same_type = obj_get_type(ctx.arena, types, o2) == t;
                if cross_ok || same_type {
                    // CPython asymmetry: try `__ne__` first for
                    // NOT_EQUAL when the type has a neq test.
                    if op == BinaryOp::NotEqual && flags & FLAG_EQ_HAS_NEQ_TEST != 0 {
                        match bin(ctx, BinaryOp::NotEqual, o1, o2) {
                            Ok(r) => return Ok(r),
                            Err(PyError::OutOfMemory) => {
                                return Err(PyError::OutOfMemory)
                            }
                            Err(PyError::PyRaise) => {} // NULL → keep looking
                        }
                    }
                    match bin(ctx, BinaryOp::Equal, o1, o2) {
                        Ok(r) => {
                            if op == BinaryOp::Equal {
                                return Ok(r);
                            }
                            return Ok(if obj_is_true(ctx, types, r)? {
                                local_true
                            } else {
                                local_false
                            });
                        }
                        Err(PyError::OutOfMemory) => {
                            return Err(PyError::OutOfMemory)
                        }
                        Err(PyError::PyRaise) => {} // NULL → keep looking
                    }
                }
            }
        }
        std::mem::swap(&mut o1, &mut o2);
    }

    // Equality not implemented: fall back to pointer comparison
    // (`obj.zig:367-368`).
    Ok(if o1 == o2 { local_true } else { local_false })
}

/// `mp_obj_equal` (`obj.zig:369-371`): truth of the EQUAL dispatch.
pub fn obj_equal(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    o1: Value,
    o2: Value,
) -> PyResult<bool> {
    let r = obj_equal_not_equal(ctx, types, BinaryOp::Equal, o1, o2)?;
    obj_is_true(ctx, types, r)
}

// ── type name / print ─────────────────────────────────────────────────

/// `mp_obj_get_type_str` (`obj.zig:447-450`): the type's name bytes.
/// Static names resolve through the generated table; a dynamic type
/// name (a user class) resolves to empty here — see the module docs.
pub fn obj_get_type_str(arena: &GcArena, value: Value) -> &'static [u8] {
    match value {
        Value::SmallInt(_) => b"int",
        Value::Bool(_) => b"bool",
        Value::None => b"NoneType",
        Value::Qstr(_) => b"str",
        Value::Heap(r) => match arena.header(r).type_ {
            Some(t) => match arena.get(t) {
                Obj::Type(d) => qstr_bytes(d.name).unwrap_or(b""),
                _ => b"",
            },
            None => b"",
        },
    }
}

/// `mp_obj_print_helper` (`obj.zig:452-461`): print via the type's
/// print slot, else `<typename>`. (The Zig comment about NDEBUG and
/// the "(nil)" defensive print concerns vendor C only — the Zig code
/// has no nil branch, and neither does this.)
pub fn obj_print_helper(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    value: Value,
    kind: PrintKind,
    sink: &mut dyn fmt::Write,
) {
    let t = obj_get_type(ctx.arena, types, value);
    if let Some(f) = print_slot(ctx.arena, t) {
        f(ctx, value, kind, sink);
        return;
    }
    let name = obj_get_type_str(ctx.arena, value);
    let _ = write!(sink, "<{}>", String::from_utf8_lossy(name));
}

/// `mp_obj_print` (`obj.zig:464-466`): print to the Python printer
/// (stdout under full config — `MP_PYTHON_PRINTER` resolves to
/// `mp_sys_stdout_print`). `StdoutLock` does not implement
/// `fmt::Write`, so the text is collected first and written to
/// stdout with errors ignored, as the original ignores
/// `mp_printf`'s return.
pub fn obj_print(ctx: &mut CallCtx, types: &BuiltinTypes, value: Value, kind: PrintKind) {
    let mut s = String::new();
    obj_print_helper(ctx, types, value, kind, &mut s);
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(s.as_bytes());
}

// ── array access / index / subscript / buffer ─────────────────────────

/// `mp_obj_get_array` (`obj.zig:547-576`): a tuple/list into a
/// `Vec<Value>`. The Zig `mp_obj_is_tuple_or_list` check first:
/// anything else raises `TypeError: expected tuple/list`.
pub fn obj_get_array(ctx: &mut CallCtx, types: &BuiltinTypes, o: Value) -> Result<Vec<Value>, ObjError> {
    match o {
        Value::Heap(r) => {
            let is_tuple = ctx.arena.header(r).type_ == Some(types.tuple);
            let is_list = ctx.arena.header(r).type_ == Some(types.list);
            if is_tuple || is_list {
                match ctx.arena.get(r) {
                    Obj::Tuple(v) => Ok(v.clone()),
                    Obj::List(l) => Ok(l.items.clone()),
                    _ => Err(ObjError::ExpectedTupleList),
                }
            } else {
                Err(ObjError::ExpectedTupleList)
            }
        }
        _ => Err(ObjError::ExpectedTupleList),
    }
}

/// `mp_obj_get_array_fixed_n` (`obj.zig:578-591`): `get_array` plus
/// a length check (`ValueError: tuple/list has wrong length`).
pub fn obj_get_array_fixed_n(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    o: Value,
    n: usize,
) -> Result<Vec<Value>, ObjError> {
    let v = obj_get_array(ctx, types, o)?;
    if v.len() != n {
        return Err(ObjError::WrongLength);
    }
    Ok(v)
}

/// `mp_get_index` (`obj.zig:593-620`): normalise an index for a
/// sequence of length `len` (`_type` is ignored, as in the original).
/// Negative indices wrap; slices clamp; plain indices raise
/// `IndexError` out of range. An `get_int_maybe` Overflow is
/// preserved (the Zig `pending_exc != 0` check).
pub fn get_index(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    len: usize,
    index: Value,
    is_slice: bool,
) -> Result<usize, ObjError> {
    let mut i: i64 = match index {
        Value::SmallInt(n) => n,
        _ => match obj_get_int_maybe(ctx, types, index)? {
            Some(n) => n,
            None => return Err(ObjError::IndicesMustBeInts),
        },
    };
    let ilen = len as i64;
    if i < 0 {
        i += ilen;
    }
    if is_slice {
        if i < 0 {
            i = 0;
        } else if i > ilen {
            i = ilen;
        }
    } else if i < 0 || i >= ilen {
        return Err(ObjError::IndexOutOfRange);
    }
    Ok(i as usize)
}

/// `mp_obj_subscr` (`obj.zig:622-649`): subscript dispatch.
/// `SubscrArg` encodes the Zig `value` (`NULL` = delete,
/// `SENTINEL` = get). If the type's subscr slot returns a value it
/// wins; a slot "not implemented" falls through. The `list[str]` /
/// `__class_getitem__` branch (full config) needs
/// `runtime.typingCall` (C14/C15) — until then it falls through to
/// the raises below; see the module docs. Do not pin `list[str]` in
/// a test until C14 lands it.
pub fn obj_subscr(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> Result<Value, ObjError> {
    let t = obj_get_type(ctx.arena, types, base);
    match call_subscr(ctx, t, base, index, arg) {
        Ok(SlotOutcome::Value(v)) => return Ok(v),
        Ok(SlotOutcome::NotImplemented) => {}
        Err(e) => return Err(e),
    }
    match arg {
        SubscrArg::Delete => Err(ObjError::NoItemDeletion),
        SubscrArg::Get => Err(ObjError::NotSubscriptable),
        SubscrArg::Set(_) => Err(ObjError::NoItemAssignment),
    }
}

/// `mp_identity` (`obj.zig:651-653`): the identity function. (The
/// `mp_identity_obj` builtin object itself is C10's — it needs the
/// `fun_builtin_1` type.)
pub fn identity(value: Value) -> Value {
    value
}

/// `mp_get_buffer` (`obj.zig:666-677`): the buffer-protocol
/// dispatch. The slot gets `flags & MP_BUFFER_RW` (the RAISE flag is
/// stripped before the call, exactly as in the original); a slot
/// failure falls through to the raise/false decision.
pub fn get_buffer(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    obj: Value,
    bufinfo: &mut BufferInfo,
    flags: u32,
) -> Result<bool, ObjError> {
    let t = obj_get_type(ctx.arena, types, obj);
    if let Some(f) = buffer_slot(ctx.arena, t) {
        match f(ctx, obj, bufinfo, flags & BUFFER_RW) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(PyError::OutOfMemory) => return Err(ObjError::OutOfMemory),
            Err(PyError::PyRaise) => {} // slot failure → fall through
        }
    }
    if flags & BUFFER_RAISE_IF_UNSUPPORTED != 0 {
        return Err(ObjError::BufferProtocol);
    }
    Ok(false)
}

// ── exception printing ────────────────────────────────────────────────

/// Whether `value` is an exception instance (`obj.zig`'s
/// `mp_obj_is_exception_instance`, extern C). Today: heap
/// `Obj::Exception` values only — instances of Python exception
/// subclasses need the instance representation (C12), so this is
/// partial until then (the callers below degrade to "not an
/// exception" for those).
pub fn is_exception_instance(arena: &GcArena, value: Value) -> bool {
    matches!(value, Value::Heap(r) if matches!(arena.get(r), Obj::Exception(_)))
}

/// The `__cause__` of an exception (`npy_exception_get_cause`,
/// `objexcept.zig:273`): the cause slot, or `None` when there is
/// none (the Zig `cause == 0`).
pub fn exception_cause(arena: &GcArena, exc: Value) -> Option<Value> {
    match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(e) => e.cause,
            _ => None,
        },
        _ => None,
    }
}

/// `mp_obj_print_exception` (`obj.zig:377-380`): print the cause
/// chain first, then the traceback, then the exception itself
/// (`PRINT_EXC`), then a newline. `resolve` maps a qstr id to its
/// bytes — traceback file/block names are dynamic qstrs in practice
/// (a file path interned at load time), so the VM passes its
/// `RuntimeQstr` pool here (`&|id| vm.pool.qstr_str(id)`); the
/// static table alone cannot resolve them.
pub fn obj_print_exception<'r>(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    exc: Value,
    resolve: &'r dyn Fn(u32) -> &'r [u8],
    sink: &mut dyn fmt::Write,
) {
    print_exception_chain(ctx, types, exc, resolve, 16, sink);
}

/// The cause first, as CPython prints a `raise ... from` chain;
/// `depth` bounds a cycle (`a.__cause__ = b; b.__cause__ = a`)
/// (`obj.zig:383-417`).
fn print_exception_chain<'r>(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    exc: Value,
    resolve: &'r dyn Fn(u32) -> &'r [u8],
    depth: u8,
    sink: &mut dyn fmt::Write,
) {
    if depth > 0 && is_exception_instance(ctx.arena, exc) {
        if let Some(cause) = exception_cause(ctx.arena, exc) {
            if !cause.is_none() {
                print_exception_chain(ctx, types, cause, resolve, depth - 1, sink);
                let _ = sink.write_str(
                    "\nThe above exception was the direct cause of the following exception:\n\n",
                );
            }
        }
    }
    if is_exception_instance(ctx.arena, exc) {
        if let Value::Heap(r) = exc {
            if let Obj::Exception(e) = ctx.arena.get(r) {
                if !e.traceback.is_empty() {
                    // Frames are stored innermost-first
                    // (`add_traceback` appends while unwinding,
                    // `objexcept.zig:381`); the Zig renderer iterates
                    // from `n - 3` down to 0 — oldest first.
                    // No line numbers: `MICROPY_ENABLE_SOURCE_LINE`
                    // is off at this ROM level (`mpconfig.h:908-909`
                    // — `ROM_LEVEL_AT_LEAST_EXTRA_FEATURES`),
                    // verified on the reference binary
                    // (`File "<stdin>", in <module>`).
                    let _ = sink.write_str("Traceback (most recent call last):\n");
                    for frame in e.traceback.iter().rev() {
                        let file = resolve(frame.name);
                        let _ = write!(sink, "  File \"{}\"", String::from_utf8_lossy(file));
                        match frame.block {
                            Some(b) => {
                                let name = resolve(b);
                                let _ = write!(
                                    sink,
                                    ", in {}\n",
                                    String::from_utf8_lossy(name)
                                );
                            }
                            None => {
                                let _ = sink.write_str("\n");
                            }
                        }
                    }
                }
            }
        }
    }
    obj_print_helper(ctx, types, exc, PrintKind::Exc, sink);
    let _ = sink.write_str("\n");
}

// ── slot-call entry points ────────────────────────────────────────────

/// Call a type's binary-op slot directly (the shared helper for the
/// VM's `mp_binary_op` entry — `obj.zig` itself only uses it
/// internally for the equality dispatch above; the full runtime
/// `mp_binary_op` with its raise paths is C14/C15's).
pub fn obj_binary_op(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    match call_binary(ctx, obj_get_type(ctx.arena, types, lhs), op, lhs, rhs)
        .map_err(ObjError::to_pyerror)?
    {
        SlotOutcome::Value(v) => Ok(v),
        SlotOutcome::NotImplemented => Err(PyError::PyRaise),
    }
}

/// Call a type's unary-op slot directly (same note as
/// `obj_binary_op`).
pub fn obj_unary_op(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    op: UnaryOp,
    arg: Value,
) -> PyResult<Value> {
    match call_unary(ctx, obj_get_type(ctx.arena, types, arg), op, arg)
        .map_err(ObjError::to_pyerror)?
    {
        SlotOutcome::Value(v) => Ok(v),
        SlotOutcome::NotImplemented => Err(PyError::PyRaise),
    }
}
