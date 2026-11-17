//! Lane C9a test fixture — a `CallCtx` with slotted int/bool types
//! (so `hash_of` works for int/bool keys, exactly as the real VM
//! wires them) and a slotted dict type (so the dict slots are the
//! ones under test), plus plain types for everything else.
//!
//! Mirrors the C7 `Fixture` (`primitives/tests.rs`): the type
//! objects are real heap allocations with empty dicts, rooted so
//! the arena does not collect them; `mro` is left empty (no test
//! here needs it).

use super::objarray::{array_type_data, bytearray_type_data, memoryview_type_data};
use super::objdict::dict_type_data;
use super::objlist::list_type_data;
use super::objrange::range_type_data;
use super::objset::{frozenset_type_data, set_type_data};
use super::objtuple::tuple_type_data;
use crate::memory::gc::{CallCtx, GcArena, RootSet};
use crate::objects::callables::objgenerator::gen_instance_type_data;
use crate::objects::obj::BuiltinTypes;
use crate::objects::slots::PrintKind;
use crate::objects::primitives::objbool::bool_type_data;
use crate::objects::primitives::objint::int_type_data;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{
    BytesData, DictData, GcRef, ListData, MapData, Obj, StrData, TypeData, Value,
};
use crate::runtime::qstr::RuntimeQstr;
use crate::vendor::mpz::BigInt;

pub(crate) fn empty_dict(arena: &mut GcArena, roots: &RootSet) -> GcRef {
    arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::new(),
        }),
        roots,
    )
}

/// Test-double `str` print (`'…'` with the raw bytes, no
/// escaping). C8a owns the real `str` print slot; the fixture's str
/// type uses this so dict/view print tests can assert real reprs
/// instead of C6's `<str>` placeholder.
pub(crate) fn str_print_double(
    ctx: &mut CallCtx,
    v: Value,
    _kind: PrintKind,
    sink: &mut dyn std::fmt::Write,
) {
    if let Value::Heap(r) = v {
        if let Obj::Str(s) = ctx.arena.get(r) {
            let _ = write!(sink, "'");
            let _ = sink.write_str(&String::from_utf8_lossy(&s.bytes));
            let _ = write!(sink, "'");
        }
    }
}

/// Like `make_type`, but with a print slot (for the doubles above).
fn make_type_with_print(
    arena: &mut GcArena,
    roots: &RootSet,
    name: &[u8],
    print: crate::objects::slots::PrintFn,
) -> GcRef {
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
        print: Some(print),
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
    };
    arena.alloc(None, Obj::Type(data), roots)
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
    };
    arena.alloc(None, Obj::Type(data), roots)
}

pub(crate) struct Fixture {
    pub(crate) arena: GcArena,
    pub(crate) roots: RootSet,
    pub(crate) types: BuiltinTypes,
    pub(crate) pool: RuntimeQstr,
    /// The three array-family types (C9b). They are fixture
    /// fields — not `BuiltinTypes` fields — because the
    /// callers take the header type explicitly, exactly like
    /// `dict_type_data`'s parameter.
    pub(crate) array: GcRef,
    pub(crate) bytearray: GcRef,
    pub(crate) memoryview: GcRef,
    /// The generator instance type (C10b). Fixture field — not a
    /// `BuiltinTypes` field — because the callers take the header
    /// type explicitly, exactly like the array types above. Its
    /// locals dict is EMPTY here (methods are tested through
    /// `gen_instance_locals_dict` directly). (The gen_wrap type is
    /// NOT a fixture field: its tests build it fresh via
    /// `gen_wrap_type_data`.)
    pub(crate) gen_instance: GcRef,
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        let mut arena = GcArena::new();
        let mut roots = RootSet::new();
        let int_dict = empty_dict(&mut arena, &roots);
        let int = arena.alloc(None, Obj::Type(int_type_data(int_dict)), &roots);
        let bool_dict = empty_dict(&mut arena, &roots);
        let bool_ = arena.alloc(None, Obj::Type(bool_type_data(bool_dict)), &roots);
        let str_ = make_type_with_print(&mut arena, &roots, b"str", str_print_double);
        let bytes = make_type(&mut arena, &roots, b"bytes");
        let none_type = make_type(&mut arena, &roots, b"NoneType");
        let float = make_type(&mut arena, &roots, b"float");
        let complex = make_type(&mut arena, &roots, b"complex");
        let tuple_dict = empty_dict(&mut arena, &roots);
        let tuple = arena.alloc(None, Obj::Type(tuple_type_data(tuple_dict)), &roots);
        let list_dict = empty_dict(&mut arena, &roots);
        let list = arena.alloc(None, Obj::Type(list_type_data(list_dict)), &roots);
        let set_dict = empty_dict(&mut arena, &roots);
        let set = arena.alloc(None, Obj::Type(set_type_data(set_dict)), &roots);
        let frozenset_dict = empty_dict(&mut arena, &roots);
        let frozenset = arena.alloc(None, Obj::Type(frozenset_type_data(frozenset_dict)), &roots);
        let range_dict = empty_dict(&mut arena, &roots);
        let range = arena.alloc(None, Obj::Type(range_type_data(range_dict)), &roots);
        let array_dict = empty_dict(&mut arena, &roots);
        let array = arena.alloc(None, Obj::Type(array_type_data(array_dict)), &roots);
        let bytearray_dict = empty_dict(&mut arena, &roots);
        let bytearray =
            arena.alloc(None, Obj::Type(bytearray_type_data(bytearray_dict)), &roots);
        let memoryview_dict = empty_dict(&mut arena, &roots);
        let memoryview = arena.alloc(
            None,
            Obj::Type(memoryview_type_data(memoryview_dict)),
            &roots,
        );
        let dict_dict = empty_dict(&mut arena, &roots);
        let dict = arena.alloc(None, Obj::Type(dict_type_data(dict_dict)), &roots);
        let gen_instance_dict = empty_dict(&mut arena, &roots);
        let gen_instance = arena.alloc(
            None,
            Obj::Type(gen_instance_type_data(gen_instance_dict)),
            &roots,
        );
        let type_ = make_type(&mut arena, &roots, b"type");
        // C12: the object/super/staticmethod/classmethod/boundmeth
        // types and the ROM `()` singleton (plain types here — the
        // objtype tests overwrite them with the real ROM types).
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
        for t in [
            int, str_, bytes, bool_, none_type, float, complex, tuple, list, set, frozenset,
            range, array, bytearray, memoryview, dict, type_, gen_instance, object, super_,
            staticmethod, classmethod, boundmeth, property, empty_tuple, empty_bytes,
            native_init_wrapper,
        ] {
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
        Fixture {
            arena,
            roots,
            types,
            pool: RuntimeQstr::new(),
            array,
            bytearray,
            memoryview,
            gen_instance,
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool)
    }

