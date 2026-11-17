//! Argument checking (`runtime/argcheck.zig`, Zig port of vendor
//! `py/argcheck.c`).
//!
//! The canonical arg-check module for the tree (C15a). Two helpers
//! already existed in scattered form — `errors.rs::arg_check_num_sig`
//! (decoded min/max, polled `Option`) and `objfun.rs::arg_check_num`
//! (decoded min/max, bool predicate) — both are the same predicate
//! over decoded bounds; this module adds the packed-`sig` form the
//! Zig actually takes (`mp_arg_check_num_sig(n_args, n_kw, sig)`),
//! the `mp_arg_parse_all` keyword parser, and the `KwPos` /
//! `kwMakeNew` positional+keyword slotting. Existing call sites
//! are NOT rewritten (see the result file); new code uses this
//! module.
//!
//! Polled model (the Zig's `pyerr.bridge` + `terse`): a failed
//! check records the single terse `TypeError` in
//! `ctx.pending_raise` (first record wins — `record_raise`) and
//! returns `Err(PyError::PyRaise)`; the caller keeps the error and
//! lets a later error overwrite it only by recording again, which
//! `record_raise` refuses — so the FIRST failure is the one that
//! surfaces, exactly as the Zig's polled callers behave (the
//! `tuple(1, 2)` → "object not iterable" vs `tuple("a", "b")` →
//! "argument num/types mismatch" ordering is decided inside each
//! `make_new` by which check it runs first, not here).

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int, obj_is_true};
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};

// ── `mp_arg_t` flag constants (vendor `py/runtime.h:47-52`) ─────────

/// `MP_ARG_BOOL` — the slot converts with `mp_obj_is_true`.
pub const ARG_BOOL: u16 = 0x001;
/// `MP_ARG_INT` — the slot converts with `mp_obj_get_int`.
pub const ARG_INT: u16 = 0x002;
/// `MP_ARG_OBJ` — the slot takes the object as-is.
pub const ARG_OBJ: u16 = 0x003;
/// `MP_ARG_KIND_MASK` — the kind bits of `flags`.
pub const ARG_KIND_MASK: u16 = 0x0ff;
/// `MP_ARG_REQUIRED` — missing → terse mismatch.
pub const ARG_REQUIRED: u16 = 0x100;
/// `MP_ARG_KW_ONLY` — given positionally → terse mismatch.
pub const ARG_KW_ONLY: u16 = 0x200;

/// The single terse error text (nano `ERROR_REPORTING <= TERSE`):
/// every arg-check failure in the Zig is this one
/// (`argcheck.zig:29-33`, `makeTerseExc`).
pub const TERSE_MISMATCH: &str = "argument num/types mismatch";

/// `makeFunSig` (`mp_mirrors.zig:464`): pack `(min, max,
/// takes_kw)` into the `sig` word `mp_arg_check_num_sig` takes:
/// `takes_kw` is bit 0, `max` is bits 1–16, `min` is bits 17+.
pub fn make_sig(min: u32, max: u32, takes_kw: bool) -> u32 {
    (min << 17) | (max << 1) | u32::from(takes_kw)
}

/// Decode a packed `sig` word into `(min, max, takes_kw)`
/// (`argcheck.zig:40-42`).
pub fn sig_decode(sig: u32) -> (usize, usize, bool) {
    ((sig >> 17) as usize, ((sig >> 1) & 0xffff) as usize, sig & 1 != 0)
}

/// `mpArgCheckNumSigImpl` (`argcheck.zig:39-54`) as a pure
/// predicate: `true` when the counts are in range. The Zig
/// branches on `min == max` (`:45-53`); the two branches are the
/// same predicate, written once here (the `objfun.rs` note).
pub fn check_num_sig(n_args: usize, n_kw: usize, sig: u32) -> bool {
    let (min, max, takes_kw) = sig_decode(sig);
    if n_kw != 0 && !takes_kw {
        return false;
    }
    n_args >= min && n_args <= max
}

