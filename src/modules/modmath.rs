//! The `math` builtin module (`src/modules/modmath.zig`, 441 lines).
//!
//! Ported from the Zig (`modmath.zig` — itself a port of vendor
//! `py/modmath.c`): the `generic1`/`generic2` libm wrappers with the
//! vendor domain-error predicate (`py/modmath.c:44-61`), the
//! `MATH_FUN_1_TO_INT` ceil/floor/trunc, the `MATH_FUN_1_TO_BOOL`
//! isnan/isinf/isfinite, `ldexp`/`log`/`frexp`/`modf`/
//! `radians`/`degrees`/`gcd`, and the five float constants.
//!
//! Faithful points (all probed on the reference binary 2026-11-18,
//! see `scripts/d3_result.txt` §2):
//! - `log2`/`log10` are NOT libm `log2`/`log10`: the vendor's
//!   portable form is `log(x) * (1/ln 2)` / `log(x) * (1/ln 10)`
//!   (`log2(8) == 2.9999999999999996`, not `3.0`).
//! - Domain errors are `ValueError("math domain error")` — including
//!   `exp(1000)` and `pow(0, -1)` (CPython raises `OverflowError`
//!   for those; petitpy does not).
//! - `ceil`/`floor`/`trunc` of inf raise `OverflowError("can't
//!   convert inf to int")`, of NaN `ValueError("can't convert NaN
//!   to int")` (C7's `int_from_float`).
//! - `gcd` is Euclid over int objects (big ints work);
//!   `gcd()` is `0`; bools count as ints; anything else is
//!   `TypeError("gcd() requires integers")`.
//! - `ldexp`'s exponent is a C `int` in the original: an out-of-range
//!   exponent ABORTS the reference (`@intCast` panic, rc=134). Rust
//!   saturates the exponent instead (documented divergence).
//! - There is NO `factorial` / `isclose` / `gamma` in this module
//!   (nano config gates them off; the table has no such entries).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::{is_exact_int, obj_get_float_maybe, obj_get_int};
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::{
    int_from_float, new_int_from_bigint, static_qstr_id, PrimError,
};
use crate::objects::value::{FunBuiltinData, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::vendor::mpz::BigInt;

/// `mp_obj_get_float` (`objfloat.c`): int/bool/float → `f64`;
/// anything else is `TypeError: can't convert to float`
/// (`math.ceil('x')`, probed).
fn get_float(ctx: &mut CallCtx, v: Value) -> Result<f64, PyError> {
    let types = ctx.types;
    match obj_get_float_maybe(ctx, types, v) {
        Some(f) => Ok(f),
        None => Err(ctx.record_typed(PrimError::FloatConversion)),
    }
}

/// `raiseDomainError` (`modmath.zig:50-56`): `ValueError("math
/// domain error")`.
fn domain_error(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("ValueError", Some("math domain error"));
    PyError::PyRaise
}

/// `raiseDivByZero` (`modmath.zig:57-62`): `ZeroDivisionError
/// ("divide by zero")` — only `log(x, 1)`.
fn div_by_zero(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("ZeroDivisionError", Some("divide by zero"));
    PyError::PyRaise
}

/// `generic1` (`modmath.zig:76-85`): domain error if the result is
/// NaN/inf but the input wasn't.
fn generic1(ctx: &mut CallCtx, x: Value, f: fn(f64) -> f64) -> PyResult<Value> {
    let x = get_float(ctx, x)?;
    let ans = f(x);
    if (ans.is_nan() && !x.is_nan()) || (ans.is_infinite() && !x.is_infinite()) {
        return Err(domain_error(ctx));
    }
    Ok(new_float(ctx, ctx.types, ans))
}

/// `generic2` (`modmath.zig:87-97`): the 2-arg form.
fn generic2(ctx: &mut CallCtx, x: Value, y: Value, f: fn(f64, f64) -> f64) -> PyResult<Value> {
    let x = get_float(ctx, x)?;
    let y = get_float(ctx, y)?;
    let ans = f(x, y);
    if (ans.is_nan() && !x.is_nan() && !y.is_nan())
        || (ans.is_infinite() && !x.is_infinite() && !y.is_infinite())
    {
        return Err(domain_error(ctx));
    }
    Ok(new_float(ctx, ctx.types, ans))
}

/// `MATH_FUN_1_TO_INT` (`modmath.c`): ceil/floor/trunc return an int.
fn to_int1(ctx: &mut CallCtx, x: Value, f: fn(f64) -> f64) -> PyResult<Value> {
    let x = get_float(ctx, x)?;
    let y = f(x);
    int_from_float(ctx.arena, ctx.types, &ctx.roots, y).map_err(|e| ctx.record_typed(e))
}

/// `MATH_FUN_1_TO_BOOL`: isnan/isinf/isfinite return a bool.
fn to_bool1(ctx: &mut CallCtx, x: Value, f: fn(f64) -> bool) -> PyResult<Value> {
    let x = get_float(ctx, x)?;
    Ok(Value::Bool(f(x)))
}

fn math_sin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::sin)
}
fn math_cos(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::cos)
}
fn math_tan(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::tan)
}
fn math_asin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::asin)
}
fn math_acos(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::acos)
}
fn math_atan(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::atan)
}
fn math_exp(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::exp)
}
fn math_sqrt(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::sqrt)
}
fn math_fabs(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], f64::abs)
}
fn math_log2(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // Vendor portable form (`modmath.zig:168-170`): `log(x) *
    // (1/ln 2)` — NOT libm `log2` (see module docs).
    generic1(ctx, args[0], |x| {
        x.ln() * 1.442695040888963407354163704
    })
}
fn math_log10(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // Vendor portable form (`modmath.zig:171-173`).
    generic1(ctx, args[0], |x| {
        x.ln() * 0.4342944819032518276511289189
    })
}
fn math_atan2(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic2(ctx, args[0], args[1], f64::atan2)
}
fn math_hypot(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic2(ctx, args[0], args[1], f64::hypot)
}
fn math_pow(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic2(ctx, args[0], args[1], f64::powf)
}
fn math_copysign(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic2(ctx, args[0], args[1], f64::copysign)
}
fn math_fmod(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // Rust `%` on `f64` is C `fmod` (truncated remainder).
    generic2(ctx, args[0], args[1], |x, y| x % y)
}
fn math_ceil(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_int1(ctx, args[0], f64::ceil)
}
fn math_floor(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_int1(ctx, args[0], f64::floor)
}
fn math_trunc(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_int1(ctx, args[0], f64::trunc)
}
fn math_isnan(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_bool1(ctx, args[0], f64::is_nan)
}
fn math_isinf(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_bool1(ctx, args[0], f64::is_infinite)
}
fn math_isfinite(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    to_bool1(ctx, args[0], f64::is_finite)
}
fn math_radians(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], |x| x * (std::f64::consts::PI / 180.0))
}
fn math_degrees(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    generic1(ctx, args[0], |x| x * (180.0 / std::f64::consts::PI))
}

