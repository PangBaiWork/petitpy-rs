//! str / bytes iterators (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 464–513
//! (`str_it_iternext`, `mp_obj_new_str_iterator`,
//! `bytes_it_iternext`, `mp_obj_new_bytes_iterator`). Split out of
//! `objstr_a.rs` so that file stays under 800 lines; the shared
//! prelude still lives there and is imported, never copied.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::BuiltinTypes;
use crate::objects::strings::objstr_a::{new_str_via_qstr, str_data};
use crate::objects::strings::unicode::{utf8_charlen, utf8_next_char};
use crate::objects::value::{GcRef, IteratorData, Obj, Value};

// ── iterators ─────────────────────────────────────────────────────────

/// `mp_obj_new_str_iterator` (`objstr.zig:478-486`): a str iterator
/// over `s` (`Obj::Iterator`, index = byte offset). `iter_type` is
/// the `mp_type_polymorph_iter` type object — C12's type ROM owns
/// it; until then callers pass a stub (tests) or leave the getiter
/// slot unwired (see the `getiter: None` note in `objstr_methods_b.rs`).
pub fn new_str_iterator(ctx: &mut CallCtx, iter_type: Option<GcRef>, s: Value) -> Value {
    Value::Heap(ctx.alloc(
        iter_type,
        Obj::Iterator(IteratorData {
            source: s,
            index: 0,
            extra: vec![],
        }),
    ))
}

/// `mp_obj_new_bytes_iterator` (`objstr.zig:499-507`): same shape
/// for bytes.
pub fn new_bytes_iterator(ctx: &mut CallCtx, iter_type: Option<GcRef>, s: Value) -> Value {
    new_str_iterator(ctx, iter_type, s)
}

/// `str_it_iternext` (`objstr.zig:464-477`): the next 1-char str
/// (one UTF-8 code point via `utf8NextChar`), or `None` at the end
/// (`MP_OBJ_STOP_ITERATION`). The character is built with
/// `new_str_via_qstr` — a static qstr when it matches one.
pub fn str_iter_next(ctx: &mut CallCtx, types: &BuiltinTypes, iter: Value) -> Option<Value> {
    let r = iter.as_heap()?;
    let (source, index) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.index),
        _ => return None,
    };
    let ch: Vec<u8> = match str_data(ctx.arena, source) {
        Some(data) if index < data.len() => {
            let end = utf8_next_char(data, index);
            data[index..end].to_vec()
        }
        _ => return None,
    };
    let end = index + ch.len();
    let out = new_str_via_qstr(ctx, types, &ch);
    if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
        d.index = end;
    }
    Some(out)
}

/// `bytes_it_iternext` (`objstr.zig:487-497`): the next byte as a
/// small int, or `None` at the end.
pub fn bytes_iter_next(ctx: &mut CallCtx, iter: Value) -> Option<Value> {
    let r = iter.as_heap()?;
    let (source, index) = match ctx.arena.get(r) {
        Obj::Iterator(d) => (d.source, d.index),
        _ => return None,
    };
    let b = match str_data(ctx.arena, source) {
        Some(data) => *data.get(index)?,
        _ => return None,
    };
    if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
        d.index = index + 1;
    }
    Some(Value::SmallInt(b as i64))
}

/// The byte length of a str value's data, in code points
/// (`utf8Charlen` over the data — `len(str)` under `STR_UNICODE=1`).
pub fn str_charlen(arena: &GcArena, v: Value) -> Option<usize> {
    str_data(arena, v).map(utf8_charlen)
}
