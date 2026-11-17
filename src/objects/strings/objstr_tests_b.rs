//! Tests for the strings lane (Lane C8a), part 2: find/replace/join/
//! split/strip, case/is*, hex/fromhex, print, `TypeData`, iterators.
//! The fixture and helpers live in `objstr_tests.rs` (same crate,
//! `#[cfg(test)]`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::*;
use crate::objects::slots::*;
use crate::objects::strings::objstr_a::*;
use crate::objects::strings::objstr_iters::*;
use crate::objects::strings::objstr_methods_a::*;
use crate::objects::strings::objstr_methods_b::*;
use crate::objects::strings::objstr_tests::{assert_err, assert_str, bytes_of, iv, qid, Fx};
use crate::objects::value::*;

// ── find / startswith ─────────────────────────────────────────────────

#[test]
fn find_family() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"abc");
    let bc = fx.s(b"bc");
    let r = str_find(&mut fx.ctx(), &types, &[s, bc]).unwrap();
    assert_eq!(r, iv(1));
    let z = fx.s(b"z");
    let r = str_find(&mut fx.ctx(), &types, &[s, z]).unwrap();
    assert_eq!(r, iv(-1));
    let e = str_index(&mut fx.ctx(), &types, &[s, z]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("substring not found"));
    let u = fx.s("héllo".as_bytes());
    let e_acute = fx.s("é".as_bytes());
    let r = str_find(&mut fx.ctx(), &types, &[u, e_acute]).unwrap();
    assert_eq!(r, iv(1));
    let empty = fx.s(b"");
    let r = str_rfind(&mut fx.ctx(), &types, &[s, empty]).unwrap();
    assert_eq!(r, iv(3));
    let b1 = fx.s(b"b");
    let r = str_find(
        &mut fx.ctx(),
        &types,
        &[s, b1, iv(2)],
    )
    .unwrap();
    assert_eq!(r, iv(-1)); // "b" only occurs at index 1
    let bc1 = fx.s(b"bc");
    let r = str_find(&mut fx.ctx(), &types, &[s, bc1, iv(1)]).unwrap();
    assert_eq!(r, iv(1));
    let c1 = fx.s(b"c");
    let r = str_find(&mut fx.ctx(), &types, &[s, c1, iv(2)]).unwrap();
    assert_eq!(r, iv(2));
    let e = str_find(&mut fx.ctx(), &types, &[s, iv(1)]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to str implicitly"));
}

#[test]
fn startswith_endswith() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"abc");
    let a = fx.s(b"a");
    let r = str_startswith(&mut fx.ctx(), &types, &[s, a]).unwrap();
    assert_eq!(r, Value::Bool(true));
    let ab = fx.b(b"a");
    let r = str_startswith(&mut fx.ctx(), &types, &[s, ab]).unwrap();
    assert_eq!(r, Value::Bool(true));
    let x = fx.s(b"x");
    let t = fx.tuple(vec![x, a]);
    let r = str_startswith(&mut fx.ctx(), &types, &[s, t]).unwrap();
    assert_eq!(r, Value::Bool(true));
    let c = fx.s(b"c");
    let r = str_endswith(&mut fx.ctx(), &types, &[s, c, iv(0), iv(2)]).unwrap();
    assert_eq!(r, Value::Bool(false));
    let empty = fx.s(b"");
    let r = str_startswith(&mut fx.ctx(), &types, &[s, empty, iv(2)]).unwrap();
    assert_eq!(r, Value::Bool(true));
}

// ── replace / join / split / strip ────────────────────────────────────

#[test]
fn replace_cases() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"aaa");
    let aa = fx.s(b"aa");
    let b1 = fx.s(b"b");
    let r = str_replace(&mut fx.ctx(), &types, &[s, aa, b1]).unwrap();
    assert_str(&fx, r, b"ba");
    let s = fx.s(b"ab");
    let empty = fx.s(b"");
    let big_x = fx.s(b"X");
    let r = str_replace(&mut fx.ctx(), &types, &[s, empty, big_x]).unwrap();
    assert_str(&fx, r, b"XaXbX");
    let a1 = fx.s(b"a");
    let big_z = fx.s(b"Z");
    let r = str_replace(
        &mut fx.ctx(),
        &types,
        &[s, a1, big_z, iv(0)],
    )
    .unwrap();
    assert_eq!(r, s); // count 0 returns self itself
    let z = fx.s(b"z");
    let r = str_replace(
        &mut fx.ctx(),
        &types,
        &[s, z, big_z],
    )
    .unwrap();
    assert_eq!(r, s); // no occurrence returns self itself
    let r = str_replace(
        &mut fx.ctx(),
        &types,
        &[s, a1, big_z, iv(-1)],
    )
    .unwrap();
    assert_str(&fx, r, b"Zb");
}

