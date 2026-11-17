# Brief — petitpy-rs Lane C (first chunk): mpz bignum

## Standing rules
Read `docs/agent-rules.md` FIRST. It is the contract; this brief adds
chunk-specific acceptance on top of it. Do not relax any rule in it.

## Goal
Port the MicroPython bignum engine from C to Rust, checked line by line
against the original. This lane runs in parallel with the frontend lane
(A2 lexer) and the .mpy lane. You work ONLY in your own worktree
(`~/workspace/petitpy-rs-vendorc`, branch `lane/vendorc`). This chunk is
mpz ONLY — formatfloat/objfloat/objint/objint_mpz/modio/objstringio/
builtinhelp/warning/modre/re1.5 are later chunks in this same lane.

## Sources (read them first, all of them)
- `~/workspace/petitpy-orig/vendor/micropython/py/mpz.c` (1,758 lines) —
  the bignum engine. Read ALL of it.
- `~/workspace/petitpy-orig/vendor/micropython/py/mpz.h` — the interface
  (function list + semantics).
- `~/workspace/petitpy-orig/vendor/micropython/py/objint_mpz.c`
  (473 lines) — READ it to learn how mpz is USED (which functions the
  int object calls, sign/zero conventions at the boundary), but do NOT
  port it in this chunk.
- `~/workspace/petitpy-rs/docs/comparison-and-plan.md` — project rules.

## Procedure
1. Work ONLY in `~/workspace/petitpy-rs-vendorc/` (branch `lane/vendorc`).
   Create `src/vendor/` with the port. `mpz.c` is 1,758 lines — split
   into submodules if a single file would exceed 800 lines (e.g.
   `src/vendor/mpz.rs` + `src/vendor/mpz_ops.rs`), keeping a 1:1
   function mapping to `mpz.c`; the result file must list the mapping.
2. Port every `mpz_*` function: digit-array representation (same limb
   size and normalization as the C — zero is the empty/zeroed array,
   sign carried separately, exactly as `mpz.c` does it), add/sub/mul/
   divmod (incl. negative-operand semantics — MicroPython follows
   CPython floor-division semantics, VERIFY this from the code, do not
   assume), shifts, bit ops, comparisons, bit_length, pow, conversions
   to/from small ints, strings, and bytes.
3. The object-model binding (`objint`) comes later — this chunk exposes
   a standalone `BigInt` type with the `mpz.c` operations. Do not invent
   a Value/Obj interface here.
4. Pure Rust std, no external crates, no new `unsafe` without a written
   justification. Every `.rs` file < 800 lines.
5. `git commit` when done (one commit for this chunk, clear message).

## Acceptance (all must hold; report every number)
- `cargo test --release --lib --tests`: a Rust test suite covering EVERY
  ported `mpz_*` function (state the count per function group).
- Differential check (oracle: CPython big ints, via `python3`):
  a probe battery of at least 200 cases across add/sub/mul/divmod/pow/
  shifts/bitops/compare/bit_length/conversions, incl. 0, negatives,
  huge values (100+ digits), and the exact edge cases `mpz.c` handles
  specially (read the code for them — e.g. division by zero error path,
  `mpz_divmod` sign rules, `mpz_pow` with negative exponent). ALL cases
  must agree with CPython. Report the case count and any mismatch.
- `cargo build --release` clean, 0 warnings (file-level `allow` only
  with a written justification).
- No new `unsafe`; pure Rust std.
- Write the full result (files ported, line counts, function mapping,
  test numbers, probe battery count, anything that did NOT port cleanly
  and why) to `scripts/vendorc_result.txt` and report back.

## Hard constraints
- No behavior change vs `mpz.c`. Do NOT touch
  `~/workspace/petitpy-orig/` (read-only reference).
- Do NOT touch the main worktree `~/workspace/petitpy-rs/` or the other
  lane's worktree. Do NOT port objint/objfloat/formatfloat in this
  chunk. Stay in scope.