/// `mp_arg_check_num_sig` (`argcheck.zig:39-54` + the
/// `pyerr.bridge` wrapper at `:116`): the packed-sig check that
/// records the terse raise (first record wins) and returns
/// `Err(PyRaise)` on failure. The Zig's exported wrapper sets
/// `pending_exc` and returns `void` — the caller polls; in Rust
/// the `Err` IS the poll signal (the VM drains `pending_raise`
/// after the slot returns).
pub fn arg_check_num_sig(
    ctx: &mut CallCtx,
    n_args: usize,
    n_kw: usize,
    sig: u32,
) -> PyResult<()> {
    if check_num_sig(n_args, n_kw, sig) {
        Ok(())
    } else {
        Err(terse_mismatch(ctx))
    }
}

/// `mp_arg_error_terse_mismatch_polled` (`argcheck.zig:155-158`):
/// record the terse mismatch and return the `PyRaise` signal.
/// (The noreturn `mp_arg_error_terse_mismatch` at `:148-151` is
/// `@panic("unreachable under nano")` in the Zig and has no
/// vendor-C callers in the full build — it is NOT ported; see
/// the result file.)
pub fn terse_mismatch(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise("TypeError", Some(TERSE_MISMATCH));
    PyError::PyRaise
}

// ── `mp_arg_parse_all` ───────────────────────────────────────────────

/// One `mp_arg_t` entry (`mp_mirrors.zig:385-396`): the keyword
/// (`qst`, a qstr id), the `ARG_*` flags, and the default value.
/// The Zig's `defval` is the `MpArgVal` union; `ArgVal` is the
/// same three variants. A spec's `defval` kind must match its
/// flags kind (callers construct them consistently).
#[derive(Clone, Debug, PartialEq)]
pub struct ArgSpec {
    pub qst: u32,
    pub flags: u16,
    pub defval: ArgVal,
}

/// One `mp_arg_val_t` (`mp_mirrors.zig:375-380`).
#[derive(Clone, Debug, PartialEq)]
pub enum ArgVal {
    Bool(bool),
    Int(i64),
    Obj(Value),
}

/// `mp_arg_parse_all` (`argcheck.zig:55-155`, vendor
/// `py/argcheck.c:78-130`) over the keyword-array form: `pos`
/// are the positional arguments, `kws` the `(qstr, value)` pairs
/// in call order (the `mp_arg_parse_all_kw_array` fixed table —
/// the Zig builds the table from the pairs and runs the same
/// `Impl`, so this IS the `Impl`, not a second algorithm).
///
/// For each allowed slot in order: a positional fills it
/// (`KW_ONLY` → terse); otherwise the keyword map is searched by
/// qstr (first match wins, as a fixed-table lookup) — missing
/// with `REQUIRED` → terse, missing without it → the default.
/// `BOOL` converts with `mp_obj_is_true`, `INT` with
/// `mp_obj_get_int`, anything else takes the object as-is. A
/// trailing positional (`pos.len() > n_allowed`) or an
/// unmatched keyword (`kws` entry no spec claims) → terse.
/// A conversion failure returns `Err` at once with the raise
/// recorded (the Zig keeps filling `out_vals` and lets the
/// caller poll `pending_exc`; the filled values are never
/// observed after a failure, so stopping early is unobservable).
pub fn arg_parse_all(
    ctx: &mut CallCtx,
    pos: &[Value],
    kws: &[(u32, Value)],
    allowed: &[ArgSpec],
) -> PyResult<Vec<ArgVal>> {
    let mut pos_found = 0usize;
    let mut kws_found = 0usize;
    let mut out: Vec<ArgVal> = Vec::with_capacity(allowed.len());
    for (i, arg) in allowed.iter().enumerate() {
        let given = if i < pos.len() {
            if arg.flags & ARG_KW_ONLY != 0 {
                return Err(terse_mismatch(ctx));
            }
            pos_found += 1;
            pos[i]
        } else {
            match kws.iter().find(|(k, _)| *k == arg.qst) {
                None => {
                    if arg.flags & ARG_REQUIRED != 0 {
                        return Err(terse_mismatch(ctx));
                    }
                    out.push(arg.defval.clone());
                    continue;
                }
                Some((_, v)) => {
                    kws_found += 1;
                    *v
                }
            }
        };
        let kind = arg.flags & ARG_KIND_MASK;
        if kind == ARG_BOOL {
            out.push(ArgVal::Bool(obj_is_true(ctx, ctx.types, given)?));
        } else if kind == ARG_INT {
            let n = obj_get_int(ctx, ctx.types, given)
                .map_err(|e| ctx.record_typed(e))?;
            out.push(ArgVal::Int(n));
        } else {
            out.push(ArgVal::Obj(given));
        }
    }
    if pos_found < pos.len() {
        return Err(terse_mismatch(ctx));
    }
    if kws_found < kws.len() {
        return Err(terse_mismatch(ctx));
    }
    Ok(out)
}

