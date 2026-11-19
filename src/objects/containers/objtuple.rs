//! `objtuple.zig` — the `tuple` type (388 lines, 0 test blocks).
//!
//! Ported from `src/objects/containers/objtuple.zig`.
//!
//! Faithful points:
//! - print: kind ignored, always `PRINT_REPR` elements, `"(a, b)"`,
//!   a lone element gets a trailing comma `"(a,)"`.
//! - `make_new`: sig `(0, 1, false)` — but the Zig sig check is
//!   **polled-only** (`runtime/argcheck.zig:100`: it sets
//!   `pending_exc` and returns; `mp_obj_tuple_make_new` never
//!   polls it). So a too-many-args error survives only if nothing
//!   later overwrites it: `tuple(1, 2)` → "object not iterable"
//!   (the `getiter` failure overwrites), `tuple("a", "b")` →
//!   "argument num/types mismatch" (the `getiter` succeeds), and
//!   `tuple((1, 2), 99)` → "argument num/types mismatch" (the
//!   exact-tuple early return leaves the pending error in place).
//!   The Rust `tuple_make_new` models this with a recorded
//!   `sig_err` that a later failure overwrites and a later
//!   success preserves. `n_args == 0` → the ROM empty-tuple
//!   singleton; exact-tuple arg → returned as-is; else iterate
//!   with a doubling alloc starting at 4.
//! - unary HASH: seed + wrapping `+%` sum of element hashes,
//!   returned through `newSmallInt` (tag, no window check — a
//!   sum outside the 62-bit window wraps, e.g.
//!   `hash((2**62-1, 2**62-1))` is `-2` with seed 0). An
//!   unhashable element propagates the element's hash failure:
//!   `hash(([],))` → `TypeError("unsupported type for operator")`
//!   (vendor `mp_unary_op` raises it when the slot returns
//!   NULL — see the probe notes in `tuple_tests.rs`).
//! - binary ADD: rhs must be tuple-subclass-compatible (exact
//!   tuple here — no user subclasses yet) else NULL; MULTIPLY:
//!   `get_int_maybe` rhs else NULL, `n <= 0` → the empty tuple
//!   (the ROM singleton in Zig); comparisons via
//!   `tupleCmpHelper` → `seq_cmp_objs`: direct when the rhs
//!   type's iter slot IS the tuple getiter (covers attrtuples —
//!   objattrtuple.zig:109); the subclass slow path
//!   (cast-to-native-base) is C12's.
//! - subscr: `value != SENTINEL` → NULL (no set/delete — the
//!   dispatch raises "object doesn't support item
//!   assignment/deletion"); a slice loads through
//!   `fast_slice_indexes`, else `NotImplementedError("only
//!   slices with step=1 (aka None) are supported")`; otherwise
//!   `mp_get_index`.
//! - `count`/`index` delegate to `seq_count_obj` /
//!   `seq_index_obj`.
//! - getiter: the polymorph-iter overlay `(tuple, cur)`.
//!
//! Divergences (documented, not hidden):
//! - **Hash seed is the empty tuple's arena index, not the
//!   reference binary's 22815552.** The Zig seeds with the ROM
//!   empty-tuple object's *address*
//!   (`@bitCast(objFromPtr(&mp_const_empty_tuple_obj))`) — a
//!   link artifact of the Zig binary, not a language semantic;
//!   the address is stable across runs only because the binary
//!   is non-PIE. The *structure* (seed + wrapping sum of
//!   element hashes) is ported faithfully, and `hash(()) ==
//!   id(())` holds in Rust too now that the singleton has
//!   landed (C12a): the seed is `ctx.types.empty_tuple.0`.
//!   Reference-binary probe values are recorded in
//!   `tuple_tests.rs` (`hash((1,2)) == 22815555` there,
//!   `== seed + 3` here).
//! - **The ROM empty-tuple singleton landed (C12a)**:
//!   `new_tuple(0)` returns `ctx.types.empty_tuple`, so `()
//!   is ()` is `True` and `tuple()` / `x * 0` return the
//!   singleton, as in the Zig (`mp_const_empty_tuple_obj`).

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::containers::map::{hash_of, MapError};
use crate::objects::containers::objdict::DictError;
use crate::objects::containers::objdict_methods::collect_iterable;
use crate::objects::containers::sequence::{seq_cmp_objs, seq_count_obj, seq_index_obj};
use crate::objects::obj::{obj_get_int_maybe, obj_get_type, ObjError};
use crate::objects::objtype::subclass::{cast_to_native_base, is_subclass_fast};
use crate::objects::obj_ops::{get_index, obj_print_helper};
use crate::objects::slots::{BinaryOp, GetIterFn, PrintKind, SubscrArg, UnaryOp};
use crate::objects::slots::CallHook;
use crate::objects::strings::objstr_a::{fast_slice_indexes, StrError};
use crate::objects::value::{Obj, SliceData, TypeData};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::{PyError, PyResult};

