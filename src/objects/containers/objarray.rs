//! `objarray.zig` — `array`, `bytearray`, `memoryview` (the
//! second half of the array lane; `objarray_methods.rs` holds
//! append/extend, the binary op, subscr, the buffer slot and
//! the iterator).
//!
//! Three types share one payload (`MpObjArray { base, bits,
//! len, items }` in the Zig; `ArrayData` here — RAW BYTES,
//! not `Value`s):
//! - `array.array`: `typecode` is the struct code (`'i'`,
//!   `'B'`, …); flags `0x0000`.
//! - `bytearray`: `typecode == BYTEARRAY_TYPECODE` (1);
//!   flags `EQ_CHECKS_OTHER_TYPE | SUBSCR_ALLOWS_STACK_SLICE`
//!   (`0x0408`).
//! - `memoryview`: `typecode` is the viewed code, OR-ed with
//!   the `0x80` RW flag when the viewed buffer is writable;
//!   `free` is the ELEMENT offset into the viewed object;
//!   `len` the element count; `data` empty; `source` the
//!   viewed object (an `Array` or `Bytes` — views-of-views
//!   flatten at construction). Flags `0x0408`.
//!
//! Buffer protocol, safe-Rust rule (see `binary.rs`): the
//! buffer slot fills `BufferInfo` (a raw pointer is stored,
//! never dereferenced); every byte read goes through
//! `buffer_bytes`, which resolves owned data / view chains
//! from the arena. `str` has a buffer in the Zig
//! (`mp_obj_str_get_buffer`), so `buffer_bytes` covers
//! `Obj::Str` too (probed: `"a" in bytearray(b'abc')` is
//! `True`; `bytearray("ab", "utf-8")` works).
//!
//! Probed divergences (the reference ABORTS, Rust raises an
//! intended error — the C8a crash-divergence convention):
//! - `bytearray(-1)` aborts (rc=134, `@intCast` of a negative
//!   length); Rust raises `ValueError("negative count")`.
//! - `memoryview([1, 2])` / `memoryview(123)` abort (rc=134,
//!   `bufinfo.typecode` read uninitialised after the failed
//!   `mp_get_buffer_raise`); Rust raises the buffer-protocol
//!   `TypeError` (`"object with buffer protocol required"`).
//! - `array('i') + array('i')` / `bytearray() +
//!   bytearray()` abort (rc=134, `res.items.?` on the NULL
//!   items pointer of an empty result); Rust returns the empty
//!   array (a `Vec` is never null).
//! - `sort(key=len)` on ints aborts the same way (see
//!   `objlist_methods.rs`); the key path is deferred here too.
//!
//! Deferred to C13/C14: the `key=` sort path (needs
//! `mp_call_function_1`), the shared locals tables (C8b
//! assembles `array_bytearray_str_bytes_locals_table`; this
//! lane provides the append/extend adapters), `bytes(mv)`
//! (the `bytes` constructor).