/// `py_math_log` (`modmath.zig:243-254`): `log(x[, base])`, sig
/// (1, 2). `x <= 0` and `base <= 0` are domain errors;
/// `base == 1` is `ZeroDivisionError("divide by zero")`.
fn math_log(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let x = get_float(ctx, args[0])?;
    if x <= 0.0 {
        return Err(domain_error(ctx));
    }
    let l = x.ln();
    if args.len() == 1 {
        return Ok(new_float(ctx, ctx.types, l));
    }
    let base = get_float(ctx, args[1])?;
    if base <= 0.0 {
        return Err(domain_error(ctx));
    }
    if base == 1.0 {
        return Err(div_by_zero(ctx));
    }
    Ok(new_float(ctx, ctx.types, l / base.ln()))
}

/// `py_math_ldexp` (`modmath.zig:230-235`): `ldexp(x, exp)`,
/// float × int → float. The exponent is a C `int` in the
/// original (out-of-range aborts the reference, rc=134);
/// Rust saturates it to `i32` (see module docs).
///
/// Scaling is done in ≤1023-exponent chunks: `2f64.powi(e)`
/// underflows to `0.0` for `e ≤ -1075` (it computes `1 /
/// 2^-e`, and `2^1075` is already `inf`), so a single multiply
/// would lose `ldexp(1.0, -1074) == 5e-324` (probed). Each
/// chunk multiply by a power of two is exact whenever the
/// result is representable, so chunking matches C `ldexp`.
fn math_ldexp(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let x = get_float(ctx, args[0])?;
    let e = obj_get_int(ctx, ctx.types, args[1]).map_err(|e| ctx.record_typed(e))?;
    let e = e.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if x == 0.0 || !x.is_finite() {
        // `0.0 * inf` would be NaN; C `ldexp` returns `x` here.
        return Ok(new_float(ctx, ctx.types, x));
    }
    let mut result = x;
    let mut e = e;
    while e > 0 {
        let step = e.min(1023);
        result *= 2f64.powi(step);
        e -= step;
        if result.is_infinite() {
            break;
        }
    }
    while e < 0 {
        let step = (-e).min(1023);
        result *= 2f64.powi(-step);
        e += step;
        if result == 0.0 {
            break;
        }
    }
    Ok(new_float(ctx, ctx.types, result))
}

