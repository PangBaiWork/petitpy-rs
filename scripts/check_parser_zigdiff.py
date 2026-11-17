#!/usr/bin/env python3
"""Zig-vs-Rust parse-tree diff: the strong form of the A3 check.

Usage: python3 scripts/check_parser_zigdiff.py [--corpus DIR]
        [--workdir DIR]

Builds a minimal Zig program in a scratch dir (default
/tmp/zig_parsedump) that links the ORIGINAL parser.zig + lexer.zig +
qstr.zig + grammar_data.zig + grammar_ext.zig + bigint.zig + mpy.zig
(copied read-only from petitpy-orig) and dumps parse trees in the
exact wire format of `ParseTree::dump_wire` (see
src/compiler/parser/nodes.rs). Then runs both the Zig dump and the
Rust `parse_dump` example over every *.py file in the corpus and
diffs the streams byte-for-byte.

A byte-identical stream proves the Rust parser matches the Zig
parser — same tree shape, same rule ids, same source lines, same
constant folding (small ints, big ints, const tuples, logical
short-circuit) — not just that both accept the file.

On parse error both sides print `PARSE_ERROR <kind>` (kind =
SyntaxError / IndentationError / DedentMismatch / OutOfMemory) and
exit 1; the error line is compared as part of the stream, and the
exit codes must agree too.

Exit 0 and print PASS only if every file's streams (and exit codes)
match; otherwise print the first divergence per file (with context)
and exit 1. Exit 2 on infrastructure failure (Zig build failed,
binaries missing).
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
const parser = @import("parser.zig");
const lexer = @import("lexer.zig");
const qstr_mod = @import("qstr.zig");
const grammar = @import("grammar_data.zig");

const HEX = "0123456789abcdef";

fn writeHex(out: *std.ArrayList(u8), alloc: std.mem.Allocator, bytes: []const u8) !void {
    for (bytes) |b| {
        try out.append(alloc, HEX[b >> 4]);
        try out.append(alloc, HEX[b & 0x0f]);
    }
}

/// Mirror of Rust `dump_wire_pn` (src/compiler/parser/nodes.rs):
/// one line per node, preorder, 2-space indent per depth.
fn dumpPn(out: *std.ArrayList(u8), alloc: std.mem.Allocator, pn: parser.Pn, depth: usize) !void {
    for (0..depth) |_| try out.appendSlice(alloc, "  ");
    if (parser.pnIsNull(pn)) {
        try out.appendSlice(alloc, "null\n");
        return;
    }
    if (parser.pnIsSmallInt(pn)) {
        const line = try std.fmt.allocPrint(alloc, "small_int {d}\n", .{parser.pnLeafSmallInt(pn)});
        defer alloc.free(line);
        try out.appendSlice(alloc, line);
        return;
    }
    if (parser.pnIsLeaf(pn)) {
        const kind = parser.pnLeafKind(pn);
        const arg = parser.pnLeafArg(pn);
        if (kind == parser.PN_ID) {
            const line = try std.fmt.allocPrint(alloc, "id {x}\n", .{arg});
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
        } else if (kind == parser.PN_STRING) {
            const line = try std.fmt.allocPrint(alloc, "string {x}\n", .{arg});
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
        } else if (kind == parser.PN_TOKEN) {
            const line = try std.fmt.allocPrint(alloc, "token {d}\n", .{arg});
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
        } else {
            const line = try std.fmt.allocPrint(alloc, "leaf {x} {x}\n", .{ kind, arg });
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
        }
        return;
    }
    // Struct node.
    const pns: *parser.PnStruct = @ptrFromInt(pn);
    const nodes = parser.pnsNodes(pns);
    if (parser.pnsKind(pns) == grammar.RULE_const_object) {
        const kind: u32 = @intCast(nodes[0]);
        const len: usize = nodes[1];
        if (kind == @intFromEnum(parser.ConstObjectKind.tuple)) {
            const line = try std.fmt.allocPrint(alloc, "const_object {d} {d}\n", .{ kind, len });
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
            const items: [*]parser.Pn = @ptrFromInt(nodes[2]);
            for (0..len) |i| try dumpPn(out, alloc, items[i], depth + 1);
        } else {
            const line = try std.fmt.allocPrint(alloc, "const_object {d} {d} ", .{ kind, len });
            defer alloc.free(line);
            try out.appendSlice(alloc, line);
            if (len > 0) {
                const buf: [*]const u8 = @ptrFromInt(nodes[2]);
                try writeHex(out, alloc, buf[0..len]);
            }
            try out.append(alloc, '\n');
        }
        return;
    }
    const line = try std.fmt.allocPrint(alloc, "rule {d} {d}\n", .{ parser.pnsKind(pns), pns.source_line });
    defer alloc.free(line);
    try out.appendSlice(alloc, line);
    for (0..parser.pnsNumNodes(pns)) |i| try dumpPn(out, alloc, nodes[i], depth + 1);
}

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

    var out = std.ArrayList(u8).empty;
    defer out.deinit(gpa);

    var qctx = qstr_mod.QstrContext.init(gpa);
    defer qctx.deinit();
    const lex = lexer.lexerNewFromStr(gpa, 0, src, true) catch |e| {
        // Same mapping as Rust ParseError::from for LexError.
        const name: []const u8 = switch (e) {
            error.OutOfMemory => "OutOfMemory",
            error.UnicodeNameEscape => "SyntaxError",
        };
        const line = try std.fmt.allocPrint(gpa, "PARSE_ERROR {s}\n", .{name});
        defer gpa.free(line);
        try out.appendSlice(gpa, line);
        try std.Io.File.stdout().writeStreamingAll(io, out.items);
        std.process.exit(1);
    };
    defer lexer.lexerFree(lex);
    var tree = parser.parse(gpa, lex, &qctx, .file_input) catch |e| {
        const line = try std.fmt.allocPrint(gpa, "PARSE_ERROR {s}\n", .{@errorName(e)});
        defer gpa.free(line);
        try out.appendSlice(gpa, line);
        try std.Io.File.stdout().writeStreamingAll(io, out.items);
        std.process.exit(1);
    };
    defer tree.deinit();

    try dumpPn(&out, gpa, tree.root, 0);
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

    const grammar_data_mod = b.createModule(.{
        .root_source_file = b.path("grammar_data.zig"),
        .target = target,
        .optimize = optimize,
    });
    const bigint_mod = b.createModule(.{
        .root_source_file = b.path("bigint.zig"),
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

    const grammar_ext_mod = b.createModule(.{
        .root_source_file = b.path("grammar_ext.zig"),
        .target = target,
        .optimize = optimize,
    });
    grammar_ext_mod.addImport("grammar_data.zig", grammar_data_mod);
    grammar_ext_mod.addImport("lexer.zig", lexer_mod);

    const parser_mod = b.createModule(.{
        .root_source_file = b.path("parser.zig"),
        .target = target,
        .optimize = optimize,
    });
    parser_mod.addImport("lexer.zig", lexer_mod);
    parser_mod.addImport("grammar_ext.zig", grammar_ext_mod);
    parser_mod.addImport("qstr.zig", qstr_mod);
    parser_mod.addImport("grammar_data.zig", grammar_data_mod);
    parser_mod.addImport("bigint.zig", bigint_mod);
    parser_mod.addImport("build_options", build_options_mod);

    const exe_mod = b.createModule(.{
        .root_source_file = b.path("dump.zig"),
        .target = target,
        .optimize = optimize,
    });
    exe_mod.addImport("parser.zig", parser_mod);
    exe_mod.addImport("lexer.zig", lexer_mod);
    exe_mod.addImport("qstr.zig", qstr_mod);
    exe_mod.addImport("grammar_data.zig", grammar_data_mod);
    const exe = b.addExecutable(.{ .name = "zig_parsedump", .root_module = exe_mod });
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
    ap.add_argument("--workdir", default="/tmp/zig_parsedump")
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
    for name in ["parser.zig", "lexer.zig", "qstr.zig", "grammar_data.zig",
                 "grammar_ext.zig", "bigint.zig", "mpy.zig"]:
        shutil.copy2(ORIG / "src" / "compiler" / name, work / name)
    shutil.copy2(HEADER, work / "qstrdefs.generated.h")
    (work / "build_options.zig").write_text(
        "pub const str_unicode: bool = true;\npub const is_full: bool = true;\n")
    (work / "dump.zig").write_text(DUMP_ZIG)
    (work / "build.zig").write_text(BUILD_ZIG)

    t0 = time.time()
    rc, out, err = run([str(ZIG), "build"], cwd=work, timeout=300)
    if rc != 0:
        print("ERROR (infra): zig build of the dump program failed")
        print(err.decode(errors="replace")[-3000:])
        return 2
    zig_dump = work / "zig-out" / "bin" / "zig_parsedump"
    if not zig_dump.is_file():
        print("ERROR (infra): zig_parsedump binary not found after build")
        return 2

    b = subprocess.run(
        [cargo_bin(), "build", "--release", "--example", "parse_dump"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if b.returncode != 0:
        print("ERROR (infra): cargo build --example parse_dump failed")
        print(b.stderr[-2000:])
        return 2
    rs_dump = ROOT / "target" / "release" / "examples" / "parse_dump"

    files = sorted(corpus.rglob("*.py"))
    if not files:
        print(f"ERROR (infra): no .py files under {corpus}")
        return 2

    n_pass = n_fail = 0
    fails = []
    for path in files:
        rc_z, out_z, _ = run([str(zig_dump), str(path)])
        rc_r, out_r, _ = run([str(rs_dump), str(path)])
        if rc_z == "timeout" or rc_r == "timeout":
            n_fail += 1
            fails.append((path.name, "timeout", b"", b""))
            continue
        # Both sides exit 0 on success, 1 on parse error; the streams
        # carry the verdict (tree dump or PARSE_ERROR line).
        if out_z == out_r and rc_z == rc_r:
            n_pass += 1
        else:
            n_fail += 1
            div = first_divergence(out_z, out_r)
            fails.append((path.name, (div, rc_z, rc_r), out_z, out_r))

    n = len(files)
    print(f"zig-vs-rust parse trees: {n_pass}/{n} identical in {time.time() - t0:.1f}s")
    if n_fail == 0:
        print("PASS: every corpus file's parse tree is byte-identical")
        return 0
    print(f"FAIL: {n_fail} files diverge:")
    for name, div, _, _ in fails[:10]:
        if div == "timeout":
            print(f"  {name}: TIMEOUT")
        else:
            d, rc_z, rc_r = div
            print(f"  {name}: exit zig={rc_z} rust={rc_r}, divergence at dump line {d[0] if d else '?'}")
            if d:
                print(f"    zig  | {d[1].decode(errors='replace')[:200]}")
                print(f"    rust | {d[2].decode(errors='replace')[:200]}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