use crate::memory::gc::{CallCtx, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::containers::binary::{
    array_item_size, binary_get_val_array, buffer_bytes, BinaryError,
    BYTEARRAY_TYPECODE, TYPECODE_MASK,
};
use crate::objects::containers::objarray_methods::array_extend_impl;
use crate::objects::obj::{obj_len_maybe, ObjError};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, CallHook, PrintKind, UnaryOp};
use crate::objects::strings::objstr_a::StrError;
use crate::objects::strings::objstr_methods_b::str_print_quoted;
use crate::objects::value::{ArrayData, GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// Errors from the array operations (the C9a `DictError`
/// shape: `exc_name()` for the VM's pending-exception wiring,
/// `to_pyerror()` for the slot boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrayError {
    /// `ValueError("bad typecode")` — unknown typecode, or an
    /// empty typecode string (`array('')`).
    BadTypecode,
    /// `TypeError("argument num/types mismatch")` — the polled
    /// sig check, and the `kwMakeNew` failures.
    ArgNumTypesMismatch,
    /// `TypeError("wrong number of arguments")` —
    /// `bytearray("ab")` (a lone str needs an encoding).
    WrongNumArgs,
    /// `TypeError("can't convert to str implicitly")` —
    /// `array(1)` (the typecode argument is not a str).
    NotStr,
    /// `ValueError("negative count")` — `bytearray(-1)`; the
    /// reference ABORTS here, Rust raises (see module docs).
    NegativeCount,
    /// `NotImplementedError("only slices with step=1 (aka None)
    /// are supported")` — slice load/store with `step != 1`.
    SliceStep,
    /// `ValueError("lhs and rhs should be compatible")` — slice
    /// store with a mismatched item size, or a memoryview
    /// slice store that changes the length.
    SliceCompat,
    /// `NotImplementedError("array/bytes required on right
    /// side")` — slice store with a non-array/bytes rhs.
    SliceRhsRequired,
    /// `OverflowError("memoryview offset too large")` — slicing
    /// a memoryview past `2**56 - 1`.
    MemviewOffsetOverflow,
    /// `TypeError("object not iterable")` — construction /
    /// extension from a non-iterable.
    NotIterable,
    /// A `get_index` / buffer / conversion failure — the
    /// `ObjError` carries the type/message.
    Obj(ObjError),
    /// A slot raised (data dropped at the boundary — the
    /// accepted C6/C7r convention); also the bare
    /// `NotImplementedError` from the comparison arms and
    /// `CONTAINS` on an int/float rhs.
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl ArrayError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            ArrayError::BadTypecode => "ValueError",
            ArrayError::ArgNumTypesMismatch => "TypeError",
            ArrayError::WrongNumArgs => "TypeError",
            ArrayError::NotStr => "TypeError",
            ArrayError::NegativeCount => "ValueError",
            ArrayError::SliceStep => "NotImplementedError",
            ArrayError::SliceCompat => "ValueError",
            ArrayError::SliceRhsRequired => "NotImplementedError",
            ArrayError::MemviewOffsetOverflow => "OverflowError",
            ArrayError::NotIterable => "TypeError",
            ArrayError::Obj(e) => e.exc_type().map(|t| t.name()).unwrap_or(""),
            ArrayError::OutOfMemory => "MemoryError",
            ArrayError::PyRaise => "",
        }
    }

    /// The TERSE message literal (`None` for `OutOfMemory` /
    /// `PyRaise`).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            ArrayError::BadTypecode => Some("bad typecode"),
            ArrayError::ArgNumTypesMismatch => Some("argument num/types mismatch"),
            ArrayError::WrongNumArgs => Some("wrong number of arguments"),
            ArrayError::NotStr => Some("can't convert to str implicitly"),
            ArrayError::NegativeCount => Some("negative count"),
            ArrayError::SliceStep => {
                Some("only slices with step=1 (aka None) are supported")
            }
            ArrayError::SliceCompat => Some("lhs and rhs should be compatible"),
            ArrayError::SliceRhsRequired => Some("array/bytes required on right side"),
            ArrayError::MemviewOffsetOverflow => Some("memoryview offset too large"),
            ArrayError::NotIterable => Some("object not iterable"),
            ArrayError::Obj(e) => e.message(),
            ArrayError::OutOfMemory | ArrayError::PyRaise => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ArrayError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for ArrayError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.message().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.clone().to_pyerror()
    }
}

impl From<ObjError> for ArrayError {
    fn from(e: ObjError) -> ArrayError {
        match e {
            ObjError::OutOfMemory => ArrayError::OutOfMemory,
            other => ArrayError::Obj(other),
        }
    }
}

impl From<BinaryError> for ArrayError {
    fn from(e: BinaryError) -> ArrayError {
        match e {
            BinaryError::BadTypecode => ArrayError::BadTypecode,
            BinaryError::IntConversion => ArrayError::Obj(ObjError::IntConversion),
            BinaryError::Overflow => ArrayError::Obj(ObjError::Overflow),
            BinaryError::OutOfMemory => ArrayError::OutOfMemory,
            BinaryError::PyRaise => ArrayError::PyRaise,
        }
    }
}

impl From<StrError> for ArrayError {
    fn from(e: StrError) -> ArrayError {
        match e {
            StrError::ArgNumTypesMismatch => ArrayError::ArgNumTypesMismatch,
            StrError::Obj(ObjError::OutOfMemory) => ArrayError::OutOfMemory,
            StrError::Obj(o) => ArrayError::Obj(o),
            _ => ArrayError::PyRaise,
        }
    }
}

/// `memview_offset_max` (`objarray.zig:56`): `2**56 - 1`.
pub const MEMVIEW_OFFSET_MAX: usize = (1usize << 56) - 1;

/// The masked typecode of an array-family object (the `0x80`
/// RW flag stripped); `None` for anything else.
pub fn array_typecode(ctx: &CallCtx, value: Value) -> Option<u8> {
    match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Array(a) => Some(a.typecode & TYPECODE_MASK),
            _ => None,
        },
        _ => None,
    }
}

