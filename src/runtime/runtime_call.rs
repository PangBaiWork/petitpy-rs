//! Call protocol (`runtime.zig:811-1054`): `mp_call_function_n_kw`
//! and friends, plus `mp_call_prepare_args_n_kw_var`.
//!
//! The Zig's `mp_call_method_n_kw_py` takes `args[1] == NULL` as
//! "no self"; the Rust shape takes the self explicitly as
//! `Option<Value>` (`call_method_self_n_kw`) or inside a
//! `MethodDest` (`call_method_n_kw`). The Zig's `have_self` /
//! `n_args_n_kw` packing is a C-ism the VM adapts — the
//! `PreparedCall` here carries the flattened args directly.

use crate::memory::gc::CallCtx;
use crate::objects::callables::objboundmeth::bound_meth_call_slot;
use crate::objects::callables::objclosure::closure_call_slot;
use crate::objects::callables::objfun::{call_fun_builtin, fun_bc_call_slot};
use crate::objects::obj::obj_get_type;
use crate::objects::obj_ops::obj_get_array;
use crate::objects::objtype::seams::type_call_slot;
use crate::objects::objtype::typecreate::type_call;
use crate::objects::value::{Obj, Value};
use crate::runtime::runtime::{dunder, PreparedCall, RtError, RuntimeSeams};
use crate::runtime::runtime_iter::{getiter, iternext, IterNext};
use crate::runtime::runtime_method::{load_method, MethodDest};

/// `mp_call_function_n_kw_py` (`runtime.zig:811-834`): the
/// dispatch itself is payload-based, because values allocated
/// with a `None` header (`FunBuiltin` / `FunBc` / `Closure` — no
/// type object exists for them yet) have no type to read a `call`
/// slot from. Instances need no payload arm: C12a's
/// `typecreate.rs:601` sets a user class's `call` slot to
/// `instance_call_slot`, which performs the `__call__` lookup.
pub fn call_function_n_kw(
    ctx: &mut CallCtx,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, RtError> {
    let r = match fun {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBuiltin(_) => call_fun_builtin(ctx, fun, n_args, n_kw, args),
            Obj::FunBc(_) => fun_bc_call_slot(ctx, fun, n_args, n_kw, args),
            Obj::BoundMeth(_) => bound_meth_call_slot(ctx, fun, n_args, n_kw, args),
            Obj::Closure(_) => closure_call_slot(ctx, fun, n_args, n_kw, args),
            Obj::Type(_) => type_call(ctx, fun, n_args, n_kw, args, None),
            _ => {
                let t = obj_get_type(ctx.arena, &ctx.types, fun);
                match t.and_then(|tr| type_call_slot(ctx, tr)) {
                    Some(slot) => slot(ctx, fun, n_args, n_kw, args),
                    None => {
                        return Err(RtError::TypeError(
                            "object not callable".to_string(),
                        ));
                    }
                }
            }
        },
        _ => {
            return Err(RtError::TypeError("object not callable".to_string()));
        }
    };
    r.map_err(RtError::from)
}

/// `mp_call_function_0` (`runtime.zig:1048-1050`).
pub fn call_function_0(ctx: &mut CallCtx, fun: Value) -> Result<Value, RtError> {
    call_function_n_kw(ctx, fun, 0, 0, &[])
}

/// `mp_call_function_1` (`runtime.zig:1051`).
pub fn call_function_1(ctx: &mut CallCtx, fun: Value, a: Value) -> Result<Value, RtError> {
    call_function_n_kw(ctx, fun, 1, 0, &[a])
}

/// `mp_call_function_2` (`runtime.zig:1052-1054`).
pub fn call_function_2(ctx: &mut CallCtx, fun: Value, a: Value, b: Value) -> Result<Value, RtError> {
    call_function_n_kw(ctx, fun, 2, 0, &[a, b])
}

/// `mp_call_method_n_kw_py` (`runtime.zig:838-855`): call
/// `dest.func` with `dest.self_` prepended when present (the
/// Zig's `args[1] == NULL` → "no self" adjust rule).
pub fn call_method_n_kw(
    ctx: &mut CallCtx,
    n_args: usize,
    n_kw: usize,
    dest: &MethodDest,
    args: &[Value],
) -> Result<Value, RtError> {
    match dest.self_ {
        Some(self_obj) => call_method_self_n_kw(ctx, dest.func, self_obj, n_args, n_kw, args),
        None => call_function_n_kw(ctx, dest.func, n_args, n_kw, args),
    }
}

