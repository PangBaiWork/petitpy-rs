//! Iterators (Lane C11 — `src/objects/iters/` in the original).
//!
//! Ports `objpolyiter.zig`, `objmap.zig`, `objfilter.zig`,
//! `objenumerate.zig`, `objzip.zig`, `objreversed.zig`,
//! `objgetitemiter.zig`, `objnamedtuple.zig`, `objdeque.zig`
//! (1,236 lines, 0 test blocks).
//!
//! # The iterator convention (C0 `IteratorData`, formalised here)
//!
//! Every iterator is an `Obj::Iterator(IteratorData { source, index,
//! extra })` allocated with header type `None` (the overlay pattern
//! C9b/C9c established: `tuple_getiter`, `list_getiter`,
//! `range_getiter`, `dict_view_it_next`). The kind is the
//! `SmallInt` tag in `extra[0]`:
//!
//! | tag | kind | `extra` layout |
//! |----:|------|----------------|
//! | 100 | list iterator (C9b, `sequence.rs`) | `[100]` |
//! | 101 | tuple iterator (C9b) | `[101]` |
//! | 102 | array iterator (C9b) | `[102]` |
//! | 0–2 | dict view iterator (C9a) | `[kind]` (`Items=0, Keys=1, Values=2`) |
//! | 110 | map | `[110, it1, …]` |
//! | 111 | filter | `[111, iter]` |
//! | 112 | enumerate | `[112, cur]` |
//! | 113 | zip | `[113, it1, …]` |
//! | 114 | reversed | `[114]` (`index` = `cur_index`) |
//! | 115 | getitem iterator | `[115]` (`index` = next index) |
//! | 116 | deque iterator | `[116]` (`index` = ring cursor) |
//!
//! `iternext` returns `Ok(Some(v))` for the next item,
//! `Ok(None)` for `StopIteration` (the C9a `dict_view_it_next`
//! convention), `Err` for a raise.
//!
//! # The `extra.len() == 2` collision
//!
//! A range iterator stores `[stop, step]` in `extra` (no tag).
//! Three C11 iterators also have exactly two `extra` entries
//! (`map` with one sub-iterator, `filter`, `enumerate`), and a
//! range's `stop` value can numerically equal a tag (e.g.
//! `range(110, 115)` has `extra[0] == 110 == MAP_ITER_TAG`).
//! The dispatch therefore resolves a two-entry `extra` by
//! *shape*, not by tag alone:
//!
//! * `extra[1]` is an `Obj::Iterator` → map-1 / filter / zip-1
//!   (their `extra[1]` is always a sub-iterator); dispatch by tag.
//! * otherwise `extra[1]` is an int → range (source is
//!   `Obj::Range`) or enumerate (`extra[0] == 112`, source is the
//!   sub-iterator). Pinned by tests (`range(110, …)` iterates,
//!   `enumerate(range(…))` iterates, `map(range(5), …)` treats the
//!   range as the (non-callable) function).
//!
//! # Seams (C13 consumes them — same pattern as C10's local runner)
//!
//! The VM is not this lane's work, so every call this lane needs
//! the VM to make goes through a seam, implemented by the caller:
//!
//! * [`IterSeams::call`] — call a function value:
//!   `mp_call_function_n_kw(fun, n, 0, args)` (map's function,
//!   filter's predicate, `reversed`'s `__reversed__` method with
//!   the object passed as its first argument, i.e. the bound call).
//! * [`IterSeams::getitem`] — `obj.__getitem__(index)`: the Zig's
//!   `mp_call_method_n_kw` on the bound `__getitem__` method
//!   (loaded eagerly at `getiter` time) plus the
//!   `excIsStopOrIndex` classification. The seam is one operation
//!   because the bound method value cannot be produced without
//!   C12's method lookup; the seam contract is exactly the Zig's
//!   call + classification.
//!
//! [`CallOutcome`] is the seam's answer: `Value(v)`, `Stop`
//! (the callee raised `StopIteration` or `IndexError`), `Raise`
//! (anything else). Only the getitem iterator treats `Stop` as
//! end-of-iteration; map/filter propagate a callee raise as an
//! error, never as the end (the Zig does the same — its
//! `mp_call_function_n_kw` bridge raises).
//!
//! * The **intern seam** ([`InternFn`]) — `qstr_from_strn` needs
//!   `&mut RuntimeQstr`, but `CallCtx.pool` is a shared `&`. The
//!   named-tuple factory takes the interning closure as a
//!   parameter; the VM (which owns the pool mutably) supplies it.
//!   In tests the fixture pre-interns the names and the seam is a
//!   `find_strn` lookup over the (shared) pool.
//!
//! # What is NOT here
//!
//! * The `str`/`bytes` iterators: the Zig has a dedicated vendor-C
//!   str iterator (`objstr.zig:3662`); Rust routes str/bytes
//!   iteration through the getitem fallback below (same observable
//!   values, same `'iterator'` type name) until C12 wires the
//!   dedicated iterator. Recorded in the result file, not hidden.
//! * User iterators (`__next__`): `iternext` on a non-`Iterator`
//!   value raises "object not an iterator" — the `__next__`
//!   method load is C12's method machinery.
//! * The deque in `ucollections` vs `collections.deque`: the
//!   native deque ported here is `ucollections.deque`; the
//!   `collections.deque` list-backed wrapper is petitpy-lib
//!   (Phase D).

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::objects::containers::objarray_methods::array_iternext;
use crate::objects::containers::objdict_methods::dict_view_it_next;
use crate::objects::containers::objlist::list_iternext;
use crate::objects::containers::objrange::range_it_next;
use crate::objects::containers::objtuple::tuple_iternext;
use crate::objects::containers::sequence::{ARRAY_ITER_TAG, LIST_ITER_TAG, TUPLE_ITER_TAG};
use crate::objects::obj::{obj_get_type, ObjError};
use crate::objects::value::{IteratorData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;

use super::objdeque::deque_iternext;
use super::objenumerate::enumerate_iternext;
use super::objfilter::filter_iternext;
use super::objgetitemiter::{getitem_iternext, new_getitem_iter};
use super::objmap::map_iternext;
use super::objreversed::reversed_iternext;
use super::objzip::zip_iternext;

/// `MP_TYPE_FLAG_ITER_IS_ITERNEXT` (`0x0080`,
/// `mp_mirrors.zig` — the polymorph-iterator flag;
/// objpolyiter.zig sets it on the `iterator` type).
pub const FLAG_ITER_IS_ITERNEXT: u16 = 0x0080;

/// `map` iterator tag (`extra[0]`).
pub const MAP_ITER_TAG: i64 = 110;
/// `filter` iterator tag (`extra[0]`).
pub const FILTER_ITER_TAG: i64 = 111;
/// `enumerate` iterator tag (`extra[0]`).
pub const ENUMERATE_ITER_TAG: i64 = 112;
/// `zip` iterator tag (`extra[0]`).
pub const ZIP_ITER_TAG: i64 = 113;
/// `reversed` iterator tag (`extra[0]`).
pub const REVERSED_ITER_TAG: i64 = 114;
/// getitem iterator tag (`extra[0]`).
pub const GETITEM_ITER_TAG: i64 = 115;
/// deque iterator tag (`extra[0]`).
pub const DEQUE_ITER_TAG: i64 = 116;

/// `TypeError("argument num/types mismatch")` (vendor
/// `py/argcheck.c:141`, the terse sig-check message).
pub const ARG_NUM_TYPES_MISMATCH: &str = "argument num/types mismatch";
/// `TypeError("object not iterable")`.
pub const NOT_ITERABLE: &str = "object not iterable";
/// `TypeError("object not an iterator")` (runtime.zig's
/// `mp_iternext` fallback).
pub const NOT_AN_ITERATOR: &str = "object not an iterator";
/// `TypeError("can't convert to str implicitly")`
/// (`mp_obj_str_get_qstr` failure).
pub const CANT_CONVERT_STR: &str = "can't convert to str implicitly";

/// Errors raised by the iterator machinery. Messages are the
/// original's, verified on the reference binary (see the result
/// file). The `u32` payloads are qstr ids; the full message is
/// formatted by [`IterError::message`] against the pool.
#[derive(Clone, Debug, PartialEq)]
pub enum IterError {
    /// `TypeError(msg)` — sig mismatch, not iterable, not an
    /// iterator, can't-convert-to-str, …
    TypeError(&'static str),
    /// `ValueError(msg)` — e.g. deque's bare negative-maxlen
    /// error (`msg == ""`).
    ValueError(&'static str),
    /// `IndexError(msg)` — deque `pop`/`popleft` on an empty
    /// deque (`"empty"`), `append`/`appendleft` on a full
    /// overflow-checked deque (`"full"`).
    IndexError(&'static str),
    /// A wrapped [`ObjError`] (int conversion, overflow, no-len,
    /// not-subscriptable, expected-tuple/list, …) — its message
    /// and exception type carry through.
    Obj(ObjError),
    /// `TypeError("unexpected keyword argument '<name>'")` —
    /// named-tuple `make_new`, payload is the kw name's qstr id.
    UnexpectedKwArg(u32),
    /// `TypeError("function got multiple values for argument
    /// '<name>'")` — named-tuple `make_new`, payload is the
    /// argument name's qstr id.
    MultipleValues(u32),
    /// `TypeError("function takes <expected> positional arguments
    /// but <got> were given")` — named-tuple `make_new`;
    /// `got` counts positionals + keywords.
    WrongArgCount { expected: usize, got: usize },
    /// An allocation failed.
    OutOfMemory,
    /// A slot / seam raised (data dropped at the boundary — the
    /// accepted C6 convention).
    PyRaise,
}

impl IterError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            IterError::TypeError(_) => "TypeError",
            IterError::ValueError(_) => "ValueError",
            IterError::IndexError(_) => "IndexError",
            IterError::Obj(e) => e.exc_type().map_or("", |t| t.name()),
            IterError::UnexpectedKwArg(_)
            | IterError::MultipleValues(_)
            | IterError::WrongArgCount { .. } => "TypeError",
            IterError::OutOfMemory => "MemoryError",
            IterError::PyRaise => "",
        }
    }

    /// The full message text (kw-argument names are read out of
    /// the pool — the C13 raise site formats them the same way).
    pub fn message(&self, pool: &RuntimeQstr) -> String {
        match self {
            IterError::TypeError(m) | IterError::ValueError(m) | IterError::IndexError(m) => {
                (*m).to_string()
            }
            IterError::Obj(e) => e.message().unwrap_or("").to_string(),
            IterError::UnexpectedKwArg(q) => format!(
                "unexpected keyword argument '{}'",
                String::from_utf8_lossy(pool.qstr_str(*q as u16))
            ),
            IterError::MultipleValues(q) => format!(
                "function got multiple values for argument '{}'",
                String::from_utf8_lossy(pool.qstr_str(*q as u16))
            ),
            IterError::WrongArgCount { expected, got } => format!(
                "function takes {expected} positional arguments but {got} were given"
            ),
            IterError::OutOfMemory => String::new(),
            IterError::PyRaise => String::new(),
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message). The message needs the qstr pool
/// (kw-argument names are read out of it).
impl TypedRaise for IterError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, pool: &RuntimeQstr) -> Option<String> {
        Some(self.message(pool))
    }
    fn raise_signal(&self) -> PyError {
        match self {
            IterError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<ObjError> for IterError {
    fn from(e: ObjError) -> Self {
        match e {
            ObjError::OutOfMemory => IterError::OutOfMemory,
            other => IterError::Obj(other),
        }
    }
}

impl From<PyError> for IterError {
    fn from(e: PyError) -> Self {
        match e {
            PyError::OutOfMemory => IterError::OutOfMemory,
            PyError::PyRaise => IterError::PyRaise,
        }
    }
}

/// The outcome of a VM call made on behalf of iterator machinery
/// (see the module docs). `Stop` is the callee raising
/// `StopIteration` or `IndexError`; `Raise` is anything else.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome {
    Value(Value),
    Stop,
    Raise,
}

/// The VM call seams (see the module docs). Both closures take
/// the `CallCtx` first so the caller can root values across the
/// call (D6).
pub struct IterSeams<'a> {
    /// `call(ctx, fun, args)`: `mp_call_function_n_kw(fun,
    /// args.len(), 0, args)`.
    pub call: &'a mut dyn FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome,
    /// `getitem(ctx, obj, index)`: the bound `__getitem__`
    /// call plus the Stop/Index classification.
    pub getitem: &'a mut dyn FnMut(&mut CallCtx, Value, i64) -> CallOutcome,
    /// `next(ctx, it)` (E2c): advance an arbitrary iterator
    /// value — `Value(v)` = next item, `Stop` = exhausted,
    /// `Raise` = a raise (stashed on the side channel, same
    /// as `call`). `None` = no resume closure: fall through
    /// to `next_builtin` below. The C14 runtime
    /// (`runtime_iter.rs`) supplies the generator-aware
    /// resume (`resume_gen`); the ctx-only paths supply
    /// neither, so a generator sub-iterator keeps the old
    /// "object not an iterator" error there.
    pub next: Option<&'a mut dyn FnMut(&mut CallCtx, Value) -> CallOutcome>,
    /// The `next` builtin object (E2c): when `next` is `None`
    /// but this is `Some`, a non-`Iterator` sub-iterator is
    /// advanced by calling the builtin through the `call`
    /// seam. The VM (`vm_rt_iter.rs`) supplies
    /// `state.next_builtin` — the intercept routes it to the
    /// real resume, so no second `&mut VmState` borrow is
    /// needed (a `next` closure over `state` would collide
    /// with the `call` seam's borrow, E0524). A `StopIteration`
    /// from the builtin surfaces as `Raise` (the call seam
    /// stashes every raise); the caller translates it back
    /// to exhaustion (`iternext_vm_ctx` swallows it, the
    /// raising form propagates it) — the same shape as a
    /// `StopIteration` from a mapped function.
    pub next_builtin: Option<Value>,
}

/// Advance one sub-iterator of a map/filter/zip/enumerate
/// (E2c): an `Obj::Iterator` goes through the C11 dispatch;
/// anything else (a generator, a user iterator) goes through
/// the `next` closure when one is present, else through the
/// `next` builtin via the `call` seam when one is present;
/// with neither, the old `TypeError("object not an
/// iterator")` is preserved.
pub(crate) fn next_sub(
    ctx: &mut CallCtx,
    sub: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    if matches!(sub, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Iterator(_))) {
        return iternext(ctx, sub, seams);
    }
    if let Some(next) = seams.next.as_mut() {
        return match next(ctx, sub) {
            CallOutcome::Value(v) => Ok(Some(v)),
            CallOutcome::Stop => Ok(None),
            CallOutcome::Raise => Err(IterError::PyRaise),
        };
    }
    if let Some(nb) = seams.next_builtin {
        return match (seams.call)(ctx, nb, &[sub]) {
            CallOutcome::Value(v) => Ok(Some(v)),
            CallOutcome::Stop => Ok(None),
            CallOutcome::Raise => Err(IterError::PyRaise),
        };
    }
    Err(IterError::TypeError(NOT_AN_ITERATOR))
}

/// The interning seam: `qstr_from_strn` over the caller's pool.
/// The factory passes the raw bytes of a name/field; the seam
/// returns the (new or existing) qstr id.
pub type InternFn<'a> = &'a mut dyn FnMut(&[u8]) -> PyResult<u32>;

/// `mp_obj_str_get_qstr` for a name/field value: a `Qstr` is its
/// own id; a heap `Str` is interned through the seam; anything
/// else (including `Bytes` — probed: `namedtuple(b'T', …)`
/// raises) is `TypeError("can't convert to str implicitly")`.
pub fn value_to_qstr(
    ctx: &mut CallCtx,
    v: Value,
    intern: InternFn<'_>,
) -> Result<u32, IterError> {
    match v {
        Value::Qstr(id) => Ok(id),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => {
                let bytes = s.bytes.clone();
                intern(&bytes).map_err(IterError::from)
            }
            _ => Err(IterError::TypeError(CANT_CONVERT_STR)),
        },
        _ => Err(IterError::TypeError(CANT_CONVERT_STR)),
    }
}

