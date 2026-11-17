//! Dict methods, views and `BuiltinFn` adapters (C9a).
//!
//! Split out of `objdict.rs` (C9a-split: the 800-line cap). The core
//! (`DictError`, construction, the `mp_obj_dict_*` ABI, the type slots,
//! `dict_type_data`) stays in `objdict.rs`; this file holds the method
//! implementations (`get` / `pop` / `setdefault` / `popitem` / `update` /
//! `fromkeys`), the dict views (`keys` / `items` / `values` + their
//! iterators and slots), and the `BuiltinFn` adapters for the method
//! table (built by C12/D1). The `collect_iterable` machinery lives in
//! `objdict_collect.rs` (C13b-split: the same cap) and is re-exported
//! below, so the call sites keep their paths.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::containers::map::{
    map_lookup, map_of, map_of_mut, map_set_value, LookupKind, LookupOutcome,
};
use crate::objects::obj::{obj_id, obj_len_maybe};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{IteratorData, MapData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use super::objdict::{
    dict_binary_op, dict_clear, dict_copy, dict_new, dict_ref_of, dict_unary_op,
    ensure_not_fixed, entry_value, is_dict_or_ordered, DictError, ARG_NUM_TYPES_MISMATCH,
    UPDATE_SEQ_WRONG_LEN,
};

pub(crate) use super::objdict_collect::collect_iterable;
use super::objdict_collect::view_next_entry;

// ── dict-view kinds ─────────────────────────────────────────────────

/// `dict_view` kinds (`objdict.zig`: `DICT_VIEW_ITEMS = 0`,
/// `DICT_VIEW_KEYS = 1`, `DICT_VIEW_VALUES = 2`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum DictViewKind {
    Items = 0,
    Keys = 1,
    Values = 2,
}

impl DictViewKind {
    fn from_i32(v: i32) -> Option<DictViewKind> {
        match v {
            0 => Some(DictViewKind::Items),
            1 => Some(DictViewKind::Keys),
            2 => Some(DictViewKind::Values),
            _ => None,
        }
    }

    /// The view's type name (`dict_items` / `dict_keys` /
    /// `dict_values` — `viewName` in `objdict.zig`).
    pub fn type_name(self) -> &'static str {
        match self {
            DictViewKind::Items => "dict_items",
            DictViewKind::Keys => "dict_keys",
            DictViewKind::Values => "dict_values",
        }
    }
}

/// Build a dict view (`Obj::Iterator`, kind in `index`, no `extra`).
pub(crate) fn make_view(ctx: &mut CallCtx, dict: Value, kind: DictViewKind) -> Value {
    let it = IteratorData {
        source: dict,
        index: kind as usize,
        extra: Vec::new(),
    };
    Value::Heap(ctx.alloc(None, Obj::Iterator(it)))
}

/// Build a dict view iterator (`Obj::Iterator`, cursor in `index`,
/// kind in `extra[0]`).
pub(crate) fn make_view_it(ctx: &mut CallCtx, dict: Value, kind: DictViewKind) -> Value {
    let it = IteratorData {
        source: dict,
        index: 0,
        extra: vec![Value::SmallInt(kind as i64)],
    };
    Value::Heap(ctx.alloc(None, Obj::Iterator(it)))
}

/// Read a value as a dict view: `(dict, kind)`. `None` for any other
/// `Obj::Iterator` shape (C11's iterators use other shapes).
pub fn as_dict_view(arena: &GcArena, v: Value) -> Option<(Value, DictViewKind)> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Iterator(it) if it.extra.is_empty() => {
                DictViewKind::from_i32(it.index as i32).map(|k| (it.source, k))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Read a value as a dict view iterator: `(dict, kind, cur)`.
pub fn as_dict_view_it(arena: &GcArena, v: Value) -> Option<(Value, DictViewKind, usize)> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Iterator(it) if it.extra.len() == 1 => {
                let kind = match it.extra[0] {
                    Value::SmallInt(n) => DictViewKind::from_i32(n as i32)?,
                    _ => return None,
                };
                Some((it.source, kind, it.index))
            }
            _ => None,
        },
        _ => None,
    }
}

// ── small helpers ───────────────────────────────────────────────────


// ── `dict_get_helper` (get / pop / setdefault) ──────────────────────

