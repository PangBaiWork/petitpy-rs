//! Operator dispatch (`runtime.zig:352-810`): `mp_unary_op` and
//! `mp_binary_op`.
//!
//! `mp_binary_op` is the largest function in the Zig runtime: the
//! small-int fast path (with its `go_big` escape to the generic
//! path), the float/complex promotion arms, the `IN` → `CONTAINS`
//! swap, and the generic slot loop (inplace → plain → reverse
//! dispatch → `CONTAINS` iteration → `binopUnsupported`).
//!
//! Two structural notes:
//!
//! - The float/complex promotion arms route through the exact
//!   float/complex type's binary-op slot with the plain op
//!   (`call_binary`), not through the reverse dispatch: the
//!   vendor float slot has no `REVERSE_` cases (C7's
//!   `int_binary_op` docs), so reverse dispatch cannot reach it.
//!   The slot wrapper converts the int lhs via `mp_obj_get_float`
//!   semantics — exactly what the Zig's direct
//!   `mp_obj_float_binary_op(op, lhs as f64, rhs)` call does, so
//!   the routing is observably identical once C18 wires the slots
//!   (pre-C18 the slots are absent and the arm raises the
//!   vendor-NORMAL `TypeError` — see the result file for the
//!   exact probe cases). The `floatOpPrecheck` runs first, exactly
//!   as in the Zig.
//! - The int slot's `extra_cases` delegation (`int * str/bytes /
//!   tuple/list` → recurse swapped, `objint.zig:182-200`) lives in
//!   the generic loop: C7's `int_binary_op` returns `NotHandled`
//!   for those cases instead of recursing (the slot boundary has
//!   no `mp_binary_op` handle), so this lane reproduces the
//!   delegation when the int-like lhs's slot declines a plain
//!   `MULTIPLY` with a sequence rhs. `INPLACE_MULTIPLY` reaches
//!   it after the inplace → plain conversion, exactly as in the
//!   Zig (where `extra_cases` only fires for plain `MULTIPLY`).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    call_binary, call_unary, is_bytes, is_exact_int, is_float, is_str, obj_get_type, obj_id,
    obj_is_true, type_data, ObjError, SlotOutcome, FLAG_INSTANCE_TYPE,
};
use crate::objects::obj_ops::{obj_equal};
use crate::objects::objtype::instance_create::count_native_bases;
use crate::objects::objtype::lookup::binary_op_method_name;
use crate::objects::objtype::objexcept::exception_match;
use crate::objects::objtype::subclass::instance_subobj0;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{Obj, Value};
use crate::qstr_table::qstr_compute_hash;
use crate::runtime::runtime::{
    dunder, is_native_scalar, type_name_of, Builtins, ExcRefs, RtError, RuntimeSeams,
};
use crate::runtime::runtime_call::{call_function_2, call_method_n_kw};
use crate::runtime::runtime_import::import_name;
use crate::runtime::runtime_iter::{getiter, iternext, IterNext};
use crate::runtime::runtime_method::load_attr;
use crate::runtime::runtime_method::load_method_maybe;
use crate::runtime::runtime_smallint::{small_int_binary_op, SmallIntOutcome};

// ── mp_unary_op ─────────────────────────────────────────────────────

