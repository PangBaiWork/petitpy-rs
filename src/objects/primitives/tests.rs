//! Lane C7 tests — int / bool / cell against the reference binary's
//! verified behaviour (see `scripts/prims_result.txt` for the probe
//! list). The fixture builds real type objects (with an empty dict)
//! so exact-type checks and allocations go through the real paths.
//! The qstr resolver reads the static table only — dynamic ids
//! resolve to `None` here (the VM's `RuntimeQstr` pool owns them).

use super::*;
use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{
    BytesData, DictData, GcRef, MapData, Obj, StrData, TypeData, Value,
};
use crate::runtime::qstr::RuntimeQstr;
use crate::vendor::mpz::BigInt;

pub(crate) fn static_resolver(id: u32) -> Option<Vec<u8>> {
    crate::compiler::qstr::QSTR_STRINGS
        .get(id as usize)
        .map(|s| s.to_vec())
}

pub(crate) struct Fixture {
    pub(crate) arena: GcArena,
    pub(crate) roots: RootSet,
    pub(crate) types: BuiltinTypes,
    pub(crate) pool: RuntimeQstr,
    pub(crate) pending_raise: Option<PendingRaise>,
}

pub(crate) fn empty_dict(arena: &mut GcArena, roots: &RootSet) -> GcRef {
    arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::new(),
        }),
        roots,
    )
}

pub(crate) fn make_type(arena: &mut GcArena, roots: &RootSet, name: &[u8]) -> GcRef {
    let dict = empty_dict(arena, roots);
    let data = TypeData {
        name: static_qstr_id(name).expect("type name is a static qstr"),
        flags: 0,
        base: None,
        mro: Vec::new(),
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
        protocol: None,
    };
    arena.alloc(None, Obj::Type(data), roots)
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        let mut arena = GcArena::new();
        let mut roots = RootSet::new();
        let int = make_type(&mut arena, &roots, b"int");
        let str_ = make_type(&mut arena, &roots, b"str");
        let bytes = make_type(&mut arena, &roots, b"bytes");
        let bool_ = make_type(&mut arena, &roots, b"bool");
        let none_type = make_type(&mut arena, &roots, b"NoneType");
        let float = make_type(&mut arena, &roots, b"float");
        let complex = make_type(&mut arena, &roots, b"complex");
        let tuple = make_type(&mut arena, &roots, b"tuple");
        let list = make_type(&mut arena, &roots, b"list");
        let set = make_type(&mut arena, &roots, b"set");
        let frozenset = make_type(&mut arena, &roots, b"frozenset");
        let range = make_type(&mut arena, &roots, b"range");
        let dict = make_type(&mut arena, &roots, b"dict");
        let array = make_type(&mut arena, &roots, b"array");
        let bytearray = make_type(&mut arena, &roots, b"bytearray");
        let type_ = make_type(&mut arena, &roots, b"type");
        // C12: plain object/super/staticmethod/classmethod/
        // boundmeth types and the ROM `()` singleton.
        let object = make_type(&mut arena, &roots, b"object");
        let super_ = make_type(&mut arena, &roots, b"super");
        let staticmethod = make_type(&mut arena, &roots, b"staticmethod");
        let classmethod = make_type(&mut arena, &roots, b"classmethod");
        let boundmeth = make_type(&mut arena, &roots, b"bound_method");
        let property = make_type(&mut arena, &roots, b"property");
        let empty_tuple = arena.alloc(Some(tuple), Obj::Tuple(vec![]), &roots);
        let empty_bytes = arena.alloc(
            Some(bytes),
            Obj::Bytes(BytesData { bytes: Vec::new() }),
            &roots,
        );
        let native_init_wrapper = make_type(&mut arena, &roots, b"function");
        for t in [int, str_, bytes, bool_, none_type, float, complex, tuple, list, set, frozenset, range, dict, array, bytearray, type_, object, super_, staticmethod, classmethod, boundmeth, empty_tuple, empty_bytes, property, native_init_wrapper] {
            roots.push(Value::Heap(t));
        }
        let types = BuiltinTypes {
            int,
            str_,
            bytes,
            bool_,
            none_type,
            float,
            complex,
            tuple,
            list,
            set,
            frozenset,
            range,
            dict,
            array,
            bytearray,
            type_,
            object,
            super_,
            staticmethod,
            classmethod,
            boundmeth,
            empty_tuple,
            empty_bytes,
            native_init_wrapper,
            property,
            socket: None,
        };
        Fixture { arena, roots, types, pool: RuntimeQstr::new(), pending_raise: None }
    }

    pub(crate) fn call<R>(&mut self, f: impl FnOnce(&mut CallCtx<'_>, &BuiltinTypes) -> R) -> R {
        let mut ctx = CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool, &mut self.pending_raise);
        f(&mut ctx, &self.types)
    }

    pub(crate) fn heap_str(&mut self, bytes: &[u8]) -> Value {
        let t = self.types.str_;
        Value::Heap(self.arena.alloc(
            Some(t),
            Obj::Str(StrData { bytes: bytes.to_vec() }),
            &self.roots,
        ))
    }

    pub(crate) fn heap_bytes(&mut self, bytes: &[u8]) -> Value {
        let t = self.types.bytes;
        Value::Heap(self.arena.alloc(
            Some(t),
            Obj::Bytes(BytesData { bytes: bytes.to_vec() }),
            &self.roots,
        ))
    }

    pub(crate) fn heap_float(&mut self, f: f64) -> Value {
        let t = self.types.float;
        Value::Heap(self.arena.alloc(Some(t), Obj::Float(f), &self.roots))
    }

    pub(crate) fn heap_int(&mut self, z: BigInt) -> Value {
        let t = self.types.int;
        Value::Heap(self.arena.alloc(Some(t), Obj::Int(z), &self.roots))
    }

    pub(crate) fn heap_tuple(&mut self, items: Vec<Value>) -> Value {
        let t = self.types.tuple;
        Value::Heap(self.arena.alloc(Some(t), Obj::Tuple(items), &self.roots))
    }

    pub(crate) fn heap_list(&mut self, items: Vec<Value>) -> Value {
        let t = self.types.list;
        Value::Heap(self.arena.alloc(
            Some(t),
            Obj::List(crate::objects::value::ListData { items }),
            &self.roots,
        ))
    }
}

