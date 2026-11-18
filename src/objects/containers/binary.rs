//! `binary.zig` — the `struct`/`array` byte layer (`py/binary.c`).
//!
//! Pure byte manipulation plus the two value-converting entry
//! points the `array` type (and, later, `modstruct`) build on.
//! Ported from `src/objects/containers/binary.zig` (295 lines,
//! 8 class-A exports, 0 nlr-hosts).
//!
//! Config (probed, `binary.zig:1-14`): `MP_ENDIANNESS_LITTLE=1`,
//! `MP_BYTES_PER_OBJ_WORD=8`, `FLOAT=0` (half-float + `e`/`f`/`d`
//! *value* branches dead; their SIZES stay — struct layout),
//! `LONGINT_IMPL=NONE` (the `q`/`Q` value + `int_to_bytes_impl` /
//! exact-int paths are dead *in the vendor C sense* — the Zig still
//! calls `mp_obj_int_to_bytes_impl` for exact ints, and the probe
//! `array('i', [2**70 + 5])[0] == 5` proves the low-bytes write is
//! live, so the exact-int path IS ported here), `STRUCT_UNSAFE_
//! TYPECODES=1` (`O`/`P`/`S` live). Widths come from Zig
//! `@sizeOf`/`@alignOf(c_*)` — ABI-exact on x86-64:
//! `c_short`=2, `c_int`=4, `c_long`=8, `c_longlong`=8, pointer=8.
//!
//! Faithful points (lesson #6):
//! - `is_signed(tc) = (tc > 'Z')` — the whole signed/unsigned
//!   decision is one comparison.
//! - C integer narrowing on store is `@truncate` of the
//!   two's-complement bits — verified: `array('i', [2**31])[0] ==
//!   -2147483648`, `array('b', [200])[0] == -56`,
//!   `array('B', [300])[0] == 44`.
//! - `mp_binary_get_size('<' | '>', tc)`: `l`/`L` are 4 bytes
//!   (struct layout), while `'@'` gives the native 8 — the one
//!   place the two modes disagree.
//! - `e`/`f`/`d` have NO value branch in `get_val_array` /
//!   `set_val_array_from_int`: storing an int writes nothing (the
//!   slot stays zero) and reading returns small int 0 — verified:
//!   `array('f', [1]) == array('f', [0])` and `[0] == 0`.
//!   Floats are REJECTED for `f`/`d` arrays (`array('f', [1.5])`
//!   raises `TypeError: can't convert to int`, via `mp_obj_get_int`).
//!
//! Safe-Rust limits (no `unsafe` in this lane):
//! - `get_val`'s `'S'` branch reads a C string through a raw
//!   pointer stored in the buffer; there is no safe way to deref
//!   it, so it returns an empty `str` (pinned by test). `set_val`
//!   for `'S'` is fully portable (`get_int` + `set_int`).
//! - `get_val`/`set_val`'s `'O'` branch stores the raw `Obj` word.
//!   The Rust word encoding is the REPR_A encoding for immediates
//!   (`SmallInt(n)` → `(n << 1) | 1`, `Bool` → 30/14, `None` → 6,
//!   `Qstr(id)` → `(id << 3) | 2`) and `(index + 1) << 3` for a heap
//!   object (ends in `0b000`, distinct from qstr words `0b010`).
//!   The Zig's pointer bits are not reproducible; the observable
//!   `pack` → `unpack` round-trip is preserved while the object
//!   lives. The packed buffer does NOT root the object — `modstruct`
//!   (D lane) must root packed objects itself. A zero word (an
//!   unwritten slot, unreachable through the API) reads back as
//!   `None`.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::obj::{obj_get_int, ObjError};
use crate::objects::value::{GcRef, Obj, Value, SMALL_INT_MAX, SMALL_INT_MIN};
use crate::pyerr::PyError;
use crate::vendor::mpz::BigInt;

/// `mp.BYTEARRAY_TYPECODE` (`py/binary.h`): the array typecode
/// that means "bytearray" — `arrayNew` picks the bytearray type
/// iff `typecode == 1`, else the array type.
pub const BYTEARRAY_TYPECODE: u8 = 1;

