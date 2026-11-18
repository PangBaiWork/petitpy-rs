//! The `match` object's methods (Lane D5 — `extmod/modre.c`
//! `match_group` / `match_groups` / `match_span_helper` /
//! `match_span` / `match_start` / `match_end`): split out of
//! `modre.rs` so neither file exceeds the 800-line cap.
//! The shared helpers (`match_parts`, `subject_is_str`,
//! `subject_bytes`, `piece`) live in `modre.rs`.

use super::modre::{match_parts, piece, subject_bytes, subject_is_str};
use crate::memory::gc::CallCtx;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_int;
use crate::objects::strings::unicode::utf8_ptr_to_index;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_type_arg;

// ── match methods ───────────────────────────────────────────────────

/// `match_group` (modre.c:72-85): `group(n)` — `n` out of
/// range (`n < 0 || n >= num_matches`) raises
/// `IndexError(no_in)` with the ORIGINAL arg (probed:
/// `group(5)` → `IndexError args (5,)`); a
/// non-participating group is `None`; else the span as a str
/// or bytes (the subject's type). `n` via `mp_obj_get_int`
/// (a str arg is `TypeError("can't convert to int")`,
/// probed).
pub(crate) fn match_group_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (caps, subject) = match match_parts(ctx, args[0]) {
        Some(p) => p,
        None => {
            // Unreachable through normal attribute access —
            // defensive, like `re_split_builtin`.
            ctx.record_raise("TypeError", Some("group() requires a match object"));
            return Err(PyError::PyRaise);
        }
    };
    let n = obj_get_int(ctx, ctx.types, args[1]).map_err(|e| ctx.record_typed(e))?;
    if n < 0 || n as usize >= caps.len() {
        return Err(raise_type_arg(ctx, "IndexError", args[1]));
    }
    let cap = &caps[n as usize];
    match cap.start {
        None => Ok(Value::None),
        Some(s) => {
            let e = cap.end.expect("a participating group has an end");
            let is_str = subject_is_str(ctx, subject);
            let sb = subject_bytes(ctx, subject);
            Ok(piece(ctx, is_str, &sb[s..e]))
        }
    }
}

/// `match_groups` (modre.c:89-101): `groups()` — the tuple
/// of groups 1..n (`()` when there are no capture groups —
/// the C returns `mp_const_empty_tuple`). Each element is
/// `match_group(i)`, so a non-participating group is `None`.
pub(crate) fn match_groups_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (caps, subject) = match match_parts(ctx, args[0]) {
        Some(p) => p,
        None => {
            ctx.record_raise("TypeError", Some("groups() requires a match object"));
            return Err(PyError::PyRaise);
        }
    };
    if caps.len() <= 1 {
        return Ok(Value::Heap(ctx.types.empty_tuple));
    }
    let is_str = subject_is_str(ctx, subject);
    let sb = subject_bytes(ctx, subject);
    let items: Vec<Value> = caps[1..]
        .iter()
        .map(|c| match c.start {
            None => Value::None,
            Some(s) => {
                let e = c.end.expect("a participating group has an end");
                piece(ctx, is_str, &sb[s..e])
            }
        })
        .collect();
    let t = new_tuple(ctx, ctx.types.tuple, &items);
    Ok(Value::Heap(t))
}

/// `match_span_helper` (modre.c:106-135): the shared
/// `span` / `start` / `end` body. The group number defaults
/// to 0 and is bounds-checked like `group` (negative or `>=
/// num_matches` → `IndexError(arg)`). A non-participating
/// group spans `(-1, -1)`. For a STR subject the byte
/// offsets are converted to CHAR indices
/// (`utf8_ptr_to_index` — probed: `search("é")` on `"aéb"`
/// spans `(1, 2)`); a bytes subject keeps byte indices.
pub(crate) fn match_span_pair(ctx: &mut CallCtx, args: &[Value]) -> PyResult<(i64, i64)> {
    let (caps, subject) = match match_parts(ctx, args[0]) {
        Some(p) => p,
        None => {
            ctx.record_raise("TypeError", Some("span() requires a match object"));
            return Err(PyError::PyRaise);
        }
    };
    let mut no: i64 = 0;
    if args.len() == 2 {
        no = obj_get_int(ctx, ctx.types, args[1]).map_err(|e| ctx.record_typed(e))?;
        if no < 0 || no as usize >= caps.len() {
            return Err(raise_type_arg(ctx, "IndexError", args[1]));
        }
    }
    let (mut s, mut e) = (-1i64, -1i64);
    if let Some(start) = caps[no as usize].start {
        let end = caps[no as usize]
            .end
            .expect("a participating group has an end");
        s = start as i64;
        e = end as i64;
        if subject_is_str(ctx, subject) {
            let sb = subject_bytes(ctx, subject);
            s = utf8_ptr_to_index(&sb, start) as i64;
            e = utf8_ptr_to_index(&sb, end) as i64;
        }
    }
    Ok((s, e))
}

/// `match_span` (modre.c): `span([group])` → `(start, end)`.
pub(crate) fn match_span_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (s, e) = match_span_pair(ctx, args)?;
    let items = [
        Value::small_int(s).expect("a span fits the small-int window"),
        Value::small_int(e).expect("a span fits the small-int window"),
    ];
    let t = new_tuple(ctx, ctx.types.tuple, &items);
    Ok(Value::Heap(t))
}

/// `match_start` (modre.c): `start([group])`.
pub(crate) fn match_start_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (s, _e) = match_span_pair(ctx, args)?;
    Ok(Value::small_int(s).expect("a span fits the small-int window"))
}

/// `match_end` (modre.c): `end([group])`.
pub(crate) fn match_end_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_s, e) = match_span_pair(ctx, args)?;
    Ok(Value::small_int(e).expect("a span fits the small-int window"))
}
