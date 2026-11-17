# Agent rules — petitpy-rs (standing rules for EVERY lane)

These rules come from the user (PangBai). They apply to every agent,
every chunk, no exceptions. A brief may add chunk-specific acceptance,
but it can never relax these.

## 1. 对照原项目，不许自创
- Every piece of Rust code must be checked line by line against the
  original (`~/workspace/petitpy-orig/`, petitpy HEAD `df7de3a`,
  vendor MicroPython `b14d129a1`). Each implementation must be traceable
  to a specific file (and function) in the original.
- No invented behavior, no "reasonable" substitutes. If the original's
  behavior is unclear, read the original's code AND its tests before
  deciding. If you still have to guess, write the guess down in the
  result file as a guess — never present it as verified.
- The original is READ-ONLY. Never modify anything under
  `~/workspace/petitpy-orig/`.

## 2. 同一套测试是验收标准
- The original's test suite is the bar: corpus (552), integration
  (202/202), golden-programs (584), capability (67), and the Zig unit
  tests in the ported files. Your own tests are EXTRA EVIDENCE ONLY —
  they never replace the original's tests as acceptance.
- If an original test cannot be run yet (e.g. it needs a not-yet-ported
  module), say so in the result file and cover its assertions with the
  strongest available check. Do not silently drop it.
- Report every number: tests run, passed, failed, skipped — with the
  exact command you ran. I (the main agent) re-run everything myself.

## 3. 纯 Rust，干净的代码
- Pure Rust std only. NO external crates.
- NO new `unsafe` unless the original's semantics force it — and then
  only with a written justification in code and in the result file.
- Every `.rs` file < 800 lines. Split before you exceed it.
- `cargo build --release` must be clean (0 warnings). A file-level
  `allow` is permitted only with a written justification in code.
- Mirror the original's module layout in Rust (e.g. `compiler/qstr.zig`
  → `src/compiler/qstr.rs`). Do not reorganize for taste.

## 4. Git discipline
- Work ONLY in your own worktree/branch. Never touch another lane's
  worktree or the main worktree.
- One commit per chunk, clear message stating what was ported and the
  test numbers. No force-push, no rebasing someone else's branch.
- Write the full result to `scripts/<lane>_result.txt` (files ported,
  line counts, test numbers, divergences, what did NOT port cleanly and
  why) and report back.

## 5. 不许假完成
- "不允许再用这种假装做完的状态欺骗我" (user, 2026-11-12). If a chunk
  is not done, say what is missing and why. A partial result reported
  as complete is the worst failure mode of this project.
- Acceptance criteria are written in the brief BEFORE the work starts.
  They do not move because the work got hard.
