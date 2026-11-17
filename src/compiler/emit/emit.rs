//! `Emit` — per-scope bytecode emitter state.
//!
//! Port of `Emit` in `src/compiler/emit.zig`. One `Emit` instance per
//! scope, reused across all passes. `dummy_data` is scratch used by
//! size-only passes and suppressed writes (after unconditional control
//! flow). The same `Emit` is driven SCOPE → STACK_SIZE → CODE_SIZE →
//! EMIT (with EMIT possibly re-running if sizes changed).
//!
//! ## Divergences
//!
//! * Zig's `Emit` holds `emit_common: *EmitCommon` (a pointer to a
//!   caller-owned table); Rust owns the `EmitCommon` by value inside
//!   `Emit`. Same data, no indirection, no lifetime.
//! * Zig's `Emit` holds `scope: ?*Scope`; Rust holds
//!   `scope: Option<ScopeId>` and takes the `ScopeArena` as a
//!   parameter wherever scope fields are read or written. Every
//!   method that can touch the scope therefore takes `scopes` —
//!   either `&ScopeArena` (read-only: `start_pass`, `end_pass`) or
//!   `&mut ScopeArena` (writes: everything that adjusts the stack,
//!   plus `yield_op`'s generator flag).
//! * `load_const_obj` is infallible here (see `writers.rs`).

use super::common::EmitCommon;
use super::const_obj::ConstObj;
use super::consts::*;
use super::writers::*;
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{
    IdInfoKind, ScopeArena, ScopeId, ScopeKind, ID_FLAG_IS_PARAM, SCOPE_FLAG_GENERATOR,
};

/// Per-scope bytecode emitter state.
#[derive(Debug)]
pub struct Emit {
    pub emit_common: EmitCommon,
    pub scope: Option<ScopeId>,
    pub pass: Pass,

    pub suppress: bool,
    pub stack_size: i32,

    pub dummy_data: [u8; DUMMY_DATA_SIZE],
    pub label_offsets: Vec<usize>,
    pub max_num_labels: usize,

    pub code_info_offset: usize,
    pub code_info_size: usize,
    pub bytecode_offset: usize,
    pub bytecode_size: usize,
    /// Owns the contiguous `[code_info | bytecode]` buffer once the
    /// EMIT pass allocates it. Length = `code_info_size +
    /// bytecode_size`.
    pub code_base: Vec<u8>,

    pub overflow: bool,
    pub n_info: usize,
    pub n_cell: usize,
}

impl Emit {
    pub fn new() -> Self {
        Emit {
            emit_common: EmitCommon::new(),
            scope: None,
            pass: Pass::Scope,
            suppress: false,
            stack_size: 0,
            dummy_data: [0u8; DUMMY_DATA_SIZE],
            label_offsets: Vec::new(),
            max_num_labels: 0,
            code_info_offset: 0,
            code_info_size: 0,
            bytecode_offset: 0,
            bytecode_size: 0,
            code_base: Vec::new(),
            overflow: false,
            n_info: 0,
            n_cell: 0,
        }
    }

    /// Reserve `label_offsets` storage. Vendor `m_malloc(0)` returns
    /// NULL; we leave the slice empty for `max_num_labels == 0`
    /// (never deref'd because no label-emit fires).
    pub fn set_max_num_labels(&mut self, n: usize) {
        self.max_num_labels = n;
        if n == 0 {
            return;
        }
        self.label_offsets = vec![0usize; n];
    }

