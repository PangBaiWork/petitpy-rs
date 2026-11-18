//! `objlist.zig` methods — `append` … `sort` (the second half
//! of the list lane; `objlist.rs` holds the slots). Split so
//! neither file exceeds the 800-line cap.
//!
//! Faithful points (see `objlist.rs` for the slot-level notes):
//! - Only `append` carries the ADR 012 self-type guard
//!   (`TypeError("argument should be a list")`); the other
//!   methods read the list payload directly (the Zig does the
//!   same — the guard is only on `mp_obj_list_append`).
//! - `extend` bulk-copies for an exact-list argument (the
//!   snapshot is cloned first, so self-extend works);
//!   anything else iterates via `collect_iterable`.
//! - `insert` uses the raw REPR_A word (`smallIntValue`):
//!   `False` → 7, `True` → 15, `None` → 3, a qstr → `(q <<
//!   2) | 1`, a heap object → `i64::MAX` (clamped to the end).
//! - `remove` raises before popping (the list is unchanged
//!   on a miss); `pop` shrinks are unobservable in Rust.
//! - `sort` is the faithful quicksort (see `objlist.rs`);
//!   `key=` is a documented gap (needs the VM call machinery,
//!   C13/C14) and returns `PyRaise` for now.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict_methods::collect_iterable;
use crate::objects::containers::objlist::{is_exact_list, list_items_vec, list_len, ListError};
use crate::objects::containers::sequence::{seq_count_obj, seq_index_obj};
use crate::objects::obj::obj_is_true;
use crate::objects::obj_ops::{get_index, obj_binary_op, obj_get_type_str};
use crate::objects::slots::BinaryOp;
use crate::objects::slots::CallHook;
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};

/// `mirrors.smallIntValue(idx)` — the raw REPR_A word read,
/// NOT `get_int_maybe` (probed: `insert(False, x)` lands at
/// index 7, `insert(True, x)` at 15, `insert(None, x)` at 3,
/// `insert("a", x)` appends, `insert(2**70, x)` appends).
/// A heap object reads its pointer word — a huge positive —
/// so `i64::MAX` (the clamp below sends it to the end).
pub fn insert_index_word(value: Value) -> i64 {
    match value {
        Value::SmallInt(n) => n,
        Value::Bool(true) => 15,
        Value::Bool(false) => 7,
        Value::None => 3,
        Value::Qstr(id) => ((id as i64) << 2) | 1,
        Value::Heap(_) => i64::MAX,
    }
}

/// `mp_obj_list_append` (`objlist.zig:178-197`): the ADR 012
/// self-type guard, then `listAppendInline`.
pub fn list_append(ctx: &mut CallCtx, self_: Value, arg: Value) -> Result<Value, ListError> {
    let types = ctx.types;
    if !is_exact_list(ctx, types.list, self_) {
        return Err(ListError::SelfType);
    }
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items.push(arg);
    }
    Ok(Value::None)
}

/// `npy_lst_extend` (`objlist.zig:202-226`): an exact-list
/// argument bulk-copies (realloc to `len + arg.len + 4` in
/// the Zig — `Vec` manages capacity here); anything else
/// goes through the `getiter`/`iternext` loop.
pub fn list_extend(
    ctx: &mut CallCtx,
    self_: Value,
    arg: Value,
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, ListError> {
    let types = ctx.types;
    if is_exact_list(ctx, types.list, arg) {
        // Clone first: `self.extend(self)` must see the old
        // items (the Zig copies from the same buffer after the
        // realloc; a snapshot is the same observable).
        let arg_items = list_items_vec(ctx, arg);
        let Value::Heap(r) = self_ else {
            return Err(ListError::PyRaise);
        };
        if let Obj::List(l) = ctx.arena.get_mut(r) {
            l.items.extend_from_slice(&arg_items);
        }
    } else {
        let items = collect_iterable(ctx, arg, call)?;
        let Value::Heap(r) = self_ else {
            return Err(ListError::PyRaise);
        };
        if let Obj::List(l) = ctx.arena.get_mut(r) {
            l.items.extend_from_slice(&items);
        }
    }
    Ok(Value::None)
}

/// `npy_lst_clear` (`objlist.zig:419-426`): `len = 0`
/// (the realloc to 4 is unobservable).
pub fn list_clear(ctx: &mut CallCtx, self_: Value) -> Result<Value, ListError> {
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items.clear();
    }
    Ok(Value::None)
}