/// Build a C11 iterator overlay: `Obj::Iterator` with header
/// type `None`, rooted across the allocation (D6).
pub fn new_iter(ctx: &mut CallCtx, data: IteratorData) -> Value {
    Value::Heap(ctx.alloc(None, Obj::Iterator(data)))
}

/// `mp_getiter` (`runtime.zig:1277-1300`, `mpGetiterImpl`):
/// (1) an iterator is its own iterator — `Obj::Iterator`, or a
/// type with the `ITER_IS_ITERNEXT` flag; (2) the type's `getiter`
/// slot, when it answers; (3) the `__getitem__` fallback — the
/// Zig loads the method, the Rust interim routes through the
/// `subscr` slot's presence (a type with a subscript slot but no
/// `__getitem__` method would diverge; no such type exists in
/// the fixture — C12 replaces this with the real method load);
/// (4) `TypeError("object not iterable")`.
///
/// A `getiter` slot answering `Err(PyRaise)` falls through to
/// (3) — the C6 convention reads it as "not implemented".
/// End-to-end str iteration through the real `subscr` slot is
/// NOT testable in this lane (the slot boundary erases the
/// Stop/Index distinction — see the result file); the logic is
/// stub-tested and C13's VM integration covers the live path.
pub fn getiter(ctx: &mut CallCtx, value: Value) -> Result<Value, IterError> {
    if let Value::Heap(r) = value {
        if let Obj::Iterator(it) = ctx.arena.get(r) {
            // Every iterator is its own iterator — EXCEPT a dict
            // VIEW, which is an `Obj::Iterator` too (no `extra`,
            // the kind — 0/1/2, `DictViewKind` in objdict_methods
            // — in `index`): it must fall through to the type's
            // getiter slot (`dict_view_getiter` → a fresh view
            // iterator), or `next(iter(d.keys()))` fails with
            // "object not an iterator" (E2). E2b: the shape test
            // alone misfires on SET iterators, which share the
            // shape (no `extra`, the cursor in `index`) — a
            // fresh set iterator fell through to the type's
            // getiter slot, but the iterator's header type is
            // `None`, so `iter(iter({1,2,3}))` raised "object
            // not iterable". Discriminate by the SOURCE type: a
            // dict source is a view (fall through); anything
            // else — a set iterator, a list/tuple iterator, …
            // — is an iterator in its own right.
            let is_dict_view = it.extra.is_empty()
                && it.index <= 2
                && matches!(it.source, Value::Heap(sr) if matches!(ctx.arena.get(sr), Obj::Dict(_)));
            if !is_dict_view {
                return Ok(value);
            }
        }
    }
    let type_ = obj_get_type(ctx.arena, ctx.types, value);
    if let Some(t) = type_ {
        // Copy the slots out first: calling one borrows `ctx`
        // mutably, which conflicts with the arena borrow.
        let (flags, getiter_fn, has_subscr) = match ctx.arena.get(t) {
            Obj::Type(d) => (d.flags, d.getiter, d.subscr.is_some()),
            _ => (0, None, false),
        };
        if flags & FLAG_ITER_IS_ITERNEXT != 0 {
            return Ok(value);
        }
        if let Some(getiter_fn) = getiter_fn {
            match getiter_fn(ctx, value) {
                Ok(v) => return Ok(v),
                Err(PyError::OutOfMemory) => return Err(IterError::OutOfMemory),
                Err(PyError::PyRaise) => {}
            }
        }
        if has_subscr {
            return Ok(new_getitem_iter(ctx, value));
        }
    }
    Err(IterError::TypeError(NOT_ITERABLE))
}

