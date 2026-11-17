//! Tests for `obj.rs` (Lane C6): id/type, int/float/complex
//! extraction, len, truth, callable, error metadata. The fixture +
//! slot stubs are the shared harness for `obj_ops_tests.rs` too.
//!
//! The fixture builds stub builtin type objects by hand (C12 owns the
//! real type ROM): each type is an `Obj::Type` whose header type is the
//! `type` type, with the slot under test filled in. Payloads need not
//! match their header type in tests — the arena does not enforce it —
//! so a `Float` payload can carry a slotted test type's header.
//!
//! Slots that must allocate a typed heap object (the `__float__` /
//! `__complex__` stubs) read the builtin type refs from a
//! thread-local the fixture installs; slots are plain `fn` pointers
//! and cannot capture anything.

use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::obj::*;
use crate::objects::slots::*;
use crate::objects::value::*;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;
use crate::vendor::mpz::BigInt;

thread_local! {
    static TEST_TYPES: std::cell::Cell<Option<BuiltinTypes>> =
        const { std::cell::Cell::new(None) };
}

pub(crate) fn qstr_id(s: &[u8]) -> u32 {
    crate::qstr_table::QSTR_STRINGS
        .iter()
        .position(|x| *x == s)
        .unwrap_or_else(|| panic!("static qstr not found: {:?}", String::from_utf8_lossy(s)))
        as u32
}

fn plain_type_data(name: u32, flags: u16, dict: GcRef) -> TypeData {
    TypeData {
        name,
        flags,
        base: None,
        mro: vec![],
        dict,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    }
}

/// A call context borrowing the arena + roots + types fields only, so
/// `&fx.types` stays usable alongside it (a `Fixture::ctx` method
/// would tie the returned lifetime to the whole `&mut self`).
pub(crate) fn ctx<'a>(
    arena: &'a mut GcArena,
    roots: &'a mut RootSet,
    types: &'a BuiltinTypes,
    pool: &'a RuntimeQstr,
    pending_raise: &'a mut Option<PendingRaise>,
) -> CallCtx<'a> {
    CallCtx::new(arena, roots, types, pool, pending_raise)
}

pub(crate) struct Fixture {
    pub(crate) arena: GcArena,
    pub(crate) roots: RootSet,
    pub(crate) types: BuiltinTypes,
    pub(crate) pool: RuntimeQstr,
    pub(crate) pending_raise: Option<PendingRaise>,
    dict: GcRef,
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        let mut arena = GcArena::new();
        let roots = RootSet::new();
        let dict = arena.alloc(
            None,
            Obj::Dict(DictData {
                map: MapData::new(),
            }),
            &roots,
        );
        // The `type` type first (header type None — bootstrapping).
        let type_ = arena.alloc(
            None,
            Obj::Type(plain_type_data(qstr_id(b"type"), 0, dict)),
            &roots,
        );
        let mk = |arena: &mut GcArena, name: &[u8]| {
            arena.alloc(
                Some(type_),
                Obj::Type(plain_type_data(qstr_id(name), 0, dict)),
                &roots,
            )
        };
        // C12: plain object/super/staticmethod/classmethod/
        // boundmeth types and the ROM `()` singleton.
        let object = mk(&mut arena, b"object");
        let super_ = mk(&mut arena, b"super");
        let staticmethod = mk(&mut arena, b"staticmethod");
        let classmethod = mk(&mut arena, b"classmethod");
        let boundmeth = mk(&mut arena, b"bound_method");
        let property = mk(&mut arena, b"property");
        let tuple = mk(&mut arena, b"tuple");
        let bytes = mk(&mut arena, b"bytes");
        let empty_tuple = arena.alloc(Some(tuple), Obj::Tuple(vec![]), &roots);
        let empty_bytes = arena.alloc(
            Some(bytes),
            Obj::Bytes(BytesData { bytes: Vec::new() }),
            &roots,
        );
        let native_init_wrapper = mk(&mut arena, b"function");
        let types = BuiltinTypes {
            int: mk(&mut arena, b"int"),
            str_: mk(&mut arena, b"str"),
            bytes,
            bool_: mk(&mut arena, b"bool"),
            none_type: mk(&mut arena, b"NoneType"),
            float: mk(&mut arena, b"float"),
            complex: mk(&mut arena, b"complex"),
            tuple,
            list: mk(&mut arena, b"list"),
            set: mk(&mut arena, b"set"),
            frozenset: mk(&mut arena, b"frozenset"),
            range: mk(&mut arena, b"range"),
            dict: mk(&mut arena, b"dict"),
            array: mk(&mut arena, b"array"),
            bytearray: mk(&mut arena, b"bytearray"),
            type_,
            object,
            super_,
            staticmethod,
            classmethod,
            boundmeth,
            property,
            empty_tuple,
            empty_bytes,
            native_init_wrapper,
        };
        TEST_TYPES.with(|c| c.set(Some(types)));
        Fixture {
            arena,
            roots,
            types,
            pool: RuntimeQstr::new(),
            pending_raise: None,
            dict,
        }
    }

    /// A fresh type object with the given slots (name = a static qstr).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn slotted_type(
        &mut self,
        name: &str,
        flags: u16,
        unary: Option<UnaryOpFn>,
        binary: Option<BinaryOpFn>,
        subscr: Option<SubscrFn>,
        buffer: Option<BufferFn>,
        print: Option<PrintFn>,
        call: Option<CallFn>,
    ) -> GcRef {
        let mut d = plain_type_data(qstr_id(name.as_bytes()), flags, self.dict);
        d.unary_op = unary;
        d.binary_op = binary;
        d.subscr = subscr;
        d.buffer = buffer;
        d.print = print;
        d.call = call;
        self.arena.alloc(Some(self.types.type_), Obj::Type(d), &self.roots)
    }

    pub(crate) fn value_of(&mut self, type_: GcRef, obj: Obj) -> Value {
        Value::Heap(self.arena.alloc(Some(type_), obj, &self.roots))
    }

    pub(crate) fn bigint(&mut self, z: BigInt) -> Value {
        self.value_of(self.types.int, Obj::Int(z))
    }
}

