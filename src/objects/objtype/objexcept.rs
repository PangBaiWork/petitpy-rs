//! Exception objects + predicates (`objexcept.zig`, 581 lines, 0 test
//! blocks; the Zig port of vendor `py/objexcept.c`).
//!
//! What lives here: the exception constructors (`exception_make_new`,
//! `new_exception[_msg/_args]`), the attribute accessors
//! (`exception_attr` load + `exception_store_attr` store),
//! the print slot (`exception_print`, incl. the OSError
//! `"[Errno N] name"` form and its 22-entry errno table from
//! `moderrno.zig`), the traceback helpers, and the predicates
//! (`is_exception_type` / `is_exception_instance` /
//! `exception_match` / `is_native_exception_instance`).
//!
//! The 28 ROM exception types + the hierarchy builder live in
//! `objexcept_types.rs`; the module/object types are siblings
//! (`objmodule.rs`, `objobject.rs`).
//!
//! Zig-isms ported exactly:
//! - `mp_obj_exception_make_new` sig is `(0, 0xffff, false)` —
//!   kwargs are a terse mismatch (`ValueError(x=1)`), positionals
//!   are stored verbatim as `args` (`makeArgsTuple`).
//! - `__cause__` / `__suppress_context__`: the cause slot is 0
//!   (`None`) on a fresh exception → `__cause__` is None and
//!   `__suppress_context__` is False; `raise ... from X` stores X
//!   (suppress True); `raise ... from None` stores `None`
//!   explicitly (`Some(Value::None)` — suppress True, cause None).
//! - `__context__` is ALWAYS None ("implicit chaining is not
//!   recorded", `objexcept.zig:174`); the VM does not record it.
//! - `__traceback__` has NO load branch in the Zig — reading it
//!   raises AttributeError even after a real raise; storing
//!   `__traceback__ = None` clears the traceback.
//! - `mp_obj_exception_print`: `str` = the args form (0 args → "",
//!   1 arg → that arg printed with STR, >1 → the tuple form);
//!   `repr` = type name + tuple form (note the trailing comma:
//!   `repr(ValueError('x')) == "ValueError('x',)"`); `exc` =
//!   name + ": " + args form. OSError only in the STR/EXC form:
//!   exact-type OSError, 1–2 args, args[0] a small int whose errno
//!   is in the table → `"[Errno N] NAME"` (+ `": msg"` with 2 args);
//!   unknown errno / 3 args fall back to the tuple form.
//! - `PRINT_EXC_SUBCLASS` (0x80) is set only by vendor
//!   `objtype.c:271` when printing a *user-subclass* exception's
//!   inner native exception (skips the type-name prefix). The Rust
//!   spelling is the `is_subclass` parameter of
//!   `exception_print_inner`, set only by `instance_print`'s
//!   exception-subclass branch (wired at the C12a merge — the
//!   `Instance` payload exists now); the print slot itself
//!   always takes the `!is_subclass` branch.
//!
//! Config gates (full config — the only config this port targets):
//! - `MICROPY_PY_ERRNO = 1` (default on, `build.zig:96,122`) → the
//!   OSError branch is live; the errno table is copied from
//!   `moderrno.zig`'s `errorcode_table` (22 entries, values from
//!   vendor `py/mperrno.h`). The D lane owns the `errno` module +
//!   dict; this table is the print-side lookup only, same source.
//! - `MICROPY_PY_ASYNC_AWAIT = 1` → `StopAsyncIteration` exists.
//! - `MICROPY_PY_BUILTINS_STR_UNICODE = 1` → `UnicodeError` exists.
//! - `MICROPY_EMIT_NATIVE = 0` → no `ViperTypeError`.
//! - `MICROPY_ENABLE_EMERGENCY_EXCEPTION_BUF = 0` → the
//!   emergency-buf fallback in `allocExc` is dead: Rust allocation
//!   is infallible (`CallCtx::alloc` returns `GcRef`), so there is
//!   no fallback to port.
//!
//! Divergences (documented, not silent):
//! - `e.args` returns a FRESH tuple on every access (the Zig
//!   returns the stored tuple object; `ExceptionData.args` is a
//!   `Vec<Value>` in the frozen interface, so identity cannot be
//!   preserved — `e.args is e.args` is False in Rust, True in the
//!   Zig). Value-equal in all cases.
//! - A 0-arg exception's `args` is a fresh empty tuple, not the
//!   ROM `()` singleton (`makeArgsTuple(0)` returns
//!   `&mp_const_empty_tuple_obj`); the singleton is C12a's ROM
//!   work — `e.args == ()` still holds, only identity differs.
//! - `exception_make_new`'s arg-count check returns `Err`
//!   immediately (the Zig's `mp_arg_check_num_sig` is polled-only,
//!   but the make_new has no later step that could overwrite the
//!   pending error — the observable outcome is identical).
//! - `is_exception_type` / `is_exception_instance` take the
//!   `BaseException` type ref explicitly (C5 precedent — no
//!   globals): the Zig's make_new-slot-identity branch is subsumed
//!   by the MRO branch, since every type whose make_new IS
//!   `exception_make_new` inherits it from `BaseException` and
//!   therefore has it in its MRO.
//! - `exception_match` on an immediate value returns `false`
//!   (immediates are never exception instances; the only
//!   `true` case would need an immediate's type as the except
//!   clause, which the VM rejects earlier via `is_exception_type`).

