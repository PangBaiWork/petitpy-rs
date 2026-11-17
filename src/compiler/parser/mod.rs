//! Compiler-track parser — pure Rust Python parser.
//!
//! Port of `src/compiler/parser.zig` (1,878 lines), split across
//! submodules so no file exceeds 800 lines (the A2 lexer precedent):
//! this file holds the public types, the `Pn` bit-packing, the arena,
//! the `Parser` state, the main parse loop and the `parse()` entry
//! point; `fold.rs` holds the constant-folding family; `nodes.rs`
//! holds token-to-node conversion, `push_result_rule`, the
//! const-bool predicates, `parse_node_extract_list` and the tree
//! dump; `names.rs` (generated) maps rule ids to short names.
//!
//! ## Divergence: arena instead of the chunk bump allocator
//!
//! The Zig original stores parse nodes in a chunk-allocated slab
//! chain and encodes struct nodes as raw pointers (`Pn = usize`
//! holding a pointer, low 2 bits zero). Rust cannot do that without
//! `unsafe`, so the Rust port stores nodes in a `Vec<ArenaNode>`
//! owned by the `ParseTree` and encodes a struct node as
//! `(arena_index << 2)` (index 0 reserved, so `Pn == 0` still means
//! `PN_NULL`). The *tag* encoding is identical — leaves, small ints
//! and nulls are bit-packed exactly as in the Zig original, and all
//! the `pn_*` predicates work on the raw `Pn` value — so every
//! algorithm that walks the tree (folding, scope extraction, the
//! future compiler) behaves the same. The only observable difference
//! is that a const-object node's `nodes[2]` slot is always 0 here:
//! the payload lives in the arena entry's `payload` field, not in a
//! chunk buffer the slot points at. `ParseTree` methods read it from
//! there; nothing outside the parser ever sees the slot.
//!
//! ## Divergence: `ParseError` is its own enum
//!
//! Zig's parser defines its own error set (`ParseError =
//! error{OutOfMemory, SyntaxError, IndentationError,
//! DedentMismatch}`), distinct from the runtime `PyError`
//! (`error{PyRaise}`). The Rust port keeps that split: this module's
//! `ParseError` is the parser's error set, and the future compiler
//! will translate it into a `PyError` (a `SyntaxError` exception)
//! at the boundary.

mod fold;
mod names;
mod nodes;
mod pn;
#[cfg(test)]
mod tests;

// Re-export the Pn bit-packing API (defined in pn.rs) so the
// parser's public surface — and the submodules' `super::` imports —
// keep working after the split.
pub use pn::{
    ConstObjectKind, Pn, PN_ID, PN_NULL, PN_SMALL_INT, PN_STRING, PN_TOKEN, pn_is_id,
    pn_is_leaf, pn_is_null, pn_is_small_int, pn_is_struct, pn_is_token_kind, pn_leaf_arg,
    pn_leaf_kind, pn_leaf_small_int, pn_new_leaf, pn_new_small_int,
};
pub(crate) use pn::ConstPayload;

use crate::compiler::grammar_data as grammar;
use crate::compiler::grammar_ext;
use crate::compiler::lexer::{LexError, Lexer, TokenKind};
use crate::compiler::qstr::{Qstr, QstrContext};
use std::fmt;

/// Errors that may arise from `parse()`. SyntaxError / IndentationError /
/// DedentMismatch are distinguished so the caller can build a precise
/// traceback (the parser knows only the failing token; the diagnostic
/// surface is the caller's concern).
///
/// Mirrors Zig `ParseError = error{OutOfMemory, SyntaxError,
/// IndentationError, DedentMismatch}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    OutOfMemory,
    SyntaxError,
    IndentationError,
    DedentMismatch,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::OutOfMemory => write!(f, "out of memory"),
            ParseError::SyntaxError => write!(f, "invalid syntax"),
            ParseError::IndentationError => write!(f, "unexpected indent"),
            ParseError::DedentMismatch => write!(f, "unindent does not match any outer indentation level"),
        }
    }
}

impl std::error::Error for ParseError {}

/// `Result<T, ParseError>` — the Rust spelling of Zig's
/// `ParseError!T`.
pub type ParseResult<T> = Result<T, ParseError>;

