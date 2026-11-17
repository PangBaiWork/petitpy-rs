//! Compiler driver — pure-Rust port of `src/compiler/compile.zig`
//! (5,593 lines, 12 test blocks).
//!
//! Port of the frontend compiler: the algorithmic structure follows
//! vendor `py/compile.c` (~3700 LOC) — handler order, label allocation
//! sequence, `scopeComputeThings` ordering, scope-slot stash,
//! `c_if_cond` ordering, optimised-range recognizer, and the 3-pass
//! driver — all ported verbatim because byte-identical `.mpy` output
//! against mpy-cross requires it.
//!
//! Split across submodules so no file exceeds 800 lines:
//!   * `dispatch` — the dispatch table + `compile_node`.
//!   * `driver` — `scope_compute_things`, `drive_scope_body`, the
//!     SCOPE / codegen pass drivers, and the public `compile()` entry.
//!   * `handlers` — statement handlers (§11, §12, §20) + re-exports
//!     of the sibling handler modules: `handlers_import` (§13),
//!     `handlers_control` (§14–§16), `handlers_try` (§17),
//!     `handlers_expr_stmt` (§18).
//!   * `assign` — the `c_assign` family (§10).
//!   * `expr` — expression compilers (§19, §21, §23, §24, §29) +
//!     re-exports of `expr_atom` (§22) and `expr_const` (§26).
//!   * `funcdef` — funcdef / classdef / decorator / comprehensions
//!     (§20, §24) + re-exports of `funcdef_call` (trailer/brace
//!     helpers) and `funcdef_params` (§27).
//!   * `match_stmt` — the `match` statement (§34).
//!   * `async_stmt` — async/await (§33.1).
//!
//! ## Divergences
//!
//! * **Scope stash slots.** Zig writes child-scope pointers into
//!   parse-tree node slots (`lambdef` nodes[2], `funcdef` nodes[4],
//!   `classdef` nodes[3], `comp_for` nodes[3]) in the SCOPE pass and
//!   reads them back in later passes. The Rust parse tree is an
//!   immutable arena (its nodes are `Copy` `Pn` values, not
//!   pointers), so `Compiler` keeps a side table
//!   (`scope_slots: HashMap<Pn, ScopeId>`) keyed by the struct
//!   node's `Pn`. Same lookup, same values, no tree mutation.
//! * **Owned `QstrContext`.** Zig's `compile()` borrows the caller's
//!   `*QstrContext`; the Rust `Compiler` owns it (the bridge creates
//!   one per compilation and drops it afterwards). Same lifetime in
//!   practice, no lifetime parameters on `Compiler`.
//! * **No `extern "C"`.** The C-ABI export is Phase F; see
//!   `bridge.rs`.
//! * Allocation cannot fail gracefully in Rust, so the Zig `catch {
//!   ... }` OOM paths around `findOrAdd` / `Scope.new` /
//!   `allocConstChild` do not exist here (see `emit` docs). The
//!   `compile_error` field keeps vendor's first-error-wins timing.

pub mod assign;
pub mod async_stmt;
pub mod dispatch;
pub mod driver;
pub mod expr;
pub mod expr_atom;
pub mod expr_const;
pub mod funcdef;
pub mod funcdef_call;
pub mod funcdef_params;
pub mod handlers;
pub mod handlers_control;
pub mod handlers_expr_stmt;
pub mod handlers_import;
pub mod handlers_try;
pub mod match_stmt;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fmt;

use crate::compiler::emit::consts::{IdOp, Pass};
use crate::compiler::emit::{Emit, EmitCommon};
use crate::compiler::mpy::RawCode;
use crate::compiler::parser::{ParseTree, Pn, pn_is_struct};
use crate::compiler::qstr::{Qstr, QstrContext};
use crate::compiler::scope::{
    IdInfoKind, ScopeArena, ScopeId, ScopeKind,
};
use dispatch::Handler;

/// Sentinel label id used by vendor (see compile.c
/// MP_EMIT_BREAK_FROM_FOR peer constant; the 0xffff label is "no
/// label assigned").
pub const INVALID_LABEL: usize = 0xffff;

// ─────────────────────────────────────────────────────────────────────────
// §4 — Well-known qstr block.
// ─────────────────────────────────────────────────────────────────────────

