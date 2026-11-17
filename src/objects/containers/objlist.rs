//! `objlist.zig` — the `list` type (717 lines, 0 test blocks).
//!
//! Ported from `src/objects/containers/objlist.zig`. The methods
//! (`append` … `sort`) live in `objlist_methods.rs` so neither
//! file exceeds the 800-line cap.
//!
//! Faithful points:
//! - `LIST_MIN_ALLOC = 4`; `allocList` grows ×2 (`listAppendInline`
//!   doubles when `len >= alloc`; `pop` halves when `alloc > 4`
//!   and `alloc > 2 * len`). The capacity itself is unobservable
//!   in Rust (`Vec` manages it) and is not tracked.
//! - `make_new`: sig `(0, 1, false)` — polled-only, the same
//!   error-ordering model as `tuple_make_new` (a too-many-args
//!   error survives only if nothing later overwrites it:
//!   `list(1, 2)` → "object not iterable", `list("a", "b")` →
//!   "argument num/types mismatch"). Unlike tuple there is NO
//!   exact-list fast path — it always extends from the iterable.
//! - `mp_obj_list_append` has the ADR 012 self-type guard:
//!   `list.append(1, 2)` → `TypeError("argument should be a
//!   list")`. In Rust it is an exact-type check until C12
//!   provides `is_subclass_fast` (no user subclasses exist yet).
//! - `extend`: an exact-list argument takes the bulk-copy path
//!   (realloc to `len + arg.len + 4`); anything else goes
//!   through the `getiter`/`iternext` loop. Self-extend works
//!   (`l.extend(l)` doubles the list).
//! - `insert` uses `smallIntValue(idx)` directly (NOT
//!   `get_int_maybe`): `False` → 7, `True` → 15, `None` → 3,
//!   a qstr → `(q << 2) | 1`, a heap object → a huge positive
//!   (clamped to the end) — all probed on the reference binary.
//! - `remove` is `mp_seq_index_obj` then `listPopImpl` — the
//!   `ValueError` leaves the list unchanged (the pop never
//!   runs when the index raises).
//! - `sort` is a faithful quicksort port (C partition, `head =
//!   items-1, tail = items+len-1`, `binop_less_result` TRUE
//!   unless `reverse` → FALSE); `key=` needs `mp_call_function_1`
//!   (VM call machinery, C13/C14) and is a documented gap.
//! - slice load: fast indexes → a fresh list; otherwise
//!   `mp_seq_extract_slice` (which builds a LIST). Slice store:
//!   fast indexes only, otherwise a *bare*
//!   `NotImplementedError` (no message); `value == NULL`
//!   (delete) → the empty tuple; the right side must be a
//!   tuple or list (`TypeError("expected tuple/list")`).
//! - binary ADD needs an exact-list rhs (else NULL);
//!   INPLACE_ADD extends from ANY iterable and returns `lhs`;
//!   MULTIPLY clamps `n < 0` to 0 (contrast tuple: `n <= 0`
//!   → empty — the same result); comparisons need an
//!   exact-list rhs (EQUAL → FALSE, the rest → NULL).
//! - print: kind ignored, always `PRINT_REPR` elements,
//!   `"[a, b]"`. Type flags `0x0000`.
//!
//! Divergences (documented, not hidden):
//! - `sort(key=…)` returns `PyRaise` until C13/C14 lands the
//!   VM call machinery (the reference binary sorts with a key;
//!   the gap is pinned by a test).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objlist_methods::list_extend;
use crate::objects::containers::sequence::{seq_cmp_objs, seq_extract_slice};
use crate::objects::obj::{obj_get_int_maybe, ObjError};
use crate::objects::obj_ops::{get_index, obj_print_helper};
use crate::objects::slots::{BinaryOp, PrintKind, SubscrArg, UnaryOp};
use crate::objects::strings::objstr_a::{fast_slice_indexes, StrError};
use crate::objects::value::{GcRef, ListData, Obj, SliceData, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// Errors from the list operations (the C9a `DictError`
/// shape: `exc_name()` for the VM's pending-exception wiring,
/// `to_pyerror()` for the slot boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListError {
    /// `TypeError("argument num/types mismatch")`.
    ArgNumTypesMismatch,
    /// `IndexError("pop from empty list")`.
    PopEmpty,
    /// `TypeError("expected tuple/list")` — slice-store rhs.
    ExpectedTupleList,
    /// `ValueError("object not in sequence")` — `index`/`remove`
    /// miss.
    NotInSequence,
    /// `ValueError("slice step can't be zero")` — slice load
    /// with `step == 0` (the error survives; contrast tuple,
    /// where it is overwritten by the step-1 message).
    SliceStepZero,
    /// Bare `NotImplementedError` (no message) — slice store
    /// with `step != 1`.
    SliceStepBare,
    /// `TypeError("argument should be a list")` — the ADR 012
    /// self-type guard (`list.append(1, 2)`).
    SelfType,
    /// `TypeError("object not iterable")` — `list(5)` /
    /// `extend(5)`.
    NotIterable,
    /// `TypeError("unsupported types for __lt__: 'X', 'Y'")` —
    /// a sort comparison with no slot answer. The operand type
    /// names are static qstr bytes; use `message_string()` to
    /// render the message (it is not a static literal).
    CompareUnsupported {
        lhs: &'static [u8],
        rhs: &'static [u8],
    },
    /// A `get_index` failure (non-int index, out-of-range,
    /// overflow) — the `ObjError` carries the type/message.
    Obj(ObjError),
    /// A slot raised while comparing / iterating (data dropped
    /// at the boundary — the accepted C6/C7r convention).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl ListError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            ListError::ArgNumTypesMismatch => "TypeError",
            ListError::PopEmpty => "IndexError",
            ListError::ExpectedTupleList => "TypeError",
            ListError::NotInSequence => "ValueError",
            ListError::SliceStepZero => "ValueError",
            ListError::SliceStepBare => "NotImplementedError",
            ListError::SelfType => "TypeError",
            ListError::NotIterable => "TypeError",
            ListError::CompareUnsupported { .. } => "TypeError",
            ListError::Obj(e) => e.exc_type().map(|t| t.name()).unwrap_or(""),
            ListError::OutOfMemory => "MemoryError",
            ListError::PyRaise => "",
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` /
    /// `PyRaise` / `CompareUnsupported` — see `message_string`).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            ListError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            ListError::PopEmpty => Some("pop from empty list"),
            ListError::ExpectedTupleList => Some("expected tuple/list"),
            ListError::NotInSequence => Some("object not in sequence"),
            ListError::SliceStepZero => Some("slice step can't be zero"),
            ListError::SliceStepBare => Some(""),
            ListError::SelfType => Some("argument should be a list"),
            ListError::NotIterable => Some("object not iterable"),
            ListError::CompareUnsupported { .. } => None,
            ListError::Obj(e) => e.message(),
            ListError::OutOfMemory | ListError::PyRaise => None,
        }
    }

    /// The rendered message for `CompareUnsupported`
    /// (`"unsupported types for __lt__: 'str', 'int'"`); `None`
    /// for every other variant (use `message()` there).
    pub fn message_string(&self) -> Option<String> {
        match self {
            ListError::CompareUnsupported { lhs, rhs } => Some(format!(
                "unsupported types for __lt__: '{}', '{}'",
                String::from_utf8_lossy(lhs),
                String::from_utf8_lossy(rhs)
            )),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ListError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<ObjError> for ListError {
    fn from(e: ObjError) -> ListError {
        match e {
            ObjError::OutOfMemory => ListError::OutOfMemory,
            other => ListError::Obj(other),
        }
    }
}

impl From<crate::objects::containers::sequence::SeqError> for ListError {
    fn from(e: crate::objects::containers::sequence::SeqError) -> ListError {
        match e {
            crate::objects::containers::sequence::SeqError::NotInSequence => {
                ListError::NotInSequence
            }
            crate::objects::containers::sequence::SeqError::Obj(o) => ListError::from(o),
            crate::objects::containers::sequence::SeqError::PyRaise => ListError::PyRaise,
            crate::objects::containers::sequence::SeqError::OutOfMemory => {
                ListError::OutOfMemory
            }
        }
    }
}

impl From<crate::objects::containers::objdict::DictError> for ListError {
    fn from(e: crate::objects::containers::objdict::DictError) -> ListError {
        match e {
            crate::objects::containers::objdict::DictError::TypeError(
                "object not iterable",
            ) => ListError::NotIterable,
            crate::objects::containers::objdict::DictError::OutOfMemory => {
                ListError::OutOfMemory
            }
            _ => ListError::PyRaise,
        }
    }
}

/// The items of a heap list, if `value` is one.
pub fn list_items<'a>(ctx: &'a CallCtx<'_>, value: Value) -> Option<&'a [Value]> {
    let Value::Heap(r) = value else {
        return None;
    };
    match ctx.arena.get(r) {
        Obj::List(l) => Some(&l.items),
        _ => None,
    }
}

/// The items of a heap list as an owned `Vec` (empty for a
/// non-list — callers check the type first where it matters).
pub fn list_items_vec(ctx: &CallCtx, value: Value) -> Vec<Value> {
    list_items(ctx, value).unwrap_or(&[]).to_vec()
}

/// Is `value` an *exact* list of `list_type` (no subclass
/// check — C12 owns subclasses)?
pub fn is_exact_list(ctx: &CallCtx, list_type: GcRef, value: Value) -> bool {
    let Value::Heap(r) = value else {
        return false;
    };
    match ctx.arena.get(r) {
        Obj::List(_) => ctx.arena.header(r).type_ == Some(list_type),
        _ => false,
    }
}

/// `mp_obj_new_list(n, items)`: allocate a list holding `items`.
pub fn new_list(ctx: &mut CallCtx, list_type: GcRef, items: &[Value]) -> GcRef {
    ctx.alloc(Some(list_type), Obj::List(ListData { items: items.to_vec() }))
}

/// `mp_obj_list_make_new` (`objlist.zig:408-416`): sig `(0, 1,
/// false)` — polled-only, the same error-ordering model as
/// `tuple_make_new`. The list is allocated first, then extended
/// from the iterable (no exact-list fast path here).
pub fn list_make_new(
    ctx: &mut CallCtx,
    list_type: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, ListError> {
    // The polled sig check: record the error, keep going.
    let sig_err = if n_kw > 0 || args.len() > 1 {
        Some(ListError::ArgNumTypesMismatch)
    } else {
        None
    };
    let list = Value::Heap(new_list(ctx, list_type, &[]));
    if args.is_empty() {
        return match sig_err {
            Some(e) => Err(e),
            None => Ok(list),
        };
    }
    // `list_extend` runs the `getiter`/`iternext` loop; a
    // failure here OVERWRITES the recorded sig error.
    list_extend(ctx, list, args[0])?;
    if let Some(e) = sig_err {
        return Err(e);
    }
    Ok(list)
}

/// `npy_lst_unary_op` (`objlist.zig:248-253`): BOOL, LEN —
/// anything else answers NULL.
pub fn list_unary_op(
    ctx: &mut CallCtx,
    op: UnaryOp,
    self_: Value,
) -> PyResult<Value> {
    let len = list_items(ctx, self_).map(|i| i.len()).unwrap_or(0);
    match op {
        UnaryOp::Bool => Ok(Value::Bool(len != 0)),
        UnaryOp::Len => Ok(Value::SmallInt(len as i64)),
        _ => Err(PyError::PyRaise),
    }
}

/// `npy_lst_binary_op` (`objlist.zig:255-298`): ADD /
/// INPLACE_ADD / MULTIPLY / comparisons — anything else
/// answers NULL.
pub fn list_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    let list_type = types.list;
    // Clone first: the arms below allocate / extend through
    // `&mut ctx`.
    let items: Vec<Value> = list_items_vec(ctx, lhs);
    match op {
        BinaryOp::Add => {
            if !is_exact_list(ctx, list_type, rhs) {
                return Err(PyError::PyRaise);
            }
            let rhs_items = list_items_vec(ctx, rhs);
            let mut out = Vec::with_capacity(items.len() + rhs_items.len());
            out.extend_from_slice(&items);
            out.extend_from_slice(&rhs_items);
            Ok(Value::Heap(new_list(ctx, list_type, &out)))
        }
        BinaryOp::InplaceAdd => {
            list_extend(ctx, lhs, rhs).map_err(|e| e.to_pyerror())?;
            Ok(lhs)
        }
        BinaryOp::Multiply => {
            let n = match obj_get_int_maybe(ctx, types, rhs) {
                Ok(Some(n)) => n,
                _ => return Err(PyError::PyRaise),
            };
            let n = n.max(0);
            let out = match crate::objects::containers::sequence::seq_multiply(&items, n) {
                Some(out) => out,
                None => return Err(PyError::OutOfMemory),
            };
            Ok(Value::Heap(new_list(ctx, list_type, &out)))
        }
        BinaryOp::Equal
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::More
        | BinaryOp::MoreEqual => {
            if !is_exact_list(ctx, list_type, rhs) {
                if op == BinaryOp::Equal {
                    return Ok(Value::Bool(false));
                }
                return Err(PyError::PyRaise);
            }
            let rhs_items = list_items_vec(ctx, rhs);
            match seq_cmp_objs(ctx, op, &items, &rhs_items) {
                Ok(b) => Ok(Value::Bool(b)),
                Err(crate::objects::containers::sequence::SeqError::OutOfMemory) => {
                    Err(PyError::OutOfMemory)
                }
                Err(_) => Err(PyError::PyRaise),
            }
        }
        _ => Err(PyError::PyRaise),
    }
}

/// `listSliceLoad` (`objlist.zig:300-309`): fast slice
/// indexes → a fresh list; otherwise `mp_seq_extract_slice`
/// (which builds a LIST). A `step == 0` failure keeps its
/// own error (`ValueError("slice step can't be zero")`,
/// probed); a non-int bound keeps its own error too.
pub fn list_slice_load(
    ctx: &mut CallCtx,
    list_type: GcRef,
    items: &[Value],
    slice: &SliceData,
) -> Result<Value, ListError> {
    let types = ctx.types;
    let (bound, fast) = match fast_slice_indexes(ctx, types, slice, items.len() as i64) {
        Ok(v) => v,
        Err(StrError::SliceStepZero) => return Err(ListError::SliceStepZero),
        Err(StrError::Obj(o)) => return Err(ListError::from(o)),
        Err(_) => return Err(ListError::PyRaise),
    };
    if fast {
        let span = (bound.stop - bound.start).max(0) as usize;
        let start = bound.start.max(0) as usize;
        let end = (start + span).min(items.len());
        Ok(Value::Heap(new_list(ctx, list_type, &items[start..end])))
    } else {
        let out = seq_extract_slice(items, &bound);
        Ok(Value::Heap(new_list(ctx, list_type, &out)))
    }
}

/// `mp_obj_get_array` (vendor `py/obj.c`): the items of a
/// tuple or list value, else `TypeError("expected
/// tuple/list")` (probed: `l[1:2] = 5`, `"ab"`, `b"ab"`,
/// `{1:2}`, `range(2)`, `bytearray` all raise it; only
/// tuple/list work).
pub fn get_array_items(ctx: &CallCtx, value: Value) -> Result<Vec<Value>, ListError> {
    match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Tuple(items) => Ok(items.clone()),
            Obj::List(l) => Ok(l.items.clone()),
            _ => Err(ListError::ExpectedTupleList),
        },
        _ => Err(ListError::ExpectedTupleList),
    }
}

