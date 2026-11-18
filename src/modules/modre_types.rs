//! The `re` / `match` type objects and their print slots
//! (Lane D5 — `extmod/modre.c`): split out of `modre.rs` so
//! neither file exceeds the 800-line cap.

use crate::memory::gc::CallCtx;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::value::{GcRef, Obj, TypeData, Value};

// ── print slots ─────────────────────────────────────────────────────

/// `re_print` (modre.c): `<re %p>` — the C prints the raw
/// pointer; Rust prints the arena slot index instead (the
/// pointer is ASLR-random, no test can pin it — see the
/// module docs).
pub(crate) fn re_print(ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let _ = ctx;
    let slot = match value {
        Value::Heap(r) => r.0,
        _ => 0,
    };
    let _ = sink.write_str(&format!("<re 0x{slot:x}>"));
}

/// `match_print` (modre.c): `<match num=%d>` — exact.
pub(crate) fn match_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let _ = ctx;
    let num = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Match(m) => m.num_matches,
            _ => 0,
        },
        _ => 0,
    };
    let _ = sink.write_str(&format!("<match num={num}>"));
}

// ── type builders ───────────────────────────────────────────────────

/// `mp_type_re` (modre.c): flags `NONE`, slots print /
/// locals_dict only, no make_new (patterns are created by
/// `compile`, not by calling the type).
pub(crate) fn re_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"re").expect("re is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx,
             value: Value,
             kind: PrintKind,
             sink: &mut dyn std::fmt::Write| {
                re_print(ctx, value, kind, sink)
            },
        ),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// `mp_type_match` (modre.c): flags `NONE`, slots print /
/// locals_dict only, no make_new (matches are created by the
/// exec/search/sub helpers).
pub(crate) fn match_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"match").expect("match is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(
            |ctx: &mut CallCtx,
             value: Value,
             kind: PrintKind,
             sink: &mut dyn std::fmt::Write| {
                match_print(ctx, value, kind, sink)
            },
        ),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

