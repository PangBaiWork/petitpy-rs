#!/usr/bin/env python3
"""Generate differential test vectors for the petitpy-rs formatfloat port.

Oracle priority (see scripts/fmt_result.txt for the full story):
1. The reference binary ~/workspace/petitpy-orig/zig-out/bin/petitpy
   (`%` formatting / `format()`), which exercises mp_format_float
   through the real call path (mp_print_float, 36-byte buffer, so
   bufsize 33). repr()/str() are NOT oracles here: the Zig caller
   intercepts prec==99 + 'g' and uses its own shortest-round-trip
   formatter (formatFloatRepr), which does not call mp_format_float.
2. A direct C harness (scripts/charharness/, zig cc with the exact
   reference-build flags) calling the real mp_format_float compiled
   from the original vendor sources. Used for cases the binary cannot
   reach: prec==99 + 'g', and bufsize != 33. The harness is
   harness_ziglike: vendor C formatfloat.o linked against a C
   reimplementation of petitpy's Zig parsenum (parsenum_ziglike.c,
   strtod parse-back) — i.e. the LINKED behaviour of the reference
   binary. The pure-vendor-C harness (harness_zig) is NOT a valid
   oracle: the binary links the Zig parsenum, and the two disagree
   in the last digit on some high-precision cases (DBL_MAX '%.100f':
   vendor C ...3156, binary ...3158). harness_ziglike is verified
   to agree with the binary on all 2940 binary-reachable cases
   before the vectors are used (see scripts/fmt_result.txt).
   A gcc-compiled copy of the same C code is NOT a valid oracle: the
   (u64)(mantissa_f + 0.5) cast is UB in C for subnormal inputs and
   gcc resolves it to mantissa 0, while the reference build's clang
   emits vcvttsd2usi (mantissa u64::MAX).

CPython is NOT an oracle for this battery: the vendor algorithm is
only approximate (subnormals, last-digit rounding), so its output
differs from CPython by design. Values are constructed in CPython
only to pick the doubles; each value is fed to the binary as a repr
literal and the binary's own repr(x) is checked against that literal
(shortest repr is unique per double, so a match proves the binary
parsed the same double). Values that fail that check are dropped
and counted below.

Deterministic: random.seed(20261117). Output is written to
scripts/fmt_vectors.txt (one case per line, TAB-separated):
    bits_hex <TAB> fmt <TAB> prec <TAB> sign <TAB> bufsize <TAB> expected
sign is '0' (none), '+' or 'S' (space flag). expected is the exact
output bytes (may start with a space). The Rust probe
(src/vendor/probe_fmt.rs) reads it via include_str!.
Re-running this script must reproduce fmt_vectors.txt byte-identically.
"""
import math
import os
import random
import struct
import subprocess
import sys

SEED = 20261117
HERE = os.path.dirname(os.path.abspath(__file__))
WORKTREE = os.path.dirname(HERE)
VECTORS_PATH = os.path.join(HERE, "fmt_vectors.txt")
PETITPY = os.path.expanduser("~/workspace/petitpy-orig/zig-out/bin/petitpy")
HARNESS = "/tmp/charness/harness_ziglike"
BUILD_HARNESS = os.path.join(HERE, "charharness", "build_charharness.sh")


def bits_of(f):
    # Big-endian integer hex of the f64 bit pattern, so the Rust probe
    # (u64::from_str_radix + f64::from_bits) and the C harness
    # (sscanf %llx + memcpy) recover exactly this double.
    return f"{struct.unpack('<Q', struct.pack('<d', f))[0]:016x}"


def f_of_bits(bits_hex):
    return struct.unpack("<d", bytes.fromhex(bits_hex))[0]


# (spec, fmt, prec, sign) — spec is the petitpy '%' format string.
# prec -1 means "not specified" (C default 6).
BIN_FORMATS = [
    ("%f", "f", -1, "0"),
    ("%.0f", "f", 0, "0"),
    ("%.1f", "f", 1, "0"),
    ("%.2f", "f", 2, "0"),
    ("%.6f", "f", 6, "0"),
    ("%.10f", "f", 10, "0"),
    ("%.17f", "f", 17, "0"),
    ("%.20f", "f", 20, "0"),
    ("%.99f", "f", 99, "0"),
    ("%+f", "f", -1, "+"),
    ("% f", "f", -1, "S"),
    ("%+.0f", "f", 0, "+"),
    ("%e", "e", -1, "0"),
    ("%.0e", "e", 0, "0"),
    ("%.1e", "e", 1, "0"),
    ("%.6e", "e", 6, "0"),
    ("%.17e", "e", 17, "0"),
    ("%.99e", "e", 99, "0"),
    ("%E", "E", -1, "0"),
    ("%+.6E", "E", 6, "+"),
    ("%g", "g", -1, "0"),
    ("%.0g", "g", 0, "0"),
    ("%.1g", "g", 1, "0"),
    ("%.6g", "g", 6, "0"),
    ("%.17g", "g", 17, "0"),
    ("%G", "G", -1, "0"),
    ("%+.6g", "g", 6, "+"),
    ("%.100f", "f", 100, "0"),
]