/// `mp_call_method_self_n_kw` (`runtime.zig:856-892`): call with
/// an explicit self. The Zig builds the args in an inline `[17]`
/// buffer with a heap fallback past 16 args; the distinction is
/// unobservable in Rust (a `Vec` either way).
pub fn call_method_self_n_kw(
    ctx: &mut CallCtx,
    meth: Value,
    self_obj: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, RtError> {
    let mut full: Vec<Value> = Vec::with_capacity(1 + args.len());
    full.push(self_obj);
    full.extend_from_slice(args);
    call_function_n_kw(ctx, meth, n_args + 1, n_kw, &full)
}

/// `mp_call_prepare_args_n_kw_var` (`runtime.zig:894-1047`).
/// `kw` is `&[(Option<Value>, Value)]`: `None` key = a `**value`
/// entry (the Zig's `MP_OBJ_NULL` key slot — `bc.rs:40`).
/// `starred` is the list of `*obj` values in order (the Zig
/// reads them off its flat args array; the Rust shape takes the
/// objects themselves — the count-only sketch in the brief
/// cannot express which objects to expand). The Zig's
/// `list_len` / `kw_dict_len` precomputations are skipped: a
/// `Vec` grows, and they never raise. The Zig does not check
/// pending between the `keys()` / `__getitem__` steps; Rust
/// returns the first error immediately (same exception, and no
/// use of an uninitialized dest slot).
#[allow(clippy::too_many_arguments)]
pub fn call_prepare_args_n_kw_var(
    ctx: &mut CallCtx,
    fun: Value,
    self_: Option<Value>,
    positionals: &[Value],
    starred: &[Value],
    kw: &[(Option<Value>, Value)],
    intern: crate::objects::iters::iter::InternFn,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<PreparedCall, RtError> {
    let mut args: Vec<Value> = Vec::new();
    if let Some(s) = self_ {
        args.push(s);
    }
    args.extend_from_slice(positionals);
    for star in starred {
        expand_star_positionals(ctx, &mut args, *star, next_obj, seams)?;
    }
    let mut n_kw = 0usize;
    for (k, v) in kw {
        match k {
            Some(key) => {
                args.push(*key);
                args.push(*v);
                n_kw += 1;
            }
            None => expand_star_kw(ctx, &mut args, *v, &mut n_kw, intern, next_obj, seams)?,
        }
    }
    let n_args = args.len() - 2 * n_kw;
    Ok(PreparedCall {
        fun,
        args,
        n_args,
        n_kw,
    })
}

/// Expand one starred positional (`*obj`) into `args`:
/// exact tuple/list → its items; otherwise `getiter` +
/// `iternext` (`runtime.zig:1004-1012`).
fn expand_star_positionals(
    ctx: &mut CallCtx,
    args: &mut Vec<Value>,
    star: Value,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<(), RtError> {
    match obj_get_array(ctx, &ctx.types, star) {
        Ok(items) => {
            args.extend(items);
            Ok(())
        }
        Err(_) => {
            let iter = getiter(ctx, star, next_obj, seams)?;
            loop {
                match iternext(ctx, iter, next_obj, seams)? {
                    IterNext::Value(v) => args.push(v),
                    IterNext::Stop(_) => return Ok(()),
                }
            }
        }
    }
}

/// Expand one `**obj` entry into `args` as kw pairs
/// (`runtime.zig:1013-1023`): exact dict → its entries in
/// insertion order (keys interned); otherwise `keys()` +
/// `__getitem__` per key. A non-str key raises
/// `TypeError("can't convert to str implicitly")` — probed
/// 2026-11-18 (`f(0, **{1: 2})`).
fn expand_star_kw(
    ctx: &mut CallCtx,
    args: &mut Vec<Value>,
    kw_dict: Value,
    n_kw: &mut usize,
    intern: crate::objects::iters::iter::InternFn,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<(), RtError> {
    let keys_q = dunder(b"keys").expect("keys is a static qstr");
    let getitem_q = dunder(b"__getitem__").expect("__getitem__ is a static qstr");
    if let Value::Heap(r) = kw_dict {
        if ctx.arena.header(r).type_ == Some(ctx.types.dict) {
            if let Obj::Dict(d) = ctx.arena.get(r) {
                let entries = d.map.entries.clone();
                for e in entries {
                    let Some(k) = e.key else { continue };
                    let name = match k {
                        Value::Qstr(id) => id,
                        Value::Heap(kr) => match ctx.arena.get(kr) {
                            Obj::Str(s) => intern(&s.bytes).map_err(RtError::from)?,
                            _ => {
                                return Err(RtError::TypeError(
                                    "can't convert to str implicitly".to_string(),
                                ));
                            }
                        },
                        _ => {
                            return Err(RtError::TypeError(
                                "can't convert to str implicitly".to_string(),
                            ));
                        }
                    };
                    args.push(Value::Qstr(name));
                    args.push(e.value);
                    *n_kw += 1;
                }
                return Ok(());
            }
        }
    }
    // Non-dict `**`: `keys()` + per-key `__getitem__`.
    let keys_dest = load_method(ctx, kw_dict, keys_q, next_obj)?;
    let keys_obj = call_method_n_kw(ctx, 0, 0, &keys_dest, &[])?;
    let keys_iter = getiter(ctx, keys_obj, next_obj, seams)?;
    loop {
        let key = match iternext(ctx, keys_iter, next_obj, seams)? {
            IterNext::Value(v) => v,
            IterNext::Stop(_) => return Ok(()),
        };
        let name = match key {
            Value::Qstr(id) => id,
            Value::Heap(kr) => match ctx.arena.get(kr) {
                Obj::Str(s) => intern(&s.bytes).map_err(RtError::from)?,
                _ => {
                    return Err(RtError::TypeError(
                        "can't convert to str implicitly".to_string(),
                    ));
                }
            },
            _ => {
                return Err(RtError::TypeError(
                    "can't convert to str implicitly".to_string(),
                ));
            }
        };
        let getitem_dest = load_method(ctx, kw_dict, getitem_q, next_obj)?;
        let v = call_method_n_kw(ctx, 1, 0, &getitem_dest, &[key])?;
        args.push(Value::Qstr(name));
        args.push(v);
        *n_kw += 1;
    }
}

/// `mp_call_method_n_kw_var` (`runtime.zig:1048-1050` in the
/// Zig's numbering — the var-args entry points): prepare, then
/// call.
#[allow(clippy::too_many_arguments)]
pub fn call_method_n_kw_var(
    ctx: &mut CallCtx,
    fun: Value,
    self_: Option<Value>,
    positionals: &[Value],
    starred: &[Value],
    kw: &[(Option<Value>, Value)],
    intern: crate::objects::iters::iter::InternFn,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let prepared =
        call_prepare_args_n_kw_var(ctx, fun, self_, positionals, starred, kw, intern, next_obj, seams)?;
    call_function_n_kw(ctx, prepared.fun, prepared.n_args, prepared.n_kw, &prepared.args)
}
