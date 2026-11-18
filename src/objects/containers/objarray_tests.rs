//! Tests for `objarray.rs` + `objarray_methods.rs`.
//!
//! Reference-binary probe values (2026-11-17, `/tmp/c9b_probe*.py`):
//! - `array('i',[2**31])` → `[-2147483648]` (truncation, no
//!   error); `array('b',[200])` → `-56`; `array('B',[300])` →
//!   `44`; `array('i',[2**70+5])` → `5` (the exact-int path
//!   writes the low bytes); `array('f',[1])` → `[0]` (e/f/d
//!   *values* are dead — they read/write as 0).
//! - `array('i').tobytes()` / `.frombytes()` / `.itemsize` →
//!   `AttributeError` — the brief's premise was WRONG; array
//!   has NO tobytes/frombytes/itemsize (locals = append/extend
//!   only).
//! - `bytearray(-1)` ABORTS the reference (rc=134,
//!   `@intCast` panic); Rust raises `ValueError("negative
//!   count")` instead. `memoryview([1,2])` / `memoryview(123)`
//!   ABORT the reference (rc=134, uninitialized typecode);
//!   Rust raises the buffer-protocol `TypeError` instead.
//! - `array('i') + array('i')` and `bytearray() + bytearray()`
//!   ABORT the reference (rc=134, `res.items.?` on a NULL
//!   buffer); Rust returns the empty array.
//! - `array` is a MODULE, not a builtin — probes need
//!   `from array import array`.
//!
//! Discipline: build every fixture value first, then create the
//! `CallCtx` — a live `ctx` borrows the fixture mutably, so no
//! `fx.heap_*` call may follow it in the same scope.

use super::objarray::*;
use super::objarray_make_new::*;
use super::objarray_methods::*;
use super::tests::Fixture;
use crate::objects::obj::ObjError;
use crate::objects::slots::{BinaryOp, BufferInfo, PrintKind, SubscrArg, UnaryOp};
use crate::objects::strings::objstr_methods_c::KwArg;
use crate::objects::value::{Obj, SliceData, Value};
use crate::vendor::mpz::BigInt;

fn ints(items: &[i64]) -> Vec<Value> {
    items.iter().map(|&n| Value::SmallInt(n)).collect()
}

/// An `array('i', [...])`-style value, built through
/// `array_make_new` (the header type is `fx.array`).
fn arr(fx: &mut Fixture, tc: &[u8], elems: Vec<Value>) -> Value {
    let tc_str = fx.heap_str(tc);
    let items = fx.heap_list(elems);
    let t_arr = fx.array;
    let t_ba = fx.bytearray;
    let mut ctx = fx.ctx();
    array_make_new(&mut ctx, t_arr, t_ba, &[tc_str, items], 0, None).unwrap()
}

/// A `bytearray(b'...')` value, built through
/// `bytearray_make_new`.
fn ba(fx: &mut Fixture, bytes: &[u8]) -> Value {
    let b = fx.heap_bytes(bytes);
    let t_arr = fx.array;
    let t_ba = fx.bytearray;
    let mut ctx = fx.ctx();
    bytearray_make_new(&mut ctx, t_arr, t_ba, &[b], &[], None).unwrap()
}

/// The elements of an array/bytearray/memoryview, read back
/// through `array_get_element`.
fn elems_of(ctx: &mut crate::memory::gc::CallCtx, v: Value) -> Vec<Value> {
    let n = match array_unary_op(ctx, UnaryOp::Len, v) {
        Ok(Value::SmallInt(n)) => n as usize,
        other => panic!("len of array: {other:?}"),
    };
    (0..n)
        .map(|i| array_get_element(ctx, v, i))
        .collect()
}

fn print_to_string(ctx: &mut crate::memory::gc::CallCtx, v: Value) -> String {
    let mut s = String::new();
    array_print(ctx, v, PrintKind::Repr, &mut s);
    s
}