/// `getiter` through the VM hook (E2i): the ctx-only `getiter`
/// resolves an instance's `__iter__` through the STUB seams,
/// which cannot run a `FunBc` — it fails with an unrecorded
/// `PyRaise`, and `getiter` then falls through to the GETITEM
/// iterator (every user type carries a `subscr` slot), which
/// fails later with a bare `TypeError` when the class has no
/// `__getitem__` (`next(enumerate(A()))`, `stopiteration.py`
/// lines 3–5). With a hook, an instance that HAS an `__iter__`
/// member is resolved through `instance_getiter` with the hook
/// seams (the `try_instance_getiter` shape, but the slot has no
/// `&mut VmState` to drain through — a raise from `__iter__`
/// stays stashed on the hook and the slot returns the bare
/// `PyRaise`, which `call_type_ctx` drains from `pending_exc`
/// first). Without a hook, or when the instance has no
/// `__iter__` member, this is exactly `getiter`.
pub(crate) fn getiter_hook(
    ctx: &mut CallCtx,
    value: Value,
    hook: Option<crate::objects::slots::CallHook<'_, '_>>,
) -> Result<Value, IterError> {
    if let Some(h) = hook {
        if crate::runtime::vm_seams::is_instance(ctx.arena, value) {
            // The lookup only (no call) — a `FunBc` member is
            // found here; the stub seams fail only when RUNNING
            // it, which is exactly the case this hook path fixes.
            let has_iter = crate::objects::objtype::seams::with_stub_seams(|seams| {
                let Some(type_) = obj_get_type(ctx.arena, ctx.types, value) else {
                    return false;
                };
                let Some(q) = crate::objects::primitives::ops::static_qstr_id(b"__iter__")
                else {
                    return false;
                };
                crate::objects::objtype::lookup::class_lookup(
                    ctx,
                    Some(value),
                    q,
                    Some(crate::objects::objtype::lookup::SlotKind::Iter),
                    false,
                    type_,
                    seams,
                )
                .is_some()
            });
            if has_iter {
                return match crate::objects::objtype::seams::with_call_hook_seams(h, |seams| {
                    crate::objects::objtype::instance::instance_getiter(ctx, value, seams)
                }) {
                    Ok(v) => Ok(v),
                    Err(PyError::OutOfMemory) => Err(IterError::OutOfMemory),
                    Err(PyError::PyRaise) => Err(IterError::PyRaise),
                };
            }
        }
    }
    getiter(ctx, value)
}