use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{MakeNewFn, PrintKind};
use crate::objects::value::{ExceptionData, GcRef, Obj, StrData, TraceFrame, Value};
use crate::pyerr::{PyError, PyResult};

/// The terse `TypeError: argument num/types mismatch` text
/// (vendor `py/argcheck.c:141`, C8a).
pub(crate) const ARG_NUM_TYPES_MISMATCH: &str = "argument num/types mismatch";

/// Errors an exception operation can raise. The variant IS the
/// exception type; `TypeError` carries the TERSE message literal.
/// `PyRaise` is the C6/C7r slot-boundary carrier. (DictError
/// precedent, `objdict.rs:109`.)
#[derive(Clone, Debug, PartialEq)]
pub enum ExcError {
    /// `TypeError(msg)`.
    TypeError(&'static str),
    /// A slot raised (data dropped at the boundary).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl ExcError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            ExcError::TypeError(_) => "TypeError",
            ExcError::PyRaise => "",
            ExcError::OutOfMemory => "MemoryError",
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention; C13 constructs the exception
    /// object from the `ExcError` instead).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ExcError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for ExcError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        match self {
            ExcError::TypeError(m) => Some(m.to_string()),
            _ => None,
        }
    }
    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
    }
}

/// Arity check for a `BuiltinFn` adapter (`objset.rs:204`
/// precedent): exactly the Zig `mp_arg_check_num_sig` outcome for
/// a fixed `(min, max, false)` sig — terse mismatch outside it.
pub(crate) fn check_arity(args: &[Value], min: usize, max: usize) -> Result<(), ExcError> {
    if args.len() < min || args.len() > max {
        return Err(ExcError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    Ok(())
}

/// `mp_obj_exception_make_new` (`objexcept.zig:316`): build an
/// exception instance of `type_` from the constructor args.
/// `args` is the Zig layout — positionals only; `n_kw` counts the
/// kwargs (interleaved pairs after the positionals). Sig
/// `(0, 0xffff, false)`: any kwargs → terse mismatch.
pub fn exception_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, ExcError> {
    if n_kw > 0 || args.len() > 0xffff {
        return Err(ExcError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let n_pos = args.len().checked_sub(n_kw * 2).ok_or(ExcError::TypeError(
        ARG_NUM_TYPES_MISMATCH,
    ))?;
    Ok(Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Exception(ExceptionData {
            type_,
            args: args[..n_pos].to_vec(),
            traceback: Vec::new(),
            cause: None,
        }),
    )))
}

/// `mp_obj_exception_make_new` as a `MakeNewFn` slot (the
/// exception types' `make_new` — every ROM exception type built
/// by `exc_type_data` carries this). The `ExcError` maps to
/// `PyError` via `ExcError::to_pyerror` (the slot-boundary
/// convention: the raise message is dropped until C13 adds
/// `pending_exc`, as with every other slot).
pub fn exception_make_new_slot(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> PyResult<Value> {
    exception_make_new(ctx, type_, args, n_kw).map_err(ExcError::to_pyerror)
}

/// `mp_obj_new_exception` (`objexcept.zig:416`): no-arg exception.
pub fn new_exception(ctx: &mut CallCtx, exc_type: GcRef) -> Value {
    exception_make_new(ctx, exc_type, &[], 0).expect("0-arg make_new cannot fail")
}

/// `mp_obj_new_exception_args` (`objexcept.zig:420`): exception
/// with positional args.
pub fn new_exception_args(ctx: &mut CallCtx, exc_type: GcRef, args: &[Value]) -> Value {
    exception_make_new(ctx, exc_type, args, 0).expect("positional make_new cannot fail")
}

/// `mp_obj_new_exception_msg` (`objexcept.zig:332`): exception
/// with a single string message. The Zig builds the message `str`
/// inline (with its qstr hash — Rust `StrData` carries no hash).
pub fn new_exception_msg(ctx: &mut CallCtx, exc_type: GcRef, msg: &str) -> Value {
    let str_type = ctx.types.str_;
    let msg_obj = ctx.alloc(
        Some(str_type),
        Obj::Str(StrData {
            bytes: msg.as_bytes().to_vec(),
        }),
    );
    new_exception_args(ctx, exc_type, &[Value::Heap(msg_obj)])
}

// ── traceback ────────────────────────────────────────────────────────

/// `mp_obj_exception_add_traceback` (`objexcept.zig:381`): append
/// a (file, line, block) frame. No limit
/// (`MICROPY_PY_SYS_TRACEBACKLIMIT = 0` → `tracebackMaxEntries`
/// returns -1). `block == 0` is `MP_QSTRnull` → `None`.
pub fn exception_add_traceback(ctx: &mut CallCtx, exc: Value, file: u32, line: u32, block: u32) {
    if let Value::Heap(r) = exc {
        if let Obj::Exception(e) = ctx.arena.get_mut(r) {
            e.traceback.push(TraceFrame {
                name: file,
                line,
                block: if block == 0 { None } else { Some(block) },
            });
        }
    }
}

/// `mp_obj_exception_get_traceback` (`objexcept.zig:140`): the
/// stored frames (innermost first — the renderer iterates in
/// reverse; `obj::print_exception`).
pub fn exception_get_traceback<'a>(arena: &'a GcArena, exc: Value) -> &'a [TraceFrame] {
    match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(e) => &e.traceback,
            _ => &[],
        },
        _ => &[],
    }
}

