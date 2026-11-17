#!/usr/bin/env python3
"""Generate differential test vectors for the petitpy-rs mpz port.

Oracle: CPython big ints, EXCEPT where mpz.c semantics differ from
CPython — those cases use an explicit mpz-semantics model, documented
per op below (verified against the C code in
vendor/micropython/py/mpz.c, and where observable against the
reference petitpy binary):

- pow: mpz_pow_inpl returns 0 when base == 0 or exp < 0, and 1 when
  exp == 0 (except base == 0 -> 0). CPython: 0**0 == 1, 2**-1 == 0.5.
- pow3: mpz_pow3_inpl returns 0 when base == 0, exp < 0, or mod == 1.
  CPython: pow(2,-1,5) == 3, pow(2,10,1) == 0 (agrees there).
- hash: mpz_hash truncates the magnitude to 64 bits (shifting digits
  into a u64) and negates for negative values; CPython's hash() does
  a modular reduction (e.g. hash(2**70) is 0 in mpz, 512 in CPython).
- as_int_checked: rejects -2**63 (returns None) even though it fits
  in i64 — the C overflow check trips on its second digit.
- from_float: for floats in [-2, -1] mpz_set_from_float returns +1
  (the adj_exp == 0 branch calls mpz_set_from_int(z, 1), resetting
  the sign). Those floats are EXCLUDED from this battery and covered
  by a Rust unit test instead. inf/nan -> 0 (caller raises).
- as_float: expected value is the C's Horner accumulation
  (val * 2**32 + digit, most significant digit first) computed in
  float; the generator also reports whether CPython's float(x) agrees.
- to_bytes with as_signed=False and a negative value: mpz generates
  two's-complement bytes and ok=False (unless the value is 0); the
  caller (mp_obj_int_to_bytes_impl) never uses that combination
  (it passes as_signed = neg). The battery only uses the
  caller-reachable combinations, and the oracle simulates mpz
  directly (see mpz_as_bytes_model).
- cmp with a "negative zero" (set_from_str("-0", neg=True)): mpz_cmp
  compares the sign first, so -0 < 0. Covered by a Rust unit test,
  not by this battery (CPython has no negative zero).

Deterministic: random.seed(20261117). Output is written to
scripts/mpz_vectors.txt (one case per line, space-separated).
The Rust probe test (src/vendor/probe.rs) reads it via include_str!.
"""

import random
import struct
import sys

random.seed(20261117)

DIG_MASK = 0xFFFFFFFF
DIG_BASE = 1 << 32


def digits_of(n):
    """Little-endian base-2**32 digits of abs(n)."""
    n = abs(n)
    if n == 0:
        return []
    ds = []
    while n:
        ds.append(n & DIG_MASK)
        n >>= 32
    return ds


def mpz_hash(n):
    """mpz_hash: low 64 bits of the magnitude, negated if negative."""
    v = 0
    for d in reversed(digits_of(n)):
        v = ((v << 32) | d) & 0xFFFFFFFFFFFFFFFF
    if n < 0:
        v = (-v) & 0xFFFFFFFFFFFFFFFF
    return v if v < (1 << 63) else v - (1 << 64)


def mpz_pow(a, b):
    """mpz_pow_inpl semantics."""
    if a == 0 or b < 0:
        return 0
    if b == 0:
        return 1
    return a ** b


def mpz_pow3(a, b, m):
    """mpz_pow3_inpl semantics (m != 0)."""
    if a == 0 or b < 0 or m == 1:
        return 0
    if b == 0:
        return 1
    return pow(a, b, m)


def mpz_as_float(n):
    """mpz_as_float: Horner in double, most significant digit first."""
    v = 0.0
    for d in reversed(digits_of(n)):
        v = v * float(DIG_BASE) + float(d)
    return -v if n < 0 else v


def mpz_as_bytes_model(v, big_endian, as_signed, length):
    """Simulate mpz_as_bytes exactly; returns (hex_bytes, ok)."""
    # Re-implement the C loop over digits.
    digs = digits_of(v)
    neg = v < 0
    buf = bytearray(length)
    pos = length if big_endian else 0
    olen = length
    bits = 0
    d = 0
    carry = 1
    ok = True
    for zdig in digs:
        bits += 32
        d = ((d << 32) | zdig) & 0xFFFFFFFFFFFFFFFF
        while bits >= 8:
            bits -= 8
            val = d & 0xFFFFFFFF
            if neg:
                val = (((~val) & 0xFF) + carry) & 0xFFFFFFFF
                carry = val >> 8
            if olen == 0:
                ok = ok and (val & 0xFF) == 0
                d >>= 8
                continue
            if big_endian:
                pos -= 1
                buf[pos] = val & 0xFF
            else:
                buf[pos] = val & 0xFF
                pos += 1
            olen -= 1
            d >>= 8
    if as_signed and olen == 0 and length > 0:
        most_sig = buf[0] if big_endian else buf[length - 1]
        ok = ok and ((most_sig & 0x80) != 0) == neg
    else:
        fill = 0xFF if neg else 0x00
        if big_endian:
            buf[0:olen] = bytes([fill]) * olen
        else:
            buf[pos:pos + olen] = bytes([fill]) * olen
    return buf.hex(), 1 if ok else 0


