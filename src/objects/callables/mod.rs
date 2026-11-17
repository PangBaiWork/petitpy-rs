//! Callable objects (C10): bytecode functions, built-in functions,
//! bound methods, closures (Lane C10a) and generators (Lane C10b).
//!
//! Lane C10a ports `objfun.zig` + `objboundmeth.zig` +
//! `objclosure.zig`; Lane C10b fills in `objgenerator.rs`.
//! The runner seam (`RunOutcome`, `objfun.rs`) is the shared
//! contract with C13 (the VM): the execute step of a bytecode call
//! is a parameter, not a global.
//!
//! NOTE: C10a and C10b each define their own local `RunOutcome`
//! (`objfun::RunOutcome` = Normal/Exception/NeedsVm,
//! `objgenerator::RunOutcome` = Normal/Yield/Exception, with the
//! generator runner also taking the send/throw values). C13 unifies
//! them into one runner type; do NOT unify them here.

pub mod objboundmeth;
pub mod objclosure;
pub mod objfun;
pub mod objgenerator;

#[cfg(test)]
pub(crate) mod callables_tests;
#[cfg(test)]
pub(crate) mod callables_tests_b;
#[cfg(test)]
pub(crate) mod callables_tests_c;
#[cfg(test)]
pub(crate) mod objgenerator_tests;
#[cfg(test)]
pub(crate) mod objgenerator_tests_b;
