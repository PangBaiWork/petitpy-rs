//! The `object` base type (`objobject.zig`, 207 lines, 0 test
//! blocks; the Zig port of vendor `py/objobject.c`).
//!
//! What lives here: `mp_type_object` (`object_type_data`),
//! `object_make_new`, the four locals-dict methods
//! (`__init__` / `__new__` / `__setattr__` / `__delattr__`, as
//! `BuiltinFn` adapters over the core fns), and
//! `object_locals_dict`.
//!
//! Config gates (full config — the only config this port
//! targets): `CPYTHON_COMPAT = 1`, `DELATTR_SETATTR = 1` → the
//! locals dict has all 4 entries (nano has 2 — `__init__` and
//! `__new__` only).
//!
//! Zig-isms ported exactly:
//! - `object_make_new` sig is `(0, 0, false)` — `object(1)` is a
//!   terse mismatch (probed).
//! - `object.__init__(self)` returns None and takes exactly one
//!   arg (`object().__init__(1, 2)` → terse mismatch, probed).
//! - `object.__new__(cls)` requires `cls` to be a type object
//!   AND an instance type (`flags & 0x0200`) — `object` itself
//!   is NOT an instance type (`mp_type_object` flags are
//!   `0x0000`), so `object.__new__(object)` and
//!   `object.__new__(int)` both raise `TypeError: arg must be
//!   user-type` (probed). `__new__` is a staticmethod in the
//!   Zig (`MpObjStaticClassMethod` wrapping the fun object) —
//!   the locals dict stores it wrapped (E2g), so
//!   `super(A, A).__new__(A)` passes 1 arg, not 2.
//! - `__setattr__` / `__delattr__` require an instance type
//!   first (`object().__setattr__('a', 1)` → `TypeError: arg
//!   must be user-type`, probed), then a str attr (else a BARE
//!   TypeError), then the members-map store / remove
//!   (`__delattr__` on a missing attr → `AttributeError: no
//!   such attribute`). Both write the members map DIRECTLY —
//!   no descriptor, no `__setattr__` hook (E2g).
//! - `object` has no base (`object.__bases__ == ()`, probed) and
//!   its MRO is `[self]`.

use crate::memory::gc::{CallCtx, GcArena, TypedRaise};
use crate::objects::objtype::instance_create::new_instance;
use crate::runtime::qstr::RuntimeQstr;
use crate::objects::obj::FLAG_INSTANCE_TYPE;
use crate::objects::objtype::objexcept::{check_arity, ExcError};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{
    DictData, FunBuiltinData, GcRef, MapData, Obj, StaticClassMethodData, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};

/// Errors an `object` operation can raise (DictError/ExcError
/// precedent: the variant IS the exception type; the message
/// literal is carried for the VM).
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectError {
    /// `TypeError(msg)` — "arg must be user-type" / mismatch.
    TypeError(&'static str),
    /// `TypeError` with NO message (bare — the Zig's
    /// `raiseTypeErrorBare`, e.g. non-str `__setattr__` attr).
    BareTypeError,
    /// `AttributeError(msg)` — "no such attribute".
    AttributeError(&'static str),
    /// A slot raised (data dropped at the boundary).
    PyRaise,
    /// An allocation failed.
    OutOfMemory,
}

impl ObjectError {
    /// The exception type name (for the VM's `pending_exc` wiring).
    pub fn exc_name(&self) -> &'static str {
        match self {
            ObjectError::TypeError(_) | ObjectError::BareTypeError => "TypeError",
            ObjectError::AttributeError(_) => "AttributeError",
            ObjectError::PyRaise => "",
            ObjectError::OutOfMemory => "MemoryError",
        }
    }

    /// The TERSE message literal (`None` for the bare TypeError).
    pub fn message(&self) -> Option<&'static str> {
        match self {
            ObjectError::TypeError(m) => Some(m),
            ObjectError::AttributeError(m) => Some(m),
            _ => None,
        }
    }

    /// Convert to the slot-boundary signal (drops the data — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            ObjectError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}
/// `TypedRaise` (C13): the VM builds the exception object from
/// the recorded (name, message).
impl TypedRaise for ObjectError {
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

impl From<ExcError> for ObjectError {
    fn from(e: ExcError) -> ObjectError {
        match e {
            ExcError::TypeError(m) => ObjectError::TypeError(m),
            ExcError::PyRaise => ObjectError::PyRaise,
            ExcError::OutOfMemory => ObjectError::OutOfMemory,
        }
    }
}

/// `mp_type_object` (`objobject.zig:175`): flags `0x0000`, no
/// base, MRO `[self]` (filled by the bootstrap after allocation
/// — the `dict_type_data` precedent), make_new slot
/// (`object_make_new`, the type-call path — not a `TypeData`
/// slot), locals dict (`object_locals_dict`). No print slot.
pub fn object_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: static_qstr_id(b"object").expect("MP_QSTR_object is in the static table"),
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: None,
        bases: None,
        namedtuple_fields: None,
        protocol: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    }
}