def to_base(n, base):
    """Format n in base 2..32, lowercase, with '-' for negatives."""
    if n == 0:
        return "0"
    neg = n < 0
    n = abs(n)
    chars = "0123456789abcdefghijklmnopqrstuvwxyz"
    out = []
    while n:
        out.append(chars[n % base])
        n //= base
    s = "".join(reversed(out))
    return ("-" + s) if neg else s


def to_base_comma(n, base):
    """to_base with ',' every 3 digits (base 10) or 4 (other bases)."""
    s = to_base(n, base)
    neg = s.startswith("-")
    if neg:
        s = s[1:]
    group = 3 if base == 10 else 4
    parts = []
    while s:
        parts.append(s[-group:])
        s = s[:-group]
    s = ",".join(reversed(parts))
    return ("-" + s) if neg else s


# ---------------------------------------------------------------- values
SMALL = [0, 1, -1, 2, -2, 3, 5, 7, 10, 42, 100, 255, 256, 257,
         -255, -256, 1000, -1000, 2**31 - 1, -(2**31 - 1), 2**31,
         -(2**31), 2**32 - 1, 2**32, -(2**32), 2**32 + 1]
MEDIUM = [2**63 - 1, -(2**63 - 1), 2**63, -(2**63), 2**64 - 1, 2**64,
          -(2**64), 2**64 + 1, 2**70, -(2**70), 2**100, -(2**100),
          10**30, -(10**30), 12345678901234567890,
          -12345678901234567890, 98765432109876543210]
HUGE = []
for _ in range(12):
    bits = random.choice([100, 128, 200, 256, 300])
    v = random.getrandbits(bits)
    if random.random() < 0.5:
        v = -v
    HUGE.append(v)
# Values straddling digit boundaries.
BOUNDARY = []
for k in (1, 2, 3):
    for delta in (-2, -1, 0, 1, 2):
        BOUNDARY.append((1 << (32 * k)) + delta)
        BOUNDARY.append(-((1 << (32 * k)) + delta))

POOL = SMALL + MEDIUM + HUGE + BOUNDARY

lines = []


def emit(*parts):
    lines.append(" ".join(str(p) for p in parts))


# ---------------------------------------------------------------- add/sub/mul
for _ in range(40):
    a = random.choice(POOL)
    b = random.choice(POOL)
    emit("add", a, b, a + b)
for _ in range(40):
    a = random.choice(POOL)
    b = random.choice(POOL)
    emit("sub", a, b, a - b)
for _ in range(40):
    a = random.choice(POOL)
    b = random.choice(POOL)
    emit("mul", a, b, a * b)
# Edge pairs: carries across digit boundaries.
for a, b in [(2**32 - 1, 1), (2**64 - 1, 2**64 - 1),
             (-(2**32), -1), (2**100, -(2**100 - 1)),
             (10**50, 10**50)]:
    emit("add", a, b, a + b)
    emit("sub", a, b, a - b)
    emit("mul", a, b, a * b)

