//! Tests for the strings lane (Lane C8a): str/bytes construction,
//! binary_op, subscr, find/replace/join/split/strip, case/is*,
//! hex/fromhex, print, and the str/bytes `TypeData` — all against
//! C0's `Value`/`GcArena` + C6's `obj` dispatch + this lane's
//! `TypeData`. No VM exists yet, so there is no end-to-end
//! differential run; every expected value below was probed on the
//! reference binary (`~/workspace/petitpy-orig/zig-out/bin/petitpy`)
//! or read from the original source (`objstr.zig`, vendor
//! `py/sequence.c`, `py/objslice.c`).
//!
//! The fixture builds the str/bytes types with the REAL `TypeData`
//! (slots filled), so `obj_subscr` / `obj_binary_op` /
//! `obj_len_maybe` / `obj_is_true` dispatch through this lane's
//! slots. The int/bool/none types carry tiny print stubs (standing
//! in for C7/C12's real print slots) so `str(x)` can be tested for
//! non-str args; everything else about those types is plain.

use crate::memory::gc::{CallCtx, GcArena, RootSet};
use crate::objects::obj::*;
use crate::objects::obj_ops::*;
use crate::objects::slots::*;
use crate::objects::strings::objstr_a::*;
use crate::objects::strings::objstr_b::str_count;
use crate::objects::strings::objstr_format::{str_format, str_modulo_format};
use crate::objects::strings::objstr_methods_b::*;
use crate::objects::strings::objstr_methods_c::*;
use crate::objects::value::*;
use crate::pyerr::PyError;
use crate::runtime::qstr::RuntimeQstr;
use crate::vendor::mpz::BigInt;

fn plain_type_data(name: u32, flags: u16, dict: GcRef) -> TypeData {
    TypeData {
        name,
        flags,
        base: None,
        mro: vec![],
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    }
}

pub(crate) fn qid(name: &[u8]) -> u32 {
    static_qstr_id(name).unwrap_or_else(|| panic!("static qstr not found: {:?}", name))
}

/// Test-only print stubs standing in for C7/C12's real print slots
/// (int/bool/none). Only the `PRINT_STR` spelling is needed here.
fn int_print_stub(_ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    if let Value::SmallInt(n) = value {
        let _ = write!(sink, "{}", n);
    }
}

fn bool_print_stub(_ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    if let Value::Bool(b) = value {
        let _ = sink.write_str(if b { "True" } else { "False" });
    }
}

fn none_print_stub(_ctx: &mut CallCtx, _value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    let _ = sink.write_str("None");
}

/// Float print stub (Lane C8b, same class as the int/bool/none
/// stubs above): formats a heap `Float` via Lane FMT's
/// `format_float` (`'g'`, default precision — the vendor float
/// print's format). This stands in for C18's float print slot
/// (registration only); the `{}`-format conversion path that calls
/// it is C8b's and is tested here.
fn float_print_stub(ctx: &mut CallCtx, value: Value, _kind: PrintKind, sink: &mut dyn std::fmt::Write) {
    if let Value::Heap(r) = value {
        if let Obj::Float(f) = ctx.arena.get(r) {
            let _ = sink.write_str(&crate::vendor::formatfloat::format_float(*f, b'g', -1, 0));
        }
    }
}

pub(crate) struct Fx {
    pub(crate) arena: GcArena,
    pub(crate) roots: RootSet,
    pub(crate) types: BuiltinTypes,
    pub(crate) pool: RuntimeQstr,
}

