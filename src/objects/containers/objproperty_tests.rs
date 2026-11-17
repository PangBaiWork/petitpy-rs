//! Tests for property (C9c): make_new (incl. the kw-parse rules),
//! getter/setter/deleter clones, the proxy triple, the method
//! table, type data. Every error case is verified against the
//! reference binary (see `objproperty.rs` docs for the probe
//! citations).

use super::objproperty::{
    lookup_property_method, property_deleter_builtin, property_getter_builtin,
    property_make_new, property_proxy, property_setter_builtin, property_type_data,
    PropertyError, PROPERTY_METHODS,
};
use super::tests::{empty_dict, Fixture};
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::pyerr::PyError;

/// Allocate a real `property` type in the fixture (rooted, the
/// `Fixture::new` pattern).
fn property_type(fx: &mut Fixture) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let t = fx
        .arena
        .alloc(None, Obj::Type(property_type_data(dict)), &fx.roots);
    fx.roots.push(Value::Heap(t));
    t
}

/// Build a property via `property_make_new`.
fn mk_property(fx: &mut Fixture, args: &[Value], n_kw: usize) -> Value {
    let t = property_type(fx);
    let mut ctx = fx.ctx();
    property_make_new(&mut ctx, t, args, n_kw).expect("mk_property")
}

/// Read a property's proxy triple back.
fn proxy_of(fx: &Fixture, v: Value) -> (Value, Value, Value) {
    property_proxy(&fx.arena, v)
}

/// Assert a `PropertyError`'s type name + message.
fn assert_err(e: PropertyError, name: &str, msg: &str) {
    assert_eq!(e.exc_name(), name);
    assert_eq!(e.msg(), Some(msg));
}

// ── make_new ─────────────────────────────────────────────────────────

#[test]
fn property_make_new_no_args() {
    let mut fx = Fixture::new();
    let p = mk_property(&mut fx, &[], 0);
    assert_eq!(proxy_of(&fx, p), (Value::None, Value::None, Value::None));
}

#[test]
fn property_make_new_one_arg() {
    let mut fx = Fixture::new();
    let p = mk_property(&mut fx, &[Value::SmallInt(1)], 0);
    assert_eq!(
        proxy_of(&fx, p),
        (Value::SmallInt(1), Value::None, Value::None)
    );
}

#[test]
fn property_make_new_four_positional() {
    let mut fx = Fixture::new();
    // The 4th positional fills `doc` — and is silently discarded.
    let p = mk_property(
        &mut fx,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
        ],
        0,
    );
    assert_eq!(
        proxy_of(&fx, p),
        (Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3))
    );
}

#[test]
fn property_make_new_doc_kw() {
    let mut fx = Fixture::new();
    let doc = static_qstr_id(b"doc").expect("MP_QSTR_doc is static");
    let x = fx.heap_str(b"x");
    let p = mk_property(
        &mut fx,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::Qstr(doc),
            x,
        ],
        1,
    );
    assert_eq!(
        proxy_of(&fx, p),
        (Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3))
    );
}

#[test]
fn property_make_new_doc_only_kw() {
    let mut fx = Fixture::new();
    let doc = static_qstr_id(b"doc").expect("MP_QSTR_doc is static");
    let x = fx.heap_str(b"x");
    let p = mk_property(&mut fx, &[Value::Qstr(doc), x], 1);
    assert_eq!(proxy_of(&fx, p), (Value::None, Value::None, Value::None));
}

#[test]
fn property_make_new_duplicate_doc() {
    // `property(1,2,3,4,doc='x')` → mismatch: the 4th positional
    // already filled `doc`, so the kw is never counted and
    // `kws_found < kws->used` (reference binary).
    let mut fx = Fixture::new();
    let doc = static_qstr_id(b"doc").expect("MP_QSTR_doc is static");
    let x = fx.heap_str(b"x");
    let t = property_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = property_make_new(
        &mut ctx,
        t,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
            Value::Qstr(doc),
            x,
        ],
        1,
    )
    .expect_err("duplicate doc");
    assert_err(e, "TypeError", "argument num/types mismatch");
}

#[test]
fn property_make_new_unknown_kw() {
    // `property(fget=1)` → mismatch: `fget` is not a named arg
    // (only `doc` is); `property(1,2,3,4,doc='x',fget=1)` → mismatch
    // too (reference binary). (`start` is a static qstr used here
    // as the unknown kw name — `fget` is not in the static table.)
    let mut fx = Fixture::new();
    let fget = static_qstr_id(b"start").expect("MP_QSTR_start is static");
    let t = property_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = property_make_new(
        &mut ctx,
        t,
        &[Value::Qstr(fget), Value::SmallInt(1)],
        1,
    )
    .expect_err("fget kw");
    assert_err(e, "TypeError", "argument num/types mismatch");
}

#[test]
fn property_make_new_too_many_positional() {
    let mut fx = Fixture::new();
    let t = property_type(&mut fx);
    let mut ctx = fx.ctx();
    let e = property_make_new(
        &mut ctx,
        t,
        &[
            Value::SmallInt(1),
            Value::SmallInt(2),
            Value::SmallInt(3),
            Value::SmallInt(4),
            Value::SmallInt(5),
        ],
        0,
    )
    .expect_err("property(1,2,3,4,5)");
    assert_err(e, "TypeError", "argument num/types mismatch");
}

