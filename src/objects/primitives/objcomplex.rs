//! Complex type — vendor `py/objcomplex.c` (full port of the type
//! slots + `mp_obj_complex_binary_op`).
//!
//! NOTE — this file ports a source the Zig build does NOT compile:
//! `objcomplex.c` is excluded from `build.zig`, and
//! `full_stubs.zig` provides `@panic` stubs for
//! `mp_obj_complex_binary_op` / `mp_obj_complex_get` /
//! `mp_obj_new_complex` / `mp_type_complex`. The reference binary
//! therefore ABORTS (rc=134) on ANY `complex(...)` call and on any
//! complex arithmetic. This lane implements complex properly from
//! the vendor C code — the brief's hard requirement 4
//! (`complex(3, 4)` must work) — and records the divergence in
//! `scripts/vendorc_float_result.txt` (precedent: the C9b/C11
//! abort-divergences, where the original aborts and Rust
//! returns/raises instead).
//!
//! The one place the Zig precheck shapes this file is the float
//! op's negative-base power raise (`objfloat.rs`): the vendor
//! float POWER arm would delegate that case to
//! `complex_binary_op` here, but the Zig `floatOpPrecheck` fires
//! first, so the delegation is unreachable in petitpy. The
//! complex op itself is still reachable — from `complex(...)`,
//! from int/float slots with a complex rhs, and from a hand-built
//! `.mpy` complex constant (C17: only bare `"Nj"` loads).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_complex_maybe, obj_get_float, BuiltinTypes};
use crate::objects::obj::{FLAG_EQ_CHECKS_OTHER_TYPE, FLAG_EQ_NOT_REFLEXIVE};
use crate::objects::primitives::objfloat::{float_hash, new_float};
use crate::objects::primitives::ops::{
    is_complex, is_str, prim_to_pyresult, static_qstr_id, str_bytes_of, PrimError,
    PrimOutcome, PrimResult, QstrResolver,
};
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::strings::mpprint::{
    mp_print_float, Print, PrintSink, MP_FLOAT_REPR_PREC,
};
use crate::objects::strings::objstr_print::PF_SHOW_SIGN;
use crate::objects::value::{wrap_small_int, GcRef, Obj, TypeData, Value};
use crate::pyerr::PyResult;
use crate::vendor::parsenum::{parse_num_complex, DecimalNum};