// ── slot stubs ────────────────────────────────────────────────────────

pub(crate) fn len3_slot(_ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::Len => Ok(Value::SmallInt(3)),
        _ => Err(PyError::PyRaise),
    }
}

fn len0_slot(_ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::Len => Ok(Value::SmallInt(0)),
        _ => Err(PyError::PyRaise),
    }
}

fn bool_false_slot(_ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::Bool => Ok(Value::Bool(false)),
        _ => Err(PyError::PyRaise),
    }
}

fn int42_slot(_ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::IntMaybe => Ok(Value::SmallInt(42)),
        _ => Err(PyError::PyRaise),
    }
}

fn float_slot(ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::FloatMaybe => {
            let t = TEST_TYPES.with(|c| c.get().unwrap().float);
            Ok(Value::Heap(ctx.alloc(Some(t), Obj::Float(2.5))))
        }
        _ => Err(PyError::PyRaise),
    }
}

fn complex_slot(ctx: &mut CallCtx, op: UnaryOp, _arg: Value) -> PyResult<Value> {
    match op {
        UnaryOp::ComplexMaybe => {
            let t = TEST_TYPES.with(|c| c.get().unwrap().complex);
            Ok(Value::Heap(ctx.alloc(
                Some(t),
                Obj::Complex { re: 1.0, im: 2.0 },
            )))
        }
        _ => Err(PyError::PyRaise),
    }
}

/// Equal → true, NotEqual → false (for the `__ne__`-first path).
pub(crate) fn eq_true_slot(_ctx: &mut CallCtx, op: BinaryOp, _l: Value, _r: Value) -> PyResult<Value> {
    match op {
        BinaryOp::Equal => Ok(Value::Bool(true)),
        BinaryOp::NotEqual => Ok(Value::Bool(false)),
        _ => Err(PyError::PyRaise),
    }
}

/// Equal → false (distinguishes the reflexive shortcut from the slot).
pub(crate) fn eq_false_slot(_ctx: &mut CallCtx, op: BinaryOp, _l: Value, _r: Value) -> PyResult<Value> {
    match op {
        BinaryOp::Equal => Ok(Value::Bool(false)),
        _ => Err(PyError::PyRaise),
    }
}

pub(crate) fn subscr_echo_slot(
    _ctx: &mut CallCtx,
    _base: Value,
    index: Value,
    arg: SubscrArg,
) -> PyResult<Value> {
    match arg {
        SubscrArg::Get => Ok(index),
        _ => Err(PyError::PyRaise),
    }
}

pub(crate) static BUF_BYTES: &[u8] = b"abc";

pub(crate) fn buffer_ok_slot(
    _ctx: &mut CallCtx,
    _obj: Value,
    info: &mut BufferInfo,
    flags: u32,
) -> PyResult<bool> {
    // get_buffer strips everything but MP_BUFFER_RW before the call.
    assert_eq!(flags, BUFFER_READ);
    info.data = BUF_BYTES.as_ptr();
    info.len = BUF_BYTES.len();
    info.typecode = 0;
    Ok(true)
}

