//! Closures (`objclosure.zig`, 90 lines).
//!
//! Ports, in order:
//! - `npy_clos_fn_call` (`objclosure.zig:21-40`): prepend the
//!   closed-over cells to the args (`n_total = n_closed + n_args
//!   + 2*n_kw`) and call the underlying fun with
//!   `n_closed + n_args` positionals (`mp_call_function_n_kw`).
//!   The Zig uses a 5-slot stack buffer for `n_total <= 5` and a
//!   heap buffer above that; Rust builds one `Vec` either way
//!   (non-observable). The builtin-fun path is live (sig dispatch
//!   via `call_fun_builtin`); the `FunBc` path goes through
//!   `fun_bc_call` with the runner seam (see `objfun.rs`).
//! - `mp_obj_new_closure` (`:42-53`).
//! - `mp_obj_closure_attr` (`:54-58`): `mp_load_method_maybe` on
//!   the underlying fun (see `load_attr_on_fun`, `objfun.rs`).
//!
//! No print slot: `ERROR_REPORTING=TERSE` under nano, and the
//! full config keeps it that way — `repr(closure)` is the VM's
//! default `"<closure>"` (probed 2026-11-17), not a slot in this
//! lane. The cells are `Cell` objects (`CellData`, C7) passed
//! as-is — the bytecode's `LOAD_DEREF` / `LOAD_CLOSURE` reads the
//! cells from the argument slots, not their contents.

use crate::compiler::mpy::RawCode;
use crate::memory::gc::CallCtx;
use crate::objects::obj::FLAG_BINDS_SELF;
use crate::objects::value::{ClosureData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::bc::CodeState;

use super::objfun::{call_fun_builtin, fun_bc_call, load_attr_on_fun, RunOutcome};

/// Read a `ClosureData` out of a heap `Value`. `None` when the
/// value is not a heap closure — the Zig's `fromObj` would read
/// garbage; Rust returns `PyRaise` instead (caller-contract
/// guard).
fn closure_data(ctx: &CallCtx, self_value: Value) -> Option<ClosureData> {
    match self_value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Closure(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The underlying fun's payload kind (discriminant-first: the
/// `arena.get` borrow must end before the `&mut ctx` call below).
fn fun_kind(ctx: &CallCtx, fun: Value) -> u8 {
    match fun {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBuiltin(_) => 0,
            Obj::FunBc(_) => 1,
            _ => 2,
        },
        _ => 2,
    }
}

// ── `npy_clos_fn_call` (`objclosure.zig:21-40`) ──────────────────

/// The full closure call: `args2 = [cells..., args...]`, call the
/// underlying fun with `n_closed + n_args` positionals and the
/// same `n_kw` (`mp_call_function_n_kw`). `code` / `qstr_table` /
/// `empty_tuple` are resolved by the caller (C13's dispatcher)
/// for the `FunBc` path; the builtin path needs none of them.
pub fn closure_call(
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
    let data = closure_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    let mut args2: Vec<Value> = Vec::with_capacity(data.cells.len() + args.len());
    args2.extend(data.cells.iter().map(|r| Value::Heap(*r)));
    args2.extend_from_slice(args);
    let fun = Value::Heap(data.fun);
    let n_args2 = data.cells.len() + n_args;
    match fun_kind(ctx, fun) {
        0 => call_fun_builtin(ctx, fun, n_args2, n_kw, &args2),
        1 => fun_bc_call(ctx, fun, code, qstr_table, empty_tuple, n_args2, n_kw, &args2, runner),
        _ => Err(PyError::PyRaise),
    }
}

/// The `CallFn` registered on `mp_type_closure`. The builtin path
/// is live (no runner / code needed); the `FunBc` path reports
/// not-yet-runnable (no `RawCode` / qstr table / empty-tuple
/// singleton in the slot — C13's, see `objfun.rs`
/// `fun_bc_call_slot`).
pub fn closure_call_slot(
    ctx: &mut CallCtx,
    self_value: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> PyResult<Value> {
    let data = closure_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    let mut args2: Vec<Value> = Vec::with_capacity(data.cells.len() + args.len());
    args2.extend(data.cells.iter().map(|r| Value::Heap(*r)));
    args2.extend_from_slice(args);
    let fun = Value::Heap(data.fun);
    let n_args2 = data.cells.len() + n_args;
    match fun_kind(ctx, fun) {
        // Builtin path is live (no runner / code needed).
        0 => call_fun_builtin(ctx, fun, n_args2, n_kw, &args2),
        // FunBc: no code store in the slot — C13.
        _ => Err(PyError::PyRaise),
    }
}

// ── `mp_obj_new_closure` (`objclosure.zig:42-53`) ────────────────

/// Build a closure object. `fun` is the underlying function
/// (a `FunBc` or `FunBuiltin` heap object); `cells` are the
/// closed-over `Cell` objects, in order. `type_` is the `closure`
/// type object (the caller supplies it — C12 owns the registry).
/// Panics when `fun` is not a heap object (caller-contract
/// violation — the Zig's `fromObj` would read garbage).
pub fn new_closure(ctx: &mut CallCtx, type_: GcRef, fun: Value, cells: Vec<GcRef>) -> GcRef {
    let fun = match fun {
        Value::Heap(r) => r,
        _ => panic!("new_closure on a non-heap fun"),
    };
    ctx.alloc(Some(type_), Obj::Closure(ClosureData { fun, cells }))
}

// ── `mp_obj_closure_attr` (`objclosure.zig:54-58`) ───────────────

/// Attribute load: `mp_load_method_maybe` on the underlying fun
/// (see `load_attr_on_fun`, `objfun.rs`). A `FunBc` answers
/// `__name__` / `__globals__` (probed: `h.__name__ == "h"`,
/// `h.__globals__ is globals()`); a `FunBuiltin` never answers.
pub fn closure_attr(ctx: &mut CallCtx, self_value: Value, attr: u32) -> PyResult<Option<Value>> {
    let data = closure_data(ctx, self_value).ok_or(PyError::PyRaise)?;
    load_attr_on_fun(ctx, Value::Heap(data.fun), attr)
}

// ── type data (`objclosure.zig:61-82`) ───────────────────────────

/// `mp_type_closure`: flags `FLAG_BINDS_SELF` (`0x0020`), name
/// `closure`, slots attr=1, call=2 (the Zig's `slot_index_*`).
/// No print slot (see the module docs). `attr` is `closure_attr`,
/// wired by C12a.
pub fn closure_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"closure")
            .expect("closure is a static qstr"),
        flags: FLAG_BINDS_SELF,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        unary_op: None,
        binary_op: None,
        call: Some(closure_call_slot),
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: Some(closure_attr),
        make_new: None,
        bases: None,
    }
}
