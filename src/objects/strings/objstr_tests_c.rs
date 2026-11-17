//! Lane C8b tests — `objstr` part 2 + `vstr` (methods).
//!
//! All expected values are probe records from the reference binary
//! (`~/workspace/petitpy-orig/zig-out/bin/petitpy`, 0.9.0); see
//! `scripts/strings_b_result.txt`. Where the reference binary aborts
//! (rc=134) the Rust behaviour is the documented intentional
//! divergence (wrap / saturate / return-self) and the test pins the
//! Rust behaviour, not the crash.
//!
//! Values are always bound in `let` statements before a call that
//! takes `&mut fx.ctx()`: building a value (`fx.s(...)`) and taking
//! the call context borrow `fx` mutably in the same expression.

use super::objstr_tests::{
    assert_err, assert_str, bytes_of, count_val, call_bytes, dict_of, float_of, iv,
    list_items, qid, tuple_items, Fx,
};
use crate::objects::strings::objstr_a::StrExc;
use crate::objects::strings::objstr_b::*;
use crate::objects::strings::objstr_format::str_to_int;
use crate::objects::strings::vstr::{round_alloc, Vstr};
use crate::objects::value::Value;

// ── vstr ──────────────────────────────────────────────────────────────

#[test]
fn vstr_init_and_growth() {
    assert_eq!(round_alloc(0), 8);
    assert_eq!(round_alloc(1), 8);
    assert_eq!(round_alloc(8), 16);
    assert_eq!(round_alloc(9), 16);
    assert_eq!(round_alloc(16), 24);
    let mut v = Vstr::init(1);
    assert_eq!(v.alloc, 1);
    assert_eq!(v.len, 0);
    assert_eq!(v.as_bytes(), b"");
    v.add_byte(b'a').unwrap();
    v.add_byte(b'b').unwrap();
    v.add_byte(b'c').unwrap();
    assert_eq!(v.len, 3);
    // Second byte grows: round_alloc(1 + 1 + 16) = 24.
    assert_eq!(v.alloc, 24);
    assert_eq!(v.as_bytes(), b"abc");
    v.add_strn(b"def").unwrap();
    assert_eq!(v.as_bytes(), b"abcdef");
}

#[test]
fn vstr_add_char_utf8() {
    let mut v = Vstr::new(16);
    v.add_char(0x41).unwrap();
    v.add_char(0xE9).unwrap(); // é → C3 A9
    v.add_char(0x1F600).unwrap(); // 😀 → F0 9F 98 80
    assert_eq!(v.as_bytes(), b"A\xc3\xa9\xf0\x9f\x98\x80");
}

#[test]
fn vstr_fixed_buf_runtime_error() {
    // A fixed buffer with room accepts writes; the error only
    // fires when growth is needed.
    let mut v = Vstr::init_fixed_buf(vec![0u8; 4]);
    assert!(v.fixed_buf);
    v.add_byte(b'a').unwrap();
    v.add_byte(b'b').unwrap();
    v.add_byte(b'c').unwrap();
    v.add_byte(b'd').unwrap();
    assert_eq!(v.as_bytes(), b"abcd");
    let e = v.add_byte(b'e').unwrap_err();
    assert_err(e, StrExc::RuntimeError, None);
    let mut v = Vstr::init_fixed_buf(vec![]);
    let e = v.ensure_extra(1).unwrap_err();
    assert_err(e, StrExc::RuntimeError, None);
}

#[test]
fn vstr_cut_ops() {
    let mut v = Vstr::new(16);
    v.add_strn(b"abcdef").unwrap();
    v.cut_head_bytes(2);
    assert_eq!(v.as_bytes(), b"cdef");
    v.cut_tail_bytes(2);
    assert_eq!(v.as_bytes(), b"cd");
    v.cut_out_bytes(1, 1);
    assert_eq!(v.as_bytes(), b"c");
    v.cut_out_bytes(99, 1);
    assert_eq!(v.as_bytes(), b"c");
    v.cut_tail_bytes(99);
    assert_eq!(v.as_bytes(), b"");
}

