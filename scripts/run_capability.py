#!/usr/bin/env python3
"""Capability-matrix gate for the Rust petitpy binaries.

Imports the original's `tools/capability_matrix.py` as a module
and overrides its hardcoded tool paths, the way
`scripts/run_full_gate.py` does for `full_gate.py`:

- `ZIGPY` -> the Rust `zigpy-compile` binary (default
  `target/release/zigpy-compile`); `VM` -> the Rust
  `petitpy-vm` binary (default `target/release/petitpy-vm`).
- `compile_pyfront` -> a stub returning False: pyfront needs
  `uv run --project projects/pyfront`, and `uv` is not installed
  in this VM, so the pyfront column is unmeasurable here (the
  original's `compile_pyfront` would raise FileNotFoundError).
  A construct no front-end compiled is NOT a gate finding (only
  back-end gaps, back-end splits, and front-end splits are
  gated), so this does not hide failures — it only leaves the
  `via` column as `z`.
- The vendor MicroPython column (`UPY`) is absent in this VM
  (no `vendor/micropython/ports/unix/build-standard/micropython`),
  so `have_upy` is False and that column is always None, exactly
  as when the matrix runs against the reference here.

The gate itself (`--gate`, bidirectional vs
`corpus/capability-known.txt`, whose baseline is EMPTY — only
comments) is unchanged. `want` is CPython's stdout for each of
the 67 constructs; `outcome` is ok/diff/FAIL (rc != 0).

Usage:
    python3 scripts/run_capability.py [--zigpy BIN] [--vm BIN]
Pass the reference binaries to verify the runner reproduces the
reference's own result first (must print GATE: GREEN).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = Path("/home/hatch/workspace/petitpy-orig")

sys.path.insert(0, str(ORIG / "tools"))

import capability_matrix  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--zigpy", default=str(ROOT / "target" / "release" / "zigpy-compile"))
    ap.add_argument("--vm", default=str(ROOT / "target" / "release" / "petitpy-vm"))
    args = ap.parse_args()
    capability_matrix.ZIGPY = Path(args.zigpy)
    capability_matrix.VM = Path(args.vm)
    # `uv` is not installed in this VM; the pyfront column is
    # unmeasurable (see the module docstring).
    capability_matrix.compile_pyfront = lambda src, out: False  # noqa: E731
    print(f"capability: zigpy={args.zigpy} vm={args.vm} (pyfront unmeasurable: no uv)")
    return capability_matrix.main(["--gate"])


if __name__ == "__main__":
    sys.exit(main())
