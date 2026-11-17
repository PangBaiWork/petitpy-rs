#!/usr/bin/env python3
"""Strong differential 2: Rust .mpy under petitpy-vm vs. original running .py.

Usage: python3 scripts/check_compile_run.py [--corpus DIR]

For every *.py file under the corpus dir (default:
../petitpy-orig/corpus/basics):
  - compile it with the Rust compiler (compile_mpy example) to a
    temporary .mpy (source name `<emit-mpy>`, same as the original's
    --emit-mpy output);
  - PRIMARY: run the original binary on the .py (`petitpy
    <stem>.py`, cwd=corpus dir) and the VM-only binary on the Rust
    .mpy (`petitpy-vm <abs .mpy>`, same cwd), and compare
    (rc, merged stdout+stderr) after filename normalisation;
  - CONTROL: also run the VM-only binary on the ORIGINAL's own
    .mpy (`petitpy --emit-mpy`, base64-decoded) and compare it
    against the Rust-.mpy run under the SAME runner.

Both sides run the SAME petitpy VM, so a byte-identical .mpy must
produce identical output — even for the 7 documented known-fails
(they fail against the *vendor* goldens, not against themselves).

Why the control arm exists — the two runners differ by design,
and the primary comparison alone cannot tell a compiler divergence
from a runner difference:
  1. petitpy-vm does NOT link the compiler: any test that calls
     eval()/exec()/compile() (or checks SyntaxError via compile(),
     or f-string evaluation) at runtime raises
     `NotImplementedError: compiler not linked in petitpy-vm`
     under petitpy-vm but passes under petitpy. (19 files.)
  2. Traceback routing differs: petitpy prints an uncaught
     traceback to stderr; petitpy-vm prints it to stdout. Hence
     stdout+stderr are merged before comparing.
  3. Traceback filenames differ: the .py run records `<stdin>`
     as the source name; the .mpy run records the .mpy's source
     name (`<emit-mpy>`). Both are normalised to the bare stem.
  4. `__file__` / `sys.path[0]` are set by the runner, not the
     .mpy: sys_path.py behaves differently under the two runners.

Classification of every PRIMARY diff:
  - CONTROL-MATCH: the control arm agrees (orig.mpy-vm ==
    rust.mpy-vm) — the primary diff is a runner/oracle artifact,
    not a compiler divergence. The detail names the cause
    (compiler-not-linked / traceback-stream / __file__).
  - REAL-DIFF: the control arm ALSO differs — a genuine compiler
    divergence. This is the only failing class.

Exit 0 only if there are zero REAL-DIFFs and zero Rust compile
failures; otherwise list every REAL-DIFF with its first differing
output line, and exit 1.
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
PETITPY_VM = ORIG / "zig-out" / "bin" / "petitpy-vm"
KNOWN_FAILS = ORIG / "corpus" / "full-known-fails.txt"
TIMEOUT_S = 15


def load_known_fails() -> set:
    """Active (non-comment, non-blank) entries of full-known-fails.txt."""
    known = set()
    for line in KNOWN_FAILS.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            known.add(line)
    return known


def run(cmd, cwd=None, timeout=TIMEOUT_S):
    try:
        p = subprocess.run(cmd, capture_output=True, cwd=cwd, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""


def normalise(data: bytes, stem: str, abspath_py: str, abspath_mpy: str) -> bytes:
    """Replace every spelling of the input file name with the bare stem.

    Longest first: absolute paths, then the runner-recorded pseudo
    names `<stdin>` (petitpy .py run) and `<emit-mpy>` (.mpy runs),
    then the suffixed basenames.
    """
    text = data.decode("utf-8", "replace")
    for old in (abspath_mpy, abspath_py, "<emit-mpy>", "<stdin>"):
        text = text.replace(old, stem)
    text = text.replace(stem + ".mpy", stem)
    text = text.replace(stem + ".py", stem)
    return text.encode("utf-8", "replace")


def merged(rc, out, err, stem, abspath_py, abspath_mpy):
    """(rc, stdout+stderr concatenated) after normalisation.

    The two runners route tracebacks to different streams (petitpy:
    stderr; petitpy-vm: stdout), so the streams are concatenated —
    the comparison is about program output, not runner plumbing.
    Concatenation reconstructs chronological order for these tests
    because all stdout prints happen during execution and the
    traceback, when any, is last on either stream.
    """
    return (rc, normalise(out + err, stem, abspath_py, abspath_mpy))


def first_diff_line(a: bytes, b: bytes) -> str:
    la = a.decode("utf-8", "replace").splitlines()
    lb = b.decode("utf-8", "replace").splitlines()
    for i, (x, y) in enumerate(zip(la, lb)):
        if x != y:
            return f"line {i}: orig={x!r} rust={y!r}"
    if len(la) != len(lb):
        return f"line count orig={len(la)} rust={len(lb)}"
    return "identical lines, different bytes"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=str(ORIG / "corpus" / "basics"))
    args = ap.parse_args()
    corpus = Path(args.corpus)

    if not corpus.is_dir():
        print(f"ERROR (infra): corpus dir not found: {corpus}")
        return 2
    for binpath in (PETITPY, PETITPY_VM):
        if not binpath.is_file():
            print(f"ERROR (infra): original binary not found: {binpath}")
            return 2

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

    known = load_known_fails()
    print(f"known-fails (active): {sorted(known)}")

    files = sorted(corpus.glob("*.py"))
    print(f"corpus: {len(files)} files (build {time.time() - t0:.1f}s)")

    matched = 0
    control_matched = 0
    control_diffs = []  # REAL-DIFFs: (name, klass, detail)
    oracle_diffs = []  # primary diffs explained by the control arm
    rust_compile_fail = []
    orig_fail_match = 0  # both sides fail identically

    with tempfile.TemporaryDirectory(prefix="check_compile_run_") as tmp:
        tmp = Path(tmp)
        for f in files:
            stem = f.stem
            abspath_py = str(f.resolve())
            rust_mpy = tmp / (stem + ".mpy")
            orig_mpy = tmp / ("orig_" + stem + ".mpy")

            rc, _out, err = run(
                [str(COMPILE_MPY), abspath_py, str(rust_mpy), "file", "<emit-mpy>"],
                cwd=corpus,
            )
            if rc != 0:
                rust_compile_fail.append(
                    (f.name, rc, err.decode("utf-8", "replace")[:200])
                )
                continue
            abspath_rust_mpy = str(rust_mpy.resolve())

            # Original's own .mpy, for the control arm.
            rc, out, err = run([str(PETITPY), "--emit-mpy", abspath_py])
            if rc != 0:
                rust_compile_fail.append(
                    (f.name, f"orig --emit-mpy rc={rc}", err.decode("utf-8", "replace")[:200])
                )
                continue
            try:
                orig_mpy.write_bytes(base64.b64decode(out))
            except Exception as e:
                rust_compile_fail.append((f.name, f"base64 decode: {e}", ""))
                continue
            abspath_orig_mpy = str(orig_mpy.resolve())

            rc_o, out_o, err_o = run([str(PETITPY), f.name], cwd=corpus)
            rc_r, out_r, err_r = run(
                [str(PETITPY_VM), abspath_rust_mpy], cwd=corpus
            )
            rc_c, out_c, err_c = run(
                [str(PETITPY_VM), abspath_orig_mpy], cwd=corpus
            )

            prim_o = merged(rc_o, out_o, err_o, stem, abspath_py, abspath_rust_mpy)
            prim_r = merged(rc_r, out_r, err_r, stem, abspath_py, abspath_rust_mpy)
            ctrl_c = merged(rc_c, out_c, err_c, stem, abspath_py, abspath_orig_mpy)
            ctrl_r = merged(rc_r, out_r, err_r, stem, abspath_py, abspath_rust_mpy)

            # Control arm: same runner (petitpy-vm), orig .mpy vs rust .mpy.
            # The two .mpy files differ only in nothing (byte-identical
            # per differential 1), so this must match; a mismatch here
            # is a genuine compiler divergence.
            if ctrl_c == ctrl_r:
                control_matched += 1
            else:
                klass = []
                if stem in known:
                    klass.append("KNOWN-FAIL")
                klass.append("ORIG-FAILS" if rc_o != 0 else "ORIG-PASS")
                if rc_c != rc_r:
                    detail = f"control rc orig-mpy={rc_c} rust-mpy={rc_r}"
                else:
                    detail = "control: " + first_diff_line(ctrl_c[1], ctrl_r[1])
                control_diffs.append((f.name, ",".join(klass), detail))

            if prim_o == prim_r:
                matched += 1
                if rc_o != 0:
                    orig_fail_match += 1
                continue

            # Primary diff — explain it from the control arm + output.
            cause = "RUNNER-ARTIFACT"
            blob = prim_r[1].decode("utf-8", "replace")
            if "compiler not linked in petitpy-vm" in blob:
                cause = "ORACLE-COMPILER-NOT-LINKED"
            elif "SKIP" in blob or "__file__" in blob or "sys.path" in blob:
                cause = "RUNNER-__file__/sys.path"
            if rc_o != rc_r:
                detail = f"rc orig={rc_o} rust={rc_r}"
            else:
                detail = first_diff_line(prim_o[1], prim_r[1])
            klass = ("KNOWN-FAIL," if stem in known else "") + (
                "ORIG-FAILS" if rc_o != 0 else "ORIG-PASS"
            )
            oracle_diffs.append((f.name, klass, cause, detail))

    total = len(files)
    print(f"primary matched: {matched}/{total} (of which both-fail-identically: {orig_fail_match})")
    print(f"control matched (orig.mpy-vm == rust.mpy-vm): {control_matched}/{total}")
    print(f"rust compile failures: {len(rust_compile_fail)}")
    for name, rc, detail in rust_compile_fail:
        print(f"  RUST-COMPILE-FAIL {name}: rc={rc} {detail}")
    print(f"REAL-DIFFs (control also differs): {len(control_diffs)}")
    for name, klass, detail in control_diffs:
        print(f"  REAL-DIFF [{klass}] {name}: {detail}")
    print(f"primary diffs explained by control arm (runner/oracle artifacts): {len(oracle_diffs)}")
    for name, klass, cause, detail in oracle_diffs:
        print(f"  ARTIFACT [{klass}] [{cause}] {name}: {detail}")

    ok = not control_diffs and not rust_compile_fail
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
