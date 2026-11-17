//! Float type — vendor `py/objfloat.c` (339 lines, full port of the
//! type slots + `mp_obj_float_binary_op` / `mp_obj_float_divmod`).
//!
//! `Obj::Float(f64)` values already exist (C0); this lane wires the
//! type: `make_new` (`float()`), `print`, `unary_op`, `binary_op`
//! (`mp_obj_float_binary_op`), and the `IntMaybe` slot — which
//! deliberately stays `NotHandled`: the vendor float type has no
//! `IntMaybe` case, so `"%o" % 3.0` raises `TypeError: can't convert
//! to int` (the C8b hard requirement, probed on the reference
//! binary 2026-11-18).
//!
//! Raises inside the float op follow the **Zig** `floatOpPrecheck`
//! (`runtime.zig:487-498`), not the vendor C arms: vendor C raises
//! by longjmp, which the VM's `try/except` cannot catch (ADR 013),
//! so the Zig raises them catchably first — `ZeroDivisionError`
//! for a zero divisor or zero to a negative power, `ValueError:
//! complex values not supported` for a negative base to a
//! fractional power. The vendor POWER arm would delegate that case
//! to the complex op; the precheck fires first in petitpy, so the
//! delegation is dead code here and is NOT ported (the complex op
//! is a `@panic` stub in the reference anyway — see the result
//! file).
//!
//! `mp_float_hash` is the SIMPLE version (`obj.h:1169-1171`:
//! `(mp_int_t)val`) because `MICROPY_FLOAT_HIGH_QUALITY_HASH` is
//! OFF (`ROM_LEVEL_MINIMUM`, `port/mpconfigport.h:35`). The C cast
//! is undefined for NaN / inf / out-of-range values; in practice
//! `cvttsd2si` yields `INT64_MIN` there, which `float_hash`
//! reproduces explicitly (Rust's saturating `as` cast would give
//! `i64::MAX` for large positives instead).
//!
//! `objint.c` / `objint_mpz.c` need no new code here: C7's
//! `objint.rs` / `ops.rs` already ports every entry point (see
//! `scripts/vendorc_float_result.txt` for the per-function table);
//! the only gap is the float/complex-rhs delegation in
//! `int_binary_op`, added by this lane in `objint.rs`.

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    obj_get_float_maybe, BuiltinTypes, FLAG_EQ_CHECKS_OTHER_TYPE, FLAG_EQ_NOT_REFLEXIVE,
};
use crate::objects::primitives::objcomplex::complex_binary_op;
use crate::objects::primitives::ops::{
    is_complex, is_float, prim_to_pyresult, static_qstr_id, str_bytes_of, PrimError,
    PrimOutcome, PrimResult, QstrResolver,
};
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::strings::mpprint::{
    mp_print_float, Print, PrintSink, MP_FLOAT_REPR_PREC, PF_FLAG_ALWAYS_DECIMAL,
};
use crate::objects::value::{wrap_small_int, GcRef, Obj, TypeData, Value};
use crate::pyerr::PyResult;
use crate::vendor::parsenum::{parse_num_float, DecimalNum};

