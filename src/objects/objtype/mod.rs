//! Object-type machinery (`src/objects/objtype/`).
//!
//! Lane C12b owns three of the four files here: `objexcept`,
//! `objmodule`, `objobject` (plus their `_types` helpers and
//! tests). Lane C12a owns `objtype.rs` (the type-system core,
//! 2,214 lines) — the main agent merges the two `mod.rs` files
//! at merge time (this file lists only the C12b modules).
//!
//! MERGE NOTE (main agent): add `pub mod objtype;` (C12a's
//! `objtype.rs` + tests) to this list and keep the C12b modules.

pub mod objexcept;
pub mod objexcept_types;
pub mod objmodule;
pub mod objobject;

#[cfg(test)]
mod objtype_tests;
#[cfg(test)]
mod objtype_tests_b;