/// A heap `Obj::Slice` with these bounds (`None` = omitted).
fn slice_val(
    fx: &mut Fixture,
    start: Option<Value>,
    stop: Option<Value>,
    step: Option<Value>,
) -> Value {
    let r = fx
        .arena
        .alloc(None, Obj::Slice(SliceData { start, stop, step }), &fx.roots);
    Value::Heap(r)
}

#[test]
fn array_make_new_basics() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2, 3]));
    let tc_ii = fx.heap_str(b"ii");
    let empty_items = fx.heap_list(vec![]);
    let b2 = fx.heap_bytes(b"\x01\x02");
    let tc_i = fx.heap_str(b"i");
    let one_half = {
        let t = fx.types.float;
        let mut c2 = fx.ctx();
        Value::Heap(c2.alloc(Some(t), Obj::Float(1.5)))
    };
    let items_half = fx.heap_list(vec![one_half]);
    let tc_i2 = fx.heap_str(b"i");
    let t = fx.array;
    let t_ba = fx.bytearray;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(elems_of(ctx, a), ints(&[1, 2, 3]));
    // First char only is the typecode.
    let a2 = array_make_new(ctx, t, t_ba, &[tc_ii, empty_items], 0, None).unwrap();
    assert_eq!(array_typecode(ctx, a2), Some(b'i'));
    // `array('i', b"\x01\x02")` — the memcpy path truncates to
    // a whole number of elements (len 2/4 = 0).
    let a3 = array_make_new(ctx, t, t_ba, &[tc_i, b2], 0, None).unwrap();
    assert_eq!(elems_of(ctx, a3), Vec::<Value>::new());
    // `array('i', [1.5])` → "can't convert to int".
    match array_make_new(ctx, t, t_ba, &[tc_i2, items_half], 0, None) {
        Err(ArrayError::Obj(ObjError::IntConversion)) => {}
        other => panic!("array('i', [1.5]): {other:?}"),
    }
}

#[test]
fn array_make_new_errors() {
    let mut fx = Fixture::new();
    let tc_empty = fx.heap_str(b"");
    let tc_x = fx.heap_str(b"x");
    let tc_i = fx.heap_str(b"i");
    let items_i = fx.heap_list(vec![]);
    let extra = fx.heap_list(vec![]);
    let empty_items = fx.heap_list(vec![]);
    let empty_items2 = fx.heap_list(vec![]);
    let empty_items3 = fx.heap_list(vec![]);
    let t = fx.array;
    let t_ba = fx.bytearray;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    match array_make_new(ctx, t, t_ba, &[tc_empty, empty_items], 0, None) {
        Err(ArrayError::BadTypecode) => {}
        other => panic!("array(''): {other:?}"),
    }
    match array_make_new(ctx, t, t_ba, &[tc_x, empty_items2], 0, None) {
        Err(ArrayError::BadTypecode) => {}
        other => panic!("array('x'): {other:?}"),
    }
    // Non-str typecode → "can't convert to str implicitly".
    match array_make_new(ctx, t, t_ba, &[Value::SmallInt(1), empty_items3], 0, None) {
        Err(ArrayError::NotStr) => {}
        other => panic!("array(1): {other:?}"),
    }
    // Too many positionals → the polled sig error survives
    // (the construct path succeeds, so nothing overwrites it).
    match array_make_new(ctx, t, t_ba, &[tc_i, items_i, extra], 0, None) {
        Err(ArrayError::ArgNumTypesMismatch) => {}
        other => panic!("array('i', [1], [2]): {other:?}"),
    }
}

