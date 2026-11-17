//! §31–§33 — scope passes, the 3-pass driver, module assembly.
//!
//! Port of `runScopePass` / `runScopePassOne` / `runCodegenPasses` /
//! `driveScopeBody` / `scopeComputeThings` / `compile` in
//! `src/compiler/compile.zig`.

use super::{handlers, Compiler, CompileError, SyntaxErrorKind};
use crate::compiler::emit::consts::{
    MP_EMIT_ATTR_LOAD, MP_EMIT_BUILD_LIST, MP_EMIT_BUILD_MAP, MP_EMIT_BUILD_SET,
    MP_EMIT_IDOP_LOCAL_FAST, MP_EMIT_IMPORT_NAME, MP_TOKEN_KW_NONE, Pass,
};
use crate::compiler::grammar_data as grammar;
use crate::compiler::mpy::{Module, ObjEntry, QstrEntry, RawCode, MPY_VERSION, MP_CODE_BYTECODE};
use crate::compiler::parser::{pn_is_struct, ParseTree, Pn};
use crate::compiler::qstr::{Qstr, QstrContext};
use crate::compiler::scope::{
    IdInfoKind, ScopeArena, ScopeId, ScopeKind, ID_FLAG_IS_PARAM, ID_FLAG_IS_STAR_PARAM,
    SCOPE_FLAG_GENERATOR, SCOPE_FLAG_VARARGS,
};
use super::funcdef::compile_scope_comp_iter;
use super::wk_idx;

// ─────────────────────────────────────────────────────────────────────────
// §32 — scopeComputeThings.
//
// BYTE-CRITICAL: this is a verbatim port of vendor py/compile.c
// scope_compute_things (compile.c:3380). Local-numbering order follows
// vendor exactly because the .mpy emits the IdInfo table in local_num
// order — any deviation shifts the FAST_N indices that the bytecode
// references.
//
// Vendor order:
//   1. Star-param swap if VARARGS: move `*args` to the end of the
//      regular-param run (before `**kwargs`).
//   2. Single forward pass: LOCAL or IS_PARAM gets local_num++. Convert
//      GLOBAL_IMPLICIT → GLOBAL_EXPLICIT in func-like scopes. Skip
//      __class__ in class scopes.
//   3. CELL (without IS_PARAM): local_num++.
//   4. FREE vars: indexed in PARENT's CELL/FREE order; shift all
//      non-FREE locals by +num_free; bump num_pos_args + num_locals.
// ─────────────────────────────────────────────────────────────────────────

