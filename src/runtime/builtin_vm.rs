//! VM-intercepted builtins (`modbuiltins.zig`, D1).
//!
//! The builtins that need VM state — `globals`, `locals`, `dir`,
//! `sorted`, `min`, `max`, `getattr`, `setattr`, `hasattr`,
//! `delattr`, `iter`, `all`, `any`, `sum` — are intercepted by
//! GcRef identity in `vm_call::call_function_ctx` (the `next` /
//! `eval` pattern): a bare `BuiltinFn` cannot see the VM's
//! globals/locals, run the dispatch loop's iteration seams, or
//! call a user key function. Each `call_*_ctx` below takes
//! `(ctx, state, n_args, n_kw, args)` with the flat call layout
//! (`args[0..n_args]` positionals, then interleaved qstr/value
//! kwarg pairs) and performs its own arity checks — interception
//! bypasses `call_fun_builtin`'s sig check, so the Zig's
//! `argcheck` mismatch (`"argument num/types mismatch"`) is
//! reproduced by hand where the table requires it.
//!
//! `min` / `max` notes (probed on the reference binary):
//! unknown kwargs are IGNORED (`min([1], foo=1) == 1` —
//! `unpackMinMaxKwargs` only reads `key` / `default`); an
//! explicit `key=None` IS called (`min([3, 1, 2], key=None)`
//! raises "object not callable"); the comparison is the
//! dispatch loop's `binary_op_ctx` with `== Value::Bool(true)`
//! (pointer equality with `True`, not truthiness). `sorted`
//! with a key decorates once per item (the key fn is called
//! through `call_function_ctx`, so `FunBc` keys work) and sorts
//! with a stable 3-way comparator over `binary_op_ctx` — ties
//! keep their original order (probed: `reverse=True` does NOT
//! reverse ties); a comparison failure is stashed and raised
//! AFTER the sort completes (the C9b rule).

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::containers::map::map_of;
use crate::objects::containers::objdict_collect::collect_iterable;
use crate::objects::containers::objlist::new_list;
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::iters::iter::value_to_qstr;
use crate::objects::objtype::objexcept::is_exception_instance;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::BinaryOp;
use crate::objects::value::{Obj, Value};
use crate::runtime::import_search::rt_error_to_value;
use crate::runtime::runtime::RtError;
use crate::runtime::runtime_method::{load_attr, load_method_protected, MethodDest};
use crate::runtime::vm_seams::{is_instance, try_instance_load_attr};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error, value_error};
use crate::runtime::vm_rt::{binary_op_ctx, is_true_ctx};
use crate::runtime::vm_rt_iter::{
    drain_seam_raise, getiter_vm_ctx, iternext_vm_ctx,
};
use crate::runtime::vm_rt_name::store_attr_ctx;

/// Map a `RtError` from the `runtime_method` helpers onto the
/// exception object (the `import_name_ctx` convention:
/// `PyRaise` drains the seam stash, anything else goes through
/// `rt_error_to_value`).
fn rt_to_value(ctx: &mut CallCtx, state: &mut VmState, e: RtError) -> Value {
    match e {
        RtError::PyRaise => drain_seam_raise(ctx, state),
        other => rt_error_to_value(ctx, state, other),
    }
}

/// Look a kwarg up in the flat call layout (`args[n_args +
/// 2*i]` is the name qstr, `+1` the value). Returns `None`
/// when absent — the caller decides whether that is the default
/// or a mismatch.
fn kw_lookup(args: &[Value], n_args: usize, n_kw: usize, name: u32) -> Option<Value> {
    for i in 0..n_kw {
        if let Some(Value::Qstr(q)) = args.get(n_args + 2 * i) {
            if *q == name {
                return args.get(n_args + 2 * i + 1).copied();
            }
        }
    }
    None
}

/// The flat kwargs as `KwArg`s (for `parse_sort_args`).
/// Shared with `builtin_sort.rs`.
pub(crate) fn kw_args(args: &[Value], n_args: usize, n_kw: usize) -> Vec<KwArg> {
    let mut out = Vec::with_capacity(n_kw);
    for i in 0..n_kw {
        if let (Some(Value::Qstr(name)), Some(value)) =
            (args.get(n_args + 2 * i), args.get(n_args + 2 * i + 1))
        {
            out.push(KwArg { name: *name, value: *value });
        }
    }
    out
}

/// The common mismatch error (`argcheck`'s terse message).
/// Shared with `builtin_sort.rs`.
pub(crate) fn mismatch(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    type_error(ctx, state, "argument num/types mismatch")
}

