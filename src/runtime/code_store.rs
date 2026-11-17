//! The VM's code store: every compiled function/module's `RawCode`,
//! indexed by the code index in `FunBcData.code`.
//! Split out of `vm.rs` (C17 merge integration — that file was 804
//! lines, over the 800 cap).

use std::collections::HashMap;

use crate::compiler::mpy::RawCode;
use crate::objects::value::{GcRef, Value};

/// One compiled code blob in the store: the raw bytecode, the
/// index of its module's resolved qstr table, the index of its
/// module's const-object table (converted `ObjEntry`s, pinned —
/// `LOAD_CONST_OBJ` reads them), and the module object (whose
/// globals dict is the code's globals).
#[derive(Debug, Clone)]
pub struct CodeEntry {
    pub raw: RawCode,
    pub qstr_table: usize,
    pub const_table: usize,
    pub module: GcRef,
}

/// The VM's code store: every compiled function/module's `RawCode`,
/// indexed by the code index in `FunBcData.code`. The Zig resolves
/// a child by index into its parent's `children` table
/// (`childTablePtr`, `mp_make_function_from_proto_fun`); the store
/// keeps that `(parent, child) -> code` map explicitly.
#[derive(Debug, Default)]
pub struct CodeStore {
    pub codes: Vec<CodeEntry>,
    pub qstr_tables: Vec<Vec<u16>>,
    /// Converted const-object tables, one per module (parallel to
    /// `qstr_tables`); the values are pinned in `roots.extra`
    /// at registration (see `load_module_from_mpy`).
    pub const_tables: Vec<Vec<Value>>,
    pub children: HashMap<(usize, usize), usize>,
}

impl CodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a module's `RawCode` and all its children
    /// recursively. Returns the module code's index. The module's
    /// qstr table is resolved once by the caller (static ids pass
    /// through, `New` bytes are interned into the pool — see
    /// `resolve_qstr_table`) and shared by every code in the
    /// module, as is the converted const-object table.
    pub fn register_module(
        &mut self,
        rc: RawCode,
        qstr_table: Vec<u16>,
        const_table: Vec<Value>,
        module: GcRef,
    ) -> usize {
        let table_idx = self.qstr_tables.len();
        self.qstr_tables.push(qstr_table);
        self.const_tables.push(const_table);
        self.register_code(rc, table_idx, module)
    }

    fn register_code(&mut self, rc: RawCode, table_idx: usize, module: GcRef) -> usize {
        let RawCode {
            kind,
            fun_data,
            children,
        } = rc;
        let idx = self.codes.len();
        // (Rebuild the RawCode without children — the children live
        // in the store now, keyed by (parent, child).)
        self.codes.push(CodeEntry {
            raw: RawCode {
                kind,
                fun_data,
                children: Vec::new(),
            },
            qstr_table: table_idx,
            const_table: table_idx,
            module,
        });
        for (i, child) in children.into_iter().enumerate() {
            let child_idx = self.register_code(child, table_idx, module);
            self.children.insert((idx, i), child_idx);
        }
        idx
    }

    pub fn child(&self, parent: usize, child: usize) -> Option<usize> {
        self.children.get(&(parent, child)).copied()
    }

    pub fn code(&self, idx: usize) -> &RawCode {
        &self.codes[idx].raw
    }

    pub fn qstr_table(&self, idx: usize) -> &[u16] {
        let t = self.codes[idx].qstr_table;
        &self.qstr_tables[t]
    }

    /// The module's converted const-object table (`LOAD_CONST_OBJ`
    /// reads `table[u]` — the same object every time, as in the
    /// Zig's `obj_table`).
    pub fn const_table(&self, idx: usize) -> &[Value] {
        let t = self.codes[idx].const_table;
        &self.const_tables[t]
    }

    pub fn module(&self, idx: usize) -> GcRef {
        self.codes[idx].module
    }
}
