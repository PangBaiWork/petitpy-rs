//! Class lookup, member conversion, C3 MRO, special accessors
//! (`objtype.zig:840-1040` + helpers at :60-150, :196-277).
//!
//! * `class_lookup` — `npy_ty_class_lookup` (:840): the MRO
//!   walker. Steps per type: (1) native-slot fast path, (2)
//!   locals-dict search, (3) native `load_attr` fallback
//!   (`lookupIn`, :901).
//! * `convert_member_lookup` — `mp_convert_member_lookup`
//!   (`runtime.zig:1137`): binds-self / builtin-fun /
//!   staticmethod / classmethod dispatch. Fully portable, no
//!   seams.
//! * `mro_of` / `c3_mro` — `mroOf` / `c3Mro` (:960-1040).
//! * `attr_value_is_special_accessor` /
//!   `scan_special_accessors_flag` (:172-227).
//! * `type_locals_dict_readonly` / `instance_dict_readonly` /
//!   `type_bases_tuple` (:232-277).
//! * `unary_op_method_name` / `binary_op_method_name` — the
//!   `mp_unary_op_method_name` / `mp_binary_op_method_name`
//!   tables (:747-772), as matches (the Zig's tables are `[u8; N]`
//!   indexed by the op discriminant; the Rust op enums carry the
//!   same vendor discriminants, so a match is the idiomatic
//!   equivalent). `None` = the table's 0 entry.
//!
//! The Zig's slot-index byte offsets (`SLOT_OFFSET_*`, :277-288)
//! have no Rust equivalent — `TypeData` is a struct with named
//! fields, so the lookup takes a `SlotKind` instead of an offset.
//! `SlotKind::Protocol` never hits the fast path: the Rust
//! `TypeData` has no protocol field (no lane has needed it; the
//! Zig's protocol slot is a bitmask, not a fn).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::{dict_copy, dict_get_value};
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::{obj_get_type, type_data, FLAG_INSTANCE_TYPE};
use crate::objects::slots::{BinaryOp, UnaryOp};
use crate::objects::value::{GcRef, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

use super::errors::ObjTypeError;
use super::seams::VmSeams;
use super::subclass::instance_subobj0;
use super::subclass::is_exact_type;

/// C3's bound on bases (`objtype.zig:992`, "ponytail: a fixed
/// bound on bases").
pub const MAX_BASES: usize = 15;

/// The `FLAG_HAS_SPECIAL_ACCESSORS` bit (re-exported for the
/// scan's callers).
pub use crate::objects::obj::FLAG_HAS_SPECIAL_ACCESSORS;

/// A native slot, by kind (the Zig's `slot_index_*` list).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    MakeNew,
    Print,
    Call,
    UnaryOp,
    BinaryOp,
    Attr,
    Subscr,
    Iter,
    Buffer,
    Protocol,
    Parent,
    LocalsDict,
}

/// What `class_lookup` found.
///
/// `Member(fun, bound_self)` is the `dest` pair after
/// `convert_member_lookup`: `bound_self == None` is a plain
/// attribute (dest[1] == NULL), `Some(self)` is a bound method.
/// `NativeSlot(kind)` is the Zig's `MP_OBJ_SENTINEL` — the type
/// has a native slot for the requested attribute, and the caller
/// invokes that slot directly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LookupHit {
    Member(Value, Option<Value>),
    NativeSlot(SlotKind),
}

/// Does the type carry this slot (`typeHasSlotByOffset` — the
/// Zig reads the slot-index byte; Rust reads the named field)?
/// `Protocol` is always false: the Rust `TypeData` has no
/// protocol field.
pub fn has_slot(t: &TypeData, kind: SlotKind) -> bool {
    match kind {
        SlotKind::MakeNew => t.make_new.is_some(),
        SlotKind::Print => t.print.is_some(),
        SlotKind::Call => t.call.is_some(),
        SlotKind::UnaryOp => t.unary_op.is_some(),
        SlotKind::BinaryOp => t.binary_op.is_some(),
        SlotKind::Attr => t.attr.is_some(),
        SlotKind::Subscr => t.subscr.is_some(),
        SlotKind::Iter => t.getiter.is_some(),
        SlotKind::Buffer => t.buffer.is_some(),
        SlotKind::Protocol => false,
        SlotKind::Parent => t.base.is_some() || t.bases.is_some(),
        SlotKind::LocalsDict => true,
    }
}