/// `listSliceStore` (`objlist.zig:311-352`): fast indexes
/// only, otherwise a *bare* `NotImplementedError` (no
/// message); `value == None` deletes (the empty tuple); the
/// right side must be a tuple or list. The grow/no-grow
/// `memmove` pair is `Vec::splice` here — the same
/// replacement semantics.
pub fn list_slice_store(
    ctx: &mut CallCtx,
    self_: Value,
    slice: &SliceData,
    value: Option<Value>,
) -> Result<Value, ListError> {
    let types = ctx.types;
    let (bound, fast) = match fast_slice_indexes(ctx, types, slice, list_len(ctx, self_)) {
        Ok(v) => v,
        Err(StrError::SliceStepZero) => return Err(ListError::SliceStepZero),
        Err(StrError::Obj(o)) => return Err(ListError::from(o)),
        Err(_) => return Err(ListError::PyRaise),
    };
    if !fast {
        return Err(ListError::SliceStepBare);
    }
    let replacement: Vec<Value> = match value {
        None => Vec::new(),
        Some(v) => get_array_items(ctx, v)?,
    };
    let start = bound.start.max(0) as usize;
    let stop = bound.stop.max(0) as usize;
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        let end = stop.min(l.items.len());
        let start = start.min(end);
        l.items.splice(start..end, replacement);
    }
    Ok(Value::None)
}