#[test]
fn array_truncation() {
    let mut fx = Fixture::new();
    // `array('i',[2**31])` → `[-2147483648]` (truncation).
    let a = arr(&mut fx, b"i", ints(&[1 << 31]));
    let ab = arr(&mut fx, b"b", ints(&[200]));
    let a_b = arr(&mut fx, b"B", ints(&[300]));
    // `array('i',[2**70+5])` → `5` — the exact-int path writes
    // the low bytes (C7's `as_bytes` return ignored).
    let big = fx.heap_bigint((BigInt::from_int(1) << 70u64) + BigInt::from_int(5));
    let a_big = arr(&mut fx, b"i", vec![big]);
    // `array('q',[2**62])` → `2**62` (in window for i64).
    let big62 = fx.heap_bigint(BigInt::from_int(1) << 62u64);
    let a_q = arr(&mut fx, b"q", vec![big62]);
    // `array('f',[1])` → `[0]` — the e/f/d value branches are
    // dead; `binary_get_val_array` falls through to SmallInt(0).
    let a_f = arr(&mut fx, b"f", ints(&[1]));
    // The `q` element reads back as a FRESH heap int (a new
    // GcRef) — compare payloads, not refs. The `ctx` block
    // must end first so the arena can be read again.
    let got_q = {
        let mut ctx = fx.ctx();
        let ctx = &mut ctx;
        assert_eq!(elems_of(ctx, a), ints(&[-2147483648]));
        assert_eq!(elems_of(ctx, ab), ints(&[-56]));
        assert_eq!(elems_of(ctx, a_b), ints(&[44]));
        assert_eq!(elems_of(ctx, a_big), ints(&[5]));
        assert_eq!(elems_of(ctx, a_f), ints(&[0]));
        elems_of(ctx, a_q)
    };
    assert_eq!(got_q.len(), 1);
    assert_eq!(
        crate::objects::primitives::ops::heap_int(&fx.arena, got_q[0]),
        Some(&(BigInt::from_int(1) << 62u64))
    );
}

#[test]
fn bytearray_make_new_basics() {
    let mut fx = Fixture::new();
    let b = ba(&mut fx, b"ab");
    let s_ab = fx.heap_str(b"ab");
    let enc = fx.heap_str(b"utf-8");
    let items_12300 = fx.heap_list(ints(&[1, 2, 300]));
    let t = fx.bytearray;
    let t_arr = fx.array;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(elems_of(ctx, b), ints(&[97, 98]));
    // Zero-filled count.
    let b = bytearray_make_new(ctx, t_arr, t, &[Value::SmallInt(3)], &[], None).unwrap();
    assert_eq!(elems_of(ctx, b), ints(&[0, 0, 0]));
    // `bytearray("ab", "utf-8")` → `b'ab'`.
    let b = bytearray_make_new(ctx, t_arr, t, &[s_ab, enc], &[], None).unwrap();
    assert_eq!(elems_of(ctx, b), ints(&[97, 98]));
    // `bytearray([1,2,300])` → `[1,2,44]` (truncation).
    let b = bytearray_make_new(ctx, t_arr, t, &[items_12300], &[], None).unwrap();
    assert_eq!(elems_of(ctx, b), ints(&[1, 2, 44]));
    // Extra positional ignored (`bytearray(3, 4)`).
    let b = bytearray_make_new(
        ctx,
        t_arr,
        t,
        &[Value::SmallInt(3), Value::SmallInt(4)],
        &[],
        None,
    )
    .unwrap();
    assert_eq!(elems_of(ctx, b), ints(&[0, 0, 0]));
}

