//! attrtuple (C9c: `src/objects/containers/objattrtuple.zig`, 118 lines, 0 test blocks).
//!
//! An attrtuple is a regular tuple with named fields: the Zig stores
//! the tuple's flex `items[]` sized `len + 1`, the trailing slot
//! holding the qstr field-name array as a fake-Obj pointer. The Rust
//! port stores the same data plainly — `AttrTupleData { items,
//! fields }` (`fields: Vec<u32>` of qstr ids; added to the C0 stub
//! this lane).
//!
//! The type NAME is `tuple` (`MP_QSTR_tuple`), flags 0. Slots: print
//! (custom), unary_op / binary_op / subscr / getiter = the TUPLE's
//! slots, attr (custom field lookup). The tuple slots are C9b's —
//! C9b has merged into master, but this worktree is on the stale
//! base 613b242 (pre-C9b) where the tuple type has no slots yet, so
//! the four delegating slots are `None` here with a TODO for the
//! rebase: wire `tuple_unary_op` / `tuple_binary_op` /
//! `tuple_subscr` / `tuple_getiter` from C9b's `objtuple.rs` at
//! merge time. NO make_new, NO locals dict.
//!
//! The print helper is exported for C11's namedtuple repr:
//! `mp_obj_attrtuple_print_helper` is used by modcollections.c.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::value::{AttrTupleData, GcRef, Obj, TypeData, Value};

/// Extract a cloned `AttrTupleData` from a heap attrtuple value
/// (panics on any other value — the slot/type-data contract).
fn attrtuple_data_of(arena: &GcArena, v: Value) -> AttrTupleData {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::AttrTuple(a) => a.clone(),
            _ => panic!("attrtuple op on a non-attrtuple value"),
        },
        _ => panic!("attrtuple op on a non-heap value"),
    }
}

/// `mp_obj_new_attrtuple(fields, n, items)`
/// (objattrtuple.zig:78-94): allocate with the given (attrtuple)
/// type. `fields` and `items` must have the same length — the
/// caller (C11's namedtuple constructor) builds both from one
/// field list; a mismatch is a caller bug, asserted here (the Zig
/// would read the fields array out of bounds).
pub fn attrtuple_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    fields: Vec<u32>,
    items: Vec<Value>,
) -> Value {
    assert_eq!(
        fields.len(),
        items.len(),
        "attrtuple fields/items length mismatch"
    );
    Value::Heap(ctx.alloc(Some(type_), Obj::AttrTuple(AttrTupleData { items, fields })))
}

/// `mp_obj_attrtuple_print_helper` (objattrtuple.zig:24-38): `(` +
/// `field=item` (PRINT_REPR) joined by `, ` + `)`. NO type-name
/// prefix. Exported for C11's namedtuple repr. Field names render
/// raw via the qstr pool (`qstr_str` — the `id as u16` cast is the
/// `map.rs` `hash_of` precedent); items print via
/// `obj_print_helper` (PRINT_REPR).
pub fn attrtuple_print_helper(
    ctx: &mut CallCtx,
    fields: &[u32],
    items: &[Value],
    sink: &mut dyn std::fmt::Write,
) {
    let _ = sink.write_str("(");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            let _ = sink.write_str(", ");
        }
        let name = ctx.pool.qstr_str(fields[i] as u16);
        let _ = sink.write_str(&String::from_utf8_lossy(name));
        let _ = sink.write_str("=");
        obj_print_helper(ctx, ctx.types, *item, PrintKind::Repr, sink);
    }
    let _ = sink.write_str(")");
}

/// `mp_obj_attrtuple_print` (objattrtuple.zig:40-47): the type's
/// print slot — delegates to the helper with the stored fields.
pub fn attrtuple_print(
    ctx: &mut CallCtx,
    v: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let data = attrtuple_data_of(ctx.arena, v);
    attrtuple_print_helper(ctx, &data.fields, &data.items, sink);
}

/// `mp_obj_attrtuple_attr` (objattrtuple.zig:49-60): field-name
/// qstr lookup → the item (first match wins — the Zig scans in
/// order and returns on the first hit, so a duplicated field name
/// resolves to its first item). The store path (`dest[0] != NULL`)
/// is a no-op in the Zig; this fn is the LOAD lookup and returns
/// `None` for a miss — C12 owns the store dispatch and must NOT
/// write through here.
pub fn attrtuple_attr(arena: &GcArena, self_v: Value, attr: u32) -> Option<Value> {
    let data = attrtuple_data_of(arena, self_v);
    data.fields
        .iter()
        .position(|f| *f == attr)
        .map(|i| data.items[i])
}

/// `mp_type_attrtuple` ROM data (objattrtuple.zig:97-116): name
/// `tuple`, flags `0x0000`; print slot only (the tuple slots are
/// C9b's — see the module docs; wire them at the rebase).
pub fn attrtuple_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"tuple").expect("MP_QSTR_tuple is in the static table"),
        flags: 0x0000,
        base: None,
        mro: Vec::new(),
        dict,
        // TODO(C9b rebase): tuple_unary_op (this worktree predates C9b).
        unary_op: None,
        // TODO(C9b rebase): tuple_binary_op.
        binary_op: None,
        call: None,
        // TODO(C9b rebase): tuple_getiter.
        getiter: None,
        // TODO(C9b rebase): tuple_subscr.
        subscr: None,
        buffer: None,
        print: Some(attrtuple_print),
    }
}