pub(crate) fn as_i64(v: Value, arena: &GcArena) -> Option<i64> {
    match v {
        Value::SmallInt(n) => Some(n),
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => z.as_int_checked(),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn as_big(v: Value, arena: &GcArena) -> Option<BigInt> {
    match v {
        Value::SmallInt(n) => Some(BigInt::from_int(n)),
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => Some(z.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn as_f64(v: Value, arena: &GcArena) -> Option<f64> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Float(f) => Some(*f),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn as_bytes(v: Value, arena: &GcArena) -> Option<Vec<u8>> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Bytes(b) => Some(b.bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn as_bool(v: Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(b),
        _ => None,
    }
}

pub(crate) fn pow2(n: u64) -> BigInt {
    BigInt::from_int(1) << n
}

pub(crate) fn err_kind(e: PrimError) -> (Option<ExcType>, Option<&'static str>) {
    (e.exc_type(), e.message())
}

// ── parse ───────────────────────────────────────────────────────────────

#[test]
fn parse_decimal_signs_and_space() {
    assert_eq!(parse_int_text(b"42", 10).unwrap(), BigInt::from_int(42));
    assert_eq!(parse_int_text(b"  -42 ", 10).unwrap(), BigInt::from_int(-42));
    assert_eq!(parse_int_text(b"+7", 10).unwrap(), BigInt::from_int(7));
    assert_eq!(parse_int_text(b"  + 42", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"   ", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"42x", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"4 2", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"42\t\n", 10).unwrap(), BigInt::from_int(42));
}

#[test]
fn parse_base_prefixes() {
    assert_eq!(parse_int_text(b"0x1F", 0).unwrap(), BigInt::from_int(31));
    assert_eq!(parse_int_text(b"0X1F", 0).unwrap(), BigInt::from_int(31));
    assert_eq!(parse_int_text(b"0o17", 0).unwrap(), BigInt::from_int(15));
    assert_eq!(parse_int_text(b"0b11", 0).unwrap(), BigInt::from_int(3));
    assert_eq!(parse_int_text(b"0B11", 0).unwrap(), BigInt::from_int(3));
    assert_eq!(parse_int_text(b"010", 10).unwrap(), BigInt::from_int(10));
    // Base 0 with a leading `0` and no prefix letter becomes base 1:
    // `int("010", 0)` is a ValueError on the reference binary.
    assert_eq!(parse_int_text(b"010", 0).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"00", 0).unwrap(), BigInt::from_int(0));
    assert_eq!(parse_int_text(b"01", 0).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"0x", 0).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"0x1F", 10).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"ff", 16).unwrap(), BigInt::from_int(255));
    assert_eq!(parse_int_text(b"FF", 16).unwrap(), BigInt::from_int(255));
    assert_eq!(parse_int_text(b"z", 36).unwrap(), BigInt::from_int(35));
    assert_eq!(parse_int_text(b"19", 8).unwrap_err(), PrimError::InvalidSyntaxForInt);
    assert_eq!(parse_int_text(b"17", 8).unwrap(), BigInt::from_int(15));
}

#[test]
fn parse_underscores_and_base_range() {
    assert_eq!(parse_int_text(b"1_000", 10).unwrap(), BigInt::from_int(1000));
    assert_eq!(parse_int_text(b"_", 10).unwrap(), BigInt::from_int(0));
    assert_eq!(parse_int_text(b"_1", 10).unwrap(), BigInt::from_int(1));
    assert_eq!(parse_int_text(b"1__2", 10).unwrap(), BigInt::from_int(12));
    assert_eq!(parse_int_text(b"0x_1", 0).unwrap(), BigInt::from_int(1));
    assert_eq!(parse_int_text(b"42", 1).unwrap_err(), PrimError::IntBaseRange);
    assert_eq!(parse_int_text(b"42", 37).unwrap_err(), PrimError::IntBaseRange);
    assert_eq!(parse_int_text(b"42", -5).unwrap_err(), PrimError::IntBaseRange);
    assert_eq!(
        err_kind(parse_int_text(b"42", 1).unwrap_err()),
        (Some(ExcType::ValueError), Some("int() arg 2 must be >= 2 and <= 36"))
    );
}

#[test]
fn parse_big_int_exact() {
    let z = parse_int_text(b"1180591620717411303424", 10).unwrap();
    assert_eq!(z, pow2(70));
    assert_eq!(z.bit_length(), 71);
}

// ── make_new ────────────────────────────────────────────────────────────

#[test]
fn make_new_zero_and_one_arg() {
    let mut fx = Fixture::new();
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[], 0).unwrap());
    assert_eq!(v, Value::SmallInt(0));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[Value::SmallInt(42)], 0).unwrap());
    assert_eq!(v, Value::SmallInt(42));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[Value::Bool(true)], 0).unwrap());
    assert_eq!(v, Value::SmallInt(1));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[Value::Bool(false)], 0).unwrap());
    assert_eq!(v, Value::SmallInt(0));
    let big = fx.heap_int(pow2(70));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[big], 0).unwrap());
    assert_eq!(as_big(v, &fx.arena).unwrap(), pow2(70));
}

