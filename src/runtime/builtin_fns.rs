//! Plain builtins (`modbuiltins.zig`, D1).
//!
//! The builtins that need no VM state live here as `BuiltinFn` /
//! `BuiltinKwFn` values: `abs`, `hash`, `divmod`, `bin`, `hex`,
//! `oct`, `format`, `chr`, `ord`, `pow`, `repr`, `callable`, `id`,
//! `len`, `round`, `input` and the `open` stub (D2 fills the body
//! — see `open_builtin`). The stateful builtins (`globals`,
//! `locals`, `dir`, `sorted`, `min`, `max`, `getattr`, `setattr`,
//! `hasattr`, `delattr`, `iter`, `all`, `any`, `sum`) are
//! intercepted by GcRef identity in `vm_call::call_function_ctx`
//! and live in `builtin_vm.rs`; their `FunBuiltinData.func` slots
//! point at the shared `intercepted_stub_fn` below (the `next`
//! pattern — the stub is unreachable through the VM).
//!
//! Operator delegation: a plain `BuiltinFn` cannot call the
//! full `runtime_ops::binary_op` (it needs `ExcRefs`, the next
//! object, `Builtins` and `RuntimeSeams`), so `binary_op_plain`
//! reproduces the dispatcher's slot protocol at `CallCtx` level:
//! the lhs slot with the plain op, then — for the plain-arith ops
//! only, exactly as `generic_binary_op` does — the rhs slot with
//! the reverse op, then the dispatcher's formatted
//! `binop_unsupported` error. (The small-int fast path, the
//! float/complex promotion arms and `native_subclass_op` are the
//! dispatcher's, not this helper's: the int slot already delegates
//! a float/complex rhs to the float/complex slot (C18a), and the
//! str slot handles `str + str`; a user subclass overriding a
//! binary op is NOT honoured here — the VM expression path is.
//! Recorded in `scripts/d1_result.txt` §4.)

use std::io::{BufRead, Write};

use crate::memory::gc::CallCtx;
use crate::objects::containers::map::{hash_of, MapError};
use crate::objects::obj::{
    call_binary, call_unary, obj_get_float, obj_get_int, obj_get_type, obj_id, obj_is_callable,
    obj_is_true, obj_len, SlotOutcome,
};
use crate::objects::obj_ops::obj_print_helper;
use crate::objects::primitives::objint::int_pow3;
use crate::objects::primitives::ops::{int_from_float, is_exact_int};
use crate::objects::slots::{BinaryOp, PrintKind, UnaryOp};
use crate::objects::strings::objstr_a::new_str;
use crate::objects::strings::objstr_format::str_format;
use crate::objects::value::{Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::{
    raise_not_implemented, raise_type_error, raise_value_error,
};
use crate::runtime::runtime::RtError;
use crate::runtime::runtime_ops::{binop_unsupported, is_plain_arith, to_reverse};
use crate::support::readline::{readline as support_readline, CHAR_CTRL_D};

/// The shared stub body for the VM-intercepted builtins (see the
/// module docs): intercepted by identity before the type dispatch,
/// so this is unreachable through the VM — the `next` pattern.
pub fn intercepted_stub_fn(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// The dispatcher's slot protocol at `CallCtx` level (see the
/// module docs): lhs slot, then the rhs slot with the reverse op
/// for plain-arith ops, then the formatted "unsupported types"
/// error.
pub(crate) fn binary_op_plain(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let types = ctx.types;
    match call_binary(ctx, obj_get_type(ctx.arena, types, lhs), op, lhs, rhs) {
        Ok(SlotOutcome::Value(v)) => return Ok(v),
        Ok(SlotOutcome::NotImplemented) => {}
        Err(e) => return Err(ctx.record_typed(e)),
    }
    if is_plain_arith(op) {
        match call_binary(
            ctx,
            obj_get_type(ctx.arena, types, rhs),
            to_reverse(op),
            rhs,
            lhs,
        ) {
            Ok(SlotOutcome::Value(v)) => return Ok(v),
            Ok(SlotOutcome::NotImplemented) => {}
            Err(e) => return Err(ctx.record_typed(e)),
        }
    }
    match binop_unsupported(ctx, op, lhs, rhs) {
        RtError::TypeError(msg) => Err(raise_type_error(ctx, Some(&msg))),
        RtError::OutOfMemory => Err(PyError::OutOfMemory),
        // `binop_unsupported` only ever builds the TypeError above;
        // the rest is unreachable from here.
        _ => Err(PyError::PyRaise),
    }
}

/// `mp_builtin_abs` (`modbuiltins.zig`): the unary ABS slot, or
/// `TypeError("unsupported type for operator")` when it declines
/// (probed: `abs("x")`, `abs([])`, `abs(None)` all raise that).
pub fn abs_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    match call_unary(ctx, obj_get_type(ctx.arena, ctx.types, *arg), UnaryOp::Abs, *arg) {
        Ok(SlotOutcome::Value(v)) => Ok(v),
        Ok(SlotOutcome::NotImplemented) => Err(raise_type_error(
            ctx,
            Some("unsupported type for operator"),
        )),
        Err(e) => Err(ctx.record_typed(e)),
    }
}

/// `mp_builtin_hash` (`modbuiltins.zig`): `map::hash_of` (the
/// slot rule — a type with a unary-op slot that declines HASH
/// raises `TypeError("unsupported type for operator")`, a type
/// with no slot falls back to the id hash), as an int object.
pub fn hash_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    match hash_of(ctx, types, *arg) {
        Ok(h) => Ok(Value::int(ctx.arena, Some(types.int), ctx.roots, h as i64)),
        Err(MapError::TypeError(msg)) => Err(raise_type_error(ctx, Some(msg))),
        Err(MapError::OutOfMemory) => Err(PyError::OutOfMemory),
        Err(MapError::PyRaise) => Err(PyError::PyRaise),
    }
}