impl Fx {
    pub(crate) fn new() -> Fx {
        let mut arena = GcArena::new();
        let roots = RootSet::new();
        let mk_dict = |arena: &mut GcArena| {
            arena.alloc(
                None,
                Obj::Dict(DictData {
                    map: MapData::new(),
                }),
                &roots,
            )
        };
        let dict = mk_dict(&mut arena);
        let type_ = arena.alloc(
            None,
            Obj::Type(plain_type_data(qid(b"type"), 0, dict)),
            &roots,
        );
        let mk_plain = |arena: &mut GcArena, name: &[u8]| {
            arena.alloc(
                Some(type_),
                Obj::Type(plain_type_data(qid(name), 0, dict)),
                &roots,
            )
        };
        let str_dict = mk_dict(&mut arena);
        let bytes_dict = mk_dict(&mut arena);
        let str_ = arena.alloc(
            Some(type_),
            Obj::Type(str_type_data(qid(b"str"), str_dict)),
            &roots,
        );
        let bytes_ = arena.alloc(
            Some(type_),
            Obj::Type(bytes_type_data(qid(b"bytes"), bytes_dict)),
            &roots,
        );
        let int = mk_plain(&mut arena, b"int");
        let bool_ = mk_plain(&mut arena, b"bool");
        let none_type = mk_plain(&mut arena, b"NoneType");
        let float = mk_plain(&mut arena, b"float");
        // Print stubs for the make_new tests (see the module docs).
        if let Obj::Type(d) = arena.get_mut(int) {
            d.print = Some(int_print_stub);
        }
        if let Obj::Type(d) = arena.get_mut(bool_) {
            d.print = Some(bool_print_stub);
        }
        if let Obj::Type(d) = arena.get_mut(none_type) {
            d.print = Some(none_print_stub);
        }
        if let Obj::Type(d) = arena.get_mut(float) {
            d.print = Some(float_print_stub);
        }
        let types = BuiltinTypes {
            int,
            str_: str_,
            bytes: bytes_,
            bool_,
            none_type,
            float,
            complex: mk_plain(&mut arena, b"complex"),
            tuple: mk_plain(&mut arena, b"tuple"),
            list: mk_plain(&mut arena, b"list"),
            dict: mk_plain(&mut arena, b"dict"),
            type_,
        };
        clear_pending_str_error();
        Fx {
            arena,
            roots,
            types,
            pool: RuntimeQstr::new(),
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool)
    }

    pub(crate) fn s(&mut self, data: &[u8]) -> Value {
        let types = self.types;
        new_str(&mut self.ctx(), &types, data)
    }

    pub(crate) fn b(&mut self, data: &[u8]) -> Value {
        let types = self.types;
        new_bytes(&mut self.ctx(), &types, data)
    }

    pub(crate) fn list(&mut self, items: Vec<Value>) -> Value {
        let types = self.types;
        Value::Heap(self.ctx().alloc(
            Some(types.list),
            Obj::List(ListData { items }),
        ))
    }

    pub(crate) fn tuple(&mut self, items: Vec<Value>) -> Value {
        let types = self.types;
        Value::Heap(self.ctx().alloc(Some(types.tuple), Obj::Tuple(items)))
    }

    pub(crate) fn slice(&mut self, start: Option<Value>, stop: Option<Value>, step: Option<Value>) -> Value {
        let types = self.types;
        let _ = types;
        Value::Heap(self.ctx().alloc(
            None,
            Obj::Slice(SliceData { start, stop, step }),
        ))
    }

    pub(crate) fn range(&mut self, start: i64, stop: i64, step: i64) -> Value {
        let types = self.types;
        let _ = types;
        Value::Heap(self.ctx().alloc(
            None,
            Obj::Range(RangeData { start, stop, step }),
        ))
    }
}

pub(crate) fn bytes_of(fx: &Fx, v: Value) -> Vec<u8> {
    str_data(&fx.arena, v).unwrap().to_vec()
}

pub(crate) fn assert_str(fx: &Fx, v: Value, expected: &[u8]) {
    assert_eq!(bytes_of(fx, v), expected);
}

pub(crate) fn assert_err(e: StrError, exc: StrExc, msg: Option<&str>) {
    assert_eq!(e.exc_type(), Some(exc));
    assert_eq!(e.message(), msg);
}

pub(crate) fn iv(n: i64) -> Value {
    Value::SmallInt(n)
}

// ── constructors ──────────────────────────────────────────────────────

#[test]
fn str_make_new_empty() {
    let mut fx = Fx::new();
    let types = fx.types;
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[], &[]).unwrap();
    assert_eq!(r, Value::Qstr(1));
    assert_str(&fx, r, b"");
}

#[test]
fn str_make_new_case1() {
    let mut fx = Fx::new();
    let types = fx.types;
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[iv(123)], &[]).unwrap();
    assert_str(&fx, r, b"123");
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[Value::None], &[]).unwrap();
    assert_str(&fx, r, b"None");
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[Value::Bool(true)], &[]).unwrap();
    assert_str(&fx, r, b"True");
    let b = fx.b(b"ab");
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[b], &[]).unwrap();
    assert_str(&fx, r, b"b'ab'");
    let s = fx.s(b"hi");
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[s], &[]).unwrap();
    assert_str(&fx, r, b"hi");
}