#[test]
fn make_new_str_and_bytes() {
    let mut fx = Fixture::new();
    let s = fx.heap_str(b"  -0x1F ");
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, Value::SmallInt(0)], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(-31));
    let s = fx.heap_str(b"42");
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(42));
    let b = fx.heap_bytes(b"123");
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[b], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(123));
    let b = fx.heap_bytes(b"42");
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[b, Value::SmallInt(10)], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(42));
}

#[test]
fn make_new_float_exact() {
    let mut fx = Fixture::new();
    let f = fx.heap_float(3.99);
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[f], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(3));
    let f = fx.heap_float(-3.99);
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[f], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(-3));
    let f = fx.heap_float(1e30);
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[f], 0).unwrap());
    assert_eq!(
        as_big(v, &fx.arena).unwrap().as_str(10, "", b'a', 0),
        "1000000000000000019884624838656"
    );
    let f = fx.heap_float(f64::NAN);
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[f], 0).unwrap_err());
    assert_eq!(err_kind(e), (Some(ExcType::ValueError), Some("can't convert NaN to int")));
    let f = fx.heap_float(f64::INFINITY);
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[f], 0).unwrap_err());
    assert_eq!(err_kind(e), (Some(ExcType::OverflowError), Some("can't convert inf to int")));
}

