#!/usr/bin/env python3
"""Corpus nano gate (INVARIANT CHECK ONLY) for the Rust binary.

Thin wrapper around the original's
`python3 tools/progress.py --config nano --strict <BIN>` — the
nano arm (541/541 byte-exact on the reference nano build).
The Rust project has NO nano build; the full binary is the only
binary. This runs the nano corpus goldens
(`corpus/nano/basics/`) through the Rust full binary with the
same strict rule and REPORTS the number. Divergences that come
from full-vs-nano config differences (ROM_LEVEL, frozen set,
module tables) are documented in §4 of
`scripts/e2_result.txt`, not fixed here.

Usage:
    python3 scripts/run_nano.py [--bin BIN]
Default --bin is target/release/petitpy-rs.
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
    return subprocess.run(
        [
            sys.executable,
            str(ORIG / "tools" / "progress.py"),
            "--config",
            "nano",
            "--strict",
            args.bin,
        ]
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
