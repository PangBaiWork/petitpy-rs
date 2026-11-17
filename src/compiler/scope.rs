//! Compiler-track scope analysis — pure Rust.
//!
//! Port of `src/compiler/scope.zig` (605 lines), which replaces vendor
//! `py/scope.c` (154 LOC) and `src/frontend/scope_shim.c`. Consumed by
//! the future compile phase for name resolution: every Python block
//! (module, class, function, lambda, the four comprehension flavours)
//! becomes a `Scope` carrying the per-name `IdInfo` table.
//!
//! ## Divergence: arena instead of heap pointers
//!
//! The Zig original allocates each `Scope` on the heap and links them
//! with raw `*Scope` pointers (`parent`, `next`), and hands out
//! `*IdInfo` pointers from `find` / `findOrAdd`. Rust cannot do that
//! without `unsafe`, so the Rust port stores scopes in a `ScopeArena`
//! (`Vec<Scope>`) and refers to them by `ScopeId` (an index); `find`
//! / `findOrAdd` return the index of the `IdInfo` entry in that
//! scope's table instead of a pointer. Indices stay valid across
//! growth (unlike pointers), which is exactly what the closure walk
//! needs: `checkToCloseOver` holds an `IdInfo` handle while mutating
//! the same scope, and an `Rc<RefCell>` encoding would double-borrow
//! panic there.
//!
//! Test assertions are ported verbatim; only construction / linking
//! syntax changes (`arena.new_scope(...)`, `arena.scopes[id].parent =
//! Some(other)`). `Scope::new` needs a `&ParseTree` to extract the
//! simple_name for FUNCTION/CLASS (the Zig code reads the PnStruct
//! directly off the pointer).
//!
//! Other divergences (same as Zig's own list):
//!   1. `raw_code` is `usize` (0 = null) — the emitter wires it later.
//!   2. The `next` sibling-chain field is exposed but never written
//!      by Scope itself (the compile driver owns it), exactly as
//!      vendor scope.c never touches it.
//!   3. simple_name lookup via pinned qstr ids (see the pin-check
//!      test below — in Rust it is a test, not a comptime assert,
//!      because QSTR_STRINGS is a `static`, not usable in const eval).

use crate::compiler::parser::{pn_is_struct, pn_leaf_arg, ParseTree, Pn};
use crate::compiler::qstr::Qstr;

/// A scope handle — an index into `ScopeArena::scopes`.
pub type ScopeId = usize;

/// Scope kind (mirrors vendor `scope_kind_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScopeKind {
    Module = 0,
    Class = 1,
    Lambda = 2,
    ListComp = 3,
    DictComp = 4,
    SetComp = 5,
    GenExpr = 6,
    Function = 7,
}

impl ScopeKind {
    /// SCOPE_IS_FUNC_LIKE(s) := (s) >= SCOPE_LAMBDA.
    pub fn is_func_like(self) -> bool {
        (self as u8) >= (ScopeKind::Lambda as u8)
    }

    /// SCOPE_IS_COMP_LIKE(s) := SCOPE_LIST_COMP <= (s) <= SCOPE_GEN_EXPR.
    pub fn is_comp_like(self) -> bool {
        let v = self as u8;
        v >= (ScopeKind::ListComp as u8) && v <= (ScopeKind::GenExpr as u8)
    }
}

/// Id-info kind (mirrors vendor `id_info_kind_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdInfoKind {
    Undecided = 0,
    GlobalImplicit = 1,
    GlobalImplicitAssigned = 2,
    GlobalExplicit = 3,
    /// In a function f, written and only referenced by f.
    Local = 4,
    /// In a function f, read/written by children of f.
    Cell = 5,
    /// In a function f, belongs to the parent of f.
    Free = 6,
}

/// ID_FLAG_* bitmasks (mirrors enum in py/scope.h).
pub const ID_FLAG_IS_PARAM: u8 = 0x01;
pub const ID_FLAG_IS_STAR_PARAM: u8 = 0x02;
pub const ID_FLAG_IS_DBL_STAR_PARAM: u8 = 0x04;
pub const ID_FLAG_VIPER_TYPE_POS: u8 = 4;

