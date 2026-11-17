//! Runtime-track modules — the interpreter side of petitpy.
//!
//! Mirrors `src/runtime/` in the original: the VM, the runtime qstr
//! pool, the object runtime support. The compiler track lives in
//! `crate::compiler`; the two tracks share the static qstr table
//! (`crate::qstr_table`).

pub mod qstr;
