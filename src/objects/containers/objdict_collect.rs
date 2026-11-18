//! Iterable materialisation (C13b-split from `objdict_methods.rs`).
//!
//! `collect_iterable` — the `mp_getiter` / `mp_iternext` loop
//! shared by `dict` construction / `update` / `fromkeys`, `list`
//! / `tuple` / `set` construction, and the deque lane — plus the
//! dict-view stepping it is built on (`view_next_entry`). Split
//! out of `objdict_methods.rs` for the 800-line cap (that file was
//! already 810 lines at the C13b baseline; this file holds the
//! whole iteration section). `objdict_methods.rs` re-exports
//! `collect_iterable`, so the call sites keep their paths.

use crate::memory::gc::CallCtx;
use crate::objects::containers::map::{map_of, set_items};
use crate::objects::iters::iter::{getiter, next_sub, CallOutcome, IterError, IterSeams};
use crate::objects::obj::ObjError;
use crate::objects::obj_ops::obj_subscr;
use crate::objects::objtype::call_via_type_slot;
use crate::objects::objtype::seams::real_load_maybe;
use crate::objects::slots::{CallHook, SubscrArg};
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::PyError;
use super::objdict::{dict_ref_of, DictError, NOT_ITERABLE};
use super::objdict_methods::{as_dict_view, as_dict_view_it, DictViewKind};

// ── iteration helper ────────────────────────────────────────────────


/// The next value a dict view iterator yields (`npy_dt_view_it_iternext`
/// without the iterator object itself): the live entry at `*cur`
/// (advancing it past the entry), as a key / value / `(key, value)`
/// tuple by `kind`. `None` on exhaustion.
pub(crate) fn view_next_entry(
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
/// concrete types available today — see the module docs). A
/// non-view `Obj::Iterator` (C11's map / filter / zip / enumerate
/// / range / list / tuple / reversed / getitem iterators) drains
/// through the general protocol (`IterPlan::Protocol`): the call
/// seam routes through `call_via_type_slot`, so builtin functions
/// and builtin types (`map(str, …)`) work; a user function
/// (`FunBc`) runs only when the caller passes the VM's call hook
/// (`Some`, E2 — the `MakeNewFn` hook threaded down from the
/// type call), and raises through the stub otherwise. A generator
/// drains through the hook's `resume` operation (E2b — the
/// `VmHook` pair: `list(gen())` / `sorted(gen())` / `dict(gen)`
/// resume it step by step; the ctx-only path, with no hook,
/// keeps `TypeError("object not iterable")`). A user instance
/// drains through the Protocol arm too (E2f — its `__iter__` /
/// `__next__` run through the hook there). A type with a
/// getiter slot but no direct arm here still raises
/// `TypeError("object not iterable")`.
/// What `collect_iterable` does, decided from an immutable borrow
/// first so the allocations below do not borrow the arena.
/// Shared with the set lane (C9c: `set` construction / `update` /
/// `difference` / `intersection` iterate the same way).
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
    /// A C11 iterator to drain through the general `getiter` /
    /// `iternext` protocol with stub seams (see the `fn` docs) —
    /// or a user instance (E2f: its `__iter__` / `__next__` run
    /// through the hook's `call` / `next` in the arm below).
    Protocol(Value),
    /// A generator to drain through the hook's `resume`
    /// operation (E2b — the `VmHook` pair): `Some` hook resumes
    /// it step by step; `None` (the ctx-only path) keeps the
    /// old `TypeError("object not iterable")`.
    Generator(Value),
}