/// `mirrors.newSmallInt(n)` — re-exported from `crate::objects::value`
/// (single definition; the tuple hash and the int hash both use it).
pub use crate::objects::value::wrap_small_int;

/// Errors from the tuple operations (the C9a `DictError`
/// shape: `exc_name()` for the VM's pending-exception wiring,
/// `to_pyerror()` for the slot boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TupleError {
    /// `NotImplementedError("only slices with step=1 (aka None)
    /// are supported")` — tuple slice load with `step != 1`.
    SliceStep,
    /// `TypeError("argument num/types mismatch")`.
    ArgNumTypesMismatch,
    /// `ValueError("object not in sequence")` — `index` miss.
    NotInSequence,
    /// `TypeError("unsupported type for operator")` — an
    /// element's HASH slot answered NULL (vendor `mp_unary_op`
    /// raises it); e.g. `hash(([],))`.
    UnsupportedOperator,
    /// `TypeError("object not iterable")` — `tuple_make_new`
    /// (via `collect_iterable`) or `tuple.index` on a
    /// non-iterable first argument.
    NotIterable,
    /// A `get_index` failure (non-int index, out-of-range,
    /// overflow) — the `ObjError` carries the type/message.
    Obj(ObjError),
    /// A slot raised while comparing / iterating (data dropped
    /// at the boundary — the accepted C6/C7r convention).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl TupleError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            TupleError::SliceStep => "NotImplementedError",
            TupleError::ArgNumTypesMismatch => "TypeError",
            TupleError::NotInSequence => "ValueError",
            TupleError::UnsupportedOperator => "TypeError",
            TupleError::NotIterable => "TypeError",
            TupleError::Obj(e) => e.exc_type().map(|t| t.name()).unwrap_or(""),
            TupleError::OutOfMemory => "MemoryError",
            TupleError::PyRaise => "",
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` /
    /// `PyRaise`).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            TupleError::SliceStep => {
                Some("only slices with step=1 (aka None) are supported")
            }
            TupleError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            TupleError::NotInSequence => Some("object not in sequence"),
            TupleError::UnsupportedOperator => Some("unsupported type for operator"),
            TupleError::NotIterable => Some("object not iterable"),
            TupleError::Obj(e) => e.message(),
            TupleError::OutOfMemory | TupleError::PyRaise => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            TupleError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for TupleError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.message().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
    }
}

impl From<ObjError> for TupleError {
    fn from(e: ObjError) -> TupleError {
        match e {
            ObjError::OutOfMemory => TupleError::OutOfMemory,
            other => TupleError::Obj(other),
        }
    }
}

impl From<crate::objects::containers::sequence::SeqError> for TupleError {
    fn from(e: crate::objects::containers::sequence::SeqError) -> TupleError {
        match e {
            crate::objects::containers::sequence::SeqError::NotInSequence => {
                TupleError::NotInSequence
            }
            crate::objects::containers::sequence::SeqError::Obj(o) => TupleError::from(o),
            crate::objects::containers::sequence::SeqError::PyRaise => TupleError::PyRaise,
            crate::objects::containers::sequence::SeqError::OutOfMemory => {
                TupleError::OutOfMemory
            }
        }
    }
}

impl From<DictError> for TupleError {
    fn from(e: DictError) -> TupleError {
        match e {
            DictError::TypeError("object not iterable") => TupleError::NotIterable,
            DictError::OutOfMemory => TupleError::OutOfMemory,
            // Any other `DictError` from `collect_iterable` (a
            // `KeyError`/`ValueError` from a dict-view step, a
            // swallowed slot raise) carries no message this lane
            // can reuse — the slot boundary drops it anyway.
            _ => TupleError::PyRaise,
        }
    }
}

/// The items of a heap tuple, if `value` is one.
pub fn tuple_items<'a>(ctx: &'a CallCtx<'_>, value: Value) -> Option<&'a [Value]> {
    let Value::Heap(r) = value else {
        return None;
    };
    match ctx.arena.get(r) {
        Obj::Tuple(items) => Some(items),
        // An attrtuple IS a tuple in the Zig (`objattrtuple.zig:3`:
        // "a regular `mp_obj_tuple_t` whose flex `items[]`...") —
        // the tuple slots operate on the shared layout, so the
        // items accessor covers both payloads. (Wired at the C9c
        // rebase for `attrtuple_type_data`.)
        Obj::AttrTuple(a) => Some(&a.items),
        _ => None,
    }
}

/// Is `value` an *exact* tuple of `tuple_type` (no subclass
/// check — C12 owns subclasses)?
pub fn is_exact_tuple(ctx: &CallCtx, tuple_type: GcRef, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    match ctx.arena.get(r) {
        Obj::Tuple(_) => ctx.arena.header(r).type_ == Some(tuple_type),
        _ => false,
    }
}

/// `mp_obj_new_tuple(n, items)`: allocate a tuple holding
/// `items`. For `n == 0` the Zig returns the ROM singleton
/// (`mp_const_empty_tuple_obj`); Rust returns
/// `ctx.types.empty_tuple` (C12a — the singleton landed, see
/// the module docs), so `() is ()` holds.
pub fn new_tuple(ctx: &mut CallCtx, tuple_type: GcRef, items: &[Value]) -> GcRef {
    if items.is_empty() {
        return ctx.types.empty_tuple;
    }
    ctx.alloc(Some(tuple_type), Obj::Tuple(items.to_vec()))
}

/// `mp_obj_tuple_make_new` (`objtuple.zig:113-135`): sig
/// `(0, 1, false)` — polled-only (see the module docs for the
/// error-ordering model). Extra positional args are *ignored*
/// when the first arg is an exact tuple or an iterable; a
/// non-iterable first arg reports "object not iterable"
/// instead of the arg-count error.
pub fn tuple_make_new(
    ctx: &mut CallCtx,
    tuple_type: GcRef,
    args: &[Value],
    n_kw: usize,
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, TupleError> {
    // The polled sig check: record the error, keep going.
    let sig_err = if n_kw > 0 || args.len() > 1 {
        Some(TupleError::ArgNumTypesMismatch)
    } else {
        None
    };
    if args.is_empty() {
        return match sig_err {
            Some(e) => Err(e),
            None => Ok(Value::Heap(new_tuple(ctx, tuple_type, &[]))),
        };
    }
    if is_exact_tuple(ctx, tuple_type, args[0]) {
        return match sig_err {
            Some(e) => Err(e),
            None => Ok(args[0]),
        };
    }
    // `collect_iterable` is C9a's `getiter`/`iternext` loop; a
    // failure here OVERWRITES the recorded sig error (the Zig's
    // `mp_getiter` sets `pending_exc` again).
    let items = collect_iterable(ctx, args[0], call)?;
    if let Some(e) = sig_err {
        return Err(e);
    }
    Ok(Value::Heap(new_tuple(ctx, tuple_type, &items)))
}

/// The tuple HASH slot body (`objtuple.zig:166-175`): the seed
/// plus the wrapping sum of the element hashes. The element
/// hash is C9a's `hash_of` — the full vendor `mp_unary_op`
/// HASH semantics: small int → itself; str/bytes → the qstr
/// hash; a type whose HASH slot answers NULL → `TypeError
/// ("unsupported type for operator")` (e.g. `hash(([],))`);
/// a type with NO unary_op slot at all → the identity word
/// (so `hash(None) == 6`, `hash(True) == 1` via C7's bool
/// slot delegating to int). The sum is returned through
/// `newSmallInt` (tag, no window check — see
/// `wrap_small_int`).
pub fn tuple_hash(ctx: &mut CallCtx, items: &[Value]) -> Result<i64, TupleError> {
    let types = ctx.types;
    // The seed is the empty tuple's identity word (C12a — the
    // ROM singleton; the Zig uses its address, 22815552 in the
    // reference binary). `hash(()) == id(())` holds.
    let mut hash: i64 = ctx.types.empty_tuple.0 as i64;
    for item in items {
        match hash_of(ctx, types, *item) {
            // `hash_of` returns `usize`; the `as i64` cast is
            // bit-preserving, so a negative small-int hash
            // round-trips exactly.
            Ok(h) => hash = hash.wrapping_add(h as i64),
            Err(MapError::TypeError("unsupported type for operator")) => {
                return Err(TupleError::UnsupportedOperator);
            }
            Err(MapError::OutOfMemory) => return Err(TupleError::OutOfMemory),
            Err(_) => return Err(TupleError::PyRaise),
        }
    }
    Ok(wrap_small_int(hash))
}

