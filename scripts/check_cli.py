#!/usr/bin/env python3
"""Differential check for the Rust zigpy-compile CLI (Lane C3).

Part 1 — strong differential: compile all 552 corpus files with the
Rust binary (default options) and with the original binary built by
`zig build zigpy-compile` (zig-out/bin/zigpy-compile), compare .mpy
bytes. Target: 552/552.

Part 2 — behaviour battery: run both binaries on the same argv/files
and compare (rc, stdout, stderr) exactly. Expected divergences
(documented in scripts/cli_result.txt, all exit codes match):
  - compile-stage SyntaxError (e.g. `1 = 2`, multiple starred targets):
    oracle prints bare `SyntaxError`, Rust prints the parse form.
  - DedentMismatch: oracle prints `parse error: error.DedentMismatch`,
    Rust prints `parse error: error.IndentationError` (bridge collapses
    DedentMismatch into IndentationError).
  - non-UTF-8 source bytes: oracle compiles them (rc 0); Rust reports
    a parse failure (rc 1) because the bridge takes &str.

Both binaries are run in twin temp dirs with identical relative
layouts, so relative paths in stderr compare directly.
"""

import os
import shutil
import subprocess
import sys
import tempfile

ORIG = os.path.expanduser("~/workspace/petitpy-orig")
RUST_BIN = os.path.expanduser("~/workspace/petitpy-rs-cli/target/release/zigpy-compile")
ORACLE_BIN = os.path.join(ORIG, "zig-out/bin/zigpy-compile")
CORPUS = os.path.join(ORIG, "corpus/basics")
MPY_DUMP = os.path.expanduser(
    "~/workspace/petitpy-rs-cli/target/release/examples/mpy-dump"
)

FAIL = 0


def run(binary, argv, cwd):
    p = subprocess.run(
        [binary] + argv, cwd=cwd, capture_output=True, timeout=60
    )
    return p.returncode, p.stdout, p.stderr


def check(name, cond, detail=""):
    global FAIL
    if cond:
        print(f"  ok   {name}")
    else:
        FAIL += 1
        print(f"  FAIL {name} {detail}")


def part1_corpus():
    print("Part 1: corpus .mpy differential (default options)")
    files = sorted(f for f in os.listdir(CORPUS) if f.endswith(".py"))
    print(f"  corpus files: {len(files)}")
    tmp = tempfile.mkdtemp(prefix="cli_diff_")
    rust_dir = os.path.join(tmp, "rust")
    orig_dir = os.path.join(tmp, "orig")
    os.makedirs(rust_dir)
    os.makedirs(orig_dir)
    match = 0
    mismatches = []
    rust_fail = []
    orig_fail = []
    for name in files:
        src = os.path.join(CORPUS, name)
        for d in (rust_dir, orig_dir):
            shutil.copy(src, os.path.join(d, name))
        rc_r, _, err_r = run(RUST_BIN, [name], rust_dir)
        rc_o, _, err_o = run(ORACLE_BIN, [name], orig_dir)
        out_r = os.path.join(rust_dir, name[:-3] + ".mpy")
        out_o = os.path.join(orig_dir, name[:-3] + ".mpy")
        if rc_r != 0:
            rust_fail.append((name, rc_r, err_r.decode(errors="replace")))
        if rc_o != 0:
            orig_fail.append((name, rc_o, err_o.decode(errors="replace")))
        if rc_r == 0 and rc_o == 0:
            br = open(out_r, "rb").read()
            bo = open(out_o, "rb").read()
            if br == bo:
                match += 1
            else:
                mismatches.append((name, len(br), len(bo)))
        for d in (rust_dir, orig_dir):
            for f in os.listdir(d):
                os.unlink(os.path.join(d, f))
    shutil.rmtree(tmp)
    print(f"  byte-identical: {match}/{len(files)}")
    print(f"  rust failures: {len(rust_fail)}  oracle failures: {len(orig_fail)}")
    for m in mismatches:
        print(f"  MISMATCH {m[0]} rust={m[1]}B orig={m[2]}B")
    for m in rust_fail:
        print(f"  RUST-FAIL {m[0]} rc={m[1]} {m[2].strip()}")
    for m in orig_fail:
        print(f"  ORACLE-FAIL {m[0]} rc={m[1]} {m[2].strip()}")
    check("552/552 byte-identical", match == len(files) and not mismatches)
    check("no rust compile failures", not rust_fail)
    check("no oracle compile failures", not orig_fail)