/// `typeIsNative` (`objtype.zig:78`): a native (non-instance)
/// type — its flags lack `INSTANCE_TYPE`.
pub fn is_native_type(t: &TypeData) -> bool {
    t.flags & FLAG_INSTANCE_TYPE == 0
}

/// `typeFlagIterIsStream`: both iter flag bits set.
pub fn iter_is_stream(t: &TypeData) -> bool {
    use crate::objects::obj::FLAG_ITER_IS_STREAM;
    t.flags & FLAG_ITER_IS_STREAM == FLAG_ITER_IS_STREAM
}

/// `typeHasParent`: a `base` or a `bases` tuple.
pub fn has_parent(t: &TypeData) -> bool {
    t.base.is_some() || t.bases.is_some()
}

/// The parent slot as a value: the `bases` tuple for an MI type,
/// else the single `base` type, else `None`.
pub fn type_parent_value(t: &TypeData) -> Option<Value> {
    if let Some(bases) = t.bases {
        Some(Value::Heap(bases))
    } else {
        t.base.map(Value::Heap)
    }
}

/// `parentIsTuple`: the parent slot holds a tuple of types.
pub fn parent_is_tuple(t: &TypeData) -> bool {
    t.bases.is_some()
}

/// `mp_convert_member_lookup` (`runtime.zig:1137-1174`).
///
/// `self_` is the object the member was looked up on (`None` =
/// `MP_OBJ_NULL`, the class-lookup case); `type_` is the type the
/// member was found in. Returns `(fun, bound_self)` — the `dest`
/// pair: `bound_self == None` means a plain attribute.
pub fn convert_member_lookup(
    ctx: &mut CallCtx,
    self_: Option<Value>,
    type_: GcRef,
    member: Value,
) -> (Value, Option<Value>) {
    use crate::objects::obj::{FLAG_BINDS_SELF, FLAG_BUILTIN_FUN};
    let Value::Heap(m) = member else {
        return (member, None);
    };
    // Payload-based function check: `FunBuiltin` / `FunBc`
    // values are allocated with header `None` across the tree
    // (the `fun_builtin_N` type objects do not exist yet — the
    // bootstrap pattern), so the header-type read below cannot
    // see their `BINDS_SELF` flag. The payload IS the identity
    // here (the `is_type_value` precedent in `subclass.rs`): a
    // builtin function binds `self` unless it was found in an
    // instance type (where the Zig makes it a staticmethod —
    // probed: a `FunBuiltin` `__init__` on a user type is
    // unbound); a bytecode function always binds `self`. This
    // is what makes `object().__init__` a bound method (probed
    // on the reference binary) instead of a bare builtin.
    if matches!(ctx.arena.get(m), Obj::FunBuiltin(_) | Obj::FunBc(_)) {
        if matches!(ctx.arena.get(m), Obj::FunBuiltin(_)) {
            match type_data(ctx.arena, type_) {
                Some(tdata) if tdata.flags & FLAG_INSTANCE_TYPE != 0 => return (member, None),
                _ => return (member, self_),
            }
        }
        return (member, self_);
    }
    let Some(mtype) = obj_get_type(ctx.arena, ctx.types, member) else {
        return (member, None);
    };
    let Some(mdata) = type_data(ctx.arena, mtype) else {
        return (member, None);
    };
    if mdata.flags & FLAG_BINDS_SELF != 0 {
        if mdata.flags & FLAG_BUILTIN_FUN != 0 {
            // Built-in fn on a user type → staticmethod (no bind);
            // on a native type → bind `self`.
            match type_data(ctx.arena, type_) {
                Some(tdata) if tdata.flags & FLAG_INSTANCE_TYPE != 0 => (member, None),
                _ => (member, self_),
            }
        } else {
            (member, self_)
        }
    } else if mtype == ctx.types.staticmethod {
        let Obj::StaticClassMethod(d) = ctx.arena.get(m) else {
            return (member, None);
        };
        (d.fun, None)
    } else if mtype == ctx.types.classmethod {
        let Obj::StaticClassMethod(d) = ctx.arena.get(m) else {
            return (member, None);
        };
        let mut t = type_;
        if let Some(self_v) = self_ {
            if let Some(st) = obj_get_type(ctx.arena, ctx.types, self_v) {
                t = st;
                if st == ctx.types.type_ {
                    if let Value::Heap(sr) = self_v {
                        t = sr;
                    }
                }
            }
        }
        (d.fun, Some(Value::Heap(t)))
    } else {
        (member, None)
    }
}