/// `dictGetHelperImpl` (`objdict.zig`): the shared core of `.get`
/// (LOOKUP), `.pop` (REMOVE) and `.setdefault` (ADD).
/// `default`: `None` = no default given (`n_args == 2` — `.get` →
/// `None`, `.pop` → `KeyError`, `.setdefault` → insert `None`);
/// `Some(v)` = the default (`n_args == 3` — returned/inserted).
/// A live entry's value is never NULL in Rust (see `MapEntry`), so
/// the Zig's `elem.value == NULL` "missing" test is exactly the
/// `Added` / `NotFound` outcomes here.
pub(crate) fn dict_get_helper(
    ctx: &mut CallCtx,
    dict: Value,
    key: Value,
    kind: LookupKind,
    default: Option<Value>,
) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    if kind != LookupKind::Lookup {
        ensure_not_fixed(ctx.arena, r)?;
    }
    match map_lookup(ctx, ctx.types, r, key, kind)? {
        LookupOutcome::Found(i) => Ok(entry_value(ctx, r, i)),
        LookupOutcome::Added(i) => {
            // `.setdefault` on a missing key: store the default.
            let v = default.unwrap_or(Value::None);
            map_set_value(ctx.arena, r, i, v);
            Ok(v)
        }
        LookupOutcome::Removed((_, v)) => Ok(v),
        LookupOutcome::NotFound => match (kind, default) {
            // `.pop(k, default)` returns the default when missing.
            (_, Some(d)) => Ok(d),
            (LookupKind::Remove, None) => Err(DictError::KeyError(key)),
            _ => Ok(Value::None),
        },
    }
}

/// `d.get(key[, default])`.
pub fn dict_get(
    ctx: &mut CallCtx,
    dict: Value,
    key: Value,
    default: Option<Value>,
) -> Result<Value, DictError> {
    dict_get_helper(ctx, dict, key, LookupKind::Lookup, default)
}

/// `d.pop(key[, default])`.
pub fn dict_pop(
    ctx: &mut CallCtx,
    dict: Value,
    key: Value,
    default: Option<Value>,
) -> Result<Value, DictError> {
    dict_get_helper(ctx, dict, key, LookupKind::Remove, default)
}

/// `d.setdefault(key[, default])`.
pub fn dict_setdefault(
    ctx: &mut CallCtx,
    dict: Value,
    key: Value,
    default: Option<Value>,
) -> Result<Value, DictError> {
    dict_get_helper(ctx, dict, key, LookupKind::Add, default)
}

/// `d.popitem()`: the LAST live entry (LIFO under full, as CPython
/// does), as a `(key, value)` tuple. `KeyError("popitem():
/// dictionary is empty")` when empty.
pub fn dict_popitem(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    ensure_not_fixed(ctx.arena, r)?;
    if map_of(ctx.arena, r).used == 0 {
        return Err(DictError::KeyErrorPopitemEmpty);
    }
    // Last filled slot (the Zig scans down from `alloc` for the
    // first `slotFilled`; the Rust entries end at `fill`, same set).
    let idx = {
        let m = map_of(ctx.arena, r);
        (0..m.entries.len())
            .rev()
            .find(|&i| m.entries[i].key.is_some())
            .expect("used > 0 implies a live entry")
    };
    let (k, v) = {
        let m = map_of_mut(ctx.arena, r);
        m.used -= 1;
        let e = &mut m.entries[idx];
        let k = e.key.take().expect("live entry has a key");
        let v = std::mem::replace(&mut e.value, Value::None);
        (k, v)
    };
    // The index slot keeps pointing at the tombstone (the Zig does
    // not clear it either) — probes step over it.
    Ok(Value::Heap(ctx.alloc(
        Some(ctx.types.tuple),
        Obj::Tuple(vec![k, v]),
    )))
}

// ── `dict_update` / `dict_fromkeys` ──────────────────────────────────

/// `updateFromKeys` (`objdict.zig`): a non-dict argument with a
/// `keys()` method is a mapping — read it through `keys()` +
/// subscripting. That needs `mp_load_method_maybe` +
/// `mp_call_method_n_kw` (C12/C14: method lookup + calls do not
/// exist yet), so this returns `false` for now and the caller falls
/// through to the pairs path. Correct for everything representable
/// today: dicts (and subclasses) take the `is_dict_or_ordered`
/// branch above, and no other mapping type exists yet (C12).
fn update_from_keys(
    _ctx: &mut CallCtx,
    _self_v: Value,
    _other: Value,
) -> Result<bool, DictError> {
    Ok(false)
}

