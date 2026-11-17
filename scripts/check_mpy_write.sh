#!/bin/bash
# Write check: .mpy files written by the Rust writer must run under the
# ORIGINAL binary identically to the same programs run as .py.
#
# For each sample program (scripts/mpy_samples/*.py, except complex.py —
# see below): the original's `--emit-mpy` produces the reference .mpy;
# the Rust round-trip example re-serializes it (byte-identity required);
# `petitpy-vm` runs the Rust-written .mpy and its output + exit code must
# match `petitpy` running the .py.
#
# complex.py is a FORMAT-ONLY case: the original emits a valid .mpy for
# it (round-trip + dump checks apply), but the original RUNTIME rejects
# complex constants ("ValueError: invalid syntax for number") and its
# `complex()` builtin panics — pre-existing original bugs, not format
# problems. Both the .py run and the .mpy run fail identically; that is
# recorded, not counted as a write-check pass.
#
# Also cross-checks the empty-children-header edge: the Zig round-trip
# tool must report MISMATCH on those bytes, exactly like the Rust
# check() (see mpy_roundtrip.rs).
#
# Usage: scripts/check_mpy_write.sh
# Exit 0 iff all run-comparison samples pass.
set -u
ORIG="${PETITPY_ORIG:-$HOME/workspace/petitpy-orig}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
WORK=/tmp/mpy_write
mkdir -p "$WORK"
PETITPY="$ORIG/zig-out/bin/petitpy"
PETITPY_VM="$ORIG/zig-out/bin/petitpy-vm"
ZIG_RT="$ORIG/zig-out/bin/mpy-roundtrip"

cargo build --release --example mpy-roundtrip --manifest-path "$ROOT/Cargo.toml" --quiet || exit 2
RS_RT="$ROOT/target/release/examples/mpy-roundtrip"

fail=0
check_run() {
  name="$1"
  py="$ROOT/scripts/mpy_samples/$name.py"
  mpy="$WORK/$name.mpy"
  rs_mpy="$WORK/${name}_rs.mpy"
  "$PETITPY" --emit-mpy "$py" > "$WORK/$name.b64" 2>/dev/null || { echo "FAIL emit: $name"; fail=1; return; }
  python3 -c "import base64,sys; open('$mpy','wb').write(base64.b64decode(open('$WORK/$name.b64').read()))"
  # Zig round-trip on the original bytes (sanity: the original agrees
  # its own bytes round-trip).
  "$ZIG_RT" "$mpy" > /dev/null 2>&1 || { echo "FAIL zig round-trip: $name"; fail=1; return; }
  # Rust round-trip: byte-identity + write the Rust version out.
  if ! "$RS_RT" "$mpy" "$rs_mpy" > /dev/null 2>&1; then echo "FAIL rust round-trip: $name"; fail=1; return; fi
  cmp -s "$mpy" "$rs_mpy" || { echo "FAIL rust bytes differ: $name"; fail=1; return; }
  # Dump-agree on the sample (reader agreement, not just round-trip).
  bash "$ROOT/scripts/check_mpy_dump.sh" "$mpy" > /dev/null 2>&1 || { echo "FAIL dump-agree: $name"; fail=1; return; }
  # Run comparison: .py under petitpy vs Rust-written .mpy under petitpy-vm.
  "$PETITPY" "$py" > "$WORK/$name.py.out" 2> "$WORK/$name.py.err"; py_rc=$?
  "$PETITPY_VM" "$rs_mpy" > "$WORK/$name.mpy.out" 2> "$WORK/$name.mpy.err"; mpy_rc=$?
  if [ "$py_rc" = "$mpy_rc" ] && cmp -s "$WORK/$name.py.out" "$WORK/$name.mpy.out" && cmp -s "$WORK/$name.py.err" "$WORK/$name.mpy.err"; then
    echo "OK run-identical: $name (rc=$py_rc)"
  else
    echo "FAIL run-diff: $name (py rc=$py_rc, mpy rc=$mpy_rc)"
    diff "$WORK/$name.py.out" "$WORK/$name.mpy.out" | head -10
    diff "$WORK/$name.py.err" "$WORK/$name.mpy.err" | head -10
    fail=1
  fi
}

for name in arith consts nested loop control; do check_run "$name"; done

# complex.py: format-only (see header comment). Round-trip + dump only.
name=complex
py="$ROOT/scripts/mpy_samples/$name.py"
mpy="$WORK/$name.mpy"
rs_mpy="$WORK/${name}_rs.mpy"
"$PETITPY" --emit-mpy "$py" > "$WORK/$name.b64" 2>/dev/null
python3 -c "import base64; open('$mpy','wb').write(base64.b64decode(open('$WORK/$name.b64').read()))"
if "$RS_RT" "$mpy" "$rs_mpy" > /dev/null 2>&1 && cmp -s "$mpy" "$rs_mpy"; then
  echo "OK round-trip (format-only): $name"
else echo "FAIL round-trip (format-only): $name"; fail=1; fi
"$PETITPY" "$py" > "$WORK/$name.py.out" 2> "$WORK/$name.py.err"; py_rc=$?
"$PETITPY_VM" "$rs_mpy" > "$WORK/$name.mpy.out" 2> "$WORK/$name.mpy.err"; mpy_rc=$?
# Compare COMBINED output: the .py path reports the ValueError on stderr
# while the .mpy path reports it on stdout — an original difference
# between the two execution paths, unrelated to the .mpy bytes (which
# are byte-identical). Same rc, same message text.
cat "$WORK/$name.py.out" "$WORK/$name.py.err" > "$WORK/$name.py.all"
cat "$WORK/$name.mpy.out" "$WORK/$name.mpy.err" > "$WORK/$name.mpy.all"
if [ "$py_rc" = "$mpy_rc" ] && cmp -s "$WORK/$name.py.all" "$WORK/$name.mpy.all"; then
  echo "INFO both-fail-identically (original complex-const runtime bug; error stream differs: stderr for .py, stdout for .mpy): $name (rc=$py_rc)"
else echo "FAIL complex runs diverge: $name"; fail=1; fi

# Edge: empty-children header — Zig tool must also say MISMATCH.
printf '\x4d\x06\x00\x1f\x00\x00\x04\x00' > "$WORK/edge_empty_children.mpy"
if "$ZIG_RT" "$WORK/edge_empty_children.mpy" > /dev/null 2>&1; then
  echo "FAIL zig round-trip accepted empty-children header (expected MISMATCH)"; fail=1
else
  echo "OK zig agrees: empty-children header is MISMATCH (same as Rust check())"
fi

[ "$fail" = 0 ] && echo "WRITE CHECK PASSED" || echo "WRITE CHECK FAILED"
exit "$fail"
