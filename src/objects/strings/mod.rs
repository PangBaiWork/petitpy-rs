//! str / bytes objects (Lane C8a/C8b).
//!
//! Port of `src/objects/strings/objstr.zig` (3,864 lines, 0 test
//! blocks). C8a owns Zig lines 1–2080 (`objstr_a.rs`,
//! `objstr_methods_a.rs`, `objstr_methods_b.rs`); C8b owns lines
//! 2081–3864 (`objstr_b.rs` etc.) plus `vstr`; C8c owns `mpprint`.
//! The shared prelude (error type, data access, constructors,
//! `str_index_to_ptr`, slice helpers, iterators) lives in
//! `objstr_a.rs` and is imported by the other C8 files — never copied.

pub mod objstr_a;
pub mod objstr_b;
pub mod objstr_format;
pub mod objstr_iters;
pub mod objstr_methods_a;
pub mod objstr_methods_b;
pub mod objstr_methods_c;
pub mod objstr_print;
pub mod objstr_slice;
pub mod unicode;
pub mod vstr;

#[cfg(test)]
mod objstr_tests;

#[cfg(test)]
mod objstr_tests_b;

#[cfg(test)]
mod objstr_tests_c;

#[cfg(test)]
mod objstr_tests_d;
