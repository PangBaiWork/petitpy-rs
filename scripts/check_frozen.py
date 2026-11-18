#!/usr/bin/env python3
"""Verify the frozen bundle (Lane D5).

1. Recompile every module in `frozen/frozen-index.txt` with
   `examples/compile_mpy` (source name `{modname}.py`) into a temp
   dir and compare bytes with the checked-in `frozen/*.mpy` —
   the baked bytes must be exactly what the current compiler
   emits (the list has 23 entries).
2. Compare each checked-in `frozen/*.mpy` with the original's
   `build-frozen/*.mpy` (compiled by `zigpy-compile` from the
   same sources) — the Rust compiler is byte-identical to the
   original on the 552-file corpus, so these should match too.

Usage: python3 scripts/check_frozen.py
Exit 0 only when both comparisons are exact.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"
COMPILE_MPY = ROOT / "target" / "release" / "examples" / "compile_mpy"
FROZEN_DIR = ROOT / "frozen"
ORIG_FROZEN = ORIG / "build-frozen"

# (module_name, relative_path_from_orig_root) — kept in sync with
# scripts/freeze_modules.py MODULES (import it to avoid drift).
sys.path.insert(0, str(ROOT / "scripts"))
from freeze_modules import MODULES  # noqa: E402


def main() -> int:
    if not COMPILE_MPY.is_file():
        print(
            f"error: {COMPILE_MPY} not found.\n"
            "  Build it first: `cargo build --release --example compile_mpy`",
            file=sys.stderr,
        )
        return 2
    names = [m for m, _ in MODULES]
    print(f"frozen modules: {len(names)}")
    # 1. Fresh compile vs checked-in bytes.
    regen_ok = 0
    regen_bad: list[str] = []
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        for modname, relpath in MODULES:
            src = ORIG / relpath
            out = tmpdir / f"{modname}.mpy"
            result = subprocess.run(
                [str(COMPILE_MPY), str(src), str(out), "file", f"{modname}.py"],
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                print(f"  COMPILE FAILED {modname}: {result.stderr.strip()}")
                regen_bad.append(modname)
                continue
            checked = FROZEN_DIR / f"{modname}.mpy"
            if not checked.is_file():
                print(f"  MISSING frozen/{modname}.mpy")
                regen_bad.append(modname)
                continue
            if out.read_bytes() == checked.read_bytes():
                regen_ok += 1
            else:
                print(f"  BYTES DIFFER (regen vs checked-in) {modname}")
                regen_bad.append(modname)
    print(f"regen vs checked-in: {regen_ok}/{len(names)} identical")
    # 2. Checked-in bytes vs the original's build-frozen/*.mpy.
    orig_ok = 0
    orig_bad: list[str] = []
    for modname, _ in MODULES:
        checked = FROZEN_DIR / f"{modname}.mpy"
        orig = ORIG_FROZEN / f"{modname}.mpy"
        if not orig.is_file():
            print(f"  NO ORIGINAL {modname}.mpy (skipped)")
            continue
        if not checked.is_file():
            orig_bad.append(modname)
            continue
        if checked.read_bytes() == orig.read_bytes():
            orig_ok += 1
        else:
            print(f"  BYTES DIFFER (checked-in vs original) {modname}")
            orig_bad.append(modname)
    print(f"checked-in vs original: {orig_ok}/{len(names)} identical")
    total = sum(p.stat().st_size for p in FROZEN_DIR.glob("*.mpy"))
    print(f"frozen total: {total} bytes across {len(list(FROZEN_DIR.glob('*.mpy')))} .mpy files")
    if regen_bad or orig_bad:
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
