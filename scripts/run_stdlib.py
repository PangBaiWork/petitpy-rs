#!/usr/bin/env python3
"""test-stdlib gate for the Rust petitpy binary.

The original's Makefile `test-stdlib` rule, verbatim:
    for f in tests/stdlib/test_*.py; do <BIN> $f || exit $?; done
Each file must exit 0 (timeout 30s per file). Reports the count
and any failure with its output.

Usage:
    python3 scripts/run_stdlib.py [--bin BIN]
Default --bin is target/release/petitpy-rs. Pass the reference
binary to verify the runner reproduces the reference's own
result first (must be 8/8).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = Path("/home/hatch/workspace/petitpy-orig")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", default=str(ROOT / "target" / "release" / "petitpy-rs"))
    args = ap.parse_args()
    files = sorted((ORIG / "tests" / "stdlib").glob("test_*.py"))
    npass = 0
    failures: list[str] = []
    for f in files:
        try:
            # The tests do `sys.path.insert(0, "petitpy-lib")` — a
            # path relative to the repo root — so they must run with
            # cwd=ORIG, exactly as `make test-stdlib` runs them.
            p = subprocess.run(
                [args.bin, str(f)],
                capture_output=True,
                text=True,
                timeout=30,
                cwd=str(ORIG),
            )
            ok = p.returncode == 0
            out = (p.stdout + p.stderr).strip()
        except subprocess.TimeoutExpired:
            ok, out = False, "[timeout after 30s]"
        if ok:
            npass += 1
            print(f"  PASS  {f.name}")
        else:
            failures.append(f.name)
            print(f"  FAIL  {f.name}")
            print(f"    {out[:400]!r}")
    print(f"stdlib: {npass}/{len(files)} passed")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
