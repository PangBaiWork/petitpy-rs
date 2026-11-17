//! Dict methods, views and `BuiltinFn` adapters (C9a).
//!
//! Split out of `objdict.rs` (C9a-split: the 800-line cap). The core
//! (`DictError`, construction, the `mp_obj_dict_*` ABI, the type slots,
//! `dict_type_data`) stays in `objdict.rs`; this file holds the method
//! implementations (`get` / `pop` / `setdefault` / `popitem` / `update` /
//! `fromkeys` + the `collect_iterable` machinery), the dict views
//! (`keys` / `items` / `values` + their iterators and slots), and the
//! `BuiltinFn` adapters for the method table (built by C12/D1).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::containers::map::{
    map_lookup, map_of, map_of_mut, map_set_value, LookupKind, LookupOutcome,
};
use crate::objects::obj::{obj_id, obj_len_maybe};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{GcRef, IteratorData, MapData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use super::objdict::{
    dict_binary_op, dict_clear, dict_copy, dict_new, dict_ref_of, dict_unary_op,
    ensure_not_fixed, entry_value, is_dict_or_ordered, DictError, ARG_NUM_TYPES_MISMATCH,
    NOT_ITERABLE, UPDATE_SEQ_WRONG_LEN,
};

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

// ── iteration helper ────────────────────────────────────────────────


/// The next value a dict view iterator yields (`npy_dt_view_it_iternext`
/// without the iterator object itself): the live entry at `*cur`
/// (advancing it past the entry), as a key / value / `(key, value)`
/// tuple by `kind`. `None` on exhaustion.
fn view_next_entry(
    ctx: &mut CallCtx,
    dict_ref: GcRef,
    kind: DictViewKind,
    cur: &mut usize,
) -> Option<Value> {
    let i = map_of(ctx.arena, dict_ref).next_live(cur)?;
    let (k, v) = {
        let m = map_of(ctx.arena, dict_ref);
        (
            m.entries[i].key.expect("live entry has a key"),
            m.entries[i].value,
        )
    };
    Some(match kind {
        DictViewKind::Keys => k,
        DictViewKind::Values => v,
        DictViewKind::Items => Value::Heap(ctx.alloc(
            Some(ctx.types.tuple),
            Obj::Tuple(vec![k, v]),
        )),
    })
}

/// Materialise an iterable into a `Vec` (the `mp_getiter` /
/// `mp_iternext` loop of `dict_update` / `dict_fromkeys`, for the
/// concrete types available today — see the module docs). The general
/// getiter-slot path is C11's: an `Obj::Iterator` that is not a dict
/// view / view-it, and any type with a `getiter` slot but no direct
/// arm here, currently raise `TypeError("object not iterable")`.
/// (No such type exists in the tree yet, so the arm is unreachable
/// today — it is the documented C11 follow-up, not a behaviour.)
/// What `collect_iterable` does, decided from an immutable borrow
/// first so the allocations below do not borrow the arena.
enum IterPlan {
    /// Elements already in hand.
    Elems(Vec<Value>),
    /// Bytes to yield as 1-char heap `Str`s.
    StrChars(Vec<u8>),
    /// A range to expand (`start`, `stop`, `step`).
    Range(i64, i64, i64),
    /// A dict view / view-it to step (`dict`, `kind`, start cursor).
    ViewSteps(Value, DictViewKind, usize),
    /// An array-family value to materialise (`typecode`, raw
    /// bytes, element count) — elements are read with
    /// `binary_get_val_array` below, where `&mut ctx` is free.
    ArrayBytes { tc: u8, bytes: Vec<u8>, n: usize },
}

pub(crate) fn collect_iterable(ctx: &mut CallCtx, value: Value) -> Result<Vec<Value>, DictError> {
    // Decide the plan from an immutable borrow (cloning payloads);
    // the allocations happen after the borrow ends.
    let plan = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => IterPlan::Elems(l.items.clone()),
            Obj::Tuple(t) => IterPlan::Elems(t.clone()),
            Obj::Dict(_) => {
                let mut cur = 0;
                let mut out = Vec::new();
                while let Some(i) = map_of(ctx.arena, r).next_live(&mut cur) {
                    out.push(
                        map_of(ctx.arena, r)
                            .entries[i]
                            .key
                            .expect("live entry has a key"),
                    );
                }
                IterPlan::Elems(out)
            }
            Obj::Str(s) => IterPlan::StrChars(s.bytes.clone()),
            Obj::Bytes(b) => IterPlan::Elems(
                b.bytes
                    .iter()
                    .map(|&x| Value::SmallInt(x as i64))
                    .collect(),
            ),
            Obj::Range(range) => IterPlan::Range(range.start, range.stop, range.step),
            Obj::Set(s) => IterPlan::Elems(s.items.clone()),
            Obj::Array(a) => {
                // Elements via `binary_get_val_array`
                // (materialised — the same observable as the
                // Zig's getiter loop). The bytes are resolved
                // here; the `Value`s are built below, where
                // `&mut ctx` is free (`binary_get_val_array`
                // may allocate for `I`/`L`/`Q` elements).
                let tc = a.typecode
                    & crate::objects::containers::binary::TYPECODE_MASK;
                let n = a.len;
                match crate::objects::containers::binary::buffer_bytes(ctx, value) {
                    Some((bytes, _)) => IterPlan::ArrayBytes { tc, bytes, n },
                    None => return Err(DictError::TypeError(NOT_ITERABLE)),
                }
            }
            Obj::Iterator(_) => match as_dict_view_it(ctx.arena, value) {
                Some((d, k, cur)) => IterPlan::ViewSteps(d, k, cur),
                None => match as_dict_view(ctx.arena, value) {
                    Some((d, k)) => IterPlan::ViewSteps(d, k, 0),
                    // C11's iterators (and any future getiter-slot
                    // type) land here until the general protocol
                    // exists.
                    None => return Err(DictError::TypeError(NOT_ITERABLE)),
                },
            },
            _ => return Err(DictError::TypeError(NOT_ITERABLE)),
        },
        Value::Qstr(id) => {
            // A 1-char string from iterating a qstr. The Zig interns
            // each char (`qstr_from_strn` → a dynamic qstr); Rust
            // cannot intern through the shared `ctx.pool`, so chars
            // are heap `Str`s — observably identical here (equality
            // and hash are by bytes; `is`-identity of a 1-char string
            // is C8a/C11's str-iterator semantics, not this lane's).
            IterPlan::StrChars(ctx.pool.qstr_str(id as u16).to_vec())
        }
        _ => return Err(DictError::TypeError(NOT_ITERABLE)),
    };
    match plan {
        IterPlan::Elems(v) => Ok(v),
        IterPlan::StrChars(bytes) => Ok(bytes
            .iter()
            .map(|&b| heap_char(ctx, b))
            .collect::<Vec<Value>>()),
        IterPlan::Range(start, stop, step) => {
            let mut out = Vec::new();
            if step != 0 {
                let mut v = start;
                while (step > 0 && v < stop) || (step < 0 && v > stop) {
                    out.push(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, v));
                    match v.checked_add(step) {
                        Some(n) => v = n,
                        // A range spanning past `i64` cannot be
                        // materialised; stop here (the Zig would wrap
                        // the counter instead — a range that large is
                        // unobservable in practice: it would OOM
                        // first).
                        None => break,
                    }
                }
            }
            Ok(out)
        }
        IterPlan::ViewSteps(dict, kind, mut cur) => {
            let dict_ref = dict_ref_of(ctx.arena, dict);
            let mut out = Vec::new();
            while let Some(v) = view_next_entry(ctx, dict_ref, kind, &mut cur) {
                out.push(v);
            }
            Ok(out)
        }
        IterPlan::ArrayBytes { tc, bytes, n } => Ok((0..n)
            .map(|i| {
                crate::objects::containers::binary::binary_get_val_array(ctx, tc, &bytes, i)
            })
            .collect()),
    }
}

