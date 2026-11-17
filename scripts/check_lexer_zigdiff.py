#!/usr/bin/env python3
"""Zig-vs-Rust token-stream diff: the strong form of the C3 check.

Usage: python3 scripts/check_lexer_zigdiff.py [--corpus DIR]
        [--workdir DIR]

Builds a minimal Zig program in a scratch dir (default /tmp/zig_lexdump)
that links the ORIGINAL lexer.zig + qstr.zig + mpy.zig (copied read-only
from petitpy-orig) and dumps tokens in the exact wire format of
lexer_test.zig's dumpZig. Then runs both the Zig dump and the Rust
`lex_dump` example over every *.py file in the corpus and diffs the
streams byte-for-byte.

This is the check the upstream project defers (its vendor `token_dump`
C harness was never built — build.zig:1022-1026); here the Zig side
itself is the oracle, so a byte-identical stream proves the Rust
lexer matches the Zig lexer, not just that both accept the file.

Exit 0 and print PASS only if every file's streams match; otherwise
print the first divergence per file (with context) and exit 1.
Exit 2 on infrastructure failure (Zig build failed, binaries missing).
"""

import argparse
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"
ZIG = Path("/home/hatch/zig-x86_64-linux-0.16.0/zig")
HEADER = ORIG / "vendor" / "micropython_embed" / "genhdr" / "qstrdefs.generated.h"
TIMEOUT_S = 10

DUMP_ZIG = r'''const std = @import("std");
const lexer = @import("lexer.zig");

const HEX = "0123456789abcdef";

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;
    const argv = try init.minimal.args.toSlice(init.arena.allocator());
    if (argv.len < 2) std.process.exit(2);
    const src = std.Io.Dir.cwd().readFileAllocOptions(
        io,
        argv[1],
        gpa,
        .unlimited,
        .of(u8),
        0,
    ) catch std.process.exit(2);
    defer gpa.free(src);
    const lex = lexer.lexerNewFromStr(gpa, 0, src, true) catch std.process.exit(1);
    defer lexer.lexerFree(lex);

    var out = std.ArrayList(u8).empty;
    defer out.deinit(gpa);

    var idx: usize = 0;
    while (true) : (idx += 1) {
        const line = try std.fmt.allocPrint(gpa, "{d}\t{d}\t{d}\t{d}\t{d}\t", .{
            idx,
            @intFromEnum(lex.tok_kind),
            lex.tok_line,
            lex.tok_column,
            lex.vstr.items.len,
        });
        defer gpa.free(line);
        try out.appendSlice(gpa, line);
        for (lex.vstr.items) |b| {
            try out.append(gpa, HEX[b >> 4]);
            try out.append(gpa, HEX[b & 0x0f]);
        }
        try out.append(gpa, '\n');
        const kind = lex.tok_kind;
        if (kind == .END or kind == .INVALID) break;
        lexer.lexerToNext(lex) catch std.process.exit(1);
    }
    try std.Io.File.stdout().writeStreamingAll(io, out.items);
}
'''

BUILD_ZIG = """const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const mpy_mod = b.createModule(.{
        .root_source_file = b.path("mpy.zig"),
        .target = target,
        .optimize = optimize,
    });
    const qstr_mod = b.createModule(.{
        .root_source_file = b.path("qstr.zig"),
        .target = target,
        .optimize = optimize,
    });
    qstr_mod.addAnonymousImport("qstrdefs_h", .{
        .root_source_file = b.path("qstrdefs.generated.h"),
    });
    qstr_mod.addImport("mpy.zig", mpy_mod);

    const build_options_mod = b.createModule(.{
        .root_source_file = b.path("build_options.zig"),
        .target = target,
        .optimize = optimize,
    });

    const lexer_mod = b.createModule(.{
        .root_source_file = b.path("lexer.zig"),
        .target = target,
        .optimize = optimize,
    });
    lexer_mod.addImport("qstr.zig", qstr_mod);
    lexer_mod.addImport("build_options", build_options_mod);

    const exe_mod = b.createModule(.{
        .root_source_file = b.path("dump.zig"),
        .target = target,
        .optimize = optimize,
    });
    exe_mod.addImport("lexer.zig", lexer_mod);
    const exe = b.addExecutable(.{ .name = "zig_lexdump", .root_module = exe_mod });
    b.installArtifact(exe);
}
"""