    /// Start a pass. Resets per-pass state and emits the prelude
    /// (signature + size encoding from CODE_SIZE onward) plus the
    /// simple_name and parameter-name qstrs into code_info.
    pub fn start_pass(&mut self, pass: Pass, scope: ScopeId, scopes: &ScopeArena) {
        self.pass = pass;
        self.stack_size = 0;
        self.suppress = false;
        self.scope = Some(scope);
        self.bytecode_offset = 0;
        self.code_info_offset = 0;
        self.overflow = false;

        // Vendor: n_state = num_locals + stack_size (or 1 if zero).
        let sc = &scopes.scopes[scope];
        let mut n_state: usize = sc.num_locals as usize + sc.stack_size as usize;
        if n_state == 0 {
            n_state = 1;
        }
        let n_exc_stack: usize = sc.exc_stack_size as usize;
        prelude_sig_encode(self, n_state, n_exc_stack, sc);

        if pass >= Pass::CodeSize {
            prelude_size_encode(self, self.n_info, self.n_cell);
        }

        // After the size prelude, the code_info_offset points at where
        // n_info will start measuring from.
        self.n_info = self.code_info_offset;

        write_code_info_qstr(self, sc.simple_name);

        // Parameter-name qstrs. For each i in [0, num_pos_args +
        // num_kwonly_args), find the IdInfo with IS_PARAM flag and
        // local_num == i; emit its qst (or MP_QSTR__star_ as a
        // placeholder if not found).
        let total = sc.num_pos_args + sc.num_kwonly_args;
        let mut i: u16 = 0;
        while i < total {
            let mut q: Qstr = MP_QSTR__STAR_;
            for info in &sc.id_info {
                if (info.flags & ID_FLAG_IS_PARAM) != 0 && info.local_num == i {
                    q = info.qst;
                    break;
                }
            }
            write_code_info_qstr(self, q);
            i += 1;
        }
    }

    /// End a pass. Returns true on success; false if the EMIT pass
    /// observed a size shift (caller must re-run EMIT).
    ///
    /// On CODE_SIZE: allocates `code_base = code_info_size +
    /// bytecode_size` bytes (zero-initialised, matches vendor
    /// `m_malloc0`). On EMIT: detects size drift, raises overflow if
    /// a label outgrew its compact encoding, and finalises. (We don't
    /// perform the raw_code wiring here — that's the compile driver's
    /// job.)
    pub fn end_pass(&mut self, scopes: &ScopeArena) -> bool {
        if self.pass == Pass::Scope {
            return true;
        }

        self.n_info = self.code_info_offset - self.n_info;

        self.n_cell = 0;
        let scope = self.scope.expect("end_pass: Emit has no scope");
        for info in &scopes.scopes[scope].id_info {
            if info.kind == IdInfoKind::Cell {
                write_code_info_byte(self, info.local_num as u8);
                self.n_cell += 1;
            }
        }

        if self.pass == Pass::CodeSize {
            self.code_info_size = self.code_info_offset;
            self.bytecode_size = self.bytecode_offset;
            let total = self.code_info_size + self.bytecode_size;
            self.code_base = vec![0u8; total];
        } else if self.pass == Pass::Emit {
            if self.code_info_offset != self.code_info_size
                || self.bytecode_offset != self.bytecode_size
            {
                self.code_info_size = self.code_info_offset;
                self.bytecode_size = self.bytecode_offset;
                return false;
            }
            // overflow surfacing — caller decides what to do. Vendor
            // longjmps; we expose the flag via emit.overflow for the
            // driver to turn into the right Python-level error.
        }
        true
    }

    /// Vendor `mp_emit_bc_adjust_stack_size` — public API for compile.
    pub fn adjust_stack_size(&mut self, delta: i32, scopes: &mut ScopeArena) {
        adjust_stack(self, delta, scopes);
    }

    /// Source-line tracking is disabled in nano config
    /// (ENABLE_SOURCE_LINE = 0). Kept on the API surface so compile
    /// doesn't need source-line-aware branches.
    pub fn set_source_line(&mut self, _line: u32) {}