#[test]
fn join_cases() {
    let mut fx = Fx::new();
    let types = fx.types;
    let sep = fx.s(b",");
    let a1 = fx.s(b"a");
    let b1 = fx.s(b"b");
    let l = fx.list(vec![a1, b1]);
    let r = str_join(&mut fx.ctx(), &types, &[sep, l]).unwrap();
    assert_str(&fx, r, b"a,b");
    let ab = fx.s(b"ab");
    let r = str_join(&mut fx.ctx(), &types, &[sep, ab]).unwrap();
    assert_str(&fx, r, b"a,b");
    let l = fx.list(vec![iv(1)]);
    let e = str_join(&mut fx.ctx(), &types, &[sep, l]).unwrap_err();
    assert_err(
        e,
        StrExc::TypeError,
        Some("join expects a list of str/bytes objects consistent with self object"),
    );
    let e = fx.s(b"");
    let l = fx.list(vec![]);
    let r = str_join(&mut fx.ctx(), &types, &[e, l]).unwrap();
    assert_str(&fx, r, b"");
    let bb = fx.b(b",");
    let ba = fx.b(b"a");
    let b2 = fx.b(b"b");
    let l = fx.list(vec![ba, b2]);
    let r = str_join(&mut fx.ctx(), &types, &[bb, l]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"a,b");
}

#[test]
fn split_cases() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"a,b,");
    let comma = fx.s(b",");
    let r = str_split(&mut fx.ctx(), &types, &[s, comma]).unwrap();
    let items = match fx.arena.get(r.as_heap().unwrap()) {
        Obj::List(l) => l.items.clone(),
        _ => panic!("split did not return a list"),
    };
    assert_eq!(items.len(), 3);
    assert_str(&fx, items[0], b"a");
    assert_str(&fx, items[2], b"");
    let s = fx.s(b" a b c ");
    let r = str_split(&mut fx.ctx(), &types, &[s, Value::None, iv(1)]).unwrap();
    let items = match fx.arena.get(r.as_heap().unwrap()) {
        Obj::List(l) => l.items.clone(),
        _ => panic!("split did not return a list"),
    };
    assert_eq!(items.len(), 2);
    assert_str(&fx, items[0], b"a");
    assert_str(&fx, items[1], b"b c ");
    let empty = fx.s(b"");
    let e = str_split(&mut fx.ctx(), &types, &[s, empty]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("empty separator"));
    let s = fx.s(b"a-b-c");
    let dash = fx.s(b"-");
    let r = str_rsplit(&mut fx.ctx(), &types, &[s, dash, iv(1)]).unwrap();
    let items = match fx.arena.get(r.as_heap().unwrap()) {
        Obj::List(l) => l.items.clone(),
        _ => panic!("rsplit did not return a list"),
    };
    assert_eq!(items.len(), 2);
    assert_str(&fx, items[0], b"a-b");
    assert_str(&fx, items[1], b"c");
    let e = str_rsplit(&mut fx.ctx(), &types, &[s, Value::None, iv(1)]).unwrap_err();
    assert_err(e, StrExc::NotImplementedError, Some("rsplit(None,n)"));
}

#[test]
fn strip_cases() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"xxabcxx");
    let x = fx.s(b"x");
    let r = str_strip(&mut fx.ctx(), &types, &[s, x]).unwrap();
    assert_str(&fx, r, b"abc");
    let s = fx.s(b"  a  ");
    let r = str_strip(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, b"a");
    let s = fx.s(b"abc");
    let empty = fx.s(b"");
    let r = str_lstrip(&mut fx.ctx(), &types, &[s, empty]).unwrap();
    assert_eq!(r, s); // empty chars strips nothing: self itself
    let s = fx.s(b"");
    let r = str_strip(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, b"");
    let b = fx.b(b" x ");
    let r = str_strip(&mut fx.ctx(), &types, &[b]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"x");
}