/// `dictUpdateImpl` (`objdict.zig`): `d.update([other][, **kwargs])`.
/// `other`: `None` = no positional (the `n_args == 1` case).
/// `kwargs`: the kwargs map (a fixed table from `dict_make_new`, or
/// an empty map from the `|` operator). The arg-count check
/// (`mp_arg_check_num_sig`, sig `(1, 2, kw)`) is the caller's — the
/// method dispatch (C15) enforces it; this takes 0/1 positional by
/// construction.
pub fn dict_update(
    ctx: &mut CallCtx,
    self_v: Value,
    other: Option<Value>,
    kwargs: &MapData,
) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, self_v);
    ensure_not_fixed(ctx.arena, r)?;
    if let Some(other_v) = other {
        if is_dict_or_ordered(ctx.arena, other_v) {
            // Dict / OrderedDict / subclass: copy live entries in
            // order. Self-update is a no-op (`args[1] != args[0]`),
            // and an `alloc == 0` source is empty (skip it).
            if other_v != self_v {
                let oref = dict_ref_of(ctx.arena, other_v);
                if map_of(ctx.arena, oref).alloc != 0 {
                    let pairs: Vec<(Value, Value)> = {
                        let m = map_of(ctx.arena, oref);
                        m.entries
                            .iter()
                            .filter_map(|e| e.key.map(|k| (k, e.value)))
                            .collect()
                    };
                    for (k, v) in pairs {
                        match map_lookup(ctx, ctx.types, r, k, LookupKind::Add)? {
                            LookupOutcome::Added(i) | LookupOutcome::Found(i) => {
                                map_set_value(ctx.arena, r, i, v)
                            }
                            _ => unreachable!("ADD never misses"),
                        }
                    }
                }
            }
        } else if update_from_keys(ctx, self_v, other_v)? {
            // A mapping that is not a dict (see above).
        } else {
            // Iterable of (key, value) pairs. Each pair is itself
            // iterated (list / tuple / str / …); exactly 2 elements
            // or `ValueError("dict update sequence has wrong length")`.
            // (The Zig stops after 3 elements of a pair; this
            // materialises the pair whole first — same verdict, see
            // the module docs.)
            let pairs = collect_iterable(ctx, other_v)?;
            for pair in pairs {
                let elems = collect_iterable(ctx, pair)?;
                if elems.len() != 2 {
                    return Err(DictError::ValueError(UPDATE_SEQ_WRONG_LEN));
                }
                match map_lookup(ctx, ctx.types, r, elems[0], LookupKind::Add)? {
                    LookupOutcome::Added(i) | LookupOutcome::Found(i) => {
                        map_set_value(ctx.arena, r, i, elems[1])
                    }
                    _ => unreachable!("ADD never misses"),
                }
            }
        }
    }

    // Update with keyword args (in order).
    let kw_pairs: Vec<(Value, Value)> = {
        let mut cur = 0;
        let mut out = Vec::new();
        while let Some(i) = kwargs.next_live(&mut cur) {
            out.push((
                kwargs.entries[i].key.expect("live entry has a key"),
                kwargs.entries[i].value,
            ));
        }
        out
    };
    for (k, v) in kw_pairs {
        match map_lookup(ctx, ctx.types, r, k, LookupKind::Add)? {
            LookupOutcome::Added(i) | LookupOutcome::Found(i) => {
                map_set_value(ctx.arena, r, i, v)
            }
            _ => unreachable!("ADD never misses"),
        }
    }
    Ok(Value::None)
}