/// `mp_obj_tuple_unary_op` (`objtuple.zig:161-178`): BOOL, HASH,
/// LEN — anything else answers NULL.
pub fn tuple_unary_op(
    ctx: &mut CallCtx,
    op: UnaryOp,
    self_: Value,
) -> PyResult<Value> {
    // Clone first: `tuple_hash` needs `&mut ctx`.
    let items: Vec<Value> = tuple_items(ctx, self_).unwrap_or(&[]).to_vec();
    let len = items.len();
    match op {
        UnaryOp::Bool => Ok(Value::Bool(len != 0)),
        UnaryOp::Len => Ok(Value::SmallInt(len as i64)),
        UnaryOp::Hash => match tuple_hash(ctx, &items) {
            Ok(h) => Ok(Value::SmallInt(h)),
            Err(TupleError::OutOfMemory) => Err(PyError::OutOfMemory),
            Err(_) => Err(PyError::PyRaise),
        },
        _ => Err(PyError::PyRaise),
    }
}

/// `tupleCmpHelper` (`objtuple.zig:146-159`): the rhs is compared
/// directly when its type's iter slot IS the tuple's getiter
/// (that covers attrtuples — their iter slot is
/// `mp_obj_tuple_getiter`, objattrtuple.zig:109); otherwise the
/// slow path for user subclasses (C12's cast-to-native-base)
/// applies, and the slot answers NULL for now.
fn tuple_cmp(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: &[Value],
    rhs_value: Value,
) -> Result<bool, TupleError> {
    if !rhs_has_tuple_getiter(ctx, rhs_value) {
        // Slow path for user subclasses (objtuple.zig:153-158):
        // the rhs type's iter slot is not the tuple's (a user
        // class gets `instance_getiter_slot`), so cast to the
        // native tuple base instead of failing. A tuple
        // subclass instance is an `Obj::Instance` whose
        // subobj[0] IS the underlying tuple —
        // `cast_to_native_base` returns it (or the value
        // itself when it already is a tuple); anything else is
        // still "not implemented".
        let Some(cast) = cast_to_native_base(
            ctx.arena,
            ctx.types,
            rhs_value,
            Value::Heap(ctx.types.tuple),
        ) else {
            return Err(TupleError::PyRaise);
        };
        let rhs: Vec<Value> = tuple_items(ctx, cast).unwrap_or(&[]).to_vec();
        return Ok(seq_cmp_objs(ctx, op, lhs, &rhs)?);
    }
    // Clone first: `seq_cmp_objs` needs `&mut ctx`.
    let rhs: Vec<Value> = tuple_items(ctx, rhs_value).unwrap_or(&[]).to_vec();
    Ok(seq_cmp_objs(ctx, op, lhs, &rhs)?)
}