/// A 1-byte heap `Str` (one iterated character).
fn heap_char(ctx: &mut CallCtx, b: u8) -> Value {
    Value::Heap(ctx.alloc(
        Some(ctx.types.str_),
        Obj::Str(crate::objects::value::StrData { bytes: vec![b] }),
    ))
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
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_clear(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.copy` (fixed 1 arg).
pub fn dict_copy_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_copy(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.get` (2–3 args: self, key[, default]).
pub fn dict_get_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(DictError::to_pyerror)?;
    dict_get(ctx, first(args), args[1], args.get(2).copied()).map_err(DictError::to_pyerror)
}

/// `dict.pop` (2–3 args: self, key[, default]).
pub fn dict_pop_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(DictError::to_pyerror)?;
    dict_pop(ctx, first(args), args[1], args.get(2).copied()).map_err(DictError::to_pyerror)
}

/// `dict.setdefault` (2–3 args: self, key[, default]).
pub fn dict_setdefault_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(DictError::to_pyerror)?;
    dict_setdefault(ctx, first(args), args[1], args.get(2).copied())
        .map_err(DictError::to_pyerror)
}

/// `dict.popitem` (fixed 1 arg).
pub fn dict_popitem_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_popitem(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.update` (1–2 positionals + kwargs). The `BuiltinFn`
/// signature carries no kwargs map, so this adapter covers the
/// no-kw call; the VM passes kwargs through `dict_update` directly
/// (C15).
pub fn dict_update_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 2).map_err(DictError::to_pyerror)?;
    let empty = MapData::new();
    dict_update(ctx, first(args), args.get(1).copied(), &empty).map_err(DictError::to_pyerror)
}

/// `dict.items` (fixed 1 arg).
pub fn dict_items_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_items(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.keys` (fixed 1 arg).
pub fn dict_keys_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_keys(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.values` (fixed 1 arg).
pub fn dict_values_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(DictError::to_pyerror)?;
    dict_values(ctx, first(args)).map_err(DictError::to_pyerror)
}

/// `dict.fromkeys` (classmethod, 2–3 args: cls, iterable[, value]).
/// The class is ignored (the Zig builds a plain dict).
pub fn dict_fromkeys_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 3).map_err(DictError::to_pyerror)?;
    dict_fromkeys(ctx, args[1], args.get(2).copied()).map_err(DictError::to_pyerror)
}