/// `mp_unary_op` (`runtime.zig:352-435`). `Ok(None)` is the Zig's
/// `MP_OBJ_NULL` — the `*_MAYBE` "not implemented" outcome, not an
/// error. The header comment's `FLOAT/COMPLEX=0` claim is stale
/// (the brief's trap): the body at `:395-400` is authoritative and
/// the `FLOAT_MAYBE` / `COMPLEX_MAYBE` branches are live.
pub fn unary_op(
    ctx: &mut CallCtx,
    op: UnaryOp,
    arg: Value,
) -> Result<Option<Value>, RtError> {
    let types = ctx.types;
    if op == UnaryOp::Not {
        let b = obj_is_true(ctx, &types, arg)?;
        return Ok(Some(Value::Bool(!b)));
    }
    if let Value::SmallInt(n) = arg {
        // The small-int fast path. The final `else` is the Zig's
        // `assert(op == MP_UNARY_OP_INVERT)` — a comment, not a
        // check: `LEN` / `FLOAT_MAYBE` / `COMPLEX_MAYBE` /
        // `SIZEOF` on a small int fall through to `~val` here.
        // That is unreachable through the real callers
        // (`obj_get_float_maybe` / `obj_len_maybe` short-circuit
        // small ints first), and porting the branch shape keeps
        // the quirk visible instead of inventing a new one.
        return Ok(Some(match op {
            UnaryOp::Bool => Value::Bool(n != 0),
            UnaryOp::Hash => arg,
            UnaryOp::Positive | UnaryOp::IntMaybe => arg,
            UnaryOp::Negative => Value::int(ctx.arena, Some(types.int), ctx.roots, -n),
            UnaryOp::Abs => {
                if n >= 0 {
                    arg
                } else {
                    Value::int(ctx.arena, Some(types.int), ctx.roots, -n)
                }
            }
            _ => Value::int(ctx.arena, Some(types.int), ctx.roots, !n),
        }));
    }
    if op == UnaryOp::Hash && (is_str(ctx.arena, &types, arg) || is_bytes(ctx.arena, &types, arg))
    {
        let h = str_unary_hash(ctx, arg);
        return Ok(Some(Value::int(ctx.arena, Some(types.int), ctx.roots, h)));
    }
    let t = obj_get_type(ctx.arena, &types, arg);
    let has_slot = matches!(t, Some(tr) if matches!(ctx.arena.get(tr), Obj::Type(d) if d.unary_op.is_some()));
    match call_unary(ctx, t, op, arg) {
        Ok(SlotOutcome::Value(v)) => return Ok(Some(v)),
        Ok(SlotOutcome::NotImplemented) => {}
        Err(ObjError::OutOfMemory) => return Err(RtError::OutOfMemory),
        Err(_) => return Err(RtError::PyRaise),
    }
    // The slot declined (or there is no slot). The `else if` binds
    // to the slot check: the id-hash applies ONLY when the type has
    // no unary slot at all (mirrors `hash_via_slot` in `map.rs`).
    if !has_slot && op == UnaryOp::Hash {
        return Ok(Some(obj_id(arg)));
    }
    if op == UnaryOp::Bool {
        return Ok(Some(Value::Bool(true)));
    }
    if matches!(op, UnaryOp::IntMaybe | UnaryOp::FloatMaybe | UnaryOp::ComplexMaybe) {
        return Ok(None);
    }
    Err(RtError::TypeError(
        "unsupported type for operator".to_string(),
    ))
}

/// `strUnaryHash`: the qstr hash of the string/bytes contents
/// (probed: `hash("abc") == hash(b"abc") == 12933`).
fn str_unary_hash(ctx: &CallCtx, arg: Value) -> i64 {
    match arg {
        Value::Qstr(id) => ctx.pool.qstr_hash(id as u16) as i64,
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => qstr_compute_hash(&s.bytes) as i64,
            Obj::Bytes(b) => qstr_compute_hash(&b.bytes) as i64,
            _ => 0,
        },
        _ => 0,
    }
}

// ── mp_binary_op ────────────────────────────────────────────────────

/// `binopUnsupported` (`runtime.zig:436-447`): the vendor-NORMAL
/// `TypeError` with three `%q` substitutions. A `None` op name
/// (`IN` / `IS` / `EXCEPTION_MATCH` / `NOT_IN` / `IS_NOT`) formats
/// as the empty string (probed: `1 in 2` →
/// `"unsupported types for : 'int', 'int'"`).
pub(crate) fn binop_unsupported(ctx: &CallCtx, op: BinaryOp, lhs: Value, rhs: Value) -> RtError {
    let op_name = match binary_op_method_name(op) {
        Some(id) => String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned(),
        None => String::new(),
    };
    let lhs_name = type_name_of(ctx, lhs);
    let rhs_name = type_name_of(ctx, rhs);
    RtError::TypeError(format!(
        "unsupported types for {op_name}: '{lhs_name}', '{rhs_name}'"
    ))
}