# (fmt, prec, sign, bufsize) — harness-only cases.
HARNESS_CASES = []
for _fmt, _prec in [("g", 99), ("G", 99)]:
    HARNESS_CASES.append((_fmt, _prec, "0", 33))
for _buf in [29, 30, 33, 36, 64, 128]:
    HARNESS_CASES.append(("f", 6, "0", _buf))
    HARNESS_CASES.append(("g", 6, "0", _buf))
    HARNESS_CASES.append(("e", 6, "0", _buf))
for _buf in [29, 33, 64, 128, 512]:
    HARNESS_CASES.append(("f", 100, "0", _buf))
# No sign cases here: the binary already covers '+' / ' ' at
# bufsize 33 (sign handling is a one-char prefix, independent of
# bufsize). (The harness now accepts 'S' for a space sign, but the
# coverage is not needed.)


def value_pool():
    rng = random.Random(SEED)
    vals = [
        0.0, -0.0, float("inf"), float("-inf"), float("nan"),
        float("-nan"),
        1.0, -1.0, 1.5, -1.5, 2.5, 3.5, 0.5, 1.25, 0.1, 0.2, 0.3,
        1.0 / 3.0, 2.0 / 3.0, 22.0 / 7.0, 355.0 / 113.0,
        math.sqrt(2.0), math.pi, math.e,
        1e15, 1e16, 1e17, 123456.0, 1234567.0, 100000.0, 1000000.0,
        0.0001, 0.00001, 0.000123, 0.0000123,
        123456789.123456789, 999.999, 9.999, 123.456, 0.0099, 0.099,
        1.0000000000000002, 9007199254740993.0, 0.30000000000000004,
        1e300, 1.7976931348623157e308, 1e-300, 1e-308,
        2.2250738585072014e-308,  # min normal
        5e-324, 1e-323, 1e-320, 2.5e-324,  # subnormals
        1234567890123456.0, 0.9999999999999999, 2.0,
    ]
    # Random finite doubles from bit patterns (mixed magnitudes,
    # including subnormals and near-boundary values).
    for _ in range(40):
        b = rng.getrandbits(64)
        # Force a finite double: clear the exponent's top bits
        # sometimes, keep the raw pattern otherwise (inf/nan filtered).
        f = struct.unpack("<d", struct.pack("<Q", b))[0]
        if math.isfinite(f):
            vals.append(f)
    # Random fractions (exact IEEE division in CPython and petitpy).
    for _ in range(10):
        p = rng.randint(-10**9, 10**9)
        q = rng.randint(1, 10**6)
        vals.append(p / q)
    # De-duplicate by bits, preserving order.
    seen = set()
    out = []
    for v in vals:
        b = bits_of(v)
        if b not in seen:
            seen.add(b)
            out.append(v)
    return out


def literal(v):
    if math.isnan(v):
        return "float('-nan')" if math.copysign(1.0, v) < 0 else "float('nan')"
    if math.isinf(v):
        return "float('-inf')" if v < 0 else "float('inf')"
    return repr(v)


def run_binary_oracle(vals):
    """Run one petitpy script; return {case_id: output} and {vi: repr}."""
    lines = []
    for vi, v in enumerate(vals):
        lines.append(f"x = {literal(v)}")
        lines.append(f"print('R{vi}\\t' + repr(x))")
        for fi, (spec, _fmt, _prec, _sign) in enumerate(BIN_FORMATS):
            lines.append(f"print('{vi}:{fi}\\t' + ('{spec}' % x))")
    script = "\n".join(lines) + "\n"
    r = subprocess.run(
        [PETITPY, "/dev/stdin"], input=script, capture_output=True, text=True
    )
    if r.returncode != 0:
        sys.exit(f"petitpy oracle failed rc={r.returncode}:\n{r.stderr[:2000]}")
    out = {}
    reprs = {}
    for line in r.stdout.split("\n"):
        if "\t" not in line:
            continue
        key, val = line.split("\t", 1)
        if key.startswith("R"):
            reprs[int(key[1:])] = val
        else:
            out[key] = val
    return out, reprs