pub(crate) fn print_marker_slot(
    _ctx: &mut CallCtx,
    _v: Value,
    _k: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    let _ = sink.write_str("PRINTED");
}

fn dummy_call(
    _ctx: &mut CallCtx,
    _f: Value,
    _n_args: usize,
    _n_kw: usize,
    _args: &[Value],
) -> PyResult<Value> {
    Ok(Value::None)
}

// ── id / type ─────────────────────────────────────────────────────────

#[test]
fn id_immediates_match_reference_binary() {
    // Verified on `petitpy`: id(5)=11, id(True)=30, id(False)=14,
    // id(None)=6, id(0)=1, id(-1)=-1.
    assert_eq!(obj_id(Value::SmallInt(5)), Value::SmallInt(11));
    assert_eq!(obj_id(Value::Bool(true)), Value::SmallInt(30));
    assert_eq!(obj_id(Value::Bool(false)), Value::SmallInt(14));
    assert_eq!(obj_id(Value::None), Value::SmallInt(6));
    assert_eq!(obj_id(Value::SmallInt(0)), Value::SmallInt(1));
    assert_eq!(obj_id(Value::SmallInt(-1)), Value::SmallInt(-1));
    assert_eq!(obj_id(Value::Qstr(0)), Value::SmallInt(2));
}

#[test]
fn id_heap_is_slot_index() {
    let mut fx = Fixture::new();
    let v = fx.value_of(fx.types.float, Obj::Float(1.5));
    let r = v.as_heap().unwrap();
    assert_eq!(obj_id(v), Value::SmallInt(r.0 as i64));
}

#[test]
fn type_of_immediates() {
    let fx = Fixture::new();
    assert_eq!(obj_get_type(&fx.arena, &fx.types, Value::SmallInt(7)), Some(fx.types.int));
    assert_eq!(obj_get_type(&fx.arena, &fx.types, Value::Bool(true)), Some(fx.types.bool_));
    assert_eq!(obj_get_type(&fx.arena, &fx.types, Value::None), Some(fx.types.none_type));
    assert_eq!(
        obj_get_type(&fx.arena, &fx.types, Value::Qstr(qstr_id(b"str"))),
        Some(fx.types.str_)
    );
}

#[test]
fn type_of_heap_uses_header() {
    let mut fx = Fixture::new();
    let v = fx.value_of(fx.types.float, Obj::Float(1.5));
    assert_eq!(obj_get_type(&fx.arena, &fx.types, v), Some(fx.types.float));
}

#[test]
fn is_str_bytes_float() {
    let mut fx = Fixture::new();
    let s = fx.value_of(
        fx.types.str_,
        Obj::Str(StrData {
            bytes: b"hi".to_vec(),
        }),
    );
    let b = fx.value_of(
        fx.types.bytes,
        Obj::Bytes(BytesData {
            bytes: b"hi".to_vec(),
        }),
    );
    let f = fx.value_of(fx.types.float, Obj::Float(1.5));
    assert!(is_str(&fx.arena, &fx.types, Value::Qstr(qstr_id(b"str"))));
    assert!(is_str(&fx.arena, &fx.types, s));
    assert!(!is_str(&fx.arena, &fx.types, b));
    assert!(is_bytes(&fx.arena, &fx.types, b));
    assert!(!is_bytes(&fx.arena, &fx.types, s));
    assert!(is_float(&fx.arena, &fx.types, f));
    assert!(!is_float(&fx.arena, &fx.types, s));
}

// ── int extraction ────────────────────────────────────────────────────

#[test]
fn get_int_small_bool_and_slot() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("int", 0, Some(int42_slot), None, None, None, None, None);
    let v = fx.value_of(t, Obj::Float(9.9));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_get_int_maybe(&mut ctx, &fx.types, Value::SmallInt(7)).unwrap(), Some(7));
    assert_eq!(obj_get_int_maybe(&mut ctx, &fx.types, Value::Bool(true)).unwrap(), Some(1));
    assert_eq!(obj_get_int_maybe(&mut ctx, &fx.types, Value::Bool(false)).unwrap(), Some(0));
    assert_eq!(obj_get_int_maybe(&mut ctx, &fx.types, v).unwrap(), Some(42));
    assert_eq!(obj_get_int(&mut ctx, &fx.types, v).unwrap(), 42);
}

#[test]
fn get_int_no_slot_is_conversion_error() {
    let mut fx = Fixture::new();
    let f = fx.value_of(fx.types.float, Obj::Float(1.5));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_get_int_maybe(&mut ctx, &fx.types, f).unwrap(), None);
    let e = obj_get_int(&mut ctx, &fx.types, f).unwrap_err();
    assert_eq!(e, ObjError::IntConversion);
    assert_eq!(e.exc_type(), Some(ExcType::TypeError));
    assert_eq!(e.message(), Some("can't convert to int"));
}