/// Whether the value's header type is an instance type
/// (`objtype.zig`'s `objIsInstanceType`: `flags & 0x0200`).
pub fn is_instance_type(arena: &GcArena, value: Value) -> bool {
    match value {
        Value::Heap(r) => match arena.header(r).type_ {
            Some(t) => match arena.get(t) {
                Obj::Type(td) => td.flags & FLAG_INSTANCE_TYPE != 0,
                _ => false,
            },
            None => false,
        },
        _ => false,
    }
}

/// Whether a type object itself carries the instance-type flag.
pub fn type_is_instance_type(arena: &GcArena, type_: GcRef) -> bool {
    match arena.get(type_) {
        Obj::Type(td) => td.flags & FLAG_INSTANCE_TYPE != 0,
        _ => false,
    }
}

/// `mp_obj_is_str` (`objobject.zig:25`): qstr-immediate OR
/// exact-type str.
fn obj_is_str(ctx: &CallCtx, value: Value) -> bool {
    match value {
        Value::Qstr(_) => true,
        Value::Heap(r) => {
            matches!(ctx.arena.get(r), Obj::Str(_))
                && ctx.arena.header(r).type_ == Some(ctx.types.str_)
        }
        _ => false,
    }
}

/// `object_make_new` (`objobject.zig:60`): sig `(0, 0, false)`.
/// Validates the args, then returns `PyRaise`: allocation needs
/// `mp_obj_new_instance` (C12a — no `Instance` payload yet).
/// The arg check IS testable now (`object(1)` → mismatch).
pub fn object_make_new(
    _ctx: &mut CallCtx,
    _type_: GcRef,
    args: &[Value],
    n_kw: usize,
) -> Result<Value, ObjectError> {
    if !args.is_empty() || n_kw > 0 {
        return Err(ObjectError::TypeError(
            crate::objects::objtype::objexcept::ARG_NUM_TYPES_MISMATCH,
        ));
    }
    Err(ObjectError::PyRaise)
}

/// `object.__init__(self)` (`objobject.zig:76`): returns None.
pub fn object_init(_ctx: &mut CallCtx, _self: Value) -> Result<Value, ObjectError> {
    Ok(Value::None)
}

/// `object.__new__(cls)` (`objobject.zig:88`): the two checks,
/// then the instance allocation (`mp_obj_new_instance` — C12a's
/// `new_instance`, which primes `subobj[0]` with the
/// `native_init_wrapper` sentinel when the type has a native
/// base). The class's own Python `__new__` is NOT consulted —
/// this IS `object`'s `__new__` (probed: `object.__new__(Foo)`
/// on a class whose `__new__` raises does not call it).
pub fn object_new(ctx: &mut CallCtx, cls: Value) -> Result<Value, ObjectError> {
    // `isExactType(cls, &mp_type_type)`: cls is a type object.
    let is_type = matches!(cls, Value::Heap(r) if ctx.arena.header(r).type_ == Some(ctx.types.type_));
    if !is_type {
        return Err(ObjectError::TypeError("arg must be user-type"));
    }
    let cls_ref = match cls {
        Value::Heap(r) => r,
        _ => unreachable!("is_type implies heap"),
    };
    if !type_is_instance_type(ctx.arena, cls_ref) {
        return Err(ObjectError::TypeError("arg must be user-type"));
    }
    Ok(new_instance(ctx, cls_ref))
}