/// `realValue` (`runtime.zig:458-466`): the float value of a float
/// / int / bool operand; `None` for anything else. Exact-type
/// only for heap ints/bools (NOT `obj_get_float_maybe`, which
/// consults the `FLOAT_MAYBE` slot — the Zig checks the type).
fn real_value(ctx: &CallCtx, o: Value) -> Option<f64> {
    match o {
        Value::SmallInt(n) => Some(n as f64),
        Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Float(f) => Some(*f),
            Obj::Int(z) => Some(z.as_float()),
            _ => None,
        },
        _ => None,
    }
}

/// The operators `floatOpPrecheck` can raise for
/// (`runtime.zig:478-486`).
fn float_op_may_raise(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::TrueDivide
            | BinaryOp::InplaceTrueDivide
            | BinaryOp::FloorDivide
            | BinaryOp::InplaceFloorDivide
            | BinaryOp::Modulo
            | BinaryOp::InplaceModulo
            | BinaryOp::Divmod
            | BinaryOp::Power
            | BinaryOp::InplacePower
    )
}

/// `floatOpPrecheck` (`runtime.zig:489-500`): raise the vendor
/// float errors catchably here (the vendor raises are longjmps
/// the VM cannot catch): `ZeroDivisionError("divide by zero")`
/// for a zero divisor or zero to a negative power, `ValueError(
/// "complex values not supported")` for a negative base to a
/// fractional power.
fn float_op_precheck(ctx: &CallCtx, op: BinaryOp, lhs: Value, rhs: Value) -> Result<(), RtError> {
    let Some(r) = real_value(ctx, rhs) else {
        return Ok(());
    };
    if op == BinaryOp::Power || op == BinaryOp::InplacePower {
        let Some(l) = real_value(ctx, lhs) else {
            return Ok(());
        };
        if l == 0.0 && r < 0.0 {
            return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
        }
        if l < 0.0 && r != r.trunc() {
            return Err(RtError::ValueError(
                "complex values not supported".to_string(),
            ));
        }
    } else if r == 0.0 {
        return Err(RtError::ZeroDivisionError("divide by zero".to_string()));
    }
    Ok(())
}

/// `isTypeOrNone` (`runtime.zig:544-546`): `None` or an exact
/// `type` object.
fn is_type_or_none(ctx: &CallCtx, o: Value) -> bool {
    o == Value::None
        || matches!(o, Value::Heap(r) if ctx.arena.header(r).type_ == Some(ctx.types.type_))
}

/// `typingCall` (`runtime.zig:537-543`): `typing.<name>(a, b)` —
/// the run-time forms of `list[str]` and `int | None`.
fn typing_call(
    ctx: &mut CallCtx,
    name: u32,
    a: Value,
    b: Value,
    next_obj: Value,
    builtins: &Builtins,
    seams: &mut RuntimeSeams,
    globals: Value,
) -> Result<Value, RtError> {
    let typing_q = dunder(b"typing").expect("typing is a static qstr");
    let module = import_name(ctx, typing_q, Value::None, Value::SmallInt(0), globals, builtins, seams)?;
    let attr = load_attr(ctx, module, name, next_obj)?;
    call_function_2(ctx, attr, a, b)
}

/// `nativeBase` (`opmethods.zig:27-33`): an instance type with
/// native bases → `subobj[0]`; otherwise the value itself.
fn native_base(ctx: &CallCtx, value: Value) -> Value {
    let Some(t) = obj_get_type(ctx.arena, ctx.types, value) else {
        return value;
    };
    let Some(d) = type_data(ctx.arena, t) else {
        return value;
    };
    if d.flags & FLAG_INSTANCE_TYPE == 0 {
        return value;
    }
    let (count, _) = count_native_bases(ctx, t);
    if count == 0 {
        return value;
    }
    instance_subobj0(ctx.arena, value).unwrap_or(value)
}

