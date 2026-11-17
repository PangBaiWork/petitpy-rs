#!/usr/bin/env python3
"""Generate differential test vectors for the petitpy-rs re1.5 port.

Oracles (priority order):
  1. The PATCHED C engine itself, via a standalone harness built from
     read-only copies of `port/re1.5-patched/*` in /tmp/re15_c_harness
     (gcc, one TU, like modre.c includes the engine). Used for `dump`
     cases (the reference binary does not compile dumpcode:
     MICROPY_PY_RE_DEBUG is off) and for the sizecode verdict.
  2. The reference binary's `ure` module (the C engine + modre.c,
     i.e. `~/workspace/petitpy-orig/zig-out/bin/petitpy`). Used for
     search/match/split/sub/compile-error cases. NOTE: `import re` in
     the binary is petitpy-lib/re.py, a Python shim that REWRITES
     patterns (brace expansion, IGNORECASE widening, unbalanced-paren
     repair) before handing them to `ure` — it is NOT the engine, so
     the oracle is `ure` directly. `re.py` is a separate layer (not
     this lane) and its preprocessing is out of scope here.
  3. CPython's `re` (bytes patterns) runs as a CROSS-CHECK only: where
     it disagrees with `ure`, the case is recorded in the divergences
     report (printed at the end) and the vector keeps ure's answer.
     Never the other way round.

Deterministic: the case list is static and ordered; the random cases
use random.Random(SEED). Re-running this script must produce a
byte-identical vectors file (checked by diff).

Vector format (one case per line, space-separated; byte strings are
lowercase hex):
  dump <pat_hex> <size> <dump_hex>
  search <pat_hex> <subj_hex> MATCH <s0> <e0> [<s1> <e1> ...] | NOMATCH
  match  <pat_hex> <subj_hex> MATCH ... | NOMATCH
  matchpos <m|s> <pat_hex> <subj_hex> <pos|-> <endpos|-> MATCH ... | NOMATCH
  split <pat_hex> <subj_hex> <maxsplit> SPLIT <n> <piece_hex> ... | SPLIT_ERR <type>
  sub <pat_hex> <subj_hex> <repl_hex> <count> SUB <result_hex> | SUB_ERR <type>
  compileerr <pat_hex> <size|NONE>
"""

import random
import subprocess
import sys

SEED = 20261117
BIN = "/home/hatch/workspace/petitpy-orig/zig-out/bin/petitpy"
HARNESS = "/tmp/re15_c_harness/harness"
OUT_PATH = "scripts/re15_vectors.txt"
CHUNK = 40  # cases per oracle-driver file (isolates a bad case)

# ---------------------------------------------------------------------------
# Case lists (patterns/subjects are bytes; ure works on bytes and str
# identically at the engine level, and bytes keep span() in byte
# offsets — modre.c converts str spans to char indices, which is
# Phase D's concern, not the engine's).
# ---------------------------------------------------------------------------

DUMP_PATTERNS = [
    b"", b"a", b"ab", b".", b"..",
    b"[a]", b"[^a]", b"[a-z]", b"[^a-z]", b"[a-cx-z]",
    b"\\d", b"\\D", b"\\s", b"\\S", b"\\w", b"\\W",
    b"[\\d]", b"[^\\D]", b"[\\s\\d]",
    b"^", b"$", b"^a$", b"^a|b$",
    b"a|b", b"a|b|c", b"a|", b"|a",
    b"a?", b"a*", b"a+", b"a??", b"a*?", b"a+?",
    b"(a)", b"(?:a)", b"(a)(b)", b"((a))", b"(a|b)*", b"a(b|c)d",
    b"[-a]", b"[a-]", b"[\\-a]", b"a\\.b", b"\\\\",
    b"(a+)(a+)", b"^a|b[0-9]\\w$",
    b"x" * 150 + b"|y", b"(a)" * 10, b"a" * 200, b"[a-z][0-9]*",
    b"(a|ab)(c|bcd)", b"\\w+\\s+\\w+",
]