/// Assign local_num to each IdInfo entry; bump scope.num_locals.
/// Mirrors vendor compile.c scope_compute_things().
pub(crate) fn scope_compute_things(
    scopes: &mut ScopeArena,
    scope: ScopeId,
    qctx: &QstrContext,
) {
    let class_qst: Qstr = qctx.find_strn(b"__class__");
    // (1) Star-param swap: in MicroPython `*args` goes after all other
    // regular params (still before `**kwargs`). Walk id_info from the
    // back; first regular IS_PARAM we see is `id_param`; first
    // STAR_PARAM after that gets swapped with id_param.
    if (scopes.scopes[scope].scope_flags & SCOPE_FLAG_VARARGS) != 0 {
        let mut id_param: Option<usize> = None;
        let mut i: isize = scopes.scopes[scope].id_info.len() as isize - 1;
        while i >= 0 {
            let id = &scopes.scopes[scope].id_info[i as usize];
            if (id.flags & ID_FLAG_IS_STAR_PARAM) != 0 {
                if let Some(p) = id_param {
                    scopes.scopes[scope].id_info.swap(p, i as usize);
                }
                break;
            } else if id_param.is_none() && id.flags == ID_FLAG_IS_PARAM {
                id_param = Some(i as usize);
            }
            i -= 1;
        }
    }

    // (2) Single forward pass: index LOCAL and IS_PARAM, in-place
    // implicit→explicit global conversion in func-like scopes, skip
    // __class__ in class scopes.
    scopes.scopes[scope].num_locals = 0;
    {
        let kind = scopes.scopes[scope].kind;
        let mut i: usize = 0;
        while i < scopes.scopes[scope].id_info.len() {
            let (qst, id_kind, flags) = {
                let id = &scopes.scopes[scope].id_info[i];
                (id.qst, id.kind, id.flags)
            };
            if kind == ScopeKind::Class && qst == class_qst {
                i += 1;
                continue;
            }
            if kind.is_func_like() && id_kind == IdInfoKind::GlobalImplicit {
                scopes.scopes[scope].id_info[i].kind = IdInfoKind::GlobalExplicit;
            }
            // Native/REFGLOBALS gating is dead in nano config.
            if id_kind == IdInfoKind::Local || (flags & ID_FLAG_IS_PARAM) != 0 {
                let n = scopes.scopes[scope].num_locals;
                scopes.scopes[scope].id_info[i].local_num = n;
                scopes.scopes[scope].num_locals += 1;
            }
            i += 1;
        }
    }

    // (3) Cell vars (not also IS_PARAM): come right after fast locals.
    {
        let mut i: usize = 0;
        while i < scopes.scopes[scope].id_info.len() {
            let (kind, flags) = {
                let id = &scopes.scopes[scope].id_info[i];
                (id.kind, id.flags)
            };
            if kind == IdInfoKind::Cell && (flags & ID_FLAG_IS_PARAM) == 0 {
                let n = scopes.scopes[scope].num_locals;
                scopes.scopes[scope].id_info[i].local_num = n;
                scopes.scopes[scope].num_locals += 1;
            }
            i += 1;
        }
    }

    // (4) Free vars: ordered by parent's CELL/FREE order.
    if let Some(parent) = scopes.scopes[scope].parent {
        let mut num_free: u16 = 0;
        let mut pi: usize = 0;
        while pi < scopes.scopes[parent].id_info.len() {
            let (pid_kind, pid_qst) = {
                let pid = &scopes.scopes[parent].id_info[pi];
                (pid.kind, pid.qst)
            };
            if pid_kind == IdInfoKind::Cell || pid_kind == IdInfoKind::Free {
                let mut ci: usize = 0;
                while ci < scopes.scopes[scope].id_info.len() {
                    let (cid_kind, cid_qst, cid_flags) = {
                        let cid = &scopes.scopes[scope].id_info[ci];
                        (cid.kind, cid.qst, cid.flags)
                    };
                    if cid_kind == IdInfoKind::Free && pid_qst == cid_qst {
                        debug_assert!((cid_flags & ID_FLAG_IS_PARAM) == 0);
                        scopes.scopes[scope].id_info[ci].local_num = num_free;
                        num_free += 1;
                    }
                    ci += 1;
                }
            }
            pi += 1;
        }
        // Shift all non-FREE (or FREE-but-param) locals by +num_free.
        if num_free > 0 {
            let mut ci: usize = 0;
            while ci < scopes.scopes[scope].id_info.len() {
                let (kind, flags) = {
                    let cid = &scopes.scopes[scope].id_info[ci];
                    (cid.kind, cid.flags)
                };
                if kind != IdInfoKind::Free || (flags & ID_FLAG_IS_PARAM) != 0 {
                    scopes.scopes[scope].id_info[ci].local_num += num_free;
                }
                ci += 1;
            }
            scopes.scopes[scope].num_pos_args += num_free;
            scopes.scopes[scope].num_locals += num_free;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// §31 — Per-scope body walker + pass drivers.
// ─────────────────────────────────────────────────────────────────────────

impl Compiler {
    /// Per-scope body walker (replaces the retired `driveScope`).
    pub(crate) fn drive_scope_body(&mut self, scope: ScopeId) {
        self.scope_cur = Some(scope);
        if self.pass == Pass::Scope {
            self.scopes.scopes[scope].stack_size = 0;
            self.scopes.scopes[scope].exc_stack_size = 0;
        }

        let (kind, scope_pn) = {
            let s = &self.scopes.scopes[scope];
            (s.kind, s.pn)
        };

        if self.tree.is_struct_kind(scope_pn, grammar::RULE_eval_input) {
            debug_assert_eq!(kind, ScopeKind::Module);
            let child = self.tree.struct_nodes(scope_pn)[0];
            self.compile_node(child);
            self.emit.return_value(&mut self.scopes);
            return;
        }

        match kind {
            ScopeKind::Module => {
                self.compile_node(scope_pn);
                self.emit
                    .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                self.emit.return_value(&mut self.scopes);
            }
            ScopeKind::Function => {
                debug_assert!(pn_is_struct(scope_pn));
                debug_assert_eq!(
                    self.tree.struct_kind(scope_pn),
                    grammar::RULE_funcdef
                );
                let nodes: Vec<Pn> = self.tree.struct_nodes(scope_pn).to_vec();
                if self.pass == Pass::Scope {
                    self.have_star = false;
                    self.apply_to_single_or_list(
                        nodes[1],
                        grammar::RULE_typedargslist,
                        super::funcdef::c_scope_func_param,
                    );
                }
                self.compile_node(nodes[3]);
                self.emit
                    .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                self.emit.return_value(&mut self.scopes);
            }
            ScopeKind::Lambda => {
                debug_assert!(pn_is_struct(scope_pn));
                let nodes: Vec<Pn> = self.tree.struct_nodes(scope_pn).to_vec();
                debug_assert_eq!(nodes.len(), 3);
                let source_line = self.tree.struct_source_line(scope_pn);
                self.emit.set_source_line(source_line);
                if self.pass == Pass::Scope {
                    self.have_star = false;
                    self.apply_to_single_or_list(
                        nodes[0],
                        grammar::RULE_varargslist,
                        super::funcdef::c_scope_lambda_param,
                    );
                }
                self.compile_node(nodes[1]);
                let is_gen = (self.scopes.scopes[scope].scope_flags & SCOPE_FLAG_GENERATOR) != 0;
                if is_gen {
                    self.emit.pop_top(&mut self.scopes);
                    self.emit
                        .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                }
                self.emit.return_value(&mut self.scopes);
            }
            ScopeKind::Class => {
                debug_assert!(pn_is_struct(scope_pn));
                debug_assert_eq!(
                    self.tree.struct_kind(scope_pn),
                    grammar::RULE_classdef
                );
                let nodes: Vec<Pn> = self.tree.struct_nodes(scope_pn).to_vec();
                if self.pass == Pass::Scope {
                    let class_q = self.wk(wk_idx::CLASS);
                    self.scopes
                        .find_or_add(scope, class_q, IdInfoKind::Local);
                }
                let name_q = self.wk(wk_idx::NAME);
                self.compile_load_id(name_q);
                let module_q = self.wk(wk_idx::MODULE);
                self.compile_store_id(module_q);
                let name_arg = {
                    use crate::compiler::parser::pn_leaf_arg;
                    pn_leaf_arg(nodes[0]) as Qstr
                };
                self.emit.load_const_str(name_arg, &mut self.scopes);
                let qualname_q = self.wk(wk_idx::QUALNAME);
                self.compile_store_id(qualname_q);
                if handlers::has_annassign(&self.tree, nodes[2]) {
                    // __annotations__ = __import__("ucollections").OrderedDict():
                    // a dict here is not insertion-ordered, and field order is
                    // what @dataclass binds positional arguments by. ucollections
                    // resolves to the built-in module, binding no name.
                    self.emit.load_const_small_int(0, &mut self.scopes);
                    self.emit
                        .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                    let ucoll = self.qstr_from_strn(b"ucollections");
                    self.emit
                        .import(ucoll, MP_EMIT_IMPORT_NAME, &mut self.scopes);
                    let od = self.qstr_from_strn(b"OrderedDict");
                    self.emit.attr(od, MP_EMIT_ATTR_LOAD, &mut self.scopes);
                    self.emit.call_function(0, 0, 0, &mut self.scopes);
                    let ann_q = handlers::annotations_qstr(self);
                    self.compile_store_id(ann_q);
                }
                self.compile_node(nodes[2]);
                let class_q = self.wk(wk_idx::CLASS);
                let id_idx = self
                    .scopes
                    .find(scope, class_q)
                    .expect("drive_scope_body: __class__ missing from class scope");
                let (id_kind, id_local_num) = {
                    let id = &self.scopes.scopes[scope].id_info[id_idx];
                    (id.kind, id.local_num)
                };
                if id_kind == IdInfoKind::Local {
                    self.emit
                        .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                } else {
                    self.emit
                        .load_local(id_local_num, MP_EMIT_IDOP_LOCAL_FAST, &mut self.scopes);
                }
                self.emit.return_value(&mut self.scopes);
            }
            ScopeKind::ListComp
            | ScopeKind::DictComp
            | ScopeKind::SetComp
            | ScopeKind::GenExpr => {
                // Vendor compile_scope's SCOPE_IS_COMP_LIKE branch
                // (compile.c:3111). The comprehension's scope.pn is the
                // original testlist_comp / dictorsetmaker / argument
                // node — same shape we wrapped in
                // compile_comprehension_stub: nodes[0]=inner expr,
                // nodes[1]=comp_for.
                debug_assert!(pn_is_struct(scope_pn));
                let nodes: Vec<Pn> = self.tree.struct_nodes(scope_pn).to_vec();
                debug_assert_eq!(nodes.len(), 2);
                debug_assert!(self
                    .tree
                    .is_struct_kind(nodes[1], grammar::RULE_comp_for));
                let comp_for_pn = nodes[1];
                let inner_pn = nodes[0];

                // The comprehension takes one synthetic positional arg
                // holding the outermost iterator. CPython uses ".0";
                // vendor uses the blank qstr (id 1) which can't clash
                // with a user binding.
                let qstr_arg: Qstr = 1; // MP_QSTR_ (the empty-string qstr)
                if self.pass == Pass::Scope {
                    self.scopes
                        .find_or_add(scope, qstr_arg, IdInfoKind::Local);
                    self.scopes.scopes[scope].num_pos_args = 1;
                }

                let source_line = self.tree.struct_source_line(scope_pn);
                self.emit.set_source_line(source_line);

                // Seed the accumulator: empty list / dict / set; for
                // gen_expr the four-slot iter buffer per the YIELD /
                // PEP-3155 ABI.
                match kind {
                    ScopeKind::ListComp => {
                        self.emit
                            .build(0, MP_EMIT_BUILD_LIST, &mut self.scopes)
                    }
                    ScopeKind::DictComp => {
                        self.emit.build(0, MP_EMIT_BUILD_MAP, &mut self.scopes)
                    }
                    ScopeKind::SetComp => {
                        self.emit.build(0, MP_EMIT_BUILD_SET, &mut self.scopes)
                    }
                    ScopeKind::GenExpr => {
                        // 4-slot iterator buffer: NULL, the iterator,
                        // two NULL padding slots
                        // (MP_OBJ_ITER_BUF_NSLOTS = 4).
                        self.emit.load_null(&mut self.scopes);
                        self.compile_load_id(qstr_arg);
                        self.emit.load_null(&mut self.scopes);
                        self.emit.load_null(&mut self.scopes);
                    }
                    _ => unreachable!(),
                }
                if kind != ScopeKind::GenExpr {
                    self.compile_load_id(qstr_arg);
                    self.emit.get_iter(true, &mut self.scopes);
                }

                compile_scope_comp_iter(self, comp_for_pn, inner_pn, 0);

                if kind == ScopeKind::GenExpr {
                    self.emit
                        .load_const_tok(MP_TOKEN_KW_NONE, &mut self.scopes);
                }
                self.emit.return_value(&mut self.scopes);
            }
        }
    }

    /// SCOPE pass for a single scope — no recursion. Marks the scope
    /// as `scope_pass_done` on completion.
    fn run_scope_pass_one(&mut self, scope: ScopeId) {
        self.emit.emit_common.children.clear();
        self.next_label = 0;

        self.pass = Pass::Scope;
        self.emit.emit_common.start_pass(Pass::Scope);
        self.emit.start_pass(Pass::Scope, scope, &self.scopes);
        self.drive_scope_body(scope);
        let _ = self.emit.end_pass(&self.scopes);

        // Vendor compile.c:3513-3517 — after each scope's SCOPE pass,
        // walk its id_info for any binding that's still
        // GLOBAL_IMPLICIT and check whether a parent scope actually
        // owns it (as LOCAL / CELL / FREE). If so,
        // scope_check_to_close_over rebinds it to FREE in this scope
        // and CELL in the owning parent. Without this, closure free
        // variables stay as GLOBAL_IMPLICIT and get emitted as
        // LOAD_GLOBAL_NAME (adding the qstr to qstr_table) instead of
        // LOAD_DEREF.
        let kinds: Vec<(usize, IdInfoKind)> = self.scopes.scopes[scope]
            .id_info
            .iter()
            .enumerate()
            .map(|(i, id)| (i, id.kind))
            .collect();
        for (i, kind) in kinds {
            if kind == IdInfoKind::GlobalImplicit {
                self.scopes.check_to_close_over(scope, i);
            }
        }

        self.scopes.scopes[scope].scope_pass_done = true;
    }

    /// SCOPE pass for a single scope. Iteratively drains the
    /// scope_head chain of any newly-discovered child scopes.
    fn run_scope_pass(&mut self, scope: ScopeId) {
        self.run_scope_pass_one(scope);

        // Worklist: keep sweeping comp.scope_head until every scope's
        // SCOPE pass has been driven. Driving one scope may discover
        // new sibling scopes (via nested funcdef/classdef/lambdef),
        // which is why we re-scan from the head each round.
        loop {
            let mut made_progress = false;
            for s in self.scope_chain() {
                if self.scopes.scopes[s].scope_pass_done {
                    continue;
                }
                self.run_scope_pass_one(s);
                made_progress = true;
            }
            if !made_progress {
                break;
            }
        }
    }

    /// STACK_SIZE → CODE_SIZE → EMIT for one scope.
    fn run_codegen_passes(&mut self, scope: ScopeId) -> Result<(), CompileError> {
        self.emit.emit_common.children.clear();

        // Vendor compile_scope (compile.c:3041) resets next_label = 0
        // at the START of EVERY pass. Label allocation must be
        // deterministic and produce the same ids across STACK_SIZE /
        // CODE_SIZE / EMIT so labelAssign and forward-jumps reference
        // stable indices into emit.label_offsets.
        self.pass = Pass::StackSize;
        self.next_label = 0;
        self.emit.emit_common.start_pass(Pass::StackSize);
        self.emit.start_pass(Pass::StackSize, scope, &self.scopes);
        self.drive_scope_body(scope);
        let _ = self.emit.end_pass(&self.scopes);

        self.pass = Pass::CodeSize;
        self.next_label = 0;
        self.emit.emit_common.start_pass(Pass::CodeSize);
        self.emit.start_pass(Pass::CodeSize, scope, &self.scopes);
        self.drive_scope_body(scope);
        let _ = self.emit.end_pass(&self.scopes);

        loop {
            self.pass = Pass::Emit;
            self.next_label = 0;
            self.emit.emit_common.start_pass(Pass::Emit);
            self.emit.start_pass(Pass::Emit, scope, &self.scopes);
            self.drive_scope_body(scope);
            if self.emit.end_pass(&self.scopes) {
                break;
            }
        }

        if self.emit.overflow {
            return Err(CompileError::OutOfMemory);
        }

        let child_ids: Vec<ScopeId> = self.emit.emit_common.children.clone();

        // Vendor emitbc.c:404-411 assigns the raw_code with
        // bytecode_len = code_info_size + bytecode_size (the FINAL
        // EMIT-pass sizes). If EMIT shrank below the CODE_SIZE pass's
        // estimate — e.g. a JUMP CODE_SIZE allocated 3 bytes for but
        // EMIT resolved to a 1-byte offset — code_base is still the
        // larger CODE_SIZE allocation with trailing zero-init bytes.
        // Slice to the used length so the .mpy never carries a phantom
        // trailing 0x00.
        let used: usize = self.emit.code_info_size + self.emit.bytecode_size;
        let fun_data = self.emit.code_base[..used].to_vec();
        self.raw_children[scope] = child_ids;
        self.raw_codes[scope] = Some(RawCode {
            kind: MP_CODE_BYTECODE,
            fun_data,
            children: Vec::new(),
        });
        Ok(())
    }
}

/// Recursively fill in a scope's `RawCode` children from the
/// per-scope tables. (Zig does this with scope-pointer slots in the
/// child `RawCode`s and `cloneRawCodeTree`; the Rust driver records
/// child `ScopeId`s directly, so no pointer round-trip is needed.)
fn assemble_raw_code(
    raw_codes: &mut Vec<Option<RawCode>>,
    raw_children: &[Vec<ScopeId>],
    scope: ScopeId,
) -> RawCode {
    let mut rc = raw_codes[scope]
        .take()
        .expect("assemble_raw_code: scope has no RawCode");
    let mut children = Vec::with_capacity(raw_children[scope].len());
    for &child in &raw_children[scope] {
        children.push(assemble_raw_code(raw_codes, raw_children, child));
    }
    rc.children = children;
    rc
}

// ─────────────────────────────────────────────────────────────────────────
// §33 — Module assembly + public entry.
// ─────────────────────────────────────────────────────────────────────────

/// Top-level compile entry. Drives the full pipeline: build a Scope
/// chain, run passes per scope, fold the results into an `mpy::Module`.
pub fn compile(
    qctx: QstrContext,
    tree: ParseTree,
    source_file: Qstr,
    is_repl: bool,
) -> Result<Module, CompileError> {
    // Root (module) scope anchors the chain.
    let mut comp = Compiler::new(qctx, tree, is_repl);
    let root_pn = comp.tree.root;
    let root_scope = comp.new_scope(ScopeKind::Module, root_pn, 0);
    comp.scope_head = Some(root_scope);
    comp.scope_cur = Some(root_scope);
    comp.next_label = 0;

    // Vendor mp_emit_common_init (compile.c:218) seeds the qstr_table
    // with the source_file qstr at index 0 BEFORE any pass runs, so
    // every emitted .mpy carries the source path as its first qstr
    // entry. We mirror that here.
    comp.emit.emit_common.use_qstr(source_file);

    comp.emit.set_max_num_labels(1024);

    // SCOPE pass on root — also discovers nested scopes via
    // scope_new_and_link, so we can't iterate scope_head here; we
    // drive root only, then let the multi-scope loop pick up
    // STACK/CODE/EMIT for every scope (root + nested) in vendor order.
    comp.run_scope_pass(root_scope);

    // Vendor compile.c:3503/3528/3539 — every per-pass loop is gated
    // on `comp->compile_error == MP_OBJ_NULL`. We must mirror that
    // bail-out here: if SCOPE recorded a SyntaxError via an
    // early-return path (e.g. compileTryExcept's default-except-not-
    // last), then dangling forward labels would survive into EMIT,
    // where the 0-offset jump overflows its encoding window and
    // run_codegen_passes spins forever on emit.overflow, surfacing as
    // OOM instead of the recorded SyntaxError.
    if let Some(err) = comp.compile_error {
        return Err(match err.kind {
            SyntaxErrorKind::Syntax => CompileError::SyntaxError,
            SyntaxErrorKind::Indentation => CompileError::IndentationError,
        });
    }

    // Vendor compile.c:3528 — scope_compute_things runs after SCOPE on
    // every scope, but BEFORE STACK_SIZE on any. We loop the chain here.
    for s in comp.scope_chain() {
        scope_compute_things(&mut comp.scopes, s, &comp.qctx);
    }

    // STACK_SIZE → CODE_SIZE → EMIT for each scope in chain order.
    for s in comp.scope_chain() {
        if comp.compile_error.is_some() {
            break;
        }
        comp.run_codegen_passes(s)?;
    }

    if let Some(err) = comp.compile_error {
        return Err(match err.kind {
            SyntaxErrorKind::Syntax => CompileError::SyntaxError,
            SyntaxErrorKind::Indentation => CompileError::IndentationError,
        });
    }

    // Assemble the Module. Qstr / const-obj tables are module-global
    // on EmitCommon.
    let qstrs: Vec<QstrEntry> = comp
        .emit
        .emit_common
        .qstr_table
        .iter()
        .map(|q| comp.qctx.to_entry(*q))
        .collect();

    let objs: Vec<ObjEntry> = comp
        .emit
        .emit_common
        .const_obj_list
        .iter()
        .map(|o| o.to_obj_entry())
        .collect();

    // Root RawCode + recursive child assembly. The chain has been
    // visited in vendor order; each scope's RawCode already holds its
    // own fun_data and direct children list.
    let rc = assemble_raw_code(&mut comp.raw_codes, &comp.raw_children, root_scope);

    Ok(Module {
        version: MPY_VERSION,
        feature_flags: 0,
        small_int_bits: 31,
        arch_flags: None,
        qstrs,
        objs,
        rc,
    })
}