# ---------------------------------------------------------------- divmod
for _ in range(40):
    a = random.choice(POOL)
    b = random.choice([x for x in POOL if x != 0])
    emit("divmod", a, b, a // b, a % b)
for a, b in [(10, 3), (-10, 3), (10, -3), (-10, -3), (7, 7),
             (5, 7), (-5, 7), (0, 7), (2**64, 3), (-(2**64), 3),
             (2**100, 2**64 + 1), (2**96 - 1, 2**64 + 1),
             (10**50, 7), (10**50, -(10**25))]:
    emit("divmod", a, b, a // b, a % b)

# ---------------------------------------------------------------- pow / pow3
for a, b in [(2, 10), (2, 0), (0, 5), (0, 0), (2, -1), (-2, 3),
             (-2, 4), (3, 40), (2, 100), (5, 0), (10, 20),
             (-3, 7), (123, 5)]:
    emit("pow", a, b, mpz_pow(a, b))
for a, b, m in [(2, 10, 1000), (2, 0, 5), (0, 5, 7), (2, -1, 5),
                (2, 10, 1), (2, 3, -5), (123456789, 987654321, 1000000007),
                (5, 0, 7), (3, 100, 1000000007), (-2, 10, 97),
                (10**20, 12345, 999999937)]:
    emit("pow3", a, b, m, mpz_pow3(a, b, m))

# ---------------------------------------------------------------- shifts
for _ in range(30):
    a = random.choice(POOL)
    n = random.choice([0, 1, 2, 7, 31, 32, 33, 63, 64, 65, 100, 128, 200])
    emit("shl", a, n, a << n)
    emit("shr", a, n, a >> n)
for a, n in [(-1025, 10), (-1, 100), (-(2**64) - 1, 64),
             (-(2**64), 64), (2**64, 64), (1, 200)]:
    emit("shr", a, n, a >> n)
    emit("shl", a, n, a << n)

# ---------------------------------------------------------------- bitwise
for _ in range(30):
    a = random.choice(POOL)
    b = random.choice(POOL)
    emit("and", a, b, a & b)
    emit("or", a, b, a | b)
    emit("xor", a, b, a ^ b)
for a, b in [(-1, 5), (-5, -3), (5, -3), (0, -1),
             (2**64 - 1, 2**64), (-(2**64), 2**64 - 1)]:
    emit("and", a, b, a & b)
    emit("or", a, b, a | b)
    emit("xor", a, b, a ^ b)

# ---------------------------------------------------------------- unary
for _ in range(20):
    a = random.choice(POOL)
    emit("neg", a, -a)
    emit("not", a, ~a)
    emit("abs", a, abs(a))
for _ in range(30):
    a = random.choice(POOL)
    b = random.choice(POOL)
    emit("cmp", a, b, (a > b) - (a < b))

# ---------------------------------------------------------------- bit length
for _ in range(20):
    a = random.choice(POOL)
    emit("bitlen", a, a.bit_length())

# ---------------------------------------------------------------- fromstr
STR_CASES = [
    ("123", 10, 0), ("ff", 16, 0), ("FF", 16, 0), ("101", 2, 0),
    ("777", 8, 0), ("z", 36, 0), ("12z34", 10, 0), ("1g", 16, 0),
    ("0", 10, 1), ("", 10, 0), ("", 10, 1), ("deadBEEF", 16, 0),
    ("123456789012345678901234567890", 10, 0),
    ("100000000000000000000000000000000000000000000000000", 2, 0),
    ("zz", 36, 0), ("10", 36, 0), ("7", 8, 0), ("8", 8, 0),
]
for s, base, neg in STR_CASES:
    consumed = 0
    for ch in s:
        if ch.isdigit():
            v = ord(ch) - ord("0")
        elif "A" <= ch <= "Z":
            v = ord(ch) - ord("A") + 10
        elif "a" <= ch <= "z":
            v = ord(ch) - ord("a") + 10
        else:
            break
        if v >= base:
            break
        consumed += 1
    val = int(s[:consumed], base) if consumed else 0
    if neg:
        val = -val
    emit("fromstr", s if s else "_", base, neg, consumed, val, neg)

# ---------------------------------------------------------------- tostr
for _ in range(30):
    a = random.choice(POOL)
    base = random.choice([2, 8, 10, 16, 32])
    emit("tostr", a, base, to_base(a, base))
for a in [0, -1, 255, -255, 2**64 - 1, -(2**64 - 1), 10**40]:
    for base in (2, 8, 10, 16, 32):
        emit("tostr", a, base, to_base(a, base))
for a in [1234567, -1234567, 123456, 1234, 123, 12345678, 10**20]:
    emit("tostr_comma", a, 10, to_base_comma(a, 10))
    emit("tostr_comma", a, 16, to_base_comma(a, 16))

# ---------------------------------------------------------------- bytes
for _ in range(10):
    nbytes = random.choice([1, 2, 4, 8, 9, 16])
    v = random.getrandbits(nbytes * 8)
    for be in (0, 1):
        emit("frombytes", v.to_bytes(nbytes, "big" if be else "little").hex(), be, v)
# tobytes: caller-reachable combos (as_signed == (v < 0)) plus a few
# unsigned non-negative cases; oracle is the mpz simulation.
for _ in range(20):
    a = random.choice(POOL)
    be = random.choice([0, 1])
    signed = 1 if a < 0 else random.choice([0, 1])
    length = random.choice([1, 2, 4, 8, 9, 16, 32])
    hx, ok = mpz_as_bytes_model(a, be == 1, signed == 1, length)
    # Cross-check against CPython where it applies. CPython and mpz
    # agree EXCEPT for negative values whose magnitude needs more
    # than `length` bytes of digits (4 * len(digits) > length):
    # mpz then returns ok=False even when the value fits the signed
    # buffer, because the two's-complement sign-extension bytes
    # (0xFF) fail its "remaining bytes must be zero" check.
    # Verified on the reference binary:
    #   (-2**70).to_bytes(9, 'little') -> OverflowError: buffer too small
    #   (2**70).to_bytes(9, 'little')  -> b'\x00...\x00@'
    if a >= 0 or 4 * len(digits_of(a)) <= length:
        try:
            cpy = a.to_bytes(length, "big" if be else "little", signed=bool(signed))
            assert ok == 1 and cpy.hex() == hx, (a, be, signed, length)
        except OverflowError:
            assert ok == 0, (a, be, signed, length)
    else:
        assert ok == 0, (a, be, signed, length)
    emit("tobytes", a, be, signed, length, hx if hx else "-", ok)
for a, be, signed, length in [
        (256, 0, 0, 1), (255, 0, 1, 1), (-129, 0, 1, 1),
        (-1, 0, 1, 4), (-1, 1, 1, 6), (0, 0, 0, 4),
        (0x01020304, 0, 0, 4), (0x01020304, 1, 0, 4)]:
    hx, ok = mpz_as_bytes_model(a, be == 1, signed == 1, length)
    emit("tobytes", a, be, signed, length, hx if hx else "-", ok)

# ---------------------------------------------------------------- hash
for _ in range(15):
    a = random.choice(POOL)
    emit("hash", a, mpz_hash(a))
for a in [0, 1, -1, 2**70, -(2**70), 2**64 + 5, -(2**64 + 5)]:
    emit("hash", a, mpz_hash(a))

# ---------------------------------------------------------------- as int/uint
for _ in range(15):
    a = random.choice(POOL)
    # mpz rejects -2**63 even though it fits in i64.
    ok = 1 if -(2**63) < a <= 2**63 - 1 else 0
    emit("asint", a, ok, a if ok else 0)
for a in [2**63 - 1, 2**63, -(2**63), -(2**63) - 1, 0, -1]:
    ok = 1 if -(2**63) < a <= 2**63 - 1 else 0
    emit("asint", a, ok, a if ok else 0)
for _ in range(15):
    a = random.choice(POOL)
    ok = 1 if 0 <= a <= 2**64 - 1 else 0
    emit("asuint", a, ok, a if ok else 0)
for a in [2**64 - 1, 2**64, -1, 0]:
    ok = 1 if 0 <= a <= 2**64 - 1 else 0
    emit("asuint", a, ok, a if ok else 0)

# ---------------------------------------------------------------- float
FLOATS = [0.0, -0.0, 0.5, -0.5, 1.5, 2.5, -2.5, 1e30, -1e30,
          2.0**70, 123456.789, 5e-324, 2.0**-10, 3.0, -3.0,
          1.0, 2.0, 9007199254740993.0]
for f in FLOATS:
    # mpz_set_from_float == int(f) except inf/nan (-> 0) and the
    # [-2, -1] quirk (excluded here; unit-tested in Rust).
    emit("fromfloat", repr(f), int(f))
for f in [float("inf"), float("-inf"), float("nan")]:
    emit("fromfloat", repr(f), 0)
# as_float: Horner oracle; report CPython agreement separately.
n_asfloat_disagree = 0
for _ in range(15):
    a = random.choice(POOL)
    horner = mpz_as_float(a)
    if float(a) != horner:
        n_asfloat_disagree += 1
    emit("asfloat", a, repr(horner))
for a in [0, 1, -1, 2**53 + 1, -(2**53 + 1), 10**30, 2**70]:
    horner = mpz_as_float(a)
    if float(a) != horner:
        n_asfloat_disagree += 1
    emit("asfloat", a, repr(horner))

out_path = sys.argv[1] if len(sys.argv) > 1 else "scripts/mpz_vectors.txt"
with open(out_path, "w") as fh:
    fh.write("\n".join(lines) + "\n")

from collections import Counter
counts = Counter(ln.split(" ", 1)[0] for ln in lines)
print(f"wrote {len(lines)} cases to {out_path}")
print("per-op:", dict(sorted(counts.items())))
print(f"as_float Horner-vs-float(x) disagreements: {n_asfloat_disagree}")