/// The `0x80` bit OR'd into a memoryview's typecode when the viewed
/// buffer is writable (`MP_OBJ_ARRAY_TYPECODE_FLAG_RW`).
pub const ARRAY_TYPECODE_FLAG_RW: u8 = 0x80;

/// Mask for the typecode bits (`MP_OBJ_ARRAY_TYPECODE_MASK`).
pub const TYPECODE_MASK: u8 = 0x7f;

/// `mp_obj_int_to_bytes_impl` failures, the `set_val_array`
/// exact-int path, and the `get_size` bad-typecode raise, as one
/// error type (the C9a `DictError` shape: `exc_name()` for the
/// VM's pending-exception wiring, `to_pyerror()` for the slot
/// boundary — the message is dropped there until C13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinaryError {
    /// `ValueError("bad typecode")` — `get_size` with size 0.
    BadTypecode,
    /// `TypeError("can't convert to int")` — `get_int` on a
    /// non-int value (e.g. a float stored into an array).
    IntConversion,
    /// `OverflowError("overflow converting long int to machine
    /// word")` — `get_int` on a heap int outside `i64`.
    Overflow,
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary — see module
    /// docs). Covers `get_int` failures that are neither
    /// `IntConversion` nor `Overflow` (only `OutOfMemory` in
    /// practice, which is mapped above).
    PyRaise,
}