def ensure_harness():
    if not os.path.exists(HARNESS):
        print("harness missing, building it ...")
        subprocess.run([BUILD_HARNESS], check=True)


def run_harness_oracle(cases):
    """cases: list of (bits_hex, fmt, prec, sign, bufsize). Returns
    list of (bits_hex, fmt, prec, sign, bufsize, expected)."""
    ensure_harness()
    inp = "".join(
        f"{b} {f} {p} {'0' if s == '0' else s} {n}\n"
        for (b, f, p, s, n) in cases
    )
    r = subprocess.run([HARNESS], input=inp, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"harness failed rc={r.returncode}:\n{r.stderr[:2000]}")
    res = []
    hlines = r.stdout.split("\n")
    assert len([h for h in hlines if h]) == len(cases), (
        f"harness returned {len(hlines)} lines for {len(cases)} cases"
    )
    for case, line in zip(cases, hlines):
        if not line:
            continue
        length, _, expected = line.partition(" ")
        assert int(length) == len(expected), (case, line)
        res.append((*case, expected))
    return res


def main():
    vals = value_pool()
    bin_out, reprs = run_binary_oracle(vals)

    lines = []
    n_binary = 0
    n_dropped = 0
    for vi, v in enumerate(vals):
        # Repr round-trip check: proves the binary parsed the same
        # double this generator picked. Compare against CPython's
        # repr (not the constructor expression used for inf/nan).
        if reprs.get(vi) != repr(v):
            # repr() of a float literal in petitpy uses its own
            # shortest-round-trip formatter; a mismatch means the
            # parse landed on a different double (parse divergence,
            # out of scope for this lane) — drop the value.
            n_dropped += 1
            continue
        b = bits_of(v)
        for fi, (_spec, fmt, prec, sign) in enumerate(BIN_FORMATS):
            key = f"{vi}:{fi}"
            assert key in bin_out, f"missing oracle output for {key}"
            expected = bin_out[key]
            lines.append(f"{b}\t{fmt}\t{prec}\t{sign}\t33\t{expected}")
            n_binary += 1

    # Harness-only cases: prec 99 'g' (binary intercepts it for repr)
    # and bufsize != 33, for a spread of values.
    harness_cases = []
    harness_vals = [
        0.0, -0.0, 1.5, -1.5, math.sqrt(2.0), math.pi, 123456789.123456789,
        1e17, 1e16, 0.00001, 1e300, 1e-300, 5e-324, 1e-320,
        2.2250738585072014e-308, 1.7976931348623157e308, float("inf"),
        float("nan"), 0.0099, 1.0000000000000002,
    ]
    for v in harness_vals:
        b = bits_of(v)
        for (fmt, prec, sign, bufsize) in HARNESS_CASES:
            # prec 99 'g' is the interesting one; bufsize cases use
            # prec 6/100 which the binary also covers at 33, but the
            # harness is the only oracle for other sizes.
            harness_cases.append((b, fmt, prec, sign, bufsize))
    # De-duplicate harness cases (the (g,99) ones repeat per value set).
    harness_cases = list(dict.fromkeys(harness_cases))
    hres = run_harness_oracle(harness_cases)
    n_harness = 0
    for (b, fmt, prec, sign, bufsize, expected) in hres:
        lines.append(f"{b}\t{fmt}\t{prec}\t{sign}\t{bufsize}\t{expected}")
        n_harness += 1

    header = (
        f"# fmt_vectors.txt — deterministic, seed {SEED}\n"
        f"# binary-oracle cases: {n_binary}, harness-oracle cases: {n_harness}, "
        f"dropped values (repr/parse mismatch): {n_dropped}\n"
        "# fields: bits_hex <TAB> fmt <TAB> prec <TAB> sign <TAB> bufsize <TAB> expected\n"
        "# sign: '0' none, '+' plus, 'S' space\n"
    )
    with open(VECTORS_PATH, "w") as fh:
        fh.write(header)
        fh.write("\n".join(lines) + "\n")
    print(f"wrote {len(lines)} cases to {VECTORS_PATH}")
    print(f"binary-oracle: {n_binary}, harness-oracle: {n_harness}, "
          f"dropped: {n_dropped}")


if __name__ == "__main__":
    main()