#[test]
fn make_new_two_arg_errors() {
    let mut fx = Fixture::new();
    // Non-str first arg: the NULL+len-0 parse raises InvalidSyntaxForInt.
    let e = fx.call(|ctx, t| {
        int_make_new(ctx, t, &static_resolver, &[Value::SmallInt(42), Value::SmallInt(10)], 0)
            .unwrap_err()
    });
    assert_eq!(err_kind(e), (Some(ExcType::ValueError), Some("invalid syntax for integer")));
    let none = Value::None;
    let e = fx.call(|ctx, t| {
        int_make_new(ctx, t, &static_resolver, &[none, Value::SmallInt(10)], 0).unwrap_err()
    });
    assert_eq!(e, PrimError::InvalidSyntaxForInt);
    // Non-int base → IntConversion.
    let s = fx.heap_str(b"42");
    let b = fx.heap_str(b"10");
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, b], 0).unwrap_err());
    assert_eq!(err_kind(e), (Some(ExcType::TypeError), Some("can't convert to int")));
    let s = fx.heap_str(b"42");
    let fl = fx.heap_float(10.0);
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, fl], 0).unwrap_err());
    assert_eq!(e, PrimError::IntConversion);
    // Too-big base → OverflowWord.
    let s = fx.heap_str(b"42");
    let big = fx.heap_int(pow2(70));
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, big], 0).unwrap_err());
    assert_eq!(
        err_kind(e),
        (Some(ExcType::OverflowError), Some("overflow converting long int to machine word"))
    );
    // Base truncated i64 → c_int: 2**40 → 0, 2**32+16 → 16.
    let s = fx.heap_str(b"42");
    let big = fx.heap_int(pow2(40));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, big], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(42));
    let s = fx.heap_str(b"ff");
    let big = fx.heap_int(BigInt::from_int(1) << 32u64 | BigInt::from_int(16));
    let v = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[s, big], 0).unwrap());
    assert_eq!(as_i64(v, &fx.arena), Some(255));
    // Bool base → 1 → IntBaseRange.
    let s = fx.heap_str(b"42");
    let e = fx.call(|ctx, t| {
        int_make_new(ctx, t, &static_resolver, &[s, Value::Bool(true)], 0).unwrap_err()
    });
    assert_eq!(e, PrimError::IntBaseRange);
}

#[test]
fn make_new_arg_counts_polled() {
    let mut fx = Fixture::new();
    // 3 positional args, non-str first: the branch raises first.
    let e = fx.call(|ctx, t| {
        int_make_new(
            ctx,
            t,
            &static_resolver,
            &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
            0,
        )
        .unwrap_err()
    });
    assert_eq!(e, PrimError::InvalidSyntaxForInt);
    // 3 positional args, clean branch → pending mismatch surfaces.
    let s = fx.heap_str(b"42");
    let e = fx.call(|ctx, t| {
        int_make_new(
            ctx,
            t,
            &static_resolver,
            &[s, Value::SmallInt(10), Value::SmallInt(99)],
            0,
        )
        .unwrap_err()
    });
    assert_eq!(err_kind(e), (Some(ExcType::TypeError), Some("argument num/types mismatch")));
    // kwargs → mismatch.
    let e = fx.call(|ctx, t| int_make_new(ctx, t, &static_resolver, &[], 1).unwrap_err());
    assert_eq!(e, PrimError::ArgNumTypesMismatch);
    let e = fx.call(|ctx, t| {
        int_make_new(ctx, t, &static_resolver, &[Value::SmallInt(1)], 1).unwrap_err()
    });
    assert_eq!(e, PrimError::ArgNumTypesMismatch);
}