pub(crate) fn collect_iterable(
    ctx: &mut CallCtx,
    value: Value,
    mut call: Option<CallHook<'_, '_>>,
) -> Result<Vec<Value>, DictError> {
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
            Obj::Set(s) => IterPlan::Elems(set_items(&s.table)),
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
                    // A C11 iterator — drain through the general
                    // protocol (see the `fn` docs).
                    None => IterPlan::Protocol(value),
                },
            },
            // A generator (E2b): drain through the hook's
            // `resume` — the `Generator` arm below. Without a
            // hook (the ctx-only path) there is no VM to resume
            // it, so it stays not-iterable there.
            Obj::Generator(_) => IterPlan::Generator(value),
            // A user instance (E2f): `__iter__` / `__next__`
            // run through the hook in the Protocol arm below —
            // without this arm `list(myiter(5))` was
            // `TypeError("object not iterable")` (iter1/iter2).
            Obj::Instance(_) => IterPlan::Protocol(value),
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
        IterPlan::Protocol(it) => {
            // NOTE: `getiter` runs AFTER the seams are built
            // (E2f — a user instance's `__iter__` needs the call
            // seam; see below).
            // The call seam (E2): `Some` hook → the VM's real
            // call hook (a `FunBc` callee actually executes, so
            // `list(map(f, xs))` works); `None` → the builtin-live
            // `call_via_type_slot` (builtins and builtin types
            // only). A `StopIteration` raised by the called
            // function is exhaustion (the Zig's `mp_iternext`
            // classification) — drain the recorded raise and stop.
            // NOTE: a `StopIteration` raised by a `FunBc` through
            // the hook is NOT classified here — the exception
            // object is stashed on `state.pending_exc`, which this
            // ctx-only fn cannot see, so it surfaces as a raise.
            //
            // E2c: ONE hook serves both the `call` seam and the
            // `next` seam below, so it is shared through a
            // `RefCell` — two closures over the same `&mut`
            // hook cannot be built (E0524, the E2b `VmHook`
            // rationale). The two seams are only ever called
            // sequentially by C11's `iternext` (pull subs, then
            // call the function); a re-entrant borrow of the
            // same cell cannot happen — a nested collection
            // builds its own hook and cell — so `borrow_mut`
            // never panics here.
            use std::cell::RefCell;
            let hook_cell = RefCell::new(call.as_deref_mut());
            let mut call_seam =
                |ctx2: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
                    let mut guard = hook_cell.borrow_mut();
                    if let Some(h) = guard.as_deref_mut() {
                        return match h.call(ctx2, fun, args.len(), 0, args) {
                            Ok(v) => CallOutcome::Value(v),
                            Err(PyError::OutOfMemory) => CallOutcome::Raise,
                            Err(PyError::PyRaise) => CallOutcome::Raise,
                        };
                    }
                    match call_via_type_slot(ctx2, fun, args.len(), 0, args) {
                        Ok(v) => CallOutcome::Value(v),
                        Err(PyError::OutOfMemory) => CallOutcome::Raise,
                        Err(PyError::PyRaise) => {
                            let stop = matches!(
                                ctx2.pending_raise.as_ref(),
                                Some(p) if p.exc_name == "StopIteration"
                            );
                            if stop {
                                ctx2.pending_raise.take();
                                CallOutcome::Stop
                            } else {
                                CallOutcome::Raise
                            }
                        }
                    }
                };
            let mut getitem_seam =
                |ctx2: &mut CallCtx, obj: Value, index: i64| -> CallOutcome {
                    match obj_subscr(ctx2, ctx2.types, obj, Value::SmallInt(index), SubscrArg::Get)
                    {
                        Ok(v) => CallOutcome::Value(v),
                        Err(ObjError::IndexOutOfRange) => CallOutcome::Stop,
                        Err(e) => {
                            // E2: the slot collapses its error into a
                            // recorded raise (`call_subscr` turns a
                            // slot `PyRaise` into `NotImplemented`,
                            // so `obj_subscr` never returns
                            // `IndexOutOfRange` here — e.g. str
                            // subscript at OOB records an IndexError)
                            // — classify on the recorded raise
                            // instead, the original's rule
                            // (`objgetitemiter.zig:excIsStopOrIndex`,
                            // mirrored in `vm_rt_iter.rs`): a
                            // `StopIteration` or `IndexError` from
                            // `__getitem__` IS exhaustion; anything
                            // else propagates. Without this,
                            // `list(enumerate('ab'))` raised
                            // `IndexError` instead of stopping.
                            let stop = matches!(
                                ctx2.pending_raise.as_ref(),
                                Some(p) if p.exc_name == "IndexError"
                                    || p.exc_name == "StopIteration"
                            );
                            if stop {
                                ctx2.pending_raise.take();
                                CallOutcome::Stop
                            } else {
                                let _ = ctx2.record_typed(e);
                                CallOutcome::Raise
                            }
                        }
                    }
                };
            // The `next` seam (E2c): resume a generator
            // sub-iterator through the hook — the `iternext`
            // form, so exhaustion is `Ok(None)` (a `Stop`) and a
            // raise inside the generator is a `Raise` (never
            // confused with the mapped function's own
            // `StopIteration`, which the call seam above correctly
            // surfaces as a raise — probed on the reference).
            // E2f: the seam is the hook's `next`, not `resume` —
            // `next` steps any iterator (a user instance's
            // `__next__` via `iternext_vm_ctx`, with a
            // `StopIteration` — including a user subclass — as
            // exhaustion), while `resume` only knows generators.
            // Without a hook there is no VM to step with, so it
            // records the old "object not an iterator" raise
            // itself and returns `Raise` — the caller drains the
            // recorded raise, so the ctx-only error is unchanged.
            let mut next_fn = |ctx2: &mut CallCtx, sub: Value| -> CallOutcome {
                let mut guard = hook_cell.borrow_mut();
                match guard.as_deref_mut() {
                    Some(h) => match h.next(ctx2, sub) {
                        Ok(Some(v)) => CallOutcome::Value(v),
                        Ok(None) => CallOutcome::Stop,
                        Err(PyError::OutOfMemory) => CallOutcome::Raise,
                        Err(PyError::PyRaise) => CallOutcome::Raise,
                    },
                    None => {
                        let _ = ctx2.record_raise("TypeError", Some("object not iterable"));
                        CallOutcome::Raise
                    }
                }
            };
            let mut seams = IterSeams {
                call: &mut call_seam,
                getitem: &mut getitem_seam,
                next: Some(&mut next_fn),
                next_builtin: None,
            };
            // E2f: `getiter` here — a user instance's `__iter__`
            // is a FunBc and only runs through the call seam (the
            // slot path, `instance_getiter_slot`, has stub seams).
            // Load + call it via the seam; no `__iter__` member
            // → the plain `getiter` (getitem fallback, native-base
            // path). A raise from `__iter__` propagates — both
            // `Stop` and `Raise` map to `PyRaise` (the Zig's
            // `mp_call_method` failure inside
            // `mp_obj_instance_getiter` is a raise, not
            // exhaustion).
            let it = if matches!(it, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Instance(_)))
            {
                let iter_q = ctx.pool.find_strn(b"__iter__");
                match (iter_q != 0)
                    .then(|| real_load_maybe(ctx, it, iter_q as u32))
                    .flatten()
                {
                    Some((fun, bound)) => {
                        let args: Vec<Value> = match bound {
                            Some(s) => vec![s],
                            None => vec![],
                        };
                        match (seams.call)(ctx, fun, &args) {
                            CallOutcome::Value(v) => v,
                            _ => {
                                return Err(iter_err_to_dict(ctx, IterError::PyRaise));
                            }
                        }
                    }
                    None => getiter(ctx, it).map_err(|e| iter_err_to_dict(ctx, e))?,
                }
            } else {
                getiter(ctx, it).map_err(|e| iter_err_to_dict(ctx, e))?
            };
            let mut out = Vec::new();
            loop {
                // `next_sub`, not `iternext`: a user iterator is
                // its own `it` (an `Obj::Instance`, not an
                // `Obj::Iterator`) and steps through the `next`
                // seam; a C11 iterator takes the same `iternext`
                // path as before (`next_sub` dispatches to it).
                match next_sub(ctx, it, &mut seams) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => break,
                    Err(e) => return Err(iter_err_to_dict(ctx, e)),
                }
            }
            Ok(out)
        }
        IterPlan::Generator(gen) => {
            // E2b: drain the generator through the hook's
            // `resume` (the `iternext` form — `Ok(Some(v))` =
            // yielded, `Ok(None)` = exhausted). A raise inside
            // the generator is `Err(PyRaise)` with the exception
            // object stashed on the VM side channel (the
            // `StateHook` contract) or recorded in
            // `pending_raise` (the `SeamHook` contract) — either
            // way the caller drains it from `DictError::PyRaise`.
            // Without a hook there is no VM to resume it: the
            // old `TypeError("object not iterable")`.
            let Some(hook) = call.as_deref_mut() else {
                return Err(DictError::TypeError(NOT_ITERABLE));
            };
            let mut out = Vec::new();
            loop {
                match hook.resume(ctx, gen) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => break,
                    Err(_) => return Err(DictError::PyRaise),
                }
            }
            Ok(out)
        }
    }
}

/// Map an iterator-protocol error onto a `DictError`. Typed
/// errors carry their message; anything else (including a slot
/// raise, whose exception object is already recorded in the
/// context) is the bare `PyRaise`.
fn iter_err_to_dict(ctx: &mut CallCtx, e: IterError) -> DictError {
    match e {
        IterError::TypeError(m) => DictError::TypeError(m),
        IterError::ValueError(m) => DictError::ValueError(m),
        IterError::OutOfMemory => DictError::OutOfMemory,
        IterError::PyRaise => DictError::PyRaise,
        other => {
            let _ = ctx.record_typed(other);
            DictError::PyRaise
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