impl From<LexError> for ParseError {
    /// The lexer can only fail with OutOfMemory or UnicodeNameEscape;
    /// the latter is a syntax error (same mapping as Zig's
    /// `lexerToNextChecked` / `lexerPeekChecked`).
    fn from(e: LexError) -> ParseError {
        match e {
            LexError::OutOfMemory => ParseError::OutOfMemory,
            LexError::UnicodeNameEscape => ParseError::SyntaxError,
        }
    }
}

/// Mirrors vendor `mp_parse_input_kind_t`. Numbering is internal — it
/// only chooses the top-level grammar rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    SingleInput,
    FileInput,
    EvalInput,
}

/// A fully-parsed module/expression/statement. Owns the arena that
/// backs the tree's struct nodes; `root` is the top-level Pn.
///
/// (Zig: owns the chunk chain; `deinit` walks and frees it. In Rust
/// the `Vec` is dropped automatically — there is no `deinit`.)
#[derive(Debug)]
pub struct ParseTree {
    /// The top-level parse node.
    pub root: Pn,
    /// Arena of struct nodes. Index 0 is reserved (never used) so
    /// that `Pn == 0` still means `PN_NULL` and every struct `Pn`
    /// is `(index << 2)` with the low two bits clear, exactly like
    /// a pointer in the Zig original.
    nodes: Vec<ArenaNode>,
}

/// One struct node in the arena. `children` are the node's child
/// `Pn` values; for a `RULE_const_object` node they are the three
/// vendor slots `[kind, len, 0]` and the real payload lives in
/// `payload` (see the module docs — `nodes[2]` is always 0 here).
#[derive(Debug)]
pub(crate) struct ArenaNode {
    pub(crate) source_line: u32,
    pub(crate) kind: u8,
    pub(crate) children: Vec<Pn>,
    pub(crate) payload: Option<ConstPayload>,
}

impl ParseTree {
    /// Empty tree (root `PN_NULL`, empty arena).
    pub fn new() -> Self {
        ParseTree {
            root: PN_NULL,
            nodes: vec![ArenaNode {
                source_line: 0,
                kind: 0,
                children: Vec::new(),
                payload: None,
            }],
        }
    }

    /// Allocate a struct node with `children` and return its `Pn`.
    pub(crate) fn alloc_struct(&mut self, source_line: u32, kind: u8, children: Vec<Pn>) -> Pn {
        self.nodes.push(ArenaNode {
            source_line,
            kind,
            children,
            payload: None,
        });
        (self.nodes.len() - 1) << 2
    }

    /// Allocate a `RULE_const_object` node carrying `payload` and
    /// return its `Pn`. `children` are the vendor's three slots
    /// `[kind, len, 0]` (see the module docs).
    pub(crate) fn alloc_const_object(
        &mut self,
        source_line: u32,
        kind: ConstObjectKind,
        payload: ConstPayload,
    ) -> Pn {
        let len = match &payload {
            ConstPayload::Bytes(b) => b.len(),
            ConstPayload::Tuple(items) => items.len(),
        };
        self.nodes.push(ArenaNode {
            source_line,
            kind: grammar::RULE_const_object,
            children: vec![kind as usize, len, 0],
            payload: Some(payload),
        });
        (self.nodes.len() - 1) << 2
    }

    /// The arena entry for a struct `Pn`. Panics on a non-struct
    /// `Pn` (mirrors the Zig code dereferencing the pointer).
    fn node(&self, pn: Pn) -> &ArenaNode {
        debug_assert!(pn_is_struct(pn));
        &self.nodes[pn >> 2]
    }

    fn node_mut(&mut self, pn: Pn) -> &mut ArenaNode {
        debug_assert!(pn_is_struct(pn));
        &mut self.nodes[pn >> 2]
    }

    /// Is `pn` a struct node of kind `k`?
    pub fn is_struct_kind(&self, pn: Pn, k: u8) -> bool {
        pn_is_struct(pn) && self.node(pn).kind == k
    }

    /// The rule id of a struct node.
    pub fn struct_kind(&self, pn: Pn) -> u8 {
        self.node(pn).kind
    }

    /// The child count of a struct node.
    pub fn struct_num_nodes(&self, pn: Pn) -> usize {
        self.node(pn).children.len()
    }

    /// The children of a struct node.
    pub fn struct_nodes(&self, pn: Pn) -> &[Pn] {
        &self.node(pn).children
    }

    /// The source line of a struct node.
    pub fn struct_source_line(&self, pn: Pn) -> u32 {
        self.node(pn).source_line
    }