impl BinaryError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            BinaryError::BadTypecode => "ValueError",
            BinaryError::IntConversion => "TypeError",
            BinaryError::Overflow => "OverflowError",
            BinaryError::OutOfMemory => "MemoryError",
            BinaryError::PyRaise => "",
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` /
    /// `PyRaise`, which carry no message of their own).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            BinaryError::BadTypecode => Some("bad typecode"),
            BinaryError::IntConversion => Some("can't convert to int"),
            BinaryError::Overflow => {
                Some("overflow converting long int to machine word")
            }
            BinaryError::OutOfMemory | BinaryError::PyRaise => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            BinaryError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<ObjError> for BinaryError {
    fn from(e: ObjError) -> BinaryError {
        match e {
            ObjError::IntConversion => BinaryError::IntConversion,
            ObjError::Overflow => BinaryError::Overflow,
            ObjError::OutOfMemory => BinaryError::OutOfMemory,
            _ => BinaryError::PyRaise,
        }
    }
}

/// `isSigned(tc)` (`binary.zig:47`): signed iff the typecode is
/// past `'Z'` in the ASCII table (`b`/`h`/`i`/`l`/`q` signed;
/// `B`/`H`/`I`/`L`/`Q` unsigned; `e`/`f`/`d` "signed" — dead
/// branches, but the predicate still answers true).
pub fn is_signed_typecode(tc: u8) -> bool {
    tc > b'Z'
}

/// `mp_binary_get_size` (`binary.zig:54-101`): `(size, align)` of
/// one element of `val_type` under `struct_type` (`'<'`, `'>'`,
/// `'@'`). `'<'`/`'>'` use the fixed struct layout (`l`/`L` = 4);
/// `'@'` uses native sizes/aligns (`l`/`L` = 8, `q`/`Q` = 8,
/// `P`/`O`/`S` = pointer). Any other `struct_type` behaves like
/// `'<'`/`'>'` for the sized cases and yields size 0 for the
/// rest. Size 0 raises `ValueError("bad typecode")`.
pub fn binary_get_size(
    struct_type: u8,
    val_type: u8,
) -> Result<(usize, usize), BinaryError> {
    let (mut size, mut al) = (0usize, 1usize);
    if struct_type == b'<' || struct_type == b'>' {
        size = match val_type {
            b'b' | b'B' => 1,
            b'h' | b'H' => 2,
            b'i' | b'I' => 4,
            b'l' | b'L' => 4,
            b'q' | b'Q' => 8,
            b'P' | b'O' | b'S' => 8,
            b'e' => 2,
            b'f' => 4,
            b'd' => 8,
            _ => 0,
        };
    } else if struct_type == b'@' {
        if val_type == BYTEARRAY_TYPECODE || val_type == b'b' || val_type == b'B' {
            al = 1;
            size = 1;
        } else if val_type == b'h' || val_type == b'H' {
            al = 2;
            size = 2;
        } else if val_type == b'i' || val_type == b'I' {
            al = 4;
            size = 4;
        } else if val_type == b'l' || val_type == b'L' {
            al = 8;
            size = 8;
        } else if val_type == b'q' || val_type == b'Q' {
            al = 8;
            size = 8;
        } else if val_type == b'P' || val_type == b'O' || val_type == b'S' {
            al = 8;
            size = 8;
        } else if val_type == b'e' {
            al = 2;
            size = 2;
        } else if val_type == b'f' {
            al = 4;
            size = 4;
        } else if val_type == b'd' {
            al = 8;
            size = 8;
        }
    }
    if size == 0 {
        return Err(BinaryError::BadTypecode);
    }
    Ok((size, al))
}

/// The REPR_A word of a `Value` (see module docs): immediates use
/// the exact `mp_mirrors.zig` encoding; a heap object uses its
/// arena index tagged `(index + 1) << 3`.
pub fn value_word(v: Value) -> u64 {
    match v {
        Value::SmallInt(n) => ((n as u64) << 1) | 1,
        Value::Bool(false) => 14,
        Value::Bool(true) => 30,
        Value::None => 6,
        Value::Qstr(id) => ((id as u64) << 3) | 2,
        Value::Heap(r) => (r.0 as u64 + 1) << 3,
    }
}

/// Inverse of `value_word` (see module docs). Word `0` — an
/// unwritten slot, unreachable through the API — reads as `None`.
pub fn value_from_word(w: u64) -> Value {
    if w & 1 == 1 {
        Value::SmallInt((w as i64) >> 1)
    } else if w == 6 {
        Value::None
    } else if w == 14 {
        Value::Bool(false)
    } else if w == 30 {
        Value::Bool(true)
    } else if w & 7 == 2 {
        Value::Qstr((w >> 3) as u32)
    } else if w == 0 {
        Value::None
    } else {
        Value::Heap(GcRef(((w >> 3) - 1) as u32))
    }
}

/// `mp_obj_new_int` for a `u64` value: small int in-window, heap
/// `Int(BigInt)` out of window (`from_ll` with `is_signed=false`
/// reads the `u64` bits unsigned — `mpz_set_from_ll`).
pub fn new_int_from_u64(ctx: &mut CallCtx, v: u64) -> Value {
    if v <= SMALL_INT_MAX as u64 {
        Value::SmallInt(v as i64)
    } else {
        Value::Heap(ctx.alloc(
            Some(ctx.types.int),
            Obj::Int(BigInt::from_ll(v as i64, false)),
        ))
    }
}

/// `mp_obj_new_int` for an `i64` value (small int in-window, heap
/// `Int` out of window).
pub fn new_int_from_i64(ctx: &mut CallCtx, v: i64) -> Value {
    Value::int(ctx.arena, Some(ctx.types.int), ctx.roots, v)
}

/// Read `size` bytes at `off` as a little-endian `u64` (missing
/// bytes read as 0 — defensive; callers pass in-range slices).
fn read_le_u64(data: &[u8], off: usize, size: usize) -> u64 {
    let mut v: u64 = 0;
    for i in 0..size {
        let b = data.get(off + i).copied().unwrap_or(0) as u64;
        v |= b << (8 * i);
    }
    v
}

/// `mp_binary_get_val_array` (`binary.zig:114-141`): element
/// `index` of type `typecode` out of the raw byte buffer `data`.
/// `i`/`l` return `new_int`, `I`/`L`/`Q` return `from_uint`/
/// `from_ull` (heap ints past the small-int window), `O` returns
/// the stored word as a `Value`, `P` returns the pointer bits as
/// an int. `e`/`f`/`d` (and anything unknown) have NO branch —
/// the fallthrough returns small int 0.
pub fn binary_get_val_array(
    ctx: &mut CallCtx,
    typecode: u8,
    data: &[u8],
    index: usize,
) -> Value {
    let (size, _) = match binary_get_size(b'@', typecode) {
        Ok(s) => s,
        Err(_) => return Value::SmallInt(0),
    };
    let off = index * size;
    match typecode {
        b'b' => Value::SmallInt(read_le_u64(data, off, 1) as i8 as i64),
        BYTEARRAY_TYPECODE | b'B' => Value::SmallInt(read_le_u64(data, off, 1) as i64),
        b'h' => Value::SmallInt(read_le_u64(data, off, 2) as i16 as i64),
        b'H' => Value::SmallInt(read_le_u64(data, off, 2) as i64),
        b'i' => Value::SmallInt(read_le_u64(data, off, 4) as i32 as i64),
        b'I' => new_int_from_u64(ctx, read_le_u64(data, off, 4)),
        b'l' => new_int_from_i64(ctx, read_le_u64(data, off, 8) as i64),
        b'L' => new_int_from_u64(ctx, read_le_u64(data, off, 8)),
        b'q' => new_int_from_i64(ctx, read_le_u64(data, off, 8) as i64),
        b'Q' => new_int_from_u64(ctx, read_le_u64(data, off, 8)),
        b'O' => value_from_word(read_le_u64(data, off, 8)),
        b'P' => new_int_from_i64(ctx, read_le_u64(data, off, 8) as i64),
        _ => Value::SmallInt(0),
    }
}

/// `mp_binary_get_int` (`binary.zig:143-166`): `size` bytes at
/// `src` as an integer; `size == 0` → 0 (the Zig safe-checks the
/// C `size - 1` wrap). Signed values sign-extend from the top
/// bit; big-endian reads forward, little-endian reads the bytes
/// in reverse (the Zig's `delta` loop, mirrored).
pub fn binary_get_int(size: usize, is_signed: bool, big_endian: bool, src: &[u8]) -> i64 {
    if size == 0 {
        return 0;
    }
    let mut val: u64 = 0;
    // First byte in read order decides the sign extension.
    let first = if big_endian {
        src.first().copied().unwrap_or(0)
    } else {
        src.get(size - 1).copied().unwrap_or(0)
    };
    if is_signed && (first & 0x80) != 0 {
        val = u64::MAX;
    }
    for i in 0..size {
        let b = if big_endian {
            src.get(i).copied().unwrap_or(0)
        } else {
            src.get(size - 1 - i).copied().unwrap_or(0)
        } as u64;
        val = (val << 8) | b;
    }
    val as i64
}

/// `mp_binary_set_int` (`binary.zig:168-184`): write the low
/// `val_sz` bytes of `val` into `dest`; little-endian copies
/// straight, big-endian reverses. Short `dest` writes what fits
/// (defensive; callers size it).
pub fn binary_set_int(val_sz: usize, big_endian: bool, dest: &mut [u8], val: u64) {
    let n = val_sz.min(dest.len());
    if !big_endian {
        dest[..n].copy_from_slice(&val.to_le_bytes()[..n]);
    } else {
        for i in 0..n {
            dest[i] = (val >> (8 * (val_sz - 1 - i))) as u8;
        }
    }
}

fn align_up(off: usize, alignment: usize) -> usize {
    (off + (alignment - 1)) & !(alignment - 1)
}

/// `mp_binary_get_val` (`binary.zig:190-222`): one struct value
/// of `val_type` out of `buf` at `*pos` (advanced past the
/// value; `'@'` aligns first, then reads little-endian). `O`
/// returns the stored word as a `Value`; `S` returns an empty
/// `str` (see module docs — the pointer cannot be dereferenced
/// in safe Rust); signed values become a small int in-window
/// else `from_ll`; unsigned become `from_uint` in-window else
/// `from_ull`.
pub fn binary_get_val(
    ctx: &mut CallCtx,
    struct_type_in: u8,
    val_type: u8,
    buf: &[u8],
    pos: &mut usize,
) -> Result<Value, BinaryError> {
    let mut struct_type = struct_type_in;
    let (size, al) = binary_get_size(struct_type, val_type)?;
    let mut p = *pos;
    if struct_type == b'@' {
        p = align_up(p, al);
        struct_type = b'<';
    }
    *pos = p + size;
    let slice = buf.get(p..).unwrap_or(&[]);
    let v = binary_get_int(size, is_signed_typecode(val_type), struct_type == b'>', slice);
    if val_type == b'O' {
        return Ok(value_from_word(v as u64));
    }
    if val_type == b'S' {
        // Cannot deref the stored pointer in safe Rust (see
        // module docs): an empty str, pinned by test.
        let t = ctx.types.str_;
        return Ok(Value::Heap(ctx.alloc(
            Some(t),
            Obj::Str(crate::objects::value::StrData { bytes: Vec::new() }),
        )));
    }
    if is_signed_typecode(val_type) {
        if SMALL_INT_MIN <= v && v <= SMALL_INT_MAX {
            Ok(Value::SmallInt(v))
        } else {
            Ok(new_int_from_i64(ctx, v))
        }
    } else {
        let uv = v as u64;
        if uv <= SMALL_INT_MAX as u64 {
            Ok(Value::SmallInt(uv as i64))
        } else {
            Ok(new_int_from_u64(ctx, uv))
        }
    }
}

/// `mp_binary_set_val` (`binary.zig:224-248`): write one struct
/// value of `val_type` into `buf` at `*pos` (advanced past the
/// value; `'@'` aligns first, then writes little-endian). `O`
/// stores the `Value`'s word; an exact int (full) writes its
/// two's-complement bytes directly (`int_to_bytes_impl` — return
/// ignored, i.e. truncated to `size`); anything else goes
/// through `get_int` (`TypeError`/`OverflowError` propagate) and
/// `set_int(min(size, 8))`.
pub fn binary_set_val(
    ctx: &mut CallCtx,
    struct_type_in: u8,
    val_type: u8,
    val_in: Value,
    buf: &mut [u8],
    pos: &mut usize,
) -> Result<(), BinaryError> {
    let mut struct_type = struct_type_in;
    let (size, al) = binary_get_size(struct_type, val_type)?;
    let mut p = *pos;
    if struct_type == b'@' {
        p = align_up(p, al);
        struct_type = b'<';
    }
    *pos = p + size;
    let val: u64;
    if val_type == b'O' {
        val = value_word(val_in);
    } else {
        let is_exact_int = matches!(val_in, Value::SmallInt(_))
            || crate::objects::obj::is_exact_int(ctx.arena, ctx.types, val_in);
        // NOTE: `is_exact_int` is false for small ints (C6: the
        // tag bits fail `isObj`), so small ints take the `get_int`
        // path below — exactly as in the Zig, where `isExactType`
        // is likewise false for them. Only heap ints take the
        // direct-bytes path.
        if matches!(val_in, Value::Heap(_)) && is_exact_int {
            if let Value::Heap(r) = val_in {
                if let Obj::Int(z) = ctx.arena.get(r) {
                    let z = z.clone();
                    let end = (p + size).min(buf.len());
                    if p < buf.len() {
                        let _ = z.as_bytes(false, z.is_neg(), &mut buf[p..end]);
                    }
                    return Ok(());
                }
            }
            val = obj_get_int(ctx, ctx.types, val_in)? as u64;
        } else {
            val = obj_get_int(ctx, ctx.types, val_in)? as u64;
        }
    }
    let m = size.min(8);
    let dest = buf.get_mut(p..).unwrap_or(&mut []);
    binary_set_int(m, struct_type == b'>', dest, val);
    Ok(())
}

/// `mp_binary_set_val_array` (`binary.zig:250-267`): store `val`
/// as element `index` of type `typecode` in `data`. `O` stores
/// the raw `Value` word; an exact heap int writes its
/// two's-complement bytes directly (return ignored — truncated
/// to the element size; verified: `array('i', [2**70 + 5])[0]
/// == 5`); anything else goes through `get_int` and
/// `set_val_array_from_int` (`TypeError` for floats, `Overflow`
/// for huge ints passed as non-exact values — the exact-int
/// branch takes heap ints first).
pub fn binary_set_val_array(
    ctx: &mut CallCtx,
    typecode: u8,
    data: &mut [u8],
    index: usize,
    val_in: Value,
) -> Result<(), BinaryError> {
    let (size, _) = binary_get_size(b'@', typecode)?;
    let off = index * size;
    if typecode == b'O' {
        if off < data.len() {
            binary_set_int(8, false, &mut data[off..], value_word(val_in));
        }
        return Ok(());
    }
    if matches!(val_in, Value::Heap(_))
        && crate::objects::obj::is_exact_int(ctx.arena, ctx.types, val_in)
    {
        if let Value::Heap(r) = val_in {
            if let Obj::Int(z) = ctx.arena.get(r) {
                let z = z.clone();
                let end = (off + size).min(data.len());
                if off < data.len() {
                    let _ = z.as_bytes(false, z.is_neg(), &mut data[off..end]);
                }
                return Ok(());
            }
        }
    }
    let v = obj_get_int(ctx, ctx.types, val_in)?;
    binary_set_val_array_from_int(typecode, data, index, v);
    Ok(())
}

