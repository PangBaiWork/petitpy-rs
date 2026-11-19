//! Compile a `.py` file to `.mpy` with the Rust compiler.
//!
//! Usage: `compile_mpy <input.py> <output.mpy> [file|single|eval] [source-name]`
//!
//! The source name seeds qstr-table index 0 and therefore affects
//! the emitted bytes. It defaults to the input path; pass
//! `<emit-mpy>` to match `petitpy --emit-mpy` byte for byte
//! (the original records that literal name, not the file path).

use petitpy_rs::compiler::bridge::compile_to_mpy_named;
use petitpy_rs::compiler::parser::InputKind;
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 || args.len() > 5 {
        eprintln!(
            "usage: {} <input.py> <output.mpy> [file|single|eval] [source-name]",
            args[0]
        );
        return ExitCode::from(2);
    }
    let input = &args[1];
    let output = &args[2];
    let kind = match args.get(3).map(|s| s.as_str()) {
        None | Some("file") => InputKind::FileInput,
        Some("single") => InputKind::SingleInput,
        Some("eval") => InputKind::EvalInput,
        Some(other) => {
            eprintln!("unknown input kind: {other}");
            return ExitCode::from(2);
        }
    };
    let source_name = args.get(4).map(|s| s.as_str()).unwrap_or(input);
    let source = match fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {input}: {e}");
            return ExitCode::from(1);
        }
    };
    match compile_to_mpy_named(&source, kind, source_name.as_bytes()) {
        Ok(mpy) => match fs::write(output, &mpy) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cannot write {output}: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("compile error: {e}");
            ExitCode::from(1)
        }
    }
}
