//! The VM's runtime-operation helpers — the `runtime.zig` ports
//! the dispatch calls: `mp_unary_op` / `mp_binary_op`
//! (`runtime.zig:352-408`, `:550-793`).
//!
//! Split (file-size rule): the name/global/attr helpers live in
//! `vm_rt_name.rs` (`runtime.zig:288-346`, `:1126-1275`); the
//! getiter/iternext/resume/unpack/import helpers and the `&mut
//! Vm` wrappers live in `vm_rt_iter.rs` (`runtime.zig:1055-1123`,
//! `:1277-1456`).
//!
//! Float/complex (C18a, LANDED): `int op float` / `float op …` /
//! `int op complex` go through the generic loop now — the int
//! slot delegates float/complex rhs to `mp_obj_float_binary_op` /
//! `mp_obj_complex_binary_op` (`objfloat.rs` / `objcomplex.rs`,
//! vendor `objint_mpz.c:175-183`). The two pure-int float results
//! — `int / int` and `int ** -n` — construct a `Float` value
//! directly (f64 arithmetic, no float op needed) with the Zig's
//! precheck.

use crate::memory::gc::CallCtx;
use crate::objects::containers::objlist::{is_exact_list, list_items_vec};
use crate::objects::containers::objtuple::{is_exact_tuple, tuple_items};
use crate::objects::obj::{
    call_binary, call_unary, is_exact_int, is_float, obj_get_type, obj_is_true, SlotOutcome,
};
use crate::objects::obj_ops::{obj_equal_not_equal, obj_type_name};
use crate::objects::objtype::lookup::binary_op_method_name;
use crate::objects::objtype::objexcept::{exception_match, is_exception_type};
use crate::objects::objtype::seams::{real_load_maybe, with_call_hook_seams};
use crate::objects::objtype::subclass::instance_subobj0;
use crate::objects::primitives::ops::new_int_from_bigint;
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::runtime::vm_seams::make_hook;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, exc_from_typed, type_error};
use crate::runtime::vm_rt_iter::contains_op_ctx;
use crate::support::smallint::{
    small_int_floor_divide as floored_div, small_int_modulo as floored_mod,
};
use crate::vendor::mpz::BigInt;

/// `mp_obj_is_true` (`obj.zig:470` + the VM's polled guard):
/// truthiness, propagating a recorded raise (the Zig returns
/// false whenever pending is set — the VM raises instead, since
/// a false-with-pending would silently swallow the raise).
pub fn is_true_ctx(ctx: &mut CallCtx, state: &mut VmState, v: Value) -> Result<bool, Value> {
    // An instance tries its `__bool__` / `__len__` dunder
    // first, with the VM's real call hook (E2); anything else
    // goes through `obj_is_true` as before.
    match crate::runtime::vm_seams::try_instance_bool(ctx, state, v) {
        Ok(Some(b)) => return Ok(b),
        Ok(None) => {}
        Err(exc) => return Err(exc),
    }
    let types = ctx.types;
    match obj_is_true(ctx, types, v) {
        Ok(b) => {
            if ctx.has_pending_raise() {
                Err(pending_exc_or_type_error(ctx, state))
            } else {
                Ok(b)
            }
        }
        Err(e) => Err(exc_from_pyerror(ctx, state, e)),
    }
}

/// Drain the recorded raise into its exception object, or a
/// bare `TypeError` when nothing was recorded (the "slot
/// raised, data dropped" case). `pub(crate)` — `vm_rt_name` /
/// `vm_rt_iter` use it too.
pub(crate) fn pending_exc_or_type_error(ctx: &mut CallCtx, state: &VmState) -> Value {
    crate::runtime::vm_raise::exception_from_pending(ctx, state).unwrap_or_else(|| {
        crate::objects::objtype::objexcept::new_exception(ctx, state.exc.type_error)
    })
}

