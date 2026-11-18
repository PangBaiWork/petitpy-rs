# Lane D2 — os / sys / io backends (modos + file_io + modsys) — BRIEF

## Scope
Port `src/modules/modos.zig` (766) + `src/modules/file_io.zig` (538) +
`src/modules/modsys.zig` (517) = 1,821 lines into Rust: the `os` module,
the `sys` module, and the FILE-backed stream types (`FileIO`,
`TextIOWrapper`) behind them. Pure Rust std — NO libc externs
(`file_io.zig` uses extern C `FILE*`/`fopen`/`fread`; the Rust port uses
`std::fs::File` + `std::io`, wired to C16's `StreamP` protocol).

## Grounding (read these before writing code)
- Originals (all read-only in `~/workspace/petitpy-orig/src/modules/`):
  - `modos.zig`: globals table `mp_module_os_globals_table` at **:636–706**.
    Orientation only: `__getattr__ _environ access altsep chdir chmod chown
    close curdir extsep fsync getcwd getenv getpid kill linesep listdir
    lseek lstat mkdir name open pardir putenv read readlink remove rename
    rmdir sep stat symlink umask unlink unsetenv urandom write`.
    NOTE modos.zig:209 — `environ` is imported on FIRST ACCESS (the ROM
    globals dict cannot hold it eagerly). Port that laziness.
  - `modsys.zig`: globals table `mp_module_sys_globals_table` at
    **:418–488**. Orientation only: `argv byteorder exc_info exit
    getrecursionlimit intern modules platform print_exception
    setrecursionlimit stderr stdin stdout version version_info`
    (+ `maxsize` — verify in the blk; it may be added conditionally).
    The file header documents the nano/full config gates — read it.
  - `file_io.zig`: header comments :16–20 name the types
    (`FileIO`, `TextIOWrapper`, `buffer`, `readlines`, `fileno`);
    the rest is extern-C plumbing. Read the type/method shape, ignore
    the C calls.
- Rust current state: C16 `src/support/stream.rs` (StreamP vtable +
  `mp_stream_rw` loop + STREAM_METHODS) is THE read/write loop — do NOT
  write a second one. C18b `src/objects/containers/objstringio*.rs`
  built StringIO/BytesIO/IOBase on it + `build_io_module`; D1 owns
  module registration. `open()` the builtin is D1's name — D2 provides
  the implementation (see Seam below). `sys.stdout` is the object form
  of C8c's print sink (`mp_plat_print` = plain `write(1, ...)`).

## Seam with D1
D1 binds the `open` builtin name; D2 provides the implementation.
Define it as `open_builtin` in `src/runtime/` (or a clearly-named fn in
the file_io module) taking the same (ctx, args) shape D1's `mk_var`
expects, and document the exact name + sig in result §4 so D1 can wire
it without guessing. Do NOT duplicate D1's binding work.

## Handovers to land in D2
1. **`sys.exc_info` / cur_exception slot.** C15b noted the current-handler
   slot has NO Rust home yet (`VmState.pending_exc` is the PENDING slot,
   not the current-handler slot). D2 owns it: add the slot to `VmState`
   (or the equivalent the original uses — read `mpstate.zig` /
   `runtime.zig` for where the original keeps it), set it on exception
   entry, clear it on exit, and expose it as `sys.exc_info()`.
2. **stdin/stdout/stderr stream objects.** `sys.stdin`/`stdout`/`stderr`
   are file_io stream objects over fds 0/1/2. `print(file=...)` (C18b
   not-done) rides this: once the streams exist, `print`'s `file`
   kwarg must accept them.
3. **`sys.version` / `version_info` / `platform` / `argv`.** Values must
   match the original's — probe the reference binary, don't invent.
   `sys.path` already exists (C15b, default `[""]`); Phase E's entry
   overwrites it with the script dir — do NOT change that here.

## Probes (reference binary `~/workspace/petitpy-orig/zig-out/bin/petitpy`)
Probe before porting: `os.getcwd()` / `listdir` / `stat` shapes;
`os.environ` first-access behavior; `open()` read/write/seek round-trip
on a temp file + `TextIOWrapper` text mode; `sys.argv`, `sys.version`,
`sys.version_info`, `sys.platform`, `sys.maxsize`; `sys.exc_info()`
inside `except`; `os.urandom(n)` length; error messages + errno for
missing files (`OSError` shape — C12b's `[Errno 2] ENOENT: x` branch).

## Acceptance
- `cargo test --release --lib --tests`: baseline **1249 + ≥30 new**,
  0 failed; `cargo test --release --bins`: 9 passed; 0 warnings;
  no `unsafe`; every file <800 lines (value.rs 886 is recorded debt —
  DO NOT GROW IT); original repo clean.
- New tests in NEW files (split by module; don't crowd nearly-full
  files). End-to-end Vm tests (`fx.run_ok` style) for: os.getcwd/listdir,
  open/read/write round-trip, sys.argv/version, exc_info in except.
  File tests must use a temp dir and clean up after themselves.
- `scripts/check_compile_mpy.py`: PASS (552/552).
- One commit: "Lane D2: os/sys/file_io — modos/file_io/modsys".
- Result file `scripts/d2_result.txt`: §1 per-function mapping table
  (Zig line numbers + Rust locations); §2 probes; §3 bugs found;
  §4 not-done + handovers (the `open_builtin` seam name/sig for D1,
  `print(file=)` status, anything needing Phase E entry).

## Working rules (agent-rules.md)
对照原文、不许自创、同一套测试、纯 Rust std、文件<800行、无 unsafe、
原项目只读、每块一提交、不许假完成. Work in small steps: one file at a
time, run `cargo test --release --lib --tests` after each file, keep the
final report short. Base: master `c9fc4e6`. Worktree
`~/workspace/petitpy-rs-d2-ossys`, branch `lane/d2-ossys`.