    /// A heap `Str` with these bytes.
    pub(crate) fn heap_str(&mut self, bytes: &[u8]) -> Value {
        let t = self.types.str_;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Str(StrData { bytes: bytes.to_vec() })))
    }

    /// A heap `Bytes` with these bytes.
    pub(crate) fn heap_bytes(&mut self, bytes: &[u8]) -> Value {
        let t = self.types.bytes;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Bytes(BytesData { bytes: bytes.to_vec() })))
    }

    /// A heap `Int` holding this `BigInt`.
    pub(crate) fn heap_bigint(&mut self, z: BigInt) -> Value {
        let t = self.types.int;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Int(z)))
    }

    /// A heap `Tuple` of these values.
    pub(crate) fn heap_tuple(&mut self, items: Vec<Value>) -> Value {
        let t = self.types.tuple;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Tuple(items)))
    }

    /// A heap `List` of these values.
    pub(crate) fn heap_list(&mut self, items: Vec<Value>) -> Value {
        let t = self.types.list;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::List(ListData { items })))
    }

    /// A fresh plain dict.
    pub(crate) fn dict(&mut self) -> Value {
        let t = self.types.dict;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Dict(DictData { map: MapData::new() })))
    }

    /// The `OrderedDict` type object (a plain type with that name —
    /// C12 owns the real type object). Rooted.
    pub(crate) fn ordered_dict_type(&mut self) -> GcRef {
        let t = make_type(&mut self.arena, &self.roots, b"OrderedDict");
        self.roots.push(Value::Heap(t));
        t
    }

    /// A fresh `OrderedDict` (same layout, header type is the
    /// `OrderedDict` type object).
    pub(crate) fn ordered_dict(&mut self) -> Value {
        let t = self.ordered_dict_type();
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(t), Obj::Dict(DictData { map: MapData::new() })))
    }

    /// Store `k -> v` in `d` (panics on error — tests assert the
    /// error paths separately, through the fallible fns).
    pub(crate) fn store(&mut self, d: Value, k: Value, v: Value) {
        let mut ctx = self.ctx();
        super::objdict::dict_store(&mut ctx, d, k, v).expect("store in test fixture");
    }

    /// The live `(key, value)` pairs of a dict, in order.
    pub(crate) fn pairs(&self, d: Value) -> Vec<(Value, Value)> {
        let r = match d {
            Value::Heap(r) => r,
            _ => panic!("pairs() on a non-heap value"),
        };
        let m = match self.arena.get(r) {
            Obj::Dict(d) => &d.map,
            _ => panic!("pairs() on a non-dict value"),
        };
        m.entries
            .iter()
            .filter_map(|e| e.key.map(|k| (k, e.value)))
            .collect()
    }
}