/// `mp_binary_set_val_array_from_int` (`binary.zig:269-295`):
/// store the `isize` `val` as element `index` of type `typecode`
/// in `data`, truncating to the element width (`@truncate` —
/// verified: `array('b', [200])[0] == -56`). `q`/`Q` store the
/// full 64-bit value; `P` stores the pointer bits; `e`/`f`/`d`
/// have NO branch (a no-op — the slot stays zero).
pub fn binary_set_val_array_from_int(
    typecode: u8,
    data: &mut [u8],
    index: usize,
    val: i64,
) {
    let (size, _) = match binary_get_size(b'@', typecode) {
        Ok(s) => s,
        Err(_) => return,
    };
    let off = index * size;
    if off >= data.len() || off + size > data.len() {
        return;
    }
    let bytes = &mut data[off..off + size];
    match typecode {
        b'b' => bytes[..1].copy_from_slice(&(val as i8).to_le_bytes()),
        BYTEARRAY_TYPECODE | b'B' => bytes[..1].copy_from_slice(&(val as u8).to_le_bytes()),
        b'h' => bytes[..2].copy_from_slice(&(val as i16).to_le_bytes()),
        b'H' => bytes[..2].copy_from_slice(&(val as u16).to_le_bytes()),
        b'i' => bytes[..4].copy_from_slice(&(val as i32).to_le_bytes()),
        b'I' => bytes[..4].copy_from_slice(&(val as u32).to_le_bytes()),
        b'l' => bytes[..8].copy_from_slice(&(val as i64).to_le_bytes()),
        b'L' => bytes[..8].copy_from_slice(&(val as u64).to_le_bytes()),
        b'P' => bytes[..8].copy_from_slice(&(val as u64).to_le_bytes()),
        b'q' | b'Q' => bytes[..8].copy_from_slice(&val.to_le_bytes()),
        _ => {}
    }
}