    /// Retag a struct node (used by the testlist_comp_3c path in
    /// `push_result_rule`, which rewrites a child's kind in place).
    pub(crate) fn set_struct_kind(&mut self, pn: Pn, kind: u8) {
        self.node_mut(pn).kind = kind;
    }

    /// The payload of a `RULE_const_object` node, if `pn` is one.
    pub(crate) fn const_payload(&self, pn: Pn) -> Option<&ConstPayload> {
        if !pn_is_struct(pn) {
            return None;
        }
        self.node(pn).payload.as_ref()
    }

    /// Extract a list of children from `pn`, "flattening" the
    /// single-leaf and matching-rule cases. Mirrors vendor
    /// `mp_parse_node_extract_list`:
    ///   - PN_NULL          -> 0 children
    ///   - leaf             -> 1 child (the node itself)
    ///   - struct of `kind` -> all children
    ///   - struct of other  -> 1 child (the node itself)
    ///
    /// (Zig returns a pointer into the node or the slot; Rust
    /// returns an owned vec — same contents.)
    pub fn extract_list(&self, pn: Pn, kind: u8) -> Vec<Pn> {
        if pn_is_null(pn) {
            return Vec::new();
        }
        if pn_is_leaf(pn) {
            return vec![pn];
        }
        if self.struct_kind(pn) != kind {
            return vec![pn];
        }
        self.struct_nodes(pn).to_vec()
    }

    /// `parseNodeIsConstFalse` — is `pn` a const-false node?
    pub fn parse_node_is_const_false(&self, pn: Pn) -> bool {
        self.parse_node_is_const_bool(pn, false)
    }

    /// `parseNodeIsConstTrue` — is `pn` a const-true node?
    pub fn parse_node_is_const_true(&self, pn: Pn) -> bool {
        self.parse_node_is_const_bool(pn, true)
    }
}

// ── Parser state (internal) ───────────────────────────────────────────────

pub(crate) struct RuleStack {
    src_line: usize,
    rule_id: u8,
    arg_i: usize,
}

const PARSE_RULE_INIT: usize = 64;
const PARSE_RESULT_INIT: usize = 32;

/// Parser state. Owns the rule/result stacks, the tree arena, and
/// borrows the lexer + qstr context for the duration of the call
/// (mirrors Zig's `Parser` struct, minus the chunk allocator and
/// the `std.mem.Allocator` — `Vec` handles growth).
pub(crate) struct Parser<'a> {
    rule_stack: Vec<RuleStack>,
    result_stack: Vec<Pn>,
    lex: &'a mut Lexer,
    qctx: &'a mut QstrContext,
    tree: ParseTree,
}

impl<'a> Parser<'a> {
    fn new(lex: &'a mut Lexer, qctx: &'a mut QstrContext) -> Self {
        Parser {
            rule_stack: Vec::with_capacity(PARSE_RULE_INIT),
            result_stack: Vec::with_capacity(PARSE_RESULT_INIT),
            lex,
            qctx,
            tree: ParseTree::new(),
        }
    }

    // ── Rule + result stack helpers ──────────────────────────────────

    fn push_rule(&mut self, src_line: usize, rule_id: u8, arg_i: usize) {
        self.rule_stack.push(RuleStack {
            src_line,
            rule_id,
            arg_i,
        });
    }

    fn push_rule_from_arg(&mut self, arg: u16) {
        let rule_id = (arg & grammar::RULE_ARG_ARG_MASK) as u8;
        let line = self.lex.tok_line();
        self.push_rule(line, rule_id, 0);
    }

    fn pop_rule(&mut self) -> (u8, usize, usize) {
        let rs = self.rule_stack.pop().expect("rule stack underflow");
        (rs.rule_id, rs.arg_i, rs.src_line)
    }

    /// Vendor peek_rule (parse.c:333). Returns the rule_id at the given
    /// depth above the top of the rule stack. peek_rule(0) is the topmost
    /// remaining rule — i.e. the parent of whatever was just popped. Used
    /// only by the MICROPY_COMP_CONST_TUPLE fold path.
    fn peek_rule(&self, n: usize) -> u8 {
        debug_assert!(self.rule_stack.len() > n);
        self.rule_stack[self.rule_stack.len() - 1 - n].rule_id
    }

    fn pop_result(&mut self) -> Pn {
        self.result_stack.pop().expect("result stack underflow")
    }

    fn peek_result(&self, n: usize) -> Pn {
        self.result_stack[self.result_stack.len() - 1 - n]
    }