/// Does `value`'s type use the tuple's getiter slot?
/// (`tupleCmpHelper`'s `typeIterSlot(another_type) !=
/// getiter_zig_ptr` test, objtuple.zig:152-153.)
fn rhs_has_tuple_getiter(ctx: &CallCtx, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    let Some(t) = ctx.arena.header(r).type_ else {
        return false;
    };
    match ctx.arena.get(t) {
        // `fn_addr_eq` (not `==`): fn-pointer `==` is linted as
        // unreliable, and the stored slot must be the bare
        // `tuple_getiter` item (`tuple_type_data` registers it
        // unwrapped — a closure wrapper would compare unequal).
        Obj::Type(td) => td
            .getiter
            .is_some_and(|f| std::ptr::fn_addr_eq(f, tuple_getiter as GetIterFn)),
        _ => false,
    }
}

/// `mp_obj_tuple_binary_op` (`objtuple.zig:180-214`): ADD /
/// MULTIPLY / comparisons — anything else answers NULL.
pub fn tuple_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    let tuple_type = types.tuple;
    // Clone first: the arms below allocate (`new_tuple`) and
    // compare (`seq_cmp_objs`) through `&mut ctx`.
    let items: Vec<Value> = tuple_items(ctx, lhs).unwrap_or(&[]).to_vec();
    match op {
        BinaryOp::Add | BinaryOp::InplaceAdd => {
            // `objtuple.zig:185-189` — the rhs must be
            // tuple-subclass-compatible
            // (`mp_obj_is_subclass_fast(rhs_type, tuple)`), not
            // an exact tuple (C12a closes C9c's recorded
            // divergence: a namedtuple / tuple-subclass value
            // concatenates). The rhs must also actually carry a
            // tuple payload — a user-class instance's items live
            // in its native-base subobj and never reach this
            // slot (the instance machinery intercepts first),
            // so a bare `Instance` payload is not addable here
            // (the Zig reads it through `fromObj`, which is UB
            // for an instance).
            let rhs_ok = obj_get_type(ctx.arena, types, rhs)
                .map(|t| is_subclass_fast(ctx.arena, Value::Heap(t), Value::Heap(tuple_type)))
                .unwrap_or(false);
            let Some(rhs_items) = tuple_items(ctx, rhs).map(|s| s.to_vec()) else {
                return Err(PyError::PyRaise);
            };
            if !rhs_ok {
                return Err(PyError::PyRaise);
            }
            let mut out = Vec::with_capacity(items.len() + rhs_items.len());
            out.extend_from_slice(&items);
            out.extend_from_slice(&rhs_items);
            Ok(Value::Heap(new_tuple(ctx, tuple_type, &out)))
        }
        BinaryOp::Multiply | BinaryOp::InplaceMultiply => {
            let n = match obj_get_int_maybe(ctx, types, rhs) {
                Ok(Some(n)) => n,
                _ => return Err(PyError::PyRaise),
            };
            if n <= 0 {
                return Ok(Value::Heap(new_tuple(ctx, tuple_type, &[])));
            }
            let out = match crate::objects::containers::sequence::seq_multiply(&items, n)
            {
                Some(out) => out,
                None => return Err(PyError::OutOfMemory),
            };
            Ok(Value::Heap(new_tuple(ctx, tuple_type, &out)))
        }
        BinaryOp::Equal
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::More
        | BinaryOp::MoreEqual => {
            match tuple_cmp(ctx, op, &items, rhs) {
                Ok(b) => Ok(Value::Bool(b)),
                Err(TupleError::OutOfMemory) => Err(PyError::OutOfMemory),
                Err(_) => Err(PyError::PyRaise),
            }
        }
        _ => Err(PyError::PyRaise),
    }
}