// ── case / predicates / zfill ─────────────────────────────────────────

#[test]
fn case_methods() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"hELLO wORLD");
    let r = str_title(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, b"Hello World");
    let s = fx.s(b"aBc");
    let r = str_capitalize(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, b"Abc");
    let s = fx.s("ß".as_bytes());
    let r = str_upper(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, "ß".as_bytes()); // byte-wise: non-ASCII passes through
    let s = fx.s(b"AbC");
    let r = str_lower(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_str(&fx, r, b"abc");
}

#[test]
fn zfill_cases() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"-42");
    let r = str_zfill(&mut fx.ctx(), &types, &[s, iv(5)]).unwrap();
    assert_str(&fx, r, b"-0042");
    let s = fx.s(b"42");
    let r = str_zfill(&mut fx.ctx(), &types, &[s, iv(2)]).unwrap();
    assert_eq!(r, s); // already long enough: self itself
    let s = fx.s(b"abc");
    let r = str_zfill(&mut fx.ctx(), &types, &[s, iv(-1)]).unwrap();
    assert_eq!(r, s); // negative width: the original crashes; Rust returns self
    let x = fx.s(b"x");
    let e = str_zfill(&mut fx.ctx(), &types, &[s, x]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to int"));
}

#[test]
fn predicates() {
    let mut fx = Fx::new();
    let types = fx.types;
    let t = |fx: &mut Fx, types: &BuiltinTypes, f: fn(&mut CallCtx, &BuiltinTypes, &[Value]) -> Result<Value, StrError>, s: &[u8]| {
        let v = fx.s(s);
        f(&mut fx.ctx(), types, &[v]).unwrap() == Value::Bool(true)
    };
    assert!(t(&mut fx, &types, str_isupper, b"AB1"));
    assert!(!t(&mut fx, &types, str_isupper, b"123"));
    assert!(t(&mut fx, &types, str_islower, b"ab1"));
    assert!(!t(&mut fx, &types, str_islower, b"AB"));
    assert!(t(&mut fx, &types, str_isalpha, b"abc"));
    assert!(!t(&mut fx, &types, str_isalpha, "é".as_bytes()));
    assert!(t(&mut fx, &types, str_isdigit, b"123"));
    assert!(!t(&mut fx, &types, str_isdigit, b""));
    assert!(t(&mut fx, &types, str_isalnum, b"a1"));
    assert!(t(&mut fx, &types, str_isspace, b"\t"));
    assert!(!t(&mut fx, &types, str_isspace, b""));
}

// ── hex / fromhex ─────────────────────────────────────────────────────

