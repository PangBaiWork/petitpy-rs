//! str / bytes slots + constructors (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 842–934
//! (`mpObjStrBinaryOpImpl`), 407–470 (`npyStrSliceLoad`),
//! 1017–1130 (`bytesSubscrImpl` / `strSubscrImpl` / `str_subscr`),
//! 3624–3633 (`str_unary_op_full`), 2083–2090
//! (`mp_obj_str_get_buffer`), and 1705–2000 (`makeNew*` /
//! `strMakeNewImpl` / `bytesMakeNewImpl` / `mp_obj_str_make_new` /
//! `bytes_make_new`).
//!
//! The slot functions (`str_binary_op`, `str_subscr`,
//! `bytes_subscr`, `str_unary_op`, `str_get_buffer`) have the
//! frozen C6 signatures (`fn(&mut CallCtx, …) -> PyResult<…>`),
//! so they resolve `&BuiltinTypes` through the `INSTALLED_TYPES`
//! thread-local (`objstr_a.rs`) and record specific raises in the
//! `PENDING_STR_ERROR` cell before returning `PyRaise` — see that
//! file for the contract. The `*_inner` helpers take `types`
//! explicitly and return `SlotResult` (`None` = the original
//! returns NULL, i.e. "not implemented"; `Some` = a value or a
//! specific `StrError`), so tests can assert values and exact
//! error texts without going through the thread-locals.

use crate::memory::gc::CallCtx;
use crate::objects::obj::{
    obj_get_int, obj_get_int_maybe, obj_get_type, BuiltinTypes, BUFFER_READ,
};
use crate::objects::obj_ops::{get_index, obj_print_helper};
use crate::objects::slots::{BinaryOp, BufferInfo, SubscrArg, UnaryOp};
use crate::objects::strings::objstr_a::*;
use crate::objects::strings::unicode::{utf8_charlen, utf8_next_char};
use crate::objects::value::{GcRef, Obj, SliceData, Value};
use crate::pyerr::{PyError, PyResult};

/// Outcome of a str/bytes slot operation: `None` = the original
/// returns NULL ("not implemented" — the slot wrapper records no
/// pending error); `Some` = a value or a specific `StrError` (the
/// wrapper records it in the pending-error cell).
pub type SlotResult = Option<Result<Value, StrError>>;

// ── mp_seq_cmp_bytes ──────────────────────────────────────────────────

/// `mp_seq_cmp_bytes` (vendor `py/sequence.c:90-130`): the shared
/// str/bytes comparison. `op` is one of EQUAL/LESS/LESS_EQUAL/MORE/
/// MORE_EQUAL — `NOT_EQUAL` is never passed (the caller inverts
/// EQUAL). EQUAL with different lengths is false; LESS/LESS_EQUAL
/// swap operands into MORE/MORE_EQUAL; then `memcmp` over the
/// shorter length decides, with a length tiebreak.
pub fn seq_cmp_bytes(op: BinaryOp, lhs: &[u8], rhs: &[u8]) -> bool {
    let (mut lhs, mut rhs, mut op) = (lhs, rhs, op);
    if op == BinaryOp::Equal {
        if lhs.len() != rhs.len() {
            return false;
        }
    } else if op == BinaryOp::Less || op == BinaryOp::LessEqual {
        std::mem::swap(&mut lhs, &mut rhs);
        op = if op == BinaryOp::Less {
            BinaryOp::More
        } else {
            BinaryOp::MoreEqual
        };
    }
    let min_len = lhs.len().min(rhs.len());
    let res = lhs[..min_len].cmp(&rhs[..min_len]);
    if op == BinaryOp::Equal {
        return res == std::cmp::Ordering::Equal;
    }
    match res {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => {
            if lhs.len() != rhs.len() {
                // Tie on the common prefix: the longer one is more.
                lhs.len() > rhs.len()
            } else {
                // Equal contents: MORE is false, MORE_EQUAL is true.
                op != BinaryOp::More
            }
        }
    }
}

// ── binary_op ─────────────────────────────────────────────────────────