# (op, pattern, subject); op is "search" or "match". Mined from the
# upstream MicroPython tests/extmod/re*.py plus systematic coverage.
SEARCH_MATCH_CASES = [
    # re1.py
    ("match", b".+", b"abc"),
    ("match", b"(.+)1", b"xyz781"),
    ("match", b"[a-cu-z]", b"a"),
    ("match", b"[a-cu-z]", b"z"),
    ("match", b"[a-cu-z]", b"d"),
    ("match", b"[a-cu-z]", b"A"),
    ("match", b"[^a-cu-z]", b"a"),
    ("match", b"[^a-cu-z]", b"z"),
    ("match", b"[^a-cu-z]", b"d"),
    ("match", b"[^a-cu-z]", b"A"),
    ("match", b"[-a]+", b"-a]d"),
    ("match", b"[a-]+", b"-a]d"),
    ("search", b"o+", b"foobar"),
    ("match", b".*", b"foo"),
    ("search", b"w.r", b"hello world"),
    ("match", b"a+?", b"ab"),
    ("match", b"a*?", b"ab"),
    ("match", b"^ab$", b"ab"),
    ("match", b"a|b", b"b"),
    ("match", b"a|b|c", b"c"),
    ("search", b"^b|b$", b"abc"),
    ("match", b"a+?", b"ab"),
    ("match", b"a\\.c", b"a.c"),
    ("match", b"a\\.b", b"abc"),
    ("match", b"a\\.b", b"a\\bc"),
    ("match", b"[a\\-z]", b"abc"),
    ("match", b"[.\\]]*", b".].]a"),
    ("match", b"[.\\]+]*", b".]+.]a"),
    ("match", b"[a-f0-9x\\-yz]*", b"abxcd1-23"),
    ("match", b"[a\\\\b]*", b"a\\aa\\bb\\bbab"),
    ("search", b"[a\\-z]", b"-"),
    ("search", b"[a\\-z]", b"f"),
    ("search", b"[a\\]z]", b"a"),
    ("match", b".", b"foo"),
    ("match", b".", b"foo"),
    ("search", b".", b"foo"),
    # re_group.py / re_namedclass.py / re_span.py
    ("match", b"(([0-9]*)([a-z]*)[0-9]*)", b"1234hello567"),
    ("match", b"([0-9]*)(([a-z]*)([0-9]*))", b"1234hello567"),
    ("match", b"(a)?b(c)", b"abc"),
    ("match", b"(a)?b(c)", b"bc"),
    ("match", b"\\w+", b"1234hello567 abc"),
    ("match", b"(\\w+)\\s+(\\w+)", b"ABC \t1234hello567 abc"),
    ("match", b"(\\S+)\\s+(\\D+)", b"ABC \thello abc567 abc"),
    ("match", b"(([0-9]*)([a-z]*)\\d*)", b"1234hello567"),
    ("match", b"([^\\s]+)\\s*([^\\s]+)", b"1 23"),
    ("match", b"([\\s\\d]+)([\\W]+)", b"1  2-+="),
    ("match", b"([\\W]+)([^\\W]+)([^\\S]+)([^\\D]+)", b" a_1 23"),
    # backtracking / laziness
    ("match", b"(a+)(a+)", b"aaa"),
    ("match", b"(a|ab)(c|bcd)", b"abcd"),
    ("match", b"(a+)+", b"aaaa"),
    ("match", b"(a|b)*", b"abba"),
    ("match", b"(ab)*", b"ababab"),
    ("match", b"(ab)*", b"aba"),
    ("search", b"a+", b"banana"),
    ("search", b"an+", b"banana"),
    ("search", b"na", b"banana"),
    ("match", b"a+?b", b"aab"),
    ("match", b".*?b", b"abc"),
    ("match", b"<.*?>", b"<a><b>"),
    ("search", b"<.*?>", b"x<a>y"),
    ("match", b"a??b", b"ab"),
    ("match", b"a*?b", b"aab"),
    ("match", b"a??", b"aaa"),
    # anchors
    ("match", b"^", b""),
    ("match", b"^", b"a"),
    ("match", b"$", b""),
    ("match", b"^$", b""),
    ("match", b"^$", b"a"),
    ("search", b"^", b"abc"),
    ("search", b"$", b"abc"),
    ("search", b"b$", b"ab"),
    ("search", b"^a", b"ba"),
    ("match", b"^a|^b", b"b"),
    ("match", b"a$|b$", b"b"),
    # classes
    ("match", b"[abc]+", b"abca"),
    ("match", b"[abc]+", b"abx"),
    ("search", b"[^abc]+", b"xyzabc"),
    ("match", b"[a-zA-Z_][a-zA-Z_0-9]*", b"_foo1"),
    ("match", b"[0-9][0-9]*", b"123abc"),
    ("search", b"[0-9]+", b"ab123cd45"),
    ("match", b"[\\]]+", b"]]"),
    ("match", b"[\\[]", b"["),
    ("match", b"[]]", b"]"),
    ("match", b"[^]]", b"a"),
    ("match", b"[^]]", b"]"),
    # named classes
    ("search", b"\\d+", b"ab123"),
    ("search", b"\\D+", b"123ab"),
    ("search", b"\\w+", b"hi there"),
    ("search", b"\\W+", b"ab--cd"),
    ("search", b"\\s", b"a b"),
    ("search", b"\\S+", b" a b "),
    ("match", b"\\d\\d", b"12"),
    ("match", b"\\d\\d", b"1a"),
    # dot (Any matches every byte, including newline)
    ("match", b"a.c", b"abc"),
    ("match", b"a.c", b"a\nc"),
    ("search", b".", b"\n"),
    ("match", b"..", b"ab"),
    # empty pattern / subject
    ("match", b"", b""),
    ("match", b"", b"abc"),
    ("search", b"", b"abc"),
    ("match", b"a*", b""),
    ("match", b"a+", b""),
    ("search", b"x*", b"foo"),
    ("match", b"()", b""),
    ("match", b"(a)?", b""),
    ("match", b"(a)?", b"a"),
    ("search", b"b*", b"aaa"),
    # high bytes (signed-char class ranges)
    ("match", b"[\x80-\xff]+", b"\x80\xff"),
    ("match", b"[a-z]", b"\x80"),
    ("match", b".", b"\xff"),
    ("search", b"\xff", b"a\xffb"),
    ("match", b"\\w", b"\x80"),
    ("match", b"\\D", b"\x80"),
    ("search", b"[\xc2-\xdf][\x80-\xbf]", b"\xc3\xa9"),
    ("match", b"a\xffb", b"a\xffb"),
    # NUL truncates the pattern (C string semantics)
    ("search", b"a\x00b", b"a"),
    ("search", b"a\x00b", b"ab"),
    ("match", b"ab\x00", b"ab"),
    # alternation order (leftmost-first)
    ("match", b"abc|ab|a", b"ab"),
    ("match", b"abc|ab|a", b"abc"),
    ("match", b"abc|ab|a", b"a"),
    ("search", b"cat|dog", b"hotdog"),
    ("match", b"(a|b|c)+", b"abcba"),
    ("match", b"a|ab", b"ab"),
    ("match", b"ab|a", b"ab"),
    # groups
    ("match", b"(a)(b)(c)", b"abc"),
    ("match", b"(a)(b)?(c)", b"ac"),
    ("match", b"((a)(b))", b"ab"),
    ("match", b"(a+)(b+)(c+)", b"aabbcc"),
    ("match", b"(a)|(b)|(c)", b"b"),
    ("match", b"x(a|b)y", b"xby"),
    ("match", b"(?:a|b)c", b"bc"),
    # Brace quantifiers: ure has none — '{' is a literal byte
    # (CPython treats {n,m} as a quantifier; recorded divergence).
    ("search", b"a{2}", b"aa"),
    ("search", b"a{2,3}", b"aaa"),
    ("search", b"a{2}", b"a{2}"),
    ("search", b"a{", b"a{"),
    ("search", b"{a}", b"{a}"),
]

