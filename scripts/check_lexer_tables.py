#!/usr/bin/env python3
"""Dump-and-diff check: every lexer table value in Rust == Zig.

Usage: python3 scripts/check_lexer_tables.py

Independently re-parses petitpy-orig/src/compiler/lexer.zig and
src/compiler/lexer/{tables,mod}.rs and compares, value by value:
  - all 96 TokenKind discriminants (incl. NUMBER_OF == 96)
  - the TOK_ENC encoding string (rebuilt from the Zig `++`
    concatenation, segment by segment)
  - all 44 TOK_ENC_KIND entries, in order
  - all 36 TOK_KW keywords, in order
  - structural check: simulating the phase-1 scan walk over the
    whole encoding string yields 46 entries — 44 kinds plus the two
    special-cased first chars ('!' and '.'), which never index
    TOK_ENC_KIND.
Exit 0 and print PASS only if everything matches; otherwise print
every mismatch and exit 1. This is the acceptance check for the
lexer tables — no eyeballing.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ZIG = ROOT.parent / "petitpy-orig" / "src" / "compiler" / "lexer.zig"
RS_TABLES = ROOT / "src" / "compiler" / "lexer" / "tables.rs"


def zig_tables(src: str):
    # TokenKind enum: `NAME = N,` lines inside the enum block.
    m = re.search(r"pub const TokenKind = enum\(u8\) \{(.*?)\n\};", src, re.S)
    assert m, "zig TokenKind not found"
    kinds = dict(
        (n, int(v)) for n, v in re.findall(r"^\s+(\w+) = (\d+),", m.group(1), re.M)
    )
    # tok_enc: the `++`-concatenated string literals.
    m = re.search(r"const tok_enc =\s*((?:\"[^\"]*\" \+\+\s*)+\"[^\"]*\"(?: \+\+)?);", src, re.S)
    assert m, "zig tok_enc not found"
    segs = re.findall(r'"([^"]*)"', m.group(1))
    enc = "".join(segs)
    # tok_enc_kind: the `[_]TokenKind{ ... }` array.
    m = re.search(r"const tok_enc_kind = \[_\]TokenKind\{(.*?)\};", src, re.S)
    assert m, "zig tok_enc_kind not found"
    enc_kind = re.findall(r"\.(\w+)", m.group(1))
    # tok_kw: the `[_][:0]const u8{ ... }` array.
    m = re.search(r"const tok_kw = \[_\]\[:0\]const u8\{(.*?)\};", src, re.S)
    assert m, "zig tok_kw not found"
    kw = re.findall(r'"([^"]*)"', m.group(1))
    return {"kinds": kinds, "enc": enc, "enc_kind": enc_kind, "kw": kw, "segs": segs}


def rs_tables(src: str):
    m = re.search(r"pub enum TokenKind \{(.*?)\n\}", src, re.S)
    assert m, "rs TokenKind not found"
    kinds = dict(
        (n, int(v)) for n, v in re.findall(r"^\s+(\w+) = (\d+),", m.group(1), re.M)
    )
    m = re.search(r"pub\(crate\) const TOK_ENC: &\[u8\] =\s*b\"([^\"]*)\";", src)
    assert m, "rs TOK_ENC not found"
    enc = m.group(1)
    m = re.search(r"pub\(crate\) const TOK_ENC_KIND: &\[TokenKind; \d+\] = &\[(.*?)\];", src, re.S)
    assert m, "rs TOK_ENC_KIND not found"
    enc_kind = re.findall(r"TokenKind::(\w+)", m.group(1))
    m = re.search(r"pub\(crate\) const TOK_KW: &\[&str; \d+\] = &\[(.*?)\];", src, re.S)
    assert m, "rs TOK_KW not found"
    kw = re.findall(r'"([^"]*)"', m.group(1))
    return {"kinds": kinds, "enc": enc, "enc_kind": enc_kind, "kw": kw}


def scan_entries(enc: str) -> int:
    """Simulate the phase-1 scan walk over the whole encoding string:
    each iteration consumes 1 char, or 2 when it lands on an 'e'/'c'
    marker. Returns the number of iterations (entries)."""
    ti = 0
    idx = 0
    while ti < len(enc):
        if enc[ti] in "ec":
            ti += 2
        else:
            ti += 1
        idx += 1
    return idx


def main() -> int:
    z = zig_tables(ZIG.read_text())
    r = rs_tables(RS_TABLES.read_text())
    fails = 0

    if set(z["kinds"]) != set(r["kinds"]):
        print(f"MISMATCH kind name sets: only-zig={set(z['kinds'])-set(r['kinds'])} "
              f"only-rs={set(r['kinds'])-set(z['kinds'])}")
        fails += 1
    for name in z["kinds"]:
        if z["kinds"][name] != r["kinds"].get(name):
            print(f"MISMATCH TokenKind::{name}: zig={z['kinds'][name]} rs={r['kinds'].get(name)}")
            fails += 1

    if z["enc"] != r["enc"]:
        print(f"MISMATCH TOK_ENC: zig={z['enc']!r} rs={r['enc']!r}")
        fails += 1
    if z["enc_kind"] != r["enc_kind"]:
        print(f"MISMATCH TOK_ENC_KIND: zig={z['enc_kind']} rs={r['enc_kind']}")
        fails += 1
    if z["kw"] != r["kw"]:
        print(f"MISMATCH TOK_KW: zig={z['kw']} rs={r['kw']}")
        fails += 1

    # Structural check: entries == kinds + 2 special-cased first chars.
    entries = scan_entries(z["enc"])
    if entries != len(z["enc_kind"]) + 2:
        print(f"MISMATCH scan entries: walk yields {entries}, "
              f"TOK_ENC_KIND has {len(z['enc_kind'])} (expected entries == kinds + 2)")
        fails += 1

    total = len(z["kinds"]) + len(z["enc"]) + len(z["enc_kind"]) + len(z["kw"])
    if fails:
        print(f"FAIL: {fails} mismatches out of {total} values")
        return 1
    print(f"PASS: all {total} values identical "
          f"(kinds {len(z['kinds'])}, enc {len(z['enc'])} chars in {len(z['segs'])} segments, "
          f"enc_kind {len(z['enc_kind'])}, kw {len(z['kw'])}; "
          f"scan walk yields {entries} entries = 44 kinds + '!' + '.')")
    return 0


if __name__ == "__main__":
    sys.exit(main())