#[test]
fn vstr_null_terminated_and_clear() {
    let mut v = Vstr::new(16);
    v.add_strn(b"ab").unwrap();
    assert_eq!(v.null_terminated_str().unwrap(), b"ab\0");
    v.clear();
    assert_eq!(v.as_bytes(), b"");
    // `clear` leaves `len`/`alloc` untouched (C semantics).
    assert_eq!(v.len, 2);
    assert_eq!(v.alloc, 16);
}

#[test]
fn vstr_ins_ops() {
    let mut v = Vstr::new(16);
    v.add_strn(b"ac").unwrap();
    v.ins_byte(1, b'b').unwrap();
    assert_eq!(v.as_bytes(), b"abc");
    v.ins_char(0, b'Z' as u32).unwrap();
    assert_eq!(v.as_bytes(), b"Zabc");
    let v2 = Vstr::init_len(3);
    assert_eq!(v2.len, 3);
    assert_eq!(v2.as_bytes(), b"\0\0\0");
}

// ── splitlines / partition / count / center ───────────────────────────

#[test]
fn splitlines_basic() {
    let mut fx = Fx::new();
    let types = fx.types;
    let s = fx.s(b"a\nb\r\nc\rd");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(
        list_items(&fx, r),
        vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
    );
    let s = fx.s(b"a\nb\r\nc\rd");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s, Value::Bool(true)]).unwrap();
    assert_eq!(
        list_items(&fx, r),
        vec![
            b"a\n".to_vec(),
            b"b\r\n".to_vec(),
            b"c\r".to_vec(),
            b"d".to_vec()
        ]
    );
    // Only \n and \r are boundaries (not \v, \f, \x1c-\x1e, \x85).
    let s = fx.s(b"a\x0bb\x0cc");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(list_items(&fx, r), vec![b"a\x0bb\x0cc".to_vec()]);
    let s = fx.s(b"");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(list_items(&fx, r), Vec::<Vec<u8>>::new());
    let s = fx.s(b"\n");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(list_items(&fx, r), vec![b"".to_vec()]);
    let s = fx.s(b"a\n\nb");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(
        list_items(&fx, r),
        vec![b"a".to_vec(), b"".to_vec(), b"b".to_vec()]
    );
    let s = fx.b(b"a\nb");
    let r = str_splitlines(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(list_items(&fx, r), vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn partition_basic() {
    let mut fx = Fx::new();
    let types = fx.types;
    let a = fx.s(b"a,b,c");
    let sep = fx.s(b",");
    let r = str_partition(&mut fx.ctx(), &types, &[a, sep]).unwrap();
    assert_eq!(
        tuple_items(&fx, r),
        vec![b"a".to_vec(), b",".to_vec(), b"b,c".to_vec()]
    );
    let a = fx.s(b"a,b,c");
    let sep = fx.s(b",");
    let r = str_rpartition(&mut fx.ctx(), &types, &[a, sep]).unwrap();
    assert_eq!(
        tuple_items(&fx, r),
        vec![b"a,b".to_vec(), b",".to_vec(), b"c".to_vec()]
    );
    let a = fx.s(b"abc");
    let sep = fx.s(b"x");
    let r = str_partition(&mut fx.ctx(), &types, &[a, sep]).unwrap();
    assert_eq!(
        tuple_items(&fx, r),
        vec![b"abc".to_vec(), b"".to_vec(), b"".to_vec()]
    );
    let a = fx.s(b"abc");
    let sep = fx.s(b"x");
    let r = str_rpartition(&mut fx.ctx(), &types, &[a, sep]).unwrap();
    assert_eq!(
        tuple_items(&fx, r),
        vec![b"".to_vec(), b"".to_vec(), b"abc".to_vec()]
    );
    let a = fx.s(b"abc");
    let sep = fx.s(b"");
    let e = str_partition(&mut fx.ctx(), &types, &[a, sep]).unwrap_err();
    assert_err(e, StrExc::ValueError, Some("empty separator"));
    let a = fx.s(b"abc");
    let e = str_partition(&mut fx.ctx(), &types, &[a, iv(1)]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("can't convert to str implicitly"));
    let a = fx.b(b"a,b");
    let sep = fx.b(b",");
    let r = str_partition(&mut fx.ctx(), &types, &[a, sep]).unwrap();
    assert_eq!(
        tuple_items(&fx, r),
        vec![b"a".to_vec(), b",".to_vec(), b"b".to_vec()]
    );
}

#[test]
fn count_basic() {
    let mut fx = Fx::new();
    let a = fx.s(b"aaa");
    let sub = fx.s(b"aa");
    assert_eq!(count_val(&mut fx, &[a, sub]), 1);
    let a = fx.s(b"aaa");
    let sub = fx.s(b"");
    assert_eq!(count_val(&mut fx, &[a, sub]), 4);
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(1)]), 1);
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(5)]), 0);
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(-1)]), 0);
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(0), iv(2)]), 1);
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(1), iv(2)]), 1);
    let a = fx.s(b"abc");
    let sub = fx.s(b"");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(0), iv(0)]), 1);
    // UTF-8 charlen for the empty needle (6 = 5 chars + 1).
    let a = fx.s("héllo".as_bytes());
    let sub = fx.s(b"");
    assert_eq!(count_val(&mut fx, &[a, sub]), 6);
    // Divergence: the original aborts here (pointer underflow,
    // `objstr.zig:2238`); Rust saturates the slice to empty, and
    // the empty needle counts the single empty position in it → 1.
    let a = fx.s(b"abc");
    let sub = fx.s(b"");
    assert_eq!(count_val(&mut fx, &[a, sub, iv(5), iv(1)]), 1);
    let types = fx.types;
    let a = fx.s(b"abc");
    let sub = fx.s(b"b");
    let f = float_of(&mut fx, 1.5);
    let e = str_count(&mut fx.ctx(), &types, &[a, sub, f]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("string indices must be integers"));
}

