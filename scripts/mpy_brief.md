# Brief — petitpy-rs Lane M: .mpy reader + writer

## Standing rules
Read `docs/agent-rules.md` FIRST. It is the contract; this brief adds
chunk-specific acceptance on top of it. Do not relax any rule in it.

## Goal
Port the .mpy format reader/writer from Zig to Rust, checked line by
line against the original. This lane runs in parallel with the frontend
lane (A2 lexer) and the vendor-C lane (mpz). You work ONLY in your own
worktree (`~/workspace/petitpy-rs-mpy`, branch `lane/mpy`).

## Sources (read them first, all of them)
- `~/workspace/petitpy-orig/src/compiler/mpy.zig` (387 lines) — .mpy read.
- `~/workspace/petitpy-orig/src/compiler/mpy_roundtrip.zig` (96 lines) —
  round-trip tests.
- `~/workspace/petitpy-rs/src/compiler/qstr.rs` (A1, already in your
  worktree) — the reader uses the qstr table; reuse it, do not
  re-implement. Note `src/runtime/qstr.zig` (440 lines) is a SECOND qstr
  implementation in the original; the .mpy reader uses the compiler-side
  one — if you find the two disagree on anything, report it, do not
  silently pick one.
- `~/workspace/petitpy-orig/build-frozen/*.mpy` (23 modules, produced by
  the original's `make check` freeze step) — your fixtures.
- `~/workspace/petitpy-rs/docs/comparison-and-plan.md` — project rules
  (.mpy must be byte-identical with the original's, both directions).

## Procedure
1. Work ONLY in `~/workspace/petitpy-rs-mpy/` (branch `lane/mpy`).
   Mirror the Zig layout: `src/compiler/mpy.rs` ← `mpy.zig`,
   `src/compiler/mpy_roundtrip.rs` ← `mpy_roundtrip.zig`. Wire into
   `src/compiler/mod.rs` + `src/lib.rs`.
2. Port the reader first, then the writer (the writer is the inverse of
   the reader's format; if `mpy.zig` has no writer, the writer lives in
   `emit.zig` — read the emit-side `.mpy` writing code and port ONLY the
   serialization part, leaving the rest of emit for Phase B; say exactly
   what you took from emit.zig and its line range).
3. Establish the verification path yourself (this is part of the job):
   - The original produces `.mpy` files (frozen modules above, and/or
     its compile path — find one reproducible producer and document it).
   - The original binary `~/workspace/petitpy-orig/zig-out/bin/petitpy`
     can run `.mpy` files (already built) — use it as the consumer oracle.
4. Pure Rust std, no external crates, no new `unsafe` without a written
   justification. Every `.rs` file < 800 lines.
5. `git commit` when done (one commit for this chunk, clear message).

## Acceptance (all must hold; report every number)
- `cargo test --release --lib --tests`: ALL ported `mpy_roundtrip.zig`
  cases pass as Rust tests (state the exact count and how each maps to
  the Zig source).
- Read check: the Rust reader reads ALL 23 `build-frozen/*.mpy` files
  without error, and the parsed qstrs/objs/code agree with the Zig
  reader's output for the same files (run the Zig reader — e.g. via the
  original's `zig test`/`zig build` test entry for mpy, or a small Zig
  harness — and diff; document the command).
- Round-trip check: Rust read → Rust write is byte-identical to the
  original `.mpy` bytes for ALL 23 frozen files.
- Write check: `.mpy` files written by the Rust writer for at least 5
  sample programs run under the ORIGINAL binary with output identical
  to running the same programs as `.py` under the original binary.
- `cargo build --release` clean, 0 warnings (file-level `allow` only
  with a written justification).
- No new `unsafe`; pure Rust std.
- Write the full result (files ported, line counts, test numbers, the
  verification path you established, anything that did NOT port cleanly
  and why) to `scripts/mpy_result.txt` and report back.

## Hard constraints
- No behavior change vs the original. Do NOT touch
  `~/workspace/petitpy-orig/` (read-only reference).
- Do NOT touch the main worktree `~/workspace/petitpy-rs/` or the other
  lane's worktree. Do NOT start the parser/compiler/emit — that's the
  frontend lane / Phase B. Stay in scope.
