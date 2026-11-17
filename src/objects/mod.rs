//! Value / Obj model (C0, frozen interface).
//! See `docs/value-obj-design.md`.

pub mod callables;
pub mod containers;
pub mod obj;
pub mod obj_ops;
pub mod objtype;
pub mod primitives;
pub mod slots;
pub mod strings;
pub mod value;

#[cfg(test)]
mod obj_ops_tests;
#[cfg(test)]
mod obj_tests;