/// `mp_builtin_divmod` (`modbuiltins.zig`): the DIVMOD slot —
/// the slot itself returns the `(quotient, remainder)` tuple.
pub fn divmod_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [lhs, rhs] = args else {
        return Err(PyError::PyRaise);
    };
    binary_op_plain(ctx, BinaryOp::Divmod, *lhs, *rhs)
}

/// `bin` / `hex` / `oct` (`modbuiltins.zig`): `"{:#b}"` /
/// `"{:#x}"` / `"{:#o}"` through `str_format` (probed:
/// `bin(-10) == "-0b1010"`, `hex(-255) == "-0xff"`).
fn base_prefix_builtin(ctx: &mut CallCtx, args: &[Value], fmt: &[u8]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    let fmt_v = new_str(ctx, types, fmt);
    str_format(ctx, types, &[fmt_v, *arg], &[]).map_err(|e| ctx.record_typed(e))
}

pub fn bin_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    base_prefix_builtin(ctx, args, b"{:#b}")
}

pub fn hex_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    base_prefix_builtin(ctx, args, b"{:#x}")
}

pub fn oct_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    base_prefix_builtin(ctx, args, b"{:#o}")
}

/// `mp_builtin_format` (`modbuiltins.zig`): builds
/// `"{:" + spec + "}"` with real ADDs (a non-str `spec` raises
/// through the ADD, as on the original), then `str_format`.
pub fn format_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (value, spec) = match args {
        [v] => (*v, Value::Qstr(0)),
        [v, s] => (*v, *s),
        _ => return Err(PyError::PyRaise),
    };
    let types = ctx.types;
    // `spec` defaults to the empty qstr (id 0 — `""`); the two
    // ADDs are the Zig's string concatenations, verbatim.
    let open = new_str(ctx, types, b"{:");
    let fmt = binary_op_plain(ctx, BinaryOp::Add, open, spec)?;
    let close = new_str(ctx, types, b"}");
    let fmt = binary_op_plain(ctx, BinaryOp::Add, fmt, close)?;
    str_format(ctx, types, &[fmt, value], &[]).map_err(|e| ctx.record_typed(e))
}

/// `mp_builtin_chr` (`modbuiltins.zig`): `chr(i)` for
/// `0 <= i <= 0x10FFFF`, else `ValueError("chr() arg not in
/// range(0x110000)")`; a non-int raises "can't convert to int"
/// through `obj_get_int` (probed: `chr("a")`, `chr(3.5)`).
/// Lone surrogates are stored CESU-8 (`ED A0 80` for U+D800 —
/// probed: `len(chr(0xD800)) == 1`, `ord(chr(0xD800)) ==
/// 0xD800`), the same WTF-8 the reference's `utf8Encode`
/// produces; `utf8_charlen` counts the 3 bytes as one char.
pub fn chr_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    let n = obj_get_int(ctx, types, *arg).map_err(|e| ctx.record_typed(e))?;
    if !(0..=0x10FFFF).contains(&n) {
        return Err(raise_value_error(
            ctx,
            Some("chr() arg not in range(0x110000)"),
        ));
    }
    let n = n as u32;
    if (0xD800..0xE000).contains(&n) {
        let bytes = [
            0xED,
            0xA0 | ((n >> 6) & 0x0F) as u8,
            0x80 | (n & 0x3F) as u8,
        ];
        return Ok(new_str(ctx, types, &bytes));
    }
    let c = char::from_u32(n).expect("range-checked, non-surrogate above");
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    Ok(new_str(ctx, types, s.as_bytes()))
}