    /// Vendor `mp_emit_common_id_op`. Dispatches load / store / delete
    /// via the resolved `IdInfo` kind. (Zig has this as a static
    /// method on `EmitCommon` taking `*Emit`; here it is a method on
    /// `Emit` because `Emit` owns its `EmitCommon`.)
    ///
    /// On a missing id (corrupted scope — "should never fire"), Zig
    /// debug-asserts and returns `OutOfMemory`, which the caller turns
    /// into `overflow = true`; this port sets `overflow` directly.
    pub fn id_op(&mut self, op: IdOp, scope: ScopeId, q: Qstr, scopes: &mut ScopeArena) {
        let Some(idx) = scopes.find(scope, q) else {
            debug_assert!(false, "id_op: id not found in scope");
            self.overflow = true;
            return;
        };
        let (kind, local_num) = {
            let id = &scopes.scopes[scope].id_info[idx];
            (id.kind, id.local_num)
        };

        match op {
            IdOp::Load => {
                if kind == IdInfoKind::GlobalImplicit
                    || kind == IdInfoKind::GlobalImplicitAssigned
                {
                    self.load_global(q, MP_EMIT_IDOP_GLOBAL_NAME, scopes);
                } else if kind == IdInfoKind::GlobalExplicit {
                    self.load_global(q, MP_EMIT_IDOP_GLOBAL_GLOBAL, scopes);
                } else if kind == IdInfoKind::Local {
                    self.load_local(local_num, MP_EMIT_IDOP_LOCAL_FAST, scopes);
                } else {
                    debug_assert!(kind == IdInfoKind::Cell || kind == IdInfoKind::Free);
                    self.load_local(local_num, MP_EMIT_IDOP_LOCAL_DEREF, scopes);
                }
            }
            IdOp::Store => {
                if kind == IdInfoKind::GlobalImplicit
                    || kind == IdInfoKind::GlobalImplicitAssigned
                {
                    self.store_global(q, MP_EMIT_IDOP_GLOBAL_NAME, scopes);
                } else if kind == IdInfoKind::GlobalExplicit {
                    self.store_global(q, MP_EMIT_IDOP_GLOBAL_GLOBAL, scopes);
                } else if kind == IdInfoKind::Local {
                    self.store_local(local_num, MP_EMIT_IDOP_LOCAL_FAST, scopes);
                } else {
                    debug_assert!(kind == IdInfoKind::Cell || kind == IdInfoKind::Free);
                    self.store_local(local_num, MP_EMIT_IDOP_LOCAL_DEREF, scopes);
                }
            }
            IdOp::Delete => {
                if kind == IdInfoKind::GlobalImplicit
                    || kind == IdInfoKind::GlobalImplicitAssigned
                {
                    self.delete_global(q, MP_EMIT_IDOP_GLOBAL_NAME, scopes);
                } else if kind == IdInfoKind::GlobalExplicit {
                    self.delete_global(q, MP_EMIT_IDOP_GLOBAL_GLOBAL, scopes);
                } else if kind == IdInfoKind::Local {
                    self.delete_local(local_num, MP_EMIT_IDOP_LOCAL_FAST, scopes);
                } else {
                    debug_assert!(kind == IdInfoKind::Cell || kind == IdInfoKind::Free);
                    self.delete_local(local_num, MP_EMIT_IDOP_LOCAL_DEREF, scopes);
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Opcode emitters — verbatim port of vendor emitbc.zig.
    // Each writes bytes, possibly adjusts stack, possibly toggles
    // suppress (after unconditional control flow).
    // ─────────────────────────────────────────────────────────────────────

    pub fn label_assign(&mut self, label: usize) {
        self.suppress = false;
        if self.pass == Pass::Scope {
            return;
        }
        self.label_offsets[label] = self.bytecode_offset;
    }

    pub fn import(&mut self, q: Qstr, kind: u8, scopes: &mut ScopeArena) {
        let stack_adj: i32 = if kind == MP_EMIT_IMPORT_FROM { 1 } else { -1 };
        if kind == MP_EMIT_IMPORT_STAR {
            write_bytecode_byte(self, stack_adj, MP_BC_IMPORT_STAR, scopes);
        } else {
            write_bytecode_byte_qstr(self, stack_adj, MP_BC_IMPORT_NAME + kind, q, scopes);
        }
    }

    pub fn load_const_tok(&mut self, tok: u8, scopes: &mut ScopeArena) {
        if tok == MP_TOKEN_ELLIPSIS {
            // Vendor uses LOAD_CONST_OBJ + mp_const_ellipsis_obj. We
            // route through the const-obj table with a dedicated
            // ConstObj::Ellipsis value.
            write_bytecode_byte_obj(self, 1, MP_BC_LOAD_CONST_OBJ, ConstObj::Ellipsis, scopes);
        } else {
            // FALSE/NONE/TRUE map to the contiguous LOAD_CONST_FALSE+N.
            write_bytecode_byte(
                self,
                1,
                MP_BC_LOAD_CONST_FALSE + (tok - MP_TOKEN_KW_FALSE),
                scopes,
            );
        }
    }

    pub fn load_const_small_int(&mut self, arg: isize, scopes: &mut ScopeArena) {
        if -MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS <= arg
            && arg < MP_BC_LOAD_CONST_SMALL_INT_MULTI_NUM
                - MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS
        {
            let opcode_val: isize = MP_BC_LOAD_CONST_SMALL_INT_MULTI as isize
                + MP_BC_LOAD_CONST_SMALL_INT_MULTI_EXCESS
                + arg;
            write_bytecode_byte(self, 1, opcode_val as u8, scopes);
        } else {
            write_bytecode_byte_int(self, 1, MP_BC_LOAD_CONST_SMALL_INT, arg, scopes);
        }
    }

    pub fn load_const_str(&mut self, q: Qstr, scopes: &mut ScopeArena) {
        write_bytecode_byte_qstr(self, 1, MP_BC_LOAD_CONST_STRING, q, scopes);
    }

    pub fn load_const_obj(&mut self, obj: ConstObj, scopes: &mut ScopeArena) {
        write_bytecode_byte_obj(self, 1, MP_BC_LOAD_CONST_OBJ, obj, scopes);
    }

    pub fn load_null(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 1, MP_BC_LOAD_NULL, scopes);
    }

    pub fn load_local(&mut self, local_num: u16, kind: u8, scopes: &mut ScopeArena) {
        if kind == MP_EMIT_IDOP_LOCAL_FAST && local_num <= 15 {
            write_bytecode_byte(
                self,
                1,
                MP_BC_LOAD_FAST_MULTI + local_num as u8,
                scopes,
            );
        } else {
            write_bytecode_byte_uint(
                self,
                1,
                MP_BC_LOAD_FAST_N + kind,
                local_num as usize,
                scopes,
            );
        }
    }

    pub fn load_global(&mut self, q: Qstr, kind: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte_qstr(self, 1, MP_BC_LOAD_NAME + kind, q, scopes);
    }

    pub fn load_method(&mut self, q: Qstr, is_super: bool, scopes: &mut ScopeArena) {
        let stack_adj: i32 = 1 - 2 * (is_super as i32);
        let op: u8 = if is_super {
            MP_BC_LOAD_SUPER_METHOD
        } else {
            MP_BC_LOAD_METHOD
        };
        write_bytecode_byte_qstr(self, stack_adj, op, q, scopes);
    }

    pub fn load_build_class(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 1, MP_BC_LOAD_BUILD_CLASS, scopes);
    }

    pub fn subscr(&mut self, kind: u8, scopes: &mut ScopeArena) {
        if kind == MP_EMIT_SUBSCR_LOAD {
            write_bytecode_byte(self, -1, MP_BC_LOAD_SUBSCR, scopes);
        } else {
            if kind == MP_EMIT_SUBSCR_DELETE {
                self.load_null(scopes);
                self.rot_three(scopes);
            }
            write_bytecode_byte(self, -3, MP_BC_STORE_SUBSCR, scopes);
        }
    }

    pub fn attr(&mut self, q: Qstr, kind: u8, scopes: &mut ScopeArena) {
        if kind == MP_EMIT_ATTR_LOAD {
            write_bytecode_byte_qstr(self, 0, MP_BC_LOAD_ATTR, q, scopes);
        } else {
            if kind == MP_EMIT_ATTR_DELETE {
                self.load_null(scopes);
                self.rot_two(scopes);
            }
            write_bytecode_byte_qstr(self, -2, MP_BC_STORE_ATTR, q, scopes);
        }
    }

    pub fn store_local(&mut self, local_num: u16, kind: u8, scopes: &mut ScopeArena) {
        if kind == MP_EMIT_IDOP_LOCAL_FAST && local_num <= 15 {
            write_bytecode_byte(
                self,
                -1,
                MP_BC_STORE_FAST_MULTI + local_num as u8,
                scopes,
            );
        } else {
            write_bytecode_byte_uint(
                self,
                -1,
                MP_BC_STORE_FAST_N + kind,
                local_num as usize,
                scopes,
            );
        }
    }

    pub fn store_global(&mut self, q: Qstr, kind: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte_qstr(self, -1, MP_BC_STORE_NAME + kind, q, scopes);
    }

    pub fn delete_local(&mut self, local_num: u16, kind: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte_uint(self, 0, MP_BC_DELETE_FAST + kind, local_num as usize, scopes);
    }

    pub fn delete_global(&mut self, q: Qstr, kind: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte_qstr(self, 0, MP_BC_DELETE_NAME + kind, q, scopes);
    }

    pub fn dup_top(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 1, MP_BC_DUP_TOP, scopes);
    }

    pub fn dup_top_two(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 2, MP_BC_DUP_TOP_TWO, scopes);
    }