/// `mp_binary_get_size` for the `'@'` (native) mode only: the
/// element size of an array typecode (what `arrayNew` /
/// `arrayGetBuffer` use). Bad typecodes raise like the Zig
/// (`array('x')` → `ValueError("bad typecode")`).
pub fn array_item_size(typecode: u8) -> Result<usize, BinaryError> {
    Ok(binary_get_size(b'@', typecode)?.0)
}

/// Arena-only raw bytes of an `Obj::Array` (E2e): the
/// `buffer_bytes` array logic without the `CallCtx` (the Qstr
/// arm is not needed — `str_data_pool` handles qstrs itself).
/// Resolves view chains from the arena (`source` is the base
/// `Array` / `Bytes` / `Str`, `free` the element offset).
/// `None` when the value is not an `Array`, the typecode is
/// unknown, or a view runs past its base. Used by
/// `str_data_pool` so the str methods read a bytearray `self`
/// (the vendor's shared str/bytes/bytearray method table calls
/// the same C functions on all three).
pub fn array_raw_bytes(arena: &GcArena, r: GcRef) -> Option<Vec<u8>> {
    let Obj::Array(a) = arena.get(r) else {
        return None;
    };
    let tc = a.typecode & TYPECODE_MASK;
    let sz = array_item_size(tc).ok()?;
    match a.source {
        None => a.data.get(..a.len.checked_mul(sz)?).map(|d| d.to_vec()),
        Some(src) => {
            let base = match arena.get(src) {
                Obj::Array(_) => array_raw_bytes(arena, src)?,
                Obj::Bytes(b) => b.bytes.clone(),
                Obj::Str(s) => s.bytes.clone(),
                _ => return None,
            };
            let off = a.free.checked_mul(sz)?;
            let end = off.checked_add(a.len.checked_mul(sz)?)?;
            base.get(off..end).map(|d| d.to_vec())
        }
    }
}