/// The length of a heap list (0 for anything else — callers
/// check the type first where it matters).
pub fn list_len(ctx: &CallCtx, value: Value) -> i64 {
    list_items(ctx, value).map(|i| i.len() as i64).unwrap_or(0)
}

/// `npyLstSubscrImpl` (`objlist.zig:354-380`): slice →
/// load/store; `value == NULL` (delete) → pop; SENTINEL
/// (get) → `mp_get_index`; else set.
pub fn list_subscr(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let types = ctx.types;
    // A slice index is a heap `Obj::Slice` (C9c's type).
    let slice_data: Option<SliceData> = match index {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Slice(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(slice) = slice_data {
        return match arg {
            SubscrArg::Get => {
                let items = list_items_vec(ctx, self_);
                let list_type = types.list;
                list_slice_load(ctx, list_type, &items, &slice)
                    .map_err(|e| e.to_pyerror())
            }
            SubscrArg::Set(v) => {
                list_slice_store(ctx, self_, &slice, Some(v)).map_err(|e| e.to_pyerror())
            }
            SubscrArg::Delete => {
                list_slice_store(ctx, self_, &slice, None).map_err(|e| e.to_pyerror())
            }
        };
    }
    match arg {
        SubscrArg::Delete => {
            crate::objects::containers::objlist_methods::list_pop_impl(ctx, self_, Some(index))
                .map_err(|e| e.to_pyerror())?;
            Ok(Value::None)
        }
        SubscrArg::Get => {
            let items = list_items_vec(ctx, self_);
            let idx = match get_index(ctx, types, items.len(), index, false) {
                Ok(i) => i,
                Err(ObjError::OutOfMemory) => return Err(PyError::OutOfMemory),
                Err(_) => return Err(PyError::PyRaise),
            };
            Ok(items[idx])
        }
        SubscrArg::Set(v) => {
            let items_len = list_len(ctx, self_);
            let idx = match get_index(ctx, types, items_len as usize, index, false) {
                Ok(i) => i,
                Err(ObjError::OutOfMemory) => return Err(PyError::OutOfMemory),
                Err(_) => return Err(PyError::PyRaise),
            };
            let Value::Heap(r) = self_ else {
                return Err(PyError::PyRaise);
            };
            if let Obj::List(l) = ctx.arena.get_mut(r) {
                l.items[idx] = v;
            }
            Ok(Value::None)
        }
    }
}

/// `mp_obj_list_getiter` (`objlist.zig:382-384`): the
/// polymorph-iter overlay `(list, cur)`.
pub fn list_getiter(ctx: &mut CallCtx, self_: Value) -> PyResult<Value> {
    Ok(Value::Heap(ctx.alloc(
        None,
        Obj::Iterator(crate::objects::value::IteratorData {
            source: self_,
            index: 0,
            extra: vec![Value::SmallInt(
                crate::objects::containers::sequence::LIST_ITER_TAG,
            )],
        }),
    )))
}

/// `npy_lst_it_iternext` (`objlist.zig:386-395`): the next
/// element, or `None` at exhaustion (C9a's
/// `dict_view_it_next` convention; C11 formalises it). The
/// cursor advances in place in the arena slot.
pub fn list_iternext(
    ctx: &mut CallCtx,
    iter: Value,
) -> Result<Option<Value>, ListError> {
    let Value::Heap(r) = iter else {
        return Err(ListError::PyRaise);
    };
    let (source, index) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.index),
        _ => return Err(ListError::PyRaise),
    };
    let items = list_items_vec(ctx, source);
    if index < items.len() {
        if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
            d.index = index + 1;
        }
        Ok(Some(items[index]))
    } else {
        Ok(None)
    }
}