/// `npy_lst_copy` (`objlist.zig:428-432`): a new list with
/// the same items.
pub fn list_copy(ctx: &mut CallCtx, self_: Value) -> Result<Value, ListError> {
    let types = ctx.types;
    let items = list_items_vec(ctx, self_);
    Ok(Value::Heap(crate::objects::containers::objlist::new_list(
        ctx, types.list, &items,
    )))
}

/// `npy_lst_count` (`objlist.zig:434-437`).
pub fn list_count(ctx: &mut CallCtx, self_: Value, value: Value) -> Result<Value, ListError> {
    let items = list_items_vec(ctx, self_);
    Ok(Value::SmallInt(seq_count_obj(ctx, &items, value)? as i64))
}

/// `npy_lst_index` (`objlist.zig:439-442`).
pub fn list_index(
    ctx: &mut CallCtx,
    self_: Value,
    value: Value,
    start: Option<Value>,
    stop: Option<Value>,
) -> Result<Value, ListError> {
    let items = list_items_vec(ctx, self_);
    Ok(Value::SmallInt(
        seq_index_obj(ctx, &items, value, start, stop)? as i64,
    ))
}

/// `npy_lst_insert` (`objlist.zig:444-458`): the raw-word
/// index, negative wrap, clamp to `[0, len]`; append `None`
/// then shift right and set (the Zig's `listAppendInline`
/// + memmove).
pub fn list_insert(
    ctx: &mut CallCtx,
    self_: Value,
    idx: Value,
    obj: Value,
) -> Result<Value, ListError> {
    let len = list_len(ctx, self_);
    let mut index = insert_index_word(idx);
    if index < 0 {
        index += len;
    }
    if index < 0 {
        index = 0;
    }
    if index > len {
        index = len;
    }
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        let index = index as usize;
        let old_len = l.items.len();
        l.items.push(Value::None);
        for i in (index + 1..=old_len).rev() {
            l.items[i] = l.items[i - 1];
        }
        l.items[index] = obj;
    }
    Ok(Value::None)
}

/// `listPopImpl` (`objlist.zig:228-247`): empty →
/// `IndexError("pop from empty list")`; the index resolves
/// through `mp_get_index` (default `-1`); memmove + shrink
/// (the shrink is unobservable in Rust).
pub fn list_pop_impl(
    ctx: &mut CallCtx,
    self_: Value,
    index_arg: Option<Value>,
) -> Result<Value, ListError> {
    let types = ctx.types;
    let len = list_len(ctx, self_) as usize;
    if len == 0 {
        return Err(ListError::PopEmpty);
    }
    let idx_arg = index_arg.unwrap_or(Value::SmallInt(-1));
    let idx = get_index(ctx, types, len, idx_arg, false)?;
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    match ctx.arena.get_mut(r) {
        Obj::List(l) => Ok(l.items.remove(idx)),
        _ => Err(ListError::PyRaise),
    }
}

/// `npy_lst_pop` (`objlist.zig:248-250`).
pub fn list_pop(ctx: &mut CallCtx, self_: Value, index_arg: Option<Value>) -> Result<Value, ListError> {
    list_pop_impl(ctx, self_, index_arg)
}

/// `mp_obj_list_remove` (`objlist.zig:460-467`):
/// `mp_seq_index_obj` then `listPopImpl` — the `ValueError`
/// leaves the list unchanged.
pub fn list_remove(ctx: &mut CallCtx, self_: Value, value: Value) -> Result<Value, ListError> {
    let items = list_items_vec(ctx, self_);
    let idx = seq_index_obj(ctx, &items, value, None, None)?;
    list_pop_impl(ctx, self_, Some(Value::SmallInt(idx as i64)))?;
    Ok(Value::None)
}

