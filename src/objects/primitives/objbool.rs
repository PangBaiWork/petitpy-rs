//! Bool type — `npy_bool_fn_*` (`objbool.zig`, 83 lines, full port
//! of vendor `py/objbool.c`).
//!
//! `mp_type_bool` ROM: flags `0x0008`
//! (`MP_TYPE_FLAG_EQ_CHECKS_OTHER_TYPE`), name `bool`, slots
//! make_new=1, print=2, unary_op=3, binary_op=4, locals_dict=0 (NULL
//! — no methods). Print is unconditional `"True"` / `"False"`
//! (PY_JSON=0, ignores kind). Bool is a `Value::Bool` immediate (C0).
//!
//! bool is NOT an int subclass (see `objint.rs`) — the relationship
//! is arithmetic delegation only: the unary/binary slots convert to
//! small int 0/1 and delegate to the int logic.

use crate::memory::gc::CallCtx;
use crate::objects::obj::BuiltinTypes;
use crate::objects::primitives::objint::{int_binary_op, int_unary_op};
use crate::objects::primitives::ops::{
    prim_to_pyresult, static_qstr_id, PrimError, PrimOutcome, PrimResult,
};
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{GcRef, TypeData, Value};
use crate::pyerr::PyResult;

/// `mp_type_bool` ROM data. `dict` is the bootstrap's empty
/// placeholder (the original has no `locals_dict`). The op slots are
/// the C7r adapters below; `call` / `getiter` / `subscr` / `buffer`
/// stay `None` — the original's bool type has no such slots
/// (`make_new` is the type-call path, wired by C12).
pub fn bool_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"bool").expect("MP_QSTR_bool is in the static table"),
        flags: 0x0008,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(bool_unary_slot),
        binary_op: Some(bool_binary_slot),
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(bool_print),
    }
}

/// `npy_bool_fn_print`: `"True"` / `"False"`.
pub fn bool_to_string(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// `npy_bool_fn_print` (`objbool.zig`): `kind` is ignored (PY_JSON=0)
/// — prints `"True"` / `"False"` unconditionally. A non-bool value
/// writes nothing (the slot is only invoked for bool values).
pub fn bool_print(
    _ctx: &mut CallCtx<'_>,
    v: Value,
    _kind: PrintKind,
    w: &mut dyn std::fmt::Write,
) {
    if let Value::Bool(b) = v {
        let _ = w.write_str(bool_to_string(b));
    }
}

/// `npy_bool_fn_make_new`, sig (0, 1, false): 0 args → `False`,
/// else `newBool(mp_obj_is_true(args[0]))`. Same polled arg-check
/// rule as int: the branch's own error wins, else a wrong count
/// surfaces as `ArgNumTypesMismatch`. Truth is C0's `Value::is_true`
/// (payload table — complete for builtins; qstr `""` needs C6's
/// length fix, which merges first).
pub fn bool_make_new(
    ctx: &mut CallCtx<'_>,
    _types: &BuiltinTypes,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, PrimError> {
    let arg_ok = args.len() <= 1 && n_kw == 0;
    let r = match args.len() {
        0 => Ok(Value::Bool(false)),
        _ => {
            // C0's `is_true` is payload-only and infallible in this
            // tree; the `Err` arm exists only because `is_true`
            // returns `PyResult` for C12's `__bool__` / `__len__`
            // slots (whose data travels on C13's `pending_exc`
            // channel). Unreachable here.
            let Ok(truth) = args[0].is_true(ctx.arena) else {
                return Err(PrimError::OutOfMemory);
            };
            Ok(Value::Bool(truth))
        }
    };
    match r {
        Err(e) => Err(e),
        Ok(v) => {
            if arg_ok {
                Ok(v)
            } else {
                Err(PrimError::ArgNumTypesMismatch)
            }
        }
    }
}

/// `npy_bool_fn_unary_op`: `LEN` → NULL, else bool → small int 0/1
/// and delegate to `mp_unary_op` (the small-int fast path + int
/// slot, which `int_unary_op` implements). `NOT` is answered by the
/// generic `mp_unary_op` before the slot, so it is `NotHandled` here.
pub fn bool_unary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: UnaryOp,
    v: Value,
) -> PrimResult {
    if op == UnaryOp::Len {
        return Ok(PrimOutcome::NotHandled);
    }
    let n = match v {
        Value::Bool(b) => Value::SmallInt(if b { 1 } else { 0 }),
        _ => return Ok(PrimOutcome::NotHandled),
    };
    int_unary_op(ctx, types, op, n)
}

/// Thin adapter to the frozen `UnaryOpFn` slot signature
/// (`slots.rs`) — same conversion as `int_unary_slot`.
pub fn bool_unary_slot(ctx: &mut CallCtx<'_>, op: UnaryOp, v: Value) -> PyResult<Value> {
    let types = ctx.types;
    prim_to_pyresult(bool_unary_op(ctx, types, op, v))
}

/// `npy_bool_fn_binary_op`: bool → small int 0/1, delegate to
/// `mp_binary_op` — i.e. to the int slot logic (`int_binary_op`).
/// Float/complex rhs and `MULTIPLY` with a sequence rhs are
/// `NotHandled` for the same C14/C18 reasons as the int slot (see
/// `int_binary_op`).
pub fn bool_binary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PrimResult {
    let n = match lhs {
        Value::Bool(b) => Value::SmallInt(if b { 1 } else { 0 }),
        _ => return Ok(PrimOutcome::NotHandled),
    };
    int_binary_op(ctx, types, op, n, rhs)
}

/// Thin adapter to the frozen `BinaryOpFn` slot signature
/// (`slots.rs`) — same conversion as `int_unary_slot`.
pub fn bool_binary_slot(
    ctx: &mut CallCtx<'_>,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    prim_to_pyresult(bool_binary_op(ctx, types, op, lhs, rhs))
}