/// `mp_type_complex` ROM data. `dict` is the bootstrap's empty
/// placeholder (the original has no `locals_dict`). Flags are
/// `MP_TYPE_FLAG_EQ_NOT_REFLEXIVE | MP_TYPE_FLAG_EQ_CHECKS_OTHER_TYPE`
/// (`objcomplex.c:144`) — same shape as float. `attr` is
/// `complex_attr` (`.real` / `.imag`); `call` / `getiter` /
/// `subscr` / `buffer` stay `None`.
pub fn complex_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"complex").expect("MP_QSTR_complex is in the static table"),
        flags: FLAG_EQ_NOT_REFLEXIVE | FLAG_EQ_CHECKS_OTHER_TYPE,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(complex_unary_slot),
        binary_op: Some(complex_binary_slot),
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(complex_print),
        attr: Some(complex_attr),
        make_new: Some(crate::objects::objtype::make_new::complex_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// Allocate a heap complex (`mp_obj_new_complex`, `objcomplex.c:149-154`).
pub fn new_complex(ctx: &mut CallCtx<'_>, types: &BuiltinTypes, re: f64, im: f64) -> Value {
    Value::Heap(ctx.alloc(Some(types.complex), Obj::Complex { re, im }))
}

/// `mp_obj_complex_binary_op` (`objcomplex.c:166-239`): the complex
/// op with an `(f64, f64)` left operand. `rhs` converts via
/// `obj_get_complex_maybe` — `None` is `NotHandled` (the vendor
/// `NULL`). The `TRUE_DIVIDE` general branch reuses the multiply
/// body via a local computation (the vendor's `goto multiply`).
/// Only `EQUAL` is a comparison — `LESS`/`MORE`/… are
/// `NotHandled`, exactly as the vendor's `default: NULL`.
pub fn complex_binary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs_re: f64,
    lhs_im: f64,
    rhs: Value,
) -> PrimResult {
    let (rhs_re, rhs_im) = match obj_get_complex_maybe(ctx, types, rhs) {
        Some(pair) => pair,
        None => return Ok(PrimOutcome::NotHandled),
    };
    let (mut re, mut im) = (lhs_re, lhs_im);
    let out = match op {
        BinaryOp::Add | BinaryOp::InplaceAdd => {
            re += rhs_re;
            im += rhs_im;
            new_complex(ctx, types, re, im)
        }
        BinaryOp::Subtract | BinaryOp::InplaceSubtract => {
            re -= rhs_re;
            im -= rhs_im;
            new_complex(ctx, types, re, im)
        }
        BinaryOp::Multiply | BinaryOp::InplaceMultiply => {
            let real = re * rhs_re - im * rhs_im;
            im = re * rhs_im + im * rhs_re;
            re = real;
            new_complex(ctx, types, re, im)
        }
        BinaryOp::FloorDivide | BinaryOp::InplaceFloorDivide => {
            return Err(PrimError::ComplexFloorDivide);
        }
        BinaryOp::TrueDivide | BinaryOp::InplaceTrueDivide => {
            if rhs_im == 0.0 {
                if rhs_re == 0.0 {
                    return Err(PrimError::ComplexDivideByZero);
                }
                re /= rhs_re;
                im /= rhs_re;
            } else if rhs_re == 0.0 {
                let real = im / rhs_im;
                im = -re / rhs_im;
                re = real;
            } else {
                let rhs_len_sq = rhs_re * rhs_re + rhs_im * rhs_im;
                let inv_re = rhs_re / rhs_len_sq;
                let inv_im = rhs_im / -rhs_len_sq;
                let real = re * inv_re - im * inv_im;
                im = re * inv_im + im * inv_re;
                re = real;
            }
            new_complex(ctx, types, re, im)
        }
        BinaryOp::Power | BinaryOp::InplacePower => {
            // z1**z2 = exp(z2*ln(z1)) — the vendor's comment
            // (`objcomplex.c:204-208`) is ported with the code.
            let abs1 = (re * re + im * im).sqrt();
            if abs1 == 0.0 {
                if rhs_im == 0.0 && rhs_re >= 0.0 {
                    re = if rhs_re == 0.0 { 1.0 } else { 0.0 };
                    im = 0.0;
                } else {
                    return Err(PrimError::ComplexZeroPower);
                }
            } else {
                let ln1 = abs1.ln();
                let arg1 = im.atan2(re);
                let x3 = rhs_re * ln1 - rhs_im * arg1;
                let y3 = rhs_im * ln1 + rhs_re * arg1;
                let exp_x3 = x3.exp();
                re = exp_x3 * y3.cos();
                im = exp_x3 * y3.sin();
            }
            new_complex(ctx, types, re, im)
        }
        BinaryOp::Equal => Value::Bool(re == rhs_re && im == rhs_im),
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(PrimOutcome::Value(out))
}

/// The complex type's `binary_op` slot (`objcomplex.c:127-131`):
/// the lhs is an exact complex; the op itself is
/// `complex_binary_op` above.
pub fn complex_binary_slot_rich(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PrimResult {
    let (re, im) = match lhs {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Complex { re, im } => (*re, *im),
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    complex_binary_op(ctx, types, op, re, im, rhs)
}

/// `mp_obj_complex_unary_op` (`objcomplex.c:109-125`): `BOOL` /
/// `HASH` / `POSITIVE` / `NEGATIVE` / `ABS`; everything else —
/// notably `IntMaybe` / `FloatMaybe` / `ComplexMaybe` — is
/// `NotHandled` (answered by `obj_get_*_maybe` before the slot).
pub fn complex_unary_op(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    op: UnaryOp,
    v: Value,
) -> PrimResult {
    let (re, im) = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Complex { re, im } => (*re, *im),
            _ => return Ok(PrimOutcome::NotHandled),
        },
        _ => return Ok(PrimOutcome::NotHandled),
    };
    let out = match op {
        UnaryOp::Bool => Value::Bool(re != 0.0 || im != 0.0),
        // `MP_OBJ_NEW_SMALL_INT(mp_float_hash(real) ^
        // mp_float_hash(imag))` — the XOR is on the raw
        // `mp_float_hash` values, wrapped once at the end.
        UnaryOp::Hash => Value::SmallInt(wrap_small_int(float_hash(re) ^ float_hash(im))),
        UnaryOp::Positive => v,
        UnaryOp::Negative => new_complex(ctx, types, -re, -im),
        UnaryOp::Abs => new_float(ctx, types, (re * re + im * im).sqrt()),
        _ => return Ok(PrimOutcome::NotHandled),
    };
    Ok(PrimOutcome::Value(out))
}

/// `complex_make_new` (`objcomplex.c:71-107`): sig `(0, 2, false)`.
/// 0 args → `(0, 0)`; 1 arg: a str parses with
/// `parse_num_complex` (C17 — this is its first in-tree caller);
/// an exact complex returns as-is; anything else goes through
/// `obj_get_complex` (`TypeError: can't convert to complex`);
/// 2 args (and the `default:` fall-through for 3+): the
/// **vendor** 2-arg quirk, ported verbatim — it differs from
/// CPython: arg0 complex → `(re0, im0)`, else
/// `(get_float(arg0), 0)`; arg1 complex → `real -= im1;
/// imag += re1`, else `imag += get_float(arg1)`. So
/// `complex(1+2j, 3+4j)` is `(1-4, 2+3) = (-3, 5)` —
/// `complex(1, 2)` is `(1, 2)`, but `complex(complex(1, 2),
/// complex(3, 4))` is `(-3, 5)`, not `(1, 2) + (3, 4)`.
///
/// Same polled arg-check rule as `int_make_new` / `float_make_new`:
/// the branch's own error wins, else a wrong count surfaces as
/// `ArgNumTypesMismatch`.
pub fn complex_make_new(
    ctx: &mut CallCtx<'_>,
    types: &BuiltinTypes,
    resolver: QstrResolver<'_>,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, PrimError> {
    let arg_ok = args.len() <= 2 && n_kw == 0;
    // The flat layout is `[pos..., kw_key, kw_val, ...]` — the
    // branch runs on the POSITIONALS only (`n_pos`); see
    // `float_make_new` for why (`complex(x=1)` must raise
    // `ArgNumTypesMismatch`, not parse the key `"x"`).
    let n_pos = args.len().saturating_sub(2 * n_kw);
    let r = match n_pos {
        0 => Ok(new_complex(ctx, types, 0.0, 0.0)),
        1 => {
            if is_str(ctx.arena, types, args[0]) {
                match str_bytes_of(ctx.arena, types, resolver, args[0]) {
                    Some(bytes) => match parse_num_complex(&bytes) {
                        Ok(DecimalNum::Complex { re, im }) => {
                            Ok(new_complex(ctx, types, re, im))
                        }
                        // `force_complex` never yields a plain
                        // float; treat it as real-only if it ever
                        // does, like the vendor's constructor.
                        Ok(DecimalNum::Float(f)) => Ok(new_complex(ctx, types, f, 0.0)),
                        Err(_) => Err(PrimError::InvalidSyntaxForNumber),
                    },
                    None => Err(PrimError::InvalidSyntaxForNumber),
                }
            } else if is_complex(ctx.arena, types, args[0]) {
                Ok(args[0])
            } else {
                match obj_get_complex_maybe(ctx, types, args[0]) {
                    Some((re, im)) => Ok(new_complex(ctx, types, re, im)),
                    None => Err(PrimError::ComplexConversion),
                }
            }
        }
        _ => {
            let (mut re, mut im) = if is_complex(ctx.arena, types, args[0]) {
                match obj_get_complex_maybe(ctx, types, args[0]) {
                    Some(pair) => pair,
                    None => return Err(PrimError::ComplexConversion),
                }
            } else {
                (
                    obj_get_float(ctx, types, args[0])
                        .map_err(|_| PrimError::FloatConversion)?,
                    0.0,
                )
            };
            if is_complex(ctx.arena, types, args[1]) {
                match obj_get_complex_maybe(ctx, types, args[1]) {
                    Some((re2, im2)) => {
                        re -= im2;
                        im += re2;
                    }
                    None => return Err(PrimError::ComplexConversion),
                }
            } else {
                im += obj_get_float(ctx, types, args[1])
                    .map_err(|_| PrimError::FloatConversion)?;
            }
            Ok(new_complex(ctx, types, re, im))
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

/// `complex_print` (`objcomplex.c:50-68`): `real != 0` → `"(" +
/// float(real, 'g', 0) + float(imag, 'g', SHOW_SIGN) + "j)"`,
/// else `float(imag, 'g', 0) + "j"`. `kind` is ignored.
pub fn complex_print(
    ctx: &mut CallCtx<'_>,
    v: Value,
    _kind: PrintKind,
    w: &mut dyn std::fmt::Write,
) {
    let (re, im) = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Complex { re, im } => (*re, *im),
            _ => return,
        },
        _ => return,
    };
    let mut sink = WriteSink(w);
    let mut print = Print::new(&mut sink);
    if re != 0.0 {
        let _ = print.emit(b"(");
        let _ = mp_print_float(&mut print, re, b'g', 0, 0, -1, MP_FLOAT_REPR_PREC);
        let _ = mp_print_float(&mut print, im, b'g', PF_SHOW_SIGN, 0, -1, MP_FLOAT_REPR_PREC);
        let _ = print.emit(b"j)");
    } else {
        let _ = mp_print_float(&mut print, im, b'g', 0, 0, -1, MP_FLOAT_REPR_PREC);
        let _ = print.emit(b"j");
    }
}

/// A `fmt::Write` → `PrintSink` bridge — same shape as the one in
/// `objfloat.rs` (a local copy: the two modules do not share a
/// helper, and the bridge is four lines).
struct WriteSink<'a>(&'a mut dyn std::fmt::Write);

impl PrintSink for WriteSink<'_> {
    fn emit(&mut self, bytes: &[u8]) -> Result<(), crate::objects::strings::objstr_a::StrError> {
        let s = std::str::from_utf8(bytes).unwrap_or("");
        let _ = self.0.write_str(s);
        Ok(())
    }
}