/// The `MpObjArray` payload of an array-family value.
pub fn array_data<'a>(ctx: &'a CallCtx, value: Value) -> Option<&'a ArrayData> {
    match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Array(a) => Some(a),
            _ => None,
        },
        _ => None,
    }
}

/// Is `value` a memoryview (its header type is `mv_type`)?
pub fn is_memoryview(ctx: &CallCtx, mv_type: GcRef, value: Value) -> bool {
    match value {
        Value::Heap(r) => ctx.arena.header(r).type_ == Some(mv_type),
        _ => false,
    }
}

/// Is `value` an exact array or bytearray (header type is
/// `array_type` or `bytearray_type`)? No subclasses exist yet,
/// so exact is the whole check.
pub fn is_array_or_bytearray(
    ctx: &CallCtx,
    array_type: GcRef,
    bytearray_type: GcRef,
    value: Value,
) -> bool {
    match value {
        Value::Heap(r) => {
            let t = ctx.arena.header(r).type_;
            t == Some(array_type) || t == Some(bytearray_type)
        }
        _ => false,
    }
}

/// `mp_obj_is_int` (C7: bool is NOT an int — `SmallInt` or a
/// heap `Obj::Int` only).
pub fn is_int_value(ctx: &CallCtx, value: Value) -> bool {
    match value {
        Value::SmallInt(_) => true,
        Value::Heap(r) => matches!(ctx.arena.get(r), Obj::Int(_)),
        _ => false,
    }
}

/// `mp_obj_is_str`: exact str type (no subclasses yet).
pub fn is_str_value(ctx: &CallCtx, value: Value) -> bool {
    match value {
        Value::Heap(r) => ctx.arena.header(r).type_ == Some(ctx.types.str_),
        _ => false,
    }
}

/// The readable bytes of a buffer-protocol value — see
/// `binary::buffer_bytes` (the implementation lives there so
/// `collect_iterable` can use it without a module cycle).

/// The typecode comparison mapping (`typecodeForComparison`,
/// `objarray.zig:348-356`): bytearray (1) → `'B'`; a code
/// `<= 'Z'` is shifted to lowercase and marked unsigned.
/// Returns `(code, is_unsigned)`. (The Zig writes both calls'
/// results into ONE shared bool — unobservable: when the
/// codes differ the slot raises anyway; when they match, the
/// final value is correct.)
pub fn typecode_for_comparison(typecode_in: u8) -> (u8, bool) {
    let mut tc = typecode_in;
    if tc == BYTEARRAY_TYPECODE {
        tc = b'B';
    }
    if tc <= b'Z' {
        (tc + 32, true)
    } else {
        (tc, false)
    }
}

/// `arrayNew` (`objarray.zig:162-171`): a fresh array with
/// `n` zero-filled elements. The header type is the bytearray
/// type iff `typecode == BYTEARRAY_TYPECODE`, else the array
/// type. (`free` is kept 0 — `Vec` manages capacity; the
/// spare reserve is unobservable.)
pub fn array_new(
    ctx: &mut CallCtx,
    array_type: GcRef,
    bytearray_type: GcRef,
    typecode: u8,
    n: usize,
) -> Result<GcRef, ArrayError> {
    let sz = array_item_size(typecode)?;
    let header = if typecode == BYTEARRAY_TYPECODE {
        bytearray_type
    } else {
        array_type
    };
    Ok(ctx.alloc(
        Some(header),
        Obj::Array(ArrayData {
            typecode,
            free: 0,
            len: n,
            data: vec![0u8; n * sz],
            source: None,
        }),
    ))
}