# (op, pattern, subject, pos, endpos); None = not passed.
# From tests/extmod/re_start_end_pos.py.
MATCHPOS_CASES = [
    ("match", b"o", b"dog", 1, None),
    ("match", b"o", b"dog", 2, None),
    ("match", b"o", b"dog", 5, None),
    ("match", b"o", b"dog", 0, 1),
    ("match", b"^o", b"dog", 1, None),
    ("match", b"o", b"dog", 1, 1),
    ("match", b"o", b"dog", 2, 1),
    ("search", b"o", b"dog", -2, None),
    ("search", b"o", b"dog", -2, -5),
    ("search", b"o", b"dog", 1, None),
    ("search", b"o", b"dog", 2, None),
    ("search", b"o$", b"dog", 0, 2),
    ("search", b"o$", b"dog", 0, 3),
    ("search", b"o$", b"dog", None, None),
    ("match", b"d", b"dog", 0, 1),
    ("match", b"d", b"dog", 1, None),
]

# (pattern, subject, maxsplit); from re_split.py / re_split_empty.py.
SPLIT_CASES = [
    (b" ", b"a b c foobar", 0),
    (b" +", b"a b    c   foobar", 0),
    (b" +", b"a b    c   foobar", 1),
    (b" +", b"a b    c   foobar", 2),
    (b"[a-f]+", b"0a3b9", 0),
    (b"x", b"fooxbar", 0),
    (b"^ab", b"abababcabab", 0),
    (b"^ab|cab", b"abababcabab", 0),
    (b" *", b"a b    c   foobar", 0),
    (b"x*", b"foo", 0),
    (b"x*", b"axbc", 0),
    (b"( )", b"a b c foobar", 0),
    (b",", b"a,b,,c", 0),
    (b",", b"a,b,,c", 1),
    (b",", b"", 0),
    (b"a", b"aaa", 0),
    (b"a*", b"banana", 0),
    (b"o", b"foobar", 0),
    (b"[0-9]+", b"ab12cd34", 0),
    (b"\\s+", b"a b\tc", 0),
    (b",", b"a,b,c,", 0),
]

