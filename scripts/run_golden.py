#!/usr/bin/env python3
"""Golden-programs gate for the Rust petitpy binaries.

Implements the run-and-compare rule of the original's
`tools/golden_bootstrap.py` (`evaluate_program`): a program
"passes" when its COMBINED stdout+stderr, normalized (rstrip per
line, strip overall), matches an oracle's, and it did not time
out (TIMEOUT_SECONDS=10, cwd=the program's parent dir).

Adaptation for a reimplementation gate (see §1 of
`scripts/e2_result.txt`): the bootstrap compares the petitpy run
against the vendor MicroPython oracle OR CPython. The vendor
MicroPython build does not exist in this VM, and matching
CPython alone would measure CPython-divergence, not
petitpy-fidelity (petitpy deliberately differs from CPython in
places, e.g. `hash(2**70) == 0`). So the oracle here is the
REFERENCE petitpy binary itself, with CPython kept as a second,
informational column (`cpy_match`): PASS iff the Rust run's
normalized combined output equals the reference petitpy run's
(no timeout on either side).

Usage:
    python3 scripts/run_golden.py [--bin BIN] [--ref REF]
        [--dir works|works-native-only|both]
Defaults: --bin target/release/petitpy-rs,
--ref <orig>/zig-out/bin/petitpy. Exits 0 iff every program
passes (both dirs).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = Path("/home/hatch/workspace/petitpy-orig")
TIMEOUT_SECONDS = 10


def normalize(text: str) -> str:
    """Bootstrap rule: rstrip each line, strip overall."""
    return "\n".join(line.rstrip() for line in text.splitlines()).strip()


def run_under(argv: list[str], script: Path):
    """Run `argv + [script]`; return (combined-normalized, timed_out, rc)."""
    try:
        p = subprocess.run(
            [*argv, str(script)],
            capture_output=True,
            text=False,
            timeout=TIMEOUT_SECONDS,
            cwd=str(script.parent),
            check=False,
        )
        combined = p.stdout.decode("utf-8", errors="replace") + p.stderr.decode(
            "utf-8", errors="replace"
        )
        return normalize(combined), False, p.returncode
    except subprocess.TimeoutExpired:
        return "", True, -1
    except (FileNotFoundError, PermissionError) as e:
        return f"[runner error] {e}", False, -2


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", default=str(ROOT / "target" / "release" / "petitpy-rs"))
    ap.add_argument("--ref", default=str(ORIG / "zig-out" / "bin" / "petitpy"))
    ap.add_argument(
        "--dir", choices=["works", "works-native-only", "both"], default="both"
    )
    args = ap.parse_args()
    dirs = (
        ["works", "works-native-only"]
        if args.dir == "both"
        else [args.dir]
    )
    total_pass = total = 0
    cpy_match = 0
    all_failures: list[tuple[str, str, str, str]] = []
    for d in dirs:
        programs = sorted((ORIG / "golden-programs" / d).glob("*.py"))
        npass = 0
        failures: list[tuple[str, str, str, str]] = []
        for prog in programs:
            rust_out, rust_to, _ = run_under([args.bin], prog)
            ref_out, ref_to, _ = run_under([args.ref], prog)
            cpy_out, cpy_to, _ = run_under([sys.executable], prog)
            if not rust_to and not cpy_to and rust_out == cpy_out:
                cpy_match += 1
            ok = (not rust_to and not ref_to) and rust_out == ref_out
            if ok:
                npass += 1
            else:
                reason = "timeout" if (rust_to or ref_to) else "output differs"
                failures.append((prog.name, reason, rust_out, ref_out))
        print(f"golden {d}: {npass}/{len(programs)} passed")
        for name, reason, got, want in failures:
            print(f"  FAIL  {name} ({reason})")
            print(f"    rust: {got[:200]!r}")
            print(f"    ref : {want[:200]!r}")
        total_pass += npass
        total += len(programs)
        all_failures.extend(failures)
    print(f"golden total: {total_pass}/{total} passed (rust==cpython on {cpy_match}/{total})")
    return 0 if not all_failures else 1


if __name__ == "__main__":
    sys.exit(main())