/// C `frexp` via bit decomposition: `x = m * 2^e` with `m` in
/// `[0.5, 1.0)` (sign kept); `(0.0, 0)` / `(inf, 0)` / `(nan, 0)`
/// for the special values (`modmath.zig:256-264`, probed).
fn frexp_impl(x: f64) -> (f64, i32) {
    if x == 0.0 || x.is_nan() || x.is_infinite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let sign = bits & 0x8000_0000_0000_0000;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: scale up by 2^64 into the normal range.
        let (m, e) = frexp_impl(x * 2f64.powi(64));
        return (m, e - 64);
    }
    let mant_bits = sign | (0x3fe << 52) | (bits & 0x000f_ffff_ffff_ffff);
    (f64::from_bits(mant_bits), exp - 1022)
}

fn math_frexp(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let x = get_float(ctx, args[0])?;
    let (m, e) = frexp_impl(x);
    let types = ctx.types;
    let m_v = new_float(ctx, types, m);
    let tuple = new_tuple(ctx, types.tuple, &[m_v, Value::SmallInt(e as i64)]);
    Ok(Value::Heap(tuple))
}

/// `py_math_modf` (`modmath.zig:266-274`): `(fractional,
/// integral)`. Rust `fract`/`trunc` match C `modf` for finite
/// values; infinities need the guard (`inf.fract()` is NaN in
/// Rust, `0.0` in C).
fn math_modf(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let x = get_float(ctx, args[0])?;
    let (frac, int_part) = if x.is_nan() {
        (x, x)
    } else if x.is_infinite() {
        (x.signum() * 0.0, x)
    } else {
        (x.fract(), x.trunc())
    };
    let types = ctx.types;
    let frac_v = new_float(ctx, types, frac);
    let int_v = new_float(ctx, types, int_part);
    let tuple = new_tuple(ctx, types.tuple, &[frac_v, int_v]);
    Ok(Value::Heap(tuple))
}

/// `py_math_gcd` (`modmath.zig:327-344`): `gcd(*integers)`,
/// Euclid over int objects. `gcd()` is `0`; the result is never
/// negative; bools count as ints; anything else is
/// `TypeError("gcd() requires integers")`.
fn math_gcd(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let mut a = BigInt::from_int(0);
    for x in args {
        let z = match *x {
            Value::Bool(b) => BigInt::from_int(if b { 1 } else { 0 }),
            Value::SmallInt(n) => BigInt::from_int(n),
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Int(z) if is_exact_int(ctx.arena, ctx.types, *x) => z.clone(),
                _ => {
                    ctx.record_raise("TypeError", Some("gcd() requires integers"));
                    return Err(PyError::PyRaise);
                }
            },
            _ => {
                ctx.record_raise("TypeError", Some("gcd() requires integers"));
                return Err(PyError::PyRaise);
            }
        };
        let mut b = z.abs();
        while !b.is_zero() {
            let (_, r) = a.divmod(&b)?;
            a = b;
            b = r;
        }
    }
    Ok(new_int_from_bigint(
        ctx.arena, ctx.types, &ctx.roots, &a,
    ))
}

