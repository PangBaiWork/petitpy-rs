#!/bin/sh
# Cross linker for x86_64-pc-windows-gnu: Zig's bundled clang/lld
# with the Windows GNU target. Used as cargo/rustc `linker` for the
# windows-gnu target (see .cargo/config.toml); no MinGW gcc is
# installed in this VM.
#
# Local cross-check only: CI builds the Windows artifact with the
# MSVC target (x86_64-pc-windows-msvc) on windows-latest. The gnu
# target is the compile proxy here — both targets lack
# `std::os::unix`, so a gnu cross-build catches every Unix-only
# use the MSVC build would reject.
#
# Zig must be installed (0.16.0 or newer): `command -v zig` wins;
# otherwise fall back to the path where this VM keeps it.
# rustc passes -lmsvcrt/-lmingwex/-lmingw32/-lgcc/-l:libpthread.a
# with -nodefaultlibs; zig links the mingw CRT itself for
# windows-gnu and fails to resolve those as dynamic libs, so
# strip them.
if command -v zig >/dev/null 2>&1; then
    ZIG=zig
elif [ -x /home/hatch/zig-x86_64-linux-0.16.0/zig ]; then
    ZIG=/home/hatch/zig-x86_64-linux-0.16.0/zig
else
    echo "zig-cc-windows.sh: zig not found (need Zig 0.16.0+ on PATH)" >&2
    exit 1
fi
args=""
for a in "$@"; do
    case "$a" in
        -lmsvcrt|-lmingwex|-lmingw32|-lgcc|-l:libpthread.a) ;;
        # rustc passes the DLL export list as `-Wl,<path>.def`;
        # zig's driver rejects a bare path behind `-Wl,` — pass
        # the .def file as a plain input instead (lld reads it
        # for the export list).
        -Wl,*.def) args="$args \"${a#-Wl,}\"";;
        *) args="$args \"$a\"";;
    esac
done
eval "exec $ZIG cc -target x86_64-windows-gnu $args"
