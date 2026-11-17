#!/bin/sh
# Build the direct C oracle harnesses for mp_format_float.
#
# Both compile the ORIGINAL vendor formatfloat.c (read-only under
# ~/workspace/petitpy-orig) with the SAME compiler and flags as the
# reference build (zig cc, -std=c99 -DNDEBUG -DPETITPY_CONFIG_FULL=1,
# Debug default -O0), plus a main() that reads
# "<bits_hex> <fmt> <prec> <sign> <bufsize>" lines and prints
# "<len> <output>". parsenum.c references runtime symbols its other
# functions need; stubs.c provides aborting stand-ins (never called
# by mp_decimal_exp / mp_parse_float_internal).
#
# Two variants (they differ only in mp_parse_float_internal):
# - harness_zig (pure vendor C parsenum.c): the vendor C behaviour.
#   NOT a valid oracle for petitpy: the reference binary links the
#   Zig parsenum (strtod parse-back) instead, and the two disagree
#   in the last digit on some high-precision cases (e.g. DBL_MAX
#   '%.100f': vendor C gives ...3156, the binary gives ...3158).
# - harness_ziglike (parsenum_ziglike.c): vendor C formatfloat +
#   the Zig parsenum's strtod parse-back, i.e. the LINKED behaviour
#   of the reference binary. This is the oracle the generator uses.
#   It is cross-checked against the binary on every binary-reachable
#   case before use (see scripts/fmt_result.txt).
#
# The gcc-compiled copy of the same C code is NOT a valid oracle
# either: the (u64)(mantissa_f + 0.5) cast in mp_format_float is UB
# in C for subnormal inputs and gcc resolves it to mantissa 0, while
# the reference build's clang emits vcvttsd2usi (mantissa u64::MAX).
set -eu
ZIG="${ZIG:-/home/hatch/zig-x86_64-linux-0.16.0/zig}"
ORIG="${ORIG:-$HOME/workspace/petitpy-orig}"
OUTDIR="${1:-/tmp/charness}"
mkdir -p "$OUTDIR"
HERE="$(dirname "$0")"
CFLAGS="-std=c99 -DNDEBUG -fno-sanitize=undefined -fno-common -DPETITPY_CONFIG_FULL=1"
INCLUDES="-I $ORIG/vendor/micropython -I $ORIG/port -I $ORIG/vendor/micropython_embed"
# shellcheck disable=SC2086
"$ZIG" cc $CFLAGS -c "$ORIG/vendor/micropython/py/formatfloat.c" $INCLUDES \
    -o "$OUTDIR/formatfloat_zig.o"
# shellcheck disable=SC2086
"$ZIG" cc $CFLAGS -c "$ORIG/vendor/micropython/py/parsenum.c" $INCLUDES \
    -o "$OUTDIR/parsenum_zig.o"
# shellcheck disable=SC2086
"$ZIG" cc $CFLAGS -c "$HERE/parsenum_ziglike.c" $INCLUDES \
    -o "$OUTDIR/parsenum_ziglike.o"
# shellcheck disable=SC2086
"$ZIG" cc $CFLAGS -o "$OUTDIR/harness_zig" "$HERE/main.c" "$HERE/stubs.c" \
    "$OUTDIR/formatfloat_zig.o" "$OUTDIR/parsenum_zig.o" $INCLUDES -lm
# shellcheck disable=SC2086
"$ZIG" cc $CFLAGS -o "$OUTDIR/harness_ziglike" "$HERE/main.c" "$HERE/stubs.c" \
    "$OUTDIR/formatfloat_zig.o" "$OUTDIR/parsenum_ziglike.o" $INCLUDES -lm
echo "built $OUTDIR/harness_zig $OUTDIR/harness_ziglike"