/// `dict.fromkeys(iterable[, value])` (classmethod, full only).
/// The class (`args[0]` in the Zig) is ignored — the result is
/// always a plain dict. Size hint: `mp_obj_len_maybe(iterable)`
/// (0 when the iterable has no `__len__`).
pub fn dict_fromkeys(
    ctx: &mut CallCtx,
    iterable: Value,
    value: Option<Value>,
) -> Result<Value, DictError> {
    let value = value.unwrap_or(Value::None);
    let hint = match obj_len_maybe(ctx, ctx.types, iterable) {
        Some(Value::SmallInt(n)) => n.max(0) as usize,
        Some(Value::Heap(r)) => match ctx.arena.get(r) {
            Obj::Int(z) => z.as_int_checked().map(|n| n.max(0) as usize).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    };
    let dict = dict_new(ctx, hint);
    let dict_ref = dict.as_heap().expect("fresh dict is heap");
    let keys = collect_iterable(ctx, iterable)?;
    for k in keys {
        match map_lookup(ctx, ctx.types, dict_ref, k, LookupKind::Add)? {
            LookupOutcome::Added(i) | LookupOutcome::Found(i) => {
                map_set_value(ctx.arena, dict_ref, i, value)
            }
            _ => unreachable!("ADD never misses"),
        }
    }
    Ok(dict)
}

// ── type slots ──────────────────────────────────────────────────────


// ── dict views ──────────────────────────────────────────────────────

/// `d.keys()` / `d.items()` / `d.values()`.
pub fn dict_keys(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    Ok(make_view(ctx, dict, DictViewKind::Keys))
}

pub fn dict_items(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    Ok(make_view(ctx, dict, DictViewKind::Items))
}

pub fn dict_values(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    Ok(make_view(ctx, dict, DictViewKind::Values))
}

/// `npy_dt_view_it_iternext`: the next value of a dict view
/// iterator, or `None` on exhaustion (`OBJ_STOP_ITERATION` in the
/// Zig — the Rust `None` is the C11 `iternext` convention). Writes
/// the advanced cursor back into the iterator object.
pub fn dict_view_it_next(
    ctx: &mut CallCtx,
    view_it: Value,
) -> Result<Option<Value>, DictError> {
    let Some((dict, kind, mut cur)) = as_dict_view_it(ctx.arena, view_it) else {
        panic!("dict_view_it_next on a non-view-iterator value");
    };
    let dict_ref = dict_ref_of(ctx.arena, dict);
    let next = view_next_entry(ctx, dict_ref, kind, &mut cur);
    if let Value::Heap(r) = view_it {
        if let Obj::Iterator(it) = ctx.arena.get_mut(r) {
            it.index = cur;
        }
    }
    Ok(next)
}

/// `npy_dt_view_print`: `dict_keys([…])` / `dict_items([…])` /
/// `dict_values([…])`, elements in REPR (the Zig builds a fresh
/// iterator and prints each yielded value).
pub fn dict_view_print(
    ctx: &mut CallCtx,
    self_v: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let Some((dict, kind)) = as_dict_view(ctx.arena, self_v) else {
        return;
    };
    let _ = write!(sink, "{}([", kind.type_name());
    let dict_ref = dict_ref_of(ctx.arena, dict);
    let mut cur = 0;
    let mut first = true;
    while let Some(v) = view_next_entry(ctx, dict_ref, kind, &mut cur) {
        if !first {
            let _ = write!(sink, ", ");
        }
        first = false;
        obj_print_helper(ctx, ctx.types, v, PrintKind::Repr, sink);
    }
    let _ = write!(sink, "])");
}

/// `npy_dt_view_unary_op`: only `.values()` supports `__hash__`
/// (the identity hash); LEN/BOOL delegate to the dict.
pub fn dict_view_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_v: Value) -> PyResult<Value> {
    let Some((dict, kind)) = as_dict_view(ctx.arena, self_v) else {
        return Err(PyError::PyRaise);
    };
    if op == UnaryOp::Hash && kind == DictViewKind::Values {
        return Ok(obj_id(self_v));
    }
    dict_unary_op(ctx, op, dict)
}

/// `npy_dt_view_binary_op`: only the KEYS view supports `in`
/// (delegated to the dict's CONTAINS); anything else → NULL.
pub fn dict_view_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let Some((dict, kind)) = as_dict_view(ctx.arena, lhs) else {
        return Err(PyError::PyRaise);
    };
    if kind != DictViewKind::Keys {
        return Err(PyError::PyRaise);
    }
    if op != BinaryOp::Contains {
        return Err(PyError::PyRaise);
    }
    dict_binary_op(ctx, op, dict, rhs)
}

/// `npy_dt_view_getiter`: a fresh view iterator over the same view.
pub fn dict_view_getiter(ctx: &mut CallCtx, self_v: Value) -> PyResult<Value> {
    let Some((dict, kind)) = as_dict_view(ctx.arena, self_v) else {
        return Err(PyError::PyRaise);
    };
    Ok(make_view_it(ctx, dict, kind))
}


// ── `BuiltinFn` adapters (the method table) ─────────────────────────

