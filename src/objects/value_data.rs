//! Value payload structs (C0 interface, split out of `value.rs` by D2).
//!
//! Every `Obj` variant's payload lives here; `value.rs` re-exports
//! them so `crate::objects::value::TypeData` (and friends) keep
//! working unchanged. Payload structs are lane-owned and may evolve;
//! the `Obj` enum itself (in `value.rs`) is frozen.

use crate::objects::containers::map_set::MpSet;
use crate::objects::slots::{
    AttrFn, BinaryOpFn, BufferFn, BuiltinFn, BuiltinKwFn, CallFn, GetIterFn, MakeNewFn,
    PrintFn, StreamP, SubscrFn, UnaryOpFn,
};
use crate::objects::value::{GcRef, Value};
use crate::vendor::re15::{ByteProg, Sub};

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

/// Compiled regex object (D5 owns; `extmod/modre.c`
/// `mp_obj_re_t`). `prog` is the RE15 bytecode program
/// (RE15 lane: `crate::vendor::re15::ByteProg`); `prog.sub`
/// is the capture-group count. Leaf payload — no `Value`
/// children, so `children()` needs no arm for it.
#[derive(Clone, Debug)]
pub struct ReData {
    pub prog: ByteProg,
}

/// Regex match object (D5 owns; `extmod/modre.c`
/// `mp_obj_match_t`). `caps` is the flat capture array
/// (`sub + 1` [`Sub`]s — slot 0 is the whole match, slot `i`
/// is group `i`; `start == None` means the group did not
/// participate); `num_matches` is `caps.len()` (the C's
/// `num_matches`, printed as `<match num=N>`); `subject` is
/// the matched-against value (str or bytes — group results
/// take their type from it, the C's `mp_obj_new_str_of_type`).
/// The subject is a child (GC arm in `value.rs`).
#[derive(Clone, Debug)]
pub struct MatchData {
    pub caps: Vec<Sub>,
    pub num_matches: usize,
    pub subject: Value,
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

/// Attribute delegation table for a module
/// (`mp_module_generic_attr`, `objmodule.zig:64`): parallel
/// (keys, values). Only the `sys` module carries one
/// (`modsys.zig`: keys `[path, ps1, ps2]`, values
/// `[path-list, unset, unset]`); `None` is the C's NULL
/// slot — the attribute is absent (`sys.ps1` →
/// AttributeError, probed). A `Some(Value::None)` is a
/// stored `None` (`sys.path = None` then reads `None`).
/// D2 adds it (the `sys.path` delegation).
#[derive(Clone, Debug)]
pub struct ModuleDelegation {
    pub keys: Vec<u32>,
    pub values: Vec<Option<Value>>,
}

/// Module object (`mp_obj_module_t`). C12 owns.
#[derive(Clone, Debug)]
pub struct ModuleData {
    pub name: u32,
    pub dict: GcRef,
    /// Attribute delegation table (`mp_module_generic_attr`),
    /// `None` for every module except `sys` (D2).
    pub delegation: Option<ModuleDelegation>,
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
/// FILE-backed stream object payload (`mp_obj_npy_file_t`,
/// file_io.zig: the Rust port of the C `{ type, FILE*,
/// is_stdio }` struct). D2 owns.
///
/// `handle` is `Owned` for files `open()` created (an owned
/// `std::fs::File`, closed when the object is collected) or a
/// stdio handle for fds 0/1/2 (never closed by the VM — the
/// C's `is_stdio` guard: `MP_STREAM_CLOSE` on stdio only nulls
/// the pointer, it does not fclose). `fd` is the descriptor
/// number (`fileno()` + the `<io.FileIO 3>` print); `is_text`
/// mirrors the header type (FileIO false, TextIOWrapper true)
/// — the stream protocol's `is_text` bit lives on the TYPE's
/// `StreamP`; this copy is for the print slot. `closed` means
/// the C's `fp == NULL` (close frees the handle but the
/// position is the kernel's, not a field here).
/// `buffer` is the FileIO sibling of a stdio TextIOWrapper
/// (`sys.stdin.buffer` — file_io.zig's `npyTextioAttr` serves
/// `.buffer` only for the three stdio objects, and the sibling
/// is stable: `sys.stdin.buffer is sys.stdin.buffer`, probed
/// 2026-11-18); `None` for files `open()` created. It is a
/// `GcRef` child, traced by `Obj::children()` — without that
/// arm the GC would collect the sibling and `.buffer` would
/// go stale (the sys dict only holds the TextIOWrapper).
#[derive(Clone, Debug)]
pub struct FileData {
    pub handle: FileHandle,
    pub fd: i32,
    pub is_text: bool,
    pub closed: bool,
    pub buffer: Option<GcRef>,
}

/// The open file behind an `Obj::File`. `Owned` holds the
/// `File` in an `Rc<RefCell<..>>` because `Obj` derives
/// `Clone` (a plain `File` is not `Clone`); cloning shares
/// the handle, exactly like aliasing the same file object in
/// Python. Stdio variants are markers: the handles are the
/// process-global `std::io::{stdin,stdout,stderr}`, borrowed
/// per operation — there is no safe `std` API to build an
/// owned `File` from a raw fd (`from_raw_fd` is `unsafe`,
/// and the tree is `unsafe`-free).
#[derive(Clone, Debug)]
pub enum FileHandle {
    Owned(std::rc::Rc<std::cell::RefCell<std::fs::File>>),
    Stdin,
    Stdout,
    Stderr,
    /// The `File` was closed (`fclose`): the `Owned` `Rc`
    /// was dropped at close time, so the descriptor is
    /// already released — this marker only records that the
    /// handle is gone (the `closed` flag is the primary
    /// check; the marker keeps `read`/`write`/`seek` total
    /// if the flag is ever bypassed).
    Closed,
}