/// The inplace → plain conversion (`op += OR - INPLACE_OR`).
fn inplace_to_plain(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::InplaceOr => BinaryOp::Or,
        BinaryOp::InplaceXor => BinaryOp::Xor,
        BinaryOp::InplaceAnd => BinaryOp::And,
        BinaryOp::InplaceLshift => BinaryOp::Lshift,
        BinaryOp::InplaceRshift => BinaryOp::Rshift,
        BinaryOp::InplaceAdd => BinaryOp::Add,
        BinaryOp::InplaceSubtract => BinaryOp::Subtract,
        BinaryOp::InplaceMultiply => BinaryOp::Multiply,
        BinaryOp::InplaceMatMultiply => BinaryOp::MatMultiply,
        BinaryOp::InplaceFloorDivide => BinaryOp::FloorDivide,
        BinaryOp::InplaceTrueDivide => BinaryOp::TrueDivide,
        BinaryOp::InplaceModulo => BinaryOp::Modulo,
        BinaryOp::InplacePower => BinaryOp::Power,
        _ => op,
    }
}

fn is_inplace(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::InplaceOr
            | BinaryOp::InplaceXor
            | BinaryOp::InplaceAnd
            | BinaryOp::InplaceLshift
            | BinaryOp::InplaceRshift
            | BinaryOp::InplaceAdd
            | BinaryOp::InplaceSubtract
            | BinaryOp::InplaceMultiply
            | BinaryOp::InplaceMatMultiply
            | BinaryOp::InplaceFloorDivide
            | BinaryOp::InplaceTrueDivide
            | BinaryOp::InplaceModulo
            | BinaryOp::InplacePower
    )
}

pub(crate) fn is_plain_arith(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Or
            | BinaryOp::Xor
            | BinaryOp::And
            | BinaryOp::Lshift
            | BinaryOp::Rshift
            | BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::MatMultiply
            | BinaryOp::FloorDivide
            | BinaryOp::TrueDivide
            | BinaryOp::Modulo
            | BinaryOp::Power
    )
}

/// `__op__` → `__rop__` (`op += REVERSE_OR - OR`).
pub(crate) fn to_reverse(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Or => BinaryOp::ReverseOr,
        BinaryOp::Xor => BinaryOp::ReverseXor,
        BinaryOp::And => BinaryOp::ReverseAnd,
        BinaryOp::Lshift => BinaryOp::ReverseLshift,
        BinaryOp::Rshift => BinaryOp::ReverseRshift,
        BinaryOp::Add => BinaryOp::ReverseAdd,
        BinaryOp::Subtract => BinaryOp::ReverseSubtract,
        BinaryOp::Multiply => BinaryOp::ReverseMultiply,
        BinaryOp::MatMultiply => BinaryOp::ReverseMatMultiply,
        BinaryOp::FloorDivide => BinaryOp::ReverseFloorDivide,
        BinaryOp::TrueDivide => BinaryOp::ReverseTrueDivide,
        BinaryOp::Modulo => BinaryOp::ReverseModulo,
        BinaryOp::Power => BinaryOp::ReversePower,
        _ => op,
    }
}

fn is_reverse(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::ReverseOr
            | BinaryOp::ReverseXor
            | BinaryOp::ReverseAnd
            | BinaryOp::ReverseLshift
            | BinaryOp::ReverseRshift
            | BinaryOp::ReverseAdd
            | BinaryOp::ReverseSubtract
            | BinaryOp::ReverseMultiply
            | BinaryOp::ReverseMatMultiply
            | BinaryOp::ReverseFloorDivide
            | BinaryOp::ReverseTrueDivide
            | BinaryOp::ReverseModulo
            | BinaryOp::ReversePower
    )
}