/// SCOPE_FLAG_* bitmasks (mirrors py/runtime0.h).
///
/// Only the bits compile.c actually ORs into scope.scope_flags. The
/// native-emitter / viper bits are present in vendor runtime0.h but
/// unused under nano config; ported here verbatim so the compile phase
/// doesn't need a second copy.
pub const SCOPE_FLAG_ALL_SIG: u16 = 0x0f;
pub const SCOPE_FLAG_GENERATOR: u16 = 0x01;
pub const SCOPE_FLAG_VARKEYWORDS: u16 = 0x02;
pub const SCOPE_FLAG_VARARGS: u16 = 0x04;
pub const SCOPE_FLAG_DEFKWARGS: u16 = 0x08;
pub const SCOPE_FLAG_REFGLOBALS: u16 = 0x10;
pub const SCOPE_FLAG_HASCONSTS: u16 = 0x20;

/// Allocation tuning (matches MICROPY_ALLOC_SCOPE_ID_* in mpconfig.h).
const ID_INFO_INIT: usize = 4; // MICROPY_ALLOC_SCOPE_ID_INIT
const ID_INFO_INC: usize = 6; // MICROPY_ALLOC_SCOPE_ID_INC

/// Per-name info (mirrors `id_info_t` — same wire layout).
///
/// Field order matches vendor: u8 kind, u8 flags, u16 local_num,
/// then qst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdInfo {
    pub kind: IdInfoKind,
    pub flags: u8,
    /// When kind == Local this is the unique local number.
    /// When kind == Cell / Free this is the closed-over variable's number.
    pub local_num: u16,
    pub qst: Qstr,
}

impl IdInfo {
    fn new(kind: IdInfoKind, qst: Qstr) -> Self {
        IdInfo {
            kind,
            flags: 0,
            local_num: 0,
            qst,
        }
    }
}

// ── simple_name lookup table (vendor scope_simple_name_table) ────────────
//
// Vendor uses C99 designated initialisers indexed by scope_kind_t. We
// inline the six QDEF0 ids that are known at comptime (and assert in a
// test below that the embedded qstrdefs hasn't drifted from these
// pins). FUNCTION and CLASS are 0 sentinels — `new_scope` handles them
// via the parse-node extraction path instead.
//
// QDEF0 indices in vendor/micropython_embed/genhdr/qstrdefs.generated.h:
//   <module>    -> id  7
//   <dictcomp>  -> id 166
//   <genexpr>   -> id 167
//   <lambda>    -> id 168
//   <listcomp>  -> id 169
//   <setcomp>   -> id 170
//
// All six fit under UINT8_MAX (255) — vendor relies on the same
// invariant for its uint8_t table.

const SIMPLE_NAME_QSTR_MODULE: Qstr = 7;
const SIMPLE_NAME_QSTR_DICTCOMP: Qstr = 166;
const SIMPLE_NAME_QSTR_GENEXPR: Qstr = 167;
const SIMPLE_NAME_QSTR_LAMBDA: Qstr = 168;
const SIMPLE_NAME_QSTR_LISTCOMP: Qstr = 169;
const SIMPLE_NAME_QSTR_SETCOMP: Qstr = 170;

/// Map ScopeKind -> simple_name qstr id (for the kinds that have one).
/// FUNCTION/CLASS return 0; the caller must extract simple_name from
/// the parse node instead.
fn simple_name_qstr(k: ScopeKind) -> Qstr {
    match k {
        ScopeKind::Module => SIMPLE_NAME_QSTR_MODULE,
        ScopeKind::Lambda => SIMPLE_NAME_QSTR_LAMBDA,
        ScopeKind::ListComp => SIMPLE_NAME_QSTR_LISTCOMP,
        ScopeKind::DictComp => SIMPLE_NAME_QSTR_DICTCOMP,
        ScopeKind::SetComp => SIMPLE_NAME_QSTR_SETCOMP,
        ScopeKind::GenExpr => SIMPLE_NAME_QSTR_GENEXPR,
        ScopeKind::Function | ScopeKind::Class => 0,
    }
}