#[test]
fn str_make_new_bytes_arg() {
    let mut fx = Fx::new();
    let types = fx.types;
    let b = fx.b(b"ab");
    let enc = Value::Qstr(qid(b"utf-8"));
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[b, enc], &[]).unwrap();
    assert_str(&fx, r, b"ab");
    // str('a', 'utf-8') — non-bytes arg0 takes the buffer path.
    let s = fx.s(b"a");
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[s, enc], &[]).unwrap();
    assert_str(&fx, r, b"a");
    // str(123, 'utf-8') — the original crashes here; Rust raises
    // "object with buffer protocol required".
    let e = str_make_new(&mut fx.ctx(), &types, types.str_, &[iv(123), enc], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("object with buffer protocol required"));
}

#[test]
fn str_make_new_arg_count() {
    let mut fx = Fx::new();
    let types = fx.types;
    let a = fx.s(b"a");
    let e = str_make_new(
        &mut fx.ctx(),
        &types,
        types.str_,
        &[a, a, a, a],
        &[],
    )
    .unwrap_err();
    assert_err(e, StrExc::TypeError, Some("argument num/types mismatch"));
}

#[test]
fn str_make_new_kwargs() {
    let mut fx = Fx::new();
    let types = fx.types;
    let a = fx.s(b"a");
    // str('a', errors='strict') — the encoding default is filled.
    let kw = [KwArg {
        name: qid(b"errors"),
        value: fx.s(b"strict"),
    }];
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[a], &kw).unwrap();
    assert_str(&fx, r, b"a");
    // str(object='a') — the keyword fills slot 0.
    let kw = [KwArg {
        name: qid(b"object"),
        value: a,
    }];
    let r = str_make_new(&mut fx.ctx(), &types, types.str_, &[], &kw).unwrap();
    assert_str(&fx, r, b"a");
    // str(encoding='utf-8') — slot 0 has no default: terse.
    let kw = [KwArg {
        name: qid(b"encoding"),
        value: Value::Qstr(qid(b"utf-8")),
    }];
    let e = str_make_new(&mut fx.ctx(), &types, types.str_, &[], &kw).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("argument num/types mismatch"));
    // str('a', object='b') — duplicate: terse.
    let kw = [KwArg {
        name: qid(b"object"),
        value: a,
    }];
    let e = str_make_new(&mut fx.ctx(), &types, types.str_, &[a], &kw).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("argument num/types mismatch"));
    // str('a', foo=1) — unknown keyword: terse.
    let kw = [KwArg {
        name: qid(b"count"),
        value: iv(1),
    }];
    let e = str_make_new(&mut fx.ctx(), &types, types.str_, &[a], &kw).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("argument num/types mismatch"));
}

#[test]
fn bytes_make_new_forms() {
    let mut fx = Fx::new();
    let types = fx.types;
    let r = bytes_make_new(&mut fx.ctx(), &types, &[], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"");
    assert!(is_bytes_value(&fx.arena, r));
    let b = fx.b(b"ab");
    let r = bytes_make_new(&mut fx.ctx(), &types, &[b], &[]).unwrap();
    assert_eq!(r, b); // returned as-is (identity)
    let s = fx.s(b"ab");
    let enc = Value::Qstr(qid(b"utf-8"));
    let r = bytes_make_new(&mut fx.ctx(), &types, &[s, enc], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    assert!(is_bytes_value(&fx.arena, r));
    let r = bytes_make_new(&mut fx.ctx(), &types, &[iv(3)], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"\x00\x00\x00");
    let e = bytes_make_new(&mut fx.ctx(), &types, &[iv(-1)], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, None); // NegLen: no message
    let e = bytes_make_new(&mut fx.ctx(), &types, &[s], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("wrong number of arguments"));
    let e = bytes_make_new(&mut fx.ctx(), &types, &[iv(1), iv(2)], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("wrong number of arguments"));
    let e = bytes_make_new(&mut fx.ctx(), &types, &[Value::Bool(true)], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("object not iterable"));
    let e = bytes_make_new(&mut fx.ctx(), &types, &[Value::SmallInt(0)], &[]).unwrap();
    assert_eq!(bytes_of(&fx, e), b"");
}

#[test]
fn bytes_make_new_iterable() {
    let mut fx = Fx::new();
    let types = fx.types;
    let l = fx.list(vec![iv(1), iv(2), iv(3)]);
    let r = bytes_make_new(&mut fx.ctx(), &types, &[l], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"\x01\x02\x03");
    let l = fx.list(vec![iv(256)]);
    let e = bytes_make_new(&mut fx.ctx(), &types, &[l], &[]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("bytes value out of range"));
    let a1 = fx.s(b"a");
    let l = fx.list(vec![a1]);
    let e = bytes_make_new(&mut fx.ctx(), &types, &[l], &[]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to int"));
    let rg = fx.range(0, 3, 1);
    let r = bytes_make_new(&mut fx.ctx(), &types, &[rg], &[]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"\x00\x01\x02");
}

// ── binary_op ─────────────────────────────────────────────────────────

#[test]
fn binary_op_add() {
    let mut fx = Fx::new();
    let types = fx.types;
    let a = fx.s(b"ab");
    let b = fx.s(b"cd");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Add, a, b).unwrap().unwrap();
    assert_str(&fx, r, b"abcd");
    // Empty lhs of the same type returns rhs itself; empty rhs
    // returns lhs itself.
    let e = fx.s(b"");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Add, e, a).unwrap().unwrap();
    assert_eq!(r, a);
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Add, a, e).unwrap().unwrap();
    assert_eq!(r, a);
    // Bytes lhs + str rhs concatenates through the buffer path.
    let bb = fx.b(b"a");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Add, bb, a);
    assert!(r.is_some());
    assert_eq!(bytes_of(&fx, r.unwrap().unwrap()), b"aab");
    // Str lhs + bytes rhs is not implemented.
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Add, a, bb);
    assert!(r.is_none());
}

