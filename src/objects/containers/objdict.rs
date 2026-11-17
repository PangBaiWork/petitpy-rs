//! Dict objects (C9a: `src/objects/containers/objdict.zig`, 990 lines).
//!
//! Port of `py/objdict.c` as re-implemented in Zig: the `dict` type
//! (make_new / print / unary / binary / subscr / getiter), all
//! methods (`clear` / `copy` / `get` / `pop` / `setdefault` /
//! `popitem` / `update` / `items` / `keys` / `values` / `fromkeys`),
//! the dict views (+ their iterators), `OrderedDict` (same slots,
//! `is_ordered` flag), and the `mp_obj_dict_*` public ABI
//! (`dict_store` / `dict_delete` / `dict_get_value` / `dict_len`).
//!
//! # Errors
//!
//! Rich functions return `Result<_, DictError>` (the exception type
//! + data: `KeyError` carries the key, `TypeError`/`ValueError`
//! carry the TERSE literal). The thin slot adapters (`dict_unary_op`
//! etc., which must return `PyResult<Value>`) convert `DictError`
//! to `PyError::PyRaise` — the raise data is dropped at the slot
//! boundary, the accepted C6/C7r convention until C13 adds
//! `VmState.pending_exc` and the adapters construct the exception
//! object there. Tests call the rich functions and match on the
//! data.
//!
//! # Views
//!
//! The Zig's `dict_view` / `dict_view_it` are heap structs with
//! their own types (`mp_type_dict_view`, `mp_type_dict_view_it`).
//! The frozen `Obj` enum has no view variant, so views are
//! `Obj::Iterator` values (the C8a precedent for str/bytes
//! iterators), distinguished by shape:
//!
//! * view: `IteratorData { source: dict, index: kind, extra: [] }`
//!   — `kind` is 0 = items, 1 = keys, 2 = values (`DICT_VIEW_*`);
//! * view-it: `IteratorData { source: dict, index: cur, extra:
//!   [SmallInt(kind)] }` — `cur` is the next table cursor.
//!
//! The header type is `None` for now: the view types do not exist
//! yet (C12 builds all type objects; the view-it type additionally
//! awaits C11's iterator-`next` slot, which `TypeData` has no field
//! for). C12 assigns the real types at bootstrap; until then the
//! shape above is what identifies a view. Views hold the dict
//! (`source`) alive through `Obj::children` (the `Iterator` arm
//! traces `source` + `extra`).
//!
//! # Iteration (`collect_iterable`)
//!
//! `dict_update` / `dict_fromkeys` iterate arbitrary iterables with
//! `mp_getiter` / `mp_iternext` in the Zig. No general iterator
//! protocol exists in the tree yet (C11), so this lane materialises
//! the iterable into a `Vec` for the concrete types available today
//! (list / tuple / dict-keys / str-chars / bytes-ints / range / set
//! items / dict views). Two deliberate deviations, both documented
//! where they occur: a pair is materialised whole before its length
//! is checked (the Zig stops after 3 elements), and a failure during
//! the OUTER iteration leaves nothing inserted yet (the Zig inserts
//! lazily, so earlier pairs survive) — both are unobservable for the
//! concrete types (their iteration cannot fail), and the general
//! protocol (C11) removes them.
//!
//! # `__missing__`
//!
//! NOT implemented, exactly as in the Zig: the `dict_subscr` load
//! path raises `KeyError` (the Zig's own comment records that the
//! subscr slot receives the inner dict storage, not the outer
//! subclass instance, so `__missing__` is unreachable there without
//! runtime support). `Counter[absent] → 0` and `defaultdict` do not
//! work — programs need `d.get(k, 0)`.
//!
//! # Self-referential `repr`
//!
//! The original has NO recursion guard in `dict_print`: printing a
//! self-referential dict recurses until the binary aborts (verified:
//! `d['s'] = d; repr(d)` → stack overflow, rc=134). This port has
//! no guard either (same structure); a Rust stack overflow aborts
//! the same way, at a different depth.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::containers::map::{
    map_lookup, map_of, map_of_mut, map_set_value, map_value, LookupKind, LookupOutcome, MapError,
};
use crate::objects::obj::{obj_id, obj_len_maybe, ObjError};
use crate::objects::obj_ops::{obj_equal, obj_print_helper};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::value::{DictData, GcRef, IteratorData, MapData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// `TypeError: argument num/types mismatch` (TERSE — verified:
/// `dict({}, {})` on the reference binary).
const ARG_NUM_TYPES_MISMATCH: &str = "argument num/types mismatch";
/// `TypeError: object not iterable` (TERSE — `mp_getiter`,
/// `runtime.zig:1298`).
const NOT_ITERABLE: &str = "object not iterable";
/// `ValueError: dict update sequence has wrong length` (TERSE).
const UPDATE_SEQ_WRONG_LEN: &str = "dict update sequence has wrong length";

/// Errors a dict operation can raise. The variant IS the exception
/// type; `TypeError`/`ValueError` carry the TERSE message literal,
/// `KeyError` carries the missing key (the exception printer renders
/// it — C12/C14). `PyRaise` is the C6/C7r slot-boundary carrier: a
/// `__hash__` / `__eq__` slot raised, and the exception object was
/// dropped at the boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum DictError {
    /// `KeyError(key)` — the missing key.
    KeyError(Value),
    /// `KeyError("popitem(): dictionary is empty")`.
    KeyErrorPopitemEmpty,
    /// `TypeError(msg)` — fixed-map mutation (bare, `msg == ""`),
    /// arg-count mismatch, unhashable key, not-iterable, …
    TypeError(&'static str),
    /// `ValueError(msg)` — bad update sequence, …
    ValueError(&'static str),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary — see module docs).
    PyRaise,
}

impl DictError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            DictError::KeyError(_) | DictError::KeyErrorPopitemEmpty => "KeyError",
            DictError::TypeError(_) => "TypeError",
            DictError::ValueError(_) => "ValueError",
            DictError::OutOfMemory => "MemoryError",
            DictError::PyRaise => "",
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention; C13 constructs the exception
    /// object from the `DictError` instead).
    pub fn to_pyerror(self) -> PyError {
        match self {
            DictError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<MapError> for DictError {
    fn from(e: MapError) -> DictError {
        match e {
            MapError::TypeError(m) => DictError::TypeError(m),
            MapError::OutOfMemory => DictError::OutOfMemory,
            MapError::PyRaise => DictError::PyRaise,
        }
    }
}

impl From<ObjError> for DictError {
    fn from(e: ObjError) -> DictError {
        match e {
            ObjError::OutOfMemory => DictError::OutOfMemory,
            _ => DictError::PyRaise,
        }
    }
}

impl From<PyError> for DictError {
    fn from(e: PyError) -> DictError {
        match e {
            PyError::OutOfMemory => DictError::OutOfMemory,
            PyError::PyRaise => DictError::PyRaise,
        }
    }
}

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
fn make_view(ctx: &mut CallCtx, dict: Value, kind: DictViewKind) -> Value {
    let it = IteratorData {
        source: dict,
        index: kind as usize,
        extra: Vec::new(),
    };
    Value::Heap(ctx.alloc(None, Obj::Iterator(it)))
}

/// Build a dict view iterator (`Obj::Iterator`, cursor in `index`,
/// kind in `extra[0]`).
fn make_view_it(ctx: &mut CallCtx, dict: Value, kind: DictViewKind) -> Value {
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

/// `ensureNotFixed` (`objdict.zig`): mutation methods raise a bare
/// `TypeError` on a fixed (ROM) map.
fn ensure_not_fixed(arena: &GcArena, dict_ref: GcRef) -> Result<(), DictError> {
    if map_of(arena, dict_ref).is_fixed {
        // The Zig raises `TypeError(NULL)` here (`raiseTypeErrorNull`,
        // builtinevex.zig:101 — a bare TypeError with NO message), so
        // the Rust error carries the empty message, not a paraphrase.
        return Err(DictError::TypeError(""));
    }
    Ok(())
}

/// The `GcRef` of a heap dict value (panics on a non-dict — a
/// caller-contract violation, as the Zig dereferences the dict
/// pointer unconditionally).
fn dict_ref_of(arena: &GcArena, dict: Value) -> GcRef {
    match dict {
        Value::Heap(r) => match arena.get(r) {
            Obj::Dict(_) => r,
            _ => panic!("dict operation on a non-dict value"),
        },
        _ => panic!("dict operation on a non-dict value"),
    }
}

/// `isDictOrOrdered` (`objdict.zig`): the `make_new` slot is
/// `mp_obj_dict_make_new`. In Rust the payload IS the dict — any
/// `Obj::Dict` (plain, `OrderedDict`, or a future subclass, which
/// shares the payload) counts. The Zig's pointer comparison and this
/// payload check agree for every dict representable today.
pub fn is_dict_or_ordered(arena: &GcArena, v: Value) -> bool {
    matches!(v, Value::Heap(r) if matches!(arena.get(r), Obj::Dict(_)))
}

/// Is `type_` the `OrderedDict` type? The Zig compares the type
/// pointer to `&mp_type_ordereddict`; Rust compares the type's name
/// qstr (the type object is the same one either way — only the
/// `OrderedDict` type object carries that name).
pub fn is_ordered_dict_type(arena: &GcArena, type_: GcRef) -> bool {
    let Some(want) = static_qstr_id(b"OrderedDict") else {
        return false;
    };
    match arena.get(type_) {
        Obj::Type(d) => d.name == want,
        _ => false,
    }
}

/// Is `v` an exact `dict` (header type `== types.dict`)? The Zig's
/// `isExactType(rhs, &mp_type_dict)` in the EQUAL branch — a
/// subclass / `OrderedDict` fails it.
fn is_exact_dict(ctx: &CallCtx, v: Value) -> bool {
    match v {
        Value::Heap(r) => ctx.arena.header(r).type_ == Some(ctx.types.dict),
        _ => false,
    }
}

/// The `GcRef` of the dict's map entry `i` value, read out.
fn entry_value(ctx: &CallCtx, dict_ref: GcRef, i: usize) -> Value {
    map_value(ctx.arena, dict_ref, i)
}

// ── construction ────────────────────────────────────────────────────

/// `mp_obj_new_dict(n)`: a dict with `n` entry slots pre-allocated
/// (the `fromkeys` size hint); type `types.dict`.
pub fn dict_new(ctx: &mut CallCtx, hint: usize) -> Value {
    dict_new_typed(ctx, ctx.types.dict, hint)
}

/// `mp_obj_new_dict` with an explicit type (for `dict_make_new`:
/// subclasses and `OrderedDict` instantiate with their own type).
pub fn dict_new_typed(ctx: &mut CallCtx, type_: GcRef, hint: usize) -> Value {
    Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Dict(DictData {
            map: MapData::new_hint(hint),
        }),
    ))
}

/// `mp_obj_dict_init(self, n_args)`: initialise an existing dict
/// object in place (type already set at allocation; the map is reset
/// to `n_args` slots). The Zig sets the header type here too — the
/// Rust header type is fixed at `alloc`, so this only resets the map.
pub fn dict_init(ctx: &mut CallCtx, dict_ref: GcRef, n_args: usize) {
    *map_of_mut(ctx.arena, dict_ref) = MapData::new_hint(n_args);
}

/// `mp_obj_dict_make_new` (rich): `dict([other][, **kwargs])`.
/// `args` is the Zig layout — positionals first, then the kwargs as
/// interleaved (key, value) pairs; `n_kw` counts the pairs. At most
/// one positional (the type-call machinery checks the make_new sig —
/// C15; this checks it too so direct callers are safe).
pub fn dict_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, DictError> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(DictError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    if n_pos > 1 {
        return Err(DictError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    let dict = dict_new_typed(ctx, type_, 0);
    // OrderedDict: flag the map as ordered.
    if is_ordered_dict_type(ctx.arena, type_) {
        map_of_mut(ctx.arena, dict.as_heap().expect("fresh dict is heap")).is_ordered = true;
    }
    if n_pos > 0 || n_kw > 0 {
        let other = if n_pos > 0 { Some(args[0]) } else { None };
        let kwargs = MapData::new_fixed(
            args[n_pos..]
                .chunks(2)
                .map(|c| (c[0], c[1]))
                .collect::<Vec<(Value, Value)>>(),
        );
        dict_update(ctx, dict, other, &kwargs)?;
    }
    Ok(dict)
}

// ── basic operations (`mp_obj_dict_*` ABI) ──────────────────────────

/// `mp_obj_dict_get`: `d[key]`, or `KeyError(key)`.
pub fn dict_get_value(ctx: &mut CallCtx, dict: Value, key: Value) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    match map_lookup(ctx, ctx.types, r, key, LookupKind::Lookup)? {
        LookupOutcome::Found(i) => Ok(entry_value(ctx, r, i)),
        _ => Err(DictError::KeyError(key)),
    }
}

/// `mp_obj_dict_store`: `d[key] = value` (returns the dict, as the
/// Zig does — the SUBSCR slot returns `None` instead; see
/// `dict_subscr`).
pub fn dict_store(
    ctx: &mut CallCtx,
    dict: Value,
    key: Value,
    value: Value,
) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    ensure_not_fixed(ctx.arena, r)?;
    match map_lookup(ctx, ctx.types, r, key, LookupKind::Add)? {
        LookupOutcome::Added(i) | LookupOutcome::Found(i) => {
            map_set_value(ctx.arena, r, i, value);
            Ok(dict)
        }
        _ => unreachable!("ADD never misses"),
    }
}

/// `mp_obj_dict_delete`: `del d[key]` (returns the dict;
/// `KeyError(key)` when missing — via `dict_get_helper` REMOVE
/// with `n_args == 2`, as in the Zig).
pub fn dict_delete(ctx: &mut CallCtx, dict: Value, key: Value) -> Result<Value, DictError> {
    dict_get_helper(ctx, dict, key, LookupKind::Remove, None)?;
    Ok(dict)
}

/// `mp_obj_dict_len`: the live entry count.
pub fn dict_len(arena: &GcArena, dict: Value) -> usize {
    let r = dict_ref_of(arena, dict);
    map_of(arena, r).used
}

/// `dict_clear` (`npy_dt_method_clear`): empty the dict.
pub fn dict_clear(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    ensure_not_fixed(ctx.arena, r)?;
    map_of_mut(ctx.arena, r).clear();
    Ok(Value::None)
}

/// `mp_obj_dict_copy`: a new dict with the same table (entries +
/// index + tombstones — the Zig `memcpy`s the whole table), the
/// same header type (an `OrderedDict` copy stays ordered), and
/// `is_fixed` forced to 0.
pub fn dict_copy(ctx: &mut CallCtx, dict: Value) -> Result<Value, DictError> {
    let r = dict_ref_of(ctx.arena, dict);
    let (type_, mut map) = match ctx.arena.get(r) {
        Obj::Dict(d) => (ctx.arena.header(r).type_, d.map.clone()),
        _ => unreachable!("dict_ref_of guarantees a dict"),
    };
    map.is_fixed = false;
    Ok(Value::Heap(ctx.alloc(type_, Obj::Dict(DictData { map }))))
}

// ── `dict_get_helper` (get / pop / setdefault) ──────────────────────

/// `dictGetHelperImpl` (`objdict.zig`): the shared core of `.get`
/// (LOOKUP), `.pop` (REMOVE) and `.setdefault` (ADD).
/// `default`: `None` = no default given (`n_args == 2` — `.get` →
/// `None`, `.pop` → `KeyError`, `.setdefault` → insert `None`);
/// `Some(v)` = the default (`n_args == 3` — returned/inserted).
/// A live entry's value is never NULL in Rust (see `MapEntry`), so
/// the Zig's `elem.value == NULL` "missing" test is exactly the
/// `Added` / `NotFound` outcomes here.
fn dict_get_helper(
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
}

fn collect_iterable(ctx: &mut CallCtx, value: Value) -> Result<Vec<Value>, DictError> {
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

/// Is `v` an `OrderedDict` VALUE (header type is the `OrderedDict`
/// type)? The Zig's print/equal check the TYPE (`objBaseType ==
/// &mp_type_ordereddict`), not the map's `is_ordered` layout flag.
fn is_ordered_dict_value(ctx: &CallCtx, v: Value) -> bool {
    match v {
        Value::Heap(r) => match ctx.arena.header(r).type_ {
            Some(t) => is_ordered_dict_type(ctx.arena, t),
            None => false,
        },
        _ => false,
    }
}

/// `npy_dt_unary_op`: BOOL / LEN; anything else → NULL.
pub fn dict_unary_op(ctx: &mut CallCtx, op: UnaryOp, self_v: Value) -> PyResult<Value> {
    let r = match self_v {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Dict(_)) => r,
        _ => return Err(PyError::PyRaise),
    };
    let used = map_of(ctx.arena, r).used;
    match op {
        UnaryOp::Bool => Ok(Value::Bool(used != 0)),
        UnaryOp::Len => Ok(Value::SmallInt(used as i64)),
        _ => Err(PyError::PyRaise),
    }
}

/// `dict_equal` (the EQUAL branch of `npy_dt_binary_op`).
/// Both-`OrderedDict` → order-sensitive compare; otherwise the rhs
/// must be an exact `dict`, same length, and every lhs entry must
/// be present in rhs with an equal value.
fn dict_equal(ctx: &mut CallCtx, lhs: Value, rhs: Value) -> PyResult<bool> {
    if !is_dict_or_ordered(ctx.arena, rhs) {
        return Ok(false);
    }
    let lref = dict_ref_of(ctx.arena, lhs);
    let rref = dict_ref_of(ctx.arena, rhs);
    if is_ordered_dict_value(ctx, lhs) && is_ordered_dict_value(ctx, rhs) {
        let (lu, ru) = (
            map_of(ctx.arena, lref).used,
            map_of(ctx.arena, rref).used,
        );
        if lu != ru {
            return Ok(false);
        }
        let mut cl = 0;
        let mut cr = 0;
        loop {
            let il = map_of(ctx.arena, lref).next_live(&mut cl);
            let ir = map_of(ctx.arena, rref).next_live(&mut cr);
            match (il, ir) {
                (None, None) => return Ok(true),
                (None, _) | (_, None) => return Ok(false),
                (Some(i), Some(j)) => {
                    let (lk, lv) = {
                        let m = map_of(ctx.arena, lref);
                        (
                            m.entries[i].key.expect("live entry has a key"),
                            m.entries[i].value,
                        )
                    };
                    let (rk, rv) = {
                        let m = map_of(ctx.arena, rref);
                        (
                            m.entries[j].key.expect("live entry has a key"),
                            m.entries[j].value,
                        )
                    };
                    if !obj_equal(ctx, ctx.types, lk, rk)? {
                        return Ok(false);
                    }
                    if !obj_equal(ctx, ctx.types, lv, rv)? {
                        return Ok(false);
                    }
                }
            }
        }
    }
    if !is_exact_dict(ctx, rhs) {
        return Ok(false);
    }
    if map_of(ctx.arena, lref).used != map_of(ctx.arena, rref).used {
        return Ok(false);
    }
    let mut cur = 0;
    while let Some(i) = map_of(ctx.arena, lref).next_live(&mut cur) {
        let (k, v) = {
            let m = map_of(ctx.arena, lref);
            (
                m.entries[i].key.expect("live entry has a key"),
                m.entries[i].value,
            )
        };
        match map_lookup(ctx, ctx.types, rref, k, LookupKind::Lookup) {
            Ok(LookupOutcome::Found(j)) => {
                let rv = map_value(ctx.arena, rref, j);
                if !obj_equal(ctx, ctx.types, v, rv)? {
                    return Ok(false);
                }
            }
            Ok(_) => return Ok(false),
            Err(e) => return Err(DictError::from(e).to_pyerror()),
        }
    }
    Ok(true)
}

/// `npy_dt_binary_op`: CONTAINS / EQUAL, plus `|` / `|=` under full
/// (`CPYTHON_COMPAT` — the Zig gates them on `is_full`; this port
/// is full-only). Anything else → NULL.
pub fn dict_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    match op {
        BinaryOp::Or | BinaryOp::InplaceOr => {
            let target = if op == BinaryOp::Or {
                dict_copy(ctx, lhs).map_err(DictError::to_pyerror)?
            } else {
                lhs
            };
            // `d | e` is `d.copy()` + `d.update(e)` (the Zig calls
            // `npy_dt_method_update` with an empty kwargs map).
            let empty = MapData::new();
            dict_update(ctx, target, Some(rhs), &empty).map_err(DictError::to_pyerror)?;
            Ok(target)
        }
        BinaryOp::Contains => {
            let r = dict_ref_of(ctx.arena, lhs);
            let found = matches!(
                map_lookup(ctx, ctx.types, r, rhs, LookupKind::Lookup)
                    .map_err(|e| DictError::from(e).to_pyerror())?,
                LookupOutcome::Found(_)
            );
            Ok(Value::Bool(found))
        }
        BinaryOp::Equal => Ok(Value::Bool(dict_equal(ctx, lhs, rhs)?)),
        _ => Err(PyError::PyRaise),
    }
}

/// `npy_dt_subscr`: load (`Get`) / store (`Set`) / delete
/// (`Delete`). Load of a missing key raises `KeyError` — the
/// `__missing__` protocol is NOT implemented (see the module docs:
/// the Zig's own comment records it as unreachable from this slot).
/// Store/delete return `None` (the `mp_obj_dict_store` / `delete`
/// ABI functions return the dict instead).
pub fn dict_subscr(
    ctx: &mut CallCtx,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    match arg {
        SubscrArg::Get => dict_get_value(ctx, base, index).map_err(DictError::to_pyerror),
        SubscrArg::Set(v) => {
            dict_store(ctx, base, index, v).map_err(DictError::to_pyerror)?;
            Ok(Value::None)
        }
        SubscrArg::Delete => {
            dict_delete(ctx, base, index).map_err(DictError::to_pyerror)?;
            Ok(Value::None)
        }
    }
}

/// `npy_dt_print`: `{k: v, …}` with REPR keys/values (the `kind`
/// parameter is ignored, as in the Zig — `_: c_int`). An
/// `OrderedDict` prints as `OrderedDict({...})`. No recursion guard
/// (see the module docs — the original aborts on self-reference).
pub fn dict_print(
    ctx: &mut CallCtx,
    self_v: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let ordered = is_ordered_dict_value(ctx, self_v);
    if ordered {
        let _ = write!(sink, "OrderedDict(");
    }
    let _ = write!(sink, "{{");
    let r = match self_v {
        Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Dict(_)) => r,
        _ => {
            let _ = write!(sink, "}}");
            if ordered {
                let _ = write!(sink, ")");
            }
            return;
        }
    };
    let mut cur = 0;
    let mut first = true;
    while let Some(i) = map_of(ctx.arena, r).next_live(&mut cur) {
        if !first {
            let _ = write!(sink, ", ");
        }
        first = false;
        let (k, v) = {
            let m = map_of(ctx.arena, r);
            (
                m.entries[i].key.expect("live entry has a key"),
                m.entries[i].value,
            )
        };
        obj_print_helper(ctx, ctx.types, k, PrintKind::Repr, sink);
        let _ = write!(sink, ": ");
        obj_print_helper(ctx, ctx.types, v, PrintKind::Repr, sink);
    }
    let _ = write!(sink, "}}");
    if ordered {
        let _ = write!(sink, ")");
    }
}

/// `npy_dt_getiter`: iterate KEYS (a view iterator, kind KEYS).
pub fn dict_getiter(ctx: &mut CallCtx, self_v: Value) -> PyResult<Value> {
    Ok(make_view_it(ctx, self_v, DictViewKind::Keys))
}

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

// ── type data ───────────────────────────────────────────────────────

/// `mp_type_dict` ROM data. `dict` is the `locals_dict` ref (the
/// method table — built by C12/D1 from the `*_builtin` adapters
/// below); `mro` is filled with `[self]` by the bootstrap after
/// allocation (the C7 `int_type_data` precedent). Slot order from
/// `MP_DEFINE_CONST_OBJ_TYPE`: make_new=1, print=2, unary_op=3,
/// binary_op=4, subscr=5, iter=6, locals_dict=7 — `make_new` is the
/// type-call path, wired by C12, not a `TypeData` slot.
pub fn dict_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"dict").expect("MP_QSTR_dict is in the static table"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(dict_unary_op),
        binary_op: Some(dict_binary_op),
        call: None,
        getiter: Some(dict_getiter),
        subscr: Some(dict_subscr),
        buffer: None,
        print: Some(dict_print),
    }
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
