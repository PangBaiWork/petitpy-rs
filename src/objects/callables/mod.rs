//! Callable objects (C10): bytecode functions, built-in functions,
//! bound methods, closures (Lane C10a) and generators (Lane C10b).
//!
//! Lane C10a ports `objfun.zig` + `objboundmeth.zig` +
//! `objclosure.zig`; Lane C10b fills in `objgenerator.rs`.
//! The runner seam (`RunOutcome`, `objfun.rs`) is the shared
//! contract with C13 (the VM): the execute step of a bytecode call
//! is a parameter, not a global.

pub mod objboundmeth;
pub mod objclosure;
pub mod objfun;

#[cfg(test)]
pub(crate) mod callables_tests;
#[cfg(test)]
pub(crate) mod callables_tests_b;
#[cfg(test)]
pub(crate) mod callables_tests_c;