/// `mp_builtin_globals` (`modbuiltins.zig`): the current
/// globals dict (a pointer return — `mp_globals_get`).
pub fn call_globals_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    _args: &[Value],
) -> Result<Value, Value> {
    if n_args != 0 || n_kw != 0 {
        return Err(mismatch(ctx, state));
    }
    Ok(Value::Heap(state.globals))
}

/// `mp_builtin_locals` (`modbuiltins.zig`): the current locals
/// dict (`locals` when set, else `globals` — the C15a slot).
pub fn call_locals_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    _args: &[Value],
) -> Result<Value, Value> {
    if n_args != 0 || n_kw != 0 {
        return Err(mismatch(ctx, state));
    }
    Ok(Value::Heap(state.locals_dict()))
}

/// `mp_builtin_dir` (`modbuiltins.zig:601-639`): 0 args → the
/// names in the locals dict (map order); 1 arg → probe every
/// qstr (from id 1 — `MP_QSTR_` itself is skipped) through
/// `load_method_protected`; a bound `__dir__` delegates to it
/// and its result is returned instead.
pub fn call_dir_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || n_args > 1 {
        return Err(mismatch(ctx, state));
    }
    let types = ctx.types;
    let next_obj = state.next_builtin;
    if n_args == 0 {
        let locals = Value::Heap(state.locals_dict());
        let mut names: Vec<Value> = Vec::new();
        if let Value::Heap(r) = locals {
            if matches!(ctx.arena.get(r), Obj::Dict(_)) {
                for e in &map_of(ctx.arena, r).entries {
                    if let Some(k) = e.key {
                        names.push(k);
                    }
                }
            }
        }
        let list = new_list(ctx, types.list, &names);
        return Ok(Value::Heap(list));
    }
    let obj = args[0];
    let dir_q = static_qstr_id(b"__dir__").expect("__dir__ is a static qstr");
    let mut names: Vec<Value> = Vec::new();
    for i in 1..ctx.pool.total() {
        let attr = i as u32;
        let dest = match load_method_protected(ctx, obj, attr, false, next_obj) {
            Ok(d) => d,
            Err(e) => return Err(rt_to_value(ctx, state, e)),
        };
        let Some(dest) = dest else {
            continue;
        };
        if attr == dir_q {
            if let Some(self_) = dest.self_ {
                // A bound `__dir__` delegates (full config).
                let bound = new_bound_meth(ctx, types.boundmeth, dest.func, self_);
                return call_function_ctx(ctx, state, Value::Heap(bound), 0, 0, &[]);
            }
        }
        names.push(Value::Qstr(attr));
    }
    let list = new_list(ctx, types.list, &names);
    Ok(Value::Heap(list))
}

/// `str.join` / `bytes.join` (E2b): `str_join` is ctx-only and
/// cannot resume a generator, so `', '.join(gen())` /
/// `', '.join(x for x in ...)` raised "object not iterable".
/// The VM intercepts the method by GcRef identity (the `sorted`
/// pattern — the handles live in `VmState`, `vm_builtins_bind.rs`)
/// and materialises the iterable through the real call hook
/// first, then delegates to the real `str_join` on `[sep, list]`
/// — join of a list is the already-working path, so the result
/// is identical for every non-generator iterable. Arity is
/// checked here (interception bypasses `call_fun_builtin`'s sig
/// check — probed: all three arity errors are `TypeError:
/// argument num/types mismatch`). This also closes a Rust panic
/// on `','.join()`: the direct `str_join` indexes `args[1]`
/// unchecked, so a short call aborted (SIGABRT) before.
pub fn call_join_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_args != 2 || n_kw != 0 {
        return Err(mismatch(ctx, state));
    }
    let types = ctx.types;
    let mut hook = crate::runtime::vm_seams::make_hook(state);
    let items = collect_iterable(ctx, args[1], Some(&mut hook)).map_err(|e| {
        if let Some(exc) = state.pending_exc.take() {
            return exc;
        }
        let pe = ctx.record_typed(e);
        exc_from_pyerror(ctx, state, pe)
    })?;
    let list = new_list(ctx, types.list, &items);
    crate::objects::strings::objstr_methods_a::str_join(
        ctx,
        types,
        &[args[0], Value::Heap(list)],
    )
    .map_err(|e| {
        let pe = ctx.record_typed(e);
        exc_from_pyerror(ctx, state, pe)
    })
}

