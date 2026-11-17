//! Runtime dispatch — the Rust port of `src/runtime/runtime.zig`
//! (1,616 lines) in the original petitpy.
//!
//! The Zig file is the interpreter's runtime support: name/global
//! loads and stores, the unary/binary operator dispatch (including
//! the small-int fast path and the generic slot loop), the call
//! protocol (`mp_call_function_n_kw` / `mp_call_method_n_kw` / the
//! `*args` / `**kwargs` preparation), sequence unpacking, method
//! loading (`mp_load_method_maybe` and friends), attribute store,
//! iterator protocol (`mp_getiter` / `mp_iternext`), generator
//! resume (`mp_resume`), `mp_make_raise_obj`, and the import
//! helpers (`mp_import_name` / `mp_import_from` / `mp_import_all`).
//!
//! Split (all under `src/runtime/`):
//!
//! - `runtime_names` — `mp_load_name` / `mp_load_global` /
//!   `mp_load_build_class` / `mp_store_name` / `mp_store_global` /
//!   `mp_delete_name` / `mp_delete_global`.
//! - `runtime_ops` — `mp_unary_op` / `mp_binary_op`.
//! - `runtime_call` — `mp_call_function_n_kw` (+ `_0` / `_1` /
//!   `_2`), `mp_call_method_n_kw`, `mp_call_method_self_n_kw`,
//!   `mp_call_prepare_args_n_kw_var`, `mp_call_method_n_kw_var`.
//! - `runtime_method` — `mp_load_method_maybe` / `mp_load_method` /
//!   `mp_load_method_protected` / `mp_load_attr` / `mp_store_attr`,
//!   the `next` builtin body + constructor.
//! - `runtime_iter` — `mp_getiter` / `mp_iternext` /
//!   `mp_iternext_allow_raise` / `mp_unpack_sequence` /
//!   `mp_unpack_ex`.
//! - `runtime_resume` — `mp_resume` / `mp_make_raise_obj`.
//! - `runtime_import` — `mp_import_name` / `mp_import_from` /
//!   `mp_import_all`.
//!
//! Error channel: every public function returns
//! `Result<_, RtError>`. `RtError` carries the exception name +
//! message so the VM (C13) can build the exception object into its
//! `pending_exc` slot. The one exception is `RtError::PyRaise`:
//! a slot raised and the exception object lives in the VM's pending
//! slot (the C6 slot-boundary convention — the object is dropped at
//! the boundary until C13 wires `pending_exc` through). `Ok(None)`
//! from `mp_unary_op` is the Zig's `MP_OBJ_NULL` ("not
//! implemented", the `*_MAYBE` outcome), not an error.
//!
//! Seams (explicit parameters, C11 / `VmSeams` precedent):
//!
//! - `RuntimeSeams::import_default` —
//!   `mp_builtin___import___default` (C15/D own it; this lane never
//!   returns a fake module).
//! - `RuntimeSeams::gen_resume` — generator resume execution
//!   (C10b's `gen_resume` + C13's runner; the frame and the runner
//!   are C13-side, so C14 cannot call `gen_resume` directly).
//!
//! Out of scope (see the result file): `mp_init` / `mp_deinit`
//! (C13/D own interpreter init — there is no global `MpStateThread`
//! here); float/complex scalar arithmetic (C18 — the `mp_binary_op`
//! float/complex promotion arms route through the generic slot
//! dispatch, which reaches C18's slots via the reverse-op path);
//! `__build_class__` (D1 owns the object; taken as a parameter).

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::callables::objgenerator::{GenError, GenResume};
use crate::objects::obj::{is_exact_int, is_float, is_str, obj_get_type};
use crate::objects::objtype::objexcept_types::ExceptionTypes;
use crate::pyerr::{PyError, PyResult};
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::qstr::RuntimeQstr;

// ── RtError ─────────────────────────────────────────────────────────

