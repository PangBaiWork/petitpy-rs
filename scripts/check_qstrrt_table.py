#!/usr/bin/env python3
"""Static-table check: runtime qstr table == compiler qstr table == header.

Usage: python3 scripts/check_qstrrt_table.py

Runs `cargo run --release --example qstrrt_dump`, which prints the
whole static pool twice (runtime accessors, then compiler accessors),
one line per entry: `id<TAB>len<TAB>hash<TAB>hexbytes`. Independently
re-parses `vendor/qstrdefs.generated.h` (same QDEF grammar as
build.rs, separate implementation) and compares, entry by entry:
  - runtime section: 613 lines, ids 0..612
  - compiler section: 613 lines, ids 0..612
  - runtime == compiler on every entry (id, len, hash, bytes)
  - both == the vendored header on every entry
Exit 0 and print PASS only if everything matches; otherwise print
every mismatch and exit 1. This is the acceptance check for the
runtime-qstr static-table sharing — no eyeballing.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADER = ROOT / "vendor" / "qstrdefs.generated.h"


def decode_c_string(raw: bytes) -> bytes:
    out = bytearray()
    i = 0
    while i < len(raw):
        if raw[i] != 0x5C:  # backslash
            out.append(raw[i])
            i += 1
            continue
        i += 1
        assert i < len(raw), "trailing backslash in QDEF string"
        c = raw[i]
        if c == ord("n"):
            out.append(0x0A)
            i += 1
        elif c == ord("t"):
            out.append(0x09)
            i += 1
        elif c == ord("r"):
            out.append(0x0D)
            i += 1
        elif c == ord("0"):
            out.append(0)
            i += 1
        elif c == ord("\\"):
            out.append(0x5C)
            i += 1
        elif c == ord('"'):
            out.append(ord('"'))
            i += 1
        elif c == ord("'"):
            out.append(ord("'"))
            i += 1
        elif c == ord("x"):
            out.append(int(raw[i + 1 : i + 3], 16))
            i += 3
        else:
            raise AssertionError(f"unhandled C escape \\{chr(c)}")
    return bytes(out)


def parse_header(path: Path):
    """Return [(bytes, hash, len)] in id order: QDEF0 first, then QDEF1.

    The header interleaves QDEF0/QDEF1 lines (62 QDEF0 lines sit after
    the first QDEF1 line), so the id order is NOT file order: it is
    all QDEF0 entries in file order, then all QDEF1 entries in file
    order — exactly as build.rs collects them.
    """
    statics = []
    mains = []
    for line in path.read_text().splitlines():
        if line.startswith("QDEF0("):
            target = statics
        elif line.startswith("QDEF1("):
            target = mains
        else:
            continue
        body = line[line.index("(") + 1 : line.rindex(")")]
        # body = " MP_QSTR_x, HASH, LEN, \"STR\" "
        parts = body.split(",", 3)
        assert len(parts) == 4, f"bad QDEF line: {line}"
        h = int(parts[1].strip())
        ln = int(parts[2].strip())
        rest = parts[3].strip()
        assert rest.startswith('"') and rest.endswith('"'), f"bad string: {line}"
        raw = rest[1:-1].encode("latin-1")
        decoded = decode_c_string(raw)
        assert len(decoded) == ln, f"declared len mismatch: {line}"
        target.append((decoded, h, ln))
    return statics + mains


def parse_dump(text: str):
    """Return {section: [(id, len, hash, bytes)]} from qstrrt_dump output."""
    sections = {}
    current = None
    for line in text.splitlines():
        if line in ("runtime:", "compiler:"):
            current = line[:-1]
            sections[current] = []
            continue
        if current is None or not line:
            continue
        fields = line.split("\t")
        assert len(fields) == 4, f"bad dump line: {line!r}"
        sections[current].append(
            (int(fields[0]), int(fields[1]), int(fields[2]), bytes.fromhex(fields[3]))
        )
    return sections


def main() -> int:
    header = parse_header(HEADER)
    n_static = sum(1 for line in HEADER.read_text().splitlines() if line.startswith("QDEF0("))
    n_main = sum(1 for line in HEADER.read_text().splitlines() if line.startswith("QDEF1("))
    print(f"header: QDEF0={n_static} QDEF1={n_main} total={len(header)}")

    proc = subprocess.run(
        ["cargo", "run", "--release", "--example", "qstrrt_dump"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print("qstrrt_dump failed:")
        print(proc.stdout)
        print(proc.stderr, file=sys.stderr)
        return 1

    sections = parse_dump(proc.stdout)
    errors = []
    for name in ("runtime", "compiler"):
        rows = sections.get(name)
        if rows is None:
            errors.append(f"missing section {name}:")
            continue
        print(f"{name}: {len(rows)} entries")
        if len(rows) != len(header):
            errors.append(f"{name}: {len(rows)} entries, header has {len(header)}")
            continue
        for (rid, rlen, rhash, rbytes), (hbytes, hhash, hlen) in zip(rows, header):
            if rlen != hlen or rhash != hhash or rbytes != hbytes:
                errors.append(
                    f"{name}[{rid}]: dump len={rlen} hash={rhash} bytes={rbytes!r} "
                    f"!= header len={hlen} hash={hhash} bytes={hbytes!r}"
                )
                if len(errors) > 10:
                    break

    rt = sections.get("runtime", [])
    cp = sections.get("compiler", [])
    agree = 0
    if len(rt) == len(cp) == len(header):
        for (rid, rlen, rhash, rbytes), (cid, clen, chash, cbytes) in zip(rt, cp):
            if (rid, rlen, rhash, rbytes) == (cid, clen, chash, cbytes):
                agree += 1
            else:
                errors.append(f"runtime[{rid}] != compiler[{cid}]")
                if len(errors) > 20:
                    break
    print(f"runtime vs compiler: {agree}/{len(header)} identical")

    if errors:
        for e in errors[:25]:
            print("MISMATCH:", e)
        print("FAIL")
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