/// Decode exactly one character from `bytes` (WTF-8: plain
/// UTF-8 plus the CESU-8 surrogate sequences `chr` emits), or
/// `None` when the input is not exactly one character. Used by
/// `ord` — the reference decodes `chr(0xD800)` back to `0xD800`.
fn decode_one_char(bytes: &[u8]) -> Option<u32> {
    let (c, len) = match bytes.first()? {
        0x00..=0x7F => (bytes[0] as u32, 1),
        0xC2..=0xDF => {
            let b1 = bytes.get(1)?;
            if !matches!(b1, 0x80..=0xBF) {
                return None;
            }
            (((bytes[0] & 0x1F) as u32) << 6 | (b1 & 0x3F) as u32, 2)
        }
        0xE0..=0xEF => {
            let b1 = *bytes.get(1)?;
            let b2 = *bytes.get(2)?;
            if !matches!(b1, 0x80..=0xBF) || !matches!(b2, 0x80..=0xBF) {
                return None;
            }
            // CESU-8 surrogate pair: ED A0..AF 80..BF.
            if bytes[0] == 0xED && matches!(b1, 0xA0..=0xAF) {
                return Some(0xD800 + (((b1 & 0x0F) as u32) << 6 | (b2 & 0x3F) as u32))
                    .filter(|_| bytes.len() == 3);
            }
            (
                ((bytes[0] & 0x0F) as u32) << 12
                    | ((b1 & 0x3F) as u32) << 6
                    | (b2 & 0x3F) as u32,
                3,
            )
        }
        0xF0..=0xF4 => {
            let b1 = *bytes.get(1)?;
            let b2 = *bytes.get(2)?;
            let b3 = *bytes.get(3)?;
            if !matches!(b1, 0x80..=0xBF)
                || !matches!(b2, 0x80..=0xBF)
                || !matches!(b3, 0x80..=0xBF)
            {
                return None;
            }
            (
                ((bytes[0] & 0x07) as u32) << 18
                    | ((b1 & 0x3F) as u32) << 12
                    | ((b2 & 0x3F) as u32) << 6
                    | (b3 & 0x3F) as u32,
                4,
            )
        }
        _ => return None,
    };
    if bytes.len() == len {
        Some(c)
    } else {
        None
    }
}

/// `mp_builtin_ord` (`modbuiltins.zig`): a 1-character str →
/// its codepoint; a 1-byte bytes → its byte; anything else →
/// `TypeError("ord expects a character")` (probed: `ord("ab")`,
/// `ord(1)`, `ord(b"a") == 97`).
pub fn ord_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    let text = match *arg {
        Value::Qstr(id) => ctx.pool.qstr_str(id as u16),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => &s.bytes,
            Obj::Bytes(b) => {
                if b.bytes.len() == 1 {
                    return Ok(Value::int(
                        ctx.arena,
                        Some(types.int),
                        ctx.roots,
                        b.bytes[0] as i64,
                    ));
                }
                return Err(raise_type_error(ctx, Some("ord expects a character")));
            }
            _ => return Err(raise_type_error(ctx, Some("ord expects a character"))),
        },
        _ => return Err(raise_type_error(ctx, Some("ord expects a character"))),
    };
    match decode_one_char(text) {
        Some(c) => Ok(Value::int(ctx.arena, Some(types.int), ctx.roots, c as i64)),
        None => Err(raise_type_error(ctx, Some("ord expects a character"))),
    }
}

/// `mp_builtin_pow` (`modbuiltins.zig`, `kwVar(2, {mod})`):
/// 2 args → the POWER slot; 3 args (or a `mod` kwarg) → the
/// vendor `int_pow3`. A `mod` kwarg that is present — even
/// `None` — selects the 3-arg path (probed:
/// `pow(2, 3, mod=None)` raises "pow() with 3 arguments
/// requires integers"); a positional 3rd arg AND a `mod`
/// kwarg is the argcheck mismatch (probed:
/// `pow(2, 3, 5, mod=6)`).
fn pow_impl(
    ctx: &mut CallCtx,
    args: &[Value],
    mod_kw: Option<Value>,
) -> PyResult<Value> {
    let types = ctx.types;
    if args.len() < 2 {
        return Err(PyError::PyRaise);
    }
    match mod_kw {
        Some(m) => {
            if args.len() == 3 {
                return Err(raise_type_error(ctx, Some("argument num/types mismatch")));
            }
            int_pow3(ctx, types, args[0], args[1], m).map_err(|e| ctx.record_typed(e))
        }
        None => match args {
            [base, exp] => binary_op_plain(ctx, BinaryOp::Power, *base, *exp),
            [base, exp, modulus] => {
                int_pow3(ctx, types, *base, *exp, *modulus).map_err(|e| ctx.record_typed(e))
            }
            _ => Err(PyError::PyRaise),
        },
    }
}