/// The runtime error: an exception name + message for the VM's
/// `pending_exc` wiring (C13 builds the exception object from
/// `exc_name()` + `message()`), or a slot-boundary signal.
#[derive(Clone, Debug, PartialEq)]
pub enum RtError {
    /// `TypeError(msg)`.
    TypeError(String),
    /// `ValueError(msg)`.
    ValueError(String),
    /// `AttributeError(msg)`.
    AttributeError(String),
    /// `NameError(msg)`.
    NameError(String),
    /// `ImportError(msg)`.
    ImportError(String),
    /// `ZeroDivisionError(msg)` — the message is always
    /// `"divide by zero"` (`binopZeroDiv`).
    ZeroDivisionError(String),
    /// `RuntimeError(msg)`.
    RuntimeError(String),
    /// Raise this exception object (`GenError::Raised` shape: the
    /// name lives on the object, so `exc_name()` is `""`).
    Raised(Value),
    /// `StopIteration` — `None` = bare, `Some(v)` = the single arg.
    /// The Zig distinguishes the two (`raiseStopIter` reads
    /// `stop_iteration_arg`, `NULL` = bare); a `None` arg is NOT
    /// the same as bare (probed: `next(gen)` on a generator that
    /// `return 99`s gives `args == (99,)`, on an exhausted list
    /// iterator `args == ()`).
    StopIteration(Option<Value>),
    /// An allocation failed.
    OutOfMemory,
    /// A slot raised; the exception object (if any) is in the VM's
    /// pending slot (C13) — the C6 slot-boundary convention.
    PyRaise,
}

impl RtError {
    /// The exception type name for the VM's pending wiring
    /// (`""` = the name lives on the object itself, or there is no
    /// object to build).
    pub fn exc_name(&self) -> &'static str {
        match self {
            RtError::TypeError(_) => "TypeError",
            RtError::ValueError(_) => "ValueError",
            RtError::AttributeError(_) => "AttributeError",
            RtError::NameError(_) => "NameError",
            RtError::ImportError(_) => "ImportError",
            RtError::ZeroDivisionError(_) => "ZeroDivisionError",
            RtError::RuntimeError(_) => "RuntimeError",
            RtError::Raised(_) => "",
            RtError::StopIteration(_) => "StopIteration",
            RtError::OutOfMemory => "MemoryError",
            RtError::PyRaise => "",
        }
    }

    /// The exception message (`""` when the message lives on the
    /// object itself, or there is no object to build).
    pub fn message(&self) -> String {
        match self {
            RtError::TypeError(m)
            | RtError::ValueError(m)
            | RtError::AttributeError(m)
            | RtError::NameError(m)
            | RtError::ImportError(m)
            | RtError::ZeroDivisionError(m)
            | RtError::RuntimeError(m) => m.clone(),
            RtError::Raised(_) | RtError::StopIteration(_) | RtError::OutOfMemory | RtError::PyRaise => {
                String::new()
            }
        }
    }

    /// The slot-boundary signal (drops the name/message — the
    /// accepted C6/C7r convention).
    pub fn to_pyerror(self) -> PyError {
        match self {
            RtError::OutOfMemory => PyError::OutOfMemory,
            _ => PyError::PyRaise,
        }
    }
}

impl From<PyError> for RtError {
    fn from(e: PyError) -> RtError {
        match e {
            PyError::OutOfMemory => RtError::OutOfMemory,
            PyError::PyRaise => RtError::PyRaise,
        }
    }
}

impl From<crate::objects::obj::ObjError> for RtError {
    fn from(e: crate::objects::obj::ObjError) -> RtError {
        match e {
            crate::objects::obj::ObjError::OutOfMemory => RtError::OutOfMemory,
            _ => RtError::PyRaise,
        }
    }
}

/// Map a `GenError` onto `RtError` (the non-object-carrying cases;
/// `Raised` and `StopIteration` are handled by the callers that
/// can carry the object — `mp_resume`'s `ResumeOutcome::Exception`
/// and `mp_iternext`'s `Err(RtError::Raised)`).
pub fn gen_error_to_rt(e: GenError, pool: &RuntimeQstr) -> RtError {
    match e {
        GenError::Raised(v) => RtError::Raised(v),
        GenError::StopIteration(sv) => RtError::StopIteration(sv),
        GenError::AlreadyExecuting => {
            RtError::ValueError("generator already executing".to_string())
        }
        GenError::CantSendNonNone => RtError::TypeError(
            "can't send non-None value to a just-started generator".to_string(),
        ),
        GenError::IgnoredGeneratorExit => {
            RtError::RuntimeError("generator ignored GeneratorExit".to_string())
        }
        GenError::GeneratorRaisedStopIteration => {
            RtError::RuntimeError("generator raised StopIteration".to_string())
        }
        GenError::ArgNumTypesMismatch => {
            RtError::TypeError("argument num/types mismatch".to_string())
        }
        GenError::Setup(_) => RtError::TypeError(
            e.setup_message(pool)
                .unwrap_or_else(|| "generator setup failed".to_string()),
        ),
        GenError::PyRaise => RtError::PyRaise,
        GenError::OutOfMemory => RtError::OutOfMemory,
    }
}