/// `mp_obj_exception_clear_traceback` (`objexcept.zig:126`).
pub fn exception_clear_traceback(ctx: &mut CallCtx, exc: Value) {
    if let Value::Heap(r) = exc {
        if let Obj::Exception(e) = ctx.arena.get_mut(r) {
            e.traceback.clear();
        }
    }
}

/// `mp_obj_exception_get_value` (`objexcept.zig:130`): `args[0]`,
/// or None when there are no args (`value` / `errno` aliases read
/// through here too).
pub fn exception_get_value(arena: &GcArena, exc: Value) -> Value {
    match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(e) => e.args.first().copied().unwrap_or(Value::None),
            _ => Value::None,
        },
        _ => Value::None,
    }
}

// ── cause ────────────────────────────────────────────────────────────

/// `npy_exception_set_cause` (`objexcept.zig:268`): record
/// `raise exc from cause`. A no-op for exceptions without a slot —
/// in Rust every `ExceptionData` HAS the slot, so this always
/// stores (`None` = no cause recorded; `Some(Value::None)` =
/// explicitly `raise ... from None`).
pub fn exception_set_cause(ctx: &mut CallCtx, exc: Value, cause: Value) {
    if let Value::Heap(r) = exc {
        if let Obj::Exception(e) = ctx.arena.get_mut(r) {
            e.cause = Some(cause);
        }
    }
}

/// `npy_exception_get_cause` (`objexcept.zig:273`): the cause
/// recorded by `raise ... from`, or None when there is none (the
/// Zig `cause == 0`).
pub fn exception_get_cause(arena: &GcArena, exc: Value) -> Value {
    match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(e) => e.cause.unwrap_or(Value::None),
            _ => Value::None,
        },
        _ => Value::None,
    }
}

