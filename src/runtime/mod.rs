//! Runtime-track modules — the interpreter side of petitpy.
//!
//! Mirrors `src/runtime/` in the original: the VM, the runtime qstr
//! pool, the object runtime support. The compiler track lives in
//! `crate::compiler`; the two tracks share the static qstr table
//! (`crate::qstr_table`).

pub mod bc;
pub mod qstr;
pub mod runner;
pub mod runtime;
pub mod runtime_call;
pub mod runtime_import;
pub mod runtime_iter;
pub mod runtime_method;
pub mod runtime_names;
pub mod runtime_ops;
pub mod runtime_resume;
pub mod runtime_smallint;
pub mod vm;
pub mod vm_builtins;
pub mod vm_call;
pub mod vm_dispatch;
pub mod vm_dispatch_exec;
pub mod vm_dispatch_util;
pub mod vm_gen;
pub mod vm_ops;
pub mod vm_raise;
pub mod vm_rt;
pub mod vm_rt_iter;
pub mod vm_rt_name;
pub mod vm_types;

#[cfg(test)]
mod bc_tests;
#[cfg(test)]
mod vm_tests;
#[cfg(test)]
mod runtime_tests;

#[cfg(test)]
mod runtime_tests_b;

#[cfg(test)]
mod runtime_tests_c;