/// A single scope (mirrors vendor `scope_t` / Zig `Scope`).
///
/// `parent` / `next` are arena handles, not pointers; `raw_code` is a
/// `usize` address (0 = null) for the same reason. `id_info` is a
/// plain `Vec` — capacity is pre-sized to ID_INFO_INIT and grows by
/// exactly ID_INFO_INC, matching vendor's m_renew cadence.
#[derive(Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    /// Sibling chain — populated by the compile driver, never touched
    /// by Scope itself. Exactly as vendor scope.c never writes
    /// scope_t.next; compile.c owns that list.
    pub next: Option<ScopeId>,
    pub pn: Pn,
    /// Deferred to the compile phase (mp_emit_glue_new_raw_code
    /// wiring). Stays 0 through scope analysis; the emitter assigns
    /// it later.
    pub raw_code: usize,
    pub simple_name: Qstr,
    pub scope_flags: u16,
    pub emit_options: u16,
    pub num_pos_args: u16,
    pub num_kwonly_args: u16,
    pub num_def_pos_args: u16,
    pub num_locals: u16,
    pub stack_size: u16,
    pub exc_stack_size: u16,
    pub id_info: Vec<IdInfo>,
    /// Has this scope been driven through the SCOPE pass yet? The
    /// compiler's iterative driver uses this to avoid re-processing
    /// (the heuristic "no locals + no ids + no pos_args"
    /// misclassifies empty function bodies like `def f(): pass` as
    /// unprocessed and caused unbounded recursion).
    pub scope_pass_done: bool,
}

/// Arena owning every `Scope`. Methods on the arena replace the Zig
/// `*Scope` methods; `ScopeId` handles replace the pointers.
#[derive(Debug, Default)]
pub struct ScopeArena {
    pub scopes: Vec<Scope>,
}

impl ScopeArena {
    pub fn new() -> Self {
        ScopeArena { scopes: Vec::new() }
    }

    /// Allocate a fresh Scope in the arena and return its handle. For
    /// FUNCTION/CLASS the `pn` must be a PnStruct whose nodes[0] is an
    /// ID leaf (the function/class name); we extract its qstr arg into
    /// simple_name. For the other six kinds the simple_name is the
    /// well-known qstr from the pinned table above.
    ///
    /// Pre-sizes id_info to ID_INFO_INIT (=4) entries to match
    /// vendor's scope->id_info_alloc bootstrap. The Vec stays empty
    /// (len == 0) — capacity is just reserved so the first few
    /// find_or_add calls don't allocate.
    pub fn new_scope(
        &mut self,
        kind: ScopeKind,
        pn: Pn,
        emit_options: u16,
        tree: &ParseTree,
    ) -> ScopeId {
        let simple_name = if kind == ScopeKind::Function || kind == ScopeKind::Class {
            // Vendor asserts MP_PARSE_NODE_IS_STRUCT(pn) here; preserve.
            debug_assert!(pn_is_struct(pn));
            let nodes = tree.struct_nodes(pn);
            // nodes[0] is the ID leaf — pull its arg as the qstr.
            pn_leaf_arg(nodes[0]) as Qstr
        } else {
            simple_name_qstr(kind)
        };
        let mut id_info: Vec<IdInfo> = Vec::new();
        id_info.reserve_exact(ID_INFO_INIT);
        self.scopes.push(Scope {
            kind,
            parent: None,
            next: None,
            pn,
            raw_code: 0,
            simple_name,
            scope_flags: 0,
            emit_options,
            num_pos_args: 0,
            num_kwonly_args: 0,
            num_def_pos_args: 0,
            num_locals: 0,
            stack_size: 0,
            exc_stack_size: 0,
            id_info,
            scope_pass_done: false,
        });
        self.scopes.len() - 1
    }

    // ── Lookup / insert ───────────────────────────────────────────────

    /// Linear scan over the scope's IdInfo table. Returns the entry
    /// index on hit, None on miss. Mirrors vendor scope_find().
    pub fn find(&self, scope: ScopeId, q: Qstr) -> Option<usize> {
        self.scopes[scope]
            .id_info
            .iter()
            .position(|info| info.qst == q)
    }

    /// Find or insert. Returns the existing entry's index on hit;
    /// otherwise appends a new entry with the given kind (and default
    /// flags=0 / local_num=0) and returns its index. Mirrors vendor
    /// scope_find_or_add_id().
    ///
    /// Growth: matches vendor's m_renew(old, old + ID_INFO_INC) —
    /// when capacity is exhausted we grow by exactly +6 slots. This
    /// keeps the realloc cadence identical to vendor (4 → 10 → 16 →
    /// 22 → ...), so any future allocator-sensitive bug surfaces in
    /// the same pattern.
    pub fn find_or_add(&mut self, scope: ScopeId, q: Qstr, kind: IdInfoKind) -> usize {
        if let Some(idx) = self.find(scope, q) {
            return idx;
        }
        let id_info = &mut self.scopes[scope].id_info;
        if id_info.len() >= id_info.capacity() {
            let new_cap = id_info.capacity() + ID_INFO_INC;
            id_info.reserve_exact(new_cap - id_info.len());
        }
        id_info.push(IdInfo::new(kind, q));
        id_info.len() - 1
    }