/// `npy_lst_reverse` (`objlist.zig:469-479`): swap halves.
pub fn list_reverse(ctx: &mut CallCtx, self_: Value) -> Result<Value, ListError> {
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items.reverse();
    }
    Ok(Value::None)
}

/// The `mp_binary_op(LESS, a, b) == binop_less_result` test
/// inside the quicksort: the result must be exactly
/// `Bool(!reverse)` — a truthy non-bool does NOT count (the
/// Zig compares object identity with `OBJ_TRUE`/`OBJ_FALSE`).
///
/// Comparison failure is NOT an error here — it is the
/// quicksort's polled-sentinel model (`objlist.zig:98` and
/// `runtime.zig:794`): `mp_binary_op` returns `OBJ_NULL` and
/// sets `pending_exc`, which the quicksort never polls. So a
/// failed comparison counts as "not less" (`NULL != TRUE`),
/// the sort runs to COMPLETION, and the caller raises the
/// pending error afterwards. The FIRST failure wins
/// (probed: `[3, 1, "a", 2].sort()` raises `'int', 'str'` —
/// the first failure — even though a later comparison fails
/// as `'str', 'int'`): `pending` records only the first
/// failure and `sort_less` returns `false` for every
/// failure. `list_sort` returns the recorded error after
/// the sort finishes, with the list left partially sorted
/// (probed: `[3, "a", 1, 2]` → `[1, 2, 'a', 3]`).
fn sort_less(
    ctx: &mut CallCtx,
    a: Value,
    b: Value,
    reverse: bool,
    pending: &mut Option<ListError>,
) -> bool {
    let types = ctx.types;
    let target = Value::Bool(!reverse);
    match obj_binary_op(ctx, types, BinaryOp::Less, a, b) {
        Ok(v) => v == target,
        Err(e) => {
            if pending.is_none() {
                *pending = Some(match e {
                    PyError::OutOfMemory => ListError::OutOfMemory,
                    PyError::PyRaise => ListError::CompareUnsupported {
                        lhs: obj_get_type_str(ctx.arena, a),
                        rhs: obj_get_type_str(ctx.arena, b),
                    },
                });
            }
            false
        }
    }
}

/// One element read from the list under sort (short borrow —
/// the comparisons need `&mut ctx`, so no borrow of the
/// items may be held across a `sort_less` call).
fn list_get_at(ctx: &CallCtx, r: GcRef, i: isize) -> Value {
    match ctx.arena.get(r) {
        Obj::List(l) => l.items[i as usize],
        _ => Value::None,
    }
}

/// One swap inside the list under sort (short borrow).
fn list_swap_at(ctx: &mut CallCtx, r: GcRef, i: isize, j: isize) {
    if let Obj::List(l) = ctx.arena.get_mut(r) {
        l.items.swap(i as usize, j as usize);
    }
}

/// `mpQuicksort` (`objlist.zig:482-512`): the faithful C
/// partition, IN PLACE on the arena. `head`/`tail` are the
/// Zig's pointers as element indices: the range sorted is
/// `(head, tail]` (`head = lo - 1`, `tail = hi`), so the
/// loop condition is `tail - head > 1` and the recursive
/// calls are `mpQuicksort(head, t)` / `mpQuicksort(h,
/// tail)` — the smaller side recursed, the larger iterated,
/// exactly as the Zig's pointer arithmetic does. Sorting
/// in place is REQUIRED, not an optimisation: the reference
/// leaves a partially-sorted list behind when a comparison
/// fails (see `sort_less`), which clone-sort-writeback
/// would lose.
fn quicksort_range(
    ctx: &mut CallCtx,
    r: GcRef,
    mut head: isize,
    mut tail: isize,
    reverse: bool,
    pending: &mut Option<ListError>,
) {
    while tail - head > 1 {
        let mut h = head;
        let mut t = tail;
        let v = list_get_at(ctx, r, tail);
        loop {
            loop {
                h += 1;
                if !(h < t && sort_less(ctx, list_get_at(ctx, r, h), v, reverse, pending)) {
                    break;
                }
            }
            loop {
                t -= 1;
                if !(h < t && sort_less(ctx, v, list_get_at(ctx, r, t), reverse, pending)) {
                    break;
                }
            }
            if h >= t {
                break;
            }
            list_swap_at(ctx, r, h, t);
        }
        list_swap_at(ctx, r, h, tail);
        if t - head < tail - h {
            quicksort_range(ctx, r, head, t, reverse, pending);
            head = h;
        } else {
            quicksort_range(ctx, r, h, tail, reverse, pending);
            tail = t;
        }
    }
}