// ── `KwPos` / `kwMakeNew` ────────────────────────────────────────────

/// `KwPos(names)` (`argcheck.zig:167-204`): positional
/// arguments with the keyword ones moved into their slots.
/// `args[i]` is `None` for an empty slot (`OBJ_NULL` in the
/// Zig); `n` counts up to the highest slot given, so the callee
/// sees what a positional call of the same arguments would pass.
#[derive(Clone, Debug, PartialEq)]
pub struct KwPos {
    pub args: Vec<Option<Value>>,
    pub n: usize,
}

/// `KwPos::init` (`argcheck.zig:180-202`): `names[i]` is the
/// keyword for slot `i` (0 for a positional-only slot). A slot
/// left empty below one given by keyword takes `defaults[i]`,
/// or is an error when that is `None` (`OBJ_NULL`). `kws` are
/// the `(qstr, value)` pairs in call order.
pub fn kw_pos_parse(
    ctx: &mut CallCtx,
    names: &[u32],
    defaults: &[Option<Value>],
    n_pos: usize,
    pos: &[Value],
    kws: &[(u32, Value)],
) -> PyResult<KwPos> {
    if n_pos > names.len() {
        return Err(terse_mismatch(ctx));
    }
    let mut args: Vec<Option<Value>> = vec![None; names.len()];
    for (i, v) in pos.iter().take(n_pos).enumerate() {
        args[i] = Some(*v);
    }
    let mut n = n_pos;
    for (key, value) in kws {
        let Some(i) = names.iter().position(|&q| q != 0 && q == *key) else {
            return Err(terse_mismatch(ctx));
        };
        if args[i].is_some() {
            return Err(terse_mismatch(ctx));
        }
        args[i] = Some(*value);
        n = n.max(i + 1);
    }
    for (i, slot) in args.iter_mut().enumerate().take(n).skip(n_pos) {
        if slot.is_none() {
            match defaults.get(i).copied().flatten() {
                Some(d) => *slot = Some(d),
                None => return Err(terse_mismatch(ctx)),
            }
        }
    }
    Ok(KwPos { args, n })
}

/// `kwMakeNew` (`argcheck.zig:207-210`): `KwPos` for a
/// constructor's `make_new`, whose keyword pairs follow the
/// positional arguments in `args` as flat `(key, value)` pairs.
/// A non-qstr key can never match a slot (the Zig compares
/// against `objFromQstr(q)`) → terse.
pub fn kw_make_new(
    ctx: &mut CallCtx,
    names: &[u32],
    defaults: &[Option<Value>],
    args: &[Value],
    n_kw: usize,
) -> PyResult<KwPos> {
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(terse_mismatch(ctx));
    };
    let mut kws: Vec<(u32, Value)> = Vec::with_capacity(n_kw);
    for pair in args[n_pos..].chunks_exact(2) {
        let Value::Qstr(key) = pair[0] else {
            return Err(terse_mismatch(ctx));
        };
        kws.push((key, pair[1]));
    }
    kw_pos_parse(ctx, names, defaults, n_pos, &args[..n_pos], &kws)
}