/// Steps 1-3 of a lookup, in type `t` alone (`lookupIn`,
/// `objtype.zig:901-950`). `start_type` is the type the walk
/// started from (the is-type branch converts against the
/// ORIGINAL type, not the base the member was found in).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lookup_in(
    ctx: &mut CallCtx,
    obj: Option<Value>,
    attr: u32,
    slot: Option<SlotKind>,
    is_type: bool,
    t: GcRef,
    start_type: GcRef,
    seams: &mut VmSeams,
) -> Option<LookupHit> {
    let Some(tdata) = type_data(ctx.arena, t) else {
        return None;
    };
    // Copy the fields out: the steps below take `&mut CallCtx`,
    // which conflicts with the `TypeData` borrow.
    let native = is_native_type(tdata);
    let dict = tdata.dict;
    let stream = iter_is_stream(tdata);
    // 1. Native-type slot fast path.
    if let Some(kind) = slot {
        if native && (has_slot(tdata, kind) || (kind == SlotKind::Iter && stream)) {
            return Some(LookupHit::NativeSlot(kind));
        }
    }
    // 2. locals_dict search.
    let key = Value::Qstr(attr);
    if let Ok(member) = dict_get_value(ctx, Value::Heap(dict), key) {
        let (fun, bound) = if is_type {
            // Class lookup: `obj` is the class itself.
            let class_ref = match obj {
                Some(Value::Heap(r)) => r,
                _ => start_type,
            };
            convert_member_lookup(ctx, None, class_ref, member)
        } else {
            let obj_obj = match obj {
                Some(v) if native && t != ctx.types.object => {
                    // Native base class: the lookup applies to
                    // the native subobj — but only when subobj[0]
                    // IS an object of type `t` (a user subclass's
                    // native payload, e.g. the list inside a
                    // `class L(list)` — the Zig's subobj[0] IS
                    // the native object there). A direct
                    // Rust-modeled native instance (socket:
                    // subobj = [fd, family, type, proto]) carries
                    // raw fields, not a native object — its
                    // methods take the instance itself (the Zig
                    // has no instance there at all: the socket
                    // IS the native object). E2: binding socket
                    // methods to the fd int raised "expected a
                    // socket" on every call.
                    match instance_subobj0(ctx.arena, v) {
                        Some(sub0)
                            if obj_get_type(ctx.arena, ctx.types, sub0) == Some(t) =>
                        {
                            Some(sub0)
                        }
                        _ => Some(v),
                    }
                }
                Some(v) => Some(v),
                None => None,
            };
            convert_member_lookup(ctx, obj_obj, t, member)
        };
        return Some(LookupHit::Member(fun, bound));
    }
    // 3. Native load_attr fallback for attributes not in
    // locals_dict. Same guard as step 2 (E2): only when
    // subobj[0] IS an object of type `t` — a socket's fd int
    // must not leak int methods onto the socket.
    if obj.is_some() && !is_type && native && t != ctx.types.object {
        if let Some(sub) = obj.and_then(|v| instance_subobj0(ctx.arena, v)) {
            if obj_get_type(ctx.arena, ctx.types, sub) == Some(t) {
                if let Some((fun, bound)) = seams.load_maybe(ctx, sub, attr) {
                    return Some(LookupHit::Member(fun, bound));
                }
            }
        }
    }
    None
}