// ── RuntimeSeams ────────────────────────────────────────────────────

/// The seams this lane needs but does not own (C11 / `VmSeams`
/// precedent: required parameters, never a fake implementation).
/// The entries are owned `Box`es (not `&mut dyn` references) so
/// `stub()` can return a complete `RuntimeSeams` — a `&mut` to a
/// bare-fn stub would be a reference to a temporary and cannot
/// leave the constructor.
pub struct RuntimeSeams<'a> {
    /// `mp_builtin___import___default` (C15/D own it): the 5-arg
    /// import call `[name, globals, locals, fromlist, level]`.
    pub import_default: Box<dyn FnMut(&mut CallCtx, &[Value]) -> PyResult<Value> + 'a>,
    /// Generator resume execution (C10b's `gen_resume` + C13's
    /// runner): `(gen, send, throw)` — `throw == None` means "no
    /// throw". Returns the raw `gen_resume` outcome; the callers
    /// map `GenResume` / `GenError` onto their own outcomes.
    pub gen_resume: Box<
        dyn FnMut(
                &mut CallCtx,
                Value,
                Value,
                Option<Value>,
            ) -> Result<GenResume, GenError>
            + 'a,
    >,
}

fn stub_import_default(
    _ctx: &mut CallCtx,
    _args: &[Value],
) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

fn stub_gen_resume(
    _ctx: &mut CallCtx,
    _gen: Value,
    _send: Value,
    _throw: Option<Value>,
) -> Result<GenResume, GenError> {
    Err(GenError::PyRaise)
}

impl<'a> RuntimeSeams<'a> {
    /// Stub seams: every seam call fails with bare `PyRaise`. Used
    /// by the `next`-builtin adapter (a bare `BuiltinFn` cannot
    /// capture seams — C13's `CallCtx` growth replaces it) and by
    /// tests that never touch the seam. A test that needs a
    /// generator or an importer builds its own closures.
    pub fn stub() -> RuntimeSeams<'a> {
        RuntimeSeams {
            import_default: Box::new(stub_import_default),
            gen_resume: Box::new(stub_gen_resume),
        }
    }
}

// ── shared outcome types ────────────────────────────────────────────

/// The Zig `dest[2]` array (`mp_load_method_maybe` /
/// `mp_load_method`): the method function plus its bound self
/// (`None` = unbound — the Zig's `dest[1] == MP_OBJ_NULL`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MethodDest {
    /// `dest[0]` — the function (or plain attribute value).
    pub func: Value,
    /// `dest[1]` — the bound self, or `None` when unbound.
    pub self_: Option<Value>,
}

impl MethodDest {
    /// An unbound method (plain function, no self).
    pub fn unbound(func: Value) -> MethodDest {
        MethodDest { func, self_: None }
    }

    /// A bound method (`dest[1] = self`).
    pub fn bound(func: Value, self_: Value) -> MethodDest {
        MethodDest { func, self_: Some(self_) }
    }
}

/// `mp_iternext` / `mp_iternext_allow_raise`: the next item, or
/// exhaustion carrying the `StopIteration` value (`None` = bare —
/// see `RtError::StopIteration`). A raise is `Err(RtError)`:
/// `RtError::Raised(v)` when the exception object is available
/// (the generator branch), `RtError::PyRaise` when it lives in
/// the VM's pending slot (the `__next__`-method branch — the
/// object is unrecoverable at the slot boundary until C13).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IterNext {
    /// The next item.
    Value(Value),
    /// Exhaustion: `None` = bare `StopIteration()`,
    /// `Some(v)` = `StopIteration(v)`.
    Stop(Option<Value>),
}

/// `mp_resume`: the `MP_VM_RETURN_*` triple, with the value in
/// the return slot (`ret_val[0]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResumeOutcome {
    /// `MP_VM_RETURN_YIELD` — the yielded value.
    Yield(Value),
    /// `MP_VM_RETURN_NORMAL` — the return value (`None` when the
    /// Zig maps a `NULL` stop value).
    Normal(Value),
    /// `MP_VM_RETURN_EXCEPTION` — the exception object.
    Exception(Value),
}

/// The builtins-module lookups `mp_load_global` needs: the
/// builtins globals dict plus the full-config override dict
/// (`builtins.X = Y` support, `mp_module_builtins_override_dict`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Builtins {
    /// `mp_module_builtins_globals` — the builtins module dict.
    pub globals: Value,
    /// `mp_module_builtins_override_dict` (`None` = not installed).
    pub override_dict: Option<Value>,
}

