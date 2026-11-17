//! Primitives — `int`, `bool`, `cell` (Lane C7).
//!
//! Port of `src/objects/primitives/objint.zig` (556 lines),
//! `objbool.zig` (83 lines) and `objcell.zig` (48 lines) — the Zig
//! ports of vendor `py/objint.c` + `py/objint_mpz.c` (full config),
//! `py/objbool.c` and the cell type. 0 test blocks upstream.
//!
//! Big integers go through Lane C's `crate::vendor::mpz::BigInt` —
//! no second bignum is written here. The rich functions in this
//! module take `&BuiltinTypes` explicitly and return `PrimResult`
//! (see `ops.rs`); the thin adapters to the frozen slot signatures
//! land when C6's op-enum signatures merge and C13 grows `CallCtx`
//! with the type registry.

pub mod objbool;
pub mod objcell;
pub mod objint;
pub mod ops;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_ops;

pub use objbool::{bool_binary_op, bool_make_new, bool_to_string, bool_type_data, bool_unary_op};
pub use objcell::{cell_get, cell_set, cell_type_data, new_cell};
pub use objint::{
    fun_n_args, int_binary_op, int_format, int_from_bytes, int_from_bytes_builtin,
    int_locals_dict, int_make_new, int_pow3, int_to_bytes, int_to_string, int_type_data,
    int_unary_op, parse_int_text,
};
pub use ops::{
    prim_to_pyresult, BinaryOp, BuiltinTypes, ExcType, PrimError, PrimOutcome, PrimResult,
    QstrResolver, UnaryOp,
};
