//! Int type — `mp_obj_int_make_new`, `mp_obj_int_unary_op`,
//! `mp_obj_int_binary_op`, `int.from_bytes` / `int.to_bytes`,
//! `mp_obj_int_formatted` (`objint.zig`, 556 lines).
//!
//! The Zig file is the port of vendor `py/objint.c` (small-int paths,
//! `make_new`, `from_bytes` / `to_bytes`, formatting) with the
//! heap-int arithmetic in vendor `py/objint_mpz.c` (full config —
//! the Zig file only declares it extern). Both are ported here; the
//! `objint_mpz.c` functions that belong to the int type live in this
//! file, and the C18 vendor-C lane must skip exactly those (see the
//! result file). Big-integer arithmetic goes through Lane C's
//! `crate::vendor::mpz::BigInt`.
//!
//! `mp_type_int` ROM: flags `0x0000`, name `int`, slots make_new=1,
//! print=2, unary_op=3, binary_op=4, locals_dict=5 (`from_bytes`
//! classmethod sig 2-4 counting `cls`, `to_bytes` sig 1-4).
//! `mp_obj_new_int` / `from_ll` / `from_ull` / `from_uint` /
//! `from_str_len`: small-int fast paths, else mpz.
//!
//! bool is NOT an int subclass in petitpy (verified on the reference
//! binary: `isinstance(True, int)` is False, `issubclass(bool, int)`
//! is False — neither type has a `parent` slot, and
//! `mp_obj_is_subclass` only special-cases `object`). The bool→int
//! relationship exists ONLY in arithmetic delegation (bool's slots
//! convert to small int 0/1 and delegate), NOT in the type hierarchy.
//! So int's `base` is `None` and its MRO is `[int]`.

use crate::memory::gc::CallCtx;
use crate::objects::obj::BuiltinTypes;
use crate::objects::primitives::ops::{
    arg_as_int, int_from_float, is_exact_int, is_float, new_int_from_bigint,
    new_int_from_bigint_typed, new_int_from_i64, prim_to_pyresult, static_qstr_id,
    str_bytes_of, PrimError, PrimOutcome, PrimResult, QstrResolver,
};
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::value::{
    wrap_small_int, BytesData, DictData, FunBuiltinData, GcRef, MapData, Obj, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};
use crate::vendor::mpz::BigInt;

// ── `mp_parse_num_integer` (vendor `py/parsenum.c:71`) ─────────────────

/// ASCII whitespace per `unichar_isspace` (vendor `py/unicode.c:119-121`):
/// `c < 128 && FL_SPACE` — exactly `0x09..=0x0d` and `0x20`. The
/// parse works on raw bytes, so non-ASCII bytes are never space.
fn is_space(b: u8) -> bool {
    matches!(b, 0x09..=0x0d | 0x20)
}

/// `mp_parse_num_base` (vendor `py/parsenumbase.c`): strip an
/// optional `0x` / `0o` / `0b` prefix (case-insensitive) and settle
/// `base`; returns the prefix length to skip. Base 0 with a leading
/// `0` and no prefix letter becomes base 1 (provokes `ValueError`
/// unless every digit is zero); base 0 with no prefix becomes 10.
/// A prefix letter only counts when it matches the requested base.
fn parse_num_base(s: &[u8], base: &mut i32) -> usize {
    if s.len() <= 1 {
        if *base == 0 {
            *base = 10;
        }
        return 0;
    }
    if s[0] == b'0' {
        let c = s[1] | 0x20;
        let b = *base;
        if c == b'x' && (b == 0 || b == 16) {
            *base = 16;
            2
        } else if c == b'o' && (b == 0 || b == 8) {
            *base = 8;
            2
        } else if c == b'b' && (b == 0 || b == 2) {
            *base = 2;
            2
        } else {
            if b == 0 {
                *base = 1;
            }
            0
        }
    } else {
        if *base == 0 {
            *base = 10;
        }
        0
    }
}