/// `mp_obj_list_sort` (`objlist.zig:522-544`): `key` (KW_ONLY,
/// default `None`), `reverse` (KW_ONLY bool, default
/// `false`); `len <= 1` is a no-op (the while condition in
/// `quicksort_range` fails). The sort runs IN PLACE (see
/// `quicksort_range`); if a comparison failed, the FIRST
/// recorded failure is returned AFTER the sort completes —
/// the list is left partially sorted, exactly as the
/// reference does (probed: `[3, "a", 1, 2].sort()` raises
/// `TypeError("unsupported types for __lt__: 'str',
/// 'int'")` and leaves `[1, 2, 'a', 3]`).
/// A non-`None` key needs `mp_call_function_1` (C13/C14) —
/// it returns `PyRaise` for now (documented gap; the
/// reference binary sorts with a key — and ABORTS, rc=134,
/// when the key call itself fails, e.g. `sort(key=len)` on
/// ints: the NULL key result is passed to the comparison,
/// `fromPtr(NULL)` panics at `mp_mirrors.zig:745`).
pub fn list_sort(
    ctx: &mut CallCtx,
    self_: Value,
    key: Option<Value>,
    reverse: bool,
) -> Result<Value, ListError> {
    if let Some(k) = key {
        if k != Value::None {
            return Err(ListError::PyRaise);
        }
    }
    let Value::Heap(r) = self_ else {
        return Err(ListError::PyRaise);
    };
    let n = match ctx.arena.get(r) {
        Obj::List(l) => l.items.len() as isize,
        _ => return Err(ListError::PyRaise),
    };
    let mut pending: Option<ListError> = None;
    if n > 1 {
        quicksort_range(ctx, r, -1, n - 1, reverse, &mut pending);
    }
    if let Some(e) = pending {
        return Err(e);
    }
    Ok(Value::None)
}

/// Parse the sort kwargs (`key` / `reverse`, KW_ONLY) from
/// `KwArg`s. Any unknown name, duplicate, or positional
/// beyond `self` is `ArgNumTypesMismatch` (probed:
/// `sort(foo=1)`, `sort(1, 2, 3)`).
pub fn parse_sort_args(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[KwArg],
) -> Result<(Option<Value>, bool), ListError> {
    if args.len() != 1 {
        return Err(ListError::ArgNumTypesMismatch);
    }
    let key_q = crate::objects::primitives::ops::static_qstr_id(b"key");
    let rev_q = crate::objects::primitives::ops::static_qstr_id(b"reverse");
    let mut key: Option<Value> = None;
    let mut reverse = false;
    let mut seen_key = false;
    let mut seen_rev = false;
    for kw in kwargs {
        if Some(kw.name) == key_q {
            if seen_key {
                return Err(ListError::ArgNumTypesMismatch);
            }
            seen_key = true;
            key = Some(kw.value);
        } else if Some(kw.name) == rev_q {
            if seen_rev {
                return Err(ListError::ArgNumTypesMismatch);
            }
            seen_rev = true;
            reverse = obj_is_true(ctx, ctx.types, kw.value)
                .map_err(|_| ListError::PyRaise)?;
        } else {
            return Err(ListError::ArgNumTypesMismatch);
        }
    }
    Ok((key, reverse))
}

/// `list.sort` as the method-table entry: `args[0]` is
/// `self`, the rest is parsed by `parse_sort_args`.
pub fn list_sort_builtin(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[KwArg],
) -> PyResult<Value> {
    if args.is_empty() {
        return Err(PyError::PyRaise);
    }
    let (key, reverse) = parse_sort_args(ctx, args, kwargs).map_err(|e| ctx.record_typed(e))?;
    list_sort(ctx, args[0], key, reverse).map_err(|e| ctx.record_typed(e))
}

