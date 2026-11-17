//! `EmitCommon` — per-compilation-unit constant tables and child registry.
//!
//! Port of `EmitCommon` in `src/compiler/emit.zig` (vendor
//! `mp_emit_common_t`). Owns:
//!   - `qstr_table`: emit-local index → real `Qstr` id (the `.mpy`
//!     writer turns each into a `QstrEntry`).
//!   - `const_obj_list`: the constants any opcode references.
//!   - `children`: nested scopes the emitter has seen (resolved to
//!     `mpy::RawCode` by the compile driver after compilation finishes).
//!
//! `ct_cur_child` ticks across passes so child allocation order is
//! deterministic.
//!
//! ## Divergences
//!
//! * Zig's `children` holds `*Scope` pointers; Rust holds `ScopeId`
//!   arena handles (same order, same meaning).
//! * Allocation cannot fail gracefully in Rust (`Vec`/`HashMap` abort
//!   on OOM, where Zig propagates `Allocator.Error`); the `use_*` /
//!   `alloc_*` methods therefore return plain values instead of
//!   `Result`s. The observable difference exists only under actual
//!   OOM. `id_op`'s missing-id path keeps the Zig shape —
//!   `debug_assert!(false)` plus the `overflow` flag — but is expressed
//!   on `Emit` (see `emit.rs`), not here, because it needs the emitter.

use std::collections::HashMap;

use super::const_obj::ConstObj;
use super::consts::Pass;
use crate::compiler::qstr::Qstr;
use crate::compiler::scope::{IdInfoKind, ScopeArena, ScopeId};

/// Per-compilation-unit constant tables and child registry.
#[derive(Debug, Default)]
pub struct EmitCommon {
    pub pass: Pass,
    pub ct_cur_child: u16,
    pub children: Vec<ScopeId>,
    pub qstr_table: Vec<Qstr>,
    pub qstr_index: HashMap<Qstr, u16>,
    pub const_obj_list: Vec<ConstObj>,
}

impl EmitCommon {
    pub fn new() -> Self {
        EmitCommon {
            pass: Pass::Scope,
            ct_cur_child: 0,
            children: Vec::new(),
            qstr_table: Vec::new(),
            qstr_index: HashMap::new(),
            const_obj_list: Vec::new(),
        }
    }

    /// Vendor `mp_emit_common_use_qstr` — dedup + return short index.
    pub fn use_qstr(&mut self, q: Qstr) -> u16 {
        if let Some(&idx) = self.qstr_index.get(&q) {
            return idx;
        }
        let idx: u16 = self.qstr_table.len() as u16;
        self.qstr_table.push(q);
        self.qstr_index.insert(q, idx);
        idx
    }

    /// Vendor `mp_emit_common_use_const_obj` — `strictly_equal` dedup.
    pub fn use_const_obj(&mut self, obj: ConstObj) -> u16 {
        for (i, existing) in self.const_obj_list.iter().enumerate() {
            if ConstObj::strictly_equal(existing, &obj) {
                return i as u16;
            }
        }
        let idx: u16 = self.const_obj_list.len() as u16;
        self.const_obj_list.push(obj);
        idx
    }

    /// Vendor `mp_emit_common_alloc_const_child` — reserve a slot for a
    /// nested function. Counter bumps across passes; same scope hits the
    /// same slot on every pass (vendor relies on this for stable
    /// indices).
    pub fn alloc_const_child(&mut self, child: ScopeId) -> u16 {
        let idx = self.ct_cur_child;
        self.ct_cur_child += 1;
        // Children array tracks only the unique scopes seen — extend it
        // on first sighting; subsequent passes hit the same slot.
        if (idx as usize) >= self.children.len() {
            self.children.push(child);
        } else {
            // Make sure we got the same scope back; vendor invariant.
            debug_assert_eq!(self.children[idx as usize], child);
        }
        idx
    }

    /// Reset `ct_cur_child` at the start of every pass so child indices
    /// remain stable across re-emits (vendor `mp_emit_common_start_pass`).
    pub fn start_pass(&mut self, pass: Pass) {
        self.pass = pass;
        self.ct_cur_child = 0;
    }

    /// Vendor `mp_emit_common_get_id_for_modification` (`emitcommon.c`).
    /// Inserts `qst` as `GlobalImplicit`, then rebinds: `Local` in
    /// func-like scopes, `GlobalImplicitAssigned` elsewhere. Returns
    /// the entry index (Zig returns the `*IdInfo` pointer; callers
    /// re-read the entry from the arena by index).
    pub fn get_id_for_modification(
        scopes: &mut ScopeArena,
        scope: ScopeId,
        q: Qstr,
    ) -> usize {
        let idx = scopes.find_or_add(scope, q, IdInfoKind::GlobalImplicit);
        if scopes.scopes[scope].id_info[idx].kind == IdInfoKind::GlobalImplicit {
            if scopes.scopes[scope].kind.is_func_like() {
                scopes.scopes[scope].id_info[idx].kind = IdInfoKind::Local;
            } else {
                scopes.scopes[scope].id_info[idx].kind =
                    IdInfoKind::GlobalImplicitAssigned;
            }
        }
        idx
    }
}