/// `npy_ty_class_lookup` (`objtype.zig:840-900`): the MRO walker.
/// `obj` is the object being looked up on (`None` when the
/// caller has no object — the Zig's NULL `obj_in`); `is_type`
/// marks a class lookup (the member is converted against the
/// class itself). Returns `None` when the attribute is nowhere.
pub fn class_lookup(
    ctx: &mut CallCtx,
    obj: Option<Value>,
    attr: u32,
    slot: Option<SlotKind>,
    is_type: bool,
    type_: GcRef,
    seams: &mut VmSeams,
) -> Option<LookupHit> {
    let object_type = ctx.types.object;
    let mut t = type_;
    loop {
        if let Some(hit) = lookup_in(ctx, obj, attr, slot, is_type, t, type_, seams) {
            return Some(hit);
        }
        // 4. Parent walk.
        let Some(tdata) = type_data(ctx.arena, t) else {
            return None;
        };
        // Copy the fields out: the branches below take
        // `&mut CallCtx`, which conflicts with the borrow.
        let parented = has_parent(tdata);
        let mi = parent_is_tuple(tdata);
        let bases = tdata.bases;
        let base = tdata.base;
        if !parented {
            return None;
        }
        // Full: the C3 order when the parent is a tuple and a
        // cached MRO exists.
        if mi {
            if let Some(m) = cached_mro(ctx, t) {
                for u in m.iter().skip(1) {
                    let Value::Heap(ut) = u else {
                        continue;
                    };
                    if *ut == object_type {
                        continue;
                    }
                    if let Some(hit) =
                        lookup_in(ctx, obj, attr, slot, is_type, *ut, type_, seams)
                    {
                        return Some(hit);
                    }
                }
                return None;
            }
            // MI recursion: all but the last base, tail into
            // the last.
            let Some(bases) = bases else {
                return None;
            };
            // Clone the items: the recursion below takes
            // `&mut CallCtx`.
            let items = match ctx.arena.get(bases) {
                Obj::Tuple(items) => items.clone(),
                _ => return None,
            };
            if items.is_empty() {
                return None;
            }
            for base in &items[..items.len() - 1] {
                let Value::Heap(bt) = base else {
                    continue;
                };
                if *bt == object_type {
                    continue;
                }
                if let Some(hit) = class_lookup(ctx, obj, attr, slot, is_type, *bt, seams) {
                    return Some(hit);
                }
            }
            let Some(Value::Heap(last)) = items.last() else {
                return None;
            };
            if *last == object_type {
                return None;
            }
            t = *last;
            continue;
        }
        // Single parent.
        let Some(parent) = base else {
            return None;
        };
        if parent == object_type {
            return None;
        }
        t = parent;
    }
}