// ── attribute access ─────────────────────────────────────────────────

fn qstr(name: &[u8]) -> u32 {
    static_qstr_id(name).unwrap_or_else(|| panic!("{} is a static qstr", String::from_utf8_lossy(name)))
}

/// The args tuple for `e.args` — a FRESH tuple per access (see the
/// module docs: the Zig returns the stored tuple object).
fn args_tuple(ctx: &mut CallCtx, args: &[Value]) -> Value {
    let tuple_type = ctx.types.tuple;
    Value::Heap(ctx.alloc(Some(tuple_type), Obj::Tuple(args.to_vec())))
}

/// `mp_obj_exception_attr` (`objexcept.zig:148`), LOAD side: the
/// `AttrFn` the type's attr slot calls (C12a wires it — `TypeData`
/// has no `attr` field yet). Returns `Ok(None)` for `__traceback__`
/// and for anything unhandled — the Zig has no load branch for
/// either, so the caller raises AttributeError.
pub fn exception_attr(ctx: &mut CallCtx, exc: Value, attr: u32) -> PyResult<Option<Value>> {
    let data = match exc {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Exception(e) => e,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    if attr == qstr(b"__cause__") || attr == qstr(b"__suppress_context__") {
        let cause = data.cause.unwrap_or(Value::None);
        if attr == qstr(b"__cause__") {
            return Ok(Some(cause));
        }
        return Ok(Some(Value::Bool(data.cause.is_some())));
    }
    if attr == qstr(b"__context__") {
        // Implicit chaining is not recorded (objexcept.zig:174).
        return Ok(Some(Value::None));
    }
    if attr == qstr(b"args") {
        let args = data.args.clone();
        return Ok(Some(args_tuple(ctx, &args)));
    }
    if attr == qstr(b"value") || attr == qstr(b"errno") {
        // Aliases for args[0] (StopIteration.value / OSError.errno).
        return Ok(Some(exception_get_value(ctx.arena, exc)));
    }
    Ok(None)
}

/// `mp_obj_exception_attr`, STORE side (`objexcept.zig:152-160`):
/// `__traceback__ = None` clears the traceback;
/// `__cause__ = v` stores the cause. `value: None` means "delete"
/// (`dest[1] == NULL` — the Zig ignores it for both attrs).
/// Returns `true` when handled; `false` means the store falls
/// through to the caller's AttributeError. The store path is NOT
/// expressible through the load-only `AttrFn` — C13/C14's
/// attribute-store machinery calls this directly.
pub fn exception_store_attr(
    ctx: &mut CallCtx,
    exc: Value,
    attr: u32,
    value: Option<Value>,
) -> PyResult<bool> {
    if attr == qstr(b"__traceback__") && value == Some(Value::None) {
        exception_clear_traceback(ctx, exc);
        return Ok(true);
    }
    if attr == qstr(b"__cause__") {
        if let Some(v) = value {
            exception_set_cause(ctx, exc, v);
            return Ok(true);
        }
    }
    Ok(false)
}

// ── printing ─────────────────────────────────────────────────────────

/// `mp_errno_to_str` (`moderrno.zig:165`): the errorcode dict
/// (`moderrno.zig:72-94`, 22 entries, values from vendor
/// `py/mperrno.h`) as a static table — the print-side lookup only.
/// Returns the errno NAME (`"ENOENT"`), or `None` for unknown
/// errnos (the Zig's `MP_QSTRnull` — the OSError branch falls
/// through to the tuple form).
pub fn errno_name(errno: i64) -> Option<&'static str> {
    // (errno, name), in moderrno.zig errorcode_table order.
    const TABLE: &[(i64, &str)] = &[
        (1, "EPERM"),
        (2, "ENOENT"),
        (5, "EIO"),
        (9, "EBADF"),
        (11, "EAGAIN"),
        (12, "ENOMEM"),
        (13, "EACCES"),
        (17, "EEXIST"),
        (19, "ENODEV"),
        (21, "EISDIR"),
        (22, "EINVAL"),
        (95, "EOPNOTSUPP"),
        (98, "EADDRINUSE"),
        (103, "ECONNABORTED"),
        (104, "ECONNRESET"),
        (105, "ENOBUFS"),
        (107, "ENOTCONN"),
        (110, "ETIMEDOUT"),
        (111, "ECONNREFUSED"),
        (113, "EHOSTUNREACH"),
        (114, "EALREADY"),
        (115, "EINPROGRESS"),
    ];
    TABLE.iter().find(|(n, _)| *n == errno).map(|(_, name)| *name)
}

