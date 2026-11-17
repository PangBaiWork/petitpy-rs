//! Bytecode emitter — pure-Rust port of `src/compiler/emit.zig`
//! (1,682 lines, 12 test blocks).
//!
//! Split across submodules so no file exceeds 800 lines (the A2
//! lexer precedent): `consts` (vendor constants), `encoders`
//! (VLI encoders), `const_obj` (`ConstObj`), `common`
//! (`EmitCommon`), `emit` (`Emit` + opcode emitters), `writers`
//! (buffer writers, prelude packing, driver helpers).

pub mod common;
pub mod const_obj;
pub mod consts;
pub mod emit;
pub mod encoders;
#[cfg(test)]
mod tests;
pub mod writers;

pub use common::EmitCommon;
pub use const_obj::ConstObj;
pub use consts::{IdOp, Pass};
pub use emit::Emit;