/// `tupleSliceLoad` (`objtuple.zig:216-225`): fast slice
/// indexes else `NotImplementedError("only slices with step=1
/// (aka None) are supported")`. A `step == 0` failure is
/// overwritten by that same message (probed:
/// `(1,2,3)[::0]`); a non-int bound keeps its own error
/// (`TypeError("can't convert to int")`, probed).
pub fn tuple_slice_load(
    ctx: &mut CallCtx,
    tuple_type: GcRef,
    items: &[Value],
    slice: &SliceData,
) -> Result<Value, TupleError> {
    let types = ctx.types;
    let bound = match fast_slice_indexes(ctx, types, slice, items.len() as i64) {
        Ok((bound, true)) => bound,
        Ok((_, false)) => return Err(TupleError::SliceStep),
        Err(StrError::SliceStepZero) => return Err(TupleError::SliceStep),
        Err(StrError::Obj(o)) => return Err(TupleError::from(o)),
        Err(_) => return Err(TupleError::PyRaise),
    };
    let span = (bound.stop - bound.start).max(0) as usize;
    let start = bound.start.max(0) as usize;
    let end = (start + span).min(items.len());
    Ok(Value::Heap(new_tuple(
        ctx,
        tuple_type,
        &items[start..end],
    )))
}

/// `mpObjTupleSubscrImpl` (`objtuple.zig:227-237`): set/delete
/// → NULL; slice → `tupleSliceLoad`; else `mp_get_index`.
pub fn tuple_subscr(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let types = ctx.types;
    let tuple_type = types.tuple;
    match arg {
        SubscrArg::Set(_) | SubscrArg::Delete => Err(PyError::PyRaise),
        SubscrArg::Get => {
            // Clone first: the arms below allocate / resolve
            // indexes through `&mut ctx`.
            let items: Vec<Value> = tuple_items(ctx, self_).unwrap_or(&[]).to_vec();
            // A slice index is a heap `Obj::Slice` (C9c's type).
            let slice_data: Option<SliceData> = match index {
                Value::Heap(r) => match ctx.arena.get(r) {
                    Obj::Slice(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(slice) = slice_data {
                return match tuple_slice_load(ctx, tuple_type, &items, &slice) {
                    Ok(v) => Ok(v),
                    Err(TupleError::OutOfMemory) => Err(PyError::OutOfMemory),
                    Err(e) => Err(ctx.record_typed(e)),
                };
            }
            let idx = match get_index(ctx, types, items.len(), index, false) {
                Ok(i) => i,
                Err(ObjError::OutOfMemory) => return Err(PyError::OutOfMemory),
                Err(e) => return Err(ctx.record_typed(e)),
            };
            Ok(items[idx])
        }
    }
}

/// `tuple.count(value)` (`objtuple.zig:246-249`).
pub fn tuple_count(
    ctx: &mut CallCtx,
    items: &[Value],
    value: Value,
) -> Result<Value, TupleError> {
    Ok(Value::SmallInt(seq_count_obj(ctx, items, value)? as i64))
}

/// `tuple.index(value[, start[, stop]])` (`objtuple.zig:252-255`).
pub fn tuple_index(
    ctx: &mut CallCtx,
    items: &[Value],
    value: Value,
    start: Option<Value>,
    stop: Option<Value>,
) -> Result<Value, TupleError> {
    Ok(Value::SmallInt(
        seq_index_obj(ctx, items, value, start, stop)? as i64,
    ))
}

/// `mp_obj_tuple_getiter` (`objtuple.zig:292-294`): the
/// polymorph-iter overlay `(tuple, cur)`. Header is the
/// `iterator` ROM type (G4/H8 — `mp_type_polymorph_iter`).
pub fn tuple_getiter(ctx: &mut CallCtx, self_: Value) -> PyResult<Value> {
    Ok(Value::Heap(ctx.alloc(
        Some(ctx.types.iterator),
        Obj::Iterator(crate::objects::value::IteratorData {
            source: self_,
            index: 0,
            extra: vec![Value::SmallInt(
                crate::objects::containers::sequence::TUPLE_ITER_TAG,
            )],
        }),
    )))
}

/// `npy_tup_it_iternext` (`objtuple.zig:297-305`): the next
/// element, or `None` at exhaustion (C9a's
/// `dict_view_it_next` convention; C11 formalises it). The
/// cursor advances in place in the arena slot.
pub fn tuple_iternext(
    ctx: &mut CallCtx,
    iter: Value,
) -> Result<Option<Value>, TupleError> {
    let Value::Heap(r) = iter else {
        return Err(TupleError::PyRaise);
    };
    let (source, index) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.index),
        _ => return Err(TupleError::PyRaise),
    };
    let items: Vec<Value> = tuple_items(ctx, source).unwrap_or(&[]).to_vec();
    if index < items.len() {
        if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
            d.index = index + 1;
        }
        Ok(Some(items[index]))
    } else {
        Ok(None)
    }
}

