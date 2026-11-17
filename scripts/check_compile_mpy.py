#!/usr/bin/env python3
"""Strong differential 1: Rust compiler .mpy vs. original `petitpy --emit-mpy`.

Usage: python3 scripts/check_compile_mpy.py [--corpus DIR]

For every *.py file under the corpus dir (default:
../petitpy-orig/corpus/basics):
  - run the original binary (`petitpy --emit-mpy <abs path>`,
    base64 on stdout) and decode to .mpy bytes;
  - run the Rust compiler (`cargo build --release --example
    compile_mpy`, built once up front) on the same absolute path
    and read the .mpy bytes it writes;
  - compare byte-for-byte.

The same absolute path is passed to both for reading, but the
source *name* recorded in the .mpy differs: the original hardcodes
`<emit-mpy>` as the source name (verified by dumping its output),
so the Rust side is invoked with that same literal name.

Acceptance: 552/552 byte-identical. Any compile failure on either
side, or any byte mismatch, is listed with size + first-diff offset,
and the script exits 1.
"""

import argparse
import base64
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORIG = ROOT.parent / "petitpy-orig"
COMPILE_MPY = ROOT / "target" / "release" / "examples" / "compile_mpy"
PETITPY = ORIG / "zig-out" / "bin" / "petitpy"
TIMEOUT_S = 30


def run(cmd, timeout=TIMEOUT_S):
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""


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

    # Build the Rust compiler CLI once.
    t0 = time.time()
    p = subprocess.run(
        ["cargo", "build", "--release", "--example", "compile_mpy"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if p.returncode != 0:
        print("ERROR (infra): cargo build --example compile_mpy failed:")
        print(p.stdout)
        print(p.stderr)
        return 2
    if not COMPILE_MPY.is_file():
        print(f"ERROR (infra): compile_mpy binary not found: {COMPILE_MPY}")
        return 2

    files = sorted(corpus.glob("*.py"))
    print(f"corpus: {len(files)} files (build {time.time() - t0:.1f}s)")

    identical = 0
    mismatches = []  # (name, reason, detail)
    orig_fail = []
    rust_fail = []

    with tempfile.TemporaryDirectory(prefix="check_compile_mpy_") as tmp:
        tmp = Path(tmp)
        for f in files:
            abspath = str(f.resolve())
            # Original: --emit-mpy prints base64 .mpy on stdout.
            rc, out, err = run([str(PETITPY), "--emit-mpy", abspath])
            if rc != 0:
                orig_fail.append((f.name, rc, err.decode("utf-8", "replace")[:200]))
                continue
            try:
                orig_mpy = base64.b64decode(out)
            except Exception as e:
                orig_fail.append((f.name, f"base64 decode: {e}", out[:200]))
                continue
            # Rust: compile_mpy writes the .mpy file. The source name
            # must be the literal `<emit-mpy>` — that is what the
            # original records, not the file path.
            out_mpy = tmp / (f.stem + ".mpy")
            rc, _out, err = run(
                [str(COMPILE_MPY), abspath, str(out_mpy), "file", "<emit-mpy>"]
            )
            if rc != 0:
                rust_fail.append((f.name, rc, err.decode("utf-8", "replace")[:200]))
                continue
            rust_mpy = out_mpy.read_bytes()
            if rust_mpy == orig_mpy:
                identical += 1
            else:
                # First differing offset, for the report.
                n = min(len(rust_mpy), len(orig_mpy))
                off = next(
                    (i for i in range(n) if rust_mpy[i] != orig_mpy[i]),
                    n,
                )
                mismatches.append(
                    (
                        f.name,
                        f"len rust={len(rust_mpy)} orig={len(orig_mpy)} first-diff@{off}",
                        "",
                    )
                )

    total = len(files)
    print(f"identical: {identical}/{total}")
    print(f"orig --emit-mpy failures: {len(orig_fail)}")
    for name, rc, detail in orig_fail:
        print(f"  ORIG-FAIL {name}: rc={rc} {detail}")
    print(f"rust compile failures: {len(rust_fail)}")
    for name, rc, detail in rust_fail:
        print(f"  RUST-FAIL {name}: rc={rc} {detail}")
    print(f"byte mismatches: {len(mismatches)}")
    for name, reason, _ in mismatches:
        print(f"  MISMATCH {name}: {reason}")

    ok = identical == total and not orig_fail and not rust_fail and not mismatches
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