#[test]
fn center_ljust_rjust() {
    let mut fx = Fx::new();
    let a = fx.s(b"ab");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(5)]), b" ab  ");
    let a = fx.s(b"ab");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(6)]), b"  ab  ");
    let a = fx.s(b"ab");
    let fill = fx.s(b"*");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(5), fill]), b"*ab**");
    let a = fx.s(b"ab");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(2)]), b"ab");
    // Negative width: the original aborts; Rust returns self.
    let a = fx.s(b"ab");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(-1)]), b"ab");
    // Width counts CHARS (é is 1 char, 2 bytes): pad 3 → left 1, right 2.
    let a = fx.s("é".as_bytes());
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(4)]), " é  ".as_bytes());
    let a = fx.s(b"ab");
    assert_eq!(call_bytes(&mut fx, str_ljust, &[a, iv(5)]), b"ab   ");
    let a = fx.s(b"ab");
    let fill = fx.s(b"0");
    assert_eq!(call_bytes(&mut fx, str_rjust, &[a, iv(5), fill]), b"000ab");
    let types = fx.types;
    let a = fx.s(b"ab");
    let fill = fx.s(b"xy");
    let e = str_center(&mut fx.ctx(), &types, &[a, iv(5), fill]).unwrap_err();
    assert_err(e, StrExc::TypeError, Some("fillchar must be 1 character"));
    let a = fx.b(b"ab");
    assert_eq!(call_bytes(&mut fx, str_center, &[a, iv(5)]), b" ab  ");
}

