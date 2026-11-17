//! petitpy-rs: Rust reimplementation of petitpy 0.9.0 (Abilian).
//!
//! Native `make build` (full config) functionality, checked against the
//! original Zig + MicroPython sources file by file. See
//! `docs/comparison-and-plan.md` for the scope and the phased plan.

pub mod compiler;
pub mod memory;
pub mod objects;
pub mod pyerr;
pub mod vendor;