/// `mp_unary_op` (`runtime.zig:352-408`): NOT is `!is_true`;
/// small ints take the fast path (HASH/POSITIVE/INT_MAYBE are
/// the identity; NEGATIVE/ABS on `SMALL_INT_MIN` make a heap
/// int); HASH on str/bytes is the string hash; otherwise the
/// type's unary slot, with the Zig's fallbacks (HASH → the
/// object's id, BOOL → True) when the slot is not implemented.
/// A recorded raise propagates (the C13 convention — the Zig
/// overwrites it with the later `raiseMsg`, which no reachable
/// unary slot raise distinguishes).
pub fn unary_op_ctx(ctx: &mut CallCtx, state: &mut VmState, op: UnaryOp, arg: Value) -> Result<Value, Value> {
    use crate::objects::value::SMALL_INT_MIN;
    if op == UnaryOp::Not {
        // `not obj`: an instance's `__bool__` / `__len__` runs
        // first (E2) — `is_true_ctx` already does this.
        let b = match is_true_ctx(ctx, state, arg) {
            Ok(b) => b,
            Err(exc) => return Err(exc),
        };
        return Ok(Value::Bool(!b));
    }
    if let Value::SmallInt(val) = arg {
        let v = match op {
            UnaryOp::Bool => return Ok(Value::Bool(val != 0)),
            UnaryOp::Hash | UnaryOp::Positive | UnaryOp::IntMaybe => return Ok(arg),
            UnaryOp::Negative => {
                if val == SMALL_INT_MIN {
                    Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, -val)
                } else {
                    Value::SmallInt(-val)
                }
            }
            UnaryOp::Abs => {
                if val >= 0 {
                    return Ok(arg);
                } else if val == SMALL_INT_MIN {
                    Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, -val)
                } else {
                    Value::SmallInt(-val)
                }
            }
            UnaryOp::Invert => Value::SmallInt(!val),
            _ => {
                return Err(type_error(ctx, state, "unsupported type for operator"));
            }
        };
        return Ok(v);
    }
    if op == UnaryOp::Hash {
        // Every value hashes through `hash_of_hook` (small ints /
        // qstrs / None / bools are handled inside it; heap
        // values go through their hash slot, and a type with no
        // slot falls back to the identity hash — `map.rs`). The
        // hook lets an instance key run its `__hash__` through
        // the VM's real call hook (E2h) — the ctx-only `hash_of`
        // slot path cannot run a `FunBc` `__hash__`.
        use crate::objects::containers::map::{hash_of_hook, MapError};
        let types = ctx.types;
        let mut hook = make_hook(state);
        return match hash_of_hook(ctx, types, arg, Some(&mut hook)) {
            Ok(h) => Ok(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, h as i64)),
            Err(MapError::TypeError(msg)) => Err(type_error(ctx, state, msg)),
            Err(_) => Err(pending_exc_or_type_error(ctx, state)),
        };
    }
    // An instance tries its unary dunder first, with the VM's
    // real call hook (E2); no such dunder falls through to the
    // slot path below.
    match crate::runtime::vm_seams::try_instance_unary_op(ctx, state, op, arg) {
        Ok(Some(v)) => return Ok(v),
        Ok(None) => {}
        Err(exc) => return Err(exc),
    }
    let type_ = obj_get_type(ctx.arena, ctx.types, arg);
    match call_unary(ctx, type_, op, arg) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => {
            if ctx.has_pending_raise() {
                return Err(pending_exc_or_type_error(ctx, state));
            }
            match op {
                UnaryOp::Hash => {
                    // Hash of an instance id (the Zig:
                    // `newSmallInt(@bitCast(arg))` — the object's
                    // word; in Rust the arena index is the
                    // identity, per C0 D3).
                    let id = match arg {
                        Value::Heap(r) => r.0 as i64,
                        _ => 0,
                    };
                    Ok(Value::SmallInt(id))
                }
                UnaryOp::Bool => Ok(Value::Bool(true)),
                _ => Err(type_error(ctx, state, "unsupported type for operator")),
            }
        }
        Err(e) => Err(exc_from_typed(ctx, state, e)),
    }
}