    fn push_result_node(&mut self, pn: Pn) {
        self.result_stack.push(pn);
    }

    fn qstr_from_strn(&mut self, s: &[u8]) -> ParseResult<Qstr> {
        self.qctx
            .from_strn(s)
            .map_err(|_| ParseError::OutOfMemory)
    }

    // ── Lexer bridges ────────────────────────────────────────────────

    /// Bridge from lexer errors to ParseError. The lexer can only
    /// fail with OutOfMemory or UnicodeNameEscape; the latter we
    /// classify as a syntax error.
    fn lexer_peek_checked(&mut self) -> ParseResult<bool> {
        self.lex.peek_line_ends_with_colon().map_err(ParseError::from)
    }

    fn lexer_to_next_checked(&mut self) -> ParseResult<()> {
        self.lex.to_next().map_err(ParseError::from)
    }

    /// Replaces npy_parse_make_syntax_exc + the nlr_raise path. We
    /// surface the *kind* of error (so the caller can build the right
    /// exception type) but the parser itself doesn't construct an
    /// exception object — there's no obj system at this layer.
    fn raise_syntax_error(&self) -> ParseError {
        match self.lex.tok_kind() {
            TokenKind::INDENT => ParseError::IndentationError,
            TokenKind::DEDENT_MISMATCH => ParseError::DedentMismatch,
            _ => ParseError::SyntaxError,
        }
    }
}

// ── Main parse algorithm ─────────────────────────────────────────────────
//
// Direct port of vendor mp_parse / parser.c:547-759. The mainloop is
// the LR-ish state machine that walks the grammar tables one rule at a
// time. `backtrack` is the across-iteration backtracking flag; the
// inner blocks set it on token-mismatch and re-jump to the body via
// `continue 'mainloop`.