/// `mp_obj_tuple_print` (`objtuple.zig:100-111`): kind
/// ignored, always `PRINT_REPR` elements, `"(a, b)"`, a lone
/// element gets a trailing comma.
pub fn tuple_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let types = ctx.types;
    let items: Vec<Value> = tuple_items(ctx, value).unwrap_or(&[]).to_vec();
    let _ = sink.write_str("(");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            let _ = sink.write_str(", ");
        }
        obj_print_helper(ctx, types, *item, PrintKind::Repr, sink);
    }
    if items.len() == 1 {
        let _ = sink.write_str(",");
    }
    let _ = sink.write_str(")");
}

/// `mp_type_tuple` (`objtuple.zig:340-373`): flags `0x0000`
/// (no `EQ_CHECKS_OTHER_TYPE` — `(1,2) == [1,2]` skips the
/// tuple slot in dispatch), slots make_new/print/unary/
/// binary/subscr/getiter/locals_dict. `make_new` is the
/// type-call path (C12) and is not a `TypeData` slot.
pub fn tuple_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"tuple")
            .expect("tuple is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(tuple_unary_op),
        binary_op: Some(tuple_binary_op),
        call: None,
        getiter: Some(tuple_getiter),
        subscr: Some(tuple_subscr),
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn crate::objects::slots::ByteSink| {
                tuple_print(ctx, value, kind, sink)
            },
        ),
        attr: None,
        make_new: Some(crate::objects::objtype::make_new::tuple_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// `tuple.count` as a `BuiltinFn` adapter (2 fixed args:
/// self, value) for C12's locals_dict.
pub fn tuple_count_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() != 2 {
        return Err(PyError::PyRaise);
    }
    let items = tuple_items(ctx, args[0]).unwrap_or(&[]).to_vec();
    tuple_count(ctx, &items, args[1]).map_err(|e| ctx.record_typed(e))
}

