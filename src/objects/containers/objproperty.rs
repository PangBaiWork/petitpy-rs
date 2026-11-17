//! property (C9c: `src/objects/containers/objproperty.zig`, 135 lines, 0 test blocks).
//!
//! `property(fget, fset, fdel, doc)` — 4 args via
//! `mp_arg_parse_all_kw_array`. The first three are positional-only
//! (their `qst` is the empty qstr, `MP_QSTR_`); `doc` is kw-able;
//! all default to `None`; `doc` is silently discarded. This file
//! is the OBJECT only — the descriptor protocol (`__get__` /
//! `__set__` / `__delete__` on attribute access) is C12's wiring;
//! `property_proxy` hands C12 the `(getter, setter, deleter)`
//! triple (`mp_obj_property_get`).
//!
//! # The arg parse (vendor `py/argcheck.c:80-134`)
//!
//! Positionals fill slots in order; a slot past the positionals is
//! filled from the kw map by qstr (the empty qstr for the first
//! three slots — the lookup runs PER SLOT, so one empty-qstr kw
//! entry fills all three: `property(**{'': 9})` has getter,
//! setter AND deleter all 9, verified on the reference binary via
//! `C().x`, `c.x = 5`, `del c.x` all raising "object not
//! callable"); anything left over defaults to `None`. Two checks at
//! both raising `TypeError: argument num/types mismatch` (TERSE):
//! extra positionals (`pos_found < n_pos`) and unknown/duplicate
//! kwargs (`kws_found < kws->used` — a kw whose slot was already
//! filled positionally is never counted, so a duplicate is a
//! mismatch too). Probes: `property(1,2,3,4,5)` → mismatch (extra
//! positional); `property(1,2,3,4,doc='x',fget=1)` → mismatch
//! (`fget` is not a named arg); `property(fget=1)` → mismatch.

use super::objdict::ARG_NUM_TYPES_MISMATCH;
use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::obj::{obj_get_type, ObjError};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::BuiltinFn;
use crate::objects::value::{GcRef, Obj, PropertyData, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

/// Errors from property operations: only the arg-count mismatch
/// (plus the slot-boundary signals). Mirrors `DictError` (C9a) in
/// shape; C13 constructs the exception object from the rich error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyError {
    /// `TypeError(msg)` — arg-count / unknown-kw mismatch.
    TypeError(&'static str),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised (data dropped at the boundary).
    PyRaise,
}

impl PropertyError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            PropertyError::TypeError(_) => "TypeError",
            PropertyError::OutOfMemory => "MemoryError",
            PropertyError::PyRaise => "",
        }
    }

    /// The exception message.
    pub fn msg(&self) -> Option<&'static str> {
        match self {
            PropertyError::TypeError(m) => Some(m),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — see the
    /// enum docs).
    pub fn to_pyerror(self) -> PyError {
        match self {
            PropertyError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for PropertyError {
    fn raise_name(&self) -> &'static str {
        self.exc_name()
    }
    fn raise_arg(&self) -> Option<Value> {
        None
    }
    fn raise_message(&self, _pool: &RuntimeQstr) -> Option<String> {
        self.msg().map(|m| m.to_string())
    }
    fn raise_signal(&self) -> PyError {
        self.to_pyerror()
    }
}

impl From<ObjError> for PropertyError {
    fn from(e: ObjError) -> PropertyError {
        match e {
            ObjError::OutOfMemory => PropertyError::OutOfMemory,
            _ => PropertyError::PyRaise,
        }
    }
}

impl From<PyError> for PropertyError {
    fn from(e: PyError) -> PropertyError {
        match e {
            PyError::OutOfMemory => PropertyError::OutOfMemory,
            PyError::PyRaise => PropertyError::PyRaise,
        }
    }
}

/// Extract a cloned `PropertyData` from a heap property value
/// (panics on any other value — the slot/type-data contract).
fn property_data_of(arena: &GcArena, v: Value) -> PropertyData {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Property(p) => p.clone(),
            _ => panic!("property op on a non-property value"),
        },
        _ => panic!("property op on a non-heap value"),
    }
}

/// Find a kw value by qstr id in the kw pairs (`args[n_pos..]`,
/// interleaved key/value — the C9a convention). A non-`Qstr` key
/// can never match an allowed qstr.
fn find_kw(args: &[Value], n_pos: usize, n_kw: usize, qst: u32) -> Option<Value> {
    for i in 0..n_kw {
        let key = args[n_pos + 2 * i];
        let value = args[n_pos + 2 * i + 1];
        if key == Value::Qstr(qst) {
            return Some(value);
        }
    }
    None
}