/// `arrayConstruct` (`objarray.zig:187-218`): bytearrays can
/// be raw-initialised from anything with the buffer protocol;
/// other arrays only from exact bytes/bytearray. The memcpy
/// path copies `bufinfo.len / sz` elements (truncating a
/// short buffer: `array('i', b"\\x01\\x02")` is empty);
/// otherwise the length comes from `obj_len_maybe` (missing
/// → 0) and the items are filled by `arrayExtendImpl`.
/// A failure inside the extend (e.g. `array('i', [1.5])` →
/// "can't convert to int") aborts the construction — the
/// half-filled array is discarded, so stopping at the first
/// error is unobservable.
pub fn array_construct(
    ctx: &mut CallCtx,
    array_type: GcRef,
    bytearray_type: GcRef,
    typecode: u8,
    initializer: Value,
    call: Option<CallHook<'_, '_>>,
) -> Result<Value, ArrayError> {
    let init_is_bytes_like = typecode == BYTEARRAY_TYPECODE
        || matches!(initializer, Value::Heap(r) if ctx.arena.header(r).type_ == Some(ctx.types.bytes))
        || matches!(initializer, Value::Heap(r) if ctx.arena.header(r).type_ == Some(bytearray_type));
    if init_is_bytes_like {
        if let Some((bytes, _)) = buffer_bytes(ctx, initializer) {
            let sz = array_item_size(typecode)?;
            let len = bytes.len() / sz;
            let r = array_new(ctx, array_type, bytearray_type, typecode, len)?;
            if len * sz > 0 {
                if let Obj::Array(a) = ctx.arena.get_mut(r) {
                    a.data[..len * sz].copy_from_slice(&bytes[..len * sz]);
                }
            }
            return Ok(Value::Heap(r));
        }
    }
    let mut len = 0usize;
    if let Some(lv) = obj_len_maybe(ctx, ctx.types, initializer) {
        if let Value::SmallInt(n) = lv {
            len = n.max(0) as usize;
        }
    }
    let r = array_new(ctx, array_type, bytearray_type, typecode, len)?;
    let arr = Value::Heap(r);
    array_extend_impl(ctx, arr, initializer, len, call)?;
    Ok(arr)
}

/// `npy_array_unary_op` (`objarray.zig:340-346`): `BOOL`
/// (`len != 0`), `LEN`, else NULL.
pub fn array_unary_op(
    ctx: &mut CallCtx,
    op: UnaryOp,
    self_: Value,
) -> PyResult<Value> {
    let len = match array_data(ctx, self_) {
        Some(a) => a.len,
        None => return Err(PyError::PyRaise),
    };
    match op {
        UnaryOp::Bool => Ok(Value::Bool(len != 0)),
        UnaryOp::Len => Ok(Value::SmallInt(len as i64)),
        _ => Err(PyError::PyRaise),
    }
}

/// `mp_binary_get_val_array` for one element of an array
/// value (the element read used by the iterator and the
/// print path; `binary.rs` owns the per-typecode read).
pub fn array_get_element(ctx: &mut CallCtx, self_: Value, index: usize) -> Value {
    let Some(a) = array_data(ctx, self_) else {
        return Value::None;
    };
    let tc = a.typecode & TYPECODE_MASK;
    let bytes = match buffer_bytes(ctx, self_) {
        Some((b, _)) => b,
        None => return Value::None,
    };
    binary_get_val_array(ctx, tc, &bytes, index)
}

/// The `mp_obj_new_memoryview` / `mp_obj_new_bytearray` /
/// `mp_obj_new_bytearray_by_ref` public ABI
/// (`objarray.zig:219-246`): fresh objects. `by_ref` cannot
/// alias in safe Rust — it copies (the Zig's callers pass
/// freshly-allocated buffers, so the aliasing is
/// unobservable).
pub fn new_memoryview(
    ctx: &mut CallCtx,
    memoryview_type: GcRef,
    typecode: u8,
    nitems: usize,
    items: &[u8],
) -> GcRef {
    ctx.alloc(
        Some(memoryview_type),
        Obj::Array(ArrayData {
            typecode,
            free: 0,
            len: nitems,
            data: items.to_vec(),
            source: None,
        }),
    )
}

pub fn new_bytearray(ctx: &mut CallCtx, bytearray_type: GcRef, items: &[u8]) -> GcRef {
    ctx.alloc(
        Some(bytearray_type),
        Obj::Array(ArrayData {
            typecode: BYTEARRAY_TYPECODE,
            free: 0,
            len: items.len(),
            data: items.to_vec(),
            source: None,
        }),
    )
}

pub fn new_bytearray_by_ref(
    ctx: &mut CallCtx,
    bytearray_type: GcRef,
    items: &[u8],
) -> GcRef {
    new_bytearray(ctx, bytearray_type, items)
}

