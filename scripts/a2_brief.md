# Brief — petitpy-rs Phase A2: lexer

## Goal
Port the lexer from Zig to Rust, checked line by line against the original.
This is the second code chunk of petitpy-rs (A1 qstr + grammar_data is done
and committed as `caaca1a`).

## Sources (read them first, all of them)
- `~/workspace/petitpy-orig/src/compiler/lexer.zig` (1,436 lines).
- `~/workspace/petitpy-orig/src/compiler/lexer_test.zig` — read it first to
  see whether it is `test` blocks (port them) or a CLI program (port its
  assertions as Rust tests; if it needs mpy/compile machinery that does
  not exist yet, say so in the result file and cover its assertions with
  the strongest available Rust tests, as A1 did for qstr_test.zig).
- `~/workspace/petitpy-rs/src/compiler/qstr.rs` + `grammar_data.rs` (A1) —
  the lexer builds on these; reuse them, do not re-implement.
- `~/workspace/petitpy-rs/docs/comparison-and-plan.md` — project rules.
- Note: `lexer.zig` uses `@cImport` — find out exactly what C it imports
  and why (probably a header for qstr/grammar constants). The Rust port
  must reproduce the behavior without C FFI; if the import turns out to
  be load-bearing for a table or constant, document how it is reproduced
  (same treatment as A1's build.rs qstr codegen).

## Procedure
1. Work in `~/workspace/petitpy-rs/` (git repo). Mirror the Zig layout:
   `src/compiler/lexer.rs` ← `src/compiler/lexer.zig`. Wire it into
   `src/compiler/mod.rs` + `src/lib.rs`.
2. Port the lexer faithfully: token kinds, keyword table, number/string
   literal scanning (incl. unicode strings — petitpy accepts more than
   MicroPython here), indentation (INDENT/DEDENT), line/column tracking,
   error positions. Zig error unions → `Result<T, PyError>` (extend
   `src/pyerr.rs` minimally if the lexer needs error kinds that do not
   exist yet; keep the enum small and justify each addition).
3. Pure Rust std, no external crates, no new `unsafe` without a written
   justification. Every `.rs` file < 800 lines.
4. `git commit` when done (one commit for this chunk, clear message).

## Acceptance (all must hold; report every number)
- `cargo test --release --lib --tests`: ALL ported `lexer_test.zig`
  cases pass as Rust tests (state the exact count and how each maps to
  the Zig source), plus table/structure tests for the keyword/operator
  tables if any (verify by dump-and-diff against the Zig source, same
  method as A1's check_grammar_data.py).
- Differential check (own evidence, not the bar): lex every file in
  `~/workspace/petitpy-orig/corpus/basics/` (552 files) with a Rust test
  or example binary — no panics, and tokenize-error files fail only
  where the original binary also reports a syntax error. Report the
  count of files lexed cleanly vs errored, and any mismatch vs the
  original's verdict (run the original binary
  `~/workspace/petitpy-orig/zig-out/bin/petitpy` on the same files to
  get its verdict; it is already built).
- `cargo build --release` clean, 0 warnings (file-level `allow` only
  with a written justification, as A1 did).
- No new `unsafe`; pure Rust std.
- Write the full result (files ported, line counts, test numbers, the
  @cImport finding, anything that did NOT port cleanly and why) to
  `scripts/a2_result.txt` and report back.

## Hard constraints
- No behavior change vs the original. Do NOT touch
  `~/workspace/petitpy-orig/` (read-only reference).
- Do NOT start the parser/compiler — that's A3/B. Stay in scope.