/// `mp_parse_num_integer` (vendor `py/parsenum.c:71`): the exact
/// integer denoted by `s` in `base` (0 = auto-detect). The original
/// parses into a `u64` and re-parses via mpz on overflow; accumulating
/// straight into a `BigInt` gives the identical value.
/// `_` is skipped ANYWHERE (including leading/trailing/doubled —
/// `int("_")` is 0: the `_` advances past `str_val_start`, so the
/// no-digits check does not fire); a digit `>= base` ends the digit
/// run without error; trailing whitespace is skipped and anything
/// else left raises `InvalidSyntaxForInt`.
pub fn parse_int_text(s: &[u8], base_in: i32) -> Result<BigInt, PrimError> {
    let mut base = base_in;
    if (base != 0 && base < 2) || base > 36 {
        return Err(PrimError::IntBaseRange);
    }
    let top = s.len();
    let mut i = 0;
    while i < top && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < top {
        if s[i] == b'+' {
            i += 1;
        } else if s[i] == b'-' {
            i += 1;
            neg = true;
        }
    }
    i += parse_num_base(&s[i..], &mut base);
    let val_start = i;
    let mut z = BigInt::zero();
    let bbase = BigInt::from_int(base as i64);
    while i < top {
        let mut dig = s[i];
        if (b'0'..=b'9').contains(&dig) {
            dig -= b'0';
        } else if dig == b'_' {
            i += 1;
            continue;
        } else {
            dig |= 0x20;
            if (b'a'..=b'z').contains(&dig) {
                dig -= b'a' - 10;
            } else {
                break;
            }
        }
        if dig as i32 >= base {
            break;
        }
        z.mul_assign(&bbase);
        z.add_assign(&BigInt::from_int(dig as i64));
        i += 1;
    }
    if i == val_start {
        return Err(PrimError::InvalidSyntaxForInt);
    }
    while i < top && is_space(s[i]) {
        i += 1;
    }
    if i != top {
        return Err(PrimError::InvalidSyntaxForInt);
    }
    if neg {
        z = -z;
    }
    Ok(z)
}

// ── `mp_obj_int_make_new` (objint.zig; vendor `py/objint.c:40`) ────────

/// One-arg `int(x)`: `INT_MAYBE` first (int → itself, bool → 0/1),
/// then the buffer path (str/bytes read as a buffer, parsed base 10),
/// then float → exact conversion, else `TypeError: can't convert to
/// int`. (A `__int__` on a user instance is C12's — `INT_MAYBE`
/// consults the type's slot there.)
fn int_make_new_1(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    arg: Value,
) -> Result<Value, PrimError> {
    if let Value::Bool(b) = arg {
        return Ok(Value::SmallInt(if b { 1 } else { 0 }));
    }
    if is_exact_int(ctx.arena, types, arg) {
        return Ok(arg);
    }
    if let Some(bytes) = str_bytes_of(ctx.arena, types, resolver, arg) {
        let z = parse_int_text(&bytes, 10)?;
        return Ok(new_int_from_bigint(ctx.arena, types, ctx.roots, &z));
    }
    if is_float(ctx.arena, types, arg) {
        if let Value::Heap(r) = arg {
            if let Obj::Float(f) = ctx.arena.get(r) {
                return int_from_float(ctx.arena, types, ctx.roots, *f);
            }
        }
    }
    Err(PrimError::IntConversion)
}

/// Two-arg `int(x, base)`: `base` via `mp_obj_get_int` (bool → 0/1,
/// float/str → `IntConversion`, too-big heap int → `OverflowWord`),
/// then `mp_obj_str_get_data(x)`. The original's Zig `str_get_data`
/// is polled — on bad input it sets `pending_exc` (TypeError) and
/// returns NULL with len 0, and `make_new` does NOT check NULL: the
/// empty parse raises `InvalidSyntaxForInt`, overwriting the pending
/// TypeError. `unwrap_or_default` reproduces exactly that observable.
/// NOTE: the base is truncated `i64` → `c_int` (verified:
/// `int("42", 2**40)` is 42 — base 0; `int("ff", 2**32 + 16)` is 255
/// — base 16).
fn int_make_new_2(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    arg0: Value,
    arg1: Value,
) -> Result<Value, PrimError> {
    let base = arg_as_int(ctx.arena, types, arg1)? as i32;
    let bytes = str_bytes_of(ctx.arena, types, resolver, arg0).unwrap_or_default();
    let z = parse_int_text(&bytes, base)?;
    Ok(new_int_from_bigint(ctx.arena, types, ctx.roots, &z))
}