#[test]
fn binary_op_multiply() {
    let mut fx = Fx::new();
    let types = fx.types;
    let a = fx.s(b"ab");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Multiply, a, iv(3))
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"ababab");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Multiply, a, iv(0))
        .unwrap()
        .unwrap();
    assert_eq!(r, Value::Qstr(1));
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Multiply, a, iv(-1))
        .unwrap()
        .unwrap();
    assert_eq!(r, Value::Qstr(1));
    let bb = fx.b(b"ab");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Multiply, bb, iv(2))
        .unwrap()
        .unwrap();
    assert_eq!(bytes_of(&fx, r), b"abab");
    // Non-int rhs is not implemented (the VM raises).
    let f = fx.s(b"2.5");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Multiply, a, f);
    assert!(r.is_none());
    // Modulo is C8b's: wired to `str_modulo_format` — `"ab" % "ab"`
    // has no conversion, so the single argument is extra.
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Modulo, a, a);
    assert!(matches!(r, Some(Err(StrError::ExtraArgs))));
}

#[test]
fn binary_op_contains() {
    let mut fx = Fx::new();
    let types = fx.types;
    let hay = fx.s(b"abc");
    let needle = fx.s(b"bc");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Contains, hay, needle)
        .unwrap()
        .unwrap();
    assert_eq!(r, Value::Bool(true));
    let needle = fx.s(b"x");
    let r = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Contains, hay, needle)
        .unwrap()
        .unwrap();
    assert_eq!(r, Value::Bool(false));
    // A non-str rhs raises "can't convert to str implicitly".
    let e = binary_op_inner(&mut fx.ctx(), &types, BinaryOp::Contains, hay, iv(1))
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to str implicitly"));
}

#[test]
fn seq_cmp_bytes_cases() {
    assert!(seq_cmp_bytes(BinaryOp::Equal, b"abc", b"abc"));
    assert!(!seq_cmp_bytes(BinaryOp::Equal, b"abc", b"abd"));
    assert!(!seq_cmp_bytes(BinaryOp::Equal, b"abc", b"ab"));
    assert!(seq_cmp_bytes(BinaryOp::Less, b"a", b"b"));
    assert!(!seq_cmp_bytes(BinaryOp::Less, b"b", b"a"));
    assert!(!seq_cmp_bytes(BinaryOp::Less, b"abc", b"abc"));
    assert!(seq_cmp_bytes(BinaryOp::LessEqual, b"abc", b"abc"));
    assert!(seq_cmp_bytes(BinaryOp::More, b"abc", b"ab"));
    assert!(!seq_cmp_bytes(BinaryOp::More, b"ab", b"abc"));
    assert!(seq_cmp_bytes(BinaryOp::MoreEqual, b"abc", b"abc"));
    assert!(!seq_cmp_bytes(BinaryOp::More, b"abc", b"abc"));
    // Prefix tie: the longer one is more.
    assert!(seq_cmp_bytes(BinaryOp::More, b"abcd", b"abc"));
    assert!(seq_cmp_bytes(BinaryOp::Less, b"abc", b"abcd"));
}