/// `npy_lst_print` (`objlist.zig:148-158`): kind ignored,
/// always `PRINT_REPR` elements, `"[a, b]"`.
pub fn list_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let types = ctx.types;
    let items = list_items_vec(ctx, value);
    let _ = sink.write_str("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            let _ = sink.write_str(", ");
        }
        obj_print_helper(ctx, types, *item, PrintKind::Repr, sink);
    }
    let _ = sink.write_str("]");
}

/// `mp_type_list` (`objlist.zig:655-`): flags `0x0000`,
/// slots make_new/print/unary/binary/subscr/getiter/
/// locals_dict. `make_new` is the type-call path (C12) and
/// is not a `TypeData` slot.
pub fn list_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"list")
            .expect("list is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(
            |ctx: &mut CallCtx, op: UnaryOp, self_: Value| list_unary_op(ctx, op, self_),
        ),
        binary_op: Some(
            |ctx: &mut CallCtx, op: BinaryOp, lhs: Value, rhs: Value| {
                list_binary_op(ctx, op, lhs, rhs)
            },
        ),
        call: None,
        getiter: Some(|ctx: &mut CallCtx, self_: Value| list_getiter(ctx, self_)),
        subscr: Some(
            |ctx: &mut CallCtx, self_: Value, index: Value, arg: SubscrArg| {
                list_subscr(ctx, self_, index, arg)
            },
        ),
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn std::fmt::Write| {
                list_print(ctx, value, kind, sink)
            },
        ),
        namedtuple_fields: None,
    }
}