/// `npy_array_print` (`objarray.zig:248-268`): bytearray →
/// `bytearray(b'...')` (quoted); array → `array('c'` +
/// `, [items]` when non-empty + `)`; kind ignored. Elements
/// print via `PRINT_REPR` (the `'O'` elements are objects).
pub fn array_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let Some(a) = array_data(ctx, value) else {
        return;
    };
    let tc = a.typecode & TYPECODE_MASK;
    if tc == BYTEARRAY_TYPECODE {
        let _ = sink.write_str("bytearray(b");
        let bytes = buffer_bytes(ctx, value).map(|(b, _)| b).unwrap_or_default();
        str_print_quoted(sink, &bytes, true);
        let _ = sink.write_str(")");
        return;
    }
    let _ = sink.write_str("array('");
    let _ = sink.write_char(tc as char);
    let _ = sink.write_char('\'');
    if a.len > 0 {
        let _ = sink.write_str(", [");
        for i in 0..a.len {
            if i > 0 {
                let _ = sink.write_str(", ");
            }
            let el = array_get_element(ctx, value, i);
            crate::objects::obj_ops::obj_print_helper(
                ctx,
                ctx.types,
                el,
                PrintKind::Repr,
                sink,
            );
        }
        let _ = sink.write_str("]");
    }
    let _ = sink.write_str(")");
}

fn array_type_data_inner(
    name: &[u8],
    flags: u16,
    dict: GcRef,
    print: bool,
    make_new: crate::objects::slots::MakeNewFn,
) -> TypeData {
    TypeData {
        name: static_qstr_id(name).expect("array type name is a static qstr"),
        flags,
        base: None,
        mro: Vec::new(),
        dict,
        unary_op: Some(
            |ctx: &mut CallCtx, op: UnaryOp, self_: Value| array_unary_op(ctx, op, self_),
        ),
        binary_op: Some(
            |ctx: &mut CallCtx, op: BinaryOp, lhs: Value, rhs: Value| {
                crate::objects::containers::objarray_methods::array_binary_op(ctx, op, lhs, rhs)
            },
        ),
        call: None,
        getiter: Some(
            |ctx: &mut CallCtx, self_: Value| {
                crate::objects::containers::objarray_methods::array_getiter(ctx, self_)
            },
        ),
        subscr: Some(
            |ctx: &mut CallCtx, self_: Value, index: Value, arg: crate::objects::slots::SubscrArg| {
                crate::objects::containers::objarray_methods::array_subscr(ctx, self_, index, arg)
            },
        ),
        buffer: Some(
            |ctx: &mut CallCtx, self_: Value, bufinfo: &mut crate::objects::slots::BufferInfo, flags: u32| {
                crate::objects::containers::objarray_methods::array_get_buffer(
                    ctx, self_, bufinfo, flags,
                )
            },
        ),
        print: if print {
            Some(
                |ctx: &mut CallCtx, value: Value, kind: PrintKind, sink: &mut dyn crate::objects::slots::ByteSink| {
                    array_print(ctx, value, kind, sink)
                },
            )
        } else {
            None
        },
        attr: None,
        make_new: Some(make_new),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
    }
}

/// `mp_type_array` (`objarray.zig:765-795`): flags `0x0000`
/// (`FLAG_ITER_IS_GETITER` is a no-op `0x0000`); slots
/// make_new/print/unary/binary/subscr/getiter/buffer/
/// locals_dict (the locals dict is C12's — this lane
/// provides the append/extend adapters).
pub fn array_type_data(dict: GcRef) -> TypeData {
    array_type_data_inner(
        b"array",
        0,
        dict,
        true,
        crate::objects::objtype::make_new::array_make_new_slot,
    )
}

/// `mp_type_bytearray` (`objarray.zig:798-830`): flags
/// `EQ_CHECKS_OTHER_TYPE | SUBSCR_ALLOWS_STACK_SLICE`
/// (`0x0408`); same slots as array.
pub fn bytearray_type_data(dict: GcRef) -> TypeData {
    array_type_data_inner(
        b"bytearray",
        crate::objects::obj::FLAG_EQ_CHECKS_OTHER_TYPE
            | crate::objects::obj::FLAG_SUBSCR_ALLOWS_STACK_SLICE,
        dict,
        true,
        crate::objects::objtype::make_new::bytearray_make_new_slot,
    )
}

/// `mp_type_memoryview` (`objarray.zig:833-867`): flags
/// `0x0408`; NO print slot; slots make_new/unary/binary/
/// subscr/getiter/buffer/locals_dict.
pub fn memoryview_type_data(dict: GcRef) -> TypeData {
    array_type_data_inner(
        b"memoryview",
        crate::objects::obj::FLAG_EQ_CHECKS_OTHER_TYPE
            | crate::objects::obj::FLAG_SUBSCR_ALLOWS_STACK_SLICE,
        dict,
        false,
        crate::objects::objtype::make_new::memoryview_make_new_slot,
    )
}