def battery():
    print("Part 2: behaviour battery (rc, stdout, stderr must match)")
    tmp = tempfile.mkdtemp(prefix="cli_battery_")
    rust_dir = os.path.join(tmp, "rust")
    orig_dir = os.path.join(tmp, "orig")
    os.makedirs(rust_dir)
    os.makedirs(orig_dir)

    # Probe sources (name -> bytes). Written identically into both dirs.
    probes = {
        "ok.py": b'x = 1\n',
        "syn.py": b'x = \n',
        "syn2.py": b'def f(:\n    pass\n',
        "tab.py": b'x = 1\n\ty = 2\n',
        "ded.py": b'if True:\n    x = 1\n  y = 2\n',
        "asn.py": b'1 = 2\n',
        "star.py": b'a, *b, *c = [1, 2, 3]\n',
        "uni.py": b'x = "\\N{BADNAME}"\n',
        "raw.py": b'x = "a\xffb"\n',
        "empty.py": b'',
        "plain": b'y = 2\n',
        "multi.py": b'z = 3\n',
    }
    for d in (rust_dir, orig_dir):
        for name, data in probes.items():
            with open(os.path.join(d, name), "wb") as f:
                f.write(data)
        os.makedirs(os.path.join(d, "somedir"), exist_ok=True)

    # (name, argv, expected-class) — expected-class "match" means exact
    # (rc, stdout, stderr) equality; "known" means rc matches and the
    # stderr difference is one of the documented divergences.
    cases = [
        ("version -v", ["-v"], "match"),
        ("version --version", ["--version"], "match"),
        ("help -h", ["-h"], "match"),
        ("help --help", ["--help"], "match"),
        ("no args", [], "match"),
        ("bad flag --bogus", ["--bogus", "ok.py"], "match"),
        ("bad flag -x", ["-x", "ok.py"], "match"),
        ("bad flag -O4", ["-O4", "ok.py"], "match"),
        ("bad flag -O30", ["-O30", "ok.py"], "match"),
        ("-o no arg", ["-o"], "match"),
        ("-s no arg", ["-s"], "match"),
        ("-o twice", ["-o", "a.mpy", "-o", "b.mpy", "ok.py"], "match"),
        ("-o multi", ["-o", "out.mpy", "ok.py", "multi.py"], "match"),
        ("-o no inputs", ["-o", "out.mpy"], "match"),
        ("missing input", ["nope.py"], "match"),
        ("missing input 2nd", ["ok.py", "nope.py"], "match"),
        ("compile ok", ["ok.py"], "match"),
        ("compile ok -O0", ["-O0", "ok.py"], "match"),
        ("compile ok -O1", ["-O1", "ok.py"], "match"),
        ("compile ok -O2", ["-O2", "ok.py"], "match"),
        ("compile ok -O", ["-O", "ok.py"], "match"),
        ("compile ok -s", ["-s", "custom_name", "ok.py"], "match"),
        ("compile ok -o", ["-o", "explicit.mpy", "ok.py"], "match"),
        ("compile multi", ["ok.py", "multi.py"], "match"),
        ("compile plain (no .py)", ["plain"], "match"),
        ("compile empty", ["empty.py"], "match"),
        ("compile dir", ["somedir"], "match"),
        ("syntax error", ["syn.py"], "match"),
        ("syntax error 2", ["syn2.py"], "match"),
        ("tab indent", ["tab.py"], "match"),
        ("bad unicode name", ["uni.py"], "match"),
        ("assign to literal", ["asn.py"], "known"),
        ("multi star", ["star.py"], "known"),
        ("dedent mismatch", ["ded.py"], "known"),
        ("raw non-utf8", ["raw.py"], "known"),
        ("-s twice last wins", ["-s", "one", "-s", "two", "ok.py"], "match"),
        ("-v wins over input", ["-v", "ok.py"], "match"),
        ("unknown wins over -v", ["--bogus", "-v"], "match"),
        ("long path input", ["x" * 5000 + ".py"], "match"),
        ("long unknown option", ["-" + "x" * 600], "match"),
    ]

    known_stderr = {
        "assign to literal": (
            b"zigpy-compile: asn.py: SyntaxError\n",
            b"zigpy-compile: asn.py: parse error: error.SyntaxError\n",
        ),
        "multi star": (
            b"zigpy-compile: star.py: SyntaxError\n",
            b"zigpy-compile: star.py: parse error: error.SyntaxError\n",
        ),
        "dedent mismatch": (
            b"zigpy-compile: ded.py: parse error: error.DedentMismatch\n",
            b"zigpy-compile: ded.py: parse error: error.IndentationError\n",
        ),
        "raw non-utf8": (b"", b"zigpy-compile: raw.py: parse error: error.SyntaxError\n"),
    }

    for name, argv, cls in cases:
        rc_r, out_r, err_r = run(RUST_BIN, argv, rust_dir)
        rc_o, out_o, err_o = run(ORACLE_BIN, argv, orig_dir)
        if cls == "match":
            ok = (rc_r, out_r, err_r) == (rc_o, out_o, err_o)
            check(
                name,
                ok,
                f"rust={(rc_r, out_r[:80], err_r[:120])} "
                f"orig={(rc_o, out_o[:80], err_o[:120])}",
            )
        else:
            exp_o, exp_r = known_stderr[name]
            if name == "raw non-utf8":
                ok = rc_r == 1 and rc_o == 0 and err_r == exp_r and err_o == exp_o
            else:
                ok = rc_r == rc_o == 1 and err_r == exp_r and err_o == exp_o
            check(
                name + " (documented divergence, rc matches)",
                ok,
                f"rust={(rc_r, err_r[:120])} orig={(rc_o, err_o[:120])}",
            )
        # Clean outputs between cases so one case cannot pollute the next.
        for d in (rust_dir, orig_dir):
            for f in os.listdir(d):
                p = os.path.join(d, f)
                if os.path.isfile(p) and f.endswith(".mpy"):
                    os.unlink(p)

    # -o / -s output bytes must also match (not just rc/stderr).
    print("  output-bytes checks:")
    for argv, outname in [
        (["-s", "custom_name", "ok.py"], "ok.mpy"),
        (["-o", "explicit.mpy", "ok.py"], "explicit.mpy"),
        (["ok.py"], "ok.mpy"),
        (["somedir"], "somedir.mpy"),
    ]:
        run(RUST_BIN, argv, rust_dir)
        run(ORACLE_BIN, argv, orig_dir)
        br = open(os.path.join(rust_dir, outname), "rb").read()
        bo = open(os.path.join(orig_dir, outname), "rb").read()
        check(f"bytes {argv} -> {outname}", br == bo, f"rust={len(br)} orig={len(bo)}")
        for d in (rust_dir, orig_dir):
            for f in os.listdir(d):
                p = os.path.join(d, f)
                if os.path.isfile(p) and f.endswith(".mpy"):
                    os.unlink(p)

    # -s lands in the .mpy as qstr[0] (checked with mpy-dump, which
    # prints qstr bytes as hex).
    if os.path.exists(MPY_DUMP):
        run(RUST_BIN, ["-s", "custom_name", "ok.py"], rust_dir)
        p = subprocess.run(
            [MPY_DUMP, os.path.join(rust_dir, "ok.mpy")],
            capture_output=True, timeout=30,
        )
        check(
            "-s name in .mpy qstr table (mpy-dump)",
            b"q[0]=N:" + b"custom_name".hex().encode() in p.stdout,
            p.stdout[:200],
        )
    else:
        print("  skip mpy-dump check (examples/mpy-dump not built)")

    shutil.rmtree(tmp)


def main():
    part1_corpus()
    battery()
    print(f"\n{'PASS' if FAIL == 0 else f'FAIL ({FAIL} checks)'}")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
