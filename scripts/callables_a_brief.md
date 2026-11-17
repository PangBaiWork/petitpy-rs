# Lane C10a — callables part 1: fun + boundmeth + closure — BRIEF

Base: master `553d9fc` (lib 542 + bin 9). Worktree
`~/workspace/petitpy-rs-callables-a`, branch `lane/callables-a`.
Result file: `scripts/callables_a_result.txt` (write it, then ONE
commit: `Lane C10a: port fun + boundmeth + closure to Rust`).

## Sources (original, read-only)

- `~/workspace/petitpy-orig/src/objects/callables/objfun.zig` — 532 lines, 0 test blocks
- `~/workspace/petitpy-orig/src/objects/callables/objboundmeth.zig` — 98 lines, 0 test blocks
- `~/workspace/petitpy-orig/src/objects/callables/objclosure.zig` — 90 lines, 0 test blocks
- Total 720 lines. Reference binary `~/workspace/petitpy-orig/zig-out/bin/petitpy`.

## Target

`src/objects/callables/` (new): `objfun.rs`, `objboundmeth.rs`,
`objclosure.rs`, `callables_tests.rs` (split the test file if it
approaches 800 lines), plus `mod.rs` entries. `mod.rs` is SHARED
with Lane C10b — the second lane to merge needs integration care;
the main agent handles that, you just keep your own `mod.rs`
changes minimal and additive.

## Frozen interfaces (do NOT re-decide)

- C0 `Value` / `Obj` / `GcArena` / `RootSet` / `CallCtx` (4-arg
  `CallCtx::new`, `ctx.types: &BuiltinTypes`, `ctx.pool:
  &RuntimeQstr`) — `src/objects/value.rs`, `src/memory/gc.rs`.
- Slot dispatch: `call_unary` / `call_binary` (`src/objects/obj.rs`,
  `pub(crate)`); slot convention: a slot returning
  `Err(PyError::PyRaise)` = Zig NULL = "not implemented", the
  message is dropped at the slot boundary until C13 adds
  `pending_exc` (C6 convention — keep it, do not invent a
  side channel).
- `setup_code_state` (`src/runtime/bc.rs:350`, Lane C5) — already
  ported; `fun_bc_call` must use it, not re-port it. Its
  signature: `(fun, code: &RawCode, qstr_table, extra_args,
  empty_tuple, n_state, n_args, n_kw, args, arena, roots) ->
  Result<CodeState, SetupError>`.
- `BuiltinFn` (the Rust fn-pointer type for builtin functions,
  `FunBuiltinData { name, n_args, func }` in `value.rs`) — the
  `fun_builtin_*_call` adapters call it directly.
