# Brief — petitpy-rs Phase A3: parser + scope + grammar_ext + bigint

## Standing rules
Read `docs/agent-rules.md` FIRST. It is the contract; this brief adds
chunk-specific acceptance on top of it. Do not relax any rule in it.

## Goal
Port the parser and its companions from Zig to Rust, checked line by
line against the original. This is the third frontend chunk (A1 qstr +
grammar_data done, A2 lexer done and committed as `3465dd9`). You work
in the MAIN worktree `~/workspace/petitpy-rs/` (branch `master`) —
the frontend lane lives here. Do NOT touch the other lanes' worktrees
(`petitpy-rs-mpy`, `petitpy-rs-vendorc`).

## Sources (read them first, all of them)
- `~/workspace/petitpy-orig/src/compiler/parser.zig` (1,878 lines,
  8 `test` blocks). No `@cImport` (header comment says so). Imports
  lexer, grammar_ext, qstr, grammar_data, bigint, build_options.
- `~/workspace/petitpy-orig/src/compiler/scope.zig` (605 lines,
  11 `test` blocks). Imports parser, qstr.
- `~/workspace/petitpy-orig/src/compiler/grammar_ext.zig` (166 lines,
  0 test blocks) — extension grammar rules petitpy adds on top of
  vendor `py/grammar.h` (match statement etc.). Rule ids NUM_RULES..255.
- `~/workspace/petitpy-orig/src/compiler/bigint.zig` (561 lines,
  35 `test` blocks) — integer-literal parsing + constant folding
  (`parseLiteral`, `foldBinary`, `foldUnary`), used by the parser.
- `~/workspace/petitpy-rs/src/compiler/lexer/` (A2) + `qstr.rs` +
  `grammar_data.rs` (A1) — reuse them, do not re-implement.
- `~/workspace/petitpy-rs/docs/comparison-and-plan.md` — project rules.

## Procedure
1. Work ONLY in `~/workspace/petitpy-rs/` (branch `master`). Mirror the
   Zig layout: `src/compiler/parser.rs` (+ submodules if a single file
   would exceed 800 lines — parser.zig is 1,878 lines, so split it the
   way A2 split the lexer, keeping a 1:1 function mapping listed in the
   result file), `src/compiler/scope.rs`, `src/compiler/grammar_ext.rs`,
   `src/compiler/bigint.rs`. Wire into `src/compiler/mod.rs`.
2. Port the parser faithfully: grammar-driven parse engine (rule act/arg
   encoding, AND/OR/LIST handling, node building), soft-keyword hooks
   (match/case), error positions, constant folding via bigint.
   Zig error unions → `Result<T, PyError>` (extend `src/pyerr.rs`
   minimally if the parser needs error kinds that do not exist yet).
3. Pure Rust std, no external crates, no new `unsafe` without a written
   justification. Every `.rs` file < 800 lines.
4. `git commit` when done (one commit for this chunk, clear message).
   NOTE: do NOT run `git add -A` in this worktree while another lane is
   in flight — commit only your own files by path.

## Acceptance (all must hold; report every number)
- `cargo test --release --lib --tests`: ALL ported `test` blocks pass
  as Rust tests — parser 8 + scope 11 + bigint 35 = 54 blocks (state
  the exact count and how each maps to the Zig source). grammar_ext
  has no test blocks; its rules are covered by the parser tests + the
  differential check below.
- Differential check, strong form (same pattern as A2's
  `check_lexer_zigdiff.py`, which worked): build a scratch Zig
  parse-tree dump program linking the ORIGINAL parser.zig + lexer.zig
  + qstr.zig + grammar_data.zig, and a Rust `examples/parse_dump.rs`
  in the same wire format; the parse trees must be byte-identical on
  ALL 552 corpus files. If the harness proves infeasible, the fallback
  is the verdict check below PLUS error-position agreement on every
  file the original rejects — but infeasibility must be documented
  with the exact blocker, not silently chosen.
- Verdict check: parse all 552 `corpus/basics/` files with the Rust
  parser; the parse-ok / parse-error verdict must match the original
  binary's verdict on every file (run the original binary
  `~/workspace/petitpy-orig/zig-out/bin/petitpy` on the same files;
  distinguish parse errors from runtime errors the way A2 did for
  `deque_micropython.py`).
- `cargo build --release` clean, 0 warnings (file-level `allow` only
  with a written justification).
- No new `unsafe`; pure Rust std.
- Write the full result (files ported, line counts, test numbers, the
  differential-check method and result, anything that did NOT port
  cleanly and why) to `scripts/a3_result.txt` and report back.

## Hard constraints
- No behavior change vs the original. Do NOT touch
  `~/workspace/petitpy-orig/` (read-only reference).
- Do NOT touch the other lanes' worktrees. Do NOT start the compiler/
  emit — that's Phase B. Stay in scope.