/// `mp_obj_list_get` (`objlist.zig` public ABI): the items
/// of a list (cloned).
pub fn list_get(ctx: &CallCtx, value: Value) -> Option<Vec<Value>> {
    match list_items(ctx, value) {
        Some(items) => Some(items.to_vec()),
        None => None,
    }
}

/// `mp_obj_list_set_len` (`objlist.zig` public ABI): set the
/// length. The only callers in the original truncate
/// (`api.zig:383` clears `sys.path`, `runtime.zig:1120`
/// trims the `*args` rest list); extending pads with
/// `Value::None` (the Zig pads with NULL words — the
/// observable difference is nil: no caller extends).
pub fn list_set_len(ctx: &mut CallCtx, value: Value, len: usize) {
    let Value::Heap(r) = value else {
        return;
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items.resize(len, Value::None);
    }
}

/// `mp_obj_list_store` (`objlist.zig` public ABI): `self[i]
/// = value` through `mp_get_index`. The Zig returns `void`
/// and silently ignores a bad index; the Rust caller gets
/// the error (C13 decides whether to ignore it, as the Zig
/// does at `main.zig:248`).
pub fn list_store(
    ctx: &mut CallCtx,
    self_: Value,
    index: Value,
    value: Value,
) -> Result<(), ListError> {
    let types = ctx.types;
    let len = list_len(ctx, self_) as usize;
    let idx = get_index(ctx, types, len, index, false)?;
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items[idx] = value;
    }
    Ok(())
}

/// `list()` as a `BuiltinFn`-shaped constructor for the
/// type-call path (C12). `n_kw` is 0 here — the keyword form
/// is C12's.
pub fn list_make_new_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let list_type = ctx.types.list;
    list_make_new(ctx, list_type, args, 0).map_err(|e| e.to_pyerror())
}
