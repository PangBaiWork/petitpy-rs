// build.rs — qstr table codegen.
//
// Mirrors the Zig original's comptime QDEF parser (src/compiler/qstr.zig):
// the qstr table is derived at build time from
// vendor/qstrdefs.generated.h, never hand-copied, so the numbering can
// never drift from the vendor header. The header itself is a verbatim
// copy of petitpy-orig's vendor/micropython_embed/genhdr/qstrdefs.generated.h
// (orig HEAD df7de3a); see vendor/README.md. To update after a vendor
// refresh, copy the fresh header over vendor/qstrdefs.generated.h and
// rebuild — the table follows automatically, and the tests in
// src/compiler/qstr.rs assert the counts and a sample of ids.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

struct QstrDef {
    bytes: Vec<u8>,
    hash: u16,
    len: u8,
}

fn skip_spaces(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    i
}

fn decode_c_string(raw: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        i += 1;
        assert!(i < raw.len(), "trailing backslash in QDEF string");
        match raw[i] {
            b'n' => {
                out.push(b'\n');
                i += 1;
            }
            b't' => {
                out.push(b'\t');
                i += 1;
            }
            b'r' => {
                out.push(b'\r');
                i += 1;
            }
            b'0' => {
                out.push(0);
                i += 1;
            }
            b'\\' => {
                out.push(b'\\');
                i += 1;
            }
            b'"' => {
                out.push(b'"');
                i += 1;
            }
            b'\'' => {
                out.push(b'\'');
                i += 1;
            }
            b'x' => {
                assert!(i + 2 < raw.len(), "short \\x escape in QDEF string");
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).expect("non-ASCII hex");
                let v = u8::from_str_radix(hex, 16).expect("bad \\x escape");
                out.push(v);
                i += 3;
            }
            other => panic!("unhandled C escape \\{}", other as char),
        }
    }
    out
}

/// Parse `QDEFx(MP_QSTR_<name>, <hash>, <len>, "<str>")` — mirrors
/// Zig `parseLine` in src/compiler/qstr.zig. The name is ignored:
/// the qstr id is the file order, same as the vendor's MP_QSTR_* enum.
fn parse_line(line: &str) -> QstrDef {
    let b = line.as_bytes();
    let open = b.iter().position(|&c| c == b'(').expect("bad QDEF line");
    let close = b.iter().rposition(|&c| c == b')').expect("bad QDEF line");
    let body = &b[open + 1..close];

    let mut i = body.iter().position(|&c| c == b',').expect("bad QDEF line") + 1;
    // hash field
    let hash_start = skip_spaces(body, i);
    let hash_end = body[hash_start..]
        .iter()
        .position(|&c| c == b',')
        .expect("bad QDEF line")
        + hash_start;
    let hash: u16 = std::str::from_utf8(&body[hash_start..hash_end])
        .unwrap()
        .trim()
        .parse()
        .expect("bad hash");
    i = hash_end + 1;
    // len field
    let len_start = skip_spaces(body, i);
    let len_end = body[len_start..]
        .iter()
        .position(|&c| c == b',')
        .expect("bad QDEF line")
        + len_start;
    let len_decl: u8 = std::str::from_utf8(&body[len_start..len_end])
        .unwrap()
        .trim()
        .parse()
        .expect("bad len");
    i = len_end + 1;
    // quoted string field
    let str_start = skip_spaces(body, i);
    assert_eq!(body[str_start], b'"', "expected quoted string");
    let str_inner_start = str_start + 1;
    let str_inner_end = body.iter().rposition(|&c| c == b'"').expect("unterminated");
    if str_inner_end <= str_inner_start {
        assert_eq!(len_decl, 0, "empty string with nonzero declared len");
        return QstrDef {
            bytes: Vec::new(),
            hash,
            len: 0,
        };
    }
    let decoded = decode_c_string(&body[str_inner_start..str_inner_end]);
    assert_eq!(
        decoded.len(), len_decl as usize,
        "declared len mismatch in QDEF line: {line}"
    );
    QstrDef {
        bytes: decoded,
        hash,
        len: len_decl,
    }
}