# (pattern, subject, replacement, count); string replacements only
# (callable replacements are modre-level dispatch, not the engine).
# From re_sub.py / re_sub_unmatched.py.
SUB_CASES = [
    (b"\\d+", b"10 20 30 40 50", b"#", 0),
    (b"\\d+", b"10 20 30 40 50", b"#", 2),
    (b"a", b"banana", b"X", 0),
    (b"a", b"banana", b"X", 1),
    (b"(a)(b)", b"ab ab", b"\\2\\1", 0),
    (b"(a)", b"a", b"\\g<1>\\g<1>", 0),
    (b"(abc)", b"abc", b"\\g<1>\\g<1>", 0),
    (b"(a)(b)?", b"1a2", b"\\2-\\1", 0),
    (b"a", b"c", b"b", 0),
    (b"a", b"1a2a3a", b"b", 2),
    (b"(a)", b"a", b"b\\2", 0),
    (b"(a)", b"a", b"b\\199999999999999999999999999999999999999", 0),
    (b"b", b"abc", b"\\\\b", 0),
    (b"^ab", b"abababcabab", b"*", 0),
    (b"^ab|cab", b"abababcabab", b"*", 0),
    (b"a", b"a", b"a", 0),
    (b".", b"a", b"a", 0),
    (b"(a)", b"a", b"\\g<1>x", 0),
    (b"(a)(b)", b"ab", b"\\2", 0),
    (b"(a)", b"a", b"\\0", 0),
    (b"(a)", b"a", b"\\9", 0),
    (b"o", b"foobar", b"0", 0),
    (b"a", b"banana", b"\\", 0),
    (b"(calzino) (blu|bianco|verde) e (scarpa) (blu|bianco|verde)",
     b"calzino blu e scarpa verde", b"\\g<1> colore \\2 con \\g<3> colore \\4? ...", 0),
    (b"def\\s+([a-zA-Z_][a-zA-Z_0-9]*)\\s*\\(\\s*\\):",
     b"\n\ndef myfunc():\ndef myfunc1():",
     b"static PyObject*\npy_\\1(void){\n  return;\n}\n", 0),
]