/// `mp_builtin_min` / `mp_builtin_max` (`modbuiltins.zig:763+`):
/// one iterable arg, or 2+ direct args; `key` / `default` are
/// the only kwargs read (unknown kwargs are ignored); an empty
/// 1-arg iterable returns `default` or raises `ValueError("arg
/// is an empty sequence")`.
fn call_minmax_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    op: BinaryOp,
) -> Result<Value, Value> {
    if n_args == 0 {
        return Err(mismatch(ctx, state));
    }
    let key_q = static_qstr_id(b"key").expect("key is a static qstr");
    let default_q = static_qstr_id(b"default").expect("default is a static qstr");
    let key_fn = kw_lookup(args, n_args, n_kw, key_q);
    let default = kw_lookup(args, n_args, n_kw, default_q);
    let mut best_key: Option<Value> = None;
    let mut best_obj: Option<Value> = None;
    // One call per item; a comparison failure propagates at
    // once (the Zig's `mp_binary_op` raise, not the sort's
    // run-to-completion rule).
    let consider = |ctx: &mut CallCtx,
                        state: &mut VmState,
                        best_key: &mut Option<Value>,
                        best_obj: &mut Option<Value>,
                        item: Value|
     -> Result<(), Value> {
        let key = match key_fn {
            Some(f) => call_function_ctx(ctx, state, f, 1, 0, &[item])?,
            None => item,
        };
        match *best_key {
            None => {
                *best_key = Some(key);
                *best_obj = Some(item);
            }
            Some(best) => {
                if binary_op_ctx(ctx, state, op, key, best)? == Value::Bool(true) {
                    *best_key = Some(key);
                    *best_obj = Some(item);
                }
            }
        }
        Ok(())
    };
    if n_args == 1 {
        let it = getiter_vm_ctx(ctx, state, args[0])?;
        loop {
            match iternext_vm_ctx(ctx, state, it)? {
                None => break,
                Some(item) => consider(ctx, state, &mut best_key, &mut best_obj, item)?,
            }
        }
    } else {
        for item in &args[..n_args] {
            consider(ctx, state, &mut best_key, &mut best_obj, *item)?;
        }
    }
    match best_obj {
        Some(v) => Ok(v),
        None => match default {
            Some(d) => Ok(d),
            // (`modbuiltins.zig:minMaxImpl`): the empty-sequence
            // error is a `ValueError`, not a `TypeError`.
            None => Err(value_error(ctx, state, "arg is an empty sequence")),
        },
    }
}

pub fn call_min_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    call_minmax_ctx(ctx, state, n_args, n_kw, args, BinaryOp::Less)
}

pub fn call_max_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    call_minmax_ctx(ctx, state, n_args, n_kw, args, BinaryOp::More)
}

/// `loadAttrDefault` (`modbuiltins.zig:380-394`) shared by
/// `getattr` / `hasattr`: the name through `value_to_qstr` (a
/// non-str raises "can't convert to str implicitly" — bytes are
/// NOT accepted, probed).
fn attr_qstr(ctx: &mut CallCtx, state: &mut VmState, name: Value) -> Result<u32, Value> {
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from).map_err(|e| e);
    value_to_qstr(ctx, name, &mut intern).map_err(|e| {
        let pe = ctx.record_typed(e);
        exc_from_pyerror(ctx, state, pe)
    })
}

/// `mp_builtin_getattr` (`modbuiltins.zig:396-400`): 2 args →
/// the raising load; 3 args → the protected load, default when
/// missing.
pub fn call_getattr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || !(2..=3).contains(&n_args) {
        return Err(mismatch(ctx, state));
    }
    let attr = attr_qstr(ctx, state, args[1])?;
    let next_obj = state.next_builtin;
    // Instances try the VM attribute path first (E2 — the
    // ctx-only `load_method_maybe` below has no `__getattr__`
    // fallback and no property-fget call, so `getattr(b, "a")`
    // on a `__getattr__` class raised instead of returning
    // `'a'`). A hit returns already-bound. An AttributeError
    // raise means "missing" for the 3-arg form (the default);
    // any other raise propagates.
    if is_instance(ctx.arena, args[0]) {
        match try_instance_load_attr(ctx, state, args[0], attr) {
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {}
            Err(exc) => {
                if n_args == 3
                    && is_exception_instance(ctx.arena, exc, state.exc.attribute_error)
                {
                    return Ok(args[2]);
                }
                return Err(exc);
            }
        }
    }
    if n_args == 2 {
        load_attr(ctx, args[0], attr, next_obj).map_err(|e| rt_to_value(ctx, state, e))
    } else {
        match load_method_protected(ctx, args[0], attr, false, next_obj) {
            Ok(Some(MethodDest { func, self_: Some(self_) })) => {
                Ok(Value::Heap(new_bound_meth(ctx, ctx.types.boundmeth, func, self_)))
            }
            Ok(Some(MethodDest { func, self_: None })) => Ok(func),
            Ok(None) => Ok(args[2]),
            Err(e) => Err(rt_to_value(ctx, state, e)),
        }
    }
}

