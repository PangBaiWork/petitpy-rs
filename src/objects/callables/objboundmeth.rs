//! Bound methods (`objboundmeth.zig`, 98 lines).
//!
//! Ports, in order:
//! - `bound_meth_call` (`objboundmeth.zig:24-27`): delegates to
//!   `mp_call_method_self_n_kw(meth, self, n_args, n_kw, args)`
//!   (`runtime.zig:856-...` — `mpCallMethodSelfNKwImpl` prepends
//!   `self` to the args and calls the underlying fun with
//!   `n_args + 1`). The builtin-fun path is live (sig dispatch via
//!   `call_fun_builtin`); the `FunBc` path goes through
//!   `fun_bc_call` with the runner seam (see `objfun.rs`).
//! - `bound_meth_unary_op` (`:29-34`): HASH only —
//!   `newSmallInt(self_obj ^ meth)` over the REPR_A words;
//!   every other op returns NULL (not implemented).
//! - `bound_meth_binary_op` (`:36-46`): EQUAL only. Full config
//!   (`MICROPY_PY_BOUND_METHOD_FULL_EQUALITY_CHECK=1`) compares
//!   both parts with `mp_obj_equal`; nano would use pointer
//!   comparison. `FLAG_EQ_CHECKS_OTHER_TYPE` is clear, so the
//!   caller guarantees both sides are bound methods — the Rust
//!   slot still guards the payloads and returns `PyRaise` for a
//!   non-bound-method rhs. NOT_EQUAL is not a slot op (the VM
//!   inverts EQUAL — C9b finding).
//! - `mp_obj_new_bound_meth` (`:48-55`).
//! - `bound_meth_attr` (`:57-62`): `mp_load_method_maybe` on the
//!   underlying fun (see `load_attr_on_fun`, `objfun.rs`).
//!
//! No print slot: `ERROR_REPORTING=TERSE` under nano, and the
//! full config keeps it that way — `repr(bound_method)` is the
//! VM's default `"<bound_method>"` (probed 2026-11-17), not a
//! slot in this lane.

use crate::compiler::mpy::RawCode;
use crate::memory::gc::CallCtx;
use crate::objects::obj::obj_id;
use crate::objects::obj_ops::obj_equal;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{BoundMethData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::bc::CodeState;

use super::objfun::{call_fun_builtin, fun_bc_call, load_attr_on_fun, RunOutcome};

/// Read a `BoundMethData` out of a heap `Value`. `None` when the
/// value is not a heap bound method — the Zig's `fromObj` would
/// read garbage; Rust returns `PyRaise` instead (caller-contract
/// guard).
fn bound_meth_data(ctx: &CallCtx, self_value: Value) -> Option<BoundMethData> {
    match self_value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::BoundMeth(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ── `bound_meth_call` (`objboundmeth.zig:24-27`) ─────────────────

/// The full bound-method call: prepend `self` (`args2[0]`) and
/// call the underlying fun with `n_args + 1` positionals
/// (`mpCallMethodSelfNKwImpl`, `runtime.zig:856`). `code` /
/// `qstr_table` / `empty_tuple` are resolved by the caller (C13's
/// dispatcher) for the `FunBc` path; the builtin path needs none
/// of them. A non-function underlying value returns `PyRaise`.
pub fn bound_meth_call(
    ctx: &mut CallCtx,
    self_value: Value,
    code: &RawCode,
    qstr_table: &[u16],
    empty_tuple: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    runner: &mut dyn FnMut(&mut CallCtx, &mut CodeState) -> RunOutcome,
) -> PyResult<Value> {
    let data = bound_meth_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    let mut args2: Vec<Value> = Vec::with_capacity(1 + args.len());
    args2.push(data.self_);
    args2.extend_from_slice(args);
    // Discriminant first: the `arena.get` borrow must end before
    // the `&mut ctx` call below (same pattern as C7r's adapters).
    let kind = match data.func {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBuiltin(_) => 0,
            Obj::FunBc(_) => 1,
            _ => 2,
        },
        _ => 2,
    };
    match kind {
        0 => call_fun_builtin(ctx, data.func, n_args + 1, n_kw, &args2),
        1 => fun_bc_call(
            ctx,
            data.func,
            code,
            qstr_table,
            empty_tuple,
            n_args + 1,
            n_kw,
            &args2,
            runner,
        ),
        _ => Err(PyError::PyRaise),
    }
}

/// The `CallFn` registered on `mp_type_bound_meth`. The builtin
/// path is live (no runner / code needed); the `FunBc` path
/// reports not-yet-runnable (no `RawCode` / qstr table /
/// empty-tuple singleton in the slot — C13's, see `objfun.rs`
/// `fun_bc_call_slot`).
pub fn bound_meth_call_slot(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = bound_meth_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    let mut args2: Vec<Value> = Vec::with_capacity(1 + args.len());
    args2.push(data.self_);
    args2.extend_from_slice(args);
    // Discriminant first (see `bound_meth_call`).
    let kind = match data.func {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBuiltin(_) => 0,
            Obj::FunBc(_) => 1,
            _ => 2,
        },
        _ => 2,
    };
    match kind {
        // Builtin path is live (no runner / code needed).
        0 => call_fun_builtin(ctx, data.func, n_args + 1, n_kw, &args2),
        // FunBc: no code store in the slot — C13.
        _ => Err(PyError::PyRaise),
    }
}

// ── `bound_meth_unary_op` (`objboundmeth.zig:29-34`) ─────────────

/// HASH only: `newSmallInt(word(self) ^ word(meth))`, where the
/// words are the REPR_A tagged words (`obj_id`, `obj.rs:725` —
/// small int `(n << 1) | 1`, qstr `(q << 3) | 2`, immediates
/// `None = 6` / `False = 14` / `True = 30`, heap objects the arena
/// slot index). The XOR is tag-wrapped (`wrap_small_int`,
/// `value.rs` — the Zig's `newSmallInt` does the same wrap with
/// no window check). Every other op returns `PyRaise` (the Zig's
/// NULL = "not implemented").
pub fn bound_meth_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_value: Value) -> PyResult<Value> {
    if op != UnaryOp::Hash {
        return Err(PyError::PyRaise);
    }
    let data = bound_meth_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    let word = |v: Value| -> i64 {
        match obj_id(v) {
            Value::SmallInt(w) => w,
            _ => unreachable!("obj_id always returns a small int"),
        }
    };
    let xored = word(data.self_) ^ word(data.func);
    Ok(Value::SmallInt(crate::objects::value::wrap_small_int(xored)))
}

