//! Tests for `.mpy` const-table conversion (`vm_raise::obj_from_entry`,
//! Lane C17): the Float/Complex arms go through `mp_parse_num_float`
//! (the Zig loader's `load_obj`, `persistentcode.zig:227-290`), not a
//! plain `str::parse`. The `ValueError` cases below were probed on
//! the reference binary with hand-built `.mpy` files (a Float entry
//! `"1e+"` / `"1j"`, a Complex entry `"(1+2j)"` / `"3+4j"`).

use super::runtime_tests::Env;
use super::vm_raise::obj_from_entry;
use crate::compiler::mpy::ObjEntry;
use crate::memory::gc::{CallCtx, PendingRaise};
use crate::objects::value::{Obj, Value};
use crate::pyerr::PyError;

/// Load one const entry through a fresh `CallCtx`; returns the
/// outcome plus any recorded pending raise.
fn load(env: &mut Env, entry: &ObjEntry) -> (Result<Value, PyError>, Option<PendingRaise>) {
    let types = env.types; // `BuiltinTypes: Copy`
    let mut ctx = CallCtx::new(
        &mut env.arena,
        &mut env.roots,
        &types,
        &env.pool,
        &mut env.pending_raise,
    );
    let r = obj_from_entry(&mut ctx, &types, Value::None, entry);
    let pr = ctx.take_pending_raise();
    (r, pr)
}

fn load_float(env: &mut Env, text: &[u8]) -> f64 {
    let (r, pr) = load(env, &ObjEntry::Float(text.to_vec()));
    assert!(pr.is_none(), "unexpected pending raise for {text:?}");
    match r.expect("float const should load") {
        Value::Heap(h) => match env.arena.get(h) {
            Obj::Float(f) => *f,
            other => panic!("Float const loaded as {other:?}"),
        },
        other => panic!("Float const loaded as {other:?}"),
    }
}

fn load_complex(env: &mut Env, text: &[u8]) -> (f64, f64) {
    let (r, pr) = load(env, &ObjEntry::Complex(text.to_vec()));
    assert!(pr.is_none(), "unexpected pending raise for {text:?}");
    match r.expect("complex const should load") {
        Value::Heap(h) => match env.arena.get(h) {
            Obj::Complex { re, im } => (*re, *im),
            other => panic!("Complex const loaded as {other:?}"),
        },
        other => panic!("Complex const loaded as {other:?}"),
    }
}

/// Assert the load raises `ValueError("invalid syntax for number")`.
fn load_err(env: &mut Env, entry: &ObjEntry) {
    let (r, pr) = load(env, entry);
    assert_eq!(r, Err(PyError::PyRaise));
    let pr = pr.expect("a pending raise should be recorded");
    assert_eq!(pr.exc_name, "ValueError");
    assert_eq!(pr.message.as_deref(), Some("invalid syntax for number"));
}

#[test]
fn obj_from_entry_float_values() {
    let mut env = Env::new();
    // `"1_0.5"` loaded as 0.0 before C17 (`str::parse` failed);
    // the Zig loader parses it as 10.5.
    assert_eq!(load_float(&mut env, b"1.5"), 1.5);
    assert_eq!(load_float(&mut env, b"1_0.5"), 10.5);
    assert_eq!(load_float(&mut env, b"inf"), f64::INFINITY);
    assert_eq!(load_float(&mut env, b" 1.5 "), 1.5);
    assert_eq!(load_float(&mut env, b".5"), 0.5);
    assert_eq!(load_float(&mut env, b"1e-3"), 0.001);
}

#[test]
fn obj_from_entry_float_errors() {
    let mut env = Env::new();
    // All probed on the reference binary (hand-built .mpy):
    // each raises ValueError "invalid syntax for number".
    for text in [b"1j".as_slice(), b"1e+", b"1e", b"3+4j", b"", b"1.5.2"] {
        load_err(&mut env, &ObjEntry::Float(text.to_vec()));
    }
}

#[test]
fn obj_from_entry_complex_values_and_errors() {
    let mut env = Env::new();
    // A bare `"Nj"` text loads (probed: no ValueError on the
    // reference binary; the runtime then trips over the complex
    // object itself — C18's slot work, not the parse).
    assert_eq!(load_complex(&mut env, b"2j"), (0.0, 2.0));
    assert_eq!(load_complex(&mut env, b"1.5j"), (0.0, 1.5));
    assert_eq!(load_complex(&mut env, b"-j"), (0.0, -1.0));
    assert_eq!(load_complex(&mut env, b"1.5"), (1.5, 0.0));
    // The vendor repr `"(1+2j)"` does NOT parse (the loader calls
    // `mp_parse_num_float`, force_complex=false) — probed.
    load_err(&mut env, &ObjEntry::Complex(b"(1+2j)".to_vec()));
    load_err(&mut env, &ObjEntry::Complex(b"3+4j".to_vec()));
    load_err(&mut env, &ObjEntry::Complex(b"(1-2j)".to_vec()));
}