#[test]
fn hex_fromhex() {
    let mut fx = Fx::new();
    let types = fx.types;
    let b = fx.b(b"ab");
    let r = bytes_hex(&mut fx.ctx(), &types, &[b]).unwrap();
    assert_str(&fx, r, b"6162");
    let sp = fx.s(b" ");
    let r = bytes_hex(&mut fx.ctx(), &types, &[b, sp]).unwrap();
    assert_str(&fx, r, b"61 62");
    let e = fx.b(b"");
    let r = bytes_hex(&mut fx.ctx(), &types, &[e]).unwrap();
    assert!(is_bytes_value(&fx.arena, r)); // b''.hex() is b'', not ''
    assert_eq!(bytes_of(&fx, r), b"");
    let empty = fx.s(b"");
    let r = bytes_hex(&mut fx.ctx(), &types, &[b, empty]).unwrap();
    assert_str(&fx, r, b"61\x0062");
    let cls = Value::Heap(types.bytes);
    let data = fx.s(b"61 62");
    let r = bytes_fromhex(&mut fx.ctx(), &types, &[cls, data]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    let data = fx.s(b"zz");
    let e = bytes_fromhex(&mut fx.ctx(), &types, &[cls, data]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("non-hex digit"));
    let data = fx.s(b"a");
    let e = bytes_fromhex(&mut fx.ctx(), &types, &[cls, data]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("non-hex digit"));
    let data = fx.s(b"a b");
    let e = bytes_fromhex(&mut fx.ctx(), &types, &[cls, data]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("non-hex digit"));
    let data = fx.s(b"61:62");
    let colon = fx.s(b":");
    let r = bytes_fromhex(
        &mut fx.ctx(),
        &types,
        &[cls, data, colon],
    )
    .unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    let data = fx.s(b"6162");
    let two_colon = fx.s(b"::");
    let e = bytes_fromhex(
        &mut fx.ctx(),
        &types,
        &[cls, data, two_colon],
    )
    .unwrap_err();
    assert_err(e, StrExc::ValueError, Some("sep must be a single character"));
}

// ── print ─────────────────────────────────────────────────────────────

#[test]
fn print_str_repr() {
    let mut fx = Fx::new();
    let s = fx.s(b"hi");
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Str, &mut out);
    assert_eq!(out, "hi");
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Repr, &mut out);
    assert_eq!(out, "'hi'");
    let b = fx.b(b"hi");
    let mut out = String::new();
    str_print(&mut fx.ctx(), b, PrintKind::Str, &mut out);
    assert_eq!(out, "b'hi'");
    let mut out = String::new();
    str_print(&mut fx.ctx(), b, PrintKind::Repr, &mut out);
    assert_eq!(out, "b'hi'");
    // Quoting: a single quote with no double quote switches to `"`.
    let s = fx.s(b"a'b");
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Repr, &mut out);
    assert_eq!(out, "\"a'b\"");
    let s = fx.s(b"a\"b");
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Repr, &mut out);
    assert_eq!(out, "'a\"b'");
    // Escapes: \n, backslash, control bytes.
    let s = fx.s(b"a\nb\\c\x01d");
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Repr, &mut out);
    assert_eq!(out, "'a\\nb\\\\c\\x01d'");
    // Non-ASCII str bytes pass through raw in a str repr.
    let s = fx.s("héllo".as_bytes());
    let mut out = String::new();
    str_print(&mut fx.ctx(), s, PrintKind::Repr, &mut out);
    assert_eq!(out, "'héllo'");
    // ...but in bytes they escape.
    let b = fx.b("héllo".as_bytes());
    let mut out = String::new();
    str_print(&mut fx.ctx(), b, PrintKind::Repr, &mut out);
    assert_eq!(out, "b'h\\xc3\\xa9llo'");
}

// ── TypeData ──────────────────────────────────────────────────────────

#[test]
fn type_data_slots() {
    let mut fx = Fx::new();
    let types = fx.types;
    let dict = fx.arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::new(),
        }),
        &fx.roots,
    );
    let d = str_type_data(qid(b"str"), dict);
    assert_eq!(d.name, qid(b"str"));
    assert_eq!(d.flags, 0);
    assert!(d.unary_op.is_some());
    assert!(d.binary_op.is_some());
    assert!(d.subscr.is_some());
    assert!(d.buffer.is_some());
    assert!(d.print.is_some());
    assert!(d.call.is_none());
    assert!(d.getiter.is_none());
    let d = bytes_type_data(qid(b"bytes"), dict);
    assert_eq!(d.name, qid(b"bytes"));
    assert!(d.unary_op.is_none());
    assert!(d.binary_op.is_some());
    assert!(d.subscr.is_some());
    assert!(d.buffer.is_some());
    assert!(d.print.is_some());
    let _ = types;
}

// ── iterators ─────────────────────────────────────────────────────────

#[test]
fn str_bytes_iterators() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s("héllo".as_bytes());
    let it = new_str_iterator(&mut fx.ctx(), None, s);
    let mut chars: Vec<Vec<u8>> = vec![];
    loop {
        match str_iter_next(&mut fx.ctx(), &types, it) {
            Some(v) => chars.push(bytes_of(&fx, v)),
            None => break,
        }
    }
    assert_eq!(chars, vec![b"h".to_vec(), "é".as_bytes().to_vec(), b"l".to_vec(), b"l".to_vec(), b"o".to_vec()]);
    let b = fx.b(b"ab");
    let it = new_bytes_iterator(&mut fx.ctx(), None, b);
    let mut vals: Vec<i64> = vec![];
    loop {
        match bytes_iter_next(&mut fx.ctx(), it) {
            Some(Value::SmallInt(n)) => vals.push(n),
            Some(_) => panic!("bytes iterator yielded a non-int"),
            None => break,
        }
    }
    assert_eq!(vals, vec![97, 98]);
}
