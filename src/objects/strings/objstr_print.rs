//! `mpprint.c` mirrors for the `%`-format / `str.format` paths
//! (Lane C8b): `mp_print_strn` (`mpprint.c:73-157`),
//! `mp_print_mp_int` (`mpprint.c:233-360`), `mp_print_float`
//! (`mpprint.c:361-392`), plus the `PF_FLAG` constants (vendor
//! `py/mpprint.h:31-40`).
//!
//! UNIFIED by Lane C8c: the three algorithms now live ONCE in
//! `mpprint.rs` as sink-generic cores (`print_strn_core` /
//! `print_mp_int_core` / `print_float_core`); the functions here
//! are thin Vstr wrappers with identical signatures, and the
//! `mp_print_*` exports in `mpprint.rs` are thin `Print` wrappers
//! over the same cores. The C8b probe records in
//! `scripts/strings_b_result.txt` pin the cores' observable
//! behaviour.
//!
//! `PRINT_STR`/`PRINT_REPR`/`PRINT_RAW` are vendor `py/obj.h:581-585`
//! (`0`/`1`/`4`).

use std::cmp::Ordering;

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int_maybe, BuiltinTypes};
use crate::objects::strings::mpprint::{
    print_float_core, print_mp_int_core, print_strn_core,
};
use crate::objects::strings::objstr_a::StrError;
use crate::objects::strings::vstr::Vstr;
use crate::objects::value::{Obj, Value};
use crate::vendor::mpz::BigInt;

// ── PF_FLAG constants (vendor `py/mpprint.h:31-40`) ───────────────────

pub const PF_LEFT_ADJUST: u32 = 0x001;
pub const PF_SHOW_SIGN: u32 = 0x002;
pub const PF_SPACE_SIGN: u32 = 0x004;
pub const PF_SHOW_PREFIX: u32 = 0x008;
pub const PF_PAD_AFTER_SIGN: u32 = 0x010;
pub const PF_CENTER_ADJUST: u32 = 0x020;
pub const PF_ADD_PERCENT: u32 = 0x040;
pub const PF_SHOW_OCTAL_LETTER: u32 = 0x080;
pub const PF_ALWAYS_DECIMAL: u32 = 0x100;
/// The grouping character (`','` or `'_'`) lives at bit 9
/// (`flags >> 9` is the separator byte, 0 when absent).
pub const PF_SEP_POS: u32 = 9;

// ── `mp_print_strn` (vendor `py/mpprint.c:73-157`) ────────────────────

/// `mp_print_strn` (`mpprint.c:73-157`): print `s` padded to
/// `width` with `fill`, honouring LEFT/CENTER. `width`/`len` are
/// `i32` (the C's `int`); a negative `pad` prints nothing.
/// Thin Vstr wrapper over `mpprint::print_strn_core` (the count
/// the core returns is discarded, as C8b's version did).
pub fn print_strn(
    v: &mut Vstr,
    s: &[u8],
    flags: u32,
    fill: u8,
    width: i32,
) -> Result<(), StrError> {
    print_strn_core(v, s, flags, fill, width).map(|_| ())
}

// ── `mp_print_mp_int` (vendor `py/mpprint.c:233-360`) ─────────────────

/// An integer value for `print_mp_int`: a small int, or a heap big
/// int. (`mp_print_mp_int` takes the object and formats it via
/// `mp_obj_int_formatted` — Lane C's `BigInt::as_str`.)
pub enum IntValue {
    I64(i64),
    Big(BigInt),
}

impl IntValue {
    pub fn is_negative(&self) -> bool {
        match self {
            IntValue::I64(n) => *n < 0,
            IntValue::Big(b) => b.cmp_mpz(&BigInt::zero()) == Ordering::Less,
        }
    }

    pub fn as_str(&self, base: u32, prefix: &str, base_char: u8, comma: u8) -> String {
        match self {
            IntValue::I64(n) => BigInt::from_int(*n).as_str(base, prefix, base_char, comma),
            IntValue::Big(b) => b.as_str(base, prefix, base_char, comma),
        }
    }
}

/// The integer value of `v` for printing, or `None` when `v` is not
/// an integer. Heap big ints are checked FIRST: C6's
/// `obj_get_int_maybe` ERRORS (`Overflow`) on an out-of-range heap
/// int, but `%d` must print it (probe: `"%d" % (2**70)` → all 19
/// digits). A bool yields 0/1 (via `obj_get_int_maybe`, which
/// handles `Bool`); a float yields `None` — the C's `mp_obj_get_int`
/// raises `TypeError: can't convert to int` for floats (vendor
/// `py/obj.c:306-313`: no implicit float→int conversion), so
/// `"%o" % 3.0` raises (probed).
pub fn int_value(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    v: Value,
) -> Result<Option<IntValue>, StrError> {
    if let Value::Heap(r) = v {
        if let Obj::Int(b) = ctx.arena.get(r) {
            return Ok(Some(IntValue::Big(b.clone())));
        }
    }
    match obj_get_int_maybe(ctx, types, v).map_err(StrError::Obj)? {
        Some(n) => Ok(Some(IntValue::I64(n))),
        None => Ok(None),
    }
}

/// `mp_print_mp_int` (`mpprint.c:233-360`): `base` is 2/8/10/16
/// only. `width`/`prec` are `i32` (the C's `int`); a negative
/// width/prec is inert (the C's comparisons make it so).
/// Thin Vstr wrapper over `mpprint::print_mp_int_core`.
pub fn print_mp_int(
    v: &mut Vstr,
    x: &IntValue,
    base: u32,
    base_char: u8,
    flags_in: u32,
    fill_in: u8,
    width_in: i32,
    prec_in: i32,
) -> Result<(), StrError> {
    print_mp_int_core(v, x, base, base_char, flags_in, fill_in, width_in, prec_in)
        .map(|_| ())
}

// ── `mp_print_float` (vendor `py/mpprint.c:361-392`) ──────────────────

/// `mp_print_float` (`mpprint.c:361-392`): `fmt` is one of
/// `eEfgG`; `prec` is the C's `int` (-1 = default 6). The buffer is
/// 33 bytes, matching the C's `buf[36]` with `sizeof(buf) - 3`.
/// Thin Vstr wrapper over `mpprint::print_float_core` (which also
/// carries the `prec == 99` repr dispatch).
pub fn print_float(
    v: &mut Vstr,
    f: f64,
    fmt: u8,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<(), StrError> {
    print_float_core(v, f, fmt, flags, fill, width, prec).map(|_| ())
}

/// `vspPrintFloatPct` (`objstr.zig`): `f * 100` formatted as `'f'`
/// (the `%` itself comes from `mp_print_float`'s `ADD_PERCENT`
/// handling — the caller ORs `PF_ADD_PERCENT` into `flags`).
pub fn print_float_pct(
    v: &mut Vstr,
    f: f64,
    flags: u32,
    fill: u8,
    width: i32,
    prec: i32,
) -> Result<(), StrError> {
    print_float(v, f * 100.0, b'f', flags, fill, width, prec)
}