/// Is the exception's header type exactly `OSError`? The Zig
/// checks `o.base.type == &mp_type_OSError` — exact type, not a
/// subclass — spelled here as a type-NAME check (the
/// `is_ordered_dict_type` precedent: only the `OSError` type
/// object carries that name).
fn is_oserror_type(ctx: &CallCtx, exc: Value) -> bool {
    let Some(t) = header_type(ctx.arena, exc) else {
        return false;
    };
    let name = match ctx.arena.get(t) {
        Obj::Type(td) => td.name,
        _ => return false,
    };
    ctx.pool.qstr_str(name as u16) == b"OSError"
}

/// `mp_obj_exception_print` (`objexcept.zig:184`): the exception
/// type's print slot. `Json`/`Raw` fall through to the tuple
/// form exactly as the Zig's `k` dispatch does (only `Str`/`Exc`
/// take the args form; only `Repr`/`Exc` print the type name).
/// `is_subclass` is the Zig's `PRINT_EXC_SUBCLASS` flag
/// (`objexcept.zig:192-195`): set only by `instance_print`'s
/// exception-subclass branch (objtype.zig:1659-1672), which has
/// already printed the subclass name — the inner print then
/// omits its own type name. The print slot itself always calls
/// with `false`.
pub fn exception_print(ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    exception_print_inner(ctx, value, kind, false, sink);
}

/// The `mp_obj_exception_print` body with the `PRINT_EXC_SUBCLASS`
/// flag spelled out (see `exception_print`).
pub fn exception_print_inner(
    ctx: &mut CallCtx,
    value: Value,
    kind: PrintKind,
    is_subclass: bool,
    sink: &mut dyn std::fmt::Write,
) {
    let (args, type_name): (Vec<Value>, String) = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Exception(e) => {
                let name = header_type(ctx.arena, value)
                    .and_then(|t| match ctx.arena.get(t) {
                        Obj::Type(td) => Some(td.name),
                        _ => None,
                    })
                    .map(|id| String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned())
                    .unwrap_or_default();
                (e.args.clone(), name)
            }
            _ => return,
        },
        _ => return,
    };
    let types = ctx.types;
    if !is_subclass && (kind == PrintKind::Repr || kind == PrintKind::Exc) {
        let _ = sink.write_str(&type_name);
    }
    if kind == PrintKind::Exc {
        let _ = sink.write_str(": ");
    }
    if kind == PrintKind::Str || kind == PrintKind::Exc {
        if args.is_empty() {
            return;
        }
        // OSError: "[Errno N] NAME" (+ ": msg" with 2 args).
        if is_oserror_type(ctx, value) && args.len() < 3 {
            if let Value::SmallInt(n) = args[0] {
                if let Some(name) = errno_name(n) {
                    let _ = write!(sink, "[Errno {}] {}", n, name);
                    if args.len() > 1 {
                        let _ = sink.write_str(": ");
                        obj_print_helper(ctx, types, args[1], PrintKind::Str, sink);
                    }
                    return;
                }
            }
        }
        if args.len() == 1 {
            obj_print_helper(ctx, types, args[0], PrintKind::Str, sink);
            return;
        }
    }
    // Tuple form (repr, multi-arg str/exc, json/raw, OSError
    // fall-through): the args as a tuple, items always repr
    // (the Rust tuple print ignores kind — the vendor tuple print
    // does the same).
    let tuple = args_tuple(ctx, &args);
    obj_print_helper(ctx, types, tuple, kind, sink);
}

// ── predicates ───────────────────────────────────────────────────────

/// The header type of a heap value (`None` for immediates and for
/// header-less bootstrapping objects).
fn header_type(arena: &GcArena, value: Value) -> Option<GcRef> {
    match value {
        Value::Heap(r) => arena.header(r).type_,
        _ => None,
    }
}