def cargo_bin() -> str:
    """cargo, resolved via ~/.cargo/bin first (rustup installs there)."""
    p = Path.home() / ".cargo" / "bin" / "cargo"
    return str(p) if p.is_file() else "cargo"


def run(cmd, cwd=None, timeout=TIMEOUT_S):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""


def first_divergence(a: bytes, b: bytes):
    """Return (line_no, a_line, b_line) of the first differing line."""
    al = a.split(b"\n")
    bl = b.split(b"\n")
    for i, (x, y) in enumerate(zip(al, bl)):
        if x != y:
            return i, x, y
    if len(al) != len(bl):
        i = min(len(al), len(bl))
        return i, al[i] if i < len(al) else b"<eof>", bl[i] if i < len(bl) else b"<eof>"
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=str(ORIG / "corpus" / "basics"))
    ap.add_argument("--workdir", default="/tmp/zig_lexdump")
    args = ap.parse_args()
    corpus = Path(args.corpus)
    work = Path(args.workdir)

    if not corpus.is_dir():
        print(f"ERROR (infra): corpus dir not found: {corpus}")
        return 2
    if not ZIG.is_file():
        print(f"ERROR (infra): zig not found at {ZIG}")
        return 2
    if not HEADER.is_file():
        print(f"ERROR (infra): generated header not found: {HEADER}")
        return 2

    # ── scratch Zig project (copies of the originals, read-only) ──
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    for name in ["lexer.zig", "qstr.zig", "mpy.zig"]:
        shutil.copy2(ORIG / "src" / "compiler" / name, work / name)
    shutil.copy2(HEADER, work / "qstrdefs.generated.h")
    (work / "build_options.zig").write_text("pub const str_unicode: bool = true;\n")
    (work / "dump.zig").write_text(DUMP_ZIG)
    (work / "build.zig").write_text(BUILD_ZIG)

    t0 = time.time()
    rc, out, err = run([str(ZIG), "build"], cwd=work, timeout=300)
    if rc != 0:
        print("ERROR (infra): zig build of the dump program failed")
        print(err.decode(errors="replace")[-3000:])
        return 2
    zig_dump = work / "zig-out" / "bin" / "zig_lexdump"
    if not zig_dump.is_file():
        print("ERROR (infra): zig_lexdump binary not found after build")
        return 2

    b = subprocess.run(
        [cargo_bin(), "build", "--release", "--example", "lex_dump"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if b.returncode != 0:
        print("ERROR (infra): cargo build --example lex_dump failed")
        print(b.stderr[-2000:])
        return 2
    rs_dump = ROOT / "target" / "release" / "examples" / "lex_dump"

    files = sorted(corpus.rglob("*.py"))
    if not files:
        print(f"ERROR (infra): no .py files under {corpus}")
        return 2

    n_pass = n_fail = 0
    fails = []
    for path in files:
        rc_z, out_z, _ = run([str(zig_dump), str(path)])
        rc_r, out_r, _ = run([str(rs_dump), str(path)])
        # Streams are the verdict; exit codes differ by design
        # (Rust exits 1 when an error-kind token appears, Zig exits 0
        # after dumping it — both print the same stream).
        if rc_z == "timeout" or rc_r == "timeout":
            n_fail += 1
            fails.append((path.name, "timeout", b"", b""))
            continue
        if out_z == out_r:
            n_pass += 1
        else:
            n_fail += 1
            div = first_divergence(out_z, out_r)
            fails.append((path.name, div, out_z, out_r))

    n = len(files)
    print(f"zig-vs-rust token streams: {n_pass}/{n} identical in {time.time() - t0:.1f}s")
    if n_fail == 0:
        print("PASS: every corpus file's token stream is byte-identical")
        return 0
    print(f"FAIL: {n_fail} files diverge:")
    for name, div, _, _ in fails[:10]:
        if div == "timeout":
            print(f"  {name}: TIMEOUT")
        else:
            i, a, b = div
            print(f"  {name}: divergence at dump line {i}")
            print(f"    zig  | {a.decode(errors='replace')[:200]}")
            print(f"    rust | {b.decode(errors='replace')[:200]}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