/// `int([x[, base]])`, sig (0, 2, false). `mp_arg_check_num_sig` is
/// POLLED-ONLY (argcheck.zig:100 — it sets `pending_exc` and returns;
/// `make_new` never checks it): the final outcome is the branch's
/// own error if it raises, else the pending `TypeError: argument
/// num/types mismatch` if the counts were wrong (verified:
/// `int(1, 2, 3)` → `ValueError: invalid syntax for integer` — the
/// 2-arg branch raised first; `int("42", 10, 99)`, `int(1, x=1)`,
/// `int(x=1)` → `TypeError: argument num/types mismatch`).
pub fn int_make_new(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, PrimError> {
    let arg_ok = args.len() <= 2 && n_kw == 0;
    let r = match args.len() {
        0 => Ok(Value::SmallInt(0)),
        1 => int_make_new_1(ctx, types, resolver, args[0]),
        _ => int_make_new_2(ctx, types, resolver, args[0], args[1]),
    };
    match r {
        Err(e) => Err(e),
        Ok(v) => {
            if arg_ok {
                Ok(v)
            } else {
                Err(PrimError::ArgNumTypesMismatch)
            }
        }
    }
}

// ── Type ROM ────────────────────────────────────────────────────────────

/// `mp_type_int` ROM data. `dict` is the `locals_dict` ref (see
/// `int_locals_dict`); `mro` is filled with `[self]` by the bootstrap
/// after allocation. The op slots are the C7r adapters below
/// (`int_unary_slot` / `int_binary_slot` / `int_print`);
/// `call` / `getiter` / `subscr` / `buffer` stay `None` — the
/// original's int type has no such slots (`make_new` is the type-call
/// path, wired by C12, not a `TypeData` slot).
pub fn int_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"int").expect("MP_QSTR_int is in the static table"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(int_unary_slot),
        binary_op: Some(int_binary_slot),
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(int_print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}

/// The `n_args` encoding for `FunBuiltinData`: `(min << 8) | max`
/// (no consumer exists yet in this tree — the call machinery of
/// C10/C14 reads it; documented so the first consumer matches).
pub const fn fun_n_args(min: u16, max: u16) -> u16 {
    (min << 8) | max
}

/// `int.from_bytes` as a `FunBuiltin` body (classmethod — `args[0]`
/// is the class, which supplies the result type; `args[1]` is the
/// buffer; `args[2]` the byteorder; a 4th positional arg, if any, is
/// IGNORED by the original — no `signed` support, same TODO as
/// vendor `py/objint.c`). The byteorder is big unless the arg is
/// exactly the qstr `little` (a heap `Str` `"little"` is NOT the
/// qstr, so it means big — the Zig compares qstr immediates).
/// Errors lose their data at this boundary (bare `PyRaise`, the C6
/// slot-boundary convention); the rich `int_from_bytes` below
/// preserves them.
pub fn int_from_bytes_builtin(ctx: &mut CallCtx<'_>, args: &[Value]) -> PyResult<Value> {
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    let cls = match args[0] {
        Value::Heap(r) => r,
        _ => return Err(PyError::PyRaise),
    };
    let buf = match args[1] {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Bytes(b) => b.bytes.clone(),
            _ => return Err(PrimError::BufferProtocol.to_pyerror()),
        },
        _ => return Err(PrimError::BufferProtocol.to_pyerror()),
    };
    let little = matches!(args.get(2), Some(Value::Qstr(id)) if Some(*id) == static_qstr_id(b"little"));
    Ok(int_from_bytes(ctx, cls, &buf, little))
}

/// `int`'s `locals_dict`: `from_bytes` (classmethod, sig 2-4
/// counting `cls`). `to_bytes` has NO entry yet — its `FunBuiltin`
/// body would need the bytes type, which the frozen `BuiltinFn`
/// signature cannot carry; C13 wires it when `CallCtx` carries the
/// type registry (the rich `int_to_bytes` below is the
/// implementation). Both objects are allocated with a `None` header
/// type — the bootstrap pattern (the dict/builtin-function types do
/// not exist yet).
pub fn int_locals_dict(ctx: &mut CallCtx<'_>) -> GcRef {
    let from_bytes_id = static_qstr_id(b"from_bytes").expect("MP_QSTR_from_bytes is static");
    let fun = ctx.alloc(
        None,
        Obj::FunBuiltin(FunBuiltinData {
            name: from_bytes_id,
            n_args: fun_n_args(2, 4),
            func: int_from_bytes_builtin,
            // C10: `from_bytes` takes no kwargs.
            kw_func: None,
        }),
    );
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![(Value::Qstr(from_bytes_id), Value::Heap(fun))]),
        }),
    )
}