/// Resolve an `object.__setattr__` / `__delattr__` attr value to
/// the members-map qstr key: a qstr immediate as-is, a heap str
/// interned by bytes (the pool's `from_strn` — the same key the
/// `LOAD_ATTR` path would use, so the store is visible to it).
/// The caller already ran `obj_is_str`, so any other shape is
/// unreachable.
fn attr_qstr(ctx: &mut CallCtx, attr: Value) -> Result<u32, ObjectError> {
    match attr {
        Value::Qstr(id) => Ok(id),
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(d) => {
                let bytes = d.bytes.clone();
                ctx.pool
                    .from_strn(&bytes)
                    .map(u32::from)
                    .map_err(|e| match e {
                        PyError::OutOfMemory => ObjectError::OutOfMemory,
                        _ => ObjectError::PyRaise,
                    })
            }
            _ => unreachable!("obj_is_str checked the attr"),
        },
        _ => unreachable!("obj_is_str checked the attr"),
    }
}

/// `object.__setattr__(self, attr, value)` (`objobject.zig:111`):
/// the two checks, then the DIRECT members-map insert
/// (`mp_map_lookup(..., ADD_IF_NOT_FOUND)`) — no descriptor,
/// no `__setattr__` hook: this is the escape hatch a class's own
/// `__setattr__` cannot intercept (probed).
pub fn object_setattr(ctx: &mut CallCtx, self_in: Value, attr: Value, value: Value) -> Result<Value, ObjectError> {
    if !is_instance_type(ctx.arena, self_in) {
        return Err(ObjectError::TypeError("arg must be user-type"));
    }
    if !obj_is_str(ctx, attr) {
        return Err(ObjectError::BareTypeError);
    }
    let attr_q = attr_qstr(ctx, attr)?;
    let Value::Heap(r) = self_in else {
        return Err(ObjectError::PyRaise);
    };
    match ctx.arena.get_mut(r) {
        Obj::Instance(d) => {
            let pool = ctx.pool;
            d.members.store_qstr(pool, attr_q, value);
            Ok(Value::None)
        }
        // An instance-type header on a non-Instance payload (a
        // user exception subclass instance) — the vendor
        // reinterprets the payload as an instance here; Rust
        // cannot do that safely, and no corpus case reaches it.
        _ => Err(ObjectError::PyRaise),
    }
}

/// `object.__delattr__(self, attr)` (`objobject.zig:129`): the
/// two checks, then the DIRECT members-map remove
/// (`mp_map_lookup(..., REMOVE_IF_FOUND)`); a missing attr
/// raises `AttributeError("no such attribute")`.
pub fn object_delattr(ctx: &mut CallCtx, self_in: Value, attr: Value) -> Result<Value, ObjectError> {
    if !is_instance_type(ctx.arena, self_in) {
        return Err(ObjectError::TypeError("arg must be user-type"));
    }
    if !obj_is_str(ctx, attr) {
        return Err(ObjectError::BareTypeError);
    }
    let attr_q = attr_qstr(ctx, attr)?;
    let Value::Heap(r) = self_in else {
        return Err(ObjectError::PyRaise);
    };
    match ctx.arena.get_mut(r) {
        Obj::Instance(d) => {
            let pool = ctx.pool;
            if d.members.remove_qstr(pool, attr_q) {
                Ok(Value::None)
            } else {
                Err(ObjectError::AttributeError("no such attribute"))
            }
        }
        _ => Err(ObjectError::PyRaise),
    }
}

// ── BuiltinFn adapters (the locals-dict entries) ───────────────────

/// `object.__init__` — fixed 1 arg.
pub fn object_init_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    object_init(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// `object.__new__` — fixed 1 arg (the class; staticmethod —
/// no self is bound by the call machinery).
pub fn object_new_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 1, 1).map_err(|e| ctx.record_typed(e))?;
    object_new(ctx, args[0]).map_err(|e| ctx.record_typed(e))
}

/// `object.__setattr__` — fixed 3 args.
pub fn object_setattr_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 3, 3).map_err(|e| ctx.record_typed(e))?;
    object_setattr(ctx, args[0], args[1], args[2]).map_err(|e| ctx.record_typed(e))
}

