#!/usr/bin/env python3
"""Run the ORIGINAL's full-corpus gate (tools/full_gate.py) against an
arbitrary binary — the Rust binary by default.

The original gate hardcodes BIN to zig-out/bin/petitpy and takes no
--bin argument, and the original repo is read-only, so this script
imports full_gate as a module and overrides full_gate.BIN before
calling its main(). Gate semantics are identical: for every
corpus/basics/*.py, run [BIN, script], compare
stdout + stderr + "\\n[exit N]\\n" byte-for-byte against
corpus/full/basics/<name>.out (10s timeout), and check
bidirectionally against corpus/full-known-fails.txt.

Usage:
    python3 scripts/run_full_gate.py [--bin PATH]
Default binary: target/release/petitpy-rs.
Pass the reference binary to reproduce the reference's own result
(545/552, the same 7 fails) as a sanity check of this runner.
"""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"

sys.path.insert(0, str(ORIG / "tools"))
import full_gate  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--bin",
        default=str(ROOT / "target" / "release" / "petitpy-rs"),
        help="binary under test (default: target/release/petitpy-rs)",
    )
    args = ap.parse_args()
    full_gate.BIN = Path(args.bin)
    print(f"full gate binary: {full_gate.BIN}")
    return full_gate.main([])


if __name__ == "__main__":
    sys.exit(main())