#[test]
fn binary_op_slot_and_pending() {
    let mut fx = Fx::new();
    // Through the frozen slot shape: a value comes back, a specific
    // raise lands in the pending cell, NULL comes back bare.
    let a = fx.s(b"ab");
    let b = fx.s(b"cd");
    let r = str_binary_op(&mut fx.ctx(), BinaryOp::Add, a, b).unwrap();
    assert_str(&fx, r, b"abcd");
    clear_pending_str_error();
    let e = str_binary_op(&mut fx.ctx(), BinaryOp::Contains, a, iv(1)).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
    assert_eq!(
        take_pending_str_error(),
        Some((StrExc::TypeError, Some("can't convert to str implicitly")))
    );
    clear_pending_str_error();
    let bb = fx.b(b"b");
    let e = str_binary_op(&mut fx.ctx(), BinaryOp::Add, a, bb).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
    assert_eq!(take_pending_str_error(), None);
    // Through C6's dispatch: obj_binary_op.
    let types = fx.types;
    let r = obj_binary_op(&mut fx.ctx(), &types, BinaryOp::Add, a, b).unwrap();
    assert_str(&fx, r, b"abcd");
}

// ── subscr ────────────────────────────────────────────────────────────

#[test]
fn subscr_index() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"abc");
    let r = subscr_inner(&mut fx.ctx(), &types, s, iv(1), SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"b");
    let r = subscr_inner(&mut fx.ctx(), &types, s, iv(-1), SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"c");
    let e = subscr_inner(&mut fx.ctx(), &types, s, iv(5), SubscrArg::Get)
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::IndexError, Some("index out of range"));
    let x = fx.s(b"x");
    let e = subscr_inner(&mut fx.ctx(), &types, s, x, SubscrArg::Get)
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::TypeError, Some("indices must be integers"));
    let b = fx.b(b"abc");
    let r = subscr_inner(&mut fx.ctx(), &types, b, iv(0), SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_eq!(r, iv(97));
    // Non-ASCII str: code-point indexing, str-specific errors.
    let u = fx.s("héllo".as_bytes());
    let r = subscr_inner(&mut fx.ctx(), &types, u, iv(1), SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, "é".as_bytes());
    let e = subscr_inner(&mut fx.ctx(), &types, u, iv(9), SubscrArg::Get)
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::IndexError, Some("string index out of range"));
    let e = subscr_inner(&mut fx.ctx(), &types, u, x, SubscrArg::Get)
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::TypeError, Some("string indices must be integers"));
    // Set / delete are not implemented.
    assert!(subscr_inner(&mut fx.ctx(), &types, s, iv(0), SubscrArg::Set(iv(1))).is_none());
    assert!(subscr_inner(&mut fx.ctx(), &types, s, iv(0), SubscrArg::Delete).is_none());
}

#[test]
fn subscr_slice() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"abc");
    let sl = fx.slice(Some(iv(1)), Some(iv(3)), None);
    let r = subscr_inner(&mut fx.ctx(), &types, s, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"bc");
    let sl = fx.slice(None, None, Some(iv(-1)));
    let r = subscr_inner(&mut fx.ctx(), &types, s, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"cba");
    let sl = fx.slice(Some(iv(1)), Some(iv(10)), None);
    let r = subscr_inner(&mut fx.ctx(), &types, s, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, b"bc");
    let sl = fx.slice(None, None, Some(iv(0)));
    let e = subscr_inner(&mut fx.ctx(), &types, s, sl, SubscrArg::Get)
        .unwrap()
        .unwrap_err();
    assert_err(e, StrExc::ValueError, Some("slice step can't be zero"));
    let b = fx.b(b"abc");
    let sl = fx.slice(None, None, Some(iv(-2)));
    let r = subscr_inner(&mut fx.ctx(), &types, b, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_eq!(bytes_of(&fx, r), b"ca");
    // Non-ASCII slice by code point.
    let u = fx.s("héllo".as_bytes());
    let sl = fx.slice(None, None, Some(iv(-1)));
    let r = subscr_inner(&mut fx.ctx(), &types, u, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, "olléh".as_bytes());
    let sl = fx.slice(Some(iv(1)), Some(iv(3)), None);
    let r = subscr_inner(&mut fx.ctx(), &types, u, sl, SubscrArg::Get)
        .unwrap()
        .unwrap();
    assert_str(&fx, r, "él".as_bytes());
}