/// The op dunder name for `binop_unsupported`
/// (`objtype.zig:772-828`, `mp_binary_op_method_name`).
/// `None` for ops with no dunder (IS / IN / EXCEPTION_MATCH /
/// NOT_IN / IS_NOT — those never reach the error).
pub fn binary_op_dunder(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Less => "__lt__",
        BinaryOp::More => "__gt__",
        BinaryOp::Equal => "__eq__",
        BinaryOp::LessEqual => "__le__",
        BinaryOp::MoreEqual => "__ge__",
        BinaryOp::NotEqual => "__ne__",
        BinaryOp::Contains => "__contains__",
        BinaryOp::InplaceAdd => "__iadd__",
        BinaryOp::InplaceSubtract => "__isub__",
        BinaryOp::InplaceMultiply => "__imul__",
        BinaryOp::InplaceMatMultiply => "__imatmul__",
        BinaryOp::InplaceFloorDivide => "__ifloordiv__",
        BinaryOp::InplaceTrueDivide => "__itruediv__",
        BinaryOp::InplaceModulo => "__imod__",
        BinaryOp::InplacePower => "__ipow__",
        BinaryOp::InplaceOr => "__ior__",
        BinaryOp::InplaceXor => "__ixor__",
        BinaryOp::InplaceAnd => "__iand__",
        BinaryOp::InplaceLshift => "__ilshift__",
        BinaryOp::InplaceRshift => "__irshift__",
        BinaryOp::Add => "__add__",
        BinaryOp::Subtract => "__sub__",
        BinaryOp::Multiply => "__mul__",
        BinaryOp::MatMultiply => "__matmul__",
        BinaryOp::FloorDivide => "__floordiv__",
        BinaryOp::TrueDivide => "__truediv__",
        BinaryOp::Modulo => "__mod__",
        BinaryOp::Divmod => "__divmod__",
        BinaryOp::Power => "__pow__",
        BinaryOp::Or => "__or__",
        BinaryOp::Xor => "__xor__",
        BinaryOp::And => "__and__",
        BinaryOp::Lshift => "__lshift__",
        BinaryOp::Rshift => "__rshift__",
        BinaryOp::ReverseAdd => "__radd__",
        BinaryOp::ReverseSubtract => "__rsub__",
        BinaryOp::ReverseMultiply => "__rmul__",
        BinaryOp::ReverseMatMultiply => "__rmatmul__",
        BinaryOp::ReverseFloorDivide => "__rfloordiv__",
        BinaryOp::ReverseTrueDivide => "__rtruediv__",
        BinaryOp::ReverseModulo => "__rmod__",
        BinaryOp::ReversePower => "__rpow__",
        BinaryOp::ReverseOr => "__ror__",
        BinaryOp::ReverseXor => "__rxor__",
        BinaryOp::ReverseAnd => "__rand__",
        BinaryOp::ReverseLshift => "__rlshift__",
        BinaryOp::ReverseRshift => "__rrshift__",
        _ => return None,
    })
}

