//! Bridge — port of `bridge.zig` (123 lines, 0 test blocks).
//!
//! The Zig bridge is the C-ABI seam between the Zig compiler and the
//! C embed: it takes a source buffer, runs lexer → parser → compile,
//! and hands back a malloc'd `.mpy` buffer (or an `ERR_*` code).
//! The Rust form is a safe API: `compile_to_mpy` returns an owned
//! `Vec<u8>` on success, or a `CompileError`.
//!
//! The C malloc/free out-buffer contract becomes `Vec` ownership.
//! ERR_* codes map onto the `CompileError` enum (`From<ParseError>`
//! carries the same mapping as `bridge.zig`: `DedentMismatch`
//! surfaces as an indentation error). No `extern "C"` exports — the
//! C-ABI export is Phase F work.
//!
//! Internal pipeline (matches bridge.zig's comment): lexer → parser
//! → compile → mpy.write.

use crate::compiler::compile::driver;
use crate::compiler::compile::CompileError;
use crate::compiler::lexer::{LexError, Lexer};
use crate::compiler::mpy;
use crate::compiler::parser::{parse, InputKind};

/// Compile Python source to a `.mpy` byte vector.
///
/// `kind` selects the input grammar: `FileInput` for a whole file,
/// `SingleInput` for REPL lines (bare expressions are printed),
/// `EvalInput` for `eval()`-style single expressions.
///
/// The source name seeds qstr-table index 0 and therefore affects the
/// emitted bytes; `compile_to_mpy_named` lets callers pass the same
/// path string `petitpy --emit-mpy` uses. `compile_to_mpy` uses the
/// empty name.
pub fn compile_to_mpy(source: &str, kind: InputKind) -> Result<Vec<u8>, CompileError> {
    compile_to_mpy_named(source, kind, "")
}

/// Like `compile_to_mpy`, but the emitted module records `source_name`
/// as its source file (qstr index 0).
pub fn compile_to_mpy_named(
    source: &str,
    kind: InputKind,
    source_name: &str,
) -> Result<Vec<u8>, CompileError> {
    let mut qctx = crate::compiler::qstr::QstrContext::new();
    // The source name is interned first so it lands at qstr index 0,
    // exactly as the Zig bridge seeds it via `qstr_from_strn`.
    let source_file_qstr = qctx.from_strn(source_name.as_bytes()).unwrap_or(0);

    let mut lex = match Lexer::new_from_str(source_file_qstr, source.as_bytes(), false) {
        Ok(lex) => lex,
        Err(LexError::UnicodeNameEscape) => return Err(CompileError::SyntaxError),
        Err(LexError::OutOfMemory) => return Err(CompileError::OutOfMemory),
    };
    let tree = parse(&mut lex, &mut qctx, kind).map_err(CompileError::from)?;
    let is_repl = kind == InputKind::SingleInput;
    let module = driver::compile(qctx, tree, source_file_qstr, is_repl)?;
    Ok(mpy::write(&module))
}
