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
use crate::objects::iters::iter::{getiter, iternext, CallOutcome, IterError, IterSeams};
use crate::objects::obj::ObjError;
use crate::objects::obj_ops::obj_subscr;
use crate::objects::objtype::call_via_type_slot;
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
/// type call), and raises through the stub otherwise. A type with
/// a `getiter` slot but no direct arm here still raises
/// `TypeError("object not iterable")` (the `__iter__` method load
/// is C12/C14's).
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
    /// `iternext` protocol with stub seams (see the `fn` docs).
    Protocol(Value),
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
            let it = getiter(ctx, it).map_err(|e| iter_err_to_dict(ctx, e))?;
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
            let mut hook = call.as_deref_mut();
            let mut call_seam =
                |ctx2: &mut CallCtx, fun: Value, args: &[Value]| -> CallOutcome {
                    if let Some(h) = hook.as_deref_mut() {
                        return match h(ctx2, fun, args.len(), 0, args) {
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
            let mut seams = IterSeams {
                call: &mut call_seam,
                getitem: &mut getitem_seam,
            };
            let mut out = Vec::new();
            loop {
                match iternext(ctx, it, &mut seams) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => break,
                    Err(e) => return Err(iter_err_to_dict(ctx, e)),
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