/// `mpObjStrBinaryOpImpl` (`objstr.zig:842-932`): the str/bytes
/// binary-op slot core. `MODULO` is C8b's (`str_modulo_format`) —
/// `None` here. `MULTIPLY` needs an int rhs (`get_int_maybe`
/// failing is NULL, not a raise); `n <= 0` yields the empty str
/// (qstr) / empty bytes. The rhs data is read by same-type, then
/// by the buffer protocol for a bytes lhs (`b"a" + "b"` is `b"ab"`
/// on the reference binary); a str lhs with an incompatible rhs
/// raises for `CONTAINS` and is NULL otherwise. `ADD`: an empty
/// lhs of the same type returns rhs itself, an empty rhs returns
/// lhs itself. Comparisons go through `seq_cmp_bytes`.
pub fn binary_op_inner(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> SlotResult {
    if op == BinaryOp::Modulo {
        return None;
    }
    let lhs_type = obj_get_type(ctx.arena, types, lhs);
    let lhs_data = match str_data(ctx.arena, lhs) {
        Some(d) => d.to_vec(),
        None => return None,
    };
    let lhs_is_str = is_str_value(ctx.arena, lhs);

    if op == BinaryOp::Multiply || op == BinaryOp::InplaceMultiply {
        let n = match obj_get_int_maybe(ctx, types, rhs) {
            Ok(Some(n)) => n,
            Ok(None) => return None,
            Err(e) => return Some(Err(StrError::Obj(e))),
        };
        if n <= 0 {
            return Some(Ok(if lhs_is_str {
                new_empty_str()
            } else {
                new_empty_bytes(ctx, types)
            }));
        }
        let mut out = Vec::with_capacity(lhs_data.len() * n as usize);
        for _ in 0..n {
            out.extend_from_slice(&lhs_data);
        }
        return Some(Ok(new_str_of_type(
            ctx,
            types,
            result_type(types, lhs_type, lhs_is_str),
            &out,
        )));
    }

    let rhs_data: Vec<u8>;
    if obj_get_type(ctx.arena, types, rhs) == lhs_type {
        rhs_data = match str_data(ctx.arena, rhs) {
            Some(d) => d.to_vec(),
            None => return None,
        };
    } else if !lhs_is_str {
        match buffer_data(ctx, types, rhs) {
            Ok(d) => rhs_data = d,
            Err(_) => return None,
        }
    } else {
        if op == BinaryOp::Contains {
            return Some(Err(StrError::BadImplicitConversion));
        }
        return None;
    }

    if op == BinaryOp::Add || op == BinaryOp::InplaceAdd {
        if lhs_data.is_empty() && obj_get_type(ctx.arena, types, rhs) == lhs_type {
            return Some(Ok(rhs));
        }
        if rhs_data.is_empty() {
            return Some(Ok(lhs));
        }
        let mut out = lhs_data.clone();
        out.extend_from_slice(&rhs_data);
        return Some(Ok(new_str_of_type(
            ctx,
            types,
            result_type(types, lhs_type, lhs_is_str),
            &out,
        )));
    } else if op == BinaryOp::Contains {
        return Some(Ok(Value::Bool(
            find_subbytes(&lhs_data, &rhs_data, 1).is_some(),
        )));
    } else if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::More
            | BinaryOp::MoreEqual
    ) {
        return Some(Ok(Value::Bool(seq_cmp_bytes(op, &lhs_data, &rhs_data))));
    }
    None
}

/// `mp_obj_str_binary_op` (`objstr.zig:934-936`): the slot — the
/// frozen `BinaryOpFn` shape, resolving `types` through the
/// installed-types cell.
pub fn str_binary_op(
    ctx: &mut CallCtx,
    op: BinaryOp,
    lhs: Value,
    rhs: Value,
) -> PyResult<Value> {
    let Some(types) = installed_types() else {
        return Err(PyError::PyRaise);
    };
    match binary_op_inner(ctx, &types, op, lhs, rhs) {
        None => Err(PyError::PyRaise),
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(slot_raise(e)),
    }
}

// ── subscr ────────────────────────────────────────────────────────────

