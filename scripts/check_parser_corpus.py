#!/usr/bin/env python3
"""Corpus verdict check: Rust parser vs. original petitpy binary.

Usage: python3 scripts/check_parser_corpus.py [--corpus DIR]

For every *.py file under the corpus dir (default:
../petitpy-orig/corpus/basics):
  - run the Rust parse dump (`cargo run --release --example
    parse_dump`, built once up front) and record whether it parsed
    cleanly (exit 0) or hit a parse error (exit 1);
  - run the original binary (../petitpy-orig/zig-out/bin/petitpy)
    on the same file and record whether it ran cleanly (exit 0) or
    failed (non-zero — syntax error, compile error, runtime error,
    anything).

Acceptance: every file the Rust parser rejects must also be rejected
by the original binary (a parse error is a syntax error). The
converse does not hold: the original may fail at compile/runtime on
files the parser accepts (e.g. deque_micropython.py fails at runtime
with TypeError but parses fine).
Exit 0 and print PASS only if there are zero mismatches; otherwise
list every file the Rust parser rejects but the original runs, and
exit 1. A per-file timeout guards against a parser that stops
advancing (infinite loop); a timeout counts as a parse error.
"""

import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"
PARSE_DUMP = ROOT / "target" / "release" / "examples" / "parse_dump"
PETITPY = ORIG / "zig-out" / "bin" / "petitpy"
TIMEOUT_S = 10


def run(cmd, timeout=TIMEOUT_S):
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout)
        return p.returncode
    except subprocess.TimeoutExpired:
        return "timeout"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=str(ORIG / "corpus" / "basics"))
    args = ap.parse_args()
    corpus = Path(args.corpus)

    if not corpus.is_dir():
        print(f"ERROR (infra): corpus dir not found: {corpus}")
        return 2
    if not PETITPY.is_file():
        print(f"ERROR (infra): original binary not found: {PETITPY}")
        return 2

    # Build the Rust dump binary once.
    t0 = time.time()
    cargo = Path.home() / ".cargo" / "bin" / "cargo"
    cargo_cmd = str(cargo) if cargo.is_file() else "cargo"
    b = subprocess.run(
        [cargo_cmd, "build", "--release", "--example", "parse_dump"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if b.returncode != 0:
        print("ERROR (infra): cargo build --example parse_dump failed")
        print(b.stdout[-2000:])
        print(b.stderr[-2000:])
        return 2
    if not PARSE_DUMP.is_file():
        print(f"ERROR (infra): parse_dump binary not found at {PARSE_DUMP}")
        return 2

    files = sorted(corpus.rglob("*.py"))
    if not files:
        print(f"ERROR (infra): no .py files under {corpus}")
        return 2

    rust_clean = rust_error = orig_ok = orig_fail = 0
    mismatches = []
    rust_timeouts = []
    rust_accepts_orig_fails = []
    for path in files:
        rc_rust = run([str(PARSE_DUMP), str(path)])
        if rc_rust == "timeout":
            rust_timeouts.append(path.name)
            rust_error += 1
            rust_ok = False
        else:
            rust_ok = rc_rust == 0
            if rust_ok:
                rust_clean += 1
            else:
                rust_error += 1
        rc_orig = run([str(PETITPY), str(path)])
        orig_ok_flag = rc_orig == 0
        if orig_ok_flag:
            orig_ok += 1
        else:
            orig_fail += 1
        if not rust_ok and orig_ok_flag:
            mismatches.append(path.name)
        if rust_ok and not orig_ok_flag:
            rust_accepts_orig_fails.append(path.name)

    n = len(files)
    print(f"corpus: {n} files in {time.time() - t0:.1f}s")
    print(f"  Rust parser: {rust_clean} clean, {rust_error} error"
          + (f" (incl. {len(rust_timeouts)} timeouts)" if rust_timeouts else ""))
    print(f"  original   : {orig_ok} ok, {orig_fail} fail")
    if rust_timeouts:
        print(f"  Rust timeouts: {', '.join(rust_timeouts)}")
    if rust_accepts_orig_fails:
        print(f"  info: {len(rust_accepts_orig_fails)} files parse in Rust "
              f"but fail in the original (compile/runtime, expected): "
              + ", ".join(rust_accepts_orig_fails[:10])
              + (" ..." if len(rust_accepts_orig_fails) > 10 else ""))
    if mismatches:
        print(f"FAIL: {len(mismatches)} files rejected by the Rust parser "
              f"but run by the original:")
        for name in mismatches:
            print(f"  {name}")
        return 1
    print("PASS: every file the Rust parser rejects is also rejected "
          "by the original binary")
    return 0


if __name__ == "__main__":
    sys.exit(main())
