//! Direct tests for `builtin_fns::input_read` (D1): the
//! `input()` core over a `Cursor` reader / `Vec` writer, so no
//! stdio is involved. The VM-level `input()` builtin itself is
//! the same core over fd 0/1 and is not exercised here (no
//! terminal in the test harness).
//!
//! The `Fixture` is the containers test fixture
//! (`objects::containers::tests`): a real arena + pool + the
//! builtin types, so `new_str` and the recorded raises behave
//! as in the VM.

use std::io::Cursor;

use crate::pyerr::PyError;
use crate::objects::containers::tests::Fixture;
use crate::objects::value::{Obj, Value};
use crate::runtime::builtin_fns::input_read;

fn read_str(fx: &mut Fixture, v: Value) -> String {
    match v {
        Value::Qstr(q) => String::from_utf8_lossy(fx.pool.qstr_str(q as u16)).into_owned(),
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
            other => panic!("not a str: {other:?}"),
        },
        other => panic!("not a str: {other:?}"),
    }
}

#[test]
fn input_read_line_and_prompt() {
    let mut fx = Fixture::new();
    let mut reader = Cursor::new(b"hello\n".to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let v = {
        let mut ctx = fx.ctx();
        input_read(&mut ctx, &mut reader, &mut writer, Some(b">>> "))
    }
    .expect("a complete line reads fine");
    assert_eq!(read_str(&mut fx, v), "hello");
    assert_eq!(writer, b">>> ");
}

#[test]
fn input_read_no_trailing_newline() {
    let mut fx = Fixture::new();
    let mut reader = Cursor::new(b"abc".to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let v = {
        let mut ctx = fx.ctx();
        input_read(&mut ctx, &mut reader, &mut writer, None)
    }
    .expect("a partial line at EOF reads fine");
    assert_eq!(read_str(&mut fx, v), "abc");
}

#[test]
fn input_read_empty_line_is_not_eof() {
    let mut fx = Fixture::new();
    let mut reader = Cursor::new(b"\n".to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let v = {
        let mut ctx = fx.ctx();
        input_read(&mut ctx, &mut reader, &mut writer, None)
    }
    .expect("a blank line is a line, not EOF");
    assert_eq!(read_str(&mut fx, v), "");
}

#[test]
fn input_read_eof() {
    let mut fx = Fixture::new();
    let mut reader = Cursor::new(b"".to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let r = {
        let mut ctx = fx.ctx();
        input_read(&mut ctx, &mut reader, &mut writer, None)
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    let pr = fx.pending_raise.as_ref().expect("EOFError recorded");
    assert_eq!(pr.exc_name, "EOFError");
}

#[test]
fn input_read_ctrl_c() {
    let mut fx = Fixture::new();
    let mut reader = Cursor::new(b"\x03".to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let r = {
        let mut ctx = fx.ctx();
        input_read(&mut ctx, &mut reader, &mut writer, None)
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    let pr = fx
        .pending_raise
        .as_ref()
        .expect("KeyboardInterrupt recorded");
    assert_eq!(pr.exc_name, "KeyboardInterrupt");
}
