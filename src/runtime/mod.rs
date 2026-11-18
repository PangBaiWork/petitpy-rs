//! Runtime-track modules — the interpreter side of petitpy.
//!
//! Mirrors `src/runtime/` in the original: the VM, the runtime qstr
//! pool, the object runtime support. The compiler track lives in
//! `crate::compiler`; the two tracks share the static qstr table
//! (`crate::qstr_table`).
//!
//! Glue files with NO Rust module (C15a, verified against the
//! original — see `scripts/runtime_glue_result.txt`):
//!
//! * `stackctrl.zig` — `mp_stack_set_top` writes the thread
//!   stack top (a GC conservative-scan bound — the Rust GC is
//!   not conservative, so there is no equivalent);
//!   `mp_stack_check` is a no-op (`MICROPY_STACK_CHECK=0`);
//!   the cstack fns are nano-dead.
//! * `nlr_glue.zig` — `nlr_jump_fail` (@panic: the VM's
//!   top-level exception path is the Rust equivalent);
//!   `stdout_print_strn` is C8c's `mp_plat_print`;
//!   the rest are @panic stubs / placeholder zero-init
//!   structs, NOT translated into `panic!`.
//! * `nlr_trampoline.zig` (+ `.c`) — setjmp/longjmp. Rust has
//!   no equivalent: `PyError` / `CallCtx.pending_raise` /
//!   `VmState.pending_exc` are the mechanism. Mapping:
//!   `callBinaryOp`/`callPow3`/`callFunction` → C14's call
//!   fns (direct call; the trampoline wrap is dead in Rust);
//!   `raiseViaLongjmp` → set `pending_exc` + `Err(PyRaise)`;
//!   `hasActiveHandler` → `pending_exc.is_some()` (in Rust
//!   there is always a handler — the VM); `npy_nlr_top_get`/
//!   `set` → no counterpart, Rust doesn't need it.
//! * `jcb_glue.zig` — `mp_globals_set`/`get` are plain
//!   `VmState.globals` field accesses; `mp_locals_set`/`get`
//!   are the `VmState` accessors in `vm.rs`
//!   (`globals_dict`/`set_globals_dict`/`locals_dict`/
//!   `set_locals_dict`, where `locals_dict()` is
//!   `locals.unwrap_or(globals)`); the `CallbackNode*`
//!   structs are NLR callback-node layouts Rust doesn't need.

pub mod argcheck;
pub mod bc;
pub mod builtin_fns;
pub mod builtin_str;
pub mod builtin_sort;
pub mod builtin_vm;
pub mod builtinhelp;
pub mod builtinevex;
pub mod builtinimport;
pub mod code_store;
pub mod compile_bridge;
pub mod d4state;
pub mod frozen_bundle;
pub mod frozen_modules;
pub mod import_search;
pub mod modos;
pub mod modos_fd;
pub mod modsys;
pub mod opmethods;
pub mod persistentcode;
pub mod qstr;
pub mod raise;
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
pub mod runtime_utils;
pub mod scheduler;
pub mod vm;
pub mod vm_builtins;
pub mod vm_builtins_bind;
pub mod vm_call;
pub mod vm_call_intercept;
pub mod vm_call_var_kw;
pub mod vm_dispatch;
pub mod vm_dispatch_exec;
pub mod vm_dispatch_util;
pub mod vm_eval;
pub mod vm_gen;
pub mod vm_modules;
pub mod vm_ops;
pub mod vm_raise;
pub mod vm_rt;
pub mod vm_rt_iter;
pub mod vm_rt_name;
pub mod vm_roots;
pub mod vm_seams;
pub mod vm_types;
pub mod warning;

#[cfg(test)]
mod argcheck_tests;
#[cfg(test)]
mod bc_tests;
#[cfg(test)]
mod builtin_fns_tests;
#[cfg(test)]
mod builtin_vm_tests;
#[cfg(test)]
mod builtin_vm_tests_b;
#[cfg(test)]
mod builtinhelp_tests;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod modsys_tests;
#[cfg(test)]
mod modos_tests;
#[cfg(test)]
mod vm_builtins_tests;
#[cfg(test)]
mod warning_tests;
#[cfg(test)]
mod vm_tests;
#[cfg(test)]
mod runtime_glue_tests;
#[cfg(test)]
mod runtime_glue_tests_b;
#[cfg(test)]
mod runtime_tests;

#[cfg(test)]
mod runtime_tests_b;

#[cfg(test)]
mod runtime_tests_c;
#[cfg(test)]
mod vm_raise_tests;
#[cfg(test)]
mod vm_tests_b;
#[cfg(test)]
mod vm_tests_e2;
#[cfg(test)]
mod vm_tests_e2b;
#[cfg(test)]
mod vm_tests_e2c;
#[cfg(test)]
mod vm_tests_e2d;
#[cfg(test)]
mod vm_tests_e2e;
#[cfg(test)]
mod vm_tests_e2f;
#[cfg(test)]
mod vm_tests_e2g;
#[cfg(test)]
mod vm_tests_e2h;
#[cfg(test)]
mod vm_tests_e2i;
#[cfg(test)]
mod vm_tests_e2j;

#[cfg(test)]
mod vm_tests_f4;