pub fn pow_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    pow_impl(ctx, args, None)
}

pub fn pow_builtin_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let mod_q = crate::objects::primitives::ops::static_qstr_id(b"mod");
    let mod_kw = kwargs.iter().find(|(n, _)| Some(*n) == mod_q).map(|(_, v)| *v);
    pow_impl(ctx, args, mod_kw)
}

/// `mp_builtin_repr` (`modbuiltins.zig`): the REPR print slot
/// into a fresh string.
pub fn repr_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    let mut out = String::new();
    obj_print_helper(ctx, types, *arg, PrintKind::Repr, &mut out);
    Ok(new_str(ctx, types, out.as_bytes()))
}

/// `mp_builtin_callable` (`modbuiltins.zig`): the type's `call`
/// slot exists.
pub fn callable_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    Ok(Value::Bool(obj_is_callable(ctx.arena, ctx.types, *arg)))
}

/// `mp_obj_id` (`obj.zig`): the tagged immediates / arena index.
pub fn id_builtin(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    Ok(obj_id(*arg))
}

/// `mp_obj_len` (`obj.zig`): the LEN slot, or
/// `TypeError("object has no len")` (probed: `len(None)`,
/// `len(5)`, `len(2.5)`).
pub fn len_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let [arg] = args else {
        return Err(PyError::PyRaise);
    };
    let types = ctx.types;
    obj_len(ctx, types, *arg).map_err(|e| ctx.record_typed(e))
}

/// `mp_builtin_round` (`modbuiltins.zig`, `kwVar(1,
/// {ndigits})`): `round(x, None)` is `round(x)`; the int path
/// is the half-to-even binop algorithm (full config); the float
/// path is `nearbyint`. A float `ndigits` raises "can't convert
/// to int" through `obj_get_int` (probed:
/// `round(1.234, 2.0)`).
fn round_impl(
    ctx: &mut CallCtx,
    args: &[Value],
    ndigits_kw: Option<Value>,
) -> PyResult<Value> {
    let types = ctx.types;
    // Resolve the effective `ndigits`: the kwarg (when present
    // and not `None`) is the 2nd arg; `None` means the 1-arg form,
    // as in the Zig (`args[1] == OBJ_NONE`); a positional 2nd arg
    // AND a `ndigits` kwarg is the argcheck mismatch.
    let num_dig: Option<Value> = match ndigits_kw {
        Some(Value::None) => None,
        Some(d) => {
            if args.len() == 2 {
                return Err(raise_type_error(ctx, Some("argument num/types mismatch")));
            }
            Some(d)
        }
        // A positional 2nd arg of `None` is the 1-arg form too
        // (the Zig checks `args[1] == OBJ_NONE` before the
        // int conversion — `round(2.5, None)` ≡ `round(2.5)`).
        None => match args.get(1).copied() {
            Some(Value::None) | None => None,
            Some(d) => Some(d),
        },
    };
    let Some(o_in) = args.first().copied() else {
        return Err(PyError::PyRaise);
    };
    if matches!(o_in, Value::SmallInt(_))
        || (matches!(o_in, Value::Heap(_)) && is_exact_int(ctx.arena, types, o_in))
    {
        let Some(d) = num_dig else {
            return Ok(o_in);
        };
        let num_dig = obj_get_int(ctx, types, d).map_err(|e| ctx.record_typed(e))?;
        if num_dig >= 0 {
            return Ok(o_in);
        }
        return round_int_negative(ctx, o_in, num_dig);
    }
    // Float path (everything else goes through `get_float`).
    let val = obj_get_float(ctx, types, o_in).map_err(|e| ctx.record_typed(e))?;
    match num_dig {
        None => {
            let r = val.round_ties_even();
            int_from_float(ctx.arena, types, ctx.roots, r).map_err(|e| ctx.record_typed(e))
        }
        Some(d) => {
            let num_dig = obj_get_int(ctx, types, d).map_err(|e| ctx.record_typed(e))?;
            let m = 10f64.powf(num_dig as f64);
            let rounded = (val * m).round_ties_even() / m;
            Ok(Value::Heap(ctx.alloc(
                Some(types.float),
                Obj::Float(rounded),
            )))
        }
    }
}

