//! The standard-library builtin modules (Lane D3: modjson,
//! modmath, modsocket, modstruct, mod_subprocess).
//!
//! Each module follows the `build_io_module` pattern from
//! `src/objects/containers/objstringio_types.rs`: a
//! `build_<name>_module(ctx, fun_builtin_var, module_type) ->
//! PyResult<Value>` builder returning the module object.
//! Registration into `VmState.builtin_modules` (so `import <name>`
//! finds them) is Lane D1's, not the builders'.

pub mod modjson;
pub mod modmath;
pub mod modsocket;
pub mod modstruct;
pub mod modsubprocess;

#[cfg(test)]
mod modjson_tests;
#[cfg(test)]
mod modmath_tests;
#[cfg(test)]
mod modsocket_tests;
#[cfg(test)]
mod modstruct_tests;
#[cfg(test)]
mod modsubprocess_tests;
