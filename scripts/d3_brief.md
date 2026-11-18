# Lane D3 — library modules (modjson + modmath + modsocket + modstruct + mod_subprocess) — BRIEF

## Scope
Port five modules (2,074 lines total) into Rust:
`src/modules/modjson.zig` (455) + `src/modules/modmath.zig` (441) +
`src/modules/modsocket.zig` (431) + `src/modules/modstruct.zig` (374) +
`src/modules/mod_subprocess.zig` (373). Pure Rust std (`std::net` for
socket, `std::process` for subprocess, `f64` methods for math).

## Grounding (read these before writing code)
- Originals (all read-only in `~/workspace/petitpy-orig/src/modules/`).
  Each module's globals table is a `[_]RomEntry` — read the WHOLE table
  (line numbers below are where the table STARTS). The name lists here
  are ORIENTATION ONLY — verify against the table + vendor C.
  - `modjson.zig`: table `mp_module__json_globals_table` at **:431**.
    NOTE the module name is `_json` (`MP_REGISTER_MODULE(MP_QSTR__json,
    ...)` at :25). Orientation: `decode encode string`. CHECK FIRST
    whether `json.loads`/`dumps` live in Python (`petitpy-lib`) on top
    of this C module — probe `import json; json.loads('{"a":1}')` on the
    reference binary. If the Python layer exists, D3 ports ONLY the C
    `_json` part and records the Python part as D5's (frozen bundle).
  - `modmath.zig`: table `mp_module_math_globals_table` at **:376**.
    Orientation: `acos asin atan atan2 ceil copysign cos e exp fabs floor
    fmod frexp hypot inf isfinite isinf ldexp log log10 log2 modf nan pi
    pow sin sqrt tan tau`.
  - `modsocket.zig`: table `mp_module_socket_globals_table` at **:367**.
    Orientation: `fromfd socket` (+ constants — read the table; socket
    constants like `AF_INET`/`SOCK_STREAM` may be entries too).
  - `modstruct.zig`: table `mp_module_struct_globals_table` at **:333**.
    Orientation: `calcsize pack pack_into unpack unpack_from` (+ `error`
    type? verify).
  - `mod_subprocess.zig`: table `mp_module__subprocess_globals_table`
    at **:342**. Orientation: `_spawn_and_wait` (+ module name is
    `_subprocess` — verify what Python layer, if any, sits on top).
- Rust current state: C18a landed float/complex type slots; RE15 landed
  the re1.5 engine (`import ure`). D1 owns builtin-module registration
  (the registry mechanism); D3's modules register through it — document
  the registration call in result §4, do NOT invent a second registry.
  If D1 hasn't landed yet, build the module objects + globals dicts and
  leave registration as a named handover (exact fn name + file).

## Behavior rules
- **math**: error behavior must match the original — probe the reference
  binary for `math.sqrt(-1)`, `math.log(0)`, `math.log(-1)`,
  `math.exp(1000)`, `math.pow(0, -1)` (exception type + message).
  NaN/inf formatting rides C8c's float print (shortest round-trip).
- **struct**: format codes are the original's — read the vendor
  `py/modstruct.c` format table, don't assume CPython's set is identical.
  Probe `struct.calcsize` / `pack` / `unpack` round-trips + error
  messages on the reference.
- **socket**: `socket.socket()` + connect/send/recv against a local
  loopback server in tests (bind 127.0.0.1:0, no external network).
  Timeouts must be short so a failure can't hang the suite.
- **subprocess**: `_spawn_and_wait` via `std::process::Command`;
  probe the reference for the exact return shape (exit code? tuple?).
- **json**: if `_json.decode`/`encode` are the C entry points, their
  input/output shapes come from the Zig + vendor `py/modjson.c` — read
  both, probe `json` (or `_json`) round-trips on the reference.

## Acceptance
- `cargo test --release --lib --tests`: baseline **1249 + ≥30 new**,
  0 failed; `cargo test --release --bins`: 9 passed; 0 warnings;
  no `unsafe`; every file <800 lines (value.rs 886 is recorded debt —
  DO NOT GROW IT); original repo clean.
- New tests in NEW files (one per module; don't crowd nearly-full
  files). End-to-end Vm tests (`fx.run_ok` style) for: math fn values +
  domain errors, struct pack/unpack round-trips, json round-trips,
  socket loopback, subprocess exit code.
- `scripts/check_compile_mpy.py`: PASS (552/552).
- One commit: "Lane D3: library modules — json/math/socket/struct/subprocess".
- Result file `scripts/d3_result.txt`: §1 per-function mapping table
  (Zig line numbers + Rust locations); §2 probes; §3 bugs found;
  §4 not-done + handovers (D1 registration call, D5 Python-layer items
  if any, anything needing Phase E entry).

## Working rules (agent-rules.md)
对照原文、不许自创、同一套测试、纯 Rust std、文件<800行、无 unsafe、
原项目只读、每块一提交、不许假完成. Work in small steps: one file at a
time, run `cargo test --release --lib --tests` after each file, keep the
final report short. Base: master `c9fc4e6`. Worktree
`~/workspace/petitpy-rs-d3-lib`, branch `lane/d3-lib`.
