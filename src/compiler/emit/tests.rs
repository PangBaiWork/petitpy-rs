//! Ported unit tests for `emit.zig` (12 test blocks).
//!
//! These cover the algorithmic core: encoders, dedup, single-opcode
//! emission, multi-pass consistency, label resolution, suppress-flag.
//! Full corpus parity against mpy-cross is the integration test
//! (`compile.rs` driver).

use super::common::EmitCommon;
use super::const_obj::ConstObj;
use super::consts::*;
use super::emit::Emit;
use super::encoders::{encode_int, encode_uint};
use crate::compiler::parser::{PN_NULL, ParseTree};
use crate::compiler::scope::{ScopeArena, ScopeId, ScopeKind};

/// Test scaffolding: a minimal Scope built without parser machinery
/// for driving `Emit::start_pass` / `end_pass`. We feed zero counts
/// everywhere so the prelude_sig_encode loop terminates after one
/// byte (n_state=1).
fn fake_module_scope() -> (ScopeArena, ScopeId) {
    let tree = ParseTree::new();
    let mut arenas = ScopeArena::new();
    let scope = arenas.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
    (arenas, scope)
}

#[test]
fn encode_uint_small_and_multi_byte_boundaries() {
    let mut buf = [0u8; DUMMY_DATA_SIZE];

    // 0 → single byte 0x00
    assert_eq!(encode_uint(&mut buf, 0), [0x00u8].as_slice());
    // 127 → single byte 0x7f
    assert_eq!(encode_uint(&mut buf, 127), [0x7fu8].as_slice());
    // 128 → 0x81 0x00 (big-endian 7-bit chunks: 1, 0)
    assert_eq!(encode_uint(&mut buf, 128), [0x81u8, 0x00].as_slice());
    // 16383 (== 0x3fff) → 0xff 0x7f
    assert_eq!(
        encode_uint(&mut buf, 16383),
        [0xffu8, 0x7f].as_slice()
    );
    // 16384 (== 0x4000) → 0x81 0x80 0x00
    assert_eq!(
        encode_uint(&mut buf, 16384),
        [0x81u8, 0x80, 0x00].as_slice()
    );
}

#[test]
fn encode_int_small_positive_single_byte() {
    let mut buf = [0u8; DUMMY_DATA_SIZE];
    // 0 → 0x00 (single byte, high bit clear, sign bit 0x40 also clear → no padding)
    assert_eq!(encode_int(&mut buf, 0), [0x00u8].as_slice());
    // -1 → 0x7f (all-ones, single byte; num shifts to -1, byte bit 0x40 is set → no extra pad)
    assert_eq!(encode_int(&mut buf, -1), [0x7fu8].as_slice());
}

#[test]
fn const_obj_strictly_equal_type_mismatch_and_tuple_recursion() {
    // True != 1 (different union tags, even if both "truthy").
    assert!(!ConstObj::strictly_equal(
        &ConstObj::BoolTrue,
        &ConstObj::IntBig(b"1".to_vec())
    ));
    assert!(ConstObj::strictly_equal(
        &ConstObj::BoolTrue,
        &ConstObj::BoolTrue
    ));
    assert!(!ConstObj::strictly_equal(
        &ConstObj::BoolTrue,
        &ConstObj::BoolFalse
    ));

    // Floats: "0.0" vs "-0.0" → text differs → not equal (preserves ±0).
    assert!(!ConstObj::strictly_equal(
        &ConstObj::Float(b"0.0".to_vec()),
        &ConstObj::Float(b"-0.0".to_vec()),
    ));
    assert!(ConstObj::strictly_equal(
        &ConstObj::Float(b"0.0".to_vec()),
        &ConstObj::Float(b"0.0".to_vec()),
    ));

    // Tuple recursion + nested element comparison.
    let inner_a = vec![ConstObj::BoolTrue, ConstObj::Str(b"x".to_vec())];
    let inner_b = vec![ConstObj::BoolTrue, ConstObj::Str(b"x".to_vec())];
    let inner_c = vec![ConstObj::BoolTrue, ConstObj::Str(b"y".to_vec())];
    assert!(ConstObj::strictly_equal(
        &ConstObj::Tuple(inner_a.clone()),
        &ConstObj::Tuple(inner_b),
    ));
    assert!(!ConstObj::strictly_equal(
        &ConstObj::Tuple(inner_a),
        &ConstObj::Tuple(inner_c),
    ));
}