// ── `bound_meth_binary_op` (`objboundmeth.zig:36-46`) ────────────

/// EQUAL only (full config): `mp_obj_equal` on both parts
/// (`obj_equal`, `obj_ops.rs`). `FLAG_EQ_CHECKS_OTHER_TYPE` is
/// clear, so the VM guarantees both sides are bound methods; a
/// non-bound-method rhs still returns `PyRaise` (defensive).
/// Any other op returns `PyRaise` (the Zig's NULL — NOT_EQUAL is
/// not a slot op, the VM inverts EQUAL).
pub fn bound_meth_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    if op != BinaryOp::Equal {
        return Err(PyError::PyRaise);
    }
    let l = bound_meth_data(ctx, lhs).ok_or(PyError::PyRaise)?;
    let r = bound_meth_data(ctx, rhs).ok_or(PyError::PyRaise)?;
    let types = ctx.types;
    let same_self = obj_equal(ctx, types, l.self_, r.self_)?;
    if !same_self {
        return Ok(Value::Bool(false));
    }
    let same_meth = obj_equal(ctx, types, l.func, r.func)?;
    Ok(Value::Bool(same_meth))
}

// ── `mp_obj_new_bound_meth` (`objboundmeth.zig:48-55`) ───────────

/// Build a bound method object. `type_` is the `bound_meth` type
/// object (the caller supplies it — C12 owns the type registry).
pub fn new_bound_meth(ctx: &mut CallCtx, type_: GcRef, func: Value, self_: Value) -> GcRef {
    ctx.alloc(Some(type_), Obj::BoundMeth(BoundMethData { func, self_ }))
}

// ── `bound_meth_attr` (`objboundmeth.zig:57-62`) ─────────────────

/// Attribute load: `mp_load_method_maybe` on the underlying fun
/// (see `load_attr_on_fun`, `objfun.rs`). A `FunBc` answers
/// `__name__` / `__globals__` (probed: `C().m.__name__ == "m"`);
/// a `FunBuiltin` never answers (probed: `[].append.__name__` →
/// `AttributeError`).
pub fn bound_meth_attr(ctx: &mut CallCtx, self_value: Value, attr: u32) -> PyResult<Option<Value>> {
    let data = bound_meth_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    load_attr_on_fun(ctx, data.func, attr)
}

// ── type data (`objboundmeth.zig:65-95`) ─────────────────────────

/// `mp_type_bound_meth`: flags 0 (`MP_TYPE_FLAG_NONE`), name
/// `bound_method`, slots attr=1, call=2, unary_op=3, binary_op=4
/// (the Zig's `slot_index_*`). No print slot (see the module
/// docs). `attr` has no `TypeData` field yet (C12 wires it —
/// design doc D5 C10 note).
pub fn bound_meth_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"bound_method")
            .expect("bound_method is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(
            |ctx: &mut CallCtx, op: UnaryOp, self_: Value| bound_meth_unary_op(ctx, op, self_),
        ),
        binary_op: Some(
            |ctx: &mut CallCtx, op: BinaryOp, lhs: Value, rhs: Value| {
                bound_meth_binary_op(ctx, op, lhs, rhs)
            },
        ),
        call: Some(bound_meth_call_slot),
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    }
}
