//! Dispatch-level tests for the stack ops (G2 P12).
//!
//! `DUP_TOP_TWO` copies the two raw stack slots (`vm.zig:703-708`);
//! the old arm pushed two NULLs whenever either slot held NULL.
//! Only a hand-made .mpy can put a NULL under the top (the
//! compiler never emits `LOAD_NULL` next to `DUP_TOP_TWO`), so
//! the test patches a compiled module's bytecode: compile
//! `x = 1`, truncate its `fun_data` at the bytecode start,
//! append the hand sequence, add a `q` qstr, re-serialise, and
//! run it through `load_module_from_mpy`. The sequence stores
//! the duplicated value into global `q`: the old arm stored
//! `None` (the duplicated `7` was lost), the fixed arm stores
//! `7`. The normal path (`a[i] += x`, which the compiler emits
//! as `DUP_TOP_TWO`) is covered by the corpus gate, not here.

use crate::compiler::bridge::compile_to_mpy;
use crate::compiler::emit::consts::{
    MP_BC_DUP_TOP_TWO, MP_BC_LOAD_CONST_NONE, MP_BC_LOAD_CONST_SMALL_INT_MULTI,
    MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS, MP_BC_LOAD_NULL, MP_BC_POP_TOP,
    MP_BC_RETURN_VALUE, MP_BC_STORE_NAME,
};
use crate::compiler::mpy::{self, QstrEntry, RawCode};
use crate::compiler::parser::InputKind;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::value::Value;
use crate::runtime::bc::{prelude_sig_decode, prelude_size_decode};
use crate::runtime::vm::Vm;

/// Bytecode start of a compiled function: prelude bytes +
/// source-info bytes + cell bytes (same measure as
/// `bc_tests::expected_ip`).
fn code_start(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    let _sig = prelude_sig_decode(&mut cursor);
    let size = prelude_size_decode(&mut cursor);
    (fun.fun_data.len() - cursor.len()) + size.n_info + size.n_cell
}

/// A module global as a small int (panics otherwise) — the
/// `vm_tests::Fx::gi` shape, without the fixture.
fn global_int(vm: &mut Vm, name: &str) -> i64 {
    let q = vm.pool.from_strn(name.as_bytes()).ok().unwrap() as u32;
    let globals = Value::Heap(vm.state.globals);
    let (mut ctx, _state) = vm.split();
    match dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok() {
        Some(Value::SmallInt(n)) => n,
        other => panic!("global {name} is not a small int: {other:?}"),
    }
}

#[test]
fn dup_top_two_copies_raw_slots() {
    let bytes = compile_to_mpy("x = 1\n", InputKind::FileInput).expect("compiles");
    let mut module = mpy::read(&bytes).expect("parses");
    let start = code_start(&module.rc);
    module.rc.fun_data.truncate(start);
    // Append a `q` qstr; its table index is the STORE_NAME arg.
    module.qstrs.push(QstrEntry::New(b"q".to_vec()));
    let q_idx = module.qstrs.len() - 1;
    // LOAD_CONST_SMALL_INT 7; LOAD_NULL; DUP_TOP_TWO; POP_TOP;
    // STORE_NAME q; LOAD_CONST_NONE; RETURN_VALUE.
    // Stack after the dup: [7, NULL, 7, NULL]; the POP_TOP
    // drops the copied NULL, so `q` gets the copied 7.
    module.rc.fun_data.extend_from_slice(&[
        (MP_BC_LOAD_CONST_SMALL_INT_MULTI as isize
            + MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS
            + 7) as u8,
        MP_BC_LOAD_NULL,
        MP_BC_DUP_TOP_TWO,
        MP_BC_POP_TOP,
        MP_BC_STORE_NAME,
        q_idx as u8,
        MP_BC_LOAD_CONST_NONE,
        MP_BC_RETURN_VALUE,
    ]);
    let bytes = mpy::write(&module);
    let mut vm = Vm::new();
    vm.load_module_from_mpy(&bytes, "__main__")
        .expect("patched module runs");
    assert_eq!(global_int(&mut vm, "q"), 7);
}