/// `__rop__` → `__op__` (`op -= REVERSE_OR - OR`).
fn from_reverse(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::ReverseOr => BinaryOp::Or,
        BinaryOp::ReverseXor => BinaryOp::Xor,
        BinaryOp::ReverseAnd => BinaryOp::And,
        BinaryOp::ReverseLshift => BinaryOp::Lshift,
        BinaryOp::ReverseRshift => BinaryOp::Rshift,
        BinaryOp::ReverseAdd => BinaryOp::Add,
        BinaryOp::ReverseSubtract => BinaryOp::Subtract,
        BinaryOp::ReverseMultiply => BinaryOp::Multiply,
        BinaryOp::ReverseMatMultiply => BinaryOp::MatMultiply,
        BinaryOp::ReverseFloorDivide => BinaryOp::FloorDivide,
        BinaryOp::ReverseTrueDivide => BinaryOp::TrueDivide,
        BinaryOp::ReverseModulo => BinaryOp::Modulo,
        BinaryOp::ReversePower => BinaryOp::Power,
        _ => op,
    }
}

/// `nativeSubclassOp` (`runtime.zig:510-535`): `1 + R(2)` for a
/// Python subclass `R` of int/float/str with a native value on the
/// left — the subclass's reflected method goes first when it
/// defines one, else the operation uses its native value
/// (a full `mpBinaryOpImpl` re-entry with the native rhs).
fn native_subclass_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
    exc: &ExcRefs,
    next_obj: Value,
    builtins: &Builtins,
    seams: &mut RuntimeSeams,
    globals: Value,
) -> Result<Option<Value>, RtError> {
    let types = ctx.types;
    if !is_native_scalar(ctx.arena, &types, lhs) {
        return Ok(None);
    }
    let native = native_base(ctx, rhs);
    if native == rhs || !is_native_scalar(ctx.arena, &types, native) {
        return Ok(None);
    }
    let mut fwd = op;
    if is_inplace(fwd) {
        fwd = inplace_to_plain(fwd);
    }
    let reflected = match fwd {
        BinaryOp::Less => Some(BinaryOp::More),
        BinaryOp::More => Some(BinaryOp::Less),
        BinaryOp::LessEqual => Some(BinaryOp::MoreEqual),
        BinaryOp::MoreEqual => Some(BinaryOp::LessEqual),
        _ if is_plain_arith(fwd) => Some(to_reverse(fwd)),
        _ => None,
    };
    if let Some(rop) = reflected {
        let method_name = binary_op_method_name(rop).expect("reflected op has a method name");
        if let Some(dest) = load_method_maybe(ctx, rhs, method_name, next_obj)? {
            let res = call_method_n_kw(ctx, 1, 0, &dest, &[lhs])?;
            return Ok(Some(res));
        }
    }
    Ok(Some(binary_op(
        ctx, op, lhs, native, exc, next_obj, builtins, seams, globals,
    )?))
}

/// Whether `lhs` is int-like for the `extra_cases` delegation:
/// a small int, a heap int, or a bool (the bool slot converts to
/// a small int and delegates to the int slot — `objbool.zig:45`).
fn is_int_like(ctx: &CallCtx, v: Value) -> bool {
    matches!(v, Value::SmallInt(_) | Value::Bool(_)) || is_exact_int(ctx.arena, &ctx.types, v)
}

/// Whether `rhs` is an exact sequence for the `extra_cases`
/// delegation (`objIsStrOrBytes` or exact tuple/list).
fn is_exact_seq(ctx: &CallCtx, v: Value) -> bool {
    is_str(ctx.arena, ctx.types, v)
        || is_bytes(ctx.arena, ctx.types, v)
        || matches!(v, Value::Heap(r) if ctx.arena.header(r).type_ == Some(ctx.types.tuple))
        || matches!(v, Value::Heap(r) if ctx.arena.header(r).type_ == Some(ctx.types.list))
}