/// Arity check for the `FunBuiltinVar` methods (the Zig's
/// `MP_DEFINE_CONST_FUN_OBJ_*` sigs are checked by the call
/// machinery — C15; the adapters check too so direct calls are safe).
/// `args` includes `self`.
fn check_arity(args: &[Value], min: usize, max: usize) -> Result<(), DictError> {
    if (min..=max).contains(&args.len()) {
        Ok(())
    } else {
        Err(DictError::TypeError(ARG_NUM_TYPES_MISMATCH))
    }
}

fn first(args: &[Value]) -> Value {
    args[0]
}

/// `dict.clear` (fixed 1 arg).
pub fn dict_clear_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_clear(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.copy` (fixed 1 arg).
pub fn dict_copy_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_copy(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.get` (2–3 args: self, key[, default]).
pub fn dict_get_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(|e| ctx.record_typed(e))?;
    dict_get(ctx, first(args), args[1], args.get(2).copied()).map_err(|e| ctx.record_typed(e))
}

/// `dict.pop` (2–3 args: self, key[, default]).
pub fn dict_pop_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(|e| ctx.record_typed(e))?;
    dict_pop(ctx, first(args), args[1], args.get(2).copied()).map_err(|e| ctx.record_typed(e))
}

/// `dict.setdefault` (2–3 args: self, key[, default]).
pub fn dict_setdefault_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(|e| ctx.record_typed(e))?;
    dict_setdefault(ctx, first(args), args[1], args.get(2).copied())
        .map_err(|e| ctx.record_typed(e))
}

/// `dict.popitem` (fixed 1 arg).
pub fn dict_popitem_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_popitem(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.update` (1–2 positionals + kwargs). The `BuiltinFn`
/// signature carries no kwargs map, so this adapter covers the
/// no-kw call; the VM passes kwargs through `dict_update` directly
/// (C15).
pub fn dict_update_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 2).map_err(|e| ctx.record_typed(e))?;
    let empty = MapData::new();
    dict_update(ctx, first(args), args.get(1).copied(), &empty).map_err(|e| ctx.record_typed(e))
}

/// `dict.items` (fixed 1 arg).
pub fn dict_items_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_items(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.keys` (fixed 1 arg).
pub fn dict_keys_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_keys(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.values` (fixed 1 arg).
pub fn dict_values_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    dict_values(ctx, first(args)).map_err(|e| ctx.record_typed(e))
}

/// `dict.fromkeys` (classmethod, 2–3 args: cls, iterable[, value]).
/// The class is ignored (the Zig builds a plain dict).
pub fn dict_fromkeys_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(|e| ctx.record_typed(e))?;
    dict_fromkeys(ctx, args[1], args.get(2).copied()).map_err(|e| ctx.record_typed(e))
}

/// `dict`'s method table (C13, the Zig's `mp_obj_dict_methods`
/// shape): name, body, positional arity counting `self`. Sigs
/// are the ones the adapters already enforce (see their doc
/// comments). `update` takes kwargs in the original; the
/// `BuiltinFn` path covers the no-kw call and the VM passes
/// kwargs through `dict_update` directly (C15).
pub static DICT_METHODS: &[crate::objects::slots::MethodEntry] = &[
    crate::objects::slots::MethodEntry {
        name: "clear",
        func: dict_clear_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "copy",
        func: dict_copy_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "get",
        func: dict_get_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "pop",
        func: dict_pop_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "setdefault",
        func: dict_setdefault_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "popitem",
        func: dict_popitem_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "update",
        func: dict_update_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "items",
        func: dict_items_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "keys",
        func: dict_keys_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "values",
        func: dict_values_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "fromkeys",
        func: dict_fromkeys_builtin,
        min: 2,
        max: 3,
        kw_func: None,
    },
    // The opmethods ROM objects (`objdict.zig:768-770,785-787` —
    // the last three rows of both the nano and the full locals
    // dict tables): `__getitem__` / `__setitem__` / `__delitem__`
    // are `mp_op_getitem_obj` / `mp_op_setitem_obj` /
    // `mp_op_delitem_obj` (`opmethods.zig:61-73`), 2-/3-/2-arg
    // `fun_builtin` objects. `__contains__` is NOT in the Zig's
    // dict table — `mp_op_contains_obj` is unreferenced there —
    // so it has no entry here either (C15a).
    crate::objects::slots::MethodEntry {
        name: "__getitem__",
        func: crate::runtime::opmethods::dict_getitem_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "__setitem__",
        func: crate::runtime::opmethods::dict_setitem_builtin,
        min: 3,
        max: 3,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "__delitem__",
        func: crate::runtime::opmethods::dict_delitem_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
];
