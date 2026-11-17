//! Lane C16 stream-protocol tests, part 1: `read` /
//! `readall` (binary + text branch) and protocol resolution.
//! The fixture (fake backends + `Sfx`) lives in
//! `stream_fixture.rs`; part 2 is `stream_tests_b.rs`.

use super::stream_fixture::{call_method, Sfx};
use crate::objects::value::Value;
use crate::pyerr::PyError;

// ── read / readall ───────────────────────────────────────────────────────

#[test]
fn stream_readall_binary() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"hello", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st]).expect("readall")
    };
    assert_eq!(s.as_bytes(v), b"hello");
}

#[test]
fn stream_readall_text() {
    let mut s = Sfx::new();
    let st = s.stream(s.text_type, "héllo".as_bytes(), 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st]).expect("readall")
    };
    assert_eq!(s.as_bytes(v), "héllo".as_bytes());
}

#[test]
fn stream_readall_trickle_chunks() {
    // 1-byte backend reads: readall's short-read path (the
    // next chunk shrinks) must still return everything.
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 1);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st]).expect("readall")
    };
    assert_eq!(s.as_bytes(v), b"0123456789");
}

#[test]
fn stream_read_fixed_and_minus_one() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abcde", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(3)]).expect("read(3)")
    };
    assert_eq!(s.as_bytes(v), b"abc");
    // The cursor advanced: the rest is still there.
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(-1)]).expect("read(-1)")
    };
    assert_eq!(s.as_bytes(v), b"de");
}

#[test]
fn stream_read_negative_size_is_memory_error() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    let r = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(-5)])
    };
    assert!(matches!(r, Err(PyError::OutOfMemory)));
}

#[test]
fn stream_read_zero_is_empty() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(0)]).expect("read(0)")
    };
    assert_eq!(s.as_bytes(v), b"");
    let st = s.stream(s.text_type, b"abc", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(0)]).expect("text read(0)")
    };
    assert_eq!(s.as_bytes(v), b"");
}

#[test]
fn stream_read1_once_with_trickle() {
    // ONCE: one backend call only, even though more is left.
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 2);
    let v = {
        let mut ctx = s.ctx();
        call_method("read1", &mut ctx, &[st, Value::SmallInt(10)]).expect("read1")
    };
    assert_eq!(s.as_bytes(v), b"01");
    // The looped read drains the rest.
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(10)]).expect("read")
    };
    assert_eq!(s.as_bytes(v), b"23456789");
}

// ── text branch ──────────────────────────────────────────────────────────

#[test]
fn stream_read_text_counts_chars_not_bytes() {
    let mut s = Sfx::new();
    // "aébc": a=1 byte, é=2 bytes, b/c=1 byte each.
    let st = s.stream(s.text_type, "aébc".as_bytes(), 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(3)]).expect("read(3)")
    };
    assert_eq!(s.as_bytes(v), "aéb".as_bytes());
}

#[test]
fn stream_read_text_split_multibyte_char() {
    // 1-byte backend reads: the incomplete-char path must
    // request exactly the missing bytes, never over-read.
    let mut s = Sfx::new();
    let st = s.stream(s.text_type, "aé".as_bytes(), 1);
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st, Value::SmallInt(2)]).expect("read(2)")
    };
    assert_eq!(s.as_bytes(v), "aé".as_bytes());
    // Nothing was over-read: the stream is exhausted.
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st]).expect("readall")
    };
    assert_eq!(s.as_bytes(v), b"");
}

#[test]
fn stream_read1_text_ignores_once() {
    // The text branch passes MP_STREAM_RW_READ, not the
    // caller's flags — read1 loops exactly like read.
    let mut s = Sfx::new();
    let st = s.stream(s.text_type, "aébc".as_bytes(), 1);
    let v = {
        let mut ctx = s.ctx();
        call_method("read1", &mut ctx, &[st, Value::SmallInt(3)]).expect("read1(3)")
    };
    assert_eq!(s.as_bytes(v), "aéb".as_bytes());
}

