// Dump a .mpy file's parsed structure in a canonical, diffable format.
//
// Zig side of the reader-agreement check: prints exactly the same format
// as the Rust `examples/mpy-dump.rs` (see that file for the format spec).
// Used by scripts/check_mpy_dump.sh.
//
// Build (Zig 0.16):
//   zig build-exe --dep mpy -Mroot=scripts/mpy_dump.zig \
//       -Mmpy=<petitpy-orig>/src/compiler/mpy.zig \
//       -femit-bin=/tmp/mpy_dump
// Usage: /tmp/mpy_dump <input.mpy>

const std = @import("std");
const mpy = @import("mpy");

const hex_digits = "0123456789abcdef";

fn writeHex(w: anytype, bytes: []const u8) !void {
    for (bytes) |b| {
        try w.print("{c}{c}", .{ hex_digits[b >> 4], hex_digits[b & 0x0f] });
    }
}

fn dumpObj(w: anytype, o: mpy.ObjEntry) !void {
    switch (o) {
        .none => try w.writeAll("none"),
        .false => try w.writeAll("false"),
        .true => try w.writeAll("true"),
        .ellipsis => try w.writeAll("ellipsis"),
        .str => |s| {
            try w.writeAll("str:");
            try writeHex(w, s);
        },
        .bytes => |b| {
            try w.writeAll("bytes:");
            try writeHex(w, b);
        },
        .int => |s| {
            try w.writeAll("int:");
            try writeHex(w, s);
        },
        .float => |s| {
            try w.writeAll("float:");
            try writeHex(w, s);
        },
        .complex => |s| {
            try w.writeAll("complex:");
            try writeHex(w, s);
        },
        .tuple => |t| {
            try w.writeAll("tuple:[");
            for (t, 0..) |it, i| {
                if (i > 0) try w.writeAll(",");
                try dumpObj(w, it);
            }
            try w.writeAll("]");
        },
    }
}

fn dumpRc(w: anytype, rc: mpy.RawCode, path: []const u8, gpa: std.mem.Allocator) !void {
    try w.print("{s} kind={d} fun_data_len={d} fun_data=", .{ path, rc.kind, rc.fun_data.len });
    try writeHex(w, rc.fun_data);
    try w.print(" children={d}\n", .{rc.children.len});
    for (rc.children, 0..) |child, i| {
        const child_path = try std.fmt.allocPrint(gpa, "{s}/{d}", .{ path, i });
        defer gpa.free(child_path);
        try dumpRc(w, child, child_path, gpa);
    }
}

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = std.heap.page_allocator;
    const argv = try init.minimal.args.toSlice(init.arena.allocator());
    if (argv.len < 2) {
        std.debug.print("usage: mpy-dump <input.mpy>\n", .{});
        std.process.exit(2);
    }
    const data = std.Io.Dir.cwd().readFileAlloc(io, argv[1], gpa, .unlimited) catch {
        std.debug.print("fopen failed\n", .{});
        std.process.exit(2);
    };
    defer gpa.free(data);
    const m = mpy.read(gpa, data) catch |err| {
        std.debug.print("mpy.read failed: {}\n", .{err});
        std.process.exit(2);
    };
    defer m.deinit(gpa);

    var aw = std.Io.Writer.Allocating.init(gpa);
    defer aw.deinit();
    const w = &aw.writer;
    if (m.arch_flags) |af| {
        try w.print("header version={d} feature_flags={d} small_int_bits={d} arch_flags={d}\n", .{ m.version, m.feature_flags, m.small_int_bits, af });
    } else {
        try w.print("header version={d} feature_flags={d} small_int_bits={d} arch_flags=none\n", .{ m.version, m.feature_flags, m.small_int_bits });
    }
    try w.print("n_qstr={d} n_obj={d}\n", .{ m.qstrs.len, m.objs.len });
    for (m.qstrs, 0..) |q, i| {
        switch (q) {
            .static => |id| try w.print("q[{d}]=S:{d}\n", .{ i, id }),
            .new => |b| {
                try w.print("q[{d}]=N:", .{i});
                try writeHex(w, b);
                try w.writeAll("\n");
            },
        }
    }
    for (m.objs, 0..) |o, i| {
        try w.print("o[{d}]=", .{i});
        try dumpObj(w, o);
        try w.writeAll("\n");
    }
    try dumpRc(w, m.rc, "rc", gpa);
    try std.Io.File.stdout().writeStreamingAll(io, aw.written());
}