/// `cachedMro` (`objtype.zig:961-966`): the `__mro__` tuple in
/// the type's locals dict, when the entry is exactly a tuple.
/// NOTE: the Zig does NOT gate this on the parent being a tuple
/// (an earlier draft did — wrongly: a single-parent type
/// descending from an MI type inherits a cached `__mro__`, and
/// `mroOf` / `newTypeImpl` both rely on seeing it).
pub fn cached_mro(ctx: &mut CallCtx, t: GcRef) -> Option<Vec<Value>> {
    let tdata = type_data(ctx.arena, t)?;
    let dict = tdata.dict;
    let mro = dict_get_value(ctx, Value::Heap(dict), Value::Qstr(mro_qstr())).ok()?;
    if !is_exact_type(ctx.arena, ctx.types.tuple, mro) {
        return None;
    }
    match mro {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Tuple(items) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `mroOf` (`objtype.zig:969-986`): the type's full linearisation
/// as a fresh `Vec` (the Zig builds it via a list, then a tuple;
/// callers that need the tuple use `mro_tuple`).
pub fn mro_of(ctx: &mut CallCtx, t: GcRef) -> Vec<Value> {
    let object = Value::Heap(ctx.types.object);
    let mut out: Vec<Value> = Vec::new();
    let mut u = t;
    loop {
        if let Some(m) = cached_mro(ctx, u) {
            out.extend(m);
            break;
        }
        out.push(Value::Heap(u));
        if u == ctx.types.object {
            break;
        }
        let Some(tdata) = type_data(ctx.arena, u) else {
            out.push(object);
            break;
        };
        match type_parent_value(tdata) {
            None => {
                out.push(object);
                break;
            }
            Some(Value::Heap(p)) => {
                // A tuple parent means an MI type without a
                // cached MRO — malformed (a well-formed MI type
                // always stores one); the Zig reads the tuple as
                // a type here. Stop at `object` instead.
                if parent_is_tuple(tdata) {
                    let _ = p;
                    out.push(object);
                    break;
                }
                u = p;
            }
            Some(_) => {
                out.push(object);
                break;
            }
        }
    }
    out
}

/// `mroOf` as a fresh tuple value.
pub fn mro_tuple(ctx: &mut CallCtx, t: GcRef) -> Value {
    let items = mro_of(ctx, t);
    let tuple_type = ctx.types.tuple;
    Value::Heap(new_tuple(ctx, tuple_type, &items))
}

/// `c3Mro` (`objtype.zig:990-1038`): merge the bases' MROs plus
/// the bases themselves. `t` is the new type (a `Value` so the
/// caller can pass the freshly allocated type object); `bases`
/// are type refs, validated by the caller.
pub fn c3_mro(
    ctx: &mut CallCtx,
    t: Value,
    bases: &[GcRef],
) -> Result<Value, ObjTypeError> {
    if bases.len() > MAX_BASES {
        return Err(ObjTypeError::TooManyBases);
    }
    let mut seqs: Vec<Vec<Value>> = Vec::with_capacity(bases.len() + 1);
    for b in bases {
        seqs.push(mro_of(ctx, *b));
    }
    seqs.push(bases.iter().map(|b| Value::Heap(*b)).collect());
    let mut out: Vec<Value> = vec![t];
    loop {
        let mut head: Option<Value> = None;
        let mut left = false;
        'find: for sq in &seqs {
            if sq.is_empty() {
                continue;
            }
            left = true;
            for other in &seqs {
                if other.len() > 1 && other[1..].contains(&sq[0]) {
                    continue 'find;
                }
            }
            head = Some(sq[0]);
            break;
        }
        if !left {
            break;
        }
        let Some(h) = head else {
            // Inconsistent MRO: the payload is the base NAME
            // qstr ids, in order (the Zig formats them into the
            // message).
            let names: Vec<u32> = bases
                .iter()
                .map(|b| type_data(ctx.arena, *b).map(|t| t.name).unwrap_or(0))
                .collect();
            return Err(ObjTypeError::MroConflict(names));
        };
        out.push(h);
        for sq in seqs.iter_mut() {
            if !sq.is_empty() && sq[0] == h {
                sq.remove(0);
            }
        }
    }
    let tuple_type = ctx.types.tuple;
    Ok(Value::Heap(new_tuple(ctx, tuple_type, &out)))
}

/// `attrValueIsSpecialAccessor` (`objtype.zig:172-194`): is this
/// `(key, value)` pair a special accessor? `__setattr__` /
/// `__delattr__` keys, property values, and values exposing any
/// of `__get__` / `__set__` / `__delete__` (probed through the
/// `load_maybe` seam — the Zig's `mp_load_method_protected`
/// with `catch_all_exc = true`).
pub fn attr_value_is_special_accessor(
    ctx: &mut CallCtx,
    key: u32,
    value: Value,
    seams: &mut VmSeams,
) -> bool {
    if key == setattr_qstr() || key == delattr_qstr() {
        return true;
    }
    if is_exact_type(ctx.arena, ctx.types.property, value) {
        return true;
    }
    for q in [get_qstr(), set_qstr(), delete_qstr()] {
        if seams.load_maybe(ctx, value, q).is_some() {
            return true;
        }
    }
    false
}

/// `npy_ty_scan_special_accessors_flag` (`objtype.zig:198-227`):
/// scan a locals dict for special accessors. Returns
/// `FLAG_HAS_SPECIAL_ACCESSORS` or 0. Unlike
/// `attr_value_is_special_accessor`, the descriptor probe only
/// fires for values whose type is an INSTANCE type.
pub fn scan_special_accessors_flag(
    ctx: &mut CallCtx,
    locals_dict: Value,
    seams: &mut VmSeams,
) -> u16 {
    let Value::Heap(r) = locals_dict else {
        return 0;
    };
    let Obj::Dict(d) = ctx.arena.get(r) else {
        return 0;
    };
    let entries: Vec<(Option<Value>, Value)> = d
        .map
        .entries
        .iter()
        .map(|e| (e.key, e.value))
        .collect();
    for (key, value) in entries {
        // PROPERTY: any property value flags the type.
        if is_exact_type(ctx.arena, ctx.types.property, value) {
            return FLAG_HAS_SPECIAL_ACCESSORS;
        }
        // DELATTR_SETATTR (full only): `__setattr__` /
        // `__delattr__` keys.
        if let Some(Value::Qstr(k)) = key {
            if k == setattr_qstr() || k == delattr_qstr() {
                return FLAG_HAS_SPECIAL_ACCESSORS;
            }
        }
        // DESCRIPTORS: only USER (instance) types carry `__get__`
        // as a descriptor.
        if let Some(vtype) = obj_get_type(ctx.arena, ctx.types, value) {
            if let Some(vt) = type_data(ctx.arena, vtype) {
                if vt.flags & FLAG_INSTANCE_TYPE != 0
                    && seams.load_maybe(ctx, value, get_qstr()).is_some()
                {
                    return FLAG_HAS_SPECIAL_ACCESSORS;
                }
            }
        }
    }
    0
}

/// `typeLocalsDictReadonly` (`objtype.zig:232-247`): the type's
/// locals dict — itself when already fixed, else a copy marked
/// fixed. E2: the type's locals dict is allocated with a `None`
/// header at ROM bootstrap (the dict type does not exist yet
/// when the first type dicts are built) — but the Zig's locals
/// dict IS a dict object (`type(int.__dict__)` is `dict`, it
/// prints and subscripts). When the dict is first exposed as a
/// value, give it the dict header in place: the fixed case
/// keeps its `is` identity (`int.__dict__ is int.__dict__`,
/// probed), the copy case gets its header on the copy.
pub fn type_locals_dict_readonly(ctx: &mut CallCtx, type_: GcRef) -> PyResult<Value> {
    let Some(tdata) = type_data(ctx.arena, type_) else {
        return Err(PyError::PyRaise);
    };
    let dict = Value::Heap(tdata.dict);
    let Value::Heap(r) = dict else {
        return Err(PyError::PyRaise);
    };
    let is_fixed = match ctx.arena.get(r) {
        Obj::Dict(d) => d.map.is_fixed,
        _ => return Err(PyError::PyRaise),
    };
    let dict_type = ctx.types.dict;
    if ctx.arena.header(r).type_.is_none() {
        ctx.arena.set_header_type(r, Some(dict_type));
    }
    if is_fixed {
        return Ok(dict);
    }
    let copy = dict_copy(ctx, dict).map_err(dict_err_to_py)?;
    let Value::Heap(cr) = copy else {
        return Err(PyError::PyRaise);
    };
    if ctx.arena.header(cr).type_.is_none() {
        ctx.arena.set_header_type(cr, Some(dict_type));
    }
    if let Obj::Dict(d) = ctx.arena.get_mut(cr) {
        d.map.is_fixed = true;
    }
    Ok(copy)
}

/// `instanceDictReadonly` (`objtype.zig:249-260`): a fixed copy of
/// the instance's members map (a fresh dict object whose header
/// type is `dict`).
pub fn instance_dict_readonly(ctx: &mut CallCtx, inst: Value) -> PyResult<Value> {
    let Value::Heap(r) = inst else {
        return Err(PyError::PyRaise);
    };
    let Obj::Instance(data) = ctx.arena.get(r) else {
        return Err(PyError::PyRaise);
    };
    let members = data.members.clone();
    let dict_type = ctx.types.dict;
    let tmp = ctx.alloc(
        Some(dict_type),
        Obj::Dict(crate::objects::value::DictData { map: members }),
    );
    let copy = dict_copy(ctx, Value::Heap(tmp)).map_err(dict_err_to_py)?;
    let Value::Heap(cr) = copy else {
        return Err(PyError::PyRaise);
    };
    if let Obj::Dict(d) = ctx.arena.get_mut(cr) {
        d.map.is_fixed = true;
    }
    Ok(copy)
}

/// `typeBasesTuple` (`objtype.zig:262-277`): `object` → the empty
/// tuple; an MI parent → the bases tuple itself; else a fresh
/// 1-tuple holding the parent (`object` when parentless).
pub fn type_bases_tuple(ctx: &mut CallCtx, t: GcRef) -> Value {
    if t == ctx.types.object {
        return Value::Heap(ctx.types.empty_tuple);
    }
    let parent = match type_data(ctx.arena, t) {
        Some(td) => type_parent_value(td),
        None => None,
    };
    let parent_obj = match parent {
        Some(v) => v,
        None => Value::Heap(ctx.types.object),
    };
    if is_exact_type(ctx.arena, ctx.types.tuple, parent_obj) {
        return parent_obj;
    }
    let tuple_type = ctx.types.tuple;
    Value::Heap(new_tuple(ctx, tuple_type, &[parent_obj]))
}

fn dict_err_to_py(e: crate::objects::containers::objdict::DictError) -> PyError {
    use crate::objects::containers::objdict::DictError;
    match e {
        DictError::OutOfMemory => PyError::OutOfMemory,
        _ => PyError::PyRaise,
    }
}

/// `mp_unary_op_method_name` (`objtype.zig:747-769`). `None` is
/// the table's 0 entry: `NOT` has no method, and `SIZEOF` is
/// off — `SYS_GETSIZEOF` is disabled under ROM_LEVEL_MINIMUM
/// and `__sizeof__` is not a static qstr. `COMPLEX_MAYBE` IS
/// populated (full config; `__complex__` is QDEF0 257).
pub fn unary_op_method_name(op: UnaryOp) -> Option<u32> {
    match op {
        UnaryOp::Bool => Some(qstr(b"__bool__")),
        UnaryOp::Len => Some(qstr(b"__len__")),
        UnaryOp::Hash => Some(qstr(b"__hash__")),
        UnaryOp::IntMaybe => Some(qstr(b"__int__")),
        UnaryOp::Positive => Some(qstr(b"__pos__")),
        UnaryOp::Negative => Some(qstr(b"__neg__")),
        UnaryOp::Invert => Some(qstr(b"__invert__")),
        UnaryOp::Abs => Some(qstr(b"__abs__")),
        UnaryOp::FloatMaybe => Some(qstr(b"__float__")),
        UnaryOp::ComplexMaybe => Some(qstr(b"__complex__")),
        UnaryOp::Not | UnaryOp::Sizeof => None,
    }
}

/// `mp_binary_op_method_name` (`objtype.zig:772-837`). `IN` is
/// converted to `CONTAINS` by the runtime (no table entry);
/// `IS` / `EXCEPTION_MATCH` have no method name.
pub fn binary_op_method_name(op: BinaryOp) -> Option<u32> {
    match op {
        BinaryOp::Less => Some(qstr(b"__lt__")),
        BinaryOp::More => Some(qstr(b"__gt__")),
        BinaryOp::Equal => Some(qstr(b"__eq__")),
        BinaryOp::LessEqual => Some(qstr(b"__le__")),
        BinaryOp::MoreEqual => Some(qstr(b"__ge__")),
        BinaryOp::NotEqual => Some(qstr(b"__ne__")),
        BinaryOp::Contains => Some(qstr(b"__contains__")),
        BinaryOp::InplaceAdd => Some(qstr(b"__iadd__")),
        BinaryOp::InplaceSubtract => Some(qstr(b"__isub__")),
        BinaryOp::InplaceMultiply => Some(qstr(b"__imul__")),
        BinaryOp::InplaceMatMultiply => Some(qstr(b"__imatmul__")),
        BinaryOp::InplaceFloorDivide => Some(qstr(b"__ifloordiv__")),
        BinaryOp::InplaceTrueDivide => Some(qstr(b"__itruediv__")),
        BinaryOp::InplaceModulo => Some(qstr(b"__imod__")),
        BinaryOp::InplacePower => Some(qstr(b"__ipow__")),
        BinaryOp::InplaceOr => Some(qstr(b"__ior__")),
        BinaryOp::InplaceXor => Some(qstr(b"__ixor__")),
        BinaryOp::InplaceAnd => Some(qstr(b"__iand__")),
        BinaryOp::InplaceLshift => Some(qstr(b"__ilshift__")),
        BinaryOp::InplaceRshift => Some(qstr(b"__irshift__")),
        BinaryOp::Add => Some(qstr(b"__add__")),
        BinaryOp::Subtract => Some(qstr(b"__sub__")),
        BinaryOp::Multiply => Some(qstr(b"__mul__")),
        BinaryOp::MatMultiply => Some(qstr(b"__matmul__")),
        BinaryOp::FloorDivide => Some(qstr(b"__floordiv__")),
        BinaryOp::TrueDivide => Some(qstr(b"__truediv__")),
        BinaryOp::Modulo => Some(qstr(b"__mod__")),
        BinaryOp::Divmod => Some(qstr(b"__divmod__")),
        BinaryOp::Power => Some(qstr(b"__pow__")),
        BinaryOp::Or => Some(qstr(b"__or__")),
        BinaryOp::Xor => Some(qstr(b"__xor__")),
        BinaryOp::And => Some(qstr(b"__and__")),
        BinaryOp::Lshift => Some(qstr(b"__lshift__")),
        BinaryOp::Rshift => Some(qstr(b"__rshift__")),
        BinaryOp::ReverseAdd => Some(qstr(b"__radd__")),
        BinaryOp::ReverseSubtract => Some(qstr(b"__rsub__")),
        BinaryOp::ReverseMultiply => Some(qstr(b"__rmul__")),
        BinaryOp::ReverseMatMultiply => Some(qstr(b"__rmatmul__")),
        BinaryOp::ReverseFloorDivide => Some(qstr(b"__rfloordiv__")),
        BinaryOp::ReverseTrueDivide => Some(qstr(b"__rtruediv__")),
        BinaryOp::ReverseModulo => Some(qstr(b"__rmod__")),
        BinaryOp::ReversePower => Some(qstr(b"__rpow__")),
        BinaryOp::ReverseOr => Some(qstr(b"__ror__")),
        BinaryOp::ReverseXor => Some(qstr(b"__rxor__")),
        BinaryOp::ReverseAnd => Some(qstr(b"__rand__")),
        BinaryOp::ReverseLshift => Some(qstr(b"__rlshift__")),
        BinaryOp::ReverseRshift => Some(qstr(b"__rrshift__")),
        BinaryOp::In | BinaryOp::Is | BinaryOp::ExceptionMatch | BinaryOp::NotIn | BinaryOp::IsNot => None,
    }
}

/// Static qstr lookup (the C7 helper). All names used here are
/// static qstrs — a missing one is a port bug, not a lookup miss.
fn qstr(name: &[u8]) -> u32 {
    crate::objects::primitives::ops::static_qstr_id(name)
        .expect("objtype dunder qstr is in the static table")
}

fn mro_qstr() -> u32 {
    qstr(b"__mro__")
}

fn setattr_qstr() -> u32 {
    qstr(b"__setattr__")
}

fn delattr_qstr() -> u32 {
    qstr(b"__delattr__")
}

fn get_qstr() -> u32 {
    qstr(b"__get__")
}

fn set_qstr() -> u32 {
    qstr(b"__set__")
}

fn delete_qstr() -> u32 {
    qstr(b"__delete__")
}
