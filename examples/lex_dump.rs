//! Token dump for one file, in the wire format of
//! `lexer_test.zig`'s `dumpZig`: one line per token,
//! `idx<TAB>kind<TAB>line<TAB>column<TAB>len<TAB>hexbytes`.
//!
//! The dump loop mirrors `dumpZig` exactly: it prints the current
//! token, stops after printing END or INVALID, and otherwise keeps
//! going (past DEDENT_MISMATCH / LONELY_STRING_OPEN /
//! MALFORMED_FSTRING tokens, which the parser — not the dump —
//! treats as errors).
//!
//! Exit 0: lexed cleanly to END with no error-kind token and no
//! `LexError`. Exit 1: lex error (any error-kind token appeared, or
//! a `LexError` from construction / `to_next`). Exit 2: infra error
//! (missing argument, unreadable file).
//!
//! Usage: `cargo run --release --example lex_dump -- <file.py>`

use petitpy_rs::compiler::lexer::{Lexer, TokenKind};
use std::io::Write;
use std::process::ExitCode;

fn write_hex(out: &mut impl Write, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        let _ = out.write_all(&[HEX[(b >> 4) as usize], HEX[(b & 0x0f) as usize]]);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: lex_dump <file.py>");
        return ExitCode::from(2);
    };
    let src = match std::fs::read(path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("lex_dump: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let mut lex = match Lexer::new_from_str(0, &src, true) {
        Ok(lex) => lex,
        Err(e) => {
            eprintln!("lex_dump: lexer construction failed: {e}");
            return ExitCode::from(1);
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut idx: usize = 0;
    let mut saw_error = false;
    loop {
        let kind = lex.tok_kind();
        // `write!` (not `writeln!`): the hex bytes continue on this
        // same line, then the trailing `writeln!` ends the line.
        let _ = write!(
            out,
            "{idx}\t{}\t{}\t{}\t{}\t",
            kind as u8,
            lex.tok_line(),
            lex.tok_column(),
            lex.vstr().len()
        );
        write_hex(&mut out, lex.vstr());
        let _ = writeln!(out);
        if matches!(
            kind,
            TokenKind::INVALID
                | TokenKind::DEDENT_MISMATCH
                | TokenKind::LONELY_STRING_OPEN
                | TokenKind::MALFORMED_FSTRING
        ) {
            saw_error = true;
        }
        if kind == TokenKind::END {
            return ExitCode::from(u8::from(saw_error));
        }
        if kind == TokenKind::INVALID {
            return ExitCode::from(1);
        }
        if let Err(e) = lex.to_next() {
            eprintln!("lex_dump: lexer step failed: {e}");
            return ExitCode::from(1);
        }
        idx += 1;
    }
}
