//! Lane C18b tests: warnings (vendor `py/warning.c`).
//!
//! The exact stderr text is asserted through the pure
//! `format_warning` (the same string `mp_warning` writes);
//! the rewire of the str-vs-bytes comparison warning is
//! covered end-to-end (the warning itself goes to fd 2, which
//! cannot be captured in-process).

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::value::Value;
use crate::runtime::vm::Vm;
use crate::runtime::warning::{format_warning, mp_emitter_warning, MP_PASS_CODE_SIZE};

fn run_ok(vm: &mut Vm, src: &str) {
    vm.load_module_from_source(src, "<test>")
        .expect("snippet runs");
}

fn gb(vm: &mut Vm, name: &str) -> bool {
    let q = vm.pool.from_strn(name.as_bytes()).expect("name") as u32;
    let globals = Value::Heap(vm.state.globals);
    let (mut ctx, _state) = vm.split();
    match dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok() {
        Some(Value::Bool(b)) => b,
        other => panic!("global {name} is not a bool: {other:?}"),
    }
}

#[test]
fn format_warning_no_category() {
    assert_eq!(
        format_warning(None, "Comparison between bytes and str"),
        "Warning: Comparison between bytes and str\n"
    );
}

#[test]
fn format_warning_with_category() {
    assert_eq!(
        format_warning(Some("DeprecationWarning"), "x is deprecated"),
        "DeprecationWarning: x is deprecated\n"
    );
}

#[test]
fn emitter_warning_pass_constant() {
    // `MP_PASS_CODE_SIZE` is vendor `py/emit.h:45` — the only
    // pass that fires `mp_emitter_warning`.
    assert_eq!(MP_PASS_CODE_SIZE, 3);
    // Smoke: neither call may panic (the non-code-size call
    // must stay silent by construction).
    mp_emitter_warning(MP_PASS_CODE_SIZE, "x");
    mp_emitter_warning(1, "x");
}

#[test]
fn str_bytes_cmp_warns_and_compares() {
    // The rewire: `warn_str_bytes_cmp` now goes through
    // `mp_warning` (the `eprintln!` is gone). The comparison
    // results are the observable part in-process; the warning
    // text itself is asserted in `format_warning_no_category`.
    let mut vm = Vm::new();
    run_ok(&mut vm, "a0 = 'a' == b'a'");
    assert!(!gb(&mut vm, "a0"));
    run_ok(&mut vm, "a1 = 'a' != b'a'");
    assert!(gb(&mut vm, "a1"));
    run_ok(&mut vm, "a2 = b'a' == 'a'");
    assert!(!gb(&mut vm, "a2"));
    run_ok(&mut vm, "a3 = 'a' == 'a'");
    assert!(gb(&mut vm, "a3"));
}