# Patterns that must fail to compile (re_error.py + re_limit.py,
# patched limits). The expected sizecode comes from the C harness
# (None when it prints SIZE -1; the "a)" quirk gives a real size).
COMPILEERR_PATTERNS = [
    b"?", b"*", b"+", b")", b"a)", b"(", b"([", b"([)", b"[a", b"[a\\",
    b"a\\", b"(a", b"(?", b"(?:", b"(a))", b"a**",
    b"[" + b"a" * 256 + b"]",
    b"(a)" * 64,
    b"(a)" * 256,
    b"(" + b"a" * 17000 + b")?",
    b"b" * 20000 + b"|a",
]


def random_cases(rng):
    """~30 extra search/match cases from a safe grammar (no nested
    quantifiers, no group that can match empty under a star — those
    recurse without bound in BOTH engines)."""
    atoms = ["a", "b", "0", "1", " ", "[a-z]", "[0-9]", "[ab]",
             "\\d", "\\w", ".", "(a)", "(ab)", "(a|b)"]
    quants = ["", "", "?", "*", "+", "??", "*?", "+?"]
    subjects = ["", "a", "ab", "ba", "aab", "abab", "012", "a b",
                "aaabbb", "0a1b", "xyz", "a0b1", "\t a"]
    out = []
    seen = set()
    while len(out) < 30:
        n = rng.randint(1, 3)
        pat = "".join(rng.choice(atoms) + rng.choice(quants) for _ in range(n))
        if rng.random() < 0.3:
            pat = pat + "|" + "".join(
                rng.choice(atoms) + rng.choice(quants) for _ in range(rng.randint(1, 2)))
        subj = rng.choice(subjects)
        if rng.random() < 0.15:
            subj = subj + "\x80\xff"
        op = rng.choice(["search", "match"])
        key = (op, pat, subj)
        if key in seen:
            continue
        seen.add(key)
        out.append((op, pat.encode("latin-1"), subj.encode("latin-1")))
    return out


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def hx(data: bytes) -> str:
    return data.hex()


def py_bytes_literal(data: bytes) -> str:
    """A petitpy bytes literal for `data` (ASCII kept readable)."""
    parts = ["b\""]
    for b in data:
        if b == 0x5C:
            parts.append("\\\\")
        elif b == 0x22:
            parts.append("\\\"")
        elif 0x20 <= b < 0x7F:
            parts.append(chr(b))
        else:
            parts.append("\\x%02x" % b)
    parts.append("\"")
    return "".join(parts)


def run(cmd, timeout=60):
    r = subprocess.run(cmd, capture_output=True, timeout=timeout)
    return r.returncode, r.stdout.decode("utf-8", "surrogateescape"), \
        r.stderr.decode("utf-8", "surrogateescape")


# ---------------------------------------------------------------------------
# Oracle 1: C harness (dump + sizecode)
# ---------------------------------------------------------------------------

def harness_dump(pat: bytes):
    """Return (size_or_None, dump_text_or_None) from the C harness."""
    rc, out, err = run([HARNESS, pat.decode("latin-1"), "dump"])
    if rc != 0:
        raise RuntimeError("harness failed for %r: rc=%d err=%s" % (pat, rc, err[:200]))
    lines = out.split("\n")
    size = None
    if lines[0].startswith("SIZE "):
        v = int(lines[0][5:])
        size = None if v == -1 else v
    if "COMPILE_ERR" in lines[1:3]:
        return size, None
    # Dump is everything after the "OK ..." line (lines[2:]); the
    # trailing "" element from the final newline keeps the dump's
    # trailing "\n".
    assert lines[1].startswith("OK "), out
    dump = "\n".join(lines[2:])
    return size, dump


# ---------------------------------------------------------------------------
# Oracle 2: reference binary (ure), chunked driver files
# ---------------------------------------------------------------------------