/// The int path with negative `ndigits`: the half-to-even binop
/// algorithm (`roundImpl`, full config) — `10 ** -d`, `mult //
/// 2`, `o_in % mult`, `o_in - modulo`, then the tie-break
/// (`half > modulo` → down, `modulo > half` → up, else the
/// floor's parity decides).
fn round_int_negative(ctx: &mut CallCtx, o_in: Value, num_dig: i64) -> PyResult<Value> {
    let types = ctx.types;
    let Some(neg_d) = num_dig.checked_neg() else {
        return Err(PyError::OutOfMemory);
    };
    let ten = Value::SmallInt(10);
    let neg_d = Value::int(ctx.arena, Some(types.int), ctx.roots, neg_d);
    let two = Value::SmallInt(2);
    let one = Value::SmallInt(1);
    let mult = binary_op_plain(ctx, BinaryOp::Power, ten, neg_d)?;
    let half_mult = binary_op_plain(ctx, BinaryOp::FloorDivide, mult, two)?;
    let modulo = binary_op_plain(ctx, BinaryOp::Modulo, o_in, mult)?;
    let rounded = binary_op_plain(ctx, BinaryOp::Subtract, o_in, modulo)?;
    let half_gt = binary_op_plain(ctx, BinaryOp::More, half_mult, modulo)?;
    if obj_is_true(ctx, types, half_gt)? {
        return Ok(rounded);
    }
    let mod_gt = binary_op_plain(ctx, BinaryOp::More, modulo, half_mult)?;
    if obj_is_true(ctx, types, mod_gt)? {
        return binary_op_plain(ctx, BinaryOp::Add, rounded, mult);
    }
    let floor = binary_op_plain(ctx, BinaryOp::FloorDivide, o_in, mult)?;
    let floor_odd = binary_op_plain(ctx, BinaryOp::And, floor, one)?;
    if obj_is_true(ctx, types, floor_odd)? {
        return binary_op_plain(ctx, BinaryOp::Add, rounded, mult);
    }
    Ok(rounded)
}

pub fn round_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    round_impl(ctx, args, None)
}

pub fn round_builtin_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let nd_q = crate::objects::primitives::ops::static_qstr_id(b"ndigits");
    let ndigits_kw = kwargs
        .iter()
        .find(|(n, _)| Some(*n) == nd_q)
        .map(|(_, v)| *v);
    round_impl(ctx, args, ndigits_kw)
}

/// `mp_builtin_open` — D2's `file_io.zig` implementation
/// (`objfileio::open_builtin`): `open(path, mode="r")`,
/// returning a `TextIOWrapper` (text) or `FileIO`
/// (binary); `fopen` failure → `OSError(errno)`.
pub fn open_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    crate::objects::containers::objfileio::open_builtin(ctx, args)
}

pub fn open_builtin_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    crate::objects::containers::objfileio::open_builtin_kw(ctx, args, kwargs)
}

/// `mp_builtin_input` (`modbuiltins.zig:837-855`): print the
/// prompt (PRINT_STR), then `readline`. A 0x03 byte in the line
/// → `KeyboardInterrupt`; an empty line at EOF → `EOFError`;
/// else the line as a string.
pub fn input_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let prompt: Option<Vec<u8>> = match args {
        [] => None,
        [p] => {
            let types = ctx.types;
            let mut out = String::new();
            obj_print_helper(ctx, types, *p, PrintKind::Str, &mut out);
            Some(out.into_bytes())
        }
        _ => return Err(PyError::PyRaise),
    };
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    input_read(ctx, &mut reader, &mut writer, prompt.as_deref())
}

/// The testable core of `input_builtin`: the prompt (already
/// rendered to bytes) goes to `prompt_out`; the line comes from
/// `reader` via `support::readline` (256-byte `fgets` chunks,
/// trailing `\n` stripped). Returns the line, or raises
/// `KeyboardInterrupt` / `EOFError` (see `input_builtin`).
pub fn input_read<R: BufRead, W: Write>(
    ctx: &mut CallCtx,
    reader: &mut R,
    prompt_out: &mut W,
    prompt: Option<&[u8]>,
) -> PyResult<Value> {
    let types = ctx.types;
    let mut line: Vec<u8> = Vec::new();
    let ret = support_readline(reader, prompt_out, prompt, &mut line);
    if line.contains(&0x03) {
        ctx.record_raise("KeyboardInterrupt", None);
        return Err(PyError::PyRaise);
    }
    if line.is_empty() && ret == CHAR_CTRL_D {
        ctx.record_raise("EOFError", None);
        return Err(PyError::PyRaise);
    }
    Ok(new_str(ctx, types, &line))
}