/// `mp_type_float` ROM data. `dict` is the bootstrap's empty
/// placeholder (the original has no `locals_dict`). Flags are
/// `MP_TYPE_FLAG_EQ_NOT_REFLEXIVE | MP_TYPE_FLAG_EQ_CHECKS_OTHER_TYPE`
/// (`objfloat.c:175`) — float equality is cross-type (`1 == 1.0`),
/// and a float is never reflexively equal to itself by identity
/// (`nan != nan`). `attr` / `call` / `getiter` / `subscr` / `buffer`
/// stay `None` — the original's float type has no such slots.
pub fn float_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"float").expect("MP_QSTR_float is in the static table"),
        flags: FLAG_EQ_NOT_REFLEXIVE | FLAG_EQ_CHECKS_OTHER_TYPE,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(float_unary_slot),
        binary_op: Some(float_binary_slot),
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(float_print),
        attr: None,
        make_new: Some(crate::objects::objtype::make_new::float_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// Allocate a heap float (`mp_obj_new_float`, `objfloat.c:184-190`).
pub fn new_float(ctx: &mut CallCtx<'_>, types: &BuiltinTypes, f: f64) -> Value {
    Value::Heap(ctx.alloc(Some(types.float), Obj::Float(f)))
}

/// `mp_float_hash` — the simple version (`obj.h:1169-1171`):
/// `(mp_int_t)val`, a C truncation toward zero. NaN, infinities,
/// and magnitudes `>= 2^63` have no defined C result; the x86-64
/// `cvttsd2si` instruction yields `INT64_MIN` for all of them,
/// which is what the reference binary produces (probed 2026-11-18:
/// `hash(float('inf')) == 0` only after `wrap_small_int` below —
/// the raw hash here is `i64::MIN`; `hash(1e30) == 0` likewise).
/// The caller wraps with `wrap_small_int` for the `SMALL_INT`
/// return, exactly as the vendor's `MP_OBJ_NEW_SMALL_INT` does.
pub fn float_hash(f: f64) -> i64 {
    if f.is_nan() || f >= 2f64.powi(63) || f < -2f64.powi(63) {
        i64::MIN
    } else {
        f as i64
    }
}

/// `mp_obj_float_divmod` (`objfloat.c:201-233`, CPython logic):
/// `divmod(x, y) == (x // y, x % y)` with `x == (x//y)*y + (x%y)`.
/// Returns `(floordiv, mod)` — the caller stores them back in
/// that order (`*x = floordiv; *y = mod`).
pub fn float_divmod(x: f64, y: f64) -> (f64, f64) {
    let mut m = x % y;
    let mut div = (x - m) / y;
    if m == 0.0 {
        m = 0.0f64.copysign(y);
    } else if (m < 0.0) != (y < 0.0) {
        m += y;
        div -= 1.0;
    }
    let floordiv = if div == 0.0 {
        0.0f64.copysign(x / y)
    } else {
        let fl = div.floor();
        if div - fl > 0.5 {
            fl + 1.0
        } else {
            fl
        }
    };
    (floordiv, m)
}

/// `mp_obj_float_binary_op` (`objfloat.c:235-339`): the float op
/// with an `f64` left operand. `rhs` converts via
/// `obj_get_float_maybe` — `None` is `NotHandled` (the vendor
/// `NULL`). The raising arms carry the Zig `floatOpPrecheck`
/// semantics (see the module doc): zero divisor → `DivideByZero`;
/// `0.0 ** negative` → `DivideByZero` (no `isinf` exclusion —
/// `0.0 ** -inf` raises, probed); negative base to a fractional
/// power → `ComplexNotSupported` (`rhs != trunc(rhs)`, so NaN
/// counts — `(-8.0) ** nan` raises, probed; `inf` does NOT count
/// — `(-8.0) ** inf == inf`, probed).
///
/// `POW_FIX_NAN` (`objfloat.c:303-312`) is OFF
/// (`MICROPY_PY_MATH_POW_FIX_NAN` defaults to 0, `mpconfig.h:1668`)
/// and unneeded: Rust's `powf` already returns `1.0` for
/// `1.0 ** nan` and `nan ** 0.0`, matching the reference probes.
pub fn float_binary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs_val: f64,
    rhs: Value,
) -> PrimResult {
    let rhs_val = match obj_get_float_maybe(ctx, types, rhs) {
        Some(f) => f,
        None => return Ok(PrimOutcome::NotHandled),
    };
    let out = match op {
        BinaryOp::Add | BinaryOp::InplaceAdd => new_float(ctx, types, lhs_val + rhs_val),
        BinaryOp::Subtract | BinaryOp::InplaceSubtract => {
            new_float(ctx, types, lhs_val - rhs_val)
        }
        BinaryOp::Multiply | BinaryOp::InplaceMultiply => {
            new_float(ctx, types, lhs_val * rhs_val)
        }
        BinaryOp::FloorDivide | BinaryOp::InplaceFloorDivide => {
            if rhs_val == 0.0 {
                return Err(PrimError::DivideByZero);
            }
            let (q, _) = float_divmod(lhs_val, rhs_val);
            new_float(ctx, types, q)
        }
        BinaryOp::TrueDivide | BinaryOp::InplaceTrueDivide => {
            if rhs_val == 0.0 {
                return Err(PrimError::DivideByZero);
            }
            new_float(ctx, types, lhs_val / rhs_val)
        }
        BinaryOp::Modulo | BinaryOp::InplaceModulo => {
            if rhs_val == 0.0 {
                return Err(PrimError::DivideByZero);
            }
            let mut m = lhs_val % rhs_val;
            if m == 0.0 {
                m = 0.0f64.copysign(rhs_val);
            } else if (m < 0.0) != (rhs_val < 0.0) {
                m += rhs_val;
            }
            new_float(ctx, types, m)
        }
        BinaryOp::Power | BinaryOp::InplacePower => {
            if lhs_val == 0.0 && rhs_val < 0.0 {
                return Err(PrimError::DivideByZero);
            }
            if lhs_val < 0.0 && rhs_val != rhs_val.trunc() {
                return Err(PrimError::ComplexNotSupported);
            }
            new_float(ctx, types, lhs_val.powf(rhs_val))
        }
        BinaryOp::Divmod => {
            if rhs_val == 0.0 {
                return Err(PrimError::DivideByZero);
            }
            let (q, r) = float_divmod(lhs_val, rhs_val);
            let qv = new_float(ctx, types, q);
            let rv = new_float(ctx, types, r);
            let tuple = crate::objects::containers::objtuple::new_tuple(
                ctx,
                types.tuple,
                &[qv, rv],
            );
            Value::Heap(tuple)
        }
        BinaryOp::Less => Value::Bool(lhs_val < rhs_val),
        BinaryOp::More => Value::Bool(lhs_val > rhs_val),
        BinaryOp::Equal => Value::Bool(lhs_val == rhs_val),
        BinaryOp::LessEqual => Value::Bool(lhs_val <= rhs_val),
        BinaryOp::MoreEqual => Value::Bool(lhs_val >= rhs_val),
        // No `NotEqual` case in the vendor (the equality protocol
        // handles it), no reverse cases, no bit ops — all
        // `NotHandled`, exactly as the vendor's `NULL`.
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(PrimOutcome::Value(out))
}

/// The float type's `binary_op` slot (`objfloat.c:163-172`): the
/// lhs is an exact float; an exact-complex rhs delegates to the
/// complex op first (`mp_obj_is_type(rhs, &mp_type_complex)`),
/// otherwise the float op above.
pub fn float_binary_slot_rich(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PrimResult {
    let lhs_val = match lhs {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Float(f) => *f,
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    if is_complex(ctx.arena, types, rhs) {
        return complex_binary_op(ctx, types, op, lhs_val, 0.0, rhs);
    }
    float_binary_op(ctx, types, op, lhs_val, rhs)
}

/// `mp_obj_float_unary_op` (`objfloat.c:143-161`): `BOOL` /
/// `HASH` / `POSITIVE` / `NEGATIVE` / `ABS`; everything else —
/// notably `IntMaybe` (there is no such case: the float type
/// cannot convert to int, which is what makes `"%o" % 3.0`
/// raise) and `FloatMaybe` (answered by `obj_get_float_maybe`
/// before the slot) — is `NotHandled`.
pub fn float_unary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: UnaryOp,
    v: Value,
) -> PrimResult {
    let val = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Float(f) => *f,
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    let out = match op {
        UnaryOp::Bool => Value::Bool(val != 0.0),
        UnaryOp::Hash => Value::SmallInt(wrap_small_int(float_hash(val))),
        UnaryOp::Positive => v,
        UnaryOp::Negative => new_float(ctx, types, -val),
        UnaryOp::Abs => {
            if val.is_sign_negative() {
                new_float(ctx, types, -val)
            } else {
                v
            }
        }
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(PrimOutcome::Value(out))
}

/// `float_make_new` (`objfloat.c:123-141`): sig `(0, 1, false)`.
/// 0 args → `0.0`; 1 arg: a str/bytes buffer parses with
/// `parse_num_float(buf, false)`; an exact float returns as-is;
/// anything else goes through `obj_get_float_maybe` (`None` →
/// `TypeError: can't convert to float`). The same polled
/// arg-check rule as `int_make_new`: the branch's own error
/// wins, else a wrong count surfaces as `ArgNumTypesMismatch`
/// (`float(1, 2)` → that TypeError, probed).
pub fn float_make_new(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, PrimError> {
    let arg_ok = args.len() <= 1 && n_kw == 0;
    // The flat layout is `[pos..., kw_key, kw_val, ...]` — the
    // branch runs on the POSITIONALS only (`n_pos`). Without this,
    // `float(x=1)` would parse the KEY (`"x"`) as the value and
    // raise `InvalidSyntaxForNumber` instead of the polled
    // `ArgNumTypesMismatch` (the same latent bug `int_make_new`
    // had — fixed alongside this lane).
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let r = match n_pos {
        0 => Ok(new_float(ctx, types, 0.0)),
        _ => {
            if let Some(bytes) = str_bytes_of(ctx.arena, types, resolver, args[0]) {
                match parse_num_float(&bytes, false) {
                    Ok(DecimalNum::Float(f)) => Ok(new_float(ctx, types, f)),
                    // `allow_imag = false` never yields a complex,
                    // but the variant exists — treat it as a
                    // syntax failure, like the vendor's `NULL`
                    // endptr would.
                    Ok(DecimalNum::Complex { .. }) | Err(_) => {
                        Err(PrimError::InvalidSyntaxForNumber)
                    }
                }
            } else if is_float(ctx.arena, types, args[0]) {
                Ok(args[0])
            } else {
                match obj_get_float_maybe(ctx, types, args[0]) {
                    Some(f) => Ok(new_float(ctx, types, f)),
                    None => Err(PrimError::FloatConversion),
                }
            }
        }
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

/// A `fmt::Write` → `PrintSink` bridge: the print slot receives
/// `&mut dyn fmt::Write` (`obj_print_helper`), while
/// `mp_print_float` writes through a `Print` (`PrintSink`).
/// Errors are ignored, as the original ignores `mp_printf`'s
/// return. Float output is ASCII, so a lossy conversion cannot
/// fire — `from_utf8` falling back to `""` is unreachable.
struct WriteSink<'a>(&'a mut dyn std::fmt::Write);

impl PrintSink for WriteSink<'_> {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), crate::objects::strings::objstr_a::StrError> {
        let s = std::str::from_utf8(bytes).unwrap_or("");
        let _ = self.0.write_str(s);
        Ok(())
    }
}

/// `float_print` (`objfloat.c:118-122`): `mp_print_float(print,
/// val, 'g', PF_FLAG_ALWAYS_DECIMAL, '\0', -1, MP_FLOAT_REPR_PREC)`
/// — the `prec == 99` repr path (shortest round-trip, C8c).
/// `kind` is ignored, like the vendor's `(void)kind`.
pub fn float_print(
    ctx: &mut CallCtx<'_>,
    v: Value,
    _kind: PrintKind,
    w: &mut dyn std::fmt::Write,
) {
    let val = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Float(f) => *f,
            _ => return,
        },
        _ => return,
    };
    let mut sink = WriteSink(w);
    let mut print = Print::new(&mut sink);
    let _ = mp_print_float(
        &mut print,
        val,
        b'g',
        PF_FLAG_ALWAYS_DECIMAL,
        0,
        -1,
        MP_FLOAT_REPR_PREC,
    );
}

/// Thin adapter to the frozen `UnaryOpFn` slot signature.
pub fn float_unary_slot(ctx: &mut CallCtx<'_>, op: UnaryOp, v: Value) -> PyResult<Value> {
    let types = ctx.types;
    let r = float_unary_op(ctx, types, op, v);
    prim_to_pyresult(ctx, r)
}

/// Thin adapter to the frozen `BinaryOpFn` slot signature.
pub fn float_binary_slot(
    ctx: &mut CallCtx<'_>,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    let r = float_binary_slot_rich(ctx, types, op, lhs, rhs);
    prim_to_pyresult(ctx, r)
}