/// `mp_builtin_setattr` (`modbuiltins.zig:402-407`): the real
/// `store_attr` (C14), returning `None`.
pub fn call_setattr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || n_args != 3 {
        return Err(mismatch(ctx, state));
    }
    let attr = attr_qstr(ctx, state, args[1])?;
    store_attr_ctx(ctx, state, args[0], attr, Some(args[2]))?;
    Ok(Value::None)
}

/// `mp_builtin_hasattr` (`modbuiltins.zig:409-417`): the
/// protected load (`catch_all_exc = false`) — `True` iff the
/// method exists.
pub fn call_hasattr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || n_args != 2 {
        return Err(mismatch(ctx, state));
    }
    let attr = attr_qstr(ctx, state, args[1])?;
    let next_obj = state.next_builtin;
    // Instances try the VM attribute path first (E2 — same
    // `__getattr__` gap as `getattr` above). A hit is `True`;
    // an AttributeError raise is `False`; any other raise
    // propagates (the `hasattr(c, "raise")` case).
    if is_instance(ctx.arena, args[0]) {
        match try_instance_load_attr(ctx, state, args[0], attr) {
            Ok(Some(_)) => return Ok(Value::Bool(true)),
            Ok(None) => {}
            Err(exc) => {
                if is_exception_instance(ctx.arena, exc, state.exc.attribute_error) {
                    return Ok(Value::Bool(false));
                }
                return Err(exc);
            }
        }
    }
    match load_method_protected(ctx, args[0], attr, false, next_obj) {
        Ok(dest) => Ok(Value::Bool(dest.is_some())),
        Err(e) => Err(rt_to_value(ctx, state, e)),
    }
}

/// `mp_builtin_delattr` (`modbuiltins.zig:863-866`): just
/// `setattr(o, name, NULL)` — the store with `None`.
pub fn call_delattr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || n_args != 2 {
        return Err(mismatch(ctx, state));
    }
    let attr = attr_qstr(ctx, state, args[1])?;
    store_attr_ctx(ctx, state, args[0], attr, None)?;
    Ok(Value::None)
}

/// `mp_builtin_iter` (`modbuiltins.zig`): `mp_getiter`.
pub fn call_iter_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw != 0 || n_args != 1 {
        return Err(mismatch(ctx, state));
    }
    getiter_vm_ctx(ctx, state, args[0])
}

/// `all` / `any` (`modbuiltins.zig`): getiter + the iternext
/// loop with `is_true`, short-circuiting.
fn call_allany_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    want_all: bool,
) -> Result<Value, Value> {
    if n_kw != 0 || n_args != 1 {
        return Err(mismatch(ctx, state));
    }
    let it = getiter_vm_ctx(ctx, state, args[0])?;
    loop {
        match iternext_vm_ctx(ctx, state, it)? {
            None => return Ok(Value::Bool(want_all)),
            Some(item) => {
                let t = is_true_ctx(ctx, state, item)?;
                if t != want_all {
                    return Ok(Value::Bool(!want_all));
                }
            }
        }
    }
}

pub fn call_all_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    call_allany_ctx(ctx, state, n_args, n_kw, args, true)
}

pub fn call_any_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    call_allany_ctx(ctx, state, n_args, n_kw, args, false)
}

/// `mp_builtin_sum` (`modbuiltins.zig`, `kwVar(1, {start})`):
/// getiter + the ADD loop from `start` (default `0`). Unknown
/// kwargs and a positional 2nd arg AND a `start` kwarg are the
/// argcheck mismatch (probed: `sum([1], foo=1)`,
/// `sum([1], 2, start=3)`).
pub fn call_sum_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if !(1..=2).contains(&n_args) {
        return Err(mismatch(ctx, state));
    }
    let start_q = static_qstr_id(b"start").expect("start is a static qstr");
    let mut start: Option<Value> = None;
    for i in 0..n_kw {
        match args.get(n_args + 2 * i) {
            Some(Value::Qstr(q)) if *q == start_q => {
                if n_args == 2 || start.is_some() {
                    return Err(mismatch(ctx, state));
                }
                start = args.get(n_args + 2 * i + 1).copied();
            }
            _ => return Err(mismatch(ctx, state)),
        }
    }
    let mut total = match (n_args, start) {
        (2, _) => args[1],
        (_, Some(s)) => s,
        _ => Value::SmallInt(0),
    };
    let it = getiter_vm_ctx(ctx, state, args[0])?;
    loop {
        match iternext_vm_ctx(ctx, state, it)? {
            None => break,
            Some(item) => {
                total = binary_op_ctx(ctx, state, BinaryOp::Add, total, item)?;
            }
        }
    }
    Ok(total)
}