// ── `int.from_bytes` / `int.to_bytes` ───────────────────────────────────

/// `int.from_bytes(buf, byteorder)` (classmethod): the result is
/// typed by `cls`. Empty buffer → 0. Little-endian walks the buffer
/// backwards (Lane C `BigInt::from_bytes(big_endian, buf)` reads
/// little-endian directly when `big_endian` is false).
pub fn int_from_bytes(
    ctx: &mut CallCtx<'_>,
    cls: GcRef,
    buf: &[u8],
    little_endian: bool,
) -> Value {
    let z = BigInt::from_bytes(!little_endian, buf);
    new_int_from_bigint_typed(ctx.arena, cls, ctx.roots, &z)
}

/// The small-int branch of `int.to_bytes` (the Zig
/// `mp_binary_set_int` path): `slen` from the clz rule
/// (`val > 0: (64 - clz(val) + 7) / 8`; `val < -1: (64 - clz(~val) +
/// 8) / 8`; else `-val`, i.e. 0 → 0, -1 → 1), sign-fill, minimal
/// two's-complement bytes at the end (big-endian) / start
/// (little-endian).
fn small_int_to_bytes(val: i64, dlen: usize, little: bool) -> Result<Vec<u8>, PrimError> {
    let slen: usize = if val > 0 {
        (64 - val.leading_zeros() as usize + 7) / 8
    } else if val < -1 {
        (64 - (!val).leading_zeros() as usize + 8) / 8
    } else {
        (-val) as usize
    };
    if slen > dlen {
        return Err(PrimError::BufferTooSmall);
    }
    let mut buf = vec![if val < 0 { 0xff } else { 0x00 }; dlen];
    let be = (val as u64).to_be_bytes();
    let min = &be[8 - slen..];
    if little {
        for (i, b) in min.iter().rev().enumerate() {
            buf[i] = *b;
        }
    } else {
        buf[dlen - slen..].copy_from_slice(min);
    }
    Ok(buf)
}

/// `int.to_bytes(length[, byteorder])`: two's-complement minimal
/// bytes, sign-filled to `length`. Small ints take the Zig
/// `mp_binary_set_int` path above; heap ints take Lane C
/// `BigInt::as_bytes` — which is the vendor `mpz_as_bytes` verbatim,
/// INCLUDING its quirk: the overflow-byte check requires trailing
/// `0x00`, so a negative heap int needs `ceil(digits * 32 / 8)`
/// bytes, not the minimal two's-complement length (verified:
/// `(-(2**70)).to_bytes(10)` and `(11)` both raise
/// `OverflowError: buffer too small`; `(12)` succeeds). Negative
/// length → bare `ValueError`. `length` defaults to 1.
pub fn int_to_bytes(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    self_: Value,
    length: Option<Value>,
    byteorder: Option<Value>,
) -> Result<Value, PrimError> {
    let dlen: i64 = match length {
        None => 1,
        Some(v) => arg_as_int(ctx.arena, types, v)?,
    };
    if dlen < 0 {
        return Err(PrimError::ValueErrorBare);
    }
    let little = matches!(byteorder, Some(Value::Qstr(id)) if Some(id) == static_qstr_id(b"little"));
    let dlen = dlen as usize;
    let buf = match self_ {
        Value::SmallInt(n) => small_int_to_bytes(n, dlen, little)?,
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => {
                let mut buf: Vec<u8> = Vec::new();
                buf.try_reserve_exact(dlen)
                    .map_err(|_| PrimError::OutOfMemory)?;
                buf.resize(dlen, 0);
                if !z.as_bytes(!little, true, &mut buf) {
                    return Err(PrimError::BufferTooSmall);
                }
                buf
            }
            _ => return Err(PrimError::IntConversion),
        },
        _ => return Err(PrimError::IntConversion),
    };
    Ok(Value::Heap(
        ctx.alloc(Some(types.bytes), Obj::Bytes(BytesData { bytes: buf })),
    ))
}