    /// Walk to the outermost (module) scope, then find() there.
    /// Returns the module scope's handle plus the entry index.
    /// Mirrors vendor scope_find_global().
    pub fn find_global(&self, scope: ScopeId, q: Qstr) -> Option<(ScopeId, usize)> {
        let mut s = scope;
        while let Some(p) = self.scopes[s].parent {
            s = p;
        }
        self.find(s, q).map(|idx| (s, idx))
    }

    // ── Closure marking ───────────────────────────────────────────────

    /// If `id` shadows a binding in any non-module ancestor, mark it
    /// FREE in `scope` and propagate CELL/FREE upward. Mirrors vendor
    /// scope_check_to_close_over() — direct port of scope.c:139-152.
    ///
    /// `id_idx` is the index of the entry in `scope`'s own table
    /// (as returned by `find_or_add`).
    pub fn check_to_close_over(&mut self, scope: ScopeId, id_idx: usize) {
        if self.scopes[scope].parent.is_none() {
            return;
        }

        let q = self.scopes[scope].id_info[id_idx].qst;
        let mut s = self.scopes[scope].parent;
        // Walk while s has a parent — we don't peek the outer (module)
        // scope, matching vendor's `for (s = parent; s->parent != NULL;
        // s = s->parent)`.
        while let Some(sp) = s {
            if self.scopes[sp].parent.is_none() {
                break;
            }
            if let Some(id2_idx) = self.find(sp, q) {
                let kind = self.scopes[sp].id_info[id2_idx].kind;
                if kind == IdInfoKind::Local
                    || kind == IdInfoKind::Cell
                    || kind == IdInfoKind::Free
                {
                    self.scopes[scope].id_info[id_idx].kind = IdInfoKind::Free;
                    self.close_over_in_parents(scope, q);
                }
                return;
            }
            s = self.scopes[sp].parent;
        }
    }