#[test]
fn emit_common_use_qstr_dedup_and_stable_index() {
    let mut ec = EmitCommon::new();

    let idx0 = ec.use_qstr(100);
    let idx1 = ec.use_qstr(200);
    let idx0b = ec.use_qstr(100);
    assert_eq!(idx0, 0);
    assert_eq!(idx1, 1);
    assert_eq!(idx0b, 0);
    assert_eq!(ec.qstr_table.len(), 2);
}

#[test]
fn emit_common_use_const_obj_insertion_order_preserved_on_dup() {
    let mut ec = EmitCommon::new();

    let idx0 = ec.use_const_obj(ConstObj::Str(b"hello".to_vec()));
    let idx1 = ec.use_const_obj(ConstObj::BoolTrue);
    let idx0b = ec.use_const_obj(ConstObj::Str(b"hello".to_vec()));
    assert_eq!(idx0, 0);
    assert_eq!(idx1, 1);
    assert_eq!(idx0b, 0);
    assert_eq!(ec.const_obj_list.len(), 2);
}

#[test]
fn emit_common_alloc_const_child_counter_and_stable_across_passes() {
    let tree = ParseTree::new();
    let mut arenas = ScopeArena::new();
    let s1 = arenas.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
    let s2 = arenas.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);

    let mut ec = EmitCommon::new();

    // First pass: two distinct children.
    ec.start_pass(Pass::Scope);
    assert_eq!(ec.alloc_const_child(s1), 0);
    assert_eq!(ec.alloc_const_child(s2), 1);
    assert_eq!(ec.children.len(), 2);

    // Second pass: counter resets; same scopes hit the same slots
    // (vendor invariant — the assert in alloc_const_child fires otherwise).
    ec.start_pass(Pass::CodeSize);
    assert_eq!(ec.alloc_const_child(s1), 0);
    assert_eq!(ec.alloc_const_child(s2), 1);
    assert_eq!(ec.children.len(), 2);
}

#[test]
fn load_const_tok_false_none_true_map_to_byte_o_plus_n() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    // Drive minimal passes to allocate code_base.
    emit.set_max_num_labels(0);
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.load_const_tok(MP_TOKEN_KW_FALSE, &mut arenas);
        emit.load_const_tok(MP_TOKEN_KW_NONE, &mut arenas);
        emit.load_const_tok(MP_TOKEN_KW_TRUE, &mut arenas);
        assert!(emit.end_pass(&arenas));
    }
    emit.emit_common.start_pass(Pass::Emit);
    emit.start_pass(Pass::Emit, scope, &arenas);
    emit.load_const_tok(MP_TOKEN_KW_FALSE, &mut arenas);
    emit.load_const_tok(MP_TOKEN_KW_NONE, &mut arenas);
    emit.load_const_tok(MP_TOKEN_KW_TRUE, &mut arenas);
    assert!(emit.end_pass(&arenas));

    // Bytecode region starts at code_info_size; first three bytes should
    // be FALSE/NONE/TRUE (0x50, 0x51, 0x52).
    let bc = &emit.code_base[emit.code_info_size..];
    assert_eq!(bc[0], MP_BC_LOAD_CONST_FALSE);
    assert_eq!(bc[1], MP_BC_LOAD_CONST_NONE);
    assert_eq!(bc[2], MP_BC_LOAD_CONST_TRUE);
}