#[test]
fn get_int_bignum_window_and_overflow() {
    let mut fx = Fixture::new();
    // 2^62 fits the small-int window (C0 fix note).
    let big62 = fx.bigint(BigInt::from_int(1 << 62));
    // 2^70 overflows the machine word; -2^63 is rejected by the C
    // overflow check even though it fits in i64.
    let big70 = fx.bigint(BigInt::from_bytes(true, &[0x40, 0, 0, 0, 0, 0, 0, 0, 0]));
    let big_neg = fx.bigint(BigInt::from_int(i64::MIN));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_get_int(&mut ctx, &fx.types, big62).unwrap(), 1 << 62);
    assert_eq!(obj_get_int(&mut ctx, &fx.types, big70).unwrap_err(), ObjError::Overflow);
    assert_eq!(obj_get_int(&mut ctx, &fx.types, big_neg).unwrap_err(), ObjError::Overflow);
    let e = ObjError::Overflow;
    assert_eq!(e.exc_type(), Some(ExcType::OverflowError));
    assert_eq!(e.message(), Some("overflow converting long int to machine word"));
}

#[test]
fn get_int_truncated_and_uint_and_ll() {
    let mut fx = Fixture::new();
    let big70 = fx.bigint(BigInt::from_bytes(true, &[0x40, 0, 0, 0, 0, 0, 0, 0, 0]));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    // Truncation is `mpz_hash`: the low 64 bits of 2^70 are 0.
    assert_eq!(obj_get_int_truncated(&mut ctx, &fx.types, big70).unwrap(), 0);
    assert_eq!(obj_get_int_truncated(&mut ctx, &fx.types, Value::SmallInt(9)).unwrap(), 9);
    // uint: small negative and bignum overflow.
    assert_eq!(obj_get_uint(&mut ctx, &fx.types, Value::SmallInt(5)).unwrap(), 5);
    assert_eq!(obj_get_uint(&mut ctx, &fx.types, Value::SmallInt(-1)).unwrap_err(), ObjError::Overflow);
    assert_eq!(obj_get_uint(&mut ctx, &fx.types, big70).unwrap_err(), ObjError::Overflow);
    // ll: the return of the 8-byte conversion is ignored — 2^70 wraps to 0.
    assert_eq!(obj_get_ll(&mut ctx, &fx.types, big70).unwrap(), 0);
    assert_eq!(obj_get_ll(&mut ctx, &fx.types, Value::SmallInt(-3)).unwrap(), -3);
}

// ── float / complex extraction ────────────────────────────────────────

#[test]
fn get_float_small_bool_bignum() {
    let mut fx = Fixture::new();
    let big = fx.bigint(BigInt::from_int(5));
    let f = fx.value_of(fx.types.float, Obj::Float(1.5));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_get_float_maybe(&mut ctx, &fx.types, Value::SmallInt(3)), Some(3.0));
    assert_eq!(obj_get_float_maybe(&mut ctx, &fx.types, Value::Bool(true)), Some(1.0));
    assert_eq!(obj_get_float_maybe(&mut ctx, &fx.types, big), Some(5.0));
    assert_eq!(obj_get_float_maybe(&mut ctx, &fx.types, f), Some(1.5));
}