/// `npyStrSliceLoad` (`objstr.zig:407-442`): slice a byte-indexed
/// str/bytes (bytes, or an ASCII str — the fast path delegates
/// here). Step 1 takes the contiguous range; other steps gather
/// byte by byte (`slice_indices` + `sliceCount` — the vendor
/// raises `NotImplementedError` for step != 1, petitpy implements
/// it, ADR 010 §C3).
fn slice_load_bytes(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    self_value: Value,
    data: &[u8],
    slice: &SliceData,
) -> Result<Value, StrError> {
    let self_type = obj_get_type(ctx.arena, types, self_value);
    let is_str = is_str_value(ctx.arena, self_value);
    let result_type = result_type(types, self_type, is_str);
    let (bounds, is_fast) = fast_slice_indexes(ctx, types, slice, data.len() as i64)?;
    if is_fast {
        let (start, stop) = (bounds.start as usize, bounds.stop as usize);
        return Ok(new_str_of_type(ctx, types, result_type, &data[start..stop]));
    }
    // The slow path re-resolves with the RAW `slice_indices` bounds:
    // `fast_slice_indexes` adjusts stop for a negative step
    // (`stop += 1`), which `slice_count` must not see.
    let bounds = slice_indices(ctx, types, slice, data.len() as i64)?;
    let count = slice_count(&bounds);
    if count == 0 {
        return Ok(new_str_of_type(ctx, types, result_type, &[]));
    }
    let mut out = Vec::with_capacity(count);
    let mut i = bounds.start;
    for _ in 0..count {
        out.push(data[i as usize]);
        i += bounds.step;
    }
    Ok(new_str_of_type(ctx, types, result_type, &out))
}