- Payload stubs in `value.rs` (C10 owns them, extend as needed,
  keep `children()` tracing correct): `FunBcData { name: u32,
  n_args: u16, code: usize }` — "code is an index into the VM's
  code store (owned by C13)"; `FunBuiltinData`; `BoundMethData
  { func: Value, self_: Value }`; `ClosureData { fun: GcRef,
  cells: Vec<GcRef> }`.

## What to port

### objfun.zig

- `fun_builtin_0/1/2/3/var_call` (objfun.zig:72-100): the
  FunBuiltin call adapters. These are FULLY portable — they call
  the builtin's Rust fn pointer. Tests must pin each arity
  (including the arg-count behaviour the Zig checks).
- `mp_obj_fun_get_name` (:167), `fun_bc_print` (:453),
  `mp_obj_fun_bc_attr` (:465), `builtinFunType` (:405, the
  type-data constructor — fill the slots this lane owns).
- `mp_obj_new_fun_bc` (:204): takes `def_args, code, context,
  child_table`. READ IT FIRST: the Zig stores default
  positional/kw args TRAILING the struct
  (`MpObjFunBc.extraArgsPtr`, `mp_mirrors.zig:118`) and the
  struct itself is `{ base, context, child_table, bytecode }`
  (`mp_mirrors.zig:109-113`). The Rust `FunBcData` stub has NO
  def-args field — decide after reading the function whether
  to extend it (e.g. `def_pos: Vec<Value>` + kw defaults) and
  document the decision with the Zig line numbers. The
  `context` (module context / globals) and `child_table` are
  C13/C12's — do NOT invent a globals model.
- `fun_bc_call` (:315): prelude decode (`decodePreludeSig`),
  code-state allocation, `mp_setup_code_state`, then
  `mp_execute_bytecode(code_state, 0)` (:~366), globals
  save/restore (`globalsGet` / `globalsSet`), result handling.
  THE VM RUN IS C13's. Port it as TWO pieces:
  1. `fun_bc_prepare_call(...) -> Result<CodeState, ...>` —
     everything up to the `mp_execute_bytecode` line, fully
     portable and directly tested (prelude decode, `n_state`,
     setup errors — note the B2 polled-sentinel comment at
     :353: setup can set pending_exc, so a setup error must be
     polled, not raised immediately).
  2. The full call behind a RUNNER SEAM: the execute step is a
     parameter `runner: &mut dyn FnMut(&mut CallCtx, &mut
     CodeState) -> RunOutcome`, where `RunOutcome` is a small
     enum YOU define locally in this lane (`Normal` /
     `Exception` / `Yield`, with the result value read from
     the CodeState afterwards, mirroring the Zig's
     `code_state.sp[0]`). Test the full call with a STUB
     runner (records the prepared CodeState, returns Normal
     with a sentinel) to pin the return handling. The seam is
     LOCAL to this lane — Lane C10b defines its own; the main
     agent unifies the two at merge time and C13 is the
     consumer. Document the seam in the result file.
  The globals save/restore lines are C13's — mark them, do not
  implement them.

### objboundmeth.zig

- `bound_meth_call` (:24): delegates to the underlying fun.
  Builtin-fun path is live (test it); FunBc path goes through
  the same runner seam (test it with a stub runner).
- `bound_meth_unary_op` (:29) / `bound_meth_binary_op` (:36):
  delegate via `call_unary` / `call_binary`.
- `mp_obj_new_bound_meth` (:48), `bound_meth_attr` (:57).

### objclosure.zig

- `npy_clos_fn_call` (:21): same runner seam as `fun_bc_call`.
- `mp_obj_new_closure` (:42) — cells come from C7's `CellData`;
  `mp_obj_closure_attr` (:54).

## Verification rules (read before writing code)

- **对照原文，不许自创**: every function maps to a named Zig
  function; cite file:line in doc comments for the decisive
  ones. If the Zig disagrees with the brief, the ZIG wins —
  record the correction in the result file (this has happened
  in C4, C7, C9b — it is expected, not a failure).
- Probe the REFERENCE BINARY for observable behaviour
  (error messages, arg-count edge cases, `__name__` /
  `__qualname__` values, `repr` of a function). Probe crash
  cases in SEPARATE scripts (a later abort loses buffered
  stdout). If the original aborts (rc=134) on an input, do NOT
  reproduce the abort — return a deliberate error and document
  it as an intentional divergence (C8a/C8b/C9b precedent).
- **Work in small steps**: one file at a time, run
  `cargo test --release --lib --tests` after each file, keep
  the final report short. (Two earlier executors died with
  nothing written after one giant write.)
- **Per-file size hard rule**: `wc -l` after every file; if a
  file would exceed 800 lines, split it IN PLACE (like C9a's
  map/objdict split). No file over 800 lines, no exceptions.
- **Do NOT touch other lanes' code.** You may only change:
  `src/objects/callables/` (new files), `src/objects/value.rs`
  (payload extensions only, additive), `src/objects/obj.rs`
  (only if a slot the type data needs is missing — additive,
  and say so in the result file), the shared fixture
  (`src/objects/containers/tests.rs`, additive fields only),
  `scripts/callables_a_result.txt`. Do NOT touch
  `src/runtime/`, `src/compiler/`, `src/memory/` (except via
  their public APIs), or any other lane's files.
- No `unsafe`. Pure Rust std only. The original repo
  `~/workspace/petitpy-orig` stays clean (`git status
  --porcelain` empty when done).

## Acceptance

- `cargo test --release --lib --tests`: baseline 542 + ≥20 new,
  0 failed, 1 ignored. `cargo test --release --bins`: 9 passed.
- `cargo build --release`: 0 warnings.
- No `unsafe` in this lane's code (a doc comment mentioning the
  word is fine).
- Every file < 800 lines (`wc -l`).
- Original repo clean.
- Result file `scripts/callables_a_result.txt` with: the commit
  hash, the test counts, the files, every verified finding
  (with the reference-binary probe values), every brief premise
  that turned out wrong, the runner-seam definition, and the
  exact list of what is DEFERRED to C13 (with Zig line refs).
- ONE commit, message `Lane C10a: port fun + boundmeth + closure to Rust`.