const WK_NAMES: [&str; 27] = [
    "range", // 0
    "super", // 1
    "__class__", // 2
    "__init__", // 3
    "__locals__", // 4
    "__module__", // 5
    "__name__", // 6
    "__qualname__", // 7
    "__doc__", // 8
    "_", // 9
    "<module>", // 10
    "<lambda>", // 11
    "<listcomp>", // 12
    "<dictcomp>", // 13
    "<setcomp>", // 14
    "<genexpr>", // 15
    "<repl_print>", // 16  (REPL only)
    "StopIteration", // 17
    "AssertionError", // 18
    "__aiter__", // 19  (async/await — A2/A3)
    "__anext__", // 20
    "__aenter__", // 21
    "__aexit__", // 22
    "StopAsyncIteration", // 23
    "update", // 24 — dict.update, used by PEP 448 {**d} unpack emit
    "append", // 25 — list.append, used by PEP 448 [a, *b] unpack emit
    "extend", // 26 — list.extend, used by PEP 448 [a, *b] unpack emit
];

const WK_LEN: usize = WK_NAMES.len();

/// Index constants into `WK_NAMES` / `wk_cache` (Zig: `WK_IDX`).
/// The table is the complete Zig WK_IDX set; entries not yet used by
/// Phase B (e.g. `INIT`, `LAMBDA`, `STOP_ITERATION`) are reserved for
/// later phases, so the module is exempt from dead-code warnings.
#[allow(dead_code)]
pub(crate) mod wk_idx {
    pub const RANGE: usize = 0;
    pub const SUPER: usize = 1;
    pub const CLASS: usize = 2;
    pub const INIT: usize = 3;
    pub const LOCALS: usize = 4;
    pub const MODULE: usize = 5;
    pub const NAME: usize = 6;
    pub const QUALNAME: usize = 7;
    pub const DOC: usize = 8;
    pub const UNDERSCORE: usize = 9;
    pub const MODULE_TAG: usize = 10;
    pub const LAMBDA: usize = 11;
    pub const LISTCOMP: usize = 12;
    pub const DICTCOMP: usize = 13;
    pub const SETCOMP: usize = 14;
    pub const GENEXPR: usize = 15;
    pub const REPL_PRINT: usize = 16;
    pub const STOP_ITERATION: usize = 17;
    pub const ASSERTION_ERROR: usize = 18;
    pub const AITER: usize = 19;
    pub const ANEXT: usize = 20;
    pub const AENTER: usize = 21;
    pub const AEXIT: usize = 22;
    pub const STOP_ASYNC_ITERATION: usize = 23;
    pub const UPDATE: usize = 24;
    pub const APPEND: usize = 25;
    pub const EXTEND: usize = 26;
}

// ─────────────────────────────────────────────────────────────────────────
// §6 — Compiler struct + SyntaxErrorInfo.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxErrorKind {
    Syntax,
    Indentation,
}