#[test]
fn bytearray_make_new_errors() {
    let mut fx = Fixture::new();
    let s_ab = fx.heap_str(b"ab");
    let big70 = fx.heap_bigint(BigInt::from_int(1) << 70u64);
    // A DYNAMIC qstr literal (what source code produces).
    let q1234 = fx.pool.from_strn(b"1234").unwrap() as u32;
    let t = fx.bytearray;
    let t_arr = fx.array;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `bytearray("ab")` → "wrong number of arguments".
    match bytearray_make_new(ctx, t_arr, t, &[s_ab], &[], None) {
        Err(ArrayError::WrongNumArgs) => {}
        other => panic!("bytearray('ab'): {other:?}"),
    }
    // E2e: `bytearray('1234')` as a dynamic qstr — the old
    // `is_str_value` check was header-type-based and missed
    // `Value::Qstr`, so it slipped through to
    // `array_construct` instead of raising.
    match bytearray_make_new(ctx, t_arr, t, &[Value::Qstr(q1234)], &[], None) {
        Err(ArrayError::WrongNumArgs) => {}
        other => panic!("bytearray('1234' qstr): {other:?}"),
    }
    // `bytearray(-1)` — the reference ABORTS (rc=134,
    // `@intCast` panic); Rust raises ValueError("negative
    // count"), CPython's message.
    match bytearray_make_new(ctx, t_arr, t, &[Value::SmallInt(-1)], &[], None) {
        Err(ArrayError::NegativeCount) => {}
        other => panic!("bytearray(-1): {other:?}"),
    }
    // `bytearray(True)` — bool is not int ("object not
    // iterable").
    match bytearray_make_new(ctx, t_arr, t, &[Value::Bool(true)], &[], None) {
        Err(ArrayError::NotIterable) => {}
        other => panic!("bytearray(True): {other:?}"),
    }
    // `bytearray(2**70)` → OverflowError (get_int overflows).
    match bytearray_make_new(ctx, t_arr, t, &[big70], &[], None) {
        Err(ArrayError::Obj(ObjError::Overflow)) => {}
        other => panic!("bytearray(2**70): {other:?}"),
    }
}

#[test]
fn bytearray_make_new_kwargs() {
    let mut fx = Fixture::new();
    let b = fx.heap_bytes(b"ab");
    let s_ab = fx.heap_str(b"ab");
    let enc = fx.heap_str(b"utf-8");
    let src = crate::objects::primitives::ops::static_qstr_id(b"source").unwrap();
    let enc_id = crate::objects::primitives::ops::static_qstr_id(b"encoding").unwrap();
    let t = fx.bytearray;
    let t_arr = fx.array;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `bytearray(source=b"ab")` — kwargs are interleaved
    // (name, value) pairs at the tail of `args`, `n_kw` = 1.
    let v = bytearray_make_new(
        ctx,
        t_arr,
        t,
        &[],
        &[KwArg { name: src, value: b }],
        None,
    )
    .unwrap();
    assert_eq!(elems_of(ctx, v), ints(&[97, 98]));
    // `bytearray("ab", encoding="utf-8")`.
    let v = bytearray_make_new(
        ctx,
        t_arr,
        t,
        &[s_ab],
        &[KwArg { name: enc_id, value: enc }],
        None,
    )
    .unwrap();
    assert_eq!(elems_of(ctx, v), ints(&[97, 98]));
}

#[test]
fn memoryview_make_new_basics() {
    let mut fx = Fixture::new();
    let b_abc = fx.heap_bytes(b"abc");
    let ba_abc = ba(&mut fx, b"abc");
    let list_12 = fx.heap_list(ints(&[1, 2]));
    let t = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `memoryview(b'abc')` — read-only view over bytes.
    let mv = memoryview_make_new(ctx, t, &[b_abc], 0).unwrap();
    assert_eq!(array_get_element(ctx, mv, 0), Value::SmallInt(97));
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, mv).unwrap(),
        Value::SmallInt(3)
    );
    // `memoryview(bytearray(b'abc'))` — writable view.
    let mv_rw = memoryview_make_new(ctx, t, &[ba_abc], 0).unwrap();
    let mut info = BufferInfo::empty();
    assert!(array_get_buffer(ctx, mv_rw, &mut info, 2).unwrap());
    // View of a view flattens to the base source.
    let mv2 = memoryview_make_new(ctx, t, &[mv], 0).unwrap();
    assert_eq!(array_get_element(ctx, mv2, 1), Value::SmallInt(98));
    // `memoryview([1,2])` / `memoryview(123)` — the reference
    // ABORTS (rc=134, uninitialized typecode); Rust raises the
    // buffer-protocol TypeError instead.
    match memoryview_make_new(ctx, t, &[list_12], 0) {
        Err(ArrayError::Obj(ObjError::BufferProtocol)) => {}
        other => panic!("memoryview([1,2]): {other:?}"),
    }
    match memoryview_make_new(ctx, t, &[Value::SmallInt(123)], 0) {
        Err(ArrayError::Obj(ObjError::BufferProtocol)) => {}
        other => panic!("memoryview(123): {other:?}"),
    }
}

