#!/bin/sh
# Cross linker for aarch64-unknown-linux-gnu: Zig's bundled clang/lld
# with the aarch64 glibc target. Used as cargo/rustc `linker` for the
# aarch64 target (see .cargo/config.toml); no aarch64-linux-gnu-gcc
# is installed in this VM.
#
# Zig must be installed (0.16.0 or newer): `command -v zig` wins;
# otherwise fall back to the path where this VM keeps it.
# rustc adds -Wl,--fix-cortex-a53-843419 (a GNU-ld erratum workaround);
# zig/lld does not accept it, so strip it — lld handles the erratum
# itself.
if command -v zig >/dev/null 2>&1; then
    ZIG=zig
elif [ -x /home/hatch/zig-x86_64-linux-0.16.0/zig ]; then
    ZIG=/home/hatch/zig-x86_64-linux-0.16.0/zig
else
    echo "zig-cc-aarch64.sh: zig not found (need Zig 0.16.0+ on PATH)" >&2
    exit 1
fi
args=""
for a in "$@"; do
    case "$a" in
        -Wl,--fix-cortex-a53-*) ;;
        -Wl,-O1) ;;  # GNU-ld opt hint; lld only warns about it
        *) args="$args \"$a\"";;
    esac
done
eval "exec $ZIG cc -target aarch64-linux-gnu $args"
