#!/usr/bin/env python3
"""Dump-and-diff check: every grammar_data value in Rust == Zig.

Usage: python3 scripts/check_grammar_data.py

Independently re-parses petitpy-orig/src/compiler/grammar_data.zig and
src/compiler/grammar_data.rs and compares, value by value:
  - NUM_RULES, FIRST_RULE_WITH_OFFSET_ABOVE_255, all *_MASK constants
  - all 183 RULE_* id constants
  - rule_act_table (183 u8), rule_arg_combined_table (467 u16),
    rule_arg_offset_table (183 u8)
Exit 0 and print PASS only if everything matches; otherwise print every
mismatch and exit 1. This is the acceptance check for the grammar_data
port — no eyeballing.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ZIG = ROOT.parent / "petitpy-orig" / "src" / "compiler" / "grammar_data.zig"
RS = ROOT / "src" / "compiler" / "grammar_data.rs"


def zig_tables(src: str):
    def table(name, typ):
        m = re.search(r"pub const " + name + r" = \[_\]" + typ + r"\{(.*?)\};", src, re.S)
        assert m, f"zig table {name} not found"
        return [int(v, 16) for v in re.findall(r"0x[0-9a-fA-F]+", m.group(1))]

    rules = dict(
        (n, int(v)) for n, v in re.findall(r"pub const (RULE_\w+): u8 = (\d+);", src)
    )
    scalars = {}
    for name, typ in [
        ("NUM_RULES", "usize"),
        ("FIRST_RULE_WITH_OFFSET_ABOVE_255", "u8"),
        ("RULE_ACT_ARG_MASK", "u8"),
        ("RULE_ACT_KIND_MASK", "u8"),
        ("RULE_ACT_ALLOW_IDENT", "u8"),
        ("RULE_ACT_ADD_BLANK", "u8"),
        ("RULE_ACT_OR", "u8"),
        ("RULE_ACT_AND", "u8"),
        ("RULE_ACT_LIST", "u8"),
        ("RULE_ARG_KIND_MASK", "u16"),
        ("RULE_ARG_ARG_MASK", "u16"),
        ("RULE_ARG_TOK", "u16"),
        ("RULE_ARG_RULE", "u16"),
        ("RULE_ARG_OPT_RULE", "u16"),
    ]:
        m = re.search(r"pub const " + name + r": " + typ + r" = (0x[0-9a-fA-F]+|\d+);", src)
        assert m, f"zig scalar {name} not found"
        scalars[name] = int(m.group(1), 0)
    return {
        "rules": rules,
        "scalars": scalars,
        "act": table("rule_act_table", "u8"),
        "comb": table("rule_arg_combined_table", "u16"),
        "off": table("rule_arg_offset_table", "u8"),
    }


def rs_tables(src: str):
    def table(name, typ):
        m = re.search(r"pub const " + name + r": &\[" + typ + r"; \d+\] = &\[(.*?)\];", src, re.S)
        assert m, f"rs table {name} not found"
        return [int(v, 16) for v in re.findall(r"0x[0-9a-fA-F]+", m.group(1))]

    rules = dict(
        (n, int(v)) for n, v in re.findall(r"pub const (RULE_\w+): u8 = (\d+);", src)
    )
    scalars = {}
    for name, typ in [
        ("NUM_RULES", "usize"),
        ("FIRST_RULE_WITH_OFFSET_ABOVE_255", "u8"),
        ("RULE_ACT_ARG_MASK", "u8"),
        ("RULE_ACT_KIND_MASK", "u8"),
        ("RULE_ACT_ALLOW_IDENT", "u8"),
        ("RULE_ACT_ADD_BLANK", "u8"),
        ("RULE_ACT_OR", "u8"),
        ("RULE_ACT_AND", "u8"),
        ("RULE_ACT_LIST", "u8"),
        ("RULE_ARG_KIND_MASK", "u16"),
        ("RULE_ARG_ARG_MASK", "u16"),
        ("RULE_ARG_TOK", "u16"),
        ("RULE_ARG_RULE", "u16"),
        ("RULE_ARG_OPT_RULE", "u16"),
    ]:
        m = re.search(r"pub const " + name + r": " + typ + r" = (0x[0-9a-fA-F]+|\d+);", src)
        assert m, f"rs scalar {name} not found"
        scalars[name] = int(m.group(1), 0)
    return {
        "rules": rules,
        "scalars": scalars,
        "act": table("RULE_ACT_TABLE", "u8"),
        "comb": table("RULE_ARG_COMBINED_TABLE", "u16"),
        "off": table("RULE_ARG_OFFSET_TABLE", "u8"),
    }


def main() -> int:
    z = zig_tables(ZIG.read_text())
    r = rs_tables(RS.read_text())
    fails = 0

    for name, zv in z["scalars"].items():
        rv = r["scalars"].get(name)
        if zv != rv:
            print(f"MISMATCH scalar {name}: zig={zv} rs={rv}")
            fails += 1
    if set(z["rules"]) != set(r["rules"]):
        print(f"MISMATCH rule name sets: only-zig={set(z['rules'])-set(r['rules'])} "
              f"only-rs={set(r['rules'])-set(z['rules'])}")
        fails += 1
    for name in z["rules"]:
        if z["rules"][name] != r["rules"].get(name):
            print(f"MISMATCH {name}: zig={z['rules'][name]} rs={r['rules'].get(name)}")
            fails += 1
    for key, label in [("act", "rule_act_table"), ("comb", "rule_arg_combined_table"),
                       ("off", "rule_arg_offset_table")]:
        zv, rv = z[key], r[key]
        if len(zv) != len(rv):
            print(f"MISMATCH {label} length: zig={len(zv)} rs={len(rv)}")
            fails += 1
            continue
        diffs = [(i, a, b) for i, (a, b) in enumerate(zip(zv, rv)) if a != b]
        for i, a, b in diffs[:10]:
            print(f"MISMATCH {label}[{i}]: zig=0x{a:x} rs=0x{b:x}")
        fails += len(diffs)

    total = (len(z["scalars"]) + len(z["rules"]) + len(z["act"])
             + len(z["comb"]) + len(z["off"]))
    if fails:
        print(f"FAIL: {fails} mismatches out of {total} values")
        return 1
    print(f"PASS: all {total} values identical "
          f"(scalars {len(z['scalars'])}, rules {len(z['rules'])}, "
          f"act {len(z['act'])}, combined {len(z['comb'])}, offset {len(z['off'])})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
