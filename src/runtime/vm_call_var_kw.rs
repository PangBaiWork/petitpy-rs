//! The `*args` / `**kwargs` call path — split out of
//! `vm_call.rs` (E2c: `vm_call.rs` was at its 800-line cap).
//! `call_method_var_kw_ctx` is `mp_call_method_n_kw_var`
//! (`runtime.zig:1048-1054` + `mp_call_prepare_args_n_kw_var`)
//! and `intern_kw_key` is its `**`-key interning helper. The
//! only caller is the dispatch loop (`vm_dispatch_exec.rs`).

use crate::memory::gc::CallCtx;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::{call_function_ctx, call_method_ctx};
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};

/// `mp_call_method_n_kw_var` (`runtime.zig:1048-1054` +
/// `mp_call_prepare_args_n_kw_var`, `:896-1046`): a call with
/// `*args` / `**kwargs` unpacking. `args` is the raw stack
/// slice starting at the fun: `[fun, (self?), pos..., (key,
/// value) pairs..., star_mask]`, where a kw key of `None`
/// (MP_OBJ_NULL) marks a `**` dict unpacking and `star_mask`
/// is a small-int bitmask over the positionals (bit i set ⟹
/// positional i is a `*` unpacking). The expanded positionals
/// and kwargs are built in a fresh `Vec` (the Zig precomputes
/// the size for one `m_malloc`; Rust grows instead), then the
/// call goes through `call_function_ctx` / `call_method_ctx`.
/// A non-tuple/list `*` arg is iterated (`getiter` +
/// `iternext`); a non-dict `**` arg goes through its `keys`
/// method + `__getitem__` per key. A `**` key that is not a
/// string raises `TypeError("keywords must be strings")`.
pub fn call_method_var_kw_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    have_self: bool,
    n_args_n_kw: usize,
    args: &[Option<Value>],
) -> Result<Value, Value> {
    use crate::objects::obj::obj_get_type;
    let fun = args.first().copied().flatten().unwrap_or(Value::None);
    let mut rest = &args[1..];
    let self_ = if have_self {
        let s = rest.first().copied().flatten();
        rest = &rest[1..];
        s
    } else {
        None
    };
    let n_args = n_args_n_kw & 0xff;
    let n_kw = (n_args_n_kw >> 8) & 0xff;
    let star_mask = match rest.get(n_args + 2 * n_kw).copied().flatten() {
        Some(Value::SmallInt(m)) => m,
        _ => 0,
    };
    let mut expanded: Vec<Value> = Vec::new();
    let mut expanded_kw: Vec<(u32, Value)> = Vec::new();
    // Positional args — star bits expand.
    for i in 0..n_args {
        let arg = rest.get(i).copied().flatten().unwrap_or(Value::None);
        if star_mask & (1i64 << i) != 0 {
            let t = obj_get_type(ctx.arena, ctx.types, arg);
            if t == Some(ctx.types.tuple) {
                if let Value::Heap(r) = arg {
                    if let Obj::Tuple(items) = ctx.arena.get(r) {
                        expanded.extend_from_slice(items);
                    }
                }
            } else if t == Some(ctx.types.list) {
                if let Value::Heap(r) = arg {
                    if let Obj::List(d) = ctx.arena.get(r) {
                        expanded.extend_from_slice(&d.items);
                    }
                }
            } else {
                let it = crate::runtime::vm_rt_iter::getiter_vm_ctx(ctx, state, arg)?;
                while let Some(v) =
                    crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)?
                {
                    expanded.push(v);
                }
            }
        } else {
            expanded.push(arg);
        }
    }
    // Keyword pairs — a `None` key is a `**` unpacking.
    for i in 0..n_kw {
        let key = rest.get(n_args + i * 2).copied().flatten();
        let value = rest
            .get(n_args + i * 2 + 1)
            .copied()
            .flatten()
            .unwrap_or(Value::None);
        match key {
            None => {
                let t = obj_get_type(ctx.arena, ctx.types, value);
                if t == Some(ctx.types.dict) {
                    if let Value::Heap(r) = value {
                        // Collect the entries first: interning
                        // a key needs `&mut ctx`, which cannot
                        // run while the arena is borrowed by
                        // the entry iteration.
                        let entries: Vec<(Value, Value)> =
                            match ctx.arena.get(r) {
                                Obj::Dict(d) => d
                                    .map
                                    .entries
                                    .iter()
                                    .filter_map(|e| e.key.map(|k| (k, e.value)))
                                    .collect(),
                                _ => Vec::new(),
                            };
                        for (k, v) in entries {
                            let q = intern_kw_key(ctx, state, k)?;
                            expanded_kw.push((q, v));
                        }
                    }
                } else {
                    let keys_q =
                        crate::objects::primitives::ops::static_qstr_id(b"keys").unwrap_or(u32::MAX);
                    let getitem_q = crate::objects::primitives::ops::static_qstr_id(b"__getitem__")
                        .unwrap_or(u32::MAX);
                    let (keys_meth, keys_self) =
                        crate::runtime::vm_rt_name::load_method_ctx(ctx, state, value, keys_q)?;
                    let keys = call_method_ctx(ctx, state, keys_meth, keys_self, 0, 0, &[])?;
                    let it = crate::runtime::vm_rt_iter::getiter_vm_ctx(ctx, state, keys)?;
                    while let Some(k) =
                        crate::runtime::vm_rt_iter::iternext_vm_ctx(ctx, state, it)?
                    {
                        let q = intern_kw_key(ctx, state, k)?;
                        let (get_meth, get_self) = crate::runtime::vm_rt_name::load_method_ctx(
                            ctx, state, value, getitem_q,
                        )?;
                        let v = call_method_ctx(ctx, state, get_meth, get_self, 1, 0, &[k])?;
                        expanded_kw.push((q, v));
                    }
                }
            }
            Some(Value::Qstr(q)) => expanded_kw.push((q, value)),
            Some(k) => {
                let q = intern_kw_key(ctx, state, k)?;
                expanded_kw.push((q, value));
            }
        }
    }
    // Rebuild the flat call layout: positionals, then
    // interleaved (name, value) pairs.
    let mut flat: Vec<Value> = expanded;
    let n_pos = flat.len();
    for (q, v) in &expanded_kw {
        flat.push(Value::Qstr(*q));
        flat.push(*v);
    }
    let n_kw2 = expanded_kw.len();
    match self_ {
        Some(s) => call_method_ctx(ctx, state, fun, Some(s), n_pos, n_kw2, &flat),
        None => call_function_ctx(ctx, state, fun, n_pos, n_kw2, &flat),
    }
}

/// Intern a `**` kwarg key to a qstr id: a `Qstr` passes
/// through, a `Str` is interned on demand (the Zig interns it
/// — `RuntimeQstr::from_strn`, C15b interior mutability — so a
/// `**` key that was never interned is created here, not
/// rejected), anything else raises `TypeError("keywords must
/// be strings")`. A key over 255 bytes is `OutOfMemory`
/// (`from_strn`'s u8 length limit) → `MemoryError`.
fn intern_kw_key(ctx: &mut CallCtx, state: &VmState, k: Value) -> Result<u32, Value> {
    match k {
        Value::Qstr(q) => Ok(q),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => {
                let bytes = s.bytes.clone();
                ctx.pool
                    .from_strn(&bytes)
                    .map(u32::from)
                    .map_err(|e| exc_from_pyerror(ctx, state, e))
            }
            _ => Err(type_error(ctx, state, "keywords must be strings")),
        },
        _ => Err(type_error(ctx, state, "keywords must be strings")),
    }
}