DRIVER_HEAD = '''import ure

def hx(data):
    H = "0123456789abcdef"
    s = ""
    for b in data:
        s += H[b >> 4] + H[b & 15]
    return s

def spans(m):
    parts = []
    i = 0
    while True:
        try:
            s, e = m.span(i)
        except IndexError:
            break
        parts.append(str(s) + " " + str(e))
        i += 1
    return " ".join(parts)

'''

DRIVER_LOOP = '''
for idx, case in enumerate(CASES):
    op = case[0]
    try:
        if op == "search" or op == "match":
            c = ure.compile(case[1])
            m = c.search(case[2]) if op == "search" else c.match(case[2])
            if m is None:
                print("CASE " + str(idx) + " NOMATCH")
            else:
                print("CASE " + str(idx) + " MATCH " + spans(m))
        elif op == "matchpos":
            c = ure.compile(case[1])
            pos = case[3]
            endpos = case[4]
            if pos is None:
                m = c.search(case[2]) if case[5] == "s" else c.match(case[2])
            elif endpos is None:
                if case[5] == "s":
                    m = c.search(case[2], pos)
                else:
                    m = c.match(case[2], pos)
            else:
                if case[5] == "s":
                    m = c.search(case[2], pos, endpos)
                else:
                    m = c.match(case[2], pos, endpos)
            if m is None:
                print("CASE " + str(idx) + " NOMATCH")
            else:
                print("CASE " + str(idx) + " MATCH " + spans(m))
        elif op == "compile":
            try:
                ure.compile(case[1])
                print("CASE " + str(idx) + " COMPILE_OK")
            except Exception as e:
                print("CASE " + str(idx) + " COMPILE_ERR " + type(e).__name__)
        elif op == "split":
            c = ure.compile(case[1])
            if case[3] == 0:
                pieces = c.split(case[2])
            else:
                pieces = c.split(case[2], case[3])
            out = "CASE " + str(idx) + " SPLIT " + str(len(pieces))
            for p in pieces:
                out += " " + hx(p)
            print(out)
        elif op == "sub":
            c = ure.compile(case[1])
            if case[4] == 0:
                res = c.sub(case[2], case[3])
            else:
                res = c.sub(case[2], case[3], case[4])
            print("CASE " + str(idx) + " SUB " + hx(res))
    except Exception as e:
        print("CASE " + str(idx) + " EXC " + type(e).__name__)
'''


def build_driver(cases):
    """cases: list of tuples; returns the driver source text."""
    lines = [DRIVER_HEAD, "CASES = ["]
    for case in cases:
        op = case[0]
        if op in ("search", "match"):
            lines.append("    (%r, %s, %s)," % (op, py_bytes_literal(case[1]), py_bytes_literal(case[2])))
        elif op == "compile":
            lines.append("    (%r, %s)," % (op, py_bytes_literal(case[1])))
        elif op == "matchpos":
            # (op, pat, subj, pos, endpos, "m"/"s")
            pos = "None" if case[3] is None else str(case[3])
            endpos = "None" if case[4] is None else str(case[4])
            lines.append("    (%r, %s, %s, %s, %s, %r)," % (
                op, py_bytes_literal(case[1]), py_bytes_literal(case[2]), pos, endpos, case[5]))
        elif op == "split":
            lines.append("    (%r, %s, %s, %d)," % (op, py_bytes_literal(case[1]), py_bytes_literal(case[2]), case[3]))
        elif op == "sub":
            lines.append("    (%r, %s, %s, %s, %d)," % (
                op, py_bytes_literal(case[1]), py_bytes_literal(case[2]),
                py_bytes_literal(case[3]), case[4]))
    lines.append("]")
    lines.append(DRIVER_LOOP)
    return "\n".join(lines)


