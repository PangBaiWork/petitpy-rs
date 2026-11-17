//! Value / Obj — the native Rust value model (C0, frozen interface).
//!
//! Port of the *semantics* of `src/objects/obj.zig` + `src/mp_mirrors.zig`
//! (REPR_A tagging, `MpObjBase`, type ROMs). The C layout itself is NOT
//! ported: there is no vendor C left to mirror, so values are native
//! Rust. See `docs/value-obj-design.md` (D1–D8) for the frozen decisions.
//!
//! Layout:
//! - `Value` — Copy enum: immediates inline, heap objects by `GcRef`.
//! - `GcRef` — arena slot index (stable: the GC never moves objects).
//! - `Obj` — every object kind as a variant. Variant set frozen; each
//!   variant's payload struct is owned by its lane and may evolve.
//! - `TypeData` — types are heap objects (`Obj::Type`); method slots are
//!   Rust fn pointers, one per `mp_obj_type_t` slot.

use crate::memory::gc::{GcArena, RootSet};
use crate::objects::containers::map_set::{set_items, MpSet};
use crate::objects::slots::{
    AttrFn, BinaryOpFn, BufferFn, BuiltinFn, BuiltinKwFn, CallFn, GetIterFn, MakeNewFn,
    PrintFn, StreamP, SubscrFn, UnaryOpFn,
};
use crate::pyerr::PyResult;
use crate::vendor::mpz::BigInt;

/// Small-int window (REPR_A, 62 bits; `mp_mirrors.zig:548-551`).
pub const SMALL_INT_BITS: u8 = 63;
pub const SMALL_INT_MAX: i64 = (1i64 << 62) - 1;
pub const SMALL_INT_MIN: i64 = -(1i64 << 62);

/// Whether `n` fits in a small int (`MP_SMALL_INT_FITS`).
pub fn small_int_fits(n: i64) -> bool {
    (SMALL_INT_MIN..=SMALL_INT_MAX).contains(&n)
}

/// `mirrors.newSmallInt(n)`: tag without a window check —
/// `((n << 1) | 1)` as a word, read back as a small int.
/// The result always lands in the 62-bit window.
/// (Vendor `MP_OBJ_NEW_SMALL_INT` does the same tag-wrap with
/// no window check — e.g. `objint_mpz.c:147-148` returns the
/// int hash through it, so `hash(2**62) == -(2**62)`.)
pub fn wrap_small_int(n: i64) -> i64 {
    (((n as u64) << 1) | 1) as i64 >> 1
}

/// A Python value.
///
/// Immediates are inline (like the Zig `Obj = usize` immediates);
/// heap objects live in the `GcArena` and are named by `GcRef`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Value {
    /// 62-bit small int (`(n << 1) | 1` in Zig).
    SmallInt(i64),
    Bool(bool),
    None,
    /// Interned string immediate (`(q << 3) | 2` in Zig); `u32` is the
    /// qstr id, shared with the compiler qstr table (A1).
    Qstr(u32),
    /// Heap object handle.
    Heap(GcRef),
}

/// Arena slot index. Stable for the object's lifetime: mark-sweep never
/// moves objects, so a `GcRef` stays valid until the object is collected
/// (and no live `Value` may point at a collected slot — see `RootSet`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GcRef(pub u32);

impl Value {
    pub fn none() -> Value {
        Value::None
    }

    pub fn new_bool(b: bool) -> Value {
        Value::Bool(b)
    }

    pub fn qstr(id: u32) -> Value {
        Value::Qstr(id)
    }

    pub fn heap(r: GcRef) -> Value {
        Value::Heap(r)
    }

    /// Small int when `n` fits the 62-bit window, else `None`
    /// (the caller allocates a heap `Int(BigInt)` instead).
    pub fn small_int(n: i64) -> Option<Value> {
        if small_int_fits(n) {
            Some(Value::SmallInt(n))
        } else {
            None
        }
    }

    /// Integer value: small int in-window, heap `Int(BigInt)` out of
    /// window (mirrors `objint.c`'s small/mpz split; `type_` is the
    /// int type object, `None` while bootstrapping).
    pub fn int(arena: &mut GcArena, type_: Option<GcRef>, roots: &RootSet, n: i64) -> Value {
        match Value::small_int(n) {
            Some(v) => v,
            None => Value::Heap(arena.alloc(type_, Obj::Int(BigInt::from_int(n)), roots)),
        }
    }

    pub fn is_small_int(&self) -> bool {
        matches!(self, Value::SmallInt(_))
    }