#[test]
fn array_unary_op_slots() {
    let mut fx = Fixture::new();
    let empty = arr(&mut fx, b"i", vec![]);
    let a = arr(&mut fx, b"i", ints(&[1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Bool, empty).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Bool, a).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, empty).unwrap(),
        Value::SmallInt(0)
    );
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, a).unwrap(),
        Value::SmallInt(1)
    );
    assert!(array_unary_op(ctx, UnaryOp::Hash, a).is_err());
}

#[test]
fn array_append_extend() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2]));
    let items_45 = fx.heap_list(ints(&[4, 5]));
    let b_06 = fx.heap_bytes(b"\x06\x00\x00\x00");
    let one_half = {
        let t = fx.types.float;
        let mut c2 = fx.ctx();
        Value::Heap(c2.alloc(Some(t), Obj::Float(1.5)))
    };
    let big = fx.heap_bigint((BigInt::from_int(1) << 70u64) + BigInt::from_int(5));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `append(3)`.
    assert_eq!(
        array_append(ctx, a, Value::SmallInt(3)).unwrap(),
        Value::None
    );
    assert_eq!(elems_of(ctx, a), ints(&[1, 2, 3]));
    // `append(2**70+5)` → `5` (truncation, exact-int path).
    array_append(ctx, a, big).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[1, 2, 3, 5]));
    // Failed append leaves the length unchanged (ADR 012 D4).
    match array_append(ctx, a, one_half) {
        Err(ArrayError::Obj(ObjError::IntConversion)) => {}
        other => panic!("append(1.5): {other:?}"),
    }
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, a).unwrap(),
        Value::SmallInt(4)
    );
    // `extend([4,5])` — the iter path.
    array_extend(ctx, a, items_45, None).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[1, 2, 3, 5, 4, 5]));
    // `extend(b'\x06\x00\x00\x00')` — the bulk memcpy path.
    array_extend(ctx, a, b_06, None).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[1, 2, 3, 5, 4, 5, 6]));
    // Self-extend doubles.
    array_extend(ctx, a, a, None).unwrap();
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, a).unwrap(),
        Value::SmallInt(14)
    );
}

#[test]
fn array_append_on_view_is_error() {
    let mut fx = Fixture::new();
    let ba_abc = ba(&mut fx, b"abc");
    let t = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let mv = memoryview_make_new(ctx, t, &[ba_abc], 0).unwrap();
    assert!(array_append(ctx, mv, Value::SmallInt(1)).is_err());
}