def run_binary_oracle(cases, only_chunk=None):
    """Run `cases` through the reference binary in chunks; return a
    list of payload strings (one per case, in order)."""
    results = []
    for start in range(0, len(cases), CHUNK):
        chunk_no = start // CHUNK
        if only_chunk is not None and chunk_no != only_chunk:
            continue
        chunk = cases[start:start + CHUNK]
        path = "/tmp/re15_oracle_chunk_%d.py" % chunk_no
        with open(path, "w") as fh:
            fh.write(build_driver(chunk))
        rc, out, err = run([BIN, path], timeout=60)
        if rc != 0:
            raise RuntimeError(
                "binary oracle chunk %d (cases %d-%d) failed: rc=%d\nstdout tail: %s\nstderr tail: %s"
                % (chunk_no, start, start + len(chunk) - 1, rc, out[-500:], err[-500:]))
        got = {}
        for line in out.split("\n"):
            if line.startswith("CASE "):
                parts = line.split(" ", 2)
                got[int(parts[1])] = parts[2] if len(parts) > 2 else ""
        for i in range(len(chunk)):
            if i not in got:
                raise RuntimeError("binary oracle chunk %d missing case %d" % (chunk_no, i))
            results.append(got[i])
    return results


# ---------------------------------------------------------------------------
# Oracle 3: CPython re cross-check (divergences only, never the oracle)
# ---------------------------------------------------------------------------