    pub fn pop_top(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, -1, MP_BC_POP_TOP, scopes);
    }

    pub fn rot_two(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 0, MP_BC_ROT_TWO, scopes);
    }

    pub fn rot_three(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 0, MP_BC_ROT_THREE, scopes);
    }

    pub fn jump(&mut self, label: usize, scopes: &mut ScopeArena) {
        write_bytecode_byte_label(self, 0, MP_BC_JUMP, label, scopes);
        self.suppress = true;
    }

    pub fn pop_jump_if(&mut self, cond: bool, label: usize, scopes: &mut ScopeArena) {
        let op: u8 = if cond {
            MP_BC_POP_JUMP_IF_TRUE
        } else {
            MP_BC_POP_JUMP_IF_FALSE
        };
        write_bytecode_byte_label(self, -1, op, label, scopes);
    }

    pub fn jump_if_or_pop(&mut self, cond: bool, label: usize, scopes: &mut ScopeArena) {
        let op: u8 = if cond {
            MP_BC_JUMP_IF_TRUE_OR_POP
        } else {
            MP_BC_JUMP_IF_FALSE_OR_POP
        };
        write_bytecode_byte_label(self, -1, op, label, scopes);
    }

    pub fn unwind_jump(&mut self, label: usize, except_depth: usize, scopes: &mut ScopeArena) {
        const BREAK: usize = MP_EMIT_BREAK_FROM_FOR;
        if except_depth == 0 {
            if (label & BREAK) != 0 {
                // Pop the iterator's stack slots before jumping.
                write_bytecode_raw_byte(self, MP_BC_POP_TOP);
                let mut i: usize = 0;
                while i < MP_OBJ_ITER_BUF_NSLOTS - 1 {
                    write_bytecode_raw_byte(self, MP_BC_POP_TOP);
                    i += 1;
                }
            }
            write_bytecode_byte_label(self, 0, MP_BC_JUMP, label & !BREAK, scopes);
        } else {
            write_bytecode_byte_label(self, 0, MP_BC_UNWIND_JUMP, label & !BREAK, scopes);
            let extra: u8 =
                ((if (label & BREAK) != 0 { 0x80usize } else { 0 }) | except_depth) as u8;
            write_bytecode_raw_byte(self, extra);
        }
        self.suppress = true;
    }

    pub fn setup_block(&mut self, label: usize, kind: u8, scopes: &mut ScopeArena) {
        let stack_adj: i32 = if kind == MP_EMIT_SETUP_BLOCK_WITH { 2 } else { 0 };
        write_bytecode_byte_label(self, stack_adj, MP_BC_SETUP_WITH + kind, label, scopes);
    }

    pub fn with_cleanup(&mut self, label: usize, scopes: &mut ScopeArena) {
        self.load_const_tok(MP_TOKEN_KW_NONE, scopes);
        self.label_assign(label);
        write_bytecode_byte(self, 2, MP_BC_WITH_CLEANUP, scopes);
        adjust_stack(self, -4, scopes);
    }

    pub fn end_finally(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, -1, MP_BC_END_FINALLY, scopes);
    }

    pub fn get_iter(&mut self, use_stack: bool, scopes: &mut ScopeArena) {
        let stack_adj: i32 = if use_stack {
            MP_OBJ_ITER_BUF_NSLOTS as i32 - 1
        } else {
            0
        };
        let op: u8 = if use_stack {
            MP_BC_GET_ITER_STACK
        } else {
            MP_BC_GET_ITER
        };
        write_bytecode_byte(self, stack_adj, op, scopes);
    }

    pub fn for_iter(&mut self, label: usize, scopes: &mut ScopeArena) {
        write_bytecode_byte_label(self, 1, MP_BC_FOR_ITER, label, scopes);
    }

    pub fn for_iter_end(&mut self, scopes: &mut ScopeArena) {
        adjust_stack(self, -(MP_OBJ_ITER_BUF_NSLOTS as i32), scopes);
    }

    pub fn pop_except_jump(&mut self, label: usize, scopes: &mut ScopeArena) {
        write_bytecode_byte_label(self, 0, MP_BC_POP_EXCEPT_JUMP, label, scopes);
        self.suppress = true;
    }

    pub fn start_except_handler(&mut self, scopes: &mut ScopeArena) {
        adjust_stack(self, 4, scopes);
    }

    pub fn end_except_handler(&mut self, scopes: &mut ScopeArena) {
        adjust_stack(self, -3, scopes);
    }

    pub fn unary_op(&mut self, op: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, 0, MP_BC_UNARY_OP_MULTI + op, scopes);
    }

    pub fn binary_op(&mut self, op_in: u8, scopes: &mut ScopeArena) {
        let mut op = op_in;
        let mut invert = false;
        if op == MP_BINARY_OP_NOT_IN {
            invert = true;
            op = MP_BINARY_OP_IN;
        } else if op == MP_BINARY_OP_IS_NOT {
            invert = true;
            op = MP_BINARY_OP_IS;
        }
        write_bytecode_byte(self, -1, MP_BC_BINARY_OP_MULTI + op, scopes);
        if invert {
            write_bytecode_byte(self, 0, MP_BC_UNARY_OP_MULTI + MP_UNARY_OP_NOT, scopes);
        }
    }

    pub fn build(&mut self, n_args: usize, kind: u8, scopes: &mut ScopeArena) {
        let stack_adj: i32 = if kind == MP_EMIT_BUILD_MAP {
            1
        } else {
            1 - n_args as i32
        };
        write_bytecode_byte_uint(self, stack_adj, MP_BC_BUILD_TUPLE + kind, n_args, scopes);
    }

    pub fn store_map(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, -2, MP_BC_STORE_MAP, scopes);
    }

    pub fn store_comp(
        &mut self,
        scope_kind: ScopeKind,
        collection_stack_index: usize,
        scopes: &mut ScopeArena,
    ) {
        // LIST_COMP -> t=0; DICT_COMP -> t=1; SET_COMP -> t=2.
        let (t, n): (u8, i32) = match scope_kind {
            ScopeKind::ListComp => (0, 0),
            ScopeKind::DictComp => (1, 1),
            ScopeKind::SetComp => (2, 0),
            _ => unreachable!("store_comp: non-comprehension scope kind"),
        };
        let arg: usize = ((collection_stack_index + n as usize) << 2) | t as usize;
        write_bytecode_byte_uint(self, -1 - n, MP_BC_STORE_COMP, arg, scopes);
    }

    pub fn unpack_sequence(&mut self, n_args: usize, scopes: &mut ScopeArena) {
        write_bytecode_byte_uint(
            self,
            -1 + n_args as i32,
            MP_BC_UNPACK_SEQUENCE,
            n_args,
            scopes,
        );
    }

    pub fn unpack_ex(&mut self, n_left: usize, n_right: usize, scopes: &mut ScopeArena) {
        write_bytecode_byte_uint(
            self,
            -1 + n_left as i32 + n_right as i32 + 1,
            MP_BC_UNPACK_EX,
            n_left | (n_right << 8),
            scopes,
        );
    }

    pub fn make_function(
        &mut self,
        child: ScopeId,
        n_pos_def: usize,
        n_kw_def: usize,
        scopes: &mut ScopeArena,
    ) {
        if n_pos_def == 0 && n_kw_def == 0 {
            write_bytecode_byte_child(self, 1, MP_BC_MAKE_FUNCTION, child, scopes);
        } else {
            write_bytecode_byte_child(self, -1, MP_BC_MAKE_FUNCTION_DEFARGS, child, scopes);
        }
    }

    pub fn make_closure(
        &mut self,
        child: ScopeId,
        n_closed_over: u8,
        n_pos_def: usize,
        n_kw_def: usize,
        scopes: &mut ScopeArena,
    ) {
        if n_pos_def == 0 && n_kw_def == 0 {
            let stack_adj: i32 = -(n_closed_over as i32) + 1;
            write_bytecode_byte_child(self, stack_adj, MP_BC_MAKE_CLOSURE, child, scopes);
            write_bytecode_raw_byte(self, n_closed_over);
        } else {
            let stack_adj: i32 = -2 - (n_closed_over as i32) + 1;
            write_bytecode_byte_child(
                self,
                stack_adj,
                MP_BC_MAKE_CLOSURE_DEFARGS,
                child,
                scopes,
            );
            write_bytecode_raw_byte(self, n_closed_over);
        }
    }

    pub fn call_function(
        &mut self,
        n_pos: usize,
        n_kw: usize,
        star_flags: u8,
        scopes: &mut ScopeArena,
    ) {
        call_helper(self, 0, MP_BC_CALL_FUNCTION, n_pos, n_kw, star_flags, scopes);
    }

    pub fn call_method(
        &mut self,
        n_pos: usize,
        n_kw: usize,
        star_flags: u8,
        scopes: &mut ScopeArena,
    ) {
        call_helper(self, -1, MP_BC_CALL_METHOD, n_pos, n_kw, star_flags, scopes);
    }

    pub fn return_value(&mut self, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, -1, MP_BC_RETURN_VALUE, scopes);
        self.suppress = true;
    }

    pub fn raise_varargs(&mut self, n_args: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte(
            self,
            -(n_args as i32),
            MP_BC_RAISE_LAST + n_args,
            scopes,
        );
        self.suppress = true;
    }

    /// (`yield` is a reserved word in Rust; the Zig method is `yield`.)
    pub fn yield_op(&mut self, kind: u8, scopes: &mut ScopeArena) {
        write_bytecode_byte(self, -(kind as i32), MP_BC_YIELD_VALUE + kind, scopes);
        let sc = self.scope.expect("yield_op: Emit has no scope");
        scopes.scopes[sc].scope_flags |= SCOPE_FLAG_GENERATOR;
    }
}
