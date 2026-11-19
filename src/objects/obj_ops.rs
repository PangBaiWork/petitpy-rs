//! Object operations — equality, printing, indexing, exceptions.
//!
//! The second half of the `obj.zig` port (the other half is
//! `obj.rs`: slot dispatch + int/float/complex extraction + len +
//! truth). Everything here dispatches through the `obj.rs` helpers
//! (`call_binary`, `call_subscr`, `obj_get_type`, …); the dependency
//! is one-way (`obj_ops` → `obj`).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{
    buffer_slot, call_binary, call_subscr, call_unary, is_bytes, is_str, obj_get_int_maybe,
    obj_get_type, obj_is_true, print_slot, qstr_bytes, type_data, BuiltinTypes, ObjError,
    SlotOutcome, BUFFER_RAISE_IF_UNSUPPORTED, BUFFER_RW, FLAG_EQ_CHECKS_OTHER_TYPE,
    FLAG_EQ_HAS_NEQ_TEST, FLAG_EQ_NOT_REFLEXIVE, FLAG_INSTANCE_TYPE,
};
use crate::objects::containers::objlist::{is_exact_list, list_items_vec};
use crate::objects::containers::objtuple::{is_exact_tuple, tuple_items};
use crate::objects::objtype::instance::instance_binary_op;
use crate::objects::objtype::seams::with_call_hook_seams;
use crate::objects::slots::{BinaryOp, BufferInfo, CallHook, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;

// ── equality ──────────────────────────────────────────────────────────

/// `mp_obj_str_equal` (vendor `py/objstr.c`): string content
/// equality. qstr/qstr compares by content when both ids are
/// statically resolvable (the static table could in principle hold
/// one string under two ids — the C1 interleave finding — so id
/// equality alone is not proof); a dynamic id compares by id only
/// (the dynamic pool dedups by content, and a dynamic id's content
/// never equals a static id's content, since `from_strn` searches
/// the static table first). Pool-aware (C13b): the mixed
/// qstr/heap arm used to read the qstr bytes through the
/// arena-only `qstr_bytes`, which returns `None` for dynamic ids
/// — so a heap `Str` never equalled a dynamic `Qstr` with the
/// same bytes and `dict([('k', 9)])['k']` raised `KeyError`.
/// `pool.qstr_str` resolves both static and dynamic ids.
fn str_equal(arena: &GcArena, pool: &RuntimeQstr, o1: Value, o2: Value) -> bool {
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
            match arena.get(r) {
                Obj::Str(s) => pool.qstr_str(id as u16) == s.bytes.as_slice(),
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
/// `Warning: Comparison between bytes and str`). Now routed
/// through the central `mp_warning` (`runtime/warning.rs` —
/// the centralisation this comment was waiting for since C8a).
fn warn_str_bytes_cmp() {
    crate::runtime::warning::mp_warning(None, "Comparison between bytes and str");
}

/// `mp_obj_equal_not_equal` (`obj.zig:309-368`): the general
/// equality dispatch. `op` must be EQUAL or NOT_EQUAL (anything
/// else is a caller bug — the original takes a `c_int` op and only
/// these two reach here). `call` is the VM's call hook (E2):
/// `Some` on the VM's equality path, so an instance's `__eq__` /
/// `__ne__` actually runs; `None` on ctx-only paths (the
/// pre-E2 behaviour — the stub slot raises, the loop falls
/// through to pointer comparison).
pub fn obj_equal_not_equal(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    op: BinaryOp,
    o1_in: Value,
    o2_in: Value,
    mut call: Option<CallHook<'_, '_>>,
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
            return Ok(if str_equal(ctx.arena, ctx.pool, o1, o2) {
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

    // E2d — same-kind container equality with a hook: when the
    // VM passes its real call hook and both operands are exact
    // lists or exact tuples, compare element-wise through the
    // hook (a FunBc `__eq__` actually executes). The ctx-only
    // path (`call` is None) keeps the generic loop below, which
    // routes through the type slots and `seq_cmp_objs` exactly
    // as before. Tuple-vs-list and subclasses fall through.
    // Length mismatch decides immediately (`seq_cmp_objs`'s
    // Equal arm); element equality is the recursive EQUAL
    // dispatch, bool-ified like `obj_equal`.
    if call.is_some() {
        let pair: Option<(Vec<Value>, Vec<Value>)> =
            if is_exact_list(ctx, types.list, o1) && is_exact_list(ctx, types.list, o2) {
                Some((list_items_vec(ctx, o1), list_items_vec(ctx, o2)))
            } else if is_exact_tuple(ctx, types.tuple, o1)
                && is_exact_tuple(ctx, types.tuple, o2)
            {
                let l = tuple_items(ctx, o1).map(|s| s.to_vec());
                let r = tuple_items(ctx, o2).map(|s| s.to_vec());
                match (l, r) {
                    (Some(l), Some(r)) => Some((l, r)),
                    _ => None,
                }
            } else {
                None
            };
        if let Some((l_items, r_items)) = pair {
            if l_items.len() != r_items.len() {
                return Ok(local_false);
            }
            for (a, b) in l_items.into_iter().zip(r_items.into_iter()) {
                let eq = obj_equal_not_equal(
                    ctx,
                    types,
                    BinaryOp::Equal,
                    a,
                    b,
                    call.as_deref_mut(),
                )?;
                if !obj_is_true(ctx, types, eq)? {
                    return Ok(local_false);
                }
            }
            return Ok(local_true);
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
                    // The slot invocation (E2): an instance type
                    // runs its dunder through the VM's real call
                    // hook when one is passed (a `FunBc` `__eq__`
                    // / `__ne__` actually executes); otherwise the
                    // type's slot fn (the stub seams for
                    // instances). `Ok(None)` from the instance
                    // path is the slot's NULL — keep looking.
                    let mut invoke = |ctx: &mut CallCtx, try_op: BinaryOp| -> PyResult<Value> {
                        if flags & FLAG_INSTANCE_TYPE != 0 {
                            match call.as_deref_mut() {
                                Some(hook) => {
                                    match with_call_hook_seams(hook, |seams| {
                                        instance_binary_op(ctx, try_op, o1, o2, seams)
                                    }) {
                                        Ok(Some(v)) => Ok(v),
                                        Ok(None) => Err(PyError::PyRaise),
                                        Err(e) => Err(e),
                                    }
                                }
                                None => bin(ctx, try_op, o1, o2),
                            }
                        } else {
                            bin(ctx, try_op, o1, o2)
                        }
                    };
                    // CPython asymmetry: try `__ne__` first for
                    // NOT_EQUAL when the type has a neq test.
                    if op == BinaryOp::NotEqual && flags & FLAG_EQ_HAS_NEQ_TEST != 0 {
                        match invoke(ctx, BinaryOp::NotEqual) {
                            Ok(r) => return Ok(r),
                            Err(PyError::OutOfMemory) => {
                                return Err(PyError::OutOfMemory)
                            }
                            Err(PyError::PyRaise) => {} // NULL → keep looking
                        }
                    }
                    match invoke(ctx, BinaryOp::Equal) {
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
    let r = obj_equal_not_equal(ctx, types, BinaryOp::Equal, o1, o2, None)?;
    obj_is_true(ctx, types, r)
}

/// `mp_obj_equal` with an optional VM hook (G3/H2): the hook
/// threads into `obj_equal_not_equal`, so an instance's FunBc
/// `__eq__` actually runs (the `instance_binary_op` path);
/// without a hook this is exactly `obj_equal`. A raise from the
/// hook propagates as `Err(PyError::PyRaise)` — the probe
/// callers below decide what it means.
pub fn obj_equal_hook(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    o1: Value,
    o2: Value,
    hook: Option<CallHook<'_, '_>>,
) -> PyResult<bool> {
    let r = obj_equal_not_equal(ctx, types, BinaryOp::Equal, o1, o2, hook)?;
    obj_is_true(ctx, types, r)
}

/// The hash-table probe comparison (G3/H2, H5): `obj_equal_hook`
/// with the vendor's `mp_obj_is_true` pending guard
/// (`obj.zig:470` — while a raise is pending, equality reads
/// false). A raise from `__eq__` therefore reads as "not
/// equal" and the probe continues; the stashed raise is
/// DISCARDED here (both the hook's side channel and the
/// `CallCtx` record), never drained — the same convention as
/// `hash_of_hook`'s swallowed `__hash__` raise.
///
/// The discard is needed in BOTH arms: the generic 2-pass
/// loop swallows a slot raise as NULL (`Err(PyError::PyRaise)
/// => {} // NULL → keep looking`) and falls through to the
/// pointer comparison, so a raising `__eq__` comes back as
/// `Ok(b)` with the raise still stashed on the hook — without
/// the Ok-arm drain it would surface later at the next
/// `drain_hook_raise` as the WRONG exception (probed
/// 2026-11-19: a load after a raising `__eq__` raised the
/// stashed `ValueError` past `except KeyError`, and the real
/// `KeyError` only surfaced on the handler's re-raise). When
/// a stash was present the answer is forced `false`, which is
/// the vendor guard exactly. `OutOfMemory` still propagates.
/// (REF note, probed 2026-11-19: a raising `__eq__` is
/// state-dependent on the reference — a load reports `KeyError`
/// (this rule, exactly), while `in` / `.get` surface the
/// `ValueError` and a store surfaces `TypeError` or nothing
/// depending on the table state — no uniform rule reproduces
/// that; the probe-level discard is the documented vendor
/// guard.)
pub fn probe_equal(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    o1: Value,
    o2: Value,
    mut hook: Option<CallHook<'_, '_>>,
) -> PyResult<bool> {
    // Drain both raise channels; true iff a raise was stashed.
    let drain_stash = |hook: &mut Option<CallHook<'_, '_>>, ctx: &mut CallCtx| -> bool {
        let mut raised = false;
        if let Some(h) = hook.as_deref_mut() {
            raised |= h.take_pending().is_some();
        }
        raised |= ctx.take_pending_raise().is_some();
        raised
    };
    match obj_equal_hook(ctx, types, o1, o2, hook.as_deref_mut()) {
        Ok(b) => {
            let raised = drain_stash(&mut hook, ctx);
            Ok(b && !raised)
        }
        Err(PyError::OutOfMemory) => Err(PyError::OutOfMemory),
        Err(PyError::PyRaise) => {
            let _ = drain_stash(&mut hook, ctx);
            Ok(false)
        }
    }
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

/// A value's type name for error messages. `obj_get_type_str`
/// resolves static qstrs only, so a user class renders empty
/// there — but the Zig's `binopUnsupported` (`runtime.zig`)
/// reads the type's name qstr through the pool, and the
/// reference prints `'B'` for a user class. This is the
/// pool-aware form; use it wherever an error message names a
/// type (the sort's `CompareUnsupported`, `binop_unsupported`).
pub fn obj_type_name(ctx: &CallCtx, value: Value) -> String {
    match value {
        Value::SmallInt(_) => "int".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::None => "NoneType".to_string(),
        Value::Qstr(_) => "str".to_string(),
        Value::Heap(r) => match ctx.arena.header(r).type_ {
            Some(t) => match ctx.arena.get(t) {
                Obj::Type(d) => {
                    String::from_utf8_lossy(ctx.pool.qstr_str(d.name as u16)).into_owned()
                }
                _ => String::new(),
            },
            None => String::new(),
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
    sink: &mut dyn crate::objects::slots::ByteSink,
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
/// `mp_sys_stdout_print`). The bytes are collected in a `ByteBuf`
/// first and written to stdout with errors ignored, as the
/// original ignores `mp_printf`'s return. G2 (P7a): a `ByteBuf`,
/// not a `String` — a non-UTF-8 `str` prints byte-exact.
/// P14: through the buffered stdout (glibc stdio buffering),
/// not a per-call write-through.
pub fn obj_print(ctx: &mut CallCtx, types: &BuiltinTypes, value: Value, kind: PrintKind) {
    let mut s = crate::objects::slots::ByteBuf::new();
    obj_print_helper(ctx, types, value, kind, &mut s);
    crate::support::output::buffered_stdout_write(&s.0);
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
/// `mp_obj_is_exception_instance`, extern C): a native
/// `Obj::Exception` payload, or an `Instance` of a user
/// exception subclass (E2g — those are `Instance` payloads
/// now, recognised by a ROM exception type in the header
/// type's MRO, the `is_exception_type_obj` slot identity).
pub fn is_exception_instance(arena: &GcArena, value: Value) -> bool {
    match value {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(_) => true,
            Obj::Instance(_) => {
                let Some(t) = arena.header(r).type_ else {
                    return false;
                };
                match arena.get(t) {
                    Obj::Type(d) => d.mro.iter().any(|m| {
                        crate::objects::objtype::objexcept::is_exception_type_obj(arena, *m)
                    }),
                    _ => false,
                }
            }
            _ => false,
        },
        _ => false,
    }
}

/// The `__cause__` of an exception (`npy_exception_get_cause`,
/// `objexcept.zig:273`): the cause slot, or `None` when there is
/// none (the Zig `cause == 0`). A user subclass instance reads
/// through its native exception object in subobj[0] (E2g).
pub fn exception_cause(arena: &GcArena, exc: Value) -> Option<Value> {
    match crate::objects::objtype::objexcept::native_exception_ref(arena, exc) {
        Some(r) => match arena.get(r) {
            Obj::Exception(e) => e.cause,
            _ => None,
        },
        None => None,
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
    sink: &mut dyn crate::objects::slots::ByteSink,
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
    sink: &mut dyn crate::objects::slots::ByteSink,
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
        // Frames are stored innermost-first
        // (`add_traceback` appends while unwinding,
        // `objexcept.zig:381`); the Zig renderer iterates
        // from `n - 3` down to 0 — oldest first.
        // No line numbers: `MICROPY_ENABLE_SOURCE_LINE`
        // is off at this ROM level (`mpconfig.h:908-909`
        // — `ROM_LEVEL_AT_LEAST_EXTRA_FEATURES`),
        // verified on the reference binary
        // (`File "<stdin>", in <module>`).
        // E2g: read through `exception_get_traceback` — a
        // user subclass instance keeps its frames on the
        // native exception object in subobj[0].
        let tb = crate::objects::objtype::objexcept::exception_get_traceback(ctx.arena, exc);
        if !tb.is_empty() {
            let _ = sink.write_str("Traceback (most recent call last):\n");
            for frame in tb.iter().rev() {
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
        .map_err(|e| ctx.record_typed(e))?
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
        .map_err(|e| ctx.record_typed(e))?
    {
        SlotOutcome::Value(v) => Ok(v),
        SlotOutcome::NotImplemented => Err(PyError::PyRaise),
    }
}