#[derive(Debug, Clone, Copy)]
pub struct SyntaxErrorInfo {
    pub line: usize,
    pub kind: SyntaxErrorKind,
    pub msg: &'static str,
    pub q_arg: Option<Qstr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileError {
    OutOfMemory,
    SyntaxError,
    IndentationError,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::OutOfMemory => write!(f, "out of memory"),
            CompileError::SyntaxError => write!(f, "invalid syntax"),
            CompileError::IndentationError => write!(f, "unexpected indent"),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<crate::compiler::parser::ParseError> for CompileError {
    /// Same mapping as `bridge.zig`: `DedentMismatch` surfaces as an
    /// indentation error.
    fn from(e: crate::compiler::parser::ParseError) -> CompileError {
        use crate::compiler::parser::ParseError;
        match e {
            ParseError::OutOfMemory => CompileError::OutOfMemory,
            ParseError::SyntaxError => CompileError::SyntaxError,
            ParseError::IndentationError | ParseError::DedentMismatch => {
                CompileError::IndentationError
            }
        }
    }
}

pub struct Compiler {
    pub(crate) qctx: QstrContext,
    pub(crate) tree: ParseTree,
    pub(crate) is_repl: bool,

    pub(crate) pass: Pass,
    pub(crate) have_star: bool,

    pub(crate) compile_error: Option<SyntaxErrorInfo>,
    pub(crate) compile_error_line: usize,

    pub(crate) next_label: u16,
    pub(crate) num_dict_params: u16,
    pub(crate) num_default_params: u16,

    /// High bit (`MP_EMIT_BREAK_FROM_FOR`) tags "break out of a for
    /// loop" so the unwind path can pop the iterator. Vendor uses the
    /// same encoding.
    pub(crate) break_label: usize,
    pub(crate) continue_label: usize,
    pub(crate) cur_except_level: u16,
    pub(crate) break_continue_except_level: u16,

    pub(crate) scope_head: Option<ScopeId>,
    pub(crate) scope_cur: Option<ScopeId>,

    pub(crate) scopes: ScopeArena,
    pub(crate) emit: Emit,

    /// Lazy-resolved well-known qstr cache. `WK_LEN` entries; 0
    /// sentinel means "not resolved yet" (real qstrs never get id 0).
    pub(crate) wk_cache: [Qstr; WK_LEN],

    /// Scope stash slots (see module docs): struct-node `Pn` →
    /// child `ScopeId`, replacing Zig's pointer writes into parse-tree
    /// node slots.
    pub(crate) scope_slots: HashMap<Pn, ScopeId>,

    /// Per-scope assembled `RawCode` (children filled in at assembly
    /// time from `raw_children`).
    pub(crate) raw_codes: Vec<Option<RawCode>>,
    /// Per-scope child scope ids, in `EmitCommon` order.
    pub(crate) raw_children: Vec<Vec<ScopeId>>,
}

impl Compiler {
    pub fn new(qctx: QstrContext, tree: ParseTree, is_repl: bool) -> Self {
        Compiler {
            qctx,
            tree,
            is_repl,
            pass: Pass::Scope,
            have_star: false,
            compile_error: None,
            compile_error_line: 0,
            next_label: 0,
            num_dict_params: 0,
            num_default_params: 0,
            break_label: INVALID_LABEL,
            continue_label: INVALID_LABEL,
            cur_except_level: 0,
            break_continue_except_level: 0,
            scope_head: None,
            scope_cur: None,
            scopes: ScopeArena::new(),
            emit: Emit::new(),
            wk_cache: [0; WK_LEN],
            scope_slots: HashMap::new(),
            raw_codes: Vec::new(),
            raw_children: Vec::new(),
        }
    }

    /// Resolve a well-known qstr lazily, caching the result. Caller
    /// supplies one of the `wk_idx` constants.
    pub fn wk(&mut self, idx: usize) -> Qstr {
        if self.wk_cache[idx] != 0 {
            return self.wk_cache[idx];
        }
        let id = self
            .qctx
            .from_strn(WK_NAMES[idx].as_bytes())
            .unwrap_or(0);
        self.wk_cache[idx] = id;
        id
    }

    /// Resolve a qstr by literal name. Returns 0 on failure
    /// (mirrors Zig's `qstrFromStrn`, which takes `[]const u8`).
    pub fn qstr_from_strn(&mut self, name: &[u8]) -> Qstr {
        self.qctx.from_strn(name).unwrap_or(0)
    }

    // ─────────────────────────────────────────────────────────────────────
    // §7 — Error plumbing. Field-based; "first error wins" timing
    // matches vendor.
    // ─────────────────────────────────────────────────────────────────────

    fn compile_error_set_line(&mut self, pn: Pn) {
        if self.compile_error_line == 0 && pn_is_struct(pn) {
            self.compile_error_line = self.tree.struct_source_line(pn) as usize;
        }
    }

    pub fn compile_syntax_error(&mut self, pn: Pn, msg: &'static str) {
        if self.compile_error.is_none() {
            self.compile_error = Some(SyntaxErrorInfo {
                line: 0,
                kind: SyntaxErrorKind::Syntax,
                msg,
                q_arg: None,
            });
            self.compile_error_set_line(pn);
            if let Some(e) = self.compile_error.as_mut() {
                e.line = self.compile_error_line;
            }
        }
    }

    /// (Defined in Zig's §7 but never called there; ported for 1:1
    /// parity.)
    pub fn compile_syntax_error_arg(&mut self, pn: Pn, msg: &'static str, q_arg: Qstr) {
        if self.compile_error.is_none() {
            self.compile_error = Some(SyntaxErrorInfo {
                line: 0,
                kind: SyntaxErrorKind::Syntax,
                msg,
                q_arg: Some(q_arg),
            });
            self.compile_error_set_line(pn);
            if let Some(e) = self.compile_error.as_mut() {
                e.line = self.compile_error_line;
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // §9 — Misc small helpers.
    // ─────────────────────────────────────────────────────────────────────

    /// THE only writer of `next_label`. Every label allocation goes
    /// through here so the byte-exact label ordering matches vendor.
    pub fn comp_next_label(&mut self) -> u16 {
        let l = self.next_label;
        self.next_label = self.next_label.wrapping_add(1);
        l
    }

    pub fn compile_increase_except_level(&mut self, label: u16, kind: u8) {
        self.emit
            .setup_block(label as usize, kind, &mut self.scopes);
        self.cur_except_level += 1;
        let sc = self
            .scope_cur
            .expect("compile_increase_except_level: no scope_cur");
        if self.cur_except_level > self.scopes.scopes[sc].exc_stack_size {
            self.scopes.scopes[sc].exc_stack_size = self.cur_except_level;
        }
    }

    pub fn compile_decrease_except_level(&mut self) {
        debug_assert!(self.cur_except_level > 0);
        self.cur_except_level -= 1;
        // Vendor compile.c:306-309 always emits END_FINALLY here. It's
        // the sentinel the VM looks at when unwinding a try/except/
        // finally block to decide whether to re-raise (no match) or
        // fall through.
        self.emit.end_finally(&mut self.scopes);
    }

    /// Allocate a fresh scope in the arena, keeping the per-scope
    /// `raw_codes` / `raw_children` slots in lock-step (ids are dense
    /// 0..n, so the slot index is the scope id).
    pub(crate) fn new_scope(&mut self, kind: ScopeKind, pn: Pn, emit_options: u16) -> ScopeId {
        let id = self.scopes.new_scope(kind, pn, emit_options, &self.tree);
        self.raw_codes.push(None);
        self.raw_children.push(Vec::new());
        debug_assert_eq!(id, self.raw_codes.len() - 1);
        id
    }

    /// Allocate a Scope, link it into the chain, and bump the child
    /// counter. Mirrors vendor `scope_new_and_link`.
    pub(crate) fn scope_new_and_link(
        &mut self,
        kind: ScopeKind,
        pn: Pn,
        emit_options: u16,
    ) -> ScopeId {
        let s = self.new_scope(kind, pn, emit_options);
        self.scopes.scopes[s].parent = self.scope_cur;
        // Chain: append to scope_head's tail. Vendor walks the existing
        // list to find the tail (it's short in practice; module + a
        // handful of functions). We do the same.
        if let Some(head) = self.scope_head {
            let mut tail = head;
            while let Some(n) = self.scopes.scopes[tail].next {
                tail = n;
            }
            self.scopes.scopes[tail].next = Some(s);
        } else {
            self.scope_head = Some(s);
        }
        s
    }

    /// Apply `f` over every element of a list rule, or once for the
    /// bare non-list case. Mirrors vendor `apply_to_single_or_list`.
    pub fn apply_to_single_or_list(&mut self, pn_in: Pn, list_rule: u8, f: Handler) {
        let list = self.tree.extract_list(pn_in, list_rule);
        for item in list {
            f(self, item);
        }
    }

    /// Vendor `compile_load_id` (compile.c:355). In SCOPE pass we
    /// register the binding as GLOBAL_IMPLICIT (later passes may
    /// promote it to LOCAL or CELL/FREE via scope_close_over). In
    /// codegen passes id_op dispatches on the resolved kind.
    pub fn compile_load_id(&mut self, q: Qstr) {
        let sc = self.scope_cur.expect("compile_load_id: no scope_cur");
        if self.pass == Pass::Scope {
            self.scopes
                .find_or_add(sc, q, IdInfoKind::GlobalImplicit);
            return;
        }
        self.emit.id_op(IdOp::Load, sc, q, &mut self.scopes);
    }

    /// Vendor `compile_store_id` (compile.c:367). In SCOPE pass:
    /// `get_id_for_modification` registers as GLOBAL_IMPLICIT, then
    /// rebinds to LOCAL in func-like scopes (so future references see
    /// a local).
    pub fn compile_store_id(&mut self, q: Qstr) {
        let sc = self.scope_cur.expect("compile_store_id: no scope_cur");
        if self.pass == Pass::Scope {
            EmitCommon::get_id_for_modification(&mut self.scopes, sc, q);
            return;
        }
        self.emit.id_op(IdOp::Store, sc, q, &mut self.scopes);
    }

    /// Vendor `compile_delete_id` (compile.c:379). Same SCOPE-pass
    /// rebind as store (a `del x` makes `x` a local in a function
    /// scope).
    pub fn compile_delete_id(&mut self, q: Qstr) {
        let sc = self.scope_cur.expect("compile_delete_id: no scope_cur");
        if self.pass == Pass::Scope {
            EmitCommon::get_id_for_modification(&mut self.scopes, sc, q);
            return;
        }
        self.emit.id_op(IdOp::Delete, sc, q, &mut self.scopes);
    }

    /// The scope chain (`scope_head` → `next`), in order.
    pub(crate) fn scope_chain(&self) -> Vec<ScopeId> {
        let mut out = Vec::new();
        let mut s = self.scope_head;
        while let Some(id) = s {
            out.push(id);
            s = self.scopes.scopes[id].next;
        }
        out
    }
}