/// `object.__delattr__` — fixed 2 args.
pub fn object_delattr_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    check_arity(args, 2, 2).map_err(|e| ctx.record_typed(e))?;
    object_delattr(ctx, args[0], args[1]).map_err(|e| ctx.record_typed(e))
}

/// Point the `__new__` wrapper in an `object` locals dict at
/// the real staticmethod type (see `object_locals_dict` — the
/// wrapper is allocated with header `None` because the type
/// does not exist yet at that point). Called by each type
/// registry once `staticmethod` is allocated; a no-op when the
/// entry is missing or not a wrapper (defensive — the dict is
/// fixed by then, but the lookup only reads).
pub fn fix_object_new_header(ctx: &mut CallCtx, object_dict: GcRef, staticmethod: GcRef) {
    use crate::objects::containers::objdict::dict_get_value;
    let Some(new_q) = static_qstr_id(b"__new__") else {
        return;
    };
    let Ok(Value::Heap(r)) =
        dict_get_value(ctx, Value::Heap(object_dict), Value::Qstr(new_q))
    else {
        return;
    };
    if matches!(ctx.arena.get(r), Obj::StaticClassMethod(_)) {
        ctx.arena.set_header_type(r, Some(staticmethod));
    }
}

/// `object_locals_dict` (`objobject.zig:165`): the fixed ROM
/// dict `{ __init__, __new__, __setattr__, __delattr__ }` (full
/// config — all 4; nano has 2). `__new__` is stored as a
/// `StaticClassMethod` headed by the staticmethod type
/// (`objobject.zig:101-107`: `object___new___obj` IS a
/// `MpObjStaticClassMethod`); `__init__` / `__setattr__` /
/// `__delattr__` are plain `FunBuiltin`s. The wrap matters:
/// `convert_member_lookup` binds `self` for a plain `FunBuiltin`
/// found on a native type, so an unwrapped `__new__` makes
/// `super(A, A).__new__(A)` call the builtin with 2 args (arity
/// mismatch); wrapped, it converts to `(fun, None)` and the call
/// passes 1 arg (E2g — class_new / p_super2).
pub fn object_locals_dict(ctx: &mut CallCtx) -> GcRef {
    let mut entries: Vec<(Value, Value)> = Vec::with_capacity(4);
    for (name, n_args, func) in [
        (
            b"__init__" as &[u8],
            fun_n_args(1, 1),
            object_init_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"__new__" as &[u8],
            fun_n_args(1, 1),
            object_new_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"__setattr__" as &[u8],
            fun_n_args(3, 3),
            object_setattr_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"__delattr__" as &[u8],
            fun_n_args(2, 2),
            object_delattr_builtin as crate::objects::slots::BuiltinFn,
        ),
    ] {
        let id = static_qstr_id(name)
            .unwrap_or_else(|| panic!("{} is a static qstr", String::from_utf8_lossy(name)));
        let fun = ctx.alloc(
            None,
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args,
                func,
                // Fixed-arity builtins: no keyword parameters (C10a).
                kw_func: None,
            }),
        );
        // `__new__` only: wrap in a StaticClassMethod (see the
        // fn docs above); the other three stay plain builtins.
        // The wrapper's header is `None` HERE — the staticmethod
        // type does not exist yet when either builder calls this
        // (both allocate `object` before `staticmethod`, and the
        // VM's bootstrap `CallCtx` carries a dummy `BuiltinTypes`
        // whose `staticmethod` is `GcRef(0)` — using it would
        // head the wrapper at a dict and `convert_member_lookup`
        // would return the wrapper itself instead of unwrapping
        // it, so `object.__new__(Foo)` raised "object not
        // callable"). Each builder calls
        // `fix_object_new_header` once the staticmethod type
        // exists; `convert_member_lookup` needs the real header
        // to tell a staticmethod wrapper from a classmethod one.
        let value = if name == b"__new__" {
            Value::Heap(ctx.alloc(
                None,
                Obj::StaticClassMethod(StaticClassMethodData {
                    fun: Value::Heap(fun),
                }),
            ))
        } else {
            Value::Heap(fun)
        };
        entries.push((Value::Qstr(id), value));
    }
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::new_fixed(entries),
        }),
    )
}
