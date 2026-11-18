#!/usr/bin/env python3
"""Freeze the petitpy-lib module set to .mpy (Lane D5).

Mirrors the original's `tools/freeze_modules.py` (ADR-003 L2): the
MODULES list below is the same list, in the same order, and each
module is compiled with the Rust compiler (`examples/compile_mpy`,
the `zigpy-compile` analogue) with the source name `{modname}.py`
(the original passes `-s {modname}.py` to `zigpy-compile`).

Output lands in `frozen/` next to this repo root:
  - `frozen/<modname>.mpy` — one file per module, CHECKED IN
    (unlike the original's `build-frozen/`, which is a build
    artifact: the Rust build has no `make freeze` step, so the
    deterministic compiler outputs are committed and `build.rs`
    bakes them in unconditionally — see `build.rs`).
  - `frozen/frozen-index.txt` — the module names in table order.

Regenerate after any change to the compiler or to `petitpy-lib/`:
    python3 scripts/freeze_modules.py
Verify with:
    python3 scripts/check_frozen.py
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"
COMPILE_MPY = ROOT / "target" / "release" / "examples" / "compile_mpy"
OUT_DIR = ROOT / "frozen"

# (module_name, relative_path_from_orig_root) — order matches the
# table in the original's tools/freeze_modules.py MODULES. `os.path`
# is deferred there and is NOT in this list.
MODULES = [
    ("json", "petitpy-lib/json.py"),
    ("types", "petitpy-lib/types.py"),
    ("_thread", "petitpy-lib/_thread.py"),
    ("js", "petitpy-lib/js.py"),
    ("heapq", "petitpy-lib/heapq.py"),
    ("bisect", "petitpy-lib/bisect.py"),
    ("functools", "petitpy-lib/functools.py"),
    ("collections", "petitpy-lib/collections/__init__.py"),
    ("collections.abc", "petitpy-lib/collections/abc.py"),
    ("random", "petitpy-lib/random.py"),
    ("itertools", "petitpy-lib/itertools.py"),
    ("string", "petitpy-lib/string.py"),
    ("re", "petitpy-lib/re.py"),
    ("subprocess", "petitpy-lib/subprocess.py"),
    ("typing", "petitpy-lib/typing.py"),
    ("uuid", "petitpy-lib/uuid.py"),
    ("shutil", "petitpy-lib/shutil.py"),
    ("pathlib", "petitpy-lib/pathlib.py"),
    ("datetime", "petitpy-lib/datetime.py"),
    ("importlib", "petitpy-lib/importlib/__init__.py"),
    ("importlib.metadata", "petitpy-lib/importlib/metadata.py"),
    ("enum", "petitpy-lib/enum.py"),
    ("ipaddress", "petitpy-lib/ipaddress.py"),
]


def main() -> int:
    if not COMPILE_MPY.is_file():
        print(
            f"error: {COMPILE_MPY} not found.\n"
            "  Build it first: `cargo build --release --example compile_mpy`",
            file=sys.stderr,
        )
        return 2
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    total_bytes = 0
    for modname, relpath in MODULES:
        src = ORIG / relpath
        if not src.is_file():
            print(f"error: source {src} not found", file=sys.stderr)
            return 2
        out = OUT_DIR / f"{modname}.mpy"
        # compile_mpy <input.py> <output.mpy> file <source-name>
        # (the source name seeds qstr-table index 0 and therefore
        # affects the emitted bytes — `{modname}.py`, like the
        # original's `-s {modname}.py`).
        cmd = [
            str(COMPILE_MPY),
            str(src),
            str(out),
            "file",
            f"{modname}.py",
        ]
        print(f"  freeze {modname:20s} <- {relpath}")
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            print(f"error: compile failed for {modname}:", file=sys.stderr)
            print(result.stdout, file=sys.stderr)
            print(result.stderr, file=sys.stderr)
            return 1
        size = out.stat().st_size
        total_bytes += size
        print(f"           -> frozen/{modname}.mpy ({size} bytes)")
    (OUT_DIR / "frozen-index.txt").write_text(
        "".join(f"{name}\n" for name, _ in MODULES)
    )
    print(f"\nfrozen total: {total_bytes} bytes across {len(MODULES)} modules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