#[test]
fn load_const_small_int_in_range_uses_multi_out_of_range_uses_vint() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    emit.set_max_num_labels(0);
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize, Pass::Emit] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.load_const_small_int(0, &mut arenas); // in-range → MULTI byte
        emit.load_const_small_int(100, &mut arenas); // out-of-range → 1 + VLI
        assert!(emit.end_pass(&arenas));
    }

    let bc = &emit.code_base[emit.code_info_size..];
    // 0 → MULTI(0x70) + EXCESS(16) + 0 == 0x80.
    assert_eq!(bc[0], 0x80);
    // 100 → LOAD_CONST_SMALL_INT opcode + VLI(100). encodeInt(100) is
    // 2 bytes (0x80, 0x64): bit 0x40 is set in 100, so a 0x00 pad-byte is
    // prepended to disambiguate from a negative number.
    assert_eq!(bc[1], MP_BC_LOAD_CONST_SMALL_INT);
    assert_eq!(bc[2], 0x80);
    assert_eq!(bc[3], 0x64);
    assert_eq!(bc.len(), 4);
}

#[test]
fn binary_op_not_in_emits_in_plus_unary_not() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    emit.set_max_num_labels(0);
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize, Pass::Emit] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.binary_op(MP_BINARY_OP_NOT_IN, &mut arenas);
        assert!(emit.end_pass(&arenas));
    }

    let bc = &emit.code_base[emit.code_info_size..];
    assert_eq!(bc[0], MP_BC_BINARY_OP_MULTI + MP_BINARY_OP_IN);
    assert_eq!(bc[1], MP_BC_UNARY_OP_MULTI + MP_UNARY_OP_NOT);
}

#[test]
fn return_value_sets_suppress_subsequent_ops_go_to_scratch() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    emit.set_max_num_labels(0);
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize, Pass::Emit] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.return_value(&mut arenas);
        // Suppressed: this dup_top must not extend the bytecode region.
        emit.dup_top(&mut arenas);
        assert!(emit.end_pass(&arenas));
    }

    assert!(emit.suppress);
    let bc = &emit.code_base[emit.code_info_size..];
    assert_eq!(bc.len(), 1);
    assert_eq!(bc[0], MP_BC_RETURN_VALUE);
}

#[test]
fn three_pass_consistency_load_const_small_int_plus_return_value_stable_size() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    emit.set_max_num_labels(0);

    let mut code_size_bc: usize = 0;
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize, Pass::Emit] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.load_const_small_int(1, &mut arenas);
        emit.return_value(&mut arenas);
        let ok = emit.end_pass(&arenas);
        assert!(ok);
        if p == Pass::CodeSize {
            code_size_bc = emit.bytecode_size;
        }
    }
    assert_eq!(code_size_bc, emit.bytecode_offset);
    assert_eq!(code_size_bc, 2); // MULTI(0x71) + RETURN_VALUE
}

#[test]
fn label_assign_plus_jump_short_forward_jump_after_code_size() {
    let (mut arenas, scope) = fake_module_scope();
    let mut emit = Emit::new();

    emit.set_max_num_labels(1);
    for p in [Pass::Scope, Pass::StackSize, Pass::CodeSize, Pass::Emit] {
        emit.emit_common.start_pass(p);
        emit.start_pass(p, scope, &arenas);
        emit.jump(0, &mut arenas); // jump to label 0
        emit.label_assign(0);
        emit.pop_top(&mut arenas); // a single byte op
        assert!(emit.end_pass(&arenas));
    }

    assert!(!emit.overflow);
    // JUMP encodes 2 bytes (opcode + offset) in the short form. After
    // CODE_SIZE the label resolves to offset 0 — fits the 1-byte form.
    let bc = &emit.code_base[emit.code_info_size..];
    assert_eq!(bc[0], MP_BC_JUMP);
    // bytecode_offset at jump emit = 0; label_offset = 2; offset = 2 - 0 - 2 = 0;
    // is_signed → +0x40 → 0x40.
    assert_eq!(bc[1], 0x40);
    assert_eq!(bc[2], MP_BC_POP_TOP);
}
