//! Primitives — `int`, `bool`, `cell` (Lane C7), `float` and
//! `complex` (Lane C18a).
//!
//! Port of `src/objects/primitives/objint.zig` (556 lines),
//! `objbool.zig` (83 lines) and `objcell.zig` (48 lines) — the Zig
//! ports of vendor `py/objint.c` + `py/objint_mpz.c` (full config),
//! `py/objbool.c` and the cell type. 0 test blocks upstream.
//!
//! C18a adds the vendor float/complex type slots
//! (`py/objfloat.c`, `py/objcomplex.c` — the latter is NOT compiled
//! by the Zig build, see `objcomplex.rs`): `float` / `complex`
//! `make_new`, print, unary/binary ops, and the complex `.real` /
//! `.imag` attr. The float/complex `binary_op` fns take an `f64` /
//! `(f64, f64)` left operand, like the vendor's
//! `mp_obj_float_binary_op` / `mp_obj_complex_binary_op`; the int
//! slot delegates to them for float/complex rhs (vendor
//! `objint_mpz.c:175-183`).
//!
//! Big integers go through Lane C's `crate::vendor::mpz::BigInt` —
//! no second bignum is written here. The rich functions in this
//! module take `&BuiltinTypes` explicitly and return `PrimResult`
//! (see `ops.rs`); the thin adapters to the frozen slot signatures
//! (`int_unary_slot` / `int_binary_slot` / `bool_unary_slot` /
//! `bool_binary_slot`) read the registry from `CallCtx.types` (the
//! C13 growth, done early by C7r) and convert with
//! `prim_to_pyresult`.

pub mod objbool;
pub mod objcell;
pub mod objcomplex;
pub mod objfloat;
pub mod objint;
pub mod ops;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_dispatch;

#[cfg(test)]
mod tests_ops;

#[cfg(test)]
mod objcomplex_tests;
#[cfg(test)]
mod objfloat_tests;
#[cfg(test)]
mod test_fx;

pub use objbool::{
    bool_binary_op, bool_binary_slot, bool_make_new, bool_print, bool_to_string, bool_type_data,
    bool_unary_op, bool_unary_slot,
};
pub use objcell::{cell_get, cell_set, cell_type_data, new_cell};
pub use objcomplex::{
    complex_binary_op, complex_binary_slot, complex_make_new, complex_print, complex_type_data,
    complex_unary_op, complex_unary_slot, new_complex,
};
pub use objfloat::{
    float_binary_op, float_binary_slot, float_hash, float_make_new, float_print, float_type_data,
    float_unary_op, float_unary_slot, new_float,
};
pub use objint::{
    fun_n_args, int_binary_op, int_binary_slot, int_format, int_from_bytes,
    int_from_bytes_builtin, int_locals_dict, int_make_new, int_pow3, int_print, int_to_bytes,
    int_to_bytes_builtin, int_to_string, int_type_data, int_unary_op, int_unary_slot,
    parse_int_text,
};
pub use ops::{prim_to_pyresult, PrimError, PrimOutcome, PrimResult, QstrResolver};
// Op codes and the builtin-type registry are C6's canonical types
// (`slots.rs` / `obj.rs`); re-exported here so the tests' `super::*`
// keeps working.
pub use crate::objects::obj::{BuiltinTypes, ExcType};
pub use crate::objects::slots::{BinaryOp, UnaryOp};