fn parse_impl(
    lex: &mut Lexer,
    qctx: &mut QstrContext,
    input_kind: InputKind,
) -> ParseResult<ParseTree> {
    let mut p = Parser::new(lex, qctx);

    let top_level_rule: u8 = match input_kind {
        InputKind::SingleInput => grammar::RULE_single_input,
        InputKind::EvalInput => grammar::RULE_eval_input,
        InputKind::FileInput => grammar::RULE_file_input,
    };
    let first_line = p.lex.tok_line();
    p.push_rule(first_line, top_level_rule, 0);

    let mut backtrack = false;
    let mut syntax_error = false;

    'mainloop: while !p.rule_stack.is_empty() {
        let (rule_id, mut i, rule_src_line) = p.pop_rule();
        let rule_act = grammar_ext::rule_act(rule_id);
        let rule_arg = grammar_ext::get_rule_arg(rule_id);
        let n: usize = (rule_act & grammar::RULE_ACT_ARG_MASK) as usize;

        let kind = rule_act & grammar::RULE_ACT_KIND_MASK;

        if kind == grammar::RULE_ACT_OR {
            // OR rule: try each alternative in turn until one matches.
            if i > 0 && !backtrack {
                // Child succeeded; we just popped ourselves off so the
                // result is already on the stack.
                continue 'mainloop;
            } else {
                backtrack = false;
            }

            // SOFT KEYWORD: `match` introduces a match statement only
            // when it leads a logical line ending in ':'. Otherwise it
            // is an ordinary name — `match(x)`, `match = 1`.
            //
            // Neither compound_stmt nor simple_stmt can make that call
            // themselves: both would have to consume the NAME before
            // finding out, and the engine has no input backtracking
            // once an AND rule is under way. So the choice happens
            // here, ahead of either, using a lookahead the parser
            // doesn't otherwise have (lexer.zig, SOFT KEYWORDS).
            if rule_id == grammar::RULE_stmt
                && i == 0
                && p.lex.tok_kind() == TokenKind::NAME
                && p.lex.vstr() == b"match"
            {
                if p.lexer_peek_checked()? {
                    p.push_rule(rule_src_line, grammar_ext::RULE_match_stmt, 0);
                    continue 'mainloop;
                }
            }

            let mut matched_child = false;
            while i < n {
                let akind = rule_arg[i] & grammar::RULE_ARG_KIND_MASK;
                if akind == grammar::RULE_ARG_TOK {
                    let want_tok: u16 = rule_arg[i] & grammar::RULE_ARG_ARG_MASK;
                    if (p.lex.tok_kind() as u16) == want_tok {
                        nodes::push_result_token(&mut p, rule_id)?;
                        p.lexer_to_next_checked()?;
                        matched_child = true;
                        break;
                    }
                } else {
                    // RULE_ARG_RULE / RULE_ARG_OPT_RULE — try this sub-
                    // rule; remember our position so we resume after it.
                    if i + 1 < n {
                        p.push_rule(rule_src_line, rule_id, i + 1);
                    }
                    p.push_rule_from_arg(rule_arg[i]);
                    matched_child = true;
                    break;
                }
                i += 1;
            }
            if matched_child {
                continue 'mainloop;
            }
            // No alternative matched. Set backtrack so the caller can
            // try its next alternative (if any).
            backtrack = true;
            continue 'mainloop;
        } else if kind == grammar::RULE_ACT_AND {
            // AND rule: match every arg in sequence.
            if backtrack {
                if (rule_arg[i - 1] & grammar::RULE_ARG_KIND_MASK) == grammar::RULE_ARG_OPT_RULE {
                    // Optional sub-rule missed; push a placeholder and
                    // continue with the next arg.
                    p.push_result_node(PN_NULL);
                    backtrack = false;
                } else if i > 1 {
                    // Required arg failed after we'd partially matched
                    // the rule — hard syntax error.
                    syntax_error = true;
                    break 'mainloop;
                } else {
                    // First arg failed; let parent backtrack.
                    continue 'mainloop;
                }
            }

            let mut pushed_child = false;
            while i < n {
                if (rule_arg[i] & grammar::RULE_ARG_KIND_MASK) == grammar::RULE_ARG_TOK {
                    let want_tok: u16 = rule_arg[i] & grammar::RULE_ARG_ARG_MASK;
                    if (p.lex.tok_kind() as u16) == want_tok {
                        if want_tok == (TokenKind::NAME as u16) {
                            nodes::push_result_token(&mut p, rule_id)?;
                        }
                        p.lexer_to_next_checked()?;
                    } else {
                        if i > 0 {
                            syntax_error = true;
                            break 'mainloop;
                        } else {
                            backtrack = true;
                            continue 'mainloop;
                        }
                    }
                } else {
                    p.push_rule(rule_src_line, rule_id, i + 1);
                    p.push_rule_from_arg(rule_arg[i]);
                    pushed_child = true;
                    break;
                }
                i += 1;
            }
            if pushed_child {
                continue 'mainloop;
            }

            // Matched the whole rule. Build its parse node.
            //
            // Discard lonely-statement doc strings. Under
            // ENABLE_DOC_STRING=0 the vendor parser turns a bare
            // string/const-object expression statement into a `pass`.
            // This keeps every emitted .mpy free of the implicit
            // module/function doc-string slot.
            if input_kind != InputKind::SingleInput
                && rule_id == grammar::RULE_expr_stmt
                && p.peek_result(0) == PN_NULL
            {
                let head = p.peek_result(1);
                if (pn_is_leaf(head) && !pn_is_id(head))
                    || p.tree.is_struct_kind(head, grammar::RULE_const_object)
                {
                    let _ = p.pop_result();
                    let _ = p.pop_result();
                    nodes::push_result_rule(&mut p, rule_src_line, grammar::RULE_pass_stmt, 0)?;
                    continue 'mainloop;
                }
            }

            // Count non-NIL args. We walk the rule args from the end so
            // we can correlate with the result stack (which is built
            // bottom-up — first arg is deepest in the stack).
            i = 0;
            let mut num_not_nil: usize = 0;
            let mut x = n;
            while x > 0 {
                x -= 1;
                if (rule_arg[x] & grammar::RULE_ARG_KIND_MASK) == grammar::RULE_ARG_TOK {
                    let tok_kind = rule_arg[x] & grammar::RULE_ARG_ARG_MASK;
                    if tok_kind == (TokenKind::NAME as u16) {
                        i += 1;
                        num_not_nil += 1;
                    }
                } else {
                    if p.peek_result(i) != PN_NULL {
                        num_not_nil += 1;
                    }
                    i += 1;
                }
            }

            if num_not_nil == 1 && (rule_act & grammar::RULE_ACT_ALLOW_IDENT) != 0 {
                // and_ident collapsing: if the rule has only one non-NIL
                // child, hoist it directly instead of wrapping in a
                // new struct (e.g., test_if_expr with no `if ... else`).
                let mut pn: Pn = PN_NULL;
                let mut xx: usize = 0;
                while xx < i {
                    let pn2 = p.pop_result();
                    if pn2 != PN_NULL {
                        pn = pn2;
                    }
                    xx += 1;
                }
                p.push_result_node(pn);
            } else {
                if (rule_act & grammar::RULE_ACT_ADD_BLANK) != 0 {
                    // and_blank: prepend a sentinel slot for the compiler
                    // (used by funcdef/classdef/lambdef for the
                    // scope-link field).
                    p.push_result_node(PN_NULL);
                    i += 1;
                }
                nodes::push_result_rule(&mut p, rule_src_line, rule_id, i)?;
            }
            continue 'mainloop;
        } else {
            // RULE_ACT_LIST: three flavours encoded in n
            //   n=2: item item+            (one_or_more — no separator)
            //   n=1: item (sep item)*      (list)
            //   n=3: item (sep item)* [sep] (list_with_end)
            //
            // The mainloop tracks position via `i`; arg index alternates
            // between item (i & 1 == 0) and sep (i & 1 == 1) for n != 2.
            // For n == 2, `i & 1 & n == 0` collapses to always 0 so we
            // re-read rule_arg[0] each iteration.
            let mut had_trailing_sep: bool;
            'inner_done: {
                if backtrack {
                    break 'inner_done;
                }
                loop {
                    let arg = rule_arg[i & 1 & n];
                    if (arg & grammar::RULE_ARG_KIND_MASK) == grammar::RULE_ARG_TOK {
                        let want_tok: u16 = arg & grammar::RULE_ARG_ARG_MASK;
                        if (p.lex.tok_kind() as u16) == want_tok {
                            if (i & 1 & n) != 0 {
                                // Separator token — don't push it.
                            } else {
                                nodes::push_result_token(&mut p, rule_id)?;
                            }
                            p.lexer_to_next_checked()?;
                            i += 1;
                        } else {
                            i += 1;
                            backtrack = true;
                            break 'inner_done;
                        }
                    } else {
                        p.push_rule(rule_src_line, rule_id, i + 1);
                        p.push_rule_from_arg(arg);
                        continue 'mainloop;
                    }
                }
            }

            // list_backtrack:
            had_trailing_sep = false;
            if n == 2 {
                if i == 1 {
                    continue 'mainloop;
                } else {
                    backtrack = false;
                }
            } else if i == 1 {
                continue 'mainloop;
            } else if (i & 1) == 1 {
                if n == 3 {
                    had_trailing_sep = true;
                    backtrack = false;
                } else {
                    syntax_error = true;
                    break 'mainloop;
                }
            } else {
                backtrack = false;
            }

            // Element count: i counts both items and separators, so for
            // separated lists we divide by two and account for the +1
            // we just did to mark exit.
            i -= 1;
            if (n & 1) != 0
                && (rule_arg[1] & grammar::RULE_ARG_KIND_MASK) == grammar::RULE_ARG_TOK
            {
                i = (i + 1) / 2;
            }

            if i == 1 {
                if had_trailing_sep {
                    nodes::push_result_rule(&mut p, rule_src_line, rule_id, i)?;
                } else {
                    // Single item with no trailing separator — leave
                    // bare on the stack; no wrapper struct.
                }
            } else {
                nodes::push_result_rule(&mut p, rule_src_line, rule_id, i)?;
            }
            continue 'mainloop;
        }
    }

    if syntax_error {
        return Err(p.raise_syntax_error());
    }

    // Sanity: did we consume the full token stream and produce exactly
    // one result?
    if p.lex.tok_kind() != TokenKind::END || p.result_stack.is_empty() {
        return Err(p.raise_syntax_error());
    }

    let root = p.result_stack[0];
    let mut tree = std::mem::replace(&mut p.tree, ParseTree::new());
    tree.root = root;
    Ok(tree)
}

// ── Public entry point ───────────────────────────────────────────────────

/// Parse the token stream produced by `lex` into a ParseTree. The caller
/// owns the lifetimes of `lex` and `qctx`; the parser holds non-owning
/// borrows for the duration of the call. The returned tree references
/// `qctx`-interned qstr ids and lives until it is dropped.
pub fn parse(
    lex: &mut Lexer,
    qctx: &mut QstrContext,
    input_kind: InputKind,
) -> ParseResult<ParseTree> {
    parse_impl(lex, qctx, input_kind)
}