    pub fn is_qstr(&self) -> bool {
        matches!(self, Value::Qstr(_))
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    pub fn is_heap(&self) -> bool {
        matches!(self, Value::Heap(_))
    }

    pub fn as_small_int(&self) -> Option<i64> {
        match self {
            Value::SmallInt(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_qstr(&self) -> Option<u32> {
        match self {
            Value::Qstr(id) => Some(*id),
            _ => None,
        }
    }

    pub fn as_heap(&self) -> Option<GcRef> {
        match self {
            Value::Heap(r) => Some(*r),
            _ => None,
        }
    }

    /// Truth testing (`mp_obj_is_true`) — fast path for non-instance
    /// values (payload rules: numbers by value, strings/collections
    /// by emptiness). The full dispatch (`__bool__`/`__len__` slots)
    /// is `obj::obj_is_true`; this stays as the primitive fast path
    /// and agrees with it for every value representable today (no
    /// instance variant exists yet).
    pub fn is_true(&self, arena: &GcArena) -> PyResult<bool> {
        match self {
            Value::None => Ok(false),
            Value::Bool(b) => Ok(*b),
            Value::SmallInt(n) => Ok(*n != 0),
            Value::Qstr(id) => {
                // C6 fix (design doc: "C6 lane 补全"): a qstr's truth
                // follows its LENGTH, like any str — `bool("")` is
                // False on the reference binary. Static ids read the
                // generated length table; dynamic ids (>= N_TOTAL)
                // are never empty (`RuntimeQstr::from_strn` maps ""
                // to the static empty qstr, id 1), so they are true.
                if (*id as usize) < crate::qstr_table::QSTR_LENGTHS.len() {
                    Ok(crate::qstr_table::QSTR_LENGTHS[*id as usize] != 0)
                } else {
                    Ok(true)
                }
            }
            Value::Heap(r) => match arena.get(*r) {
                Obj::Float(f) => Ok(*f != 0.0),
                Obj::Int(z) => Ok(!z.is_zero()),
                Obj::Complex { re, im } => Ok(*re != 0.0 || *im != 0.0),
                Obj::Str(s) => Ok(!s.bytes.is_empty()),
                Obj::Bytes(b) => Ok(!b.bytes.is_empty()),
                Obj::Tuple(t) => Ok(!t.is_empty()),
                Obj::List(l) => Ok(!l.items.is_empty()),
                Obj::Dict(d) => Ok(d.map.used != 0),
                Obj::Set(s) => Ok(s.table.used != 0),
                _ => Ok(true),
            },
        }
    }
}

/// Header of every heap object: its type (a `GcRef` to an `Obj::Type`
/// slot) plus GC flags. `type_` is `None` only while bootstrapping the
/// very first type objects (in MicroPython `mp_type_type`'s type is
/// itself — the same circularity, resolved at startup).
#[derive(Clone, Copy, Debug)]
pub struct ObjHeader {
    pub type_: Option<GcRef>,
    pub flags: u8,
}

impl ObjHeader {
    pub const FLAG_MARK: u8 = 0x01;
    pub const FLAG_FINALISER: u8 = 0x02;

    pub fn new(type_: Option<GcRef>) -> ObjHeader {
        ObjHeader { type_, flags: 0 }
    }
}

/// A heap object. Variant set frozen (`docs/value-obj-design.md` D4);
/// payload structs are lane-owned and may evolve.
#[derive(Clone, Debug)]
pub enum Obj {
    Float(f64),
    Int(BigInt),
    Complex { re: f64, im: f64 },
    Str(StrData),
    Bytes(BytesData),
    Tuple(Vec<Value>),
    List(ListData),
    Dict(DictData),
    Set(SetData),
    Array(ArrayData),
    Range(RangeData),
    Slice(SliceData),
    FunBc(FunBcData),
    FunBuiltin(FunBuiltinData),
    BoundMeth(BoundMethData),
    Closure(ClosureData),
    Generator(GeneratorData),
    Cell(CellData),
    Type(TypeData),
    Module(ModuleData),
    Exception(ExceptionData),
    Property(PropertyData),
    Super(SuperData),
    Iterator(IteratorData),
    Deque(DequeData),
    AttrTuple(AttrTupleData),
    Map(MapData),
    /// User-class instance (`mp_obj_instance_t`, objtype.zig).
    /// `members` is the instance `__dict__` (a `MapData`, not a
    /// `DictData` — the Zig keeps the raw map); `subobj` holds the
    /// native-base object(s), 0 or 1 entries (`mp_obj_new_instance`
    /// asserts fewer than 2 native bases). A bare `object()` result
    /// is also an `Instance` with an empty map and no subobj: the
    /// Zig allocates a bare `MpObjBase` for it, but every members
    /// access goes through the type's `attr` slot, which
    /// `mp_type_object` does NOT have — so the empty map is
    /// unreachable and the two are observationally identical
    /// (probed: `object().__dict__` raises AttributeError,
    /// `o.x = 1` raises AttributeError). C12 owns.
    Instance(InstanceData),
    /// `staticmethod` / `classmethod` wrapper (`MpObjStaticClassMethod`,
    /// objtype.zig: `newStaticClassMethod`). One payload for both —
    /// the header type (arena slot `type_`) says which. C12 owns.
    StaticClassMethod(StaticClassMethodData),
    /// The `Ellipsis` singleton (`...` / `Ellipsis`, C13): the
    /// `.mpy` const table carries an Ellipsis entry and the corpus
    /// has `builtin_ellipsis.py`, so the value needs a payload —
    /// a unit variant (no children, no bytes). The VM owns the
    /// single instance (header type = the VM's ellipsis type,
    /// name qstr 0 — the reference binary prints
    /// `type(...)` as `<class ''>` and `repr(...)` as
    /// `Ellipsis`, probed 2026-11-17).
    Ellipsis,
    /// `io.StringIO` / `io.BytesIO` payload (C18b owns;
    /// `vendor/py/objstringio.c`). `buf` is the owned content
    /// (the C keeps a borrowed `fixed_buf` + `ref_obj` for a
    /// str/bytes constructor arg; Rust clones eagerly —
    /// observably identical because str/bytes are immutable
    /// and a bytearray arg takes the C's copy path too,
    /// probed 2026-11-18). `pos` is the read/write cursor;
    /// `closed` means `vstr == NULL` in the C (close frees
    /// the buffer but does NOT reset `pos`).
    StringIo(StringIoData),
}

impl Obj {
    /// Rough heap footprint in bytes (GC accounting; C4 refines this to
    /// the original's block accounting).
    pub fn heap_bytes(&self) -> usize {
        const HDR: usize = 16;
        HDR + match self {
            Obj::Float(_) | Obj::Complex { .. } | Obj::Range(_) => 16,
            Obj::Int(z) => z.num_digits() * 4,
            Obj::Str(s) => s.bytes.len(),
            Obj::Bytes(b) => b.bytes.len(),
            Obj::StringIo(s) => s.buf.len(),
            Obj::Tuple(t) => t.len() * 8,
            Obj::List(l) => l.items.len() * 8,
            Obj::Dict(d) => d.map.alloc * 16 + d.map.index.len() * 4,
            Obj::Set(s) => s.table.alloc * 8,
            Obj::Array(a) => a.data.len(),
            Obj::Slice(_) => 24,
            Obj::FunBc(_) | Obj::FunBuiltin(_) => 32,
            Obj::BoundMeth(_) | Obj::Cell(_) | Obj::Super(_) => 16,
            Obj::Closure(c) => c.cells.len() * 4,
            Obj::Generator(_) => 48,
            Obj::Type(t) => {
                t.mro.len() * 4
                    + 64
                    + t.namedtuple_fields.as_ref().map_or(0, |f| f.len() * 4)
            }
            Obj::Module(_) | Obj::Exception(_) | Obj::Property(_) => 48,
            Obj::Iterator(i) => i.extra.len() * 8 + 16,
            Obj::Deque(d) => d.items.len() * 8,
            Obj::AttrTuple(a) => a.items.len() * 8,
            Obj::Map(m) => m.alloc * 16 + m.index.len() * 4,
            Obj::Instance(i) => i.members.alloc * 16 + i.subobj.len() * 8,
            Obj::StaticClassMethod(_) => 16,
            Obj::Ellipsis => 0,
        }
    }

    /// Child `Value`s this object keeps alive (GC tracing). Lanes extend
    /// this when they fill in a payload; a payload left as a leaf here
    /// must hold no `Value`/`GcRef` children.
    pub fn children(&self) -> Vec<Value> {
        match self {
            Obj::Tuple(t) => t.clone(),
            Obj::List(l) => l.items.clone(),
            Obj::Dict(d) => {
                let mut out = Vec::with_capacity(d.map.entries.len() * 2);
                for e in &d.map.entries {
                    if let Some(k) = e.key {
                        out.push(k);
                        out.push(e.value);
                    }
                }
                out
            }
            Obj::Set(s) => set_items(&s.table),
            Obj::Array(a) => a
                .source
                .map(|s| Value::Heap(s))
                .into_iter()
                .collect(),
            Obj::Slice(s) => [s.start, s.stop, s.step].into_iter().flatten().collect(),
            Obj::BoundMeth(b) => vec![b.func, b.self_],
            Obj::Closure(c) => {
                let mut out = vec![Value::Heap(c.fun)];
                out.extend(c.cells.iter().map(|r| Value::Heap(*r)));
                out
            }
            Obj::Generator(g) => {
                let mut out = vec![Value::Heap(g.fun)];
                out.extend(g.pend_throw);
                out.extend(g.stop_value);
                out
            }
            Obj::Cell(c) => c.value.into_iter().collect(),
            Obj::Type(t) => {
                let mut out: Vec<Value> = t.mro.iter().map(|r| Value::Heap(*r)).collect();
                if let Some(b) = t.base {
                    out.push(Value::Heap(b));
                }
                if let Some(b) = t.bases {
                    out.push(Value::Heap(b));
                }
                out.push(Value::Heap(t.dict));
                out
            }
            Obj::Instance(i) => {
                let mut out = Vec::with_capacity(i.members.entries.len() * 2 + i.subobj.len());
                for e in &i.members.entries {
                    if let Some(k) = e.key {
                        out.push(k);
                        out.push(e.value);
                    }
                }
                out.extend(i.subobj.iter().copied());
                out
            }
            Obj::StaticClassMethod(s) => vec![s.fun],
            Obj::Module(m) => vec![Value::Heap(m.dict)],
            Obj::Exception(e) => {
                let mut out = vec![Value::Heap(e.type_)];
                out.extend(e.args.iter().copied());
                out
            }
            Obj::Property(p) => vec![p.getter, p.setter, p.deleter],
            Obj::Super(s) => vec![Value::Heap(s.type_), s.obj],
            Obj::Iterator(i) => {
                let mut out = vec![i.source];
                out.extend(i.extra.iter().copied());
                out
            }
            Obj::Deque(d) => d.items.clone(),
            Obj::AttrTuple(a) => a.items.clone(),
            Obj::Map(m) => {
                let mut out = Vec::with_capacity(m.entries.len() * 2);
                for e in &m.entries {
                    if let Some(k) = e.key {
                        out.push(k);
                        out.push(e.value);
                    }
                }
                out
            }
            Obj::FunBc(f) => {
                let mut out = f.def_pos.clone();
                if let Some(kw) = f.def_kw {
                    out.push(kw);
                }
                if let Some(c) = f.context {
                    out.push(Value::Heap(c));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

// ── Payload structs (lane-owned; simple initial forms) ─────────────

/// Dynamic string bytes (UTF-8 under `STR_UNICODE=1`). C8 owns.
#[derive(Clone, Debug)]
pub struct StrData {
    pub bytes: Vec<u8>,
}

/// Bytes object. C8 owns.
#[derive(Clone, Debug)]
pub struct BytesData {
    pub bytes: Vec<u8>,
}

/// List object. C9 owns.
#[derive(Clone, Debug)]
pub struct ListData {
    pub items: Vec<Value>,
}

/// One entry of a `MapData` table (`mp_map_elem_t`, map.zig).
/// `key == None` is the SENTINEL tombstone: the entry was removed
/// but its index slot still points at it, so probes step over it
/// (map.zig compact-layout comment). A live entry's key is always
/// `Some` — including `Some(Value::None)` for the `None` key.
/// `value` is always a real value: the Zig's transient NULL value
/// (a slot created by ADD before the caller stores into it) is
/// unobservable, so lookups report it explicitly as `Added` instead
/// (see `map::LookupOutcome`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    pub key: Option<Value>,
    pub value: Value,
}

/// Low-level key/value map (`mp_map_t`). C9 owns. Two layouts
/// (map.zig): compact (full config — insertion-ordered hash table:
/// `entries` in insertion order, `index` an open-addressing index
/// of `index_cap(alloc)` u32 slots, 0 = empty, i+1 = entry i;
/// `entries.len()` is the Zig `fill`, live + tombstones) and ordered
/// (fixed tables + OrderedDict: dense array, no tombstones,
/// `entries.len() == used`, `index` unused). All logic lives in
/// `containers::map` (`impl MapData` there); this file holds only
/// the data definition, like the other payload structs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapData {
    /// Entry slots allocated (`alloc`).
    pub alloc: usize,
    /// Live entry count (`used`).
    pub used: usize,
    /// All keys are qstrs (pointer-compare fast path).
    pub all_keys_are_qstrs: bool,
    /// Fixed (ROM) table — never reallocated by lookup.
    pub is_fixed: bool,
    /// Ordered-array layout (fixed tables + OrderedDict).
    pub is_ordered: bool,
    /// Entries in insertion order; `None` key = SENTINEL tombstone
    /// (compact only). `len()` is the Zig `fill` (compact) or `used`
    /// (ordered).
    pub entries: Vec<MapEntry>,
    /// Open-addressing index (compact only): `index_cap(alloc)` u32
    /// slots; 0 = empty, i+1 = entry i. Empty for ordered maps.
    pub index: Vec<u32>,
}

/// Dict object (a map). C9 owns.
#[derive(Clone, Debug)]
pub struct DictData {
    pub map: MapData,
}

/// Set object. C9 owns. The payload is the `MpSet` hash table
/// itself (`mp_set_t`, map.zig: `alloc`/`used`/`table`) — the Zig
/// stores nothing else in `MpObjSet`, so there is no `items` vec:
/// table order IS the iteration/print order.
#[derive(Clone, Debug)]
pub struct SetData {
    pub table: MpSet,
}

/// Array / bytearray / memoryview object (`objarray.zig`).
/// C9b owns. The payload is RAW BYTES, not `Value`s — the
/// Zig's `MpObjArray { base, bits, len, items }` stores a
/// `*anyopaque` byte buffer; `typecode` is the `MpObjArray`
/// `bits` low byte (typecode + the `0x80` RW flag for
/// memoryview), `free` is the `bits` high 56 bits (the spare
/// count for owned arrays — kept 0, `Vec` manages capacity,
/// unobservable — and the ELEMENT offset for memoryview:
/// `arrayGetBuffer` does `buf + free * sz`, subscr does
/// `index += free`, slice load does `free += start`),
/// `len` is the element count, `data` the owned element
/// bytes (the first `len * itemsize` bytes are valid; empty
/// for views), and `source` the viewed object for a
/// memoryview (an `Array` or `Bytes` heap object;
/// views-of-views flatten at construction to the base source
/// + combined offset, so a view's `source` is never itself a
/// view).
#[derive(Clone, Debug)]
pub struct ArrayData {
    pub typecode: u8,
    pub free: usize,
    pub len: usize,
    pub data: Vec<u8>,
    pub source: Option<GcRef>,
}

/// `io.StringIO` / `io.BytesIO` object (C18b owns;
/// `vendor/py/objstringio.c:173-178` `stringio_new`).
/// `is_text` is NOT stored here: the content type
/// (`str` vs `bytes`, for `getvalue` and the print label)
/// comes from the header type's stream protocol
/// (`StreamP.is_text`, resolved through the MRO — a
/// subclass instance prints/getvalues as its base,
/// probed 2026-11-18).
#[derive(Clone, Debug)]
pub struct StringIoData {
    pub buf: Vec<u8>,
    pub pos: usize,
    pub closed: bool,
}

/// Range object. C9 owns.
#[derive(Clone, Debug)]
pub struct RangeData {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

/// Slice object; bounds are `None` or int values. C9 owns.
#[derive(Clone, Debug)]
pub struct SliceData {
    pub start: Option<Value>,
    pub stop: Option<Value>,
    pub step: Option<Value>,
}

/// Bytecode function (`mp_obj_fun_bc_t`). `code` is an index into the
/// VM's code store (owned by C13); the VM roots the store. C10 owns.
///
/// `def_pos` / `def_kw` are the trailing extra args of
/// `mp_obj_new_fun_bc` (`objfun.zig:204-241`): `def_args[0]` is the
/// tuple of positional defaults (unpacked here), `def_args[1]` the
/// keyword-defaults dict. `setup_code_state` (`bc.rs`) takes them as
/// `extra_args` — positional defaults first, then the defkwargs dict
/// iff the `DEFKWARGS` scope flag is set (`bc.rs`, `extra_args[sig.
/// n_def_pos_args]`). `context` is the module context
/// (`mp_module_context_t`, whose offset 0 IS the module object —
/// `objfun.zig:359-361`); `__globals__` reads the module's dict
/// through it. C13's loader fills `name` / `n_args` (decoded from
/// the prelude + qstr table) and `context`.
#[derive(Clone, Debug)]
pub struct FunBcData {
    pub name: u32,
    pub n_args: u16,
    pub code: usize,
    pub def_pos: Vec<Value>,
    pub def_kw: Option<Value>,
    pub context: Option<GcRef>,
}

/// Built-in function (`mp_obj_fun_builtin_t`). C10 / D1 own the registry.
///
/// `n_args` is C7's `(min << 8) | max` packing (`fun_n_args`,
/// `objint.rs:277`) — the arity range the adapters check. `kw_func`
/// is the keyword-taking body (`mp_obj_fun_builtin_var_t` with the
/// takes_kw sig bit set, `argcheck.zig` `mpArgCheckNumSigImpl`):
/// `Some` ⟺ this builtin takes kwargs, and the `fun_builtin_var_call`
/// adapter calls it (never `func`) when it is `Some`. `None` for
/// fixed-arity and var-no-kw builtins. D1 fills it for kw builtins
/// (`print`, ...).
#[derive(Clone, Debug)]
pub struct FunBuiltinData {
    pub name: u32,
    pub n_args: u16,
    pub func: BuiltinFn,
    pub kw_func: Option<BuiltinKwFn>,
}

/// Bound method (`mp_obj_bound_meth_t`). C10 owns.
#[derive(Clone, Debug)]
pub struct BoundMethData {
    pub func: Value,
    pub self_: Value,
}

/// Closure (`mp_obj_closure_t`). C10 owns.
#[derive(Clone, Debug)]
pub struct ClosureData {
    pub fun: GcRef,
    pub cells: Vec<GcRef>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenState {
    New,
    Suspended,
    Running,
    Closed,
}

/// Generator (`mp_obj_gen_instance_t`). C10 owns; the frame lives in the
/// VM (C13) — `gen_wrap_call` returns it next to the generator handle
/// and the resume functions take it as a parameter, because
/// `CodeState` is a `runtime::bc` type and this file must not depend
/// on `runtime` (that would be a `value` ↔ `bc` module cycle).
/// The Zig's `pend_exc` tri-state maps onto two fields: `state`
/// (`Running` == `pend_exc == OBJ_NULL`, the reentrance guard) and
/// `pend_throw` (`None` == `OBJ_NONE`, `Some(v)` == a queued throw).
/// `stop_value` is the Rust spelling of the Zig thread
/// `stop_iteration_arg` (`makeStopIteration`): the generator's return
/// value on exhaustion — `None` when it returned `None` (the Zig
/// maps `OBJ_NONE` → `OBJ_NULL`, i.e. "no value"). It is stored
/// per-generator (not per-thread) so `yield from` reads the right
/// one; C13's `next()` reads it when building `StopIteration`.
#[derive(Clone, Debug)]
pub struct GeneratorData {
    pub fun: GcRef,
    pub state: GenState,
    /// Queued `.pend_throw()` exception (`None` == no pending throw).
    pub pend_throw: Option<Value>,
    /// Return value on exhaustion (`None` == returned `None`).
    pub stop_value: Option<Value>,
}

/// Cell (`mp_obj_cell_t`); `None` is an empty cell. C7 owns.
#[derive(Clone, Debug)]
pub struct CellData {
    pub value: Option<Value>,
}

/// A type object (`mp_obj_type_t`). C12 owns.
#[derive(Clone, Debug)]
pub struct TypeData {
    /// Type name (qstr id).
    pub name: u32,
    /// `mp_obj_type_t.flags`.
    pub flags: u16,
    /// Base class (`Obj::Type` slot), if any.
    pub base: Option<GcRef>,
    /// Multiple-inheritance bases tuple (`Obj::Tuple` slot), if any.
    /// Set exactly when the class was created with two or more
    /// bases (objtype.zig `newTypeImpl`: the parent slot holds the
    /// bases tuple itself); `base` is `None` then. Single-parent
    /// classes keep `base` and leave this `None`.
    pub bases: Option<GcRef>,
    /// C3 linearization (type slots, self first).
    pub mro: Vec<GcRef>,
    /// Type dict (`Obj::Dict` slot).
    pub dict: GcRef,
    /// Attribute-access slot (`mp_obj_type_t.attr`, C12 adds it —
    /// C10's `AttrFn`, load-only). `None` where the Zig type has
    /// no attr slot.
    pub attr: Option<AttrFn>,
    /// Constructor slot (`mp_obj_type_t.make_new`, C12 adds it).
    /// `fn(ctx, type, args, n_kw)` — `args` holds `n_args`
    /// positionals followed by `2 * n_kw` `(name, value)` keyword
    /// pairs (the `CallFn` layout). `None` where the Zig type has
    /// no make_new slot; builtin constructors are wired by their
    /// owning lanes via thin adapters.
    pub make_new: Option<MakeNewFn>,
    pub unary_op: Option<UnaryOpFn>,
    pub binary_op: Option<BinaryOpFn>,
    pub call: Option<CallFn>,
    pub getiter: Option<GetIterFn>,
    /// Subscript slot (`mp_subscr`). C6 adds it (design doc D5:
    /// "其余槽位（subscr、buffer、print 等）按 lane 需要增补").
    pub subscr: Option<SubscrFn>,
    /// Buffer-protocol slot (`get_buffer`). C6 adds it (D5).
    pub buffer: Option<BufferFn>,
    /// Print slot (`mp_print_t`). C6 adds it (D5).
    pub print: Option<PrintFn>,
    /// Named-tuple field names (qstr ids), in order. `Some` only
    /// for classes created by the named-tuple factory
    /// (`NamedtupleType`, objnamedtuple.zig:93-99) — the Zig keeps
    /// them in the type struct itself, after the header, so Rust
    /// keeps them here too (C11). `None` for every other type.
    /// The field names are qstr ids, not `Value`s, so they need no
    /// GC tracing (see `children()`).
    pub namedtuple_fields: Option<Vec<u32>>,
    /// Stream-protocol vtable (`mp_obj_type_t.protocol`,
    /// `mp_stream_p_t`). C16 adds it (D5: slots are added as
    /// lanes need them). `Some` only for stream types;
    /// `StreamP` holds only fn pointers + a bool, so it needs
    /// no GC tracing (see `children()`).
    pub protocol: Option<StreamP>,
}

/// Module object (`mp_obj_module_t`). C12 owns.
#[derive(Clone, Debug)]
pub struct ModuleData {
    pub name: u32,
    pub dict: GcRef,
}

/// One traceback frame. `name` is the FILE qstr id, `line` the source
/// line (stored but not printed in this config — see
/// `obj::print_exception`), `block` the enclosing block/function name
/// qstr (`None` = MP_QSTRnull, i.e. no `, in <block>` suffix). Frames
/// are stored in ADD order (innermost first — the VM appends while
/// unwinding, as `mp_obj_exception_add_traceback` does); the renderer
/// iterates in reverse ("most recent call last"). C6 owns the
/// rendering.
#[derive(Clone, Debug)]
pub struct TraceFrame {
    pub name: u32,
    pub line: u32,
    pub block: Option<u32>,
}

/// Exception instance (`mp_obj_exception_t`). C6 owns.
#[derive(Clone, Debug)]
pub struct ExceptionData {
    pub type_: GcRef,
    pub args: Vec<Value>,
    pub traceback: Vec<TraceFrame>,
    /// The `__cause__` recorded by `raise ... from` (`None` = no cause
    /// slot set, i.e. the Zig `cause == 0`; `Some(Value::None)` =
    /// explicitly `__cause__ is None`). C6 adds it: the Zig
    /// `npy_exception_get_cause` reads a cause slot on the native
    /// exception (`objexcept.zig:273`).
    pub cause: Option<Value>,
}

/// Property object. C9 owns.
#[derive(Clone, Debug)]
pub struct PropertyData {
    pub getter: Value,
    pub setter: Value,
    pub deleter: Value,
}

/// Super object (`mp_obj_super_t`). C12 owns.
#[derive(Clone, Debug)]
pub struct SuperData {
    pub type_: GcRef,
    pub obj: Value,
}

/// User-class instance (`mp_obj_instance_t`). C12 owns.
#[derive(Clone, Debug)]
pub struct InstanceData {
    /// Instance `__dict__` (raw map, not a dict object).
    pub members: MapData,
    /// Native-base object(s): 0 or 1 entries. `[wrapper]` (the
    /// `native_base_init_wrapper` builtin) means "not initialised
    /// yet"; a real object means initialised; empty means the class
    /// has no native base at all.
    pub subobj: Vec<Value>,
}

/// `staticmethod` / `classmethod` wrapper (`MpObjStaticClassMethod`).
/// C12 owns.
#[derive(Clone, Debug)]
pub struct StaticClassMethodData {
    pub fun: Value,
}

/// Generic iterator state; specific iterators (enumerate/zip/filter/…)
/// are C11's and may grow this struct. C11 owns.
#[derive(Clone, Debug)]
pub struct IteratorData {
    pub source: Value,
    pub index: usize,
    pub extra: Vec<Value>,
}

/// Deque object (`mp_obj_deque_t`, objdeque.zig:6-13) — a ring
/// buffer, ported faithfully (C11). `alloc` is the ring capacity
/// (`maxlen + 1`; the extra slot distinguishes full from empty);
/// `len = (i_put - i_get) mod alloc`. `items` has exactly `alloc`
/// slots; slots outside `[i_get, i_put)` hold `Value::None` (the
/// Zig zeroes them with `m_malloc0`, i.e. `OBJ_NULL` — never
/// observed, since every read goes through the ring indices).
/// `flags` bit 0 is `FLAG_CHECK_OVERFLOW` (objdeque.zig:14).
#[derive(Clone, Debug)]
pub struct DequeData {
    pub alloc: usize,
    pub i_get: usize,
    pub i_put: usize,
    pub items: Vec<Value>,
    pub flags: u32,
}

/// Attribute tuple (`mp_obj_attrtuple_t`). C9 owns. A regular tuple
/// plus the field-name array: the Zig stores the qstr array as a
/// fake-Obj pointer in the trailing `items[len]` slot; Rust keeps it
/// as a parallel `Vec<u32>` of qstr ids (same order as `items`).
#[derive(Clone, Debug)]
pub struct AttrTupleData {
    pub items: Vec<Value>,
    pub fields: Vec<u32>,
}

// ── Method-slot fn types (op codes + slot fn pointers) ───────────────
// Live in `slots.rs` (moved out of this file so it stays under the
// 800-line cap); `TypeData` below refers to them by name.


/// Unused placeholder for slot types not yet needed; keeps the
/// `PyError` import used in this module's doc examples.
#[allow(dead_code)]
fn _assert_pyresult(_: PyResult<Value>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::gc::{GcArena, RootSet};

    #[test]
    fn small_int_window_boundaries() {
        // Mirrors mp_mirrors.zig:548-551.
        assert_eq!(SMALL_INT_MAX, (1i64 << 62) - 1);
        assert_eq!(SMALL_INT_MIN, -(1i64 << 62));
        assert!(small_int_fits(0));
        assert!(small_int_fits(SMALL_INT_MAX));
        assert!(small_int_fits(SMALL_INT_MIN));
        assert!(!small_int_fits(SMALL_INT_MAX + 1));
        assert!(!small_int_fits(SMALL_INT_MIN - 1));
        assert_eq!(Value::small_int(5), Some(Value::SmallInt(5)));
        assert_eq!(Value::small_int(1i64 << 62), None);
    }

    #[test]
    fn immediates_identity() {
        // Mirrors the OBJ_* constants (mp_mirrors.zig:492-497).
        assert_eq!(Value::none(), Value::None);
        assert_eq!(Value::new_bool(true), Value::Bool(true));
        assert_ne!(Value::new_bool(true), Value::new_bool(false));
        assert_ne!(Value::none(), Value::new_bool(false));
        assert_eq!(Value::qstr(42), Value::Qstr(42));
        assert!(Value::Qstr(7).is_qstr());
        assert!(Value::SmallInt(-3).is_small_int());
        assert!(Value::None.is_none());
        assert!(!Value::SmallInt(0).is_none());
    }

    #[test]
    fn int_out_of_window_goes_to_heap() {
        let mut arena = GcArena::new();
        let roots = RootSet::new();
        let big = 1i64 << 62;
        let v = Value::int(&mut arena, None, &roots, big);
        assert!(v.is_heap());
        match arena.get(v.as_heap().unwrap()) {
            // 2^62 fits in i64 (just outside the 62-bit small-int window).
            Obj::Int(z) => assert_eq!(z.as_int_checked(), Some(1i64 << 62)),
            other => panic!("expected Int, got {:?}", other),
        }
        let small = Value::int(&mut arena, None, &roots, 7);
        assert_eq!(small, Value::SmallInt(7));
    }

    #[test]
    fn truth_table_primitives() {
        let arena = GcArena::new();
        assert!(!Value::None.is_true(&arena).unwrap());
        assert!(!Value::Bool(false).is_true(&arena).unwrap());
        assert!(Value::Bool(true).is_true(&arena).unwrap());
        assert!(!Value::SmallInt(0).is_true(&arena).unwrap());
        assert!(Value::SmallInt(-1).is_true(&arena).unwrap());
        // Qstr(0) is MP_QSTRnull and Qstr(1) is MP_QSTR_ — the
        // generated table holds "" at both ids (LENGTHS[0] ==
        // LENGTHS[1] == 0), so both are falsy, like bool("") on the
        // reference binary; a non-empty static qstr is truthy.
        // (C6 corrects the old assertion, which was written against
        // the pre-fix arm that returned true for every qstr.)
        assert!(!Value::Qstr(0).is_true(&arena).unwrap());
        assert!(!Value::Qstr(1).is_true(&arena).unwrap());
        let int_id = crate::qstr_table::QSTR_STRINGS
            .iter()
            .position(|s| *s == b"int")
            .unwrap() as u32;
        assert!(Value::Qstr(int_id).is_true(&arena).unwrap());
    }
}