// ── `mp_obj_int_unary_op` (vendor `py/objint_mpz.c`) ────────────────────

/// Unary op on an int. Operates on heap ints in the original (small
/// ints take the `mp_unary_op` fast path in `py/runtime.c:294` — the
/// results agree, so this function handles both uniformly).
/// `NOT` is answered by the generic `mp_unary_op` first (via
/// `is_true`) and never reaches the slot; `LEN`, `SIZEOF`,
/// `FLOAT_MAYBE`, `COMPLEX_MAYBE` → `NotHandled`.
pub fn int_unary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: UnaryOp,
    v: Value,
) -> PrimResult {
    enum IntVal {
        Small(i64),
        Big(BigInt),
    }
    let val = match v {
        Value::SmallInt(n) => IntVal::Small(n),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => IntVal::Big(z.clone()),
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    let out = match op {
        UnaryOp::Bool => match val {
            IntVal::Small(n) => Value::Bool(n != 0),
            IntVal::Big(ref z) => Value::Bool(!z.is_zero()),
        },
        UnaryOp::Hash => match val {
            IntVal::Small(n) => Value::SmallInt(n),
            // Vendor `objint_mpz.c:147-148`:
            // `MP_OBJ_NEW_SMALL_INT(mpz_hash(...))` — tag-wrap,
            // no window check. `new_int_from_i64` would allocate
            // a heap int for out-of-window hashes, so
            // `hash(2**62)` came back `2**62` instead of
            // `-(2**62)` (C9b follow-up, probed on the reference
            // binary 2026-11-17).
            IntVal::Big(ref z) => Value::SmallInt(wrap_small_int(z.hash_mpz())),
        },
        UnaryOp::Positive => v,
        UnaryOp::Negative => match val {
            IntVal::Small(n) => new_int_from_i64(ctx.arena, types, ctx.roots, -n),
            IntVal::Big(z) => new_int_from_bigint(ctx.arena, types, ctx.roots, &-z),
        },
        UnaryOp::Invert => match val {
            IntVal::Small(n) => new_int_from_i64(ctx.arena, types, ctx.roots, !n),
            IntVal::Big(z) => new_int_from_bigint(ctx.arena, types, ctx.roots, &!z),
        },
        UnaryOp::Abs => match val {
            IntVal::Small(n) => new_int_from_i64(ctx.arena, types, ctx.roots, n.abs()),
            IntVal::Big(z) => new_int_from_bigint(ctx.arena, types, ctx.roots, &z.abs()),
        },
        UnaryOp::IntMaybe => v,
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(PrimOutcome::Value(out))
}

/// Thin adapter to the frozen `UnaryOpFn` slot signature
/// (`slots.rs`): reads the type registry from `CallCtx.types` and
/// converts the rich result with `prim_to_pyresult` — a value passes
/// through, `NotHandled` becomes `Err(PyError::PyRaise)` (the C6
/// "not implemented" convention: the caller falls through), and a
/// raise keeps only its `PyError` signal (the raise data is dropped
/// at the slot boundary — the accepted C6 convention until C13 adds
/// `pending_exc`).
pub fn int_unary_slot(ctx: &mut CallCtx<'_>, op: UnaryOp, v: Value) -> PyResult<Value> {
    let types = ctx.types;
    prim_to_pyresult(int_unary_op(ctx, types, op, v))
}

// ── `mp_obj_int_binary_op` (vendor `py/objint_mpz.c`) ───────────────────

/// Binary op on an int (`lhs` is an int; small and heap uniformly).
/// `mp_binary_op` (`py/runtime.c:394`) handles `IS` / `EQUAL` /
/// `NOT_EQUAL` / `EXCEPTION_MATCH` / `IN` first, so this slot only
/// sees arithmetic, shifts and the four order comparisons.
/// Bool rhs is the `extra_cases` rule (vendor `py/objint.c:381`):
/// recurse with small int 0/1. Float/complex rhs → `NotHandled`:
/// the original delegates DIRECTLY to `mp_obj_float_binary_op` /
/// `mp_obj_complex_binary_op` (C18's float/complex lanes) — the
/// reverse dispatch CANNOT cover it (the float slot has no `REVERSE_`
/// cases). `MULTIPLY` with a str/bytes/tuple/list rhs → `NotHandled`:
/// the original's `extra_cases` delegates to `mp_binary_op(MULTIPLY,
/// rhs, lhs)` — C14 must reproduce that delegation, the reverse-op
/// path alone yields `TypeError`.
pub fn int_binary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PrimResult {
    use crate::vendor::mpz_ops::{
        add_inpl, and_inpl, mul_inpl, or_inpl, shl_inpl, shr_inpl, sub_inpl, xor_inpl,
    };
    let lz: BigInt = match lhs {
        Value::SmallInt(n) => BigInt::from_int(n),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => z.clone(),
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    if let Value::Bool(b) = rhs {
        return int_binary_op(ctx, types, op, lhs, Value::SmallInt(if b { 1 } else { 0 }));
    }
    let rz: BigInt = match rhs {
        Value::SmallInt(n) => BigInt::from_int(n),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Int(z) => z.clone(),
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    let int_val = |ctx: &mut CallCtx<'_>, z: &BigInt| {
        PrimOutcome::Value(new_int_from_bigint(ctx.arena, types, ctx.roots, z))
    };
    let mut out = BigInt::zero();
    let out = match op {
        BinaryOp::Add | BinaryOp::InplaceAdd => {
            add_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::Subtract | BinaryOp::InplaceSubtract => {
            sub_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::Multiply | BinaryOp::InplaceMultiply => {
            mul_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::And | BinaryOp::InplaceAnd => {
            and_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::Or | BinaryOp::InplaceOr => {
            or_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::Xor | BinaryOp::InplaceXor => {
            xor_inpl(&mut out, &lz, &rz);
            int_val(ctx, &out)
        }
        BinaryOp::Lshift | BinaryOp::InplaceLshift | BinaryOp::Rshift | BinaryOp::InplaceRshift => {
            let shift = rz
                .as_int_checked()
                .ok_or(PrimError::OverflowWord)?;
            if shift < 0 {
                return Err(PrimError::NegativeShift);
            }
            let shift = shift as u64;
            match op {
                BinaryOp::Lshift | BinaryOp::InplaceLshift => shl_inpl(&mut out, &lz, shift),
                _ => shr_inpl(&mut out, &lz, shift),
            }
            int_val(ctx, &out)
        }
        BinaryOp::FloorDivide
        | BinaryOp::InplaceFloorDivide
        | BinaryOp::Modulo
        | BinaryOp::InplaceModulo
        | BinaryOp::Divmod => {
            if rz.is_zero() {
                return Err(PrimError::DivideByZero);
            }
            let (q, r) = lz.divmod(&rz).map_err(|_| PrimError::DivideByZero)?;
            match op {
                BinaryOp::FloorDivide | BinaryOp::InplaceFloorDivide => int_val(ctx, &q),
                BinaryOp::Modulo | BinaryOp::InplaceModulo => int_val(ctx, &r),
                _ => {
                    let qv = new_int_from_bigint(ctx.arena, types, ctx.roots, &q);
                    let rv = new_int_from_bigint(ctx.arena, types, ctx.roots, &r);
                    PrimOutcome::Value(Value::Heap(
                        ctx.alloc(Some(types.tuple), Obj::Tuple(vec![qv, rv])),
                    ))
                }
            }
        }
        BinaryOp::TrueDivide | BinaryOp::InplaceTrueDivide => {
            if rz.is_zero() {
                return Err(PrimError::DivideByZero);
            }
            PrimOutcome::Value(Value::Heap(ctx.alloc(
                Some(types.float),
                Obj::Float(lz.as_float() / rz.as_float()),
            )))
        }
        BinaryOp::Power | BinaryOp::InplacePower => {
            if rz.is_neg() {
                let base_f = lz.as_float();
                if base_f == 0.0 {
                    return Err(PrimError::DivideByZero);
                }
                PrimOutcome::Value(Value::Heap(ctx.alloc(
                    Some(types.float),
                    Obj::Float(base_f.powf(rz.as_float())),
                )))
            } else {
                int_val(ctx, &lz.pow(&rz))
            }
        }
        BinaryOp::Less => PrimOutcome::Value(Value::Bool(lz < rz)),
        BinaryOp::More => PrimOutcome::Value(Value::Bool(lz > rz)),
        BinaryOp::Equal => PrimOutcome::Value(Value::Bool(lz == rz)),
        BinaryOp::LessEqual => PrimOutcome::Value(Value::Bool(lz <= rz)),
        BinaryOp::MoreEqual => PrimOutcome::Value(Value::Bool(lz >= rz)),
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(out)
}

/// Thin adapter to the frozen `BinaryOpFn` slot signature
/// (`slots.rs`) — same conversion as `int_unary_slot`.
pub fn int_binary_slot(
    ctx: &mut CallCtx<'_>,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    prim_to_pyresult(int_binary_op(ctx, types, op, lhs, rhs))
}

// ── `mp_obj_int_pow3` (vendor `py/objint_mpz.c`) ────────────────────────

/// `pow(base, exp, modulus)`: all three must be ints (`mp_obj_is_int`
/// — bool is excluded, verified: `pow(2, 3, True)` →
/// `TypeError: pow() with 3 arguments requires integers`). Modulus 0
/// → `ValueError: divide by zero` (NOT ZeroDivisionError — verified).
/// Negative exponents return 0 (`pow(2, -1, 5) == 0`, the Lane C
/// divergence ported in `pow3_inpl`).
pub fn int_pow3(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    base: Value,
    exp: Value,
    modulus: Value,
) -> Result<Value, PrimError> {
    fn to_bigint(arena: &crate::memory::gc::GcArena, v: Value) -> Result<BigInt, PrimError> {
        match v {
            Value::SmallInt(n) => Ok(BigInt::from_int(n)),
            Value::Heap(r) => match arena.get(r) {
                Obj::Int(z) => Ok(z.clone()),
                _ => Err(PrimError::Pow3RequiresInts),
            },
            _ => Err(PrimError::Pow3RequiresInts),
        }
    }
    let bz = to_bigint(ctx.arena, base)?;
    let ez = to_bigint(ctx.arena, exp)?;
    let mz = to_bigint(ctx.arena, modulus)?;
    if mz.is_zero() {
        return Err(PrimError::Pow3ModZero);
    }
    // Unreachable after the zero check: `pow3_inpl` fails only via
    // `divmod_inpl` on a zero modulus.
    let z = bz.pow3(&ez, &mz).map_err(|_| PrimError::Pow3ModZero)?;
    Ok(new_int_from_bigint(ctx.arena, types, ctx.roots, &z))
}

// ── `mp_obj_int_formatted` / `mp_obj_int_print` ─────────────────────────

/// `mp_obj_int_formatted`: digits in `base` (2..=16) with `prefix`,
/// `base_char` for digits above 9, `comma` (0 = none, else a
/// separator every 3 digits for base 10, every 4 otherwise).
/// Small and heap ints share Lane C's `BigInt::as_str` — it is the
/// same algorithm as the Zig small-int path (digits least-significant
/// first, comma, prefix, sign).
pub fn int_format(
    v: Value,
    arena: &crate::memory::gc::GcArena,
    base: u32,
    prefix: &str,
    base_char: u8,
    comma: u8,
) -> Option<String> {
    let z: BigInt = match v {
        Value::SmallInt(n) => BigInt::from_int(n),
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => z.clone(),
            _ => return None,
        },
        _ => return None,
    };
    Some(z.as_str(base, prefix, base_char, comma))
}

/// `mp_obj_int_print`: base 10, no prefix, no comma.
pub fn int_to_string(v: Value, arena: &crate::memory::gc::GcArena) -> Option<String> {
    int_format(v, arena, 10, "", b'a', 0)
}

/// `mp_obj_int_print` (`objint.zig:330`): ignores `kind` (the Zig
/// takes `_ : c_int`) and prints the base-10 digits
/// (`mp_obj_int_formatted(..., 10, null, 0, 0)`). A non-int value
/// writes nothing (the slot is only invoked for int-typed values;
/// `int_to_string` returns `None` for anything else).
pub fn int_print(
    ctx: &mut CallCtx<'_>,
    v: Value,
    _kind: PrintKind,
    w: &mut dyn std::fmt::Write,
) {
    if let Some(s) = int_to_string(v, ctx.arena) {
        let _ = w.write_str(&s);
    }
}