/// The `mp_call_prepare_args_n_kw_var` output: the call's function
/// plus the flattened `[self?, positionals..., kw pairs...]` args.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedCall {
    /// `out_args.fun`.
    pub fun: Value,
    /// `out_args.args` — self first when `have_self`.
    pub args: Vec<Value>,
    /// `out_args.n_args` — positional count, self included.
    pub n_args: usize,
    /// `out_args.n_kw` — keyword-pair count.
    pub n_kw: usize,
}

/// The exception-type refs the subclass checks need
/// (`is_subclass_fast` classinfo). The VM (C13) owns the
/// `ExceptionTypes` registry; these refs are its
/// explicit-parameter form (C5 precedent).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExcRefs {
    /// `BaseException` (for `mp_obj_is_exception_type`).
    pub base_exception: GcRef,
    /// `StopIteration` (for the PEP 479 check in `mp_resume`).
    pub stop_iteration: GcRef,
    /// `GeneratorExit` (for the `close()` branch in `mp_resume`).
    pub generator_exit: GcRef,
    /// `RuntimeError` (for the PEP 479 tail in `mp_resume`).
    pub runtime_error: GcRef,
    /// `KeyError` (for the `del module.missing` case in
    /// `mp_store_attr`: the `DictError::KeyError` is built
    /// into an object here so the kind survives the
    /// `RtError` boundary as `Raised`).
    pub key_error: GcRef,
}

impl ExcRefs {
    /// Build from a `build_exception_types` registry.
    pub fn from_exception_types(exc: &ExceptionTypes) -> ExcRefs {
        ExcRefs {
            base_exception: exc.by_name("BaseException").expect("BaseException in registry"),
            stop_iteration: exc.by_name("StopIteration").expect("StopIteration in registry"),
            generator_exit: exc.by_name("GeneratorExit").expect("GeneratorExit in registry"),
            runtime_error: exc.by_name("RuntimeError").expect("RuntimeError in registry"),
            key_error: exc.by_name("KeyError").expect("KeyError in registry"),
        }
    }
}

// ── shared helpers ──────────────────────────────────────────────────

/// A dunder name's static qstr id (`None` = not a static qstr —
/// the attribute cannot exist, so the lookup misses). Same helper
/// as `objtype/instance.rs`'s (private there).
pub(crate) fn dunder(name: &[u8]) -> Option<u32> {
    crate::objects::primitives::ops::static_qstr_id(name)
}

/// `isNativeScalar` (`runtime.zig:505-508`): a small int, a float,
/// an exact int, a qstr, or an exact str. (The Zig spells the
/// qstr/exact-str disjuncts separately; the Rust `is_str` covers
/// both — qstr immediate OR exact-type str.)
pub(crate) fn is_native_scalar(
    arena: &GcArena,
    types: &crate::objects::obj::BuiltinTypes,
    value: Value,
) -> bool {
    matches!(value, Value::SmallInt(_))
        || is_float(arena, types, value)
        || is_exact_int(arena, types, value)
        || is_str(arena, types, value)
}

/// The type name for `binopUnsupported`'s `'%q', '%q'` (`runtime.zig:
/// 436-447` reads `mp_obj_get_type(o)->name`). Values whose header
/// is `None` (a `FunBuiltin` / `FunBc` / `Closure` allocated without
/// a type object yet) have no type to read — the Zig's
/// `fun_builtin_N` types are named `"function"`, so that is the
/// fallback (probed: `len + 1` → `'function', 'int'`); a bound
/// method's type is named `"bound_method"`.
pub(crate) fn type_name_of(ctx: &CallCtx, value: Value) -> String {
    if let Some(t) = obj_get_type(ctx.arena, &ctx.types, value) {
        if let Some(d) = crate::objects::obj::type_data(ctx.arena, t) {
            let bytes = ctx.pool.qstr_str(d.name as u16);
            return String::from_utf8_lossy(bytes).into_owned();
        }
    }
    match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::FunBuiltin(_) | Obj::FunBc(_) | Obj::Closure(_) => "function".to_string(),
            Obj::BoundMeth(_) => "bound_method".to_string(),
            Obj::Generator(_) => "generator".to_string(),
            _ => "object".to_string(),
        },
        Value::SmallInt(_) => "int".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::None => "NoneType".to_string(),
        Value::Qstr(_) => "str".to_string(),
    }
}