/// `mp_iternext` (`runtime.zig:1326+`, `mpIternextImpl`):
/// the next item, or `None` at exhaustion. Dispatches on the
/// `IteratorData` tag (see the module docs for the `len == 2`
/// collision). A non-`Iterator` value raises
/// `TypeError("object not an iterator")` — user iterators with
/// `__next__` are C12's method machinery.
pub fn iternext(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Option<Value>, IterError> {
    let Value::Heap(r) = it else {
        return Err(IterError::TypeError(NOT_AN_ITERATOR));
    };
    let (source, extra) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.extra.clone()),
        _ => return Err(IterError::TypeError(NOT_AN_ITERATOR)),
    };
    if extra.is_empty() {
        // A dict VIEW (kind in `index`), not an iterator —
        // `as_dict_view`'s shape. The VM would load `__next__`
        // and fail; same error here. E2b: a SET iterator shares
        // this shape (no `extra`, the cursor in `index`) — its
        // SOURCE is a Set, so discriminate on it and step it
        // with `set_it_next` instead of failing.
        if matches!(source, Value::Heap(sr) if matches!(ctx.arena.get(sr), Obj::Set(_))) {
            return crate::objects::containers::objset::set_it_next(ctx, it).map_err(|e| {
                let _ = ctx.record_typed(e);
                IterError::PyRaise
            });
        }
        return Err(IterError::TypeError(NOT_AN_ITERATOR));
    }
    if extra.len() == 2 {
        // Range `[stop, step]` vs map-1 / filter / enumerate /
        // zip-1 `[TAG, x]` — resolved by shape (module docs).
        // E2c: `x` is a sub-iterator — an `Obj::Iterator` OR
        // an `Obj::Generator` (a map/filter/zip over a
        // generator stores the generator itself in `extra[1]`;
        // only recognising `Iterator` made `next(map(f, gen))`
        // raise "object not an iterator"). A range's `extra[1]`
        // is a `SmallInt` and enumerate's is its counter, so
        // neither is affected. E2i: OR an `Obj::Instance` — a
        // user iterator's `__iter__` returns the instance
        // itself (`next(map(f, A()))`, `stopiteration.py`),
        // which `getiter_hook` now stores verbatim; a range's
        // `extra[1]` is still a `SmallInt`, never an instance,
        // so the range collision is unaffected.
        let second_is_iter = matches!(extra[1], Value::Heap(rr)
            if matches!(ctx.arena.get(rr), Obj::Iterator(_) | Obj::Generator(_) | Obj::Instance(_)));
        if second_is_iter {
            return match extra[0] {
                Value::SmallInt(MAP_ITER_TAG) => map_iternext(ctx, it, seams),
                Value::SmallInt(FILTER_ITER_TAG) => filter_iternext(ctx, it, seams),
                Value::SmallInt(ZIP_ITER_TAG) => zip_iternext(ctx, it, seams),
                _ => Err(IterError::TypeError(NOT_AN_ITERATOR)),
            };
        }
        if matches!(source, Value::Heap(sr) if matches!(ctx.arena.get(sr), Obj::Range(_))) {
            return range_it_next(ctx, it).map_err(|_| IterError::PyRaise);
        }
        if extra[0] == Value::SmallInt(ENUMERATE_ITER_TAG) {
            return enumerate_iternext(ctx, it, seams);
        }
        return Err(IterError::TypeError(NOT_AN_ITERATOR));
    }
    match extra[0] {
        Value::SmallInt(TUPLE_ITER_TAG) => tuple_iternext(ctx, it).map_err(|_| IterError::PyRaise),
        Value::SmallInt(LIST_ITER_TAG) => list_iternext(ctx, it).map_err(|_| IterError::PyRaise),
        Value::SmallInt(ARRAY_ITER_TAG) => array_iternext(ctx, it).map_err(|_| IterError::PyRaise),
        Value::SmallInt(0) | Value::SmallInt(1) | Value::SmallInt(2) => {
            dict_view_it_next(ctx, it).map_err(|_| IterError::PyRaise)
        }
        Value::SmallInt(MAP_ITER_TAG) => map_iternext(ctx, it, seams),
        Value::SmallInt(FILTER_ITER_TAG) => filter_iternext(ctx, it, seams),
        Value::SmallInt(ZIP_ITER_TAG) => zip_iternext(ctx, it, seams),
        Value::SmallInt(REVERSED_ITER_TAG) => reversed_iternext(ctx, it, seams),
        Value::SmallInt(GETITEM_ITER_TAG) => getitem_iternext(ctx, it, seams),
        Value::SmallInt(DEQUE_ITER_TAG) => deque_iternext(ctx, it),
        _ => Err(IterError::TypeError(NOT_AN_ITERATOR)),
    }
}