#[test]
fn get_float_via_slot_and_error() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("float", 0, Some(float_slot), None, None, None, None, None);
    let v = fx.value_of(t, Obj::Int(BigInt::from_int(1)));
    let s = fx.value_of(
        fx.types.str_,
        Obj::Str(StrData {
            bytes: b"x".to_vec(),
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_get_float(&mut ctx, &fx.types, v).unwrap(), 2.5);
    let e = obj_get_float(&mut ctx, &fx.types, s).unwrap_err();
    assert_eq!(e, ObjError::FloatConversion);
    assert_eq!(e.message(), Some("can't convert to float"));
}

#[test]
fn get_complex_from_float_exact_and_slot() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("complex", 0, Some(complex_slot), None, None, None, None, None);
    let v = fx.value_of(t, Obj::Int(BigInt::from_int(1)));
    let c = fx.value_of(
        fx.types.complex,
        Obj::Complex { re: 3.0, im: 4.0 },
    );
    let s = fx.value_of(
        fx.types.str_,
        Obj::Str(StrData {
            bytes: b"x".to_vec(),
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    // Any float-convertible value is a complex with imag 0.
    assert_eq!(obj_get_complex(&mut ctx, &fx.types, Value::SmallInt(2)).unwrap(), (2.0, 0.0));
    assert_eq!(obj_get_complex(&mut ctx, &fx.types, c).unwrap(), (3.0, 4.0));
    assert_eq!(obj_get_complex(&mut ctx, &fx.types, v).unwrap(), (1.0, 2.0));
    let e = obj_get_complex(&mut ctx, &fx.types, s).unwrap_err();
    assert_eq!(e, ObjError::ComplexConversion);
    assert_eq!(e.message(), Some("can't convert to complex"));
}

// ── len / truth / callable ────────────────────────────────────────────

#[test]
fn len_bytes_fast_path_and_no_len() {
    let mut fx = Fixture::new();
    let b = fx.value_of(
        fx.types.bytes,
        Obj::Bytes(BytesData {
            bytes: b"abc".to_vec(),
        }),
    );
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_len(&mut ctx, &fx.types, b).unwrap(), Value::SmallInt(3));
    let e = obj_len(&mut ctx, &fx.types, Value::SmallInt(1)).unwrap_err();
    assert_eq!(e, ObjError::NoLen);
    assert_eq!(e.message(), Some("object has no len"));
}

#[test]
fn len_via_slot() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("float", 0, Some(len3_slot), None, None, None, None, None);
    let v = fx.value_of(t, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert_eq!(obj_len(&mut ctx, &fx.types, v).unwrap(), Value::SmallInt(3));
}

#[test]
fn is_true_immediates_and_slots() {
    let mut fx = Fixture::new();
    let tb = fx.slotted_type("float", 0, Some(bool_false_slot), None, None, None, None, None);
    let vb = fx.value_of(tb, Obj::Float(1.0));
    let tl = fx.slotted_type("float", 0, Some(len0_slot), None, None, None, None, None);
    let vl = fx.value_of(tl, Obj::Float(1.0));
    let tl3 = fx.slotted_type("float", 0, Some(len3_slot), None, None, None, None, None);
    let vl3 = fx.value_of(tl3, Obj::Float(1.0));
    let mut ctx = ctx(&mut fx.arena, &mut fx.roots, &fx.types, &fx.pool, &mut fx.pending_raise);
    assert!(obj_is_true(&mut ctx, &fx.types, Value::Bool(true)).unwrap());
    assert!(!obj_is_true(&mut ctx, &fx.types, Value::Bool(false)).unwrap());
    assert!(!obj_is_true(&mut ctx, &fx.types, Value::None).unwrap());
    assert!(!obj_is_true(&mut ctx, &fx.types, Value::SmallInt(0)).unwrap());
    assert!(obj_is_true(&mut ctx, &fx.types, Value::SmallInt(-1)).unwrap());
    assert!(!obj_is_true(&mut ctx, &fx.types, vb).unwrap());
    assert!(!obj_is_true(&mut ctx, &fx.types, vl).unwrap());
    assert!(obj_is_true(&mut ctx, &fx.types, vl3).unwrap());
}

#[test]
fn is_callable_needs_call_slot() {
    let mut fx = Fixture::new();
    let t = fx.slotted_type("float", 0, None, None, None, None, None, Some(dummy_call));
    let v = fx.value_of(t, Obj::Float(1.0));
    let f = fx.value_of(fx.types.float, Obj::Float(1.0));
    assert!(obj_is_callable(&fx.arena, &fx.types, v));
    assert!(!obj_is_callable(&fx.arena, &fx.types, f));
    assert!(!obj_is_callable(&fx.arena, &fx.types, Value::SmallInt(1)));
}

#[test]
fn obj_error_metadata() {
    assert_eq!(ObjError::IntConversion.exc_type(), Some(ExcType::TypeError));
    assert_eq!(ObjError::WrongLength.exc_type(), Some(ExcType::ValueError));
    assert_eq!(ObjError::IndexOutOfRange.exc_type(), Some(ExcType::IndexError));
    assert_eq!(ObjError::Overflow.exc_type(), Some(ExcType::OverflowError));
    assert_eq!(ObjError::OutOfMemory.exc_type(), None);
    assert_eq!(ObjError::OutOfMemory.message(), None);
    assert_eq!(ObjError::OutOfMemory.to_pyerror(), PyError::OutOfMemory);
    assert_eq!(ObjError::IntConversion.to_pyerror(), PyError::PyRaise);
    assert_eq!(ExcType::TypeError.name(), "TypeError");
    assert_eq!(ObjError::BufferProtocol.message(), Some("object with buffer protocol required"));
}