/// The `EXCEPTION_MATCH` arm (`runtime.zig:559-573`): a single
/// exception type, or a tuple of exception types (any non-type
/// element → `binopUnsupported`).
fn exception_match_op(
    ctx: &mut CallCtx,
    lhs: Value,
    rhs: Value,
    exc: &ExcRefs,
) -> Result<Value, RtError> {
    use crate::objects::objtype::objexcept::is_exception_type;
    if is_exception_type(ctx.arena, rhs, exc.base_exception) {
        return Ok(Value::Bool(exception_match(ctx.arena, lhs, rhs)));
    }
    if let Value::Heap(r) = rhs {
        if matches!(ctx.arena.get(r), Obj::Tuple(_))
            && ctx.arena.header(r).type_ == Some(ctx.types.tuple)
        {
            let items = match ctx.arena.get(r) {
                Obj::Tuple(items) => items.clone(),
                _ => unreachable!(),
            };
            for item in &items {
                if !is_exception_type(ctx.arena, *item, exc.base_exception) {
                    return Err(binop_unsupported(ctx, BinaryOp::ExceptionMatch, lhs, *item));
                }
                if exception_match(ctx.arena, lhs, *item) {
                    return Ok(Value::Bool(true));
                }
            }
            return Ok(Value::Bool(false));
        }
    }
    Err(binop_unsupported(ctx, BinaryOp::ExceptionMatch, lhs, rhs))
}

/// The `CONTAINS` arm (`runtime.zig:768-783`): iterate `lhs`,
/// `mp_obj_equal` each item against `rhs`.
fn contains_op(
    ctx: &mut CallCtx,
    lhs: Value,
    rhs: Value,
    next_obj: Value,
    seams: &mut RuntimeSeams,
) -> Result<Value, RtError> {
    let types = ctx.types;
    let iter = getiter(ctx, lhs, next_obj, seams)?;
    loop {
        match iternext(ctx, iter, next_obj, seams)? {
            IterNext::Value(item) => {
                if obj_equal(ctx, &types, item, rhs)? {
                    return Ok(Value::Bool(true));
                }
            }
            IterNext::Stop(_) => return Ok(Value::Bool(false)),
        }
    }
}

/// The generic slot loop (`runtime.zig:725-788`,
/// `generic_binary_op`).
fn generic_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
    exc: &ExcRefs,
    next_obj: Value,
    builtins: &Builtins,
    seams: &mut RuntimeSeams,
    globals: Value,
) -> Result<Value, RtError> {
    let mut op = op;
    let mut lhs = lhs;
    let mut rhs = rhs;
    // A float on the left reaches the vendor float op through the
    // slot — precheck first (`runtime.zig:724`).
    if float_op_may_raise(op) && is_float(ctx.arena, &ctx.types, lhs) {
        float_op_precheck(ctx, op, lhs, rhs)?;
    }
    loop {
        let t = obj_get_type(ctx.arena, ctx.types, lhs);
        match call_binary(ctx, t, op, lhs, rhs) {
            Ok(SlotOutcome::Value(v)) => return Ok(v),
            Ok(SlotOutcome::NotImplemented) => {}
            Err(ObjError::OutOfMemory) => return Err(RtError::OutOfMemory),
            Err(_) => return Err(RtError::PyRaise),
        }
        // The int slot's `extra_cases` delegation (see the module
        // docs): plain `MULTIPLY` only — `INPLACE_MULTIPLY`
        // reaches here as `MULTIPLY` after the conversion below.
        if op == BinaryOp::Multiply && is_int_like(ctx, lhs) && is_exact_seq(ctx, rhs) {
            return binary_op(ctx, BinaryOp::Multiply, rhs, lhs, exc, next_obj, builtins, seams, globals);
        }
        if is_inplace(op) {
            op = inplace_to_plain(op);
            continue;
        }
        // `MICROPY_PY_REVERSE_SPECIAL_METHODS` — default-true on
        // both arms per ADR 012 D4.
        if is_plain_arith(op) {
            std::mem::swap(&mut lhs, &mut rhs);
            op = to_reverse(op);
            continue;
        } else if is_reverse(op) {
            std::mem::swap(&mut lhs, &mut rhs);
            op = from_reverse(op);
        }
        if op == BinaryOp::Contains {
            return contains_op(ctx, lhs, rhs, next_obj, seams);
        }
        break;
    }
    Err(binop_unsupported(ctx, op, lhs, rhs))
}

