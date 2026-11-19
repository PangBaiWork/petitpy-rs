# petitpy-rs

A Rust reimplementation of [petitpy](https://github.com/abilian/petitpy)
(Abilian, Zig + MicroPython, version 0.9.0, `devel` HEAD `df7de3a`),
covering all native `make build` functionality (full config): the
complete code paths, all syntax, and the `.mpy` bytecode format, with
the interpreter and the code optimizer. Every piece was checked
against the original — no freelancing — and verified against the same
test suite as the original.

Pure Rust, standard library only, no `unsafe` in the VM core (the
only `unsafe` is the FFI boundary of the embedding API, each block
with an invariant comment). Single files stay under 2500 lines.
The project is managed in git; the work is organized as lanes in
`docs/任务书.md` (Chinese task book).

## Status

Final acceptance (F2, master `bdf3c5b` + this lane):

| Gate | Result |
| --- | --- |
| Corpus, full config (`scripts/run_full_gate.py`) | 545/552 — GREEN, exactly the 7 documented known-fails |
| Integration smoke (`scripts/run_integration.py`) | 202/202 with `--features unsafe-ffi`; 200/202 default (see below) |
| Golden programs (`scripts/run_golden.py`) | 585/585 (rust==cpython on 576/585) |
| Capability matrix (`scripts/run_capability.py`) | 67/67 GREEN |
| Test-stdlib (`scripts/run_stdlib.py`) | 8/8 |
| Nano config (`scripts/run_nano.py`) | 454/541, report-only (no Rust nano build exists) |
| `cargo test --release --lib --tests` | 1708 passed, 0 failed, 3 ignored with `--features unsafe-ffi`; 1700 / 0 / 3 default (identical to master `8f9aafa`) |
| `cargo test --release --bins` / `--test entry_cli` | 9 / 45 passed |
| Benchmark vs reference binary | 0.172x wall-clock (limit ≤ 1.5x; `~/workspace/bench_e3.py`, steady-state run 2) |

The reference binary on hand is the 27 MB Debug build, so the
benchmark bar is lenient — but the 5.8x margin leaves large headroom,
and no further optimization was needed.

Integration is 202/202 with `--features unsafe-ffi` (cargo feature
`unsafe-ffi`, default OFF): that feature adds a real POSIX
`select(2)` call (hand-written `extern "C"`, zero dependencies —
see `src/modules/modselect.rs`), which flips the two `select` cases
(`select_readable_fd_is_ready`, `select_timeout_zero_returns_promptly`).
The default build is still 200/202: pure Rust std has no
`select(2)`/`poll(2)`, a helper process cannot inherit the fds, and
reading a byte to stash it would corrupt later `os.read` semantics —
so without the feature the two cases remain a documented divergence,
not faked.

Cross-compile for arm64 (Linux glibc, `aarch64-unknown-linux-gnu`)
works via the committed Zig-based linker wrapper
(`scripts/zig-cc-aarch64.sh`, `.cargo/config.toml`):
`cargo build --target aarch64-unknown-linux-gnu --release
--features unsafe-ffi` produces a real ARM aarch64 ELF (including
`libpetitpy_rs.so` / `.a`). Runtime on arm64 is unverified — this VM
has no arm64 machine; qemu-user was installed and a qemu run was
attempted, but the host has no aarch64 glibc sysroot (no
`ld-linux-aarch64.so.1`), so the dynamic loader cannot be found
and the binary cannot start under qemu here.

## Build

```sh
cargo build --release
```

Produces `target/release/petitpy-rs` (the `petitpy` CLI),
`target/release/petitpy-vm` (the VM-only `.mpy` loader), plus
`libpetitpy_rs.so` / `libpetitpy_rs.a` (the embedding library, see
below). Rust 1.99+; `cargo` must be on `PATH`
(`export PATH="$HOME/.cargo/bin:$PATH"`).

The reference binary used for differential testing is built from the
original petitpy checkout (`zig build`, full config); it is not part
of this repository. The corpus and integration tests live in that
checkout (`corpus/`, `tests/`).

## Usage

`petitpy-rs` mirrors the original CLI byte-for-byte (banner, usage,
streams, exit codes):

```sh
petitpy-rs --version          # petitpy 0.9.0
petitpy-rs --help             # usage to stderr, exit 0
petitpy-rs script.py [args]   # run a script; sys.argv = [script.py, args...]
petitpy-rs -c "print(1)"      # run code; sys.argv = ['-c', ...]
petitpy-rs --emit-mpy f.py    # print f.py compiled to .mpy as base64
petitpy-rs                    # interactive REPL (banner + >>> prompts on stderr)
```

Exit codes: 0 clean / `SystemExit(n)`; 1 uncaught non-`SystemExit`
(traceback to stderr); 2 CLI errors (unreadable script, bad `-c`).
`petitpy-vm file.mpy` loads and runs a `.mpy` file without the
compiler (`eval`/`exec`/`compile` raise `NotImplementedError` there,
as in the original).

The REPL reads one 4095-byte `fgets` chunk per line (the original's
4096-byte buffer); a continuation never happens — the original's
`mp_repl_continue_with_input` is stubbed to always-false, so compound
statements fail line-by-line on both binaries. Lines are measured
with C `strlen` semantics: bytes from the first NUL on are invisible,
and a NUL-first line ends the REPL like EOF.

## Embedding API

`include/petitpy.h` (verbatim copy of the original header) exposes
the `ppy_*` C API (ADR 022, experimental): create interpreters,
`ppy_exec`/`ppy_eval`/`ppy_import`, handles, host functions, error
fetching. `tests/embed/api_test.c` (unmodified, from the original)
passes 61/61 against both the staticlib and the cdylib, normally and
under `NPY_GC_STRESS=10`; `examples/embed/host.c` prints `41` on both
link kinds. See `src/entry/api.rs` and `include/README.md`.

## Tests

```sh
cargo test --release --lib --tests   # unit + VM tests
cargo test --release --bins          # binary smoke tests
cargo test --release --test entry_cli
python3 scripts/check_compile_mpy.py # Rust .mpy output byte-identical to the original (552/552)
python3 scripts/check_frozen.py      # frozen stdlib bundle (23 modules)
python3 scripts/run_full_gate.py     # corpus, full config
python3 scripts/run_integration.py   # integration smoke
python3 scripts/run_golden.py        # golden programs (reference binary as oracle)
python3 scripts/run_capability.py    # capability matrix
python3 scripts/run_stdlib.py        # tests/stdlib
python3 scripts/run_nano.py          # nano config, report-only
```

The gate runners take `--bin` (absolute path); with no arguments
they use the defaults under this repo. The golden runner uses the
reference petitpy binary as its oracle.

## Layout

- `src/compiler/` — lexer, parser, scope, bigint, compile + emit
  (`.mpy` byte-identical output)
- `src/objects/` — values, GC arena, containers, strings, types,
  callables, numbers
- `src/runtime/` — the bytecode VM, dispatch, seams, builtins, modules
- `src/modules/` — `os`/`sys`/`io`, math, socket, subprocess, `gc`,
  `ure`/`re`, frozen stdlib
- `src/entry/` — CLI hosts, REPL, `.mpy` loader, the `ppy_*` C API
- `src/support/` — line reader, output sink, smallint helpers
- `src/bin/` — `petitpy-vm` (+ `zigpy-compile`, a test helper)
- `include/petitpy.h` — the embedding API header (verbatim)
- `scripts/` — the six gate runners, check scripts, per-lane briefs
  and result files (`scripts/<lane>_result.txt`)
- `docs/` — `comparison-and-plan.md` (scouting + scope),
  `任务书.md` (task book: lanes C0–C21, D1–D5, E, F),
  `agent-rules.md` (lane rules), `value-obj-design.md` (frozen
  Value/Obj interface)

## Known divergences (honest list)

These are deliberate, probed, and recorded in the per-lane result
files (`scripts/<lane>_result.txt` §4); nothing here is faked to
pass a gate.

- **Integration 200/202 on the default build** — the two `select`
  cases (see above); with `--features unsafe-ffi` it is 202/202.
  Nano 454/541 is report-only: the original's nano gate is
  541/541, but Rust has no nano build.
- **arm64 runtime unverified** — cross-compile to
  `aarch64-unknown-linux-gnu` passes (Zig linker wrapper, committed),
  but no arm64 machine or qemu run was available in this lane.
- **`socket.fromfd` doesn't own the OS fd** (F5b): `fileno() == fd`
  holds, but `close()` does not close the underlying fd — pure std
  has no `from_raw_fd`. The integration case only asserts `fileno`.
- **`os.close(fd)` only closes `os.open`-tracked fds** (F6 probe):
  closing an `open()`-created fd raises `EBADF` on Rust while the
  reference's real `close(2)` succeeds.
- **File writes are unbuffered on Rust** (F6 probe): writing to
  `/dev/full` raises at `write` on Rust, succeeds on the reference
  (its buffered close swallows the flush error).
- **Subprocess timeout kill is SIGKILL (-9), reference SIGTERM
  (-15)** (D3). **`os.umask` is still a `NotImplementedError` stub**
  (D2). **Signal handlers are recorded but never dispatched**
  (D4, the original's own MVP limitation is mirrored in structure).
- **GC finaliser is drained only at an explicit `gc.collect()`**
  (F6): a stress collection inside `alloc()` defers the `__del__`
  calls to the next explicit collect (objects are seed-marked to
  survive meanwhile). The CLI never stress-collects — `Vm::new`
  never calls `gc_init()` (pre-existing gap; F1 calls it in
  `ppy_new` only). Finaliser tracebacks go to std stderr, not
  through the embedding API's print sink.
- **`float()` of a class defining `__float__` falls through to
  the native value** (F3): `float(G(1.5))` with `G.__float__`
  returning 9.9 is 9.9 on the reference, 1.5 on Rust.
- **Dict lookup with distinct instances using a FunBc `__eq__`**
  (F3): the hash half runs through the hook, but the probe-loop
  comparison is still ctx-only, so `d[A()] = "x"; d[A()]` is a
  `KeyError` on Rust. Same family: `dict.get`/`.pop`/`.setdefault`
  with instance keys, dict-subclass instances with instance keys,
  and `str(KeyError(instance))` (exception-arg printing has no
  hook — it shows the `<Color object at N>` fallback).
- **Identity-hash values differ by design** (F3): the reference
  prints raw ASLR addresses, Rust prints arena indices. The
  mechanism (stable per object, dicts work) is correct.
- **Embedding API edges** (F1): host-function kwargs are rejected;
  a `__getitem__` on user instances through the API is untested;
  non-UTF-8 exec filenames are lossy; NULL arguments are handled
  defensively where the Zig would fault; nested `ppy_*` calls from
  a host function re-enter through the raw interp pointer
  (lock-serialized, same guarantee the Zig relies on).
- **Typing edges** (F5a): `|=` on types does not take the union
  branch (the vendor checks plain `OR` only — probed identical);
  `real_load_maybe` lacks the `__class__`/`__next__` special cases
  (no caller needs them).
- **E1/E2 leftovers**: `type(iter(s))` is a bare `TypeError`;
  `super()`-construction errors lose their messages
  (`super_make_new` still returns the bare signal, F4); a
  dict-subclass `__missing__` reached through ctx-only seams
  yields a bare `TypeError` instead of `KeyError` (no gate
  coverage); the `petitpy-vm` missing-file error prints only the
  first line (`error: FileNotFound`) — the reference's Zig stack
  trace after it is runtime noise, not reproduced.
- **The original's own crashes are not mirrored**: where the
  reference aborts (rc=134, e.g. `str.center` with negative width,
  `(a*)*` in `ure`, `sort(key=len)` on ints), Rust raises or
  returns instead. Where the reference is broken at runtime
  (`complex(...)` calls abort), Rust implements the vendor C code
  properly.