/// `binopUnsupported` (`runtime.zig`): `TypeError("unsupported
/// types for %q: '%q', '%q'")` (op dunder, lhs type name, rhs
/// type name). The op is converted back from a reverse op for
/// the message (the Zig swaps back before raising).
fn binop_unsupported(
    ctx: &mut CallCtx,
    state: &VmState,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> Value {
    let op = unreverse_op(op);
    let dunder = binary_op_dunder(op).unwrap_or("");
    let lname = obj_type_name(ctx, lhs);
    let rname = obj_type_name(ctx, rhs);
    let msg = format!("unsupported types for {dunder}: '{lname}', '{rname}'");
    crate::objects::objtype::objexcept::new_exception_msg(ctx, state.exc.type_error, &msg)
}

/// Undo a reverse-op conversion (for the error message).
fn unreverse_op(op: BinaryOp) -> BinaryOp {
    let v = op.as_u8();
    if (37..=49).contains(&v) {
        BinaryOp::from_u8(v - 15).unwrap_or(op)
    } else {
        op
    }
}

/// `binopZeroDiv`: `ZeroDivisionError("divide by zero")`.
fn binop_zero_div(ctx: &mut CallCtx, state: &VmState) -> Value {
    crate::objects::objtype::objexcept::new_exception_msg(
        ctx,
        state.exc.zero_division_error,
        "divide by zero",
    )
}

/// `floatOpPrecheck` for the int paths (`runtime.zig`): rhs
/// real value 0 → ZeroDivisionError; `0 ** negative` →
/// ZeroDivisionError; negative base + non-integer exp →
/// ValueError "complex values not supported".
fn float_precheck_int(
    ctx: &mut CallCtx,
    state: &VmState,
    op: BinaryOp,
    lhs_val: i64,
    rhs_f: f64,
) -> Option<Value> {
    use BinaryOp::*;
    match op {
        TrueDivide | InplaceTrueDivide | FloorDivide | InplaceFloorDivide | Modulo
        | InplaceModulo | Divmod => {
            if rhs_f == 0.0 {
                return Some(binop_zero_div(ctx, state));
            }
            None
        }
        Power | InplacePower => {
            if lhs_val == 0 && rhs_f < 0.0 {
                return Some(binop_zero_div(ctx, state));
            }
            // The Zig's test is `r != trunc(r)` (`runtime.zig:493`);
            // `fract() != 0.0` is wrong for `inf` (`fract(inf)` is
            // NaN, so `(-8) ** inf` would raise — the reference
            // returns `inf`, probed 2026-11-18). `trunc` agrees
            // with `fract` on every finite value and is correct
            // for `inf` (no raise) and `nan` (raises, since
            // `nan != nan`).
            if lhs_val < 0 && rhs_f != rhs_f.trunc() {
                return Some(crate::objects::objtype::objexcept::new_exception_msg(
                    ctx,
                    state.exc.value_error,
                    "complex values not supported",
                ));
            }
            None
        }
        _ => None,
    }
}

// ── `binary_op` ──────────────────────────────────────────────────────

/// `mp_binary_op` (`runtime.zig:550-793`).
///
/// Order: IS → EQUAL/NOT_EQUAL → EXCEPTION_MATCH → the
/// full-only `|` typing-union stub → `nativeSubclassOp` stub →
/// the small-int/small-int fast path (wrapping arithmetic,
/// overflow → heap int + generic path) → IN→CONTAINS swap → the
/// generic loop (lhs slot → inplace→plain → reverse → CONTAINS
/// iteration → `binopUnsupported`). `int op float` /
/// `int op complex` fall through to the generic loop (C18a) —
/// the int slot delegates them to the float/complex ops.
/// `mp_seq_cmp_objs` (`sequence.zig:104-137`) over the VM's
/// dispatch loop (E2d): the ordering half of the container
/// comparison, for same-kind exact lists / exact tuples.
/// Element equality runs the EQUAL dispatch (so a nested
/// container or a user `__eq__` goes through the hook);
/// ordering runs `binary_op_ctx` on the elements and counts
/// only an exact `Bool(true)` result, exactly like
/// `seq_cmp_objs`. Only Less/More/LessEqual/MoreEqual reach
/// here (Equal/NotEqual are handled by
/// `obj_equal_not_equal`'s container branch before the
/// generic loop).
fn container_compare(
    ctx: &mut CallCtx,
    state: &mut VmState,
    op: BinaryOp,
    lhs: &[Value],
    rhs: &[Value],
) -> Result<Value, Value> {
    debug_assert!(matches!(
        op,
        BinaryOp::Less | BinaryOp::More | BinaryOp::LessEqual | BinaryOp::MoreEqual
    ));
    let types = ctx.types;
    let (mut lhs, mut rhs, mut op) = (lhs, rhs, op);
    // Less/LessEqual swap the operands and become
    // More/MoreEqual — the element loop then asks
    // `lhs[i] op rhs[i]` on the swapped sides.
    if op == BinaryOp::Less || op == BinaryOp::LessEqual {
        std::mem::swap(&mut lhs, &mut rhs);
        op = if op == BinaryOp::Less {
            BinaryOp::More
        } else {
            BinaryOp::MoreEqual
        };
    }
    let len = lhs.len().min(rhs.len());
    for i in 0..len {
        let eq = binary_op_ctx(ctx, state, BinaryOp::Equal, lhs[i], rhs[i])?;
        if obj_is_true(ctx, types, eq).map_err(|e| exc_from_pyerror(ctx, state, e))? {
            continue;
        }
        let ord = binary_op_ctx(ctx, state, op, lhs[i], rhs[i])?;
        return Ok(Value::Bool(ord == Value::Bool(true)));
    }
    if lhs.len() != rhs.len() {
        return Ok(Value::Bool(lhs.len() > rhs.len()));
    }
    if op == BinaryOp::More {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(true))
}

/// `isNativeScalar` (`runtime.zig:506`): a small int, a float,
/// an exact int, a qstr, or an exact str — the values
/// `nativeSubclassOp` accepts on the left (and as the unwrapped
/// native base on the right). Lists/bytes/dicts/tuples are NOT
/// native scalars.
fn is_native_scalar(ctx: &CallCtx, v: Value) -> bool {
    match v {
        Value::SmallInt(_) | Value::Qstr(_) => true,
        Value::Heap(_) => {
            is_float(ctx.arena, ctx.types, v)
                || is_exact_int(ctx.arena, ctx.types, v)
                || obj_get_type(ctx.arena, ctx.types, v) == Some(ctx.types.str_)
        }
        _ => false,
    }
}

/// The reflected op `nativeSubclassOp` tries on the rhs
/// (`runtime.zig:510-548`): LESS↔MORE, LESS_EQUAL↔MORE_EQUAL,
/// else `fwd + (REVERSE_OR - OR)` for fwd in OR..POWER — where
/// `fwd` is the op with an inplace op first converted to its
/// plain form (INPLACE_ADD → ADD). Anything else (DIVMOD,
/// CONTAINS, …) has no reflected form.
fn reflected_op(op: BinaryOp) -> Option<BinaryOp> {
    let mut fwd = op;
    let v = fwd.as_u8();
    if (9..=21).contains(&v) {
        fwd = BinaryOp::from_u8(v + 13).unwrap_or(op);
    }
    match fwd {
        BinaryOp::Less => Some(BinaryOp::More),
        BinaryOp::More => Some(BinaryOp::Less),
        BinaryOp::LessEqual => Some(BinaryOp::MoreEqual),
        BinaryOp::MoreEqual => Some(BinaryOp::LessEqual),
        _ => {
            let v = fwd.as_u8();
            if (22..=34).contains(&v) {
                BinaryOp::from_u8(v + 15)
            } else {
                None
            }
        }
    }
}

/// `nativeSubclassOp` (`runtime.zig:510-548`, full builds only):
/// an int/float/str subclass on the RIGHT of a native scalar
/// works as its value. If the rhs defines the reflected method
/// (`__radd__`, `__gt__` for `<`, …) it runs through the VM's
/// real call hook and its result is returned AS-IS — including
/// `NotImplemented` (probed: `1 + W(2)` prints `NotImplemented`
/// where `W.__radd__` returns it). If it defines no reflected
/// method — or the op has no reflected form — the op recurses
/// on `(lhs, nativeBase(rhs))` with the ORIGINAL op. `None` =
/// not applicable (lhs not a native scalar, rhs not a subclass
/// instance, or the native base not a native scalar) — the
/// caller falls through to the small-int fast path / generic
/// loop exactly as before.
fn native_subclass_op(
    ctx: &mut CallCtx,
    state: &mut VmState,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> Result<Option<Value>, Value> {
    if !is_native_scalar(ctx, lhs) {
        return Ok(None);
    }
    let Some(native) = instance_subobj0(ctx.arena, rhs) else {
        return Ok(None);
    };
    if native == rhs || !is_native_scalar(ctx, native) {
        return Ok(None);
    }
    if let Some(reflected) = reflected_op(op) {
        if let Some(name_q) = binary_op_method_name(reflected) {
            if let Some((fun, bound)) = real_load_maybe(ctx, rhs, name_q) {
                let mut hook = make_hook(state);
                let res = with_call_hook_seams(&mut hook, |seams| {
                    seams.call_pair(ctx, fun, bound, 1, 0, &[lhs])
                });
                return match res {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(crate::runtime::vm_seams::drain_hook_raise(ctx, state, e)),
                };
            }
        }
    }
    binary_op_ctx(ctx, state, op, lhs, native).map(Some)
}

pub fn binary_op_ctx(ctx: &mut CallCtx, state: &mut VmState, op_in: BinaryOp, lhs_in: Value, rhs_in: Value) -> Result<Value, Value> {
    use crate::objects::value::{SMALL_INT_MAX, SMALL_INT_MIN};
    let mut op = op_in;
    let mut lhs = lhs_in;
    let mut rhs = rhs_in;
    // IS → identity.
    if op == BinaryOp::Is {
        return Ok(Value::Bool(lhs == rhs));
    }
    // EQUAL / NOT_EQUAL → the equality protocol. The VM passes
    // its real call hook (E2), so an instance's `__eq__` /
    // `__ne__` actually runs inside `obj_equal_not_equal`.
    if op == BinaryOp::Equal || op == BinaryOp::NotEqual {
        let types = ctx.types;
        // The hook is scoped (and dropped) inside this block:
        // it borrows `state`, and the error drain below needs
        // `state` too.
        let result = {
            let mut hook = crate::runtime::vm_seams::make_hook(state);
            obj_equal_not_equal(ctx, types, op, lhs, rhs, Some(&mut hook))
        };
        // E2i: a slot may answer `Ok(False)` while recording a
        // raise (the vendor's `raiseNotImplemented()` sets
        // `pending_exc` AND returns a value — the array slot's
        // comparison else-branch does exactly this for
        // mismatched layouts, e.g. `array('b') == array('i')`).
        // The generic loop's `NotImplemented` arm already
        // propagates a recorded raise; the equality protocol
        // must do the same, or `==` silently swallows it to
        // `False` (`is_true_ctx`'s precedent: false whenever
        // pending is set — the VM raises instead).
        if result.is_ok() && ctx.has_pending_raise() {
            return Err(pending_exc_or_type_error(ctx, state));
        }
        return result.map_err(|e| crate::runtime::vm_seams::drain_hook_raise(ctx, state, e));
    }
    // MODULO on an exact str/bytes lhs (E2e): the `%`-format
    // path needs the VM's hook for `__int__` dispatch in the
    // `%d`/`%o`/`%x` arms (the slot's stub seams cannot run a
    // FunBc `__int__` — `"%d" % A()` raised "can't convert to
    // int"). The generic loop below would reach the same slot
    // without a hook; intercept here so the hook is available.
    // Only exact str/bytes: a subclass instance or an instance
    // with `__mod__` keeps the generic loop (its
    // `try_instance_binary_op` runs first there), and a
    // reflected `__rmod__` never runs for a str lhs either way
    // (the str slot answers first, in both paths).
    if op == BinaryOp::Modulo || op == BinaryOp::InplaceModulo {
        let lt = obj_get_type(ctx.arena, ctx.types, lhs);
        if lt == Some(ctx.types.str_) || lt == Some(ctx.types.bytes) {
            let types = ctx.types;
            let result = {
                let mut hook = crate::runtime::vm_seams::make_hook(state);
                crate::objects::strings::objstr_format::str_modulo_format_hook(
                    ctx,
                    types,
                    lhs,
                    rhs,
                    Some(&mut hook),
                )
            };
            return result.map_err(|e| {
                let pe = crate::objects::strings::objstr_a::slot_raise(ctx, e);
                crate::runtime::vm_seams::drain_hook_raise(ctx, state, pe)
            });
        }
    }
    // EXCEPTION_MATCH → `exception_match` (tuple form included).
    if op == BinaryOp::ExceptionMatch {
        if is_exception_type(ctx.arena, rhs, state.exc.base_exception) {
            return Ok(Value::Bool(exception_match(ctx.arena, lhs, rhs)));
        }
        if let Value::Heap(r) = rhs {
            if let Obj::Tuple(items) = ctx.arena.get(r) {
                let items = items.clone();
                for item in items {
                    if !is_exception_type(ctx.arena, item, state.exc.base_exception) {
                        return Err(binop_unsupported(ctx, state, op, lhs, item));
                    }
                    if exception_match(ctx.arena, lhs, item) {
                        return Ok(Value::Bool(true));
                    }
                }
                return Ok(Value::Bool(false));
            }
        }
        return Err(binop_unsupported(ctx, state, op, lhs, rhs));
    }
    // Full-only `int | None` typing union — needs the import
    // system (C15's `typingCall`); fall through to
    // `binopUnsupported` until then (documented stub).
    // `nativeSubclassOp` (F3 — see `native_subclass_op` below):
    // an int/float/str subclass on the right of a native scalar
    // works as its value (reflected method first, else recurse
    // on the native base). Placed here — after EQUAL/NOT_EQUAL,
    // before the MODULO intercept — as in the vendor
    // (`runtime.zig:580`, after IS/EQUAL/EXCEPTION_MATCH and the
    // typing union, before the small-int fast path). The MODULO
    // intercept below is Rust's `%`-format hook, which the
    // vendor reaches later through the str slot — a `'%d' % R(2)`
    // takes the same result either way (the `%` engine unwraps
    // the rhs via `nativeBase`).
    if let Some(v) = native_subclass_op(ctx, state, op, lhs, rhs)? {
        return Ok(v);
    }
    // Small-int / small-int fast path.
    if let (Value::SmallInt(lhs_val), Value::SmallInt(rhs_val)) = (lhs, rhs) {
        let mut go_big = false;
        let mut result: Option<Value> = None;
        match op {
            BinaryOp::Or | BinaryOp::InplaceOr => {
                let v = lhs_val | rhs_val;
                result = Some(Value::SmallInt(v));
            }
            BinaryOp::Xor | BinaryOp::InplaceXor => {
                result = Some(Value::SmallInt(lhs_val ^ rhs_val));
            }
            BinaryOp::And | BinaryOp::InplaceAnd => {
                result = Some(Value::SmallInt(lhs_val & rhs_val));
            }
            BinaryOp::Lshift | BinaryOp::InplaceLshift => {
                if rhs_val < 0 {
                    return Err(crate::objects::objtype::objexcept::new_exception_msg(
                        ctx,
                        state.exc.value_error,
                        "negative shift count",
                    ));
                }
                if rhs_val >= 64
                    || lhs_val > (SMALL_INT_MAX >> rhs_val)
                    || lhs_val < (SMALL_INT_MIN >> rhs_val)
                {
                    go_big = true;
                } else {
                    result = Some(Value::SmallInt(lhs_val << rhs_val));
                }
            }
            BinaryOp::Rshift | BinaryOp::InplaceRshift => {
                if rhs_val < 0 {
                    return Err(crate::objects::objtype::objexcept::new_exception_msg(
                        ctx,
                        state.exc.value_error,
                        "negative shift count",
                    ));
                }
                let shift = rhs_val.min(63) as u32;
                result = Some(Value::SmallInt(lhs_val >> shift));
            }
            BinaryOp::Add | BinaryOp::InplaceAdd => {
                let v = lhs_val.wrapping_add(rhs_val);
                result = Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, v));
            }
            BinaryOp::Subtract | BinaryOp::InplaceSubtract => {
                let v = lhs_val.wrapping_sub(rhs_val);
                result = Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, v));
            }
            BinaryOp::Multiply | BinaryOp::InplaceMultiply => {
                match lhs_val.checked_mul(rhs_val) {
                    Some(v) => {
                        result = Some(Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, v));
                    }
                    None => go_big = true,
                }
            }
            BinaryOp::FloorDivide | BinaryOp::InplaceFloorDivide => {
                if rhs_val == 0 {
                    return Err(binop_zero_div(ctx, state));
                }
                // Floored division (Python `//`, Zig
                // `mp_small_int_floor_divide`) — NOT `div_euclid`:
                // Euclid's remainder is always non-negative, so
                // `-2 // -4` would come out `1` (remainder `2`)
                // instead of `0` (remainder `-2`, the divisor's
                // sign). `Value::int` is window-aware: the
                // `-(2**62) // -1 == 2**62` quotient is out of the
                // small-int window and becomes a heap int.
                let q = floored_div(lhs_val, rhs_val);
                result = Some(Value::int(
                    ctx.arena,
                    Some(ctx.types.int),
                    ctx.roots,
                    q,
                ));
            }
            BinaryOp::TrueDivide | BinaryOp::InplaceTrueDivide => {
                if rhs_val == 0 {
                    return Err(binop_zero_div(ctx, state));
                }
                result = Some(Value::Heap(ctx.alloc(
                    Some(ctx.types.float),
                    Obj::Float(lhs_val as f64 / rhs_val as f64),
                )));
            }
            BinaryOp::Modulo | BinaryOp::InplaceModulo => {
                if rhs_val == 0 {
                    return Err(binop_zero_div(ctx, state));
                }
                // Floored modulo (Zig `@mod`, Python `%`): the
                // result takes the DIVISOR's sign — NOT
                // `rem_euclid` (always non-negative, so
                // `-2 % -4` would come out `2` instead of `-2`).
                result = Some(Value::SmallInt(floored_mod(lhs_val, rhs_val)));
            }
            BinaryOp::Power | BinaryOp::InplacePower => {
                if let Some(exc) = float_precheck_int(ctx, state, op, lhs_val, rhs_val as f64) {
                    return Err(exc);
                }
                if rhs_val < 0 {
                    // `int ** -n` is a float (the Zig's
                    // `float_binary_op` path — f64 here).
                    result = Some(Value::Heap(ctx.alloc(
                        Some(ctx.types.float),
                        Obj::Float((lhs_val as f64).powf(rhs_val as f64)),
                    )));
                } else {
                    // Square-and-multiply with overflow → big.
                    // (`lhs_val` is never mutated — `base` is
                    // the mutable copy — so the post-match
                    // `go_big` block converts the ORIGINAL lhs,
                    // as the Zig does.)
                    let mut base = lhs_val;
                    let mut exp = rhs_val;
                    let mut ans: i64 = 1;
                    let mut overflow = false;
                    while exp > 0 {
                        if exp & 1 != 0 {
                            match ans.checked_mul(base) {
                                Some(v) => ans = v,
                                None => {
                                    overflow = true;
                                    break;
                                }
                            }
                        }
                        if exp == 1 {
                            break;
                        }
                        exp /= 2;
                        match base.checked_mul(base) {
                            Some(v) => base = v,
                            None => {
                                overflow = true;
                                break;
                            }
                        }
                    }
                    if overflow {
                        // The Zig converts the ORIGINAL lhs
                        // (`smallIntValue(lhs)` — `lhs` is still
                        // the original small int) to a heap int
                        // (the post-match `go_big` block below).
                        go_big = true;
                    } else {
                        result = Some(Value::SmallInt(ans));
                    }
                }
            }
            BinaryOp::Divmod => {
                if rhs_val == 0 {
                    return Err(binop_zero_div(ctx, state));
                }
                // Floored pair (same `div_euclid` trap as the
                // FloorDivide/Modulo arms above). `q` is
                // window-aware via `Value::int` (`-(2**62) // -1`
                // is a heap int `2**62`); `r` always fits.
                let q = Value::int(
                    ctx.arena,
                    Some(ctx.types.int),
                    ctx.roots,
                    floored_div(lhs_val, rhs_val),
                );
                let r = Value::SmallInt(floored_mod(lhs_val, rhs_val));
                let tuple_type = ctx.types.tuple;
                let tuple = crate::objects::containers::objtuple::new_tuple(
                    ctx, tuple_type, &[q, r],
                );
                result = Some(Value::Heap(tuple));
            }
            BinaryOp::Less => result = Some(Value::Bool(lhs_val < rhs_val)),
            BinaryOp::More => result = Some(Value::Bool(lhs_val > rhs_val)),
            BinaryOp::LessEqual => result = Some(Value::Bool(lhs_val <= rhs_val)),
            BinaryOp::MoreEqual => result = Some(Value::Bool(lhs_val >= rhs_val)),
            _ => {
                // Ops with no small-int fast path (IN, IS,
                // CONTAINS, MATMULTIPLY, reverse ops, …) — the
                // generic loop below raises `binopUnsupported`
                // for them (IN is swapped to CONTAINS there).
                return Err(binop_unsupported(ctx, state, op, lhs, rhs));
            }
        }
        if go_big {
            // `lhs` becomes a heap int (LSHIFT / MULTIPLY /
            // POWER overflow) — the generic path below uses the
            // int slot for the big-int op.
            lhs = new_int_from_bigint(
                ctx.arena,
                ctx.types,
                ctx.roots,
                &BigInt::from(lhs_val),
            );
        } else if let Some(v) = result {
            return Ok(v);
        }
    }
    // `int op float` / `int op complex` used to short-circuit to
    // `binopUnsupported` here (C13 stub, C18's scope). C18a
    // removes the block: the generic loop below reaches the int
    // slot, which delegates float/complex rhs to
    // `mp_obj_float_binary_op` / `mp_obj_complex_binary_op`
    // (vendor `objint_mpz.c:175-183`, `objint.rs`), so small-int,
    // heap-int and bool lhs all share one path — the Zig's
    // small-int fast path calls the same two functions directly
    // (`runtime.zig:703-719`), with identical observable behavior.
    // IN → CONTAINS (operands swapped).
    if op == BinaryOp::In {
        op = BinaryOp::Contains;
        std::mem::swap(&mut lhs, &mut rhs);
    }
    // E2d — same-kind container ordering with the VM hook:
    // the ctx-only `seq_cmp_objs` cannot run a FunBc `__lt__`
    // (stub seams), so the VM intercepts list/tuple ordering
    // here and compares element-wise through `binary_op_ctx`
    // itself. Only exact lists / exact tuples of the SAME
    // kind; anything else falls through to the generic loop
    // (slots, then `binopUnsupported`) exactly as before.
    if matches!(
        op,
        BinaryOp::Less | BinaryOp::More | BinaryOp::LessEqual | BinaryOp::MoreEqual
    ) {
        let pair: Option<(Vec<Value>, Vec<Value>)> =
            if is_exact_list(ctx, ctx.types.list, lhs)
                && is_exact_list(ctx, ctx.types.list, rhs)
            {
                Some((list_items_vec(ctx, lhs), list_items_vec(ctx, rhs)))
            } else if is_exact_tuple(ctx, ctx.types.tuple, lhs)
                && is_exact_tuple(ctx, ctx.types.tuple, rhs)
            {
                let l = tuple_items(ctx, lhs).map(|s| s.to_vec());
                let r = tuple_items(ctx, rhs).map(|s| s.to_vec());
                match (l, r) {
                    (Some(l), Some(r)) => Some((l, r)),
                    _ => None,
                }
            } else {
                None
            };
        if let Some((l_items, r_items)) = pair {
            return container_compare(ctx, state, op, &l_items, &r_items);
        }
    }
    // Generic loop: lhs slot → inplace→plain → reverse →
    // CONTAINS → `binopUnsupported`. An instance lhs tries its
    // dunder first, with the VM's real call hook (E2); no such
    // dunder falls through to the slot path below.
    loop {
        match crate::runtime::vm_seams::try_instance_binary_op(ctx, state, op, lhs, rhs) {
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {}
            Err(exc) => return Err(exc),
        }
        let type_ = obj_get_type(ctx.arena, ctx.types, lhs);
        match call_binary(ctx, type_, op, lhs, rhs) {
            Ok(SlotOutcome::Value(v)) => return Ok(v),
            Ok(SlotOutcome::NotImplemented) => {
                if ctx.has_pending_raise() {
                    return Err(pending_exc_or_type_error(ctx, state));
                }
            }
            Err(e) => return Err(exc_from_typed(ctx, state, e)),
        }
        let v = op.as_u8();
        if (9..=21).contains(&v) {
            // Inplace → plain, retry.
            op = BinaryOp::from_u8(v + 13).unwrap_or(op);
            continue;
        }
        if (22..=34).contains(&v) {
            // Plain → reverse (swap operands), retry.
            op = BinaryOp::from_u8(v + 15).unwrap_or(op);
            std::mem::swap(&mut lhs, &mut rhs);
            continue;
        }
        if (37..=49).contains(&v) {
            // Reverse failed — swap back for the error message.
            std::mem::swap(&mut lhs, &mut rhs);
            op = BinaryOp::from_u8(v - 15).unwrap_or(op);
        }
        if op == BinaryOp::Contains {
            return contains_op_ctx(ctx, state, lhs, rhs);
        }
        return Err(binop_unsupported(ctx, state, op, lhs, rhs));
    }
}