/// `property_make_new` (`property(fget, fset, fdel, doc)`,
/// objproperty.zig:48-61): the 4-slot `mp_arg_parse_all_kw_array`
/// described in the module docs. `doc` is silently discarded.
/// Defaults are `None`.
pub fn property_make_new(
    ctx: &mut CallCtx,
    type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, PropertyError> {
    let n_pos = args.len() - n_kw * 2;
    // `MP_QSTR_` is the empty qstr (qstr id 1, the first slot in
    // qstrdefs) — the positional-only marker for the first three
    // slots (objproperty.zig:36-40).
    let empty = static_qstr_id(b"").expect("MP_QSTR_ is the empty qstr");
    let doc = static_qstr_id(b"doc").expect("MP_QSTR_doc is in the static table");
    let mut vals: [Value; 4] = [Value::None; 4];
    let mut kws_found = 0;
    for i in 0..4 {
        if i < n_pos {
            vals[i] = args[i];
        } else {
            let qst = if i < 3 { empty } else { doc };
            match find_kw(args, n_pos, n_kw, qst) {
                Some(v) => {
                    kws_found += 1;
                    vals[i] = v;
                }
                None => vals[i] = Value::None,
            }
        }
    }
    // Extra positionals (`pos_found < n_pos`) or unknown/duplicate
    // kwargs (`kws_found < kws->used`) — both are the terse mismatch.
    if n_pos > 4 || kws_found < n_kw {
        return Err(PropertyError::TypeError(ARG_NUM_TYPES_MISMATCH));
    }
    Ok(Value::Heap(ctx.alloc(
        Some(type_),
        Obj::Property(PropertyData {
            getter: vals[0],
            setter: vals[1],
            deleter: vals[2],
        }),
    )))
}

/// `cloneWith` (objproperty.zig:63-68): clone the property with one
/// proxy slot replaced. The clone keeps self's header type — the
/// Zig copies the whole struct (`p2.* = *self`), including
/// `base.type`, so a subclass instance clones as the subclass
/// (this is `m_malloc`, not the typed `mp_obj_malloc_helper`).
fn clone_with(ctx: &mut CallCtx, self_v: Value, slot: usize, value: Value) -> PyResult<Value> {
    let mut data = property_data_of(ctx.arena, self_v);
    match slot {
        0 => data.getter = value,
        1 => data.setter = value,
        2 => data.deleter = value,
        _ => unreachable!("clone_with slot is 0, 1 or 2"),
    }
    let header_type = obj_get_type(ctx.arena, ctx.types, self_v);
    Ok(Value::Heap(ctx.alloc(header_type, Obj::Property(data))))
}

/// `property.getter(getter)` (objproperty.zig:70-72): clone with
/// the getter replaced. Fixed arity 2 (fun_builtin_2).
pub fn property_getter_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() != 2 {
        return Err(ctx.record_typed(PropertyError::TypeError(ARG_NUM_TYPES_MISMATCH)));
    }
    clone_with(ctx, args[0], 0, args[1])
}

/// `property.setter(setter)` (objproperty.zig:73-75): clone with
/// the setter replaced. Fixed arity 2.
pub fn property_setter_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() != 2 {
        return Err(ctx.record_typed(PropertyError::TypeError(ARG_NUM_TYPES_MISMATCH)));
    }
    clone_with(ctx, args[0], 1, args[1])
}

/// `property.deleter(deleter)` (objproperty.zig:76-78): clone with
/// the deleter replaced. Fixed arity 2.
pub fn property_deleter_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    if args.len() != 2 {
        return Err(ctx.record_typed(PropertyError::TypeError(ARG_NUM_TYPES_MISMATCH)));
    }
    clone_with(ctx, args[0], 2, args[1])
}

/// `mp_obj_property_get` (objproperty.zig:139-141): the
/// `(getter, setter, deleter)` triple, for C12's descriptor wiring.
pub fn property_proxy(arena: &GcArena, self_v: Value) -> (Value, Value, Value) {
    let data = property_data_of(arena, self_v);
    (data.getter, data.setter, data.deleter)
}

/// The `property` locals dict (objproperty.zig:104-108), C8b's
/// `STR_METHODS` shape: C12 builds the dict from this table.
/// Zig order: getter, setter, deleter.
pub static PROPERTY_METHODS: &[(&str, BuiltinFn)] = &[
    ("getter", property_getter_builtin),
    ("setter", property_setter_builtin),
    ("deleter", property_deleter_builtin),
];

/// Look a property method up by name (C8b's `lookup_str_method` shape).
pub fn lookup_property_method(name: &str) -> Option<BuiltinFn> {
    PROPERTY_METHODS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| *f)
}

/// `mp_type_property` ROM data (objproperty.zig:120-137): flags 0;
/// slots make_new + locals_dict ONLY — no print/unary/binary/call/
/// getiter/subscr/attr slots. `make_new` is the type-call path,
/// wired by C12, not a `TypeData` slot (the `dict_type_data`
/// precedent). Without a print slot the object prints via C6's
/// fallback (`<property>`, verified on the reference binary).
pub fn property_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"property").expect("MP_QSTR_property is in the static table"),
        flags: 0x0000,
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
        make_new: Some(crate::objects::objtype::make_new::property_make_new_slot),
        bases: None,
        namedtuple_fields: None,
    }
}