#[test]
fn array_add() {
    let mut fx = Fixture::new();
    let a1 = arr(&mut fx, b"i", ints(&[1]));
    let a2 = arr(&mut fx, b"i", ints(&[2]));
    let e1 = arr(&mut fx, b"i", vec![]);
    let e2 = arr(&mut fx, b"i", vec![]);
    let b_05 = fx.heap_bytes(b"\x05\x00\x00\x00");
    let ba_a = ba(&mut fx, b"a");
    let ba_b = ba(&mut fx, b"b");
    let t_mv = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // `array('i',[1]) + array('i',[2])` → `array('i',[1,2])`.
    let r = array_binary_op(ctx, BinaryOp::Add, a1, a2).unwrap();
    assert_eq!(elems_of(ctx, r), ints(&[1, 2]));
    // Empty + empty — the reference ABORTS (rc=134,
    // `res.items.?` on a NULL buffer); Rust returns the empty
    // array (a `Vec` is never null).
    let r = array_binary_op(ctx, BinaryOp::Add, e1, e2).unwrap();
    assert_eq!(elems_of(ctx, r), Vec::<Value>::new());
    // `array('i',[1]) + b'\x05\x00\x00\x00'` — memcpy
    // reinterpretation.
    let r = array_binary_op(ctx, BinaryOp::Add, a1, b_05).unwrap();
    assert_eq!(elems_of(ctx, r), ints(&[1, 5]));
    // `bytearray(b'a') + bytearray(b'b')` → `bytearray(b'ab')`.
    let r = array_binary_op(ctx, BinaryOp::Add, ba_a, ba_b).unwrap();
    assert_eq!(elems_of(ctx, r), ints(&[97, 98]));
    // Memoryview lhs → NotImplemented (`PyRaise`).
    let mv = memoryview_make_new(ctx, t_mv, &[b_05], 0).unwrap();
    assert!(array_binary_op(ctx, BinaryOp::Add, mv, b_05).is_err());
    // `array('i',[1]) + 5` → "object with buffer protocol
    // required".
    // `array('i',[1]) + 5` → "object with buffer protocol
    // required" — the message is dropped at the slot boundary
    // (C6 convention); the slot just returns `PyRaise`.
    assert!(array_binary_op(ctx, BinaryOp::Add, a1, Value::SmallInt(5)).is_err());
    // INPLACE_ADD extends and returns the lhs.
    let r = array_binary_op(ctx, BinaryOp::InplaceAdd, a1, b_05).unwrap();
    assert_eq!(r, a1);
    assert_eq!(elems_of(ctx, a1), ints(&[1, 5]));
}

#[test]
fn array_contains() {
    let mut fx = Fixture::new();
    let ba_abc = ba(&mut fx, b"abc");
    let a_i = arr(&mut fx, b"i", ints(&[1, 2]));
    let s_a = fx.heap_str(b"a");
    let b_bc = fx.heap_bytes(b"bc");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Int rhs → bare NotImplemented (the Zig checks the rhs
    // kind before searching — `1 in ba` raises, it is NOT the
    // CPython `True`); `b'bc'` → True (buffer rhs searches);
    // `'a'` → True (str HAS a buffer).
    assert!(array_binary_op(ctx, BinaryOp::Contains, ba_abc, Value::SmallInt(98)).is_err());
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Contains, ba_abc, b_bc).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Contains, ba_abc, s_a).unwrap(),
        Value::Bool(true)
    );
    // Int rhs on a non-bytearray → bare NotImplemented.
    assert!(array_binary_op(ctx, BinaryOp::Contains, a_i, Value::SmallInt(1)).is_err());
}

#[test]
fn array_compare() {
    let mut fx = Fixture::new();
    let b1 = arr(&mut fx, b"B", ints(&[1]));
    let b2 = arr(&mut fx, b"B", ints(&[2]));
    let i1 = arr(&mut fx, b"i", ints(&[1]));
    let i1b = arr(&mut fx, b"i", ints(&[1]));
    let i2 = arr(&mut fx, b"i", ints(&[2]));
    let ba_1 = ba(&mut fx, b"\x01");
    let bb1 = arr(&mut fx, b"b", ints(&[1]));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Unsigned bytes compare.
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Less, b1, b2).unwrap(),
        Value::Bool(true)
    );
    // Signed + ordering op → bare NotImplemented.
    assert!(array_binary_op(ctx, BinaryOp::Less, i1, i2).is_err());
    // Equal works for any matching typecode.
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Equal, i1, i1b).unwrap(),
        Value::Bool(true)
    );
    // `array('b',[1]) == bytearray(b'\x01')` → True.
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Equal, bb1, ba_1).unwrap(),
        Value::Bool(true)
    );
    // Non-buffer rhs → False (not an error).
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Equal, i1, Value::SmallInt(1)).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        array_binary_op(ctx, BinaryOp::Less, i1, Value::SmallInt(1)).unwrap(),
        Value::Bool(false)
    );
    // NotEqual is NOT a slot op.
    assert!(array_binary_op(ctx, BinaryOp::NotEqual, i1, i1b).is_err());
}