fn arity(args: &[Value], n: usize) -> Result<(), PyError> {
    if args.len() != n {
        return Err(PyError::PyRaise);
    }
    Ok(())
}

/// `list.append` (fun_builtin_2: self, value).
pub fn list_append_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    list_append(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

/// `list.extend` (fun_builtin_2: self, iterable).
pub fn list_extend_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    list_extend(ctx, args[0], args[1], None).map_err(|e| ctx.record_typed(e))
}

/// `list.clear` (fun_builtin_1: self).
pub fn list_clear_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 1)?;
    list_clear(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// `list.copy` (fun_builtin_1: self).
pub fn list_copy_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 1)?;
    list_copy(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// `list.count` (fun_builtin_2: self, value).
pub fn list_count_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    list_count(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

/// `list.index` (fun_builtin_var, sig 2..4: self, value[,
/// start[, stop]]).
pub fn list_index_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() < 2 || args.len() > 4 {
        return Err(PyError::PyRaise);
    }
    let start = args.get(2).copied();
    let stop = args.get(3).copied();
    list_index(ctx, args[0], args[1], start, stop).map_err(|e| ctx.record_typed(e))
}

/// `list.insert` (fun_builtin_3: self, index, value).
pub fn list_insert_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 3)?;
    list_insert(ctx, args[0], args[1], args[2]).map_err(|e| ctx.record_typed(e))
}

/// `list.pop` (fun_builtin_var, sig 1..2: self[, index]).
pub fn list_pop_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() < 1 || args.len() > 2 {
        return Err(PyError::PyRaise);
    }
    let idx = args.get(1).copied();
    list_pop(ctx, args[0], idx).map_err(|e| ctx.record_typed(e))
}

/// `list.remove` (fun_builtin_2: self, value).
pub fn list_remove_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 2)?;
    list_remove(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

/// `list.reverse` (fun_builtin_1: self).
pub fn list_reverse_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    arity(args, 1)?;
    list_reverse(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// `list.sort` as a `BuiltinKwFn` adapter (C13): the call
/// machinery passes keyword pairs as `(name, value)` tuples;
/// `list_sort_builtin` takes the `KwArg` struct form.
pub fn list_sort_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let kws: Vec<KwArg> = kwargs
        .iter()
        .map(|(name, value)| KwArg {
            name: *name,
            value: *value,
        })
        .collect();
    list_sort_builtin(ctx, args, &kws)
}

/// `list.sort` as a plain `BuiltinFn` (the no-kw path):
/// `call_fun_builtin` always routes through `kw_func` when one
/// is present, so this is only the documented no-kw form — it
/// sorts with no key/reverse.
pub fn list_sort_builtin_nokw(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    list_sort_builtin(ctx, args, &[])
}

/// `list`'s method table (C13, the Zig's `mp_obj_list_methods`
/// shape): name, body, positional arity counting `self`,
/// keyword body. Sigs are the ones the adapters already enforce
/// (see their doc comments); `sort` takes `key=`/`reverse=`
/// kwargs, the rest take none.
pub static LIST_METHODS: &[crate::objects::slots::MethodEntry] = &[
    crate::objects::slots::MethodEntry {
        name: "append",
        func: list_append_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "extend",
        func: list_extend_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "clear",
        func: list_clear_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "copy",
        func: list_copy_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "count",
        func: list_count_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "index",
        func: list_index_builtin,
        min: 2,
        max: 4,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "insert",
        func: list_insert_builtin,
        min: 3,
        max: 3,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "pop",
        func: list_pop_builtin,
        min: 1,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "remove",
        func: list_remove_builtin,
        min: 2,
        max: 2,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "reverse",
        func: list_reverse_builtin,
        min: 1,
        max: 1,
        kw_func: None,
    },
    crate::objects::slots::MethodEntry {
        name: "sort",
        func: list_sort_builtin_nokw,
        min: 1,
        max: 1,
        kw_func: Some(list_sort_kw),
    },
];

