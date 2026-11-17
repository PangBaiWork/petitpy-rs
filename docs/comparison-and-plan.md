# petitpy-rs — comparison and plan

## Original identity
- Repo: `github.com/abilian/petitpy`, branch `devel`, HEAD `df7de3a`
  ("feat: run the full profile in the browser"), 30 commits, created 2026-10-21.
  Local clone: `~/workspace/petitpy-orig/`.
- Version 0.9.0, alpha. License: NOASSERTION (check LICENSE before publishing).
- Architecture: **strangler** — Zig code progressively replaces MicroPython C.
  `build.zig` header: "Compiles the generated MicroPython embed C package
  (vendor/micropython_embed) for the host, linked behind a thin Zig entry
  point. As the strangler progresses, individual C sources are dropped here
  and replaced by Zig modules."
- Vendor MicroPython pinned at `MICROPY_REV := b14d129a1` (Makefile:76);
  auto-cloned to `vendor/micropython` by `make bootstrap` / `make port`.

## What `make build` is (the target)
- `make build` → `make -C port` (generates `vendor/micropython_embed/`:
  qstr tables, moduledefs, embed package) then
  `zig build -Dconfig=$(CONFIG)`, default `CONFIG=full` (ADR 012 Phase 4b).
  Output: Debug native binary `zig-out/bin/petitpy`.
- Needs Zig 0.16 + Python 3 + GNU Make + git. (wasmtime only for wasm.)
- `make build-small` = ReleaseSmall; `make build-release` = ReleaseFast;
  `make web` = wasm32-wasi 377 KB. **Out of scope for this port: wasm/web,
  nano-config shipping arm.** Scope is native `make build` (full config)
  functionality only. The nano arm (541/541 byte-exact vs MicroPython) is a
  later invariant check, not a build target.

## Measured scope (2026-11-17)
- Zig: **53,654 lines, 123 files** (`src/`, `include/`, `port/`,
  `petitpy-lib/`). C/H in repo (excl. vendor): 1,875 lines, 11 files
  (mostly `port/` bridges + `src/runtime/nlr_trampoline.c`).
