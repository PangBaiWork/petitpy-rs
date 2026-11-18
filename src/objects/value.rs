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
use crate::objects::containers::map_set::set_items;
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
    /// FILE-backed stream payload (D2 owns; file_io.zig).
    /// `FileData` holds no `Value` children, so `children()`
    /// needs no arm for it (leaf, like `StringIo`).
    File(FileData),
    /// Compiled regex object (D5 owns; `extmod/modre.c`).
    /// Leaf payload — no `Value` children.
    Re(ReData),
    /// Regex match object (D5 owns; `extmod/modre.c`). The
    /// subject is a child (GC arm in `children()` below).
    Match(MatchData),
    /// Compiled code object (E1 owns; `builtinevex.zig`'s
    /// `MpObjCodeMinimum`). The payload is the module
    /// function the code wraps; it is a child (GC arm in
    /// `children()` below). `compile()` returns one; `eval` /
    /// `exec` accept it.
    Code(CodeData),
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
            // A FILE has no Rust-heap payload worth counting
            // (the kernel buffer is outside the arena); the
            // header + handle slot is the honest estimate.
            Obj::File(_) => 16,
            Obj::Re(r) => r.prog.bytelen,
            Obj::Match(m) => m.caps.len() * 16,
            // A code object wraps one module function.
            Obj::Code(_) => 16,
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
            Obj::Module(m) => {
                let mut out = vec![Value::Heap(m.dict)];
                // The delegation table's stored values (the
                // `sys.path` list) are reachable only from here
                // — the sys dict does NOT contain them
                // (`'path' in sys.__dict__` is False, probed).
                if let Some(d) = &m.delegation {
                    out.extend(d.values.iter().flatten().copied());
                }
                out
            }
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
            // A match object keeps its subject alive (the
            // subject's bytes back the capture spans).
            Obj::Match(m) => vec![m.subject],
            // A code object keeps its module function alive.
            Obj::Code(c) => vec![Value::Heap(c.module_fun)],
            // A FILE's only child is its `.buffer` sibling
            // (the stdio TextIOWrapper's FileIO; `None` for
            // files `open()` created — file_io.zig's
            // `npyTextioAttr` only serves the three stdio
            // objects). Without this arm the GC would collect
            // the sibling and `.buffer` would go stale.
            Obj::File(f) => f.buffer.map(|r| Value::Heap(r)).into_iter().collect(),
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

// Payload structs live in `value_data.rs` (split out by D2:
// `value.rs` was 886 lines, over the 800 cap); re-exported here
// so every `crate::objects::value::TypeData` path keeps working.
pub use crate::objects::value_data::{
    ArrayData, AttrTupleData, BoundMethData, BytesData, CellData, ClosureData, CodeData,
    DequeData, DictData, ExceptionData, FileData, FileHandle, FunBcData, FunBuiltinData,
    GenState, GeneratorData, InstanceData, IteratorData, ListData, MapData, MapEntry,
    MatchData, ModuleData, ModuleDelegation, ObjHeader, PropertyData, RangeData, ReData,
    SetData, SliceData, StaticClassMethodData, StrData, StringIoData, SuperData,
    TraceFrame, TypeData,
};


/// Unused placeholder for slot types not yet needed; keeps the
/// `PyError` import used in this module's doc examples.
#[allow(dead_code)]
fn _assert_pyresult(_: PyResult<Value>) {}