/// Render bytes as a Rust byte-string literal (`b"..."`).
fn rust_byte_str(bytes: &[u8]) -> String {
    let mut s = String::from("b\"");
    for &b in bytes {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7e => s.push(b as char),
            _ => {
                let _ = write!(s, "\\x{b:02x}");
            }
        }
    }
    s.push('"');
    s
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let header = manifest_dir.join("vendor/qstrdefs.generated.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let src = fs::read_to_string(&header).expect("cannot read vendor/qstrdefs.generated.h");

    let mut statics: Vec<QstrDef> = Vec::new();
    let mut mains: Vec<QstrDef> = Vec::new();
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("QDEF0(") {
            let _ = rest;
            statics.push(parse_line(line));
        } else if line.starts_with("QDEF1(") {
            mains.push(parse_line(line));
        }
    }

    let n_static = statics.len();
    let n_main = mains.len();
    let n_total = n_static + n_main;
    // The counts are asserted again in qstr.rs against N_STATIC/N_MAIN;
    // panic here too so a header change fails the build loudly.
    assert_eq!(n_static, 228, "QDEF0 count drift — bump N_STATIC");
    assert_eq!(n_main, 385, "QDEF1 count drift — bump N_MAIN");

    let mut out = String::new();
    out.push_str("// Generated by build.rs from vendor/qstrdefs.generated.h — do not edit.\n");
    out.push_str("// Ids are file order: QDEF0 entries first, then QDEF1 entries,\n");
    out.push_str("// exactly as the vendor's MP_QSTR_* enum assigns them.\n\n");
    let _ = writeln!(out, "pub(crate) const GEN_N_STATIC: usize = {n_static};");
    let _ = writeln!(out, "pub(crate) const GEN_N_MAIN: usize = {n_main};");
    let _ = writeln!(out, "pub(crate) const GEN_N_TOTAL: usize = {n_total};\n");

    out.push_str("pub(crate) static QSTR_STRINGS: &[&[u8]; GEN_N_TOTAL] = &[\n");
    for def in statics.iter().chain(mains.iter()) {
        let _ = writeln!(out, "    {},", rust_byte_str(&def.bytes));
    }
    out.push_str("];\n\n");

    out.push_str("pub(crate) static QSTR_HASHES: &[u16; GEN_N_TOTAL] = &[\n");
    for (i, def) in statics.iter().chain(mains.iter()).enumerate() {
        if i % 16 == 0 {
            out.push_str("    ");
        }
        let _ = write!(out, "{}, ", def.hash);
        if i % 16 == 15 {
            out.push('\n');
        }
    }
    if n_total % 16 != 0 {
        out.push('\n');
    }
    out.push_str("];\n\n");

    out.push_str("pub(crate) static QSTR_LENGTHS: &[u8; GEN_N_TOTAL] = &[\n");
    for (i, def) in statics.iter().chain(mains.iter()).enumerate() {
        if i % 16 == 0 {
            out.push_str("    ");
        }
        let _ = write!(out, "{}, ", def.len);
        if i % 16 == 15 {
            out.push('\n');
        }
    }
    if n_total % 16 != 0 {
        out.push('\n');
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("qstr_table.rs"), out).expect("cannot write qstr_table.rs");

    // Build options (mirrors Zig `build_options`, consumed by the
    // lexer via `include!` in src/compiler/lexer/mod.rs).
    //
    // STR_UNICODE selects how string-literal escapes are emitted
    // (build.zig `-Dstr-unicode`, default on): true means `str` is a
    // Unicode code-point sequence with escapes UTF-8 encoded;
    // false means `str` is a byte sequence. Override with
    // PETITPY_STR_UNICODE=0 (or =false).
    let str_unicode = match env::var("PETITPY_STR_UNICODE") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
    };
    println!("cargo:rerun-if-env-changed=PETITPY_STR_UNICODE");
    let mut opts = String::new();
    opts.push_str("// Generated by build.rs — build options (mirrors Zig build_options).\n");
    let _ = writeln!(opts, "pub(crate) const STR_UNICODE: bool = {str_unicode};");
    // IS_FULL selects the full (vs nano) configuration (build.zig
    // `-Dconfig=full`, the default; Zig: `is_full = config_preset ==
    // .full`). true means LONGINT_IMPL=MPZ, full constant folding,
    // etc. Override with PETITPY_CONFIG=nano.
    let is_full = match env::var("PETITPY_CONFIG") {
        Ok(v) => !v.eq_ignore_ascii_case("nano"),
        Err(_) => true,
    };
    println!("cargo:rerun-if-env-changed=PETITPY_CONFIG");
    let _ = writeln!(opts, "pub(crate) const IS_FULL: bool = {is_full};");
    fs::write(out_dir.join("build_options.rs"), opts).expect("cannot write build_options.rs");

    // Frozen petitpy-lib bundle (Lane D5 — ADR-003 L2). The `.mpy`
    // files are produced by `scripts/freeze_modules.py` (the
    // original's `tools/freeze_modules.py` analogue) and CHECKED IN
    // under `frozen/`; this emits a `FROZEN_ENTRIES` table of
    // `include_bytes!` slices in the original's MODULES order.
    // When any file is missing the table is EMPTY (the Zig's
    // `-Dfrozen-tiny=false` arm: `frozen_modules.zig` hands back
    // an empty table and frozen lookup always returns null) —
    // the build still succeeds, and `scripts/check_frozen.py`
    // fails loudly if the bundle is stale or absent.
    // `cargo:rerun-if-changed` on the whole dir is not supported;
    // list the files instead (a changed `.mpy` re-runs the build).
    let frozen_names: [&str; 23] = [
        "json",
        "types",
        "_thread",
        "js",
        "heapq",
        "bisect",
        "functools",
        "collections",
        "collections.abc",
        "random",
        "itertools",
        "string",
        "re",
        "subprocess",
        "typing",
        "uuid",
        "shutil",
        "pathlib",
        "datetime",
        "importlib",
        "importlib.metadata",
        "enum",
        "ipaddress",
    ];
    let mut frozen = String::new();
    frozen.push_str("// Generated by build.rs from frozen/*.mpy — do not edit.\n");
    frozen.push_str("// The frozen petitpy-lib bundle (ADR-003 L2), in the\n");
    frozen.push_str("// original tools/freeze_modules.py MODULES order.\n\n");
    frozen.push_str("pub static FROZEN_ENTRIES: &[(&str, &[u8])] = &[\n");
    let mut frozen_ok = true;
    for name in frozen_names {
        let rel = format!("frozen/{name}.mpy");
        let path = manifest_dir.join(&rel);
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_file() {
            let _ = writeln!(
                frozen,
                "    (\"{name}\", include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{rel}\"))),"
            );
        } else {
            frozen_ok = false;
        }
    }
    frozen.push_str("];\n");
    if !frozen_ok {
        // Mirror the Zig's disabled arm: an empty table, not a
        // half-filled one (a partial bundle would silently change
        // which frozen imports resolve).
        frozen = String::new();
        frozen.push_str("// Generated by build.rs — frozen/*.mpy missing; empty table.\n");
        frozen.push_str("// Run `python3 scripts/freeze_modules.py` to populate it.\n\n");
        frozen.push_str("pub static FROZEN_ENTRIES: &[(&str, &[u8])] = &[];\n");
    }
    fs::write(out_dir.join("frozen_table.rs"), frozen).expect("cannot write frozen_table.rs");
}