#[test]
fn encode_decode() {
    let mut fx = Fx::new();
    let types = fx.types;
    let b = fx.b(b"ab");
    let r = bytes_decode(&mut fx.ctx(), &types, &[b]).unwrap();
    assert_str(&fx, r, b"ab");
    let s = fx.s(b"ab");
    let r = str_encode(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    // The encoding/errors args are parsed but NEVER used.
    let b = fx.b(b"\xff");
    let r = bytes_decode(&mut fx.ctx(), &types, &[b]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"\xff");
    let s = fx.s(b"ab");
    let enc = fx.s(b"utf-16");
    let r = str_encode(&mut fx.ctx(), &types, &[s, enc]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"ab");
    let b = fx.b(b"ab");
    let enc = fx.s(b"utf-8");
    let err = fx.s(b"bogus");
    let r = bytes_decode(&mut fx.ctx(), &types, &[b, enc, err]).unwrap();
    assert_str(&fx, r, b"ab");
    let s = fx.s("héllo".as_bytes());
    let r = str_encode(&mut fx.ctx(), &types, &[s]).unwrap();
    assert_eq!(bytes_of(&fx, r), b"h\xc3\xa9llo");
}

// ── constructors / getters ────────────────────────────────────────────

#[test]
fn constructors_and_getters() {
    let mut fx = Fx::new();
    let types = fx.types;
    // `new_str_copy` is always a fresh heap object (never interned).
    let c = new_str_copy(&mut fx.ctx(), &types, types.str_, b"str");
    assert!(matches!(c, Value::Heap(_)));
    assert_eq!(bytes_of(&fx, c), b"str");
    let c = new_str_copy(&mut fx.ctx(), &types, types.bytes, b"ab");
    assert!(matches!(c, Value::Heap(_)));
    assert_eq!(bytes_of(&fx, c), b"ab");
    // `new_str_from_cstr` truncates at the first NUL.
    let c = new_str_from_cstr(&mut fx.ctx(), &types, b"ab\0cd");
    assert_str(&fx, c, b"ab");
    // `new_str_type_from_vstr` interns static content and empties
    // the vstr (`alloc` 0).
    let mut v = Vstr::new(16);
    v.add_strn(b"str").unwrap();
    let s = new_str_type_from_vstr(&mut fx.ctx(), &types, types.str_, &mut v);
    assert_eq!(s, Value::Qstr(qid(b"str")));
    assert_eq!(v.alloc, 0);
    let mut v = Vstr::new(16);
    v.add_strn(b"zzq").unwrap();
    let s = new_str_from_vstr(&mut fx.ctx(), &types, &mut v);
    assert!(matches!(s, Value::Heap(_)));
    assert_eq!(bytes_of(&fx, s), b"zzq");
    assert_eq!(v.alloc, 0);
    let mut v = Vstr::new(16);
    v.add_strn(b"ab").unwrap();
    let b = new_bytes_from_vstr(&mut fx.ctx(), &types, &mut v);
    assert_eq!(bytes_of(&fx, b), b"ab");
    // `str_equal` does NOT distinguish str from bytes.
    let a = fx.s(b"a");
    let b = fx.b(b"a");
    assert!(str_equal(&fx.arena, &fx.pool, a, b));
    let a = fx.s(b"a");
    let b = fx.s(b"a");
    assert!(str_equal(&fx.arena, &fx.pool, a, b));
    let a = fx.s(b"a");
    let b = fx.s(b"b");
    assert!(!str_equal(&fx.arena, &fx.pool, a, b));
    let a = fx.s(b"a");
    assert!(!str_equal(&fx.arena, &fx.pool, a, iv(1)));
    // `get_qstr` / `get_str` / `get_data`.
    assert_eq!(get_qstr(&fx.arena, &types, Value::Qstr(5)), Some(5));
    let s = fx.s(b"str");
    assert_eq!(get_qstr(&fx.arena, &types, s), Some(qid(b"str")));
    let s = fx.s(b"zzq");
    assert_eq!(get_qstr(&fx.arena, &types, s), None);
    assert_eq!(get_qstr(&fx.arena, &types, iv(1)), None);
    let s = fx.s(b"ab");
    assert_eq!(get_str(&fx.arena, s), Some(b"ab".as_slice()));
    let b = fx.b(b"ab");
    assert_eq!(get_data(&fx.arena, b), Some(b"ab".as_slice()));
    assert_eq!(get_str(&fx.arena, iv(1)), None);
    // `intern` / `intern_checked` / `set_str_data`.
    let s = fx.s(b"ab");
    let s = intern(&mut fx.ctx(), &types, s);
    assert_eq!(bytes_of(&fx, s), b"ab");
    assert_eq!(intern_checked(&mut fx.ctx(), &types, iv(1)), Value::Qstr(0));
    let h = fx.s(b"zzq");
    if let Value::Heap(r) = h {
        set_str_data(&mut fx.ctx(), r, b"zzr");
        assert_eq!(bytes_of(&fx, h), b"zzr");
    } else {
        panic!("expected a heap str");
    }
}

#[test]
fn str_to_int_scan() {
    let mut n: i32 = -1;
    assert_eq!(str_to_int(b"42x", 0, &mut n), 2);
    assert_eq!(n, 42);
    let mut n: i32 = -1;
    assert_eq!(str_to_int(b"x42", 0, &mut n), 0);
    assert_eq!(n, -1);
    let mut n: i32 = -1;
    assert_eq!(str_to_int(b"05d", 0, &mut n), 2);
    assert_eq!(n, 5);
}

// ── method tables ─────────────────────────────────────────────────────

#[test]
fn method_tables() {
    // The shared table has 38 entries; str = 5..38 (33), bytes =
    // 2..37 (35). (The brief's 32/34 counts were miscounts.)
    assert_eq!(STR_METHODS.len(), 33);
    assert_eq!(BYTES_METHODS.len(), 35);
    let str_names: Vec<&str> = STR_METHODS.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        str_names,
        vec![
            "find", "rfind", "index", "rindex", "join", "split", "splitlines", "rsplit",
            "startswith", "endswith", "strip", "lstrip", "rstrip", "format", "replace",
            "count", "partition", "rpartition", "center", "ljust", "rjust", "lower",
            "upper", "capitalize", "title", "zfill", "isspace", "isalpha", "isdigit",
            "isupper", "islower", "isalnum", "encode"
        ]
    );
    let bytes_names: Vec<&str> = BYTES_METHODS.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        bytes_names,
        vec![
            "hex", "fromhex", "decode", "find", "rfind", "index", "rindex", "join",
            "split", "splitlines", "rsplit", "startswith", "endswith", "strip", "lstrip",
            "rstrip", "format", "replace", "count", "partition", "rpartition", "center",
            "ljust", "rjust", "lower", "upper", "capitalize", "title", "zfill",
            "isspace", "isalpha", "isdigit", "isupper", "islower", "isalnum"
        ]
    );
    assert!(lookup_str_method("splitlines").is_some());
    assert!(lookup_str_method("encode").is_some());
    assert!(lookup_str_method("decode").is_none());
    assert!(lookup_bytes_method("decode").is_some());
    assert!(lookup_bytes_method("encode").is_none());
    assert!(lookup_str_method("nosuch").is_none());
    // Spot-call through the table.
    let mut fx = Fx::new();
    let f = lookup_str_method("splitlines").unwrap();
    let s = fx.s(b"a\nb");
    let r = f(&mut fx.ctx(), &[s]).unwrap();
    assert_eq!(list_items(&fx, r), vec![b"a".to_vec(), b"b".to_vec()]);
    let f = lookup_str_method("count").unwrap();
    let a = fx.s(b"aaa");
    let sub = fx.s(b"aa");
    let r = f(&mut fx.ctx(), &[a, sub]).unwrap();
    assert_eq!(r, Value::SmallInt(1));
}

#[test]
fn dict_get_helper() {
    let mut fx = Fx::new();
    let d = dict_of(&mut fx, vec![(Value::Qstr(qid(b"key")), iv(1))]);
    let r = dict_get(&mut fx.ctx(), d, Value::Qstr(qid(b"key")));
    assert_eq!(r, Some(iv(1)));
    let r = dict_get(&mut fx.ctx(), d, Value::Qstr(qid(b"value")));
    assert_eq!(r, None);
    // Non-static keys are heap strs and go through the hash path.
    let kx = fx.s(b"x");
    let d = dict_of(&mut fx, vec![(kx, iv(2))]);
    let kx = fx.s(b"x");
    let r = dict_get(&mut fx.ctx(), d, kx);
    assert_eq!(r, Some(iv(2)));
    let r = dict_get(&mut fx.ctx(), iv(1), Value::Qstr(qid(b"key")));
    assert_eq!(r, None);
}

