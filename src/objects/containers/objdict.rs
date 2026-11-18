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
//! The method implementations (`get` / `pop` / `setdefault` /
//! `popitem` / `update` / `fromkeys`), the dict views (+ their
//! iterators and slots) and the `BuiltinFn` adapters live in
//! `objdict_methods.rs` (C9a-split: the 800-line cap); the
//! `collect_iterable` machinery lives in `objdict_collect.rs`
//! (C13b-split: the same cap). Both are re-exported from this
//! file, so the public paths are unchanged.
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

use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::containers::map::{
    map_lookup, map_of, map_of_mut, map_set_value, map_value, LookupKind, LookupOutcome, MapError,
};
use crate::objects::obj::ObjError;
use crate::objects::obj_ops::{obj_equal, obj_print_helper};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::slots::CallHook;
use crate::objects::value::{DictData, GcRef, MapData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// `TypeError: argument num/types mismatch` (TERSE — verified:
/// `dict({}, {})` on the reference binary).
pub(crate) const ARG_NUM_TYPES_MISMATCH: &str = "argument num/types mismatch";
/// `TypeError: object not iterable` (TERSE — `mp_getiter`,
/// `runtime.zig:1298`).
pub(crate) const NOT_ITERABLE: &str = "object not iterable";
/// `ValueError: dict update sequence has wrong length` (TERSE).
pub(crate) const UPDATE_SEQ_WRONG_LEN: &str = "dict update sequence has wrong length";

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
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, arg/message). A `KeyError` records its key
/// as the value argument.
impl TypedRaise for DictError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        match self {
            DictError::KeyError(v) => Some(*v),
            _ => None,
        }
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        match self {
            DictError::TypeError(m) | DictError::ValueError(m) => Some(m.to_string()),
            DictError::KeyErrorPopitemEmpty => {
                Some("popitem(): dictionary is empty".to_string())
            }
            _ => None,
        }
    }
    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
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

/// `ensureNotFixed` (`objdict.zig`): mutation methods raise a bare
/// `TypeError` on a fixed (ROM) map.
pub(crate) fn ensure_not_fixed(arena: &GcArena, dict_ref: GcRef) -> Result<(), DictError> {
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
pub(crate) fn dict_ref_of(arena: &GcArena, dict: Value) -> GcRef {
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
pub(crate) fn entry_value(ctx: &CallCtx, dict_ref: GcRef, i: usize) -> Value {
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
    call: Option<CallHook<'_, '_>>,
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
        dict_update(ctx, dict, other, &kwargs, call)?;
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
            Err(e) => return Err(ctx.record_typed(DictError::from(e))),
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
                dict_copy(ctx, lhs).map_err(|e| ctx.record_typed(e))?
            } else {
                lhs
            };
            // `d | e` is `d.copy()` + `d.update(e)` (the Zig calls
            // `npy_dt_method_update` with an empty kwargs map).
            let empty = MapData::new();
            dict_update(ctx, target, Some(rhs), &empty, None).map_err(|e| ctx.record_typed(e))?;
            Ok(target)
        }
        BinaryOp::Contains => {
            let r = dict_ref_of(ctx.arena, lhs);
            let found = matches!(
                map_lookup(ctx, ctx.types, r, rhs, LookupKind::Lookup)
                    .map_err(|e| ctx.record_typed(DictError::from(e)))?,
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
        SubscrArg::Get => dict_get_value(ctx, base, index).map_err(|e| ctx.record_typed(e)),
        SubscrArg::Set(v) => {
            dict_store(ctx, base, index, v).map_err(|e| ctx.record_typed(e))?;
            Ok(Value::None)
        }
        SubscrArg::Delete => {
            dict_delete(ctx, base, index).map_err(|e| ctx.record_typed(e))?;
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
        attr: None,
        make_new: Some(crate::objects::objtype::make_new::dict_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}


// The method implementations, views and `BuiltinFn` adapters live in
// `objdict_methods.rs` (C9a-split); re-exported here so the public paths
// (`objdict::dict_update`, `objdict::dict_keys`, …) are unchanged.
// `dict_update` is also used by `dict_make_new` / `dict_binary_op` below.
pub use super::objdict_methods::{
    as_dict_view, as_dict_view_it, dict_clear_builtin, dict_copy_builtin, dict_fromkeys,
    dict_fromkeys_builtin, dict_get, dict_get_builtin, dict_items, dict_items_builtin,
    dict_keys, dict_keys_builtin, dict_pop, dict_pop_builtin, dict_popitem,
    dict_popitem_builtin, dict_setdefault, dict_setdefault_builtin, dict_update,
    dict_update_builtin, dict_values, dict_values_builtin, dict_view_binary_op,
    dict_view_getiter, dict_view_it_next, dict_view_print, dict_view_unary_op, DictViewKind,
};
use super::objdict_methods::dict_get_helper;
use super::objdict_methods::make_view_it;
