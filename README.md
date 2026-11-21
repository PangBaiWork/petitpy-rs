# petitpy-rs

A Rust reimplementation of [petitpy](https://github.com/abilian/petitpy)
(Abilian, Zig + MicroPython, version 0.9.0, `devel` HEAD `df7de3a`),
covering all native `make build` functionality (full config): the
complete code paths, all syntax, and the `.mpy` bytecode format, with
the interpreter and the code optimizer. Every piece was checked
against the original — no freelancing — and verified against the same
test suite as the original.

Pure Rust, standard library only. `unsafe` exists only at FFI
boundaries — the embedding API, the opt-in `--features unsafe-ffi`
build (real `socket(2)`/DNS syscalls and the `os` fd syscalls),
and one std-only block in subprocess (`BorrowedFd::borrow_raw`) —
each block with an invariant comment. Single files stay under
2500 lines.
The project is managed in git; the work is organized as lanes in
`docs/任务书.md` (Chinese task book).

## Status

Continuous integration (`.github/workflows/build.yml`, runs on every
push): builds `petitpy-rs`, `petitpy-vm` and `zigpy-compile` for
Linux x86_64 + aarch64, macOS x86_64 + aarch64, Windows x86_64
(MSVC) and Android aarch64 + x86_64, and uploads them as artifacts
(`petitpy-rs-<platform>`). The Linux, Windows and macOS rows also
run the test suite under `--features unsafe-ffi` (artifacts
`petitpy-rs-<platform>-unsafe-ffi`; Android rows are compile-only).
Build #34 (2026-11-21) is 7/7 green.

Current status (master `7f4a3bc`; CI Build #34 7/7 green, 2026-11-21):

| Gate | Result |
| --- | --- |
| Corpus, full config (`scripts/run_full_gate.py`) | 545/552 — GREEN, exactly the 7 documented known-fails |
| Integration smoke (`scripts/run_integration.py`) | 202/202 with `--features unsafe-ffi`; 201/202 default (see below) |
| Golden programs (`scripts/run_golden.py`) | 585/585 (rust==cpython on 576/585) |
| Capability matrix (`scripts/run_capability.py`) | 67/67 GREEN |
| Test-stdlib (`scripts/run_stdlib.py`) | 8/8 |
| Nano config (`scripts/run_nano.py`) | 454/541, report-only (no Rust nano build exists) |
| `cargo test --release --lib --tests` | 1896 passed, 0 failed, 5 ignored with `--features unsafe-ffi`; 1854 / 0 / 3 default (master `7f4a3bc`) |
| `cargo test --release --bins` / `--test entry_cli` | 9 / 45 passed |
| Benchmark vs reference binary | 0.172x wall-clock (limit ≤ 1.5x; `~/workspace/bench_e3.py`, steady-state run 2) |

The reference binary on hand is the 27 MB Debug build, so the
benchmark bar is lenient — but the 5.8x margin leaves large headroom,
and no further optimization was needed.

Integration is 202/202 with `--features unsafe-ffi` (cargo feature
`unsafe-ffi`, default OFF) and 201/202 without it. The one
default-build gap is `socket.fromfd`: `NotImplementedError` without
the feature (pure std has no `FromRawFd`), a real fd wrap under the
feature. The two `select` cases (`select_readable_fd_is_ready`,
`select_timeout_zero_returns_promptly`) pass in pure Rust too, via a
per-fd monitor thread + shared read buffer
(`src/support/readiness.rs`). Under the feature, socket and DNS go
through real syscalls instead (hand-written `extern "C"`, zero
dependencies — `src/support/ffi.rs`, `src/support/ffi_dns.rs`).

Cross-compile for arm64 (Linux glibc, `aarch64-unknown-linux-gnu`)
works via the committed Zig-based linker wrapper
(`scripts/zig-cc-aarch64.sh`, `.cargo/config.toml`):
`cargo build --target aarch64-unknown-linux-gnu --release
--features unsafe-ffi` produces a real ARM aarch64 ELF (including
`libpetitpy_rs.so` / `.a`). Runtime on arm64 is verified by CI: the
linux-aarch64 job runs the full test suite natively on GitHub's
ARM64 runners (Build #34 green). Locally this VM still has no arm64
machine; qemu-user was installed and a qemu run was attempted, but
the host has no aarch64 glibc sysroot (no `ld-linux-aarch64.so.1`),
so the dynamic loader cannot be found and the binary cannot start
under qemu here.

## Build

```sh
cargo build --release
```

Produces `target/release/petitpy-rs` (the `petitpy` CLI),
`target/release/petitpy-vm` (the `.mpy` runner), plus
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
`petitpy-vm file.mpy` loads and runs a `.mpy` file. (The original
Zig `petitpy-vm` has no compiler linked, so `eval`/`exec`/`compile`
raise `NotImplementedError` there; the Rust port links the compiler
into the library, so they work inside `petitpy-vm` too.)

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

- **Integration 201/202 on the default build** — the one
  `socket.fromfd` case (see above); with `--features unsafe-ffi`
  it is 202/202. Nano 454/541 is report-only: the original's nano
  gate is 541/541, but Rust has no nano build.
- **`socket.fromfd` is `NotImplementedError` on the default build**
  — pure std has no `FromRawFd` (the integration case only asserts
  `fileno`, so the default build is 201/202). Under
  `--features unsafe-ffi` the socket wraps the fd without dup:
  `fileno() == fd` holds and `close()` closes the OS fd
  (`os.close(fd)` after `s.close()` is `EBADF`, probed), and `recv` /
  `sendall` / `setblocking` / `accept` work on fromfd sockets. The
  wrapped fd stays in the `os_fds` table, so closing the socket
  then dropping the VM aborts a debug build (known hazard;
  release is harmless). On Windows, `fromfd(AF_UNIX)` is
  `NotImplementedError` by design (a plain file fd is never a
  `SOCKET`).
- **Real socket construction / bind / listen under
  `--features unsafe-ffi`** (Socket FFI): the constructor calls
  the real `socket(2)` (a bad family/type fails `EAFNOSUPPORT`
  there, as the reference), `bind()` runs the real `bind(2)`
  on the same fd (the fd number never changes; a client
  connecting between `bind()` and `listen()` is refused
  `ECONNREFUSED`, as the reference), `listen()` runs the real
  `listen(2)` (an unbound socket is `EINVAL`, probed), and
  `bind("")` binds an abstract socket (a client connecting
  to the 108-byte all-NUL name is accepted — the kernel
  matches abstract names by full length). On the default
  build the old shape stays: a real `UdpSocket` placeholder
  bound to `127.0.0.1:0`, `bind()` swaps in an
  already-listening `UnixListener` (fd number changes,
  `bind("")` binds an unnamed socket that listens but is
  unreachable by name).
- **`getsockopt`/`setsockopt` answer from shadow state** (both
  builds): `SO_PEERCRED` and any unknown `(level, opt)` are
  `ENOPROTOOPT`; unset `SO_RCVBUF`/`SO_SNDBUF` are `ENOPROTOOPT`
  (their kernel defaults are unknown — the kernel doubles a set
  value, so the shadow must answer); the other known options
  return their shadow value or known default (`TCP_NODELAY` /
  `SO_BROADCAST` / `SO_REUSEADDR` 0, `IP_TTL` 64, timeouts 16
  zero bytes). `setsockopt` stores in the shadow and lands in
  the kernel where possible.
- **`os.close(fd)` closes any fd under `--features unsafe-ffi`**
  (G6a U3): the real `close(2)` runs for every fd — an
  `open()` file's fd, stdio 0–2 (the number is freed for reuse),
  and registered fds go through the syscall first (a stale
  number fails `EBADF`, like the reference). On the default
  build the old shape stays: only `os.open`-tracked fds close,
  unregistered fds are `EBADF`, stdio is only recorded.
- **`os.umask` is implemented under `--features unsafe-ffi`**
  (G6a U2): the real `umask(2)` runs and returns the previous
  mask. On the default build it is still a
  `NotImplementedError` stub (no std API). `os.umask(-1)`
  wraps on Rust (kernel masks to 0777); the reference aborts
  (Zig `@intCast` panic, rc=134) — N1, not reproduced.
- **`os.fsync` / `os.lseek` on stdio work under
  `--features unsafe-ffi`** (G6a U9): the real `fsync(2)` /
  `lseek(2)` run, so a stdio fd redirected to a regular file
  syncs/seeks; on pipes/ttys the kernel returns `EINVAL` /
  `ESPIPE`, as before. On the default build stdio `fsync` is
  always `EINVAL` and stdio `lseek` is always `ESPIPE`.
- **Subprocess timeout kill is SIGKILL (-9) on the default
  build, reference SIGTERM (-15)** (D3, fixed under
  `--features unsafe-ffi` by G6b U5): the timeout arm sends
  `SIGTERM`, waits up to 1.0 s of grace, then `SIGKILL` — a
  plain `sleep` dies on `SIGTERM` (-15, fast), a child that
  ignores `SIGTERM` is killed after the grace (-9 after
  ~1.2 s), a child that traps `SIGTERM` and exits 0 returns
  0 (all probed on the reference; the brief's "trap-exit-0
  → -15" premise was wrong — -15 is the DEFAULT disposition).
  The timeout path no longer waits for pipe EOF either: a
  killed child's grandchild may hold the pipe open (the old
  code took 5.0 s on a `sh -c 'sleep 5'` timeout, the
  reference 1.3 s) — the drain now streams chunks and the
  timeout path takes what has arrived so far. **Signal
  handlers are recorded but never dispatched** (D4, the
  original's own MVP limitation is mirrored in structure).
- **Subprocess fd stream modes share offset/append under
  `--features unsafe-ffi`** (G6b U8): a plain-fd stdin /
  stdout / stderr goes through a `pre_exec` `dup2` in the
  child — the SAME fd number, so the file offset and
  `O_APPEND` are shared and socket fds work (a socketpair
  fd as stdout delivers to the peer; `stderr=STDOUT` with
  `stdout=fd` dup2's the child's stdout onto stderr, so
  both streams land in the same file in write order). On
  the default build the modes re-open `/proc/self/fd/<fd>`
  (a new open: no shared offset/append, socket fds fail
  `ENXIO`).
- **`sendall` on a listener is `EINVAL`, reference
  `ENOTCONN`** (107, probed while working G6b): the
  `is_stream` gate rejects non-stream sockets before the
  syscall; the reference's `send(2)` there is `ENOTCONN`.
  Out of scope for G6b (the brief leaves `sendall`
  unchanged).
- **Piped stdout is buffered on Rust, like the reference** (G0
  probe, P14 — fixed by lane P14): the reference buffers piped
  stdout (glibc stdio on the `stdout` `FILE*`, `file_io.zig:486`
  + `fwrite`; `print` + `sleep` shows both lines only after the
  sleep); Rust now has one process-global 4096-byte stdout
  buffer (`support/output.rs`: line-flush on a tty, full chunks
  on a pipe, flush at process exit, `sys.stdout.flush()` and
  `print(flush=True)` flush it). Two paths deliberately bypass
  the buffer, per the Zig source: the petitpy-vm traceback and
  the finaliser-drain print (`printExcAndClear` does no `fflush`).
  `os.write(1/2)` flushes `std`'s `Stdout` after writing (the
  reference's is a raw `write(2)` loop). Arrival-time probes
  10/10 byte-identical; `tests/p14_stdout.rs` pins the timing.
- **GC finaliser is drained only at an explicit `gc.collect()`**
  (F6): a stress collection inside `alloc()` defers the `__del__`
  calls to the next explicit collect (objects are seed-marked to
  survive meanwhile). The CLI path now stress-collects too (G2:
  `Vm::new` calls `gc_init()`). Finaliser tracebacks now go to
  fd 1 through the embedding sink when installed (G5 P8: the
  original's protected call prints via `mp_plat_print`); file
  writes are buffered through a 4096-byte `BufWriter` (G5 P3),
  so `/dev/full` errors now surface at flush/close as on the
  reference.
- **`float()` of a class defining `__float__`** (F3, fixed by
  G4/H1): `float(G(1.5))` with `G.__float__` returning 9.9 is 9.9
  on both binaries now — the `float` type object is intercepted
  by GcRef identity and the dunder runs through the VM hook. A
  raising `__float__` is swallowed into the same `TypeError:
  can't convert to float` (probed on the reference).
- **Dict lookup with distinct instances using a FunBc `__eq__`**
  (F3, fixed by G3/H2–H4): the probe comparison, `dict.get` /
  `.pop` / `.setdefault`, and dict-subclass subscripts all run
  through the VM hook now.
- **Exception-arg printing is hook-aware** (F3, fixed by G4/H7):
  `str(KeyError(A()))` is `A-str`, `repr(KeyError(A()))` is
  `KeyError(A-repr,)`, uncaught tracebacks and `sys.print_exception`
  and the REPL echo render instance args through their dunders.
  A raising `__str__`/`__repr__` on an exception arg falls back
  to `<E object at N>` (print) or propagates (str/repr builtins);
  the reference aborts (rc=134, null-pointer panic at
  `mp_mirrors.zig:745`) there — the abort is not reproduced.
- **`type(iter(...))` is the `iterator` type** (E1/E2, fixed by
  G4/H8): sequence iterators carry the `mp_type_polymorph_iter`
  header, range iterators `mp_type_range_it`, dict iterators
  `mp_type_dict_view_it` — all named `iterator`, distinct objects
  exactly as the reference reports them.
- **Identity-hash values differ by design** (F3): the reference
  prints raw ASLR addresses, Rust prints arena indices. The
  mechanism (stable per object, dicts work) is correct.
- **Embedding API edges** (F1): host-function kwargs are rejected
  with `TypeError: argument num/types mismatch` — which is the
  vendor's own behaviour (`argcheck.zig:43`, G2 P10, pinned by
  test); a `__getitem__`/`__setitem__` on user instances through
  the API works (G2 P11, pinned by test); `ppy_str`/`ppy_repr`
  run a FunBc `__str__`/`__repr__` through the VM hook (G4/H10,
  `tests/embed/api_h10_test.c` — 8 checks); a raising dunder
  falls back to `<B object at …>` with `PPY_OK` (the reference
  aborts, rc=134 — not reproduced); non-UTF-8 exec
  filenames are lossy; NULL arguments are handled defensively
  where the Zig would fault; nested `ppy_*` calls from a host
  function re-enter through the raw interp pointer
  (lock-serialized, same guarantee the Zig relies on).
- **Typing edges** (F5a): `|=` on types does not take the union
  branch (the vendor checks plain `OR` only — probed identical);
  `real_load_maybe` lacks the `__class__`/`__next__` special cases
  (no caller needs them).
- **E1/E2 leftovers**: a dict-subclass `__missing__` reached
  through ctx-only seams yields a bare `TypeError` instead of
  `KeyError` — the VM path (`try_instance_subscr`) was fixed by
  G3/H4; no gate coverage for the slot path; the `petitpy-vm`
  missing-file error prints only the first line
  (`error: FileNotFound`) — the reference's Zig stack trace after
  it is runtime noise, not reproduced. (`type(iter(s))` was a bare
  `TypeError` — fixed by G4/H8. `from pkg import sub` with a FunBc
  `__import__` override failed with a bare `TypeError` — fixed by
  G4/H9.)
- **The original's own crashes are not mirrored**: where the
  reference aborts (rc=134, e.g. `str.center` with negative width,
  `(a*)*` in `ure`, `sort(key=len)` on ints), Rust raises or
  returns instead. Where the reference is broken at runtime
  (`complex(...)` calls abort), Rust implements the vendor C code
  properly.
