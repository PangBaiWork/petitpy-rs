//! The `collections` builtin module (`src/modules/modcollections.zig`,
//! 68 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modcollections.c`
//! + the Zig's own `objdeque.zig` / `objnamedtuple.zig`): `deque`,
//! `namedtuple`, `OrderedDict` — 4 entries (`__name__` + 3).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `deque` is the NATIVE deque type (C11's `deque_type_data` +
//!   `deque_make_new`, sig `(2, 3)`: `deque([1, 2, 3])` raises
//!   `TypeError("argument num/types mismatch")` — the factory
//!   takes the iterable AND the maxlen). Its 5 methods
//!   (`append`, `appendleft`, `extend`, `pop`, `popleft`) live
//!   in the type's locals dict (built here from
//!   `DEQUE_METHODS` — C11 built the type but never wired the
//!   method table; the Zig's `objdeque.zig` table is the
//!   source).
//! - `namedtuple` is the C11 factory (`new_namedtuple_type`)
//!   as a 2-arg builtin; D4 wired the class's `make_new` +
//!   `attr` slots (C11 left them `None`, so `P(1, 2)` raised
//!   `"can't create instance"`).
//! - `OrderedDict` is the dict type with its name changed
//!   (the Zig: `mp_type_ordereddict` IS `mp_type_dict` with
//!   the name swapped — `objdict.rs`'s `is_ordered_dict_type`
//!   checks the name); it shares the dict type's locals dict
//!   and its `base` is `dict`, mro `[self, dict, object]`.
//! - `collections.deque` and `ucollections.deque` are the
//!   SAME object (the module is registered extensible, so
//!   `import ucollections` strips the `u` and returns it).
//!
//! This module is registered EXTENSIBLE (not builtin): the
//! reference's `import collections` loads the FROZEN
//! petitpy-lib Python package (`Counter`, `defaultdict`, a
//! list-backed `deque` — see the module docs of the result
//! file §3), which shadows the native module; the native
//! module is what `ucollections` (and a build without the
//! frozen bundle) resolves to. D5's frozen bundle reproduces
//! the shadowing.
//!
//! The builder returns the type refs it creates
//! (`(module, deque_type, ordereddict_type, namedtuple_fn)`)
//! so the registrar can store them in `BuiltinTypes`
//! (the D3 `socket` precedent).

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objdict::dict_type_data;
use crate::objects::iters::objdeque::{
    deque_append, deque_appendleft, deque_extend, deque_pop, deque_popleft, deque_type_data,
};
use crate::objects::iters::objnamedtuple::namedtuple_factory_builtin;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::MethodEntry;
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::PyResult;
use crate::runtime::vm_types::locals_dict_from_entries;

/// The deque method adapters (`objdeque.zig` table):
/// `append` / `appendleft` / `extend` take `(self, arg)`,
/// `pop` / `popleft` take `(self,)` — the `IterError` maps
/// through `record_typed` (the `make_new.rs` pattern).
fn deque_append_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    deque_append(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

fn deque_appendleft_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    deque_appendleft(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

fn deque_extend_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    deque_extend(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

fn deque_pop_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    deque_pop(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

fn deque_popleft_fn(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    deque_popleft(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// The deque locals table (`objdeque.zig`): exactly 5
/// methods — no `__iter__` (getiter is a type slot), no
/// `clear` / `count` / `remove` / `reverse` (the Zig has none).
pub const DEQUE_METHODS: &[MethodEntry] = &[
    MethodEntry { name: "append", func: deque_append_fn, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "appendleft", func: deque_appendleft_fn, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "extend", func: deque_extend_fn, min: 2, max: 2, kw_func: None },
    MethodEntry { name: "pop", func: deque_pop_fn, min: 1, max: 1, kw_func: None },
    MethodEntry { name: "popleft", func: deque_popleft_fn, min: 1, max: 1, kw_func: None },
];

/// `build_collections_module` (`modcollections.zig` table):
/// 4 entries. Returns `(module, deque_type,
/// ordereddict_type, namedtuple_fn)` for the registrar.
pub fn build_collections_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, GcRef, GcRef, Value)> {
    let (module, dict) = new_d4_module(ctx, "collections", module_type);

    // The deque type: locals dict from `DEQUE_METHODS`,
    // header = the `type` type (the D3 `modsocket.rs`
    // pattern), mro `[self, object]` (the D1 patch — the
    // Zig's `mroOf` appends `object` for a parentless type).
    let deque_id = static_qstr_id(b"deque").expect("deque is a static qstr");
    let deque_locals = locals_dict_from_entries(ctx, fun_builtin_var, None, DEQUE_METHODS, &[], &[]);
    let deque_type = ctx.alloc(Some(ctx.types.type_), Obj::Type(deque_type_data(deque_locals, deque_id)));
    let object = ctx.types.object;
    if let Obj::Type(d) = ctx.arena.get_mut(deque_type) {
        d.mro = vec![deque_type, object];
    }
    ctx.roots.push(Value::Heap(deque_type));

    // `OrderedDict`: the dict type with its name changed
    // (shares the dict type's locals dict; `base` is `dict`).
    let od_id = static_qstr_id(b"OrderedDict").expect("OrderedDict is a static qstr");
    let dict_type = ctx.types.dict;
    let dict_locals = match ctx.arena.get(dict_type) {
        Obj::Type(d) => d.dict,
        _ => unreachable!("dict type is a type"),
    };
    let mut od_data = dict_type_data(dict_locals);
    od_data.name = od_id;
    od_data.base = Some(dict_type);
    let ordereddict = ctx.alloc(Some(ctx.types.type_), Obj::Type(od_data));
    if let Obj::Type(d) = ctx.arena.get_mut(ordereddict) {
        d.mro = vec![ordereddict, dict_type, object];
    }
    ctx.roots.push(Value::Heap(ordereddict));

    // The `namedtuple` factory builtin (C11).
    let namedtuple_fn = mk_fun(ctx, fun_builtin_var, "namedtuple", 2, 2, namedtuple_factory_builtin, None);

    store(ctx, dict, "deque", Value::Heap(deque_type));
    store(ctx, dict, "OrderedDict", Value::Heap(ordereddict));
    store(ctx, dict, "namedtuple", namedtuple_fn);
    fix_dict(ctx, dict);
    Ok((module, deque_type, ordereddict, namedtuple_fn))
}