#[test]
fn array_subscr_get_slice() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2, 3]));
    let s_12 = slice_val(&mut fx, Some(Value::SmallInt(1)), None, None);
    let b_abc = fx.heap_bytes(b"abcdef");
    let t_mv = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        array_subscr(ctx, a, Value::SmallInt(0), SubscrArg::Get).unwrap(),
        Value::SmallInt(1)
    );
    assert_eq!(
        array_subscr(ctx, a, Value::SmallInt(-1), SubscrArg::Get).unwrap(),
        Value::SmallInt(3)
    );
    // `array('i',[1,2,3])[1:]` → `array('i',[2,3])`.
    let sl = array_subscr(ctx, a, s_12, SubscrArg::Get).unwrap();
    assert_eq!(elems_of(ctx, sl), ints(&[2, 3]));
    // Memoryview slice keeps the source (free += start).
    let mv = memoryview_make_new(ctx, t_mv, &[b_abc], 0).unwrap();
    let sl = array_subscr(ctx, mv, s_12, SubscrArg::Get).unwrap();
    assert_eq!(
        array_unary_op(ctx, UnaryOp::Len, sl).unwrap(),
        Value::SmallInt(5)
    );
    assert_eq!(array_get_element(ctx, sl, 0), Value::SmallInt(98));
    // Delete → NotImplemented (`PyRaise`).
    assert!(
        array_subscr(ctx, a, Value::SmallInt(0), SubscrArg::Delete).is_err()
    );
}

#[test]
fn array_subscr_store() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2, 3, 4]));
    let rhs_grow = arr(&mut fx, b"i", ints(&[9, 8, 7, 6]));
    let rhs_shrink = arr(&mut fx, b"i", vec![]);
    let rhs_step = arr(&mut fx, b"i", ints(&[9, 9]));
    let s_12 = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(2)), None);
    let s_13 = slice_val(&mut fx, Some(Value::SmallInt(1)), Some(Value::SmallInt(3)), None);
    let s_step2 = slice_val(
        &mut fx,
        None,
        None,
        Some(Value::SmallInt(2)),
    );
    let s_step0 = slice_val(
        &mut fx,
        None,
        None,
        Some(Value::SmallInt(0)),
    );
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Grow: `[1,2,3,4][1:2] = [9,8,7,6]` → `[1,9,8,7,6,3,4]`.
    array_subscr(ctx, a, s_12, SubscrArg::Set(rhs_grow)).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[1, 9, 8, 7, 6, 3, 4]));
    // Shrink: `[1,9,8,7,6,3,4][1:3] = []` → `[1,7,6,3,4]`.
    array_subscr(ctx, a, s_13, SubscrArg::Set(rhs_shrink)).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[1, 7, 6, 3, 4]));
    // Step != 1 → SliceStep (incl. step 0, unlike list).
    // Step != 1 → slice-step NotImplemented (the message is
    // dropped at the slot boundary; the slot returns `PyRaise`).
    assert!(array_subscr(ctx, a, s_step2, SubscrArg::Set(rhs_step)).is_err());
    assert!(array_subscr(ctx, a, s_step0, SubscrArg::Set(rhs_step)).is_err());
    // Single index store.
    array_subscr(ctx, a, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(9))).unwrap();
    assert_eq!(elems_of(ctx, a), ints(&[9, 7, 6, 3, 4]));
}

#[test]
fn array_subscr_view_store() {
    let mut fx = Fixture::new();
    let ba_abc = ba(&mut fx, b"abc");
    let b_ro = fx.heap_bytes(b"abc");
    let s_02 = slice_val(&mut fx, Some(Value::SmallInt(0)), Some(Value::SmallInt(2)), None);
    let rhs_3b = fx.heap_bytes(b"xyz");
    let t_mv = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Writable view: `mv[0] = 65` changes the source.
    let mv = memoryview_make_new(ctx, t_mv, &[ba_abc], 0).unwrap();
    array_subscr(ctx, mv, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(65))).unwrap();
    assert_eq!(elems_of(ctx, ba_abc), ints(&[65, 98, 99]));
    // Read-only view store → NotImplemented.
    let mv_ro = memoryview_make_new(ctx, t_mv, &[b_ro], 0).unwrap();
    assert!(
        array_subscr(ctx, mv_ro, Value::SmallInt(0), SubscrArg::Set(Value::SmallInt(65)))
            .is_err()
    );
    // Length-changing view store → "lhs and rhs should be
    // compatible".
    assert!(array_subscr(ctx, mv, s_02, SubscrArg::Set(rhs_3b)).is_err());
}