/// The readable bytes of a buffer-protocol value + its reported
/// (masked) typecode; `None` when the value has no buffer.
/// Resolves view chains from the arena (safe — the bytes are
/// CLONED, never read through a raw pointer):
/// - owned `Array` → `data[0..len * sz]`, masked typecode;
/// - `memoryview` → the source's bytes at `free * sz`;
/// - `Bytes` / `Str` → the bytes, `'B'` (the Zig's str/bytes
///   buffer slots report `'B'`).
/// `str` is included because the Zig's `mp_get_buffer`
/// succeeds on it (probed: `"a" in bytearray(b'abc')` is
/// `True`; `bytearray("ab", "utf-8")` works). This is the
/// safe-Rust half of the buffer protocol (see `objarray.rs`):
/// the buffer SLOT fills `BufferInfo` (a raw pointer is
/// stored, never dereferenced); every byte READ goes through
/// here.
pub fn buffer_bytes(ctx: &CallCtx, value: Value) -> Option<(Vec<u8>, u8)> {
    // String literals load as `Value::Qstr` (see
    // `str_get_buffer`); the C's `mp_get_buffer` succeeds on
    // them, so read the bytes from the pool (cloned — this
    // fn returns owned bytes).
    if let Value::Qstr(id) = value {
        return Some((ctx.pool.qstr_str(id as u16).to_vec(), b'B'));
    }
    let Value::Heap(r) = value else {
        return None;
    };
    match ctx.arena.get(r) {
        // E2i: a user instance of a native type (e.g.
        // `class X(array.array)`) stores the native object in
        // `subobj[0]` — the vendor's `instance_get_buffer`
        // (`objtype.c:925-940`) delegates the buffer slot to
        // exactly that sub-object, so `bytes(X(...))` reads the
        // array's bytes. A plain user instance has an empty
        // `subobj` and no buffer (the vendor's `return 1`).
        Obj::Instance(d) => match d.subobj.first() {
            Some(&sub) => buffer_bytes(ctx, sub),
            None => None,
        },
        Obj::Array(a) => {
            let tc = a.typecode & TYPECODE_MASK;
            let sz = array_item_size(tc).ok()?;
            match a.source {
                None => Some((a.data[..a.len * sz].to_vec(), tc)),
                Some(src) => {
                    let base = buffer_bytes(ctx, Value::Heap(src))?;
                    let off = a.free * sz;
                    let n = a.len * sz;
                    let end = off.checked_add(n)?;
                    if end > base.0.len() {
                        return None;
                    }
                    Some((base.0[off..end].to_vec(), tc))
                }
            }
        }
        Obj::Bytes(b) => Some((b.bytes.clone(), b'B')),
        Obj::Str(s) => Some((s.bytes.clone(), b'B')),
        _ => None,
    }
}