#[test]
fn subscr_dispatch_and_pending() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"abc");
    // Through C6's obj_subscr: the slot fires.
    let r = obj_subscr(&mut fx.ctx(), &types, s, iv(1), SubscrArg::Get).unwrap();
    assert_str(&fx, r, b"b");
    // Out of range: the dispatcher raises its generic error, but
    // the specific raise is preserved in the pending cell for C13.
    clear_pending_str_error();
    let e = obj_subscr(&mut fx.ctx(), &types, s, iv(5), SubscrArg::Get).unwrap_err();
    assert_eq!(e, ObjError::NotSubscriptable);
    assert_eq!(
        take_pending_str_error(),
        Some((StrExc::IndexError, Some("index out of range")))
    );
}

#[test]
fn unary_op_len_bool() {
    let mut fx = Fx::new();
    let types = fx.types;
    let u = fx.s("héllo".as_bytes());
    // Through C6's obj_len_maybe: the str LEN slot (code points).
    let r = obj_len_maybe(&mut fx.ctx(), &types, u).unwrap();
    assert_eq!(r, iv(5));
    let b = fx.b(b"abc");
    let r = obj_len_maybe(&mut fx.ctx(), &types, b).unwrap();
    assert_eq!(r, iv(3));
    let sa = fx.s(b"a");
    let sb = fx.s(b"");
    assert!(obj_is_true(&mut fx.ctx(), &types, sa).unwrap());
    assert!(!obj_is_true(&mut fx.ctx(), &types, sb).unwrap());
    // Direct slot: other ops are not implemented.
    clear_pending_str_error();
    let e = str_unary_op(&mut fx.ctx(), UnaryOp::Positive, u).unwrap_err();
    assert_eq!(e, PyError::PyRaise);
    assert_eq!(take_pending_str_error(), None);
}

// ── shared helpers for the C8b tests (`objstr_tests_c.rs`,
// `objstr_tests_d.rs`) ──────────────────────────────────────────────
// ── helpers ───────────────────────────────────────────────────────────

pub(crate) fn fmt_pct(fx: &mut Fx, pattern: &[u8], rhs: Value) -> Result<Value, StrError> {
    let types = fx.types;
    let pat = fx.s(pattern);
    str_modulo_format(&mut fx.ctx(), &types, pat, rhs)
}

pub(crate) fn fmt_str(fx: &mut Fx, pattern: &[u8], args: &[Value], kwargs: &[KwArg]) -> Result<Value, StrError> {
    let types = fx.types;
    let mut all = vec![fx.s(pattern)];
    all.extend_from_slice(args);
    str_format(&mut fx.ctx(), &types, &all, kwargs)
}

pub(crate) fn dict_of(fx: &mut Fx, entries: Vec<(Value, Value)>) -> Value {
    Value::Heap(
        fx.ctx()
            .alloc(None, Obj::Dict(DictData { map: MapData::from_pairs(entries) })),
    )
}

pub(crate) fn big_2_pow_70(fx: &mut Fx) -> Value {
    let types = fx.types;
    let mut z = BigInt::zero();
    z.set_from_str(b"1180591620717411303424", false, 10);
    Value::Heap(fx.ctx().alloc(Some(types.int), Obj::Int(z)))
}

pub(crate) fn float_of(fx: &mut Fx, f: f64) -> Value {
    let types = fx.types;
    Value::Heap(fx.ctx().alloc(Some(types.float), Obj::Float(f)))
}

pub(crate) fn list_items(fx: &Fx, v: Value) -> Vec<Vec<u8>> {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::List(l) => l.items.iter().map(|item| bytes_of(fx, *item)).collect(),
            _ => panic!("not a list"),
        },
        _ => panic!("not a heap value"),
    }
}

pub(crate) fn tuple_items(fx: &Fx, v: Value) -> Vec<Vec<u8>> {
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Tuple(items) => items.iter().map(|item| bytes_of(fx, *item)).collect(),
            _ => panic!("not a tuple"),
        },
        _ => panic!("not a heap value"),
    }
}

pub(crate) fn count_val(fx: &mut Fx, args: &[Value]) -> i64 {
    let types = fx.types;
    match str_count(&mut fx.ctx(), &types, args).unwrap() {
        Value::SmallInt(n) => n,
        _ => panic!("count is not a small int"),
    }
}

pub(crate) fn call_bytes(
    fx: &mut Fx,
    f: fn(&mut CallCtx, &BuiltinTypes, &[Value]) -> Result<Value, StrError>,
    args: &[Value],
) -> Vec<u8> {
    let types = fx.types;
    let r = f(&mut fx.ctx(), &types, args).unwrap();
    bytes_of(fx, r)
}