/// One entry of the `mp_module_math_globals_table`
/// (`modmath.zig:376-410`): name, min/max args, body.
struct MathFn {
    name: &'static str,
    min: u16,
    max: u16,
    func: crate::objects::slots::BuiltinFn,
}

/// The 28 functions, in the Zig table's order.
const MATH_FNS: &[MathFn] = &[
    MathFn { name: "sqrt", min: 1, max: 1, func: math_sqrt },
    MathFn { name: "pow", min: 2, max: 2, func: math_pow },
    MathFn { name: "exp", min: 1, max: 1, func: math_exp },
    MathFn { name: "log", min: 1, max: 2, func: math_log },
    MathFn { name: "log2", min: 1, max: 1, func: math_log2 },
    MathFn { name: "log10", min: 1, max: 1, func: math_log10 },
    MathFn { name: "cos", min: 1, max: 1, func: math_cos },
    MathFn { name: "sin", min: 1, max: 1, func: math_sin },
    MathFn { name: "tan", min: 1, max: 1, func: math_tan },
    MathFn { name: "acos", min: 1, max: 1, func: math_acos },
    MathFn { name: "asin", min: 1, max: 1, func: math_asin },
    MathFn { name: "atan", min: 1, max: 1, func: math_atan },
    MathFn { name: "atan2", min: 2, max: 2, func: math_atan2 },
    MathFn { name: "ceil", min: 1, max: 1, func: math_ceil },
    MathFn { name: "hypot", min: 2, max: 2, func: math_hypot },
    MathFn { name: "copysign", min: 2, max: 2, func: math_copysign },
    MathFn { name: "fabs", min: 1, max: 1, func: math_fabs },
    MathFn { name: "floor", min: 1, max: 1, func: math_floor },
    MathFn { name: "fmod", min: 2, max: 2, func: math_fmod },
    MathFn { name: "frexp", min: 1, max: 1, func: math_frexp },
    MathFn { name: "ldexp", min: 2, max: 2, func: math_ldexp },
    MathFn { name: "modf", min: 1, max: 1, func: math_modf },
    MathFn { name: "isfinite", min: 1, max: 1, func: math_isfinite },
    MathFn { name: "isinf", min: 1, max: 1, func: math_isinf },
    MathFn { name: "isnan", min: 1, max: 1, func: math_isnan },
    MathFn { name: "trunc", min: 1, max: 1, func: math_trunc },
    MathFn { name: "radians", min: 1, max: 1, func: math_radians },
    MathFn { name: "degrees", min: 1, max: 1, func: math_degrees },
    MathFn { name: "gcd", min: 0, max: 255, func: math_gcd },
];

/// Build the `math` module object (`modmath.zig:376-427`):
/// globals `{__name__: "math", e, pi, tau, inf, nan, <28 fns>}`
/// in the C's table order. Registration into
/// `VmState.builtin_modules` is D1's, not this builder's
/// (the `build_io_module` pattern).
pub fn build_math_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let math_id = static_qstr_id(b"math").expect("math is a static qstr");
    let module = new_module(ctx, math_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let types = ctx.types;
    for (name, f) in [
        (b"e".as_slice(), std::f64::consts::E),
        (b"pi".as_slice(), std::f64::consts::PI),
        (b"tau".as_slice(), std::f64::consts::TAU),
        (b"inf".as_slice(), f64::INFINITY),
        (b"nan".as_slice(), f64::NAN),
    ] {
        let id = static_qstr_id(name).expect("math constant name is a static qstr");
        let v = new_float(ctx, types, f);
        ctx.roots.push(v);
        dict_store(ctx, dict_v, Value::Qstr(id), v).map_err(|e| ctx.record_typed(e))?;
    }
    for f in MATH_FNS {
        let id = static_qstr_id(f.name.as_bytes()).expect("math fn name is a static qstr");
        let fun = ctx.alloc(
            Some(fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args: fun_n_args(f.min, f.max),
                func: f.func,
                kw_func: None,
            }),
        );
        let v = Value::Heap(fun);
        ctx.roots.push(v);
        dict_store(ctx, dict_v, Value::Qstr(id), v).map_err(|e| ctx.record_typed(e))?;
    }
    Ok(module)
}