/// Resolve a value to the bytes it stores as one array
/// element: `(element size, little-endian bytes)`. This is
/// the value half of `binary_set_val_array`, split out so
/// callers whose destination lives IN the arena (array
/// append / element store) can resolve with `&mut ctx`
/// first and copy into the destination afterwards — the
/// two borrows cannot overlap (a `&mut [u8]` into the arena
/// conflicts with the `&mut CallCtx` the resolution needs
/// for `obj_get_int`). The three branches mirror
/// `binary_set_val_array` exactly: `'O'` stores the raw
/// word; an exact heap int writes its two's-complement
/// bytes (return ignored — truncated); anything else goes
/// through `get_int` + `set_val_array_from_int`.
pub fn binary_set_val_bytes(
    ctx: &mut CallCtx,
    typecode: u8,
    val_in: Value,
) -> Result<(usize, [u8; 8]), BinaryError> {
    let (size, _) = binary_get_size(b'@', typecode)?;
    let mut buf = [0u8; 8];
    if typecode == b'O' {
        buf.copy_from_slice(&value_word(val_in).to_le_bytes());
        return Ok((size, buf));
    }
    if matches!(val_in, Value::Heap(_))
        && crate::objects::obj::is_exact_int(ctx.arena, ctx.types, val_in)
    {
        if let Value::Heap(r) = val_in {
            if let Obj::Int(z) = ctx.arena.get(r) {
                let z = z.clone();
                let _ = z.as_bytes(false, z.is_neg(), &mut buf[..size]);
                return Ok((size, buf));
            }
        }
    }
    let v = obj_get_int(ctx, ctx.types, val_in)?;
    binary_set_val_array_from_int(typecode, &mut buf, 0, v);
    Ok((size, buf))
}