#[test]
fn property_make_new_empty_qstr_kw() {
    // `property(**{'': 9})` fills ALL THREE positional-only slots:
    // the vendor kw lookup runs per slot, and the same map entry
    // matches every slot whose qst is the empty qstr — verified on
    // the reference binary (`C().x`, `c.x = 5` and `del c.x` all
    // raise "object not callable", i.e. getter/setter/deleter are
    // all 9).
    let mut fx = Fixture::new();
    let empty = static_qstr_id(b"").expect("MP_QSTR_ is the empty qstr");
    let p = mk_property(&mut fx, &[Value::Qstr(empty), Value::SmallInt(9)], 1);
    assert_eq!(
        proxy_of(&fx, p),
        (Value::SmallInt(9), Value::SmallInt(9), Value::SmallInt(9))
    );
}

// ── getter / setter / deleter clones ─────────────────────────────────

#[test]
fn property_getter_clone() {
    let mut fx = Fixture::new();
    let p = mk_property(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        0,
    );
    let mut ctx = fx.ctx();
    let p2 = property_getter_builtin(&mut ctx, &[p, Value::SmallInt(9)]).expect("getter");
    drop(ctx);
    assert_eq!(
        proxy_of(&fx, p2),
        (Value::SmallInt(9), Value::SmallInt(2), Value::SmallInt(3))
    );
    // The original is unchanged (the clone is a fresh object).
    assert_eq!(
        proxy_of(&fx, p),
        (Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3))
    );
}

#[test]
fn property_setter_clone() {
    let mut fx = Fixture::new();
    let p = mk_property(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        0,
    );
    let mut ctx = fx.ctx();
    let p2 = property_setter_builtin(&mut ctx, &[p, Value::SmallInt(9)]).expect("setter");
    drop(ctx);
    assert_eq!(
        proxy_of(&fx, p2),
        (Value::SmallInt(1), Value::SmallInt(9), Value::SmallInt(3))
    );
}

#[test]
fn property_deleter_clone() {
    let mut fx = Fixture::new();
    let p = mk_property(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        0,
    );
    let mut ctx = fx.ctx();
    let p2 = property_deleter_builtin(&mut ctx, &[p, Value::SmallInt(9)]).expect("deleter");
    drop(ctx);
    assert_eq!(
        proxy_of(&fx, p2),
        (Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(9))
    );
}

#[test]
fn property_clone_keeps_header_type() {
    // `cloneWith` copies the whole struct including `base.type`
    // (`m_malloc`, not the typed helper) — a subclass instance
    // clones as the subclass.
    let mut fx = Fixture::new();
    // A plain type named `object` (a static qstr) stands in for a
    // property subclass — only the header-type identity matters.
    let sub = super::tests::make_type(&mut fx.arena, &fx.roots, b"object");
    fx.roots.push(Value::Heap(sub));
    let mut ctx = fx.ctx();
    let p = property_make_new(&mut ctx, sub, &[Value::SmallInt(1)], 0).expect("sub property");
    let p2 = property_getter_builtin(&mut ctx, &[p, Value::SmallInt(9)]).expect("getter");
    drop(ctx);
    assert_eq!(obj_get_type(&fx.arena, &fx.types, p2), Some(sub));
}

#[test]
fn property_builtin_arity() {
    let mut fx = Fixture::new();
    let p = mk_property(&mut fx, &[Value::SmallInt(1)], 0);
    let mut ctx = fx.ctx();
    assert_eq!(
        property_getter_builtin(&mut ctx, &[p]),
        Err(PyError::PyRaise)
    );
    assert_eq!(
        property_setter_builtin(&mut ctx, &[p, Value::SmallInt(2), Value::SmallInt(3)]),
        Err(PyError::PyRaise)
    );
}

// ── proxy / methods / type data ──────────────────────────────────────

#[test]
fn property_proxy_triple() {
    let mut fx = Fixture::new();
    let p = mk_property(
        &mut fx,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
        0,
    );
    assert_eq!(
        property_proxy(&fx.arena, p),
        (Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3))
    );
}

#[test]
fn property_methods_table() {
    assert_eq!(PROPERTY_METHODS.len(), 3);
    let names: Vec<&str> = PROPERTY_METHODS.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, vec!["getter", "setter", "deleter"]);
    assert!(lookup_property_method("getter").is_some());
    assert!(lookup_property_method("setter").is_some());
    assert!(lookup_property_method("deleter").is_some());
    assert!(lookup_property_method("fget").is_none());
}

#[test]
fn property_type_data_shape() {
    let mut fx = Fixture::new();
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = property_type_data(dict);
    assert_eq!(
        data.name,
        static_qstr_id(b"property").expect("MP_QSTR_property is static")
    );
    assert_eq!(data.flags, 0x0000);
    // No slots at all — only make_new + locals_dict in the Zig.
    assert!(data.unary_op.is_none());
    assert!(data.binary_op.is_none());
    assert!(data.call.is_none());
    assert!(data.getiter.is_none());
    assert!(data.subscr.is_none());
    assert!(data.buffer.is_none());
    assert!(data.print.is_none());
}