    /// Walk parent chain, marking the binding's home as CELL and any
    /// intermediate scope as FREE. Direct port of vendor
    /// scope_close_over_in_parents() at scope.c:116-137.
    ///
    /// Invariants (asserted, matching vendor):
    ///   - scope has at least one parent (caller checked).
    ///   - We never reach the outer (module) scope — each parent must
    ///     itself have a parent. The vendor's inner
    ///     `assert(s->parent != NULL)` would fire on a misuse; we keep
    ///     the assert in debug builds.
    fn close_over_in_parents(&mut self, scope: ScopeId, q: Qstr) {
        debug_assert!(self.scopes[scope].parent.is_some());
        let mut s = self.scopes[scope].parent.expect("scope has a parent");
        loop {
            debug_assert!(self.scopes[s].parent.is_some()); // never the outer scope
            let id_idx = self.find_or_add(s, q, IdInfoKind::Undecided);
            let kind = self.scopes[s].id_info[id_idx].kind;
            if kind == IdInfoKind::Undecided {
                // Not previously declared here — declare as free and
                // keep walking upward.
                self.scopes[s].id_info[id_idx].kind = IdInfoKind::Free;
            } else {
                // Found a binding — promote LOCAL to CELL, leave
                // FREE/CELL alone, and stop.
                if kind == IdInfoKind::Local {
                    self.scopes[s].id_info[id_idx].kind = IdInfoKind::Cell;
                } else {
                    debug_assert!(
                        kind == IdInfoKind::Free || kind == IdInfoKind::Cell
                    );
                }
                return;
            }
            s = self.scopes[s].parent.expect("parent has a parent");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::grammar_data as grammar;
    use crate::compiler::parser::{pn_new_leaf, PN_ID, PN_NULL};
    use crate::compiler::qstr::QstrContext;

    /// Pin-check: if a vendor regen shuffles qstr ids, fail here
    /// rather than emit mysteriously-wrong simple_names downstream.
    /// (Zig does this as a comptime block; Rust cannot const-eval
    /// the static pool, so it is a test.)
    #[test]
    fn simple_name_qstr_pins() {
        let ctx = QstrContext::new();
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_MODULE), b"<module>");
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_DICTCOMP), b"<dictcomp>");
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_GENEXPR), b"<genexpr>");
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_LAMBDA), b"<lambda>");
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_LISTCOMP), b"<listcomp>");
        assert_eq!(ctx.qstr_str(SIMPLE_NAME_QSTR_SETCOMP), b"<setcomp>");
        // Width check: matches vendor's MP_STATIC_ASSERT(... <= UINT8_MAX).
        assert!(SIMPLE_NAME_QSTR_SETCOMP <= 0xff);
    }

    #[test]
    fn scope_new_module_gets_module_simple_name_and_empty_id_info() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let s = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        assert_eq!(arena.scopes[s].kind, ScopeKind::Module);
        assert_eq!(arena.scopes[s].simple_name, SIMPLE_NAME_QSTR_MODULE);
        assert_eq!(arena.scopes[s].id_info.len(), 0);
        // Pre-sized to ID_INFO_INIT — no allocation on the first 4 inserts.
        assert!(arena.scopes[s].id_info.capacity() >= ID_INFO_INIT);
    }

    #[test]
    fn scope_new_each_comprehension_kind_picks_right_qstr() {
        let cases = [
            (ScopeKind::Lambda, SIMPLE_NAME_QSTR_LAMBDA),
            (ScopeKind::ListComp, SIMPLE_NAME_QSTR_LISTCOMP),
            (ScopeKind::DictComp, SIMPLE_NAME_QSTR_DICTCOMP),
            (ScopeKind::SetComp, SIMPLE_NAME_QSTR_SETCOMP),
            (ScopeKind::GenExpr, SIMPLE_NAME_QSTR_GENEXPR),
        ];
        let tree = ParseTree::new();
        for (kind, want) in cases {
            let mut arena = ScopeArena::new();
            let s = arena.new_scope(kind, PN_NULL, 0, &tree);
            assert_eq!(arena.scopes[s].simple_name, want);
        }
    }

    #[test]
    fn scope_new_function_extracts_simple_name_from_pnstruct_nodes0() {
        // Hand-build a PnStruct whose nodes[0] is an ID leaf carrying
        // a synthetic qstr. We don't go through the parser; the struct
        // is allocated in a scratch tree and dropped after.
        let want_qstr: usize = 42;
        let mut tree = ParseTree::new();
        let pn = tree.alloc_struct(
            1,
            grammar::RULE_funcdef,
            vec![pn_new_leaf(PN_ID, want_qstr)],
        );
        let mut arena = ScopeArena::new();
        let s = arena.new_scope(ScopeKind::Function, pn, 0, &tree);
        assert_eq!(arena.scopes[s].simple_name, want_qstr as Qstr);
    }

    #[test]
    fn find_hit_and_miss() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let s = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);

        assert_eq!(arena.find(s, 100), None);
        let idx = arena.find_or_add(s, 100, IdInfoKind::Local);
        let hit = arena.find(s, 100).expect("hit");
        assert_eq!(hit, idx);
        assert_eq!(arena.scopes[s].id_info[hit].qst, 100);
        assert_eq!(arena.scopes[s].id_info[hit].kind, IdInfoKind::Local);
        assert_eq!(arena.find(s, 101), None);
    }

    #[test]
    fn find_or_add_dedup_returns_same_index_growth_past_id_info_init() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let s = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);

        let a1 = arena.find_or_add(s, 10, IdInfoKind::Local);
        let a2 = arena.find_or_add(s, 10, IdInfoKind::GlobalExplicit); // kind ignored on hit
        assert_eq!(a1, a2);
        assert_eq!(arena.scopes[s].id_info[a2].kind, IdInfoKind::Local); // hit preserves original
        assert_eq!(arena.scopes[s].id_info.len(), 1);

        // Insert past ID_INFO_INIT (=4) to force growth to 4 + ID_INFO_INC (=10).
        arena.find_or_add(s, 11, IdInfoKind::Undecided);
        arena.find_or_add(s, 12, IdInfoKind::Undecided);
        arena.find_or_add(s, 13, IdInfoKind::Undecided); // fills cap=4
        arena.find_or_add(s, 14, IdInfoKind::Undecided); // triggers grow
        assert_eq!(arena.scopes[s].id_info.len(), 5);
        assert!(arena.scopes[s].id_info.capacity() >= ID_INFO_INIT + ID_INFO_INC);

        // New entries have default flags=0 / local_num=0.
        let e = arena.find(s, 14).expect("hit");
        assert_eq!(arena.scopes[s].id_info[e].flags, 0);
        assert_eq!(arena.scopes[s].id_info[e].local_num, 0);
    }

    #[test]
    fn find_global_walks_parent_chain_to_module() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let m = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        let inner = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        arena.scopes[inner].parent = Some(m);

        arena.find_or_add(m, 200, IdInfoKind::GlobalExplicit);
        let (mscope, hit) = arena.find_global(inner, 200).expect("global hit");
        assert_eq!(mscope, m);
        assert_eq!(arena.scopes[mscope].id_info[hit].qst, 200);
        assert_eq!(arena.find_global(inner, 201), None);
    }

    #[test]
    fn check_to_close_over_local_in_parent_free_in_child_cell_in_parent() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let m = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        let outer = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        let inner = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        arena.scopes[outer].parent = Some(m);
        arena.scopes[inner].parent = Some(outer);

        let outer_id = arena.find_or_add(outer, 300, IdInfoKind::Local);
        let inner_id = arena.find_or_add(inner, 300, IdInfoKind::Undecided);

        arena.check_to_close_over(inner, inner_id);
        assert_eq!(arena.scopes[inner].id_info[inner_id].kind, IdInfoKind::Free);
        assert_eq!(arena.scopes[outer].id_info[outer_id].kind, IdInfoKind::Cell);
    }

    #[test]
    fn check_to_close_over_no_parent_noop() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let m = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        let id = arena.find_or_add(m, 400, IdInfoKind::Undecided);
        arena.check_to_close_over(m, id);
        assert_eq!(arena.scopes[m].id_info[id].kind, IdInfoKind::Undecided);
    }

    #[test]
    fn check_to_close_over_free_in_ancestor_child_free_ancestor_unchanged() {
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let m = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        let mid = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        let inner = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        arena.scopes[mid].parent = Some(m);
        arena.scopes[inner].parent = Some(mid);

        let mid_id = arena.find_or_add(mid, 500, IdInfoKind::Free);
        let inner_id = arena.find_or_add(inner, 500, IdInfoKind::Undecided);

        arena.check_to_close_over(inner, inner_id);
        assert_eq!(arena.scopes[inner].id_info[inner_id].kind, IdInfoKind::Free);
        assert_eq!(arena.scopes[mid].id_info[mid_id].kind, IdInfoKind::Free); // unchanged
    }

    #[test]
    fn close_over_in_parents_depth3_chain_intermediate_free_root_local_cell() {
        // module ← outer (LOCAL x) ← mid (no x) ← inner (refs x)
        let mut arena = ScopeArena::new();
        let tree = ParseTree::new();
        let m = arena.new_scope(ScopeKind::Module, PN_NULL, 0, &tree);
        let outer = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        let mid = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        let inner = arena.new_scope(ScopeKind::Lambda, PN_NULL, 0, &tree);
        arena.scopes[outer].parent = Some(m);
        arena.scopes[mid].parent = Some(outer);
        arena.scopes[inner].parent = Some(mid);

        let outer_id = arena.find_or_add(outer, 600, IdInfoKind::Local);
        let inner_id = arena.find_or_add(inner, 600, IdInfoKind::Undecided);

        arena.check_to_close_over(inner, inner_id);
        assert_eq!(arena.scopes[inner].id_info[inner_id].kind, IdInfoKind::Free);
        assert_eq!(arena.scopes[outer].id_info[outer_id].kind, IdInfoKind::Cell);

        // mid gets a synthesised FREE slot inserted by close_over_in_parents.
        let mid_id = arena.find(mid, 600).expect("mid inserted");
        assert_eq!(arena.scopes[mid].id_info[mid_id].kind, IdInfoKind::Free);
    }

    #[test]
    fn scope_kind_is_func_like_is_comp_like_vendor_predicates() {
        assert!(!ScopeKind::Module.is_func_like());
        assert!(!ScopeKind::Class.is_func_like());
        assert!(ScopeKind::Lambda.is_func_like());
        assert!(ScopeKind::ListComp.is_func_like());
        assert!(ScopeKind::Function.is_func_like());

        assert!(!ScopeKind::Module.is_comp_like());
        assert!(!ScopeKind::Lambda.is_comp_like());
        assert!(ScopeKind::ListComp.is_comp_like());
        assert!(ScopeKind::DictComp.is_comp_like());
        assert!(ScopeKind::SetComp.is_comp_like());
        assert!(ScopeKind::GenExpr.is_comp_like());
        assert!(!ScopeKind::Function.is_comp_like());
    }
}