/// `tuple.index` as a `BuiltinFn` adapter (var 2..4 args:
/// self, value[, start[, stop]]; the sig is enforced by the
/// call machinery — C12 — this adapter also guards it).
pub fn tuple_index_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() < 2 || args.len() > 4 {
        return Err(PyError::PyRaise);
    }
    let items = tuple_items(ctx, args[0]).unwrap_or(&[]).to_vec();
    let start = args.get(2).copied();
    let stop = args.get(3).copied();
    tuple_index(ctx, &items, args[1], start, stop).map_err(|e| ctx.record_typed(e))
}

/// `tuple`'s method table (C13, the Zig's `mp_obj_tuple_methods`
/// shape): name, body, positional arity counting `self`. Sigs
/// are the ones the adapters already enforce (see their doc
/// comments).
pub static TUPLE_METHODS: &[crate::objects::slots::MethodEntry] = &[
    crate::objects::slots::MethodEntry {
        name: "count",
        func: tuple_count_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "index",
        func: tuple_index_builtin,
        min: 2,
        max: 4,
        kw_func: None,
    },
];

/// `tuple()` as a `BuiltinFn`-shaped constructor for the
/// type-call path (C12): `tuple_make_new` with the args as
/// given. `n_kw` is 0 here — the keyword form is C12's.
pub fn tuple_make_new_builtin(
    ctx: &mut CallCtx,
    args: &[Value],
) -> PyResult<Value> {
    let tuple_type = ctx.types.tuple;
    tuple_make_new(ctx, tuple_type, args, 0, None).map_err(|e| ctx.record_typed(e))
}