#[test]
fn array_get_buffer_slot() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2, 3]));
    let ba_abc = ba(&mut fx, b"abc");
    let b_ro = fx.heap_bytes(b"abc");
    let t_mv = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let mut info = BufferInfo::empty();
    assert!(array_get_buffer(ctx, a, &mut info, 1).unwrap());
    assert_eq!(info.typecode, b'i');
    assert_eq!(info.len, 12);
    // Read-only view + WRITE flag → Ok(false).
    let mv_ro = memoryview_make_new(ctx, t_mv, &[b_ro], 0).unwrap();
    let mut info = BufferInfo::empty();
    assert!(!array_get_buffer(ctx, mv_ro, &mut info, 2).unwrap());
    // Writable view + WRITE flag → Ok(true).
    let mv_rw = memoryview_make_new(ctx, t_mv, &[ba_abc], 0).unwrap();
    let mut info = BufferInfo::empty();
    assert!(array_get_buffer(ctx, mv_rw, &mut info, 2).unwrap());
    assert_eq!(info.typecode, 1);
}

#[test]
fn array_iter() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2]));
    let b_abc = fx.heap_bytes(b"abcdef");
    let t_mv = fx.memoryview;
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let it = array_getiter(ctx, a).unwrap();
    assert_eq!(array_iternext(ctx, it).unwrap(), Some(Value::SmallInt(1)));
    assert_eq!(array_iternext(ctx, it).unwrap(), Some(Value::SmallInt(2)));
    assert_eq!(array_iternext(ctx, it).unwrap(), None);
    // View iteration starts at the view offset.
    let mv = memoryview_make_new(ctx, t_mv, &[b_abc], 0).unwrap();
    let it = array_getiter(ctx, mv).unwrap();
    assert_eq!(array_iternext(ctx, it).unwrap(), Some(Value::SmallInt(97)));
    assert_eq!(array_iternext(ctx, it).unwrap(), Some(Value::SmallInt(98)));
}

#[test]
fn array_print_repr() {
    let mut fx = Fixture::new();
    let a = arr(&mut fx, b"i", ints(&[1, 2]));
    let e = arr(&mut fx, b"i", vec![]);
    let b = ba(&mut fx, b"ab");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(print_to_string(ctx, a), "array('i', [1, 2])");
    assert_eq!(print_to_string(ctx, e), "array('i')");
    assert_eq!(print_to_string(ctx, b), "bytearray(b'ab')");
}

#[test]
fn array_type_data_slots() {
    let mut fx = Fixture::new();
    let array_dict = super::tests::empty_dict(&mut fx.arena, &fx.roots);
    let ba_dict = super::tests::empty_dict(&mut fx.arena, &fx.roots);
    let mv_dict = super::tests::empty_dict(&mut fx.arena, &fx.roots);
    let ad = array_type_data(array_dict);
    let bd = bytearray_type_data(ba_dict);
    let md = memoryview_type_data(mv_dict);
    assert_eq!(ad.flags, 0);
    assert_eq!(bd.flags, 0x0408);
    assert_eq!(md.flags, 0x0408);
    // Memoryview has NO print slot.
    assert!(ad.print.is_some());
    assert!(bd.print.is_some());
    assert!(md.print.is_none());
    assert!(ad.unary_op.is_some());
    assert!(ad.binary_op.is_some());
    assert!(ad.subscr.is_some());
    assert!(ad.getiter.is_some());
    assert!(ad.buffer.is_some());
}