- Vendor C carve-out actually compiled by `build.zig` (full config):
  `py/objint.c`, `py/objint_mpz.c`, `py/mpz.c` (bignum engine),
  `py/modio.c`, `py/objstringio.c`, `py/builtinhelp.c`, `py/warning.c`,
  `py/objfloat.c`, `py/formatfloat.c`, `extmod/modre.c` + `lib/re1.5/`
  (~1,100 LOC). Everything else in MicroPython's `py/` core has already
  been replaced by Zig. **Measured 2026-11-17 after vendor clone at
  b14d129a1: 5,318 lines total** — mpz.c 1,758; formatfloat.c 569;
  modre.c 519; objint.c 496; objint_mpz.c 473; objfloat.c 339;
  objstringio.c 269; modio.c 232; builtinhelp.c 175; re1.5/*.c 432;
  warning.c 56. Total port scope ≈ 53,654 Zig + 5,318 C ≈ 59k lines.
- Python files in repo: 1,345 (tests, corpus, golden-programs, examples,
  benchmarks, `petitpy-lib/*.py` frozen stdlib).

## Module map — Zig → Rust (planned)
| Zig area | LOC | Rust target | Notes |
|---|---|---|---|
| `src/compiler/` (compile, parser, lexer, emit, scope, qstr, bigint, grammar_data) | 15,394 | `src/compiler/` | Frontend + .mpy emitter. compile.zig alone 5,593. |
| `src/objects/` (obj, objtype, strings, containers, primitives, callables) | 17,544 | `src/objects/` | Object model. objstr.zig 3,864; objtype.zig 2,214. |
| `src/runtime/` (runtime, vm, nlr, gc bridge) | 7,514 | `src/runtime/` | VM dispatch (vm.zig 1,233), runtime.zig 1,616. |
| `src/modules/` (builtins, os, sys, math, file_io, ...) | 6,792 | `src/modules/` | Builtins + native POSIX modules (os/time/socket/select/signal/pwd/grp/subprocess). |
| `src/memory/` (gc) | 866 | `src/memory/` | GC. |
| `src/support/` (stream, output, readline) | 1,032 | `src/support/` | I/O, REPL helpers. |
| `src/entry/` (api, main) | 1,128 | `src/entry/` | CLI, REPL, C embedding API (`petitpy.h` / `libpetitpy.a` surface — native only). |
| `src/frontend/` | 779 | `src/frontend/` | Front-end (REPL/CLI parsing of args?). |
| `src/mp_mirrors.zig`, `src/mp_type_roms.zig` | 1,524 | `src/mp_mirrors.rs` | Type mirrors for vendor C structs — becomes Rust repr(C) structs or disappears where the C is reimplemented. |
| `src/lib.zig`, `main.zig`, `main_vm.zig`, `pyerr.zig`, `vendor_stubs.zig`, `tests.zig` | ~1,500 | `src/lib.rs`, `src/main.rs`, `src/pyerr.rs` | Error union `PyError!T` → Rust `Result<T, PyError>`. |
| vendor C carve-out (list above) | ~TBD | `src/vendor/` | **Reimplemented in Rust, no C FFI** (project rule: pure Rust std, same as pocketpy-rs). mpz bignum is the big one. |
| `petitpy-lib/*.py` (frozen stdlib: json, _thread, heapq, bisect, subprocess wrapper, ...) | — | `python/` vendored + embedded | Same treatment as pocketpy-rs item (14): vendor + `include_str!`. |
| `build.zig` + `port/` (embed generation, qstr scan) | 1,245 + port | build-time tooling | qstr table generation must be reproduced (build script or codegen) so qstr numbering matches. |

## Bytecode format — .mpy (MicroPython)
- petitpy shares MicroPython's `.mpy` bytecode format (README). The Zig
  compiler (`emit.zig`) writes `.mpy`; the Zig VM reads it. Frozen modules
  are `.mpy` baked in via `@embedFile`.
- The Rust port must read/write byte-identical `.mpy` (same opcodes,
  same qstr numbering, same code-object layout) so `.mpy` files are
  interchangeable with the original binary. This is the same class of work
  as pocketpy-rs Phase C (.pyc compatibility), and it is a first-class
  acceptance item, not an afterthought.
- **Not in scope:** CPython `.pyc` (different format; see pocketpy-rs
  findings — same conclusion applies here).

## Test suite — the same tests, no substitutes
- **Corpus** (`corpus/basics/`, 552 files): native full **545/552**
  (7 documented known-fails), nano **541/541** byte-exact vs MicroPython.
  Gates: `make check` (full), `make check CONFIG=nano` (nano invariant),
  `make wasm-check` (out of scope — wasm).
- **Integration smoke** (`tests/test_integration_*.py`): **202/202** native.
- **Golden programs** (`golden-programs/works/`): **584** real programs.
- **Capability gate** (`make check-capability`): 67 language constructs,
  both front-ends × both back-ends agree, 0 gaps.
- Acceptance for every chunk (same discipline as pocketpy-rs): the chunk's
  tests + corpus delta must not regress; final acceptance is the full
  corpus + integration + golden-programs run against the Rust binary vs
  the original binary, plus `.mpy` byte-identity checks.

## Rules (from the user, 2026-11-17)
- Implement **all** native `make build` functionality. No invented
  behavior: every piece of code is checked against the original code
  (`petitpy-orig`), file by file.
- Same test suite as the original. No self-written substitute tests as
  the acceptance bar (own probes are fine as extra evidence, never as
  the bar).
- Interpreter + code optimization both in scope ("解释器和代码优化").
- **petitpy-rs is git-managed from day one** (pocketpy-rs was not — a
  recorded mistake, not repeated here). Commit per chunk; no force-push;
  backups are git history, not rsync copies.

## Phased plan
- **Phase 0 — setup + harness (this chunk).** Git repo, Cargo skeleton,
  Zig 0.16 installed, vendor MicroPython cloned at b14d129a1, reference
  binary built via `make build`, corpus baseline reproduced (545/552),
  differential harness (`scripts/diff_test.py` equivalent) + `.mpy`
  round-trip checker. Acceptance: reference binary runs; baseline numbers
  match README; harness runs the corpus against both binaries.
- **Phase A — core value + qstr + lexer/parser.** `qstr` (interning +
  generated table), `lexer`, `parser`, `scope`, `grammar_data`. Mirrors
  MicroPython's grammar. Acceptance: parse-only differential tests
  (corpus parse errors byte-identical); `.mpy` not yet.
- **Phase B — compiler + emit + .mpy.** `compile.zig`, `emit.zig`,
  `mpy_roundtrip`. Acceptance: `.mpy` bytes byte-identical to the
  original's for the corpus; `mpy` loads in the original binary.
- **Phase C — VM + objects + GC.** `runtime/vm`, `objects/*`,
  `memory/gc`, `pyerr`. Acceptance: corpus basics run under the Rust
  binary; differential vs original byte-identical on passing tests.
- **Phase D — modules + stdlib + POSIX.** `modules/*`, frozen
  `petitpy-lib`, file I/O, os/time/socket/select/signal/subprocess.
  Acceptance: full corpus 545/552, integration 202/202.
- **Phase E — golden programs + optimization.** 584 golden programs;
  then code optimization (peephole / inline-cache style, measured by
  CPU sampling first — same rule as pocketpy-rs: optimize from sampling,
  not assumptions). Acceptance: golden-programs parity; bench vs original.
- **Phase F — embedding API + polish.** `petitpy.h` C API surface as a
  Rust `cdylib`/`staticlib` (native only), REPL polish, docs. Acceptance:
  integration + embed tests green.

## Open questions (not blocking Phase 0)
- The C carve-out: reimplement in Rust (chosen) — mpz bignum is the
  largest single piece; confirm exact LOC once vendor lands.
- `mp_mirrors.zig`: as the C shrinks, mirrors shrink too; the Rust port
  drops mirrors for reimplemented C and keeps `repr(C)` only where a C
  struct layout is load-bearing for `.mpy`/qstr compatibility.
- Config flags (`-Dconfig=nano/full`, `-Dfeature-*`): Rust port builds
  `full` first; feature flags become Cargo features later if needed.
- License: NOASSERTION — verify LICENSE text before any publish.
