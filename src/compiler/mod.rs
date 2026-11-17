//! Compiler track: lexer, parser, scope, compiler, emitter, qstr.
//!
//! Mirrors `src/compiler/` in the original. Phase A1 ports `qstr` and
//! `grammar_data`; the lexer/parser/compiler follow in A2/A3.

pub mod grammar_data;
pub mod lexer;
pub mod qstr;