def cpython_result(case):
    """Run one case under CPython's re (bytes); return a payload string
    in the same shape as the binary oracle, or ('ERR', type)."""
    import re as cpy_re
    op = case[0]
    try:
        if op in ("search", "match"):
            c = cpy_re.compile(case[1])
            m = c.search(case[2]) if op == "search" else c.match(case[2])
            if m is None:
                return "NOMATCH"
            parts = []
            for i in range(c.groups + 1):
                s, e = m.span(i)
                parts.append(str(s) + " " + str(e))
            return "MATCH " + " ".join(parts)
        if op == "matchpos":
            c = cpy_re.compile(case[1])
            pos = case[3]
            endpos = case[4]
            args = [case[2]]
            if pos is not None:
                args.append(pos)
            if endpos is not None:
                args.append(endpos)
            fn = c.search if case[5] == "s" else c.match
            m = fn(*args)
            if m is None:
                return "NOMATCH"
            parts = []
            for i in range(c.groups + 1):
                s, e = m.span(i)
                parts.append(str(s) + " " + str(e))
            return "MATCH " + " ".join(parts)
        if op == "split":
            c = cpy_re.compile(case[1])
            pieces = c.split(case[2], case[3]) if case[3] != 0 else c.split(case[2])
            out = "SPLIT " + str(len(pieces))
            for p in pieces:
                out += " " + hx(p)
            return out
        if op == "sub":
            c = cpy_re.compile(case[1])
            res = c.sub(case[2], case[3], case[4]) if case[4] != 0 else c.sub(case[2], case[3])
            return "SUB " + hx(res)
    except Exception as e:
        return "EXC " + type(e).__name__
    return "EXC ?"


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    only_chunk = None
    if len(sys.argv) > 2 and sys.argv[1] == "--only-chunk":
        only_chunk = int(sys.argv[2])

    rng = random.Random(SEED)
    rand_cases = random_cases(rng)

    lines = []
    divergences = []

    # --- dump cases (C harness) ---
    for pat in DUMP_PATTERNS:
        size, dump = harness_dump(pat)
        if dump is None:
            raise RuntimeError("dump pattern failed to compile in C harness: %r" % pat)
        lines.append("dump %s %d %s" % (hx(pat), size, hx(dump.encode("utf-8"))))

    # --- execution cases (reference binary) ---
    exec_cases = []
    for op, pat, subj in SEARCH_MATCH_CASES:
        exec_cases.append((op, pat, subj))
    for op, pat, subj in rand_cases:
        exec_cases.append((op, pat, subj))
    for op, pat, subj, pos, endpos in MATCHPOS_CASES:
        exec_cases.append(("matchpos", pat, subj, pos, endpos, "m" if op == "match" else "s"))
    for pat, subj, maxsplit in SPLIT_CASES:
        exec_cases.append(("split", pat, subj, maxsplit))
    for pat, subj, repl, count in SUB_CASES:
        exec_cases.append(("sub", pat, repl, subj, count))

    results = run_binary_oracle(exec_cases, only_chunk)
    if only_chunk is not None:
        print("only-chunk mode: oracle ran, vectors not written")
        return

    # Re-assemble vector lines in the same order as exec_cases, and
    # cross-check each against CPython.
    idx = 0
    for op, pat, subj in SEARCH_MATCH_CASES:
        payload = results[idx]
        lines.append("%s %s %s %s" % (op, hx(pat), hx(subj), payload))
        cp = cpython_result((op, pat, subj))
        if cp != payload:
            divergences.append("CPython!=ure %s %r %r: ure=%s cpy=%s" % (op, pat, subj, payload, cp))
        idx += 1
    for op, pat, subj in rand_cases:
        payload = results[idx]
        lines.append("%s %s %s %s" % (op, hx(pat), hx(subj), payload))
        cp = cpython_result((op, pat, subj))
        if cp != payload:
            divergences.append("CPython!=ure %s %r %r: ure=%s cpy=%s" % (op, pat, subj, payload, cp))
        idx += 1
    for op, pat, subj, pos, endpos in MATCHPOS_CASES:
        payload = results[idx]
        ps = "-" if pos is None else str(pos)
        es = "-" if endpos is None else str(endpos)
        lines.append("matchpos %s %s %s %s %s %s" % ("m" if op == "match" else "s", hx(pat), hx(subj), ps, es, payload))
        cp = cpython_result(("matchpos", pat, subj, pos, endpos, "m" if op == "match" else "s"))
        if cp != payload:
            divergences.append("CPython!=ure matchpos %s %r %r pos=%s end=%s: ure=%s cpy=%s"
                               % (op, pat, subj, pos, endpos, payload, cp))
        idx += 1
    for pat, subj, maxsplit in SPLIT_CASES:
        payload = results[idx]
        lines.append("split %s %s %d %s" % (hx(pat), hx(subj), maxsplit, payload))
        cp = cpython_result(("split", pat, subj, maxsplit))
        if cp != payload:
            divergences.append("CPython!=ure split %r %r max=%d: ure=%s cpy=%s" % (pat, subj, maxsplit, payload, cp))
        idx += 1
    for pat, subj, repl, count in SUB_CASES:
        payload = results[idx]
        lines.append("sub %s %s %s %d %s" % (hx(pat), hx(subj), hx(repl), count, payload))
        cp = cpython_result(("sub", pat, repl, subj, count))
        if cp != payload:
            divergences.append("CPython!=ure sub %r %r repl=%r count=%d: ure=%s cpy=%s"
                               % (pat, subj, repl, count, payload, cp))
        idx += 1

    # --- compile-error cases (C harness sizecode + binary verdict) ---
    # The binary must also reject every one of these (ValueError).
    bin_compile = run_binary_oracle([("compile", pat) for pat in COMPILEERR_PATTERNS])
    for pat in COMPILEERR_PATTERNS:
        size, dump = harness_dump(pat)
        if dump is not None:
            raise RuntimeError("compileerr pattern compiled in C harness: %r" % pat)
        lines.append("compileerr %s %s" % (hx(pat), "NONE" if size is None else str(size)))
    for pat, verdict in zip(COMPILEERR_PATTERNS, bin_compile):
        if not verdict.startswith("COMPILE_ERR"):
            raise RuntimeError("binary compiled a compileerr pattern %r: %s" % (pat, verdict))

    with open(OUT_PATH, "w") as fh:
        fh.write("\n".join(lines) + "\n")

    # Summary.
    from collections import Counter
    counts = Counter(ln.split(" ", 1)[0] for ln in lines)
    print("wrote %d cases to %s" % (len(lines), OUT_PATH))
    print("per-op:", dict(sorted(counts.items())))
    print("CPython divergences vs ure: %d" % len(divergences))
    with open("/tmp/re15_divergences.txt", "w") as fh:
        fh.write("\n".join(divergences) + "\n")
    for d in divergences:
        print("  DIVERGE " + d)


if __name__ == "__main__":
    main()
