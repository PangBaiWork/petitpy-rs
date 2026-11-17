//! Compiler track: lexer, parser, scope, compiler, emitter, qstr.
//!
//! Mirrors `src/compiler/` in the original. Phase A1 ports `qstr` and
//! `grammar_data`; the lexer/parser/compiler follow in A2/A3. Lane M
//! ports `mpy` (the .mpy reader/writer) and `mpy_roundtrip`.

pub mod bigint;
pub mod bridge;
pub mod build_options {
    include!(concat!(env!("OUT_DIR"), "/build_options.rs"));
}
pub mod compile;
pub mod emit;
pub mod grammar_data;
pub mod grammar_ext;
pub mod lexer;
pub mod mpy;
pub mod mpy_roundtrip;
pub mod parser;
pub mod qstr;
pub mod scope;