/// `mp_obj_is_subclass_fast` for a plain class argument
/// (`objtype.zig:409`): MRO membership. C12a's general version
/// (tuple classinfo, subclass-of-type) subsumes this; this lane
/// needs only the single-class walk for the predicates below.
pub fn type_is_subclass_of(arena: &GcArena, type_: GcRef, base: GcRef) -> bool {
    match arena.get(type_) {
        Obj::Type(td) => td.mro.contains(&base),
        _ => false,
    }
}

/// `mp_obj_is_exception_type` (`objexcept.zig:105`): the value's
/// type is an exception type — i.e. `BaseException` is in its MRO.
/// (The Zig's make_new-slot branch is subsumed: see module docs.)
/// Immediates are never exception types.
pub fn is_exception_type(arena: &GcArena, value: Value, base_exc: GcRef) -> bool {
    match value {
        Value::Heap(r) => match arena.get(r) {
            Obj::Type(_) => type_is_subclass_of(arena, r, base_exc),
            _ => match header_type(arena, value) {
                Some(t) => type_is_subclass_of(arena, t, base_exc),
                None => false,
            },
        },
        _ => false,
    }
}

/// `mp_obj_is_exception_instance` (`objexcept.zig:115`): the
/// value's type is an exception type.
pub fn is_exception_instance(arena: &GcArena, value: Value, base_exc: GcRef) -> bool {
    match header_type(arena, value) {
        Some(t) => type_is_subclass_of(arena, t, base_exc),
        None => false,
    }
}

/// `mp_obj_is_native_exception_instance` (`objexcept.zig:101`):
/// a native exception instance (payload is `Obj::Exception` and
/// its type is an exception type). Python-subclass instances
/// (`Obj::Instance` + `subobj[0]`) do not exist yet — when C12a
/// lands the instance representation, this must grow the
/// `getNativeException` branch.
pub fn is_native_exception_instance(arena: &GcArena, value: Value, base_exc: GcRef) -> bool {
    matches!(value, Value::Heap(r) if matches!(arena.get(r), Obj::Exception(_)))
        && is_exception_instance(arena, value, base_exc)
}

/// Whether `type_` matches `classinfo` — a class, or a tuple of
/// classes (any element matching counts), the
/// `mp_obj_is_subclass_fast` classinfo rule.
fn classinfo_matches(arena: &GcArena, type_: GcRef, classinfo: Value) -> bool {
    match classinfo {
        Value::Heap(r) => match arena.get(r) {
            Obj::Type(_) => type_is_subclass_of(arena, type_, r),
            Obj::Tuple(items) => items.iter().any(|v| match v {
                Value::Heap(rr) => {
                    matches!(arena.get(*rr), Obj::Type(_))
                        && type_is_subclass_of(arena, type_, *rr)
                }
                _ => false,
            }),
            _ => false,
        },
        _ => false,
    }
}

/// `mp_obj_exception_match` (`objexcept.zig:118`): the exception
/// (or its type) is a subclass of `exc_type`.
pub fn exception_match(arena: &GcArena, exc: Value, exc_type: Value) -> bool {
    let t = match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Type(_) => r,
            _ => match header_type(arena, exc) {
                Some(t) => t,
                None => return false,
            },
        },
        // Immediates are never exception instances (see module docs).
        _ => return false,
    };
    classinfo_matches(arena, t, exc_type)
}

/// `is_exception` (`objexcept.zig:87`): a TYPE is an exception
/// type iff its make_new slot IS `mp_obj_exception_make_new`.
/// `TypeData` gained the make_new slot with C12a, so the Rust
/// spelling is now the faithful one: the type's make_new fn item
/// is `exception_make_new` (bare fn items compare with
/// `fn_addr_eq`, C9c precedent). (Before C12a, this was spelled
/// as a print-slot identity — `exception_print` — because there
/// was no make_new field to compare.)
pub fn is_exception_type_obj(arena: &GcArena, type_: GcRef) -> bool {
    match arena.get(type_) {
        Obj::Type(td) => match td.make_new {
            Some(f) => std::ptr::fn_addr_eq(f, exception_make_new_slot as MakeNewFn),
            None => false,
        },
        _ => false,
    }
}
