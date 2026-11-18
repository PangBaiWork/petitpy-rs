#!/usr/bin/env python3
"""Integration-smoke gate for the Rust petitpy binary.

Thin wrapper around the original's `tests/integration.py`,
which already takes the binary as argv (202 cases, exact-stdout
assertions; see `tests/_integration_runner.py` for the
comparison rule — stdout compared exactly, `expected=None`
skips the stdout check, exit code checked when given as the
4th tuple element, timeout 30s, config `full` by default).

Usage:
    python3 scripts/run_integration.py [--bin BIN]
Default --bin is target/release/petitpy-rs. Pass the reference
binary to verify the runner reproduces the reference's own
result first.
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
    # Run from the original repo root, matching how the reference
    # baseline was measured (`make test` runs there).
    return subprocess.run(
        [sys.executable, str(ORIG / "tests" / "integration.py"), args.bin],
        cwd=str(ORIG),
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