/// `complex_attr` (`objcomplex.c:133-144`): load-only (a store is
/// not a load — `dest[0] != NULL` returns without writing).
/// `.real` / `.imag` answer a new float; anything else is `None`
/// (the caller falls through to the generic attribute path).
/// `real` / `imag` are static qstrs (`qstrdefs.generated.h:457/545`).
pub fn complex_attr(
    ctx: &mut CallCtx<'_>,
    self_in: Value,
    attr: u32,
) -> PyResult<Option<Value>> {
    let (re, im) = match self_in {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Complex { re, im } => (*re, *im),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let types = ctx.types;
    if attr == static_qstr_id(b"real").expect("MP_QSTR_real is in the static table") {
        Ok(Some(new_float(ctx, types, re)))
    } else if attr == static_qstr_id(b"imag").expect("MP_QSTR_imag is in the static table") {
        Ok(Some(new_float(ctx, types, im)))
    } else {
        Ok(None)
    }
}

/// Thin adapter to the frozen `UnaryOpFn` slot signature.
pub fn complex_unary_slot(ctx: &mut CallCtx<'_>, op: UnaryOp, v: Value) -> PyResult<Value> {
    let types = ctx.types;
    let r = complex_unary_op(ctx, types, op, v);
    prim_to_pyresult(ctx, r)
}

/// Thin adapter to the frozen `BinaryOpFn` slot signature.
pub fn complex_binary_slot(
    ctx: &mut CallCtx<'_>,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    let r = complex_binary_slot_rich(ctx, types, op, lhs, rhs);
    prim_to_pyresult(ctx, r)
}