/// The non-ASCII branch of `strSubscrImpl` (`objstr.zig:1076-1098`):
/// slice a unicode str by code point. `offs` maps each code-point
/// index to its byte offset (plus the end); the step-1 fast path
/// takes the contiguous byte range, other steps gather whole
/// code-point byte ranges.
fn slice_load_str(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    self_value: Value,
    data: &[u8],
    n_chars: usize,
    slice: &SliceData,
) -> Result<Value, StrError> {
    let self_type = obj_get_type(ctx.arena, types, self_value);
    let is_str = is_str_value(ctx.arena, self_value);
    let result_type = result_type(types, self_type, is_str);
    let mut offs: Vec<usize> = Vec::with_capacity(n_chars + 1);
    for (b, _) in data.iter().enumerate() {
        if !crate::objects::strings::unicode::is_utf8_cont(data[b]) {
            offs.push(b);
        }
    }
    offs.push(data.len());
    debug_assert_eq!(offs.len(), n_chars + 1);
    let (bounds, is_fast) = fast_slice_indexes(ctx, types, slice, n_chars as i64)?;
    if is_fast {
        let (start, stop) = (bounds.start as usize, bounds.stop as usize);
        return Ok(new_str_of_type(
            ctx,
            types,
            result_type,
            &data[offs[start]..offs[stop]],
        ));
    }
    let bounds = slice_indices(ctx, types, slice, n_chars as i64)?;
    let count = slice_count(&bounds);
    if count == 0 {
        return Ok(new_str_of_type(ctx, types, result_type, &[]));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut i = bounds.start;
    for _ in 0..count {
        let c = i as usize;
        out.extend_from_slice(&data[offs[c]..offs[c + 1]]);
        i += bounds.step;
    }
    Ok(new_str_of_type(ctx, types, result_type, &out))
}

/// `bytesSubscrImpl` (`objstr.zig:1017-1043`) + `strSubscrImpl`
/// (`objstr.zig:1045-1098`): the subscript core. Load only — set
/// and delete are NULL. A slice goes through the slice loaders;
/// a non-slice index on bytes (or an ASCII str, which delegates to
/// the byte path) uses C6's `get_index` — `TypeError: indices must
/// be integers` / `IndexError: index out of range`; on a non-ASCII
/// str it uses `str_index_to_ptr` — `TypeError: string indices
/// must be integers` / `IndexError: string index out of range`.
/// Bytes yield the byte as a small int; str yields a 1-char str.
pub(crate) fn subscr_inner(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    self_value: Value,
    index: Value,
    arg: SubscrArg,
) -> SlotResult {
    if !matches!(arg, SubscrArg::Get) {
        return None;
    }
    let data = match str_data(ctx.arena, self_value) {
        Some(d) => d.to_vec(),
        None => return None,
    };
    let is_bytes = is_bytes_value(ctx.arena, self_value);
    if let Value::Heap(r) = index {
        if let Obj::Slice(slice) = ctx.arena.get(r) {
            let slice = slice.clone();
            let res = if is_bytes || utf8_charlen(&data) == data.len() {
                slice_load_bytes(ctx, types, self_value, &data, &slice)
            } else {
                slice_load_str(ctx, types, self_value, &data, utf8_charlen(&data), &slice)
            };
            return Some(res);
        }
    }
    if is_bytes || utf8_charlen(&data) == data.len() {
        let idx = match get_index(ctx, types, data.len(), index, false) {
            Ok(i) => i,
            Err(e) => return Some(Err(StrError::Obj(e))),
        };
        if is_bytes {
            return Some(Ok(Value::SmallInt(data[idx] as i64)));
        }
        return Some(Ok(new_str_via_qstr(ctx, types, &data[idx..idx + 1])));
    }
    let idx = match str_index_to_ptr(ctx, types, &data, false, index, false) {
        Ok(i) => i,
        Err(e) => return Some(Err(e)),
    };
    let end = utf8_next_char(&data, idx);
    Some(Ok(new_str_via_qstr(ctx, types, &data[idx..end])))
}

/// `str_subscr` (`objstr.zig:1101-1103`): the str subscript slot.
pub fn str_subscr(
    ctx: &mut CallCtx,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    let Some(types) = installed_types() else {
        return Err(PyError::PyRaise);
    };
    match subscr_inner(ctx, &types, base, index, arg) {
        None => Err(PyError::PyRaise),
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(slot_raise(e)),
    }
}

/// `bytes_subscr` (`objstr.zig:1046-1048`): the bytes subscript
/// slot — same core, reached through the bytes type's slot table.
pub fn bytes_subscr(
    ctx: &mut CallCtx,
    base: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    str_subscr(ctx, base, index, arg)
}

// ── unary_op ──────────────────────────────────────────────────────────

/// `str_unary_op_full` (`objstr.zig:3624-3633`): the str unary-op
/// slot (full only — bytes has NO unary_op slot; `len(bytes)` is
/// served by C6's byte fast path). `BOOL` is the BYTE length != 0,
/// `LEN` is the UTF-8 code-point count; anything else is NULL.
pub fn str_unary_op(ctx: &mut CallCtx, op: UnaryOp, arg: Value) -> PyResult<Value> {
    let data = match str_data(ctx.arena, arg) {
        Some(d) => d,
        None => return Err(PyError::PyRaise),
    };
    match op {
        UnaryOp::Bool => Ok(Value::Bool(!data.is_empty())),
        UnaryOp::Len => Ok(Value::SmallInt(utf8_charlen(data) as i64)),
        _ => Err(PyError::PyRaise),
    }
}

// ── get_buffer ────────────────────────────────────────────────────────

/// `mp_obj_str_get_buffer` (`objstr.zig:2083-2090`): the buffer
/// slot. The `flags` word arrives with `MP_BUFFER_RAISE_IF_UNSUPPORTED`
/// already masked off (`obj::get_buffer`); only an exact
/// `MP_BUFFER_READ` succeeds, anything else (e.g. `MP_BUFFER_RW`)
/// is "not supported". On success the info is the raw data, length
/// and typecode `'B'`.
pub fn str_get_buffer(
    ctx: &mut CallCtx,
    value: Value,
    bufinfo: &mut BufferInfo,
    flags: u32,
) -> PyResult<bool> {
    if flags != BUFFER_READ {
        return Ok(false);
    }
    let Some(data) = str_data(ctx.arena, value) else {
        return Ok(false);
    };
    bufinfo.data = data.as_ptr();
    bufinfo.len = data.len();
    bufinfo.typecode = b'B';
    Ok(true)
}

// ── make_new ──────────────────────────────────────────────────────────

/// One keyword argument passed to a `make_new`: the keyword name
/// (static qstr id) and its value. The runtime call protocol
/// (C10/C13) owns the canonical kwargs representation; this is the
/// minimal shape `str`/`bytes` construction needs — only the keyword
/// NAMES matter (`encoding`/`errors` values are parsed and ignored
/// by the original).
#[derive(Clone, Copy, Debug)]
pub struct KwArg {
    pub name: u32,
    pub value: Value,
}

/// `KwPos.init` (`argcheck.zig:172-191`) for a constructor: merge
/// positional args + kwargs into positional slots `names` (filling
/// `defaults` for slots the caller skipped), exactly as the
/// original. `defaults[i] == None` means "no default — the slot is
/// required". Failures are TERSE (`ArgNumTypesMismatch`,
/// "argument num/types mismatch"): too many positionals, an unknown
/// keyword, a duplicate (positional + keyword), or a required slot
/// left empty. Returns the merged positional args (trailing empty
/// slots trimmed, as `r.n` does).
pub fn kw_make_new(
    args: &[Value],
    kwargs: &[KwArg],
    names: &[u32; 3],
    defaults: &[Option<Value>; 3],
) -> Result<Vec<Value>, StrError> {
    if args.len() > names.len() {
        return Err(StrError::ArgNumTypesMismatch);
    }
    let mut merged: [Option<Value>; 3] = [None, None, None];
    for (i, a) in args.iter().enumerate() {
        merged[i] = Some(*a);
    }
    let mut n = args.len();
    for kw in kwargs {
        let Some(i) = names.iter().position(|q| *q == kw.name) else {
            return Err(StrError::ArgNumTypesMismatch);
        };
        if merged[i].is_some() {
            return Err(StrError::ArgNumTypesMismatch);
        }
        merged[i] = Some(kw.value);
        n = n.max(i + 1);
    }
    for (slot, default) in merged
        .iter_mut()
        .zip(defaults.iter())
        .skip(args.len())
        .take(n - args.len())
    {
        if slot.is_none() {
            match default {
                Some(d) => *slot = Some(*d),
                None => return Err(StrError::ArgNumTypesMismatch),
            }
        }
    }
    Ok(merged[..n].iter().map(|s| s.unwrap()).collect())
}

/// `makeNewCase1` (`objstr.zig:1823-1835`): print `arg0` as a str
/// (`PRINT_STR` via C6's `obj_print_helper`) into a buffer and
/// build a new str/bytes of `type_` from it. (`str(b'ab')` is
/// `"b'ab'"` — the print quotes bytes — while `str(b'ab',
/// 'utf-8')` takes the 2-arg path below.)
fn make_new_case1(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    arg0: Value,
) -> Value {
    let mut s = String::new();
    obj_print_helper(ctx, types, arg0, crate::objects::slots::PrintKind::Str, &mut s);
    new_str_of_type(ctx, types, type_, s.as_bytes())
}

/// `makeNewDefaultBytes` (`objstr.zig:1705-1721`): `str(bytes)` —
/// static-qstr fast path when the content matches one, else a fresh
/// str sharing the source bytes (copied in Rust — strings are
/// immutable, so sharing is unobservable).
fn make_new_default_bytes(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    arg0: Value,
) -> Value {
    let data = str_data(ctx.arena, arg0).unwrap_or(&[]).to_vec();
    if let Some(q) = find_static_qstr(&data) {
        return Value::Qstr(q);
    }
    new_str_of_type(ctx, types, type_, &data)
}

/// `makeNewDefaultOther` (`objstr.zig:1837-1848`): `str(x, enc)` for
/// a non-bytes `x` — read a buffer (raising `BufferProtocol` when
/// the object has none; a NULL buffer yields the empty qstr).
/// NOTE: the original crashes on `str(123, 'utf-8')` — its
/// `mp_get_buffer` failure is ignored and a garbage pointer is fed
/// to `qstr_compute_hash`. Rust raises instead (the intended
/// error), a deliberate divergence.
fn make_new_default_other(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    arg0: Value,
) -> Result<Value, StrError> {
    let data = buffer_data(ctx, types, arg0)?;
    if data.is_empty() {
        return Ok(new_empty_str());
    }
    Ok(new_str(ctx, types, &data))
}

/// `mp_obj_str_make_new` (`objstr.zig:1855-1879`): `str()`,
/// `str(x)`, `str(b, encoding[, errors])`, with kwargs
/// (`object`/`encoding`/`errors`) merged by `kwMakeNew` on full.
/// 0 args → empty qstr; 1 arg → `makeNewCase1`; 2–3 args: a bytes
/// arg0 → `makeNewDefaultBytes`, else `makeNewDefaultOther`. The
/// arg-count check is TERSE ("argument num/types mismatch"); the
/// original's void C call fails to stop execution for 4 positionals
/// and crashes — Rust raises the intended error.
pub fn str_make_new(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    type_: GcRef,
    args: &[Value],
    kwargs: &[KwArg],
) -> Result<Value, StrError> {
    let names = [
        static_qstr_id(b"object").unwrap_or(0),
        static_qstr_id(b"encoding").unwrap_or(0),
        static_qstr_id(b"errors").unwrap_or(0),
    ];
    let utf8 = Value::Qstr(static_qstr_id(b"utf-8").unwrap_or(0));
    let defaults = [None, Some(utf8), None];
    let merged = if kwargs.is_empty() {
        args.to_vec()
    } else {
        kw_make_new(args, kwargs, &names, &defaults)?
    };
    if merged.len() > 3 {
        return Err(StrError::ArgNumTypesMismatch);
    }
    match merged.len() {
        0 => Ok(new_empty_str()),
        1 => Ok(make_new_case1(ctx, types, type_, merged[0])),
        _ => {
            if is_bytes_type(ctx.arena, types, merged[0]) {
                Ok(make_new_default_bytes(ctx, types, type_, merged[0]))
            } else {
                make_new_default_other(ctx, types, merged[0])
            }
        }
    }
}

/// `bytesMakeNewImpl` (`objstr.zig:1898-1937`): `bytes()`,
/// `bytes(b)`, `bytes(s, encoding[, errors])`, `bytes(n)`,
/// `bytes(iterable)`. 0 args → empty bytes; a bytes arg0 returns
/// as-is; a str arg0 needs 2–3 args (else the explicit
/// `raiseWrongArgs`, "wrong number of arguments") and is copied as
/// bytes; a non-str with >1 arg → same raise; a small int is a
/// zero-filled length (negative → `NegLen`, a bare `ValueError`);
/// a buffer is copied; otherwise the iterable path walks the items
/// (`obj_get_int` each — non-int → "can't convert to int",
/// out-of-range → `ValueError: bytes value out of range`).
pub fn bytes_make_new(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
    kwargs: &[KwArg],
) -> Result<Value, StrError> {
    let names = [
        static_qstr_id(b"source").unwrap_or(0),
        static_qstr_id(b"encoding").unwrap_or(0),
        static_qstr_id(b"errors").unwrap_or(0),
    ];
    let utf8 = Value::Qstr(static_qstr_id(b"utf-8").unwrap_or(0));
    let defaults = [None, Some(utf8), None];
    let merged = if kwargs.is_empty() {
        args.to_vec()
    } else {
        kw_make_new(args, kwargs, &names, &defaults)?
    };
    if merged.is_empty() {
        return Ok(new_empty_bytes(ctx, types));
    }
    let arg0 = merged[0];
    if is_bytes_type(ctx.arena, types, arg0) {
        return Ok(arg0);
    }
    if is_str_type(ctx.arena, types, arg0) {
        if merged.len() < 2 || merged.len() > 3 {
            return Err(StrError::WrongArgs);
        }
        let data = str_data(ctx.arena, arg0).unwrap_or(&[]).to_vec();
        return Ok(new_bytes(ctx, types, &data));
    }
    if merged.len() > 1 {
        return Err(StrError::WrongArgs);
    }
    if let Value::SmallInt(len) = arg0 {
        if len < 0 {
            return Err(StrError::NegLen);
        }
        return Ok(new_bytes(ctx, types, &vec![0u8; len as usize]));
    }
    // No small-int fast path for bools: `mirrors.isSmallInt` is
    // false for the bool immediates, so `bytes(True)` falls through
    // to the buffer check (fails) and then the iterable path, which
    // raises "object not iterable" (probed on the reference binary).
    match buffer_data(ctx, types, arg0) {
        Ok(data) => return Ok(new_bytes(ctx, types, &data)),
        Err(_) => {}
    }
    let items = iter_items(ctx, types, arg0)?;
    let mut out: Vec<u8> = Vec::new();
    for item in &items {
        let val = obj_get_int(ctx, types, *item)?;
        if !(0..=255).contains(&val) {
            return Err(StrError::BytesValueOutOfRange);
        }
        out.push(val as u8);
    }
    Ok(new_bytes(ctx, types, &out))
}