/// `mp_binary_op` (`runtime.zig:550-810`).
pub fn binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
    exc: &ExcRefs,
    next_obj: Value,
    builtins: &Builtins,
    seams: &mut RuntimeSeams,
    globals: Value,
) -> Result<Value, RtError> {
    let types = ctx.types;
    if op == BinaryOp::Is {
        return Ok(Value::Bool(lhs == rhs));
    }
    if op == BinaryOp::Equal || op == BinaryOp::NotEqual {
        return crate::objects::obj_ops::obj_equal_not_equal(ctx, &types, op, lhs, rhs, None)
            .map_err(RtError::from);
    }
    if op == BinaryOp::ExceptionMatch {
        return exception_match_op(ctx, lhs, rhs, exc);
    }
    // Full config: `int | None` / `list[str]` via `typing`
    // (`runtime.zig:574-577`).
    if op == BinaryOp::Or
        && is_type_or_none(ctx, lhs)
        && is_type_or_none(ctx, rhs)
        && (lhs != Value::None || rhs != Value::None)
    {
        let union_q = dunder(b"_union").expect("_union is a static qstr");
        return typing_call(ctx, union_q, lhs, rhs, next_obj, builtins, seams, globals);
    }
    if let Some(v) = native_subclass_op(ctx, op, lhs, rhs, exc, next_obj, builtins, seams, globals)? {
        return Ok(v);
    }
    let mut op = op;
    let mut lhs = lhs;
    let mut rhs = rhs;
    if let Value::SmallInt(lhs_val) = lhs {
        if let Value::SmallInt(rhs_val) = rhs {
            match small_int_binary_op(ctx, op, lhs_val, rhs_val, lhs)? {
                SmallIntOutcome::Done(v) => return Ok(v),
                SmallIntOutcome::GoGeneric(heap_lhs) => {
                    return generic_binary_op(
                        ctx, op, heap_lhs, rhs, exc, next_obj, builtins, seams, globals,
                    );
                }
            }
        }
        // A float on the right promotes to float
        // (`runtime.zig:703-711`): precheck, then the exact float
        // type's binary-op slot with the plain op (see the module
        // docs — the reverse dispatch cannot reach the float slot).
        if is_float(ctx.arena, &types, rhs) {
            if float_op_may_raise(op) {
                float_op_precheck(ctx, op, lhs, rhs)?;
            }
            let t = Some(types.float);
            match call_binary(ctx, t, op, lhs, rhs) {
                Ok(SlotOutcome::Value(v)) => return Ok(v),
                Ok(SlotOutcome::NotImplemented) => {
                    return Err(binop_unsupported(ctx, op, lhs, rhs));
                }
                Err(ObjError::OutOfMemory) => return Err(RtError::OutOfMemory),
                Err(_) => return Err(RtError::PyRaise),
            }
        }
        // COMPLEX (full only): int op complex promotes to complex
        // (`runtime.zig:713-721`) — same routing as the float arm.
        if matches!(rhs, Value::Heap(r) if ctx.arena.header(r).type_ == Some(types.complex)) {
            let t = Some(types.complex);
            match call_binary(ctx, t, op, lhs, rhs) {
                Ok(SlotOutcome::Value(v)) => return Ok(v),
                Ok(SlotOutcome::NotImplemented) => {
                    return Err(binop_unsupported(ctx, op, lhs, rhs));
                }
                Err(ObjError::OutOfMemory) => return Err(RtError::OutOfMemory),
                Err(_) => return Err(RtError::PyRaise),
            }
        }
    }
    if op == BinaryOp::In {
        op = BinaryOp::Contains;
        std::mem::swap(&mut lhs, &mut rhs);
    }
    generic_binary_op(ctx, op, lhs, rhs, exc, next_obj, builtins, seams, globals)
}
