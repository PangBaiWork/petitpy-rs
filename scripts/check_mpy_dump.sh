#!/bin/bash
# Reader-agreement check: the Zig reader (scripts/mpy_dump.zig) and the
# Rust reader (examples/mpy-dump.rs) must print byte-identical canonical
# dumps for the same .mpy files. Covers all 23 build-frozen/*.mpy plus
# any .mpy files passed as extra arguments.
#
# Usage: scripts/check_mpy_dump.sh [extra.mpy ...]
# Exit 0 iff every file's dumps agree. Requires Zig 0.16 on PATH and a
# release build of the Rust example (built here if missing).
set -u
ORIG="${PETITPY_ORIG:-$HOME/workspace/petitpy-orig}"
ZIG="${ZIG:-$HOME/zig-x86_64-linux-0.16.0/zig}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

if [ ! -x "$ZIG" ]; then echo "zig not found at $ZIG (set ZIG)"; exit 2; fi

echo "== building Zig dump tool =="
"$ZIG" build-exe --dep mpy -Mroot="$ROOT/scripts/mpy_dump.zig" \
  -Mmpy="$ORIG/src/compiler/mpy.zig" \
  -femit-bin=/tmp/mpy_dump || exit 2

echo "== building Rust dump example =="
cargo build --release --example mpy-dump --manifest-path "$ROOT/Cargo.toml" \
  --quiet || exit 2
RUST_DUMP="$ROOT/target/release/examples/mpy-dump"

fail=0
check_one() {
  f="$1"
  if ! /tmp/mpy_dump "$f" > /tmp/dump_zig.txt 2>/tmp/dump_zig.err; then
    echo "FAIL (zig dump rc=$?): $f"; cat /tmp/dump_zig.err; fail=1; return
  fi
  if ! "$RUST_DUMP" "$f" > /tmp/dump_rs.txt 2>/tmp/dump_rs.err; then
    echo "FAIL (rust dump rc=$?): $f"; cat /tmp/dump_rs.err; fail=1; return
  fi
  if diff -q /tmp/dump_zig.txt /tmp/dump_rs.txt > /dev/null; then
    echo "OK dump-agree: $f"
  else
    echo "FAIL dump-diff: $f"
    diff /tmp/dump_zig.txt /tmp/dump_rs.txt | head -20
    fail=1
  fi
}

for f in "$ORIG"/build-frozen/*.mpy; do check_one "$f"; done
for f in "$@"; do check_one "$f"; done
[ "$fail" = 0 ] && echo "ALL DUMPS AGREE" || echo "DUMP CHECK FAILED"
exit "$fail"
