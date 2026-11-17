//! Parse-tree dump for one file, in the rich wire format of
//! `ParseTree::dump_wire` (see `src/compiler/parser/nodes.rs`):
//! one line per node, preorder, 2-space indent per depth:
//!   rule <id> <line>
//!   small_int <v>
//!   id <hex>          (qstr id, lowercase hex)
//!   string <hex>      (qstr id, lowercase hex)
//!   token <ord>
//!   null
//!   const_object <kind> <len> <hex>   (bytes payload; for a folded
//!     tuple: `const_object 5 <n>` then the n children at depth+1)
//!
//! The scratch Zig program in `scripts/check_parser_zigdiff.py`
//! prints the same format from the original parser; the check script
//! diffs the two streams byte-for-byte over the corpus.
//!
//! Exit 0: parsed cleanly. Exit 1: parse error — prints
//! `PARSE_ERROR <kind>` (SyntaxError / IndentationError /
//! DedentMismatch / OutOfMemory) and nothing else. Exit 2: infra
//! error (missing argument, unreadable file).
//!
//! Usage: `cargo run --release --example parse_dump -- <file.py>`

use petitpy_rs::compiler::lexer::Lexer;
use petitpy_rs::compiler::parser::{parse, InputKind, ParseError};
use petitpy_rs::compiler::qstr::QstrContext;
use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: parse_dump <file.py>");
        return ExitCode::from(2);
    }
    let src = match std::fs::read(&args[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args[1]);
            return ExitCode::from(2);
        }
    };

    let mut qctx = QstrContext::new();
    let mut lex = match Lexer::new_from_str(0, &src, true) {
        Ok(l) => l,
        Err(e) => {
            // Debug format is the variant name (SyntaxError, ...),
            // matching the Zig error name the dump program prints.
            println!("PARSE_ERROR {:?}", ParseError::from(e));
            return ExitCode::from(1);
        }
    };
    match parse(&mut lex, &mut qctx, InputKind::FileInput) {
        Ok(tree) => {
            let out = std::io::stdout();
            let mut h = out.lock();
            let _ = h.write_all(tree.dump_wire().as_bytes());
            ExitCode::from(0)
        }
        Err(e) => {
            println!("PARSE_ERROR {e:?}");
            ExitCode::from(1)
        }
    }
}
