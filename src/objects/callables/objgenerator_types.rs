//! Generator ROM types, print, and method table (Lane C10b).
//!
//! Split out of `objgenerator.rs` by Lane C13 (the file was 794
//! lines; the 800-line cap leaves no room for the VM's generator
//! wiring). Contents, all ported from
//! `src/objects/callables/objgenerator.zig` (503 lines):
//! - `gen_instance_print` (`objgenerator.zig:337-347`)
//! - `gen_wrap_call_slot` (the `gen_wrap` type's `call` slot —
//!   STUB until C13 wires it to `gen_wrap_call` in the core file)
//! - `gen_wrap_type_data` / `gen_instance_type_data`
//!   (`objgenerator.zig:443-510`)
//! - the `gen_instance_*_builtin` adapters (arity-checked stubs
//!   until C13 wires them — see the core file's module docs) and
//!   `gen_instance_locals_dict` (`objgenerator.zig:386-441`).

use crate::memory::gc::CallCtx;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::PrintKind;
use crate::objects::value::{DictData, FunBuiltinData, GcRef, MapData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};

// ── print ──────────────────────────────────────────────────────────

/// `gen_instance_print` (`objgenerator.zig:337-347`): kind ignored
/// (`_ = kind`), `"<generator object 'name' at 0x…>"`. The name
/// is the function's name qstr; the address is the arena index
/// in hex — the Zig prints the raw object pointer, which is
/// non-deterministic across runs, so no test can pin it; the
/// arena index is the Rust `id()` model (C0) and keeps the
/// shape. A non-generator value prints nothing.
pub fn gen_instance_print(
    ctx: &mut CallCtx,
    value: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let (fun, r) = match value {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Generator(g) => (g.fun, r),
            _ => return,
        },
        _ => return,
    };
    let name = match ctx.arena.get(fun) {
        // `Qstr = u16`; `FunBcData.name` is a u32 qstr id — the
        // same `as u16` narrowing `bc.rs` uses for qstr-table
        // indices (`.mpy` qstr indices are u16 by format).
        Obj::FunBc(f) => ctx.pool.qstr_str(f.name as u16),
        _ => b"?",
    };
    let _ = write!(
        sink,
        "<generator object '{}' at 0x{:x}>",
        String::from_utf8_lossy(name),
        r.0
    );
}

// ── ROM types ──────────────────────────────────────────────────────

/// `gen_wrap_call` as the `gen_wrap` type's `call` slot — STUB:
/// the call needs C13's code store to resolve the `FunBc`'s
/// bytecode (see the module docs), so until C13 wires it this
/// returns `PyRaise` (not implemented, the C6 slot convention).
/// The real constructor is `gen_wrap_call` (explicit
/// code/qstr-table/extra-args parameters).
pub fn gen_wrap_call_slot(
    ctx: &mut CallCtx,
    _self: Value,
    _n_args: usize,
    _n_kw: usize,
    _args: &[Value],
) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// `mp_type_gen_wrap` (`objgenerator.zig:443-475`): the
/// generating-function type — flags `0x0020`
/// (`MP_TYPE_FLAG_BINDS_SELF`), name `generator`, slots attr
/// (`mp_obj_fun_bc_attr`, C10a's — `attr: None` here, the wiring
/// is C14's) and call (`gen_wrap_call`). `dict` is the caller-owned
/// locals dict.
pub fn gen_wrap_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"generator")
            .expect("generator is a static qstr"),
        flags: 0x0020,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        protocol: None,
        attr: None,
        make_new: None,
        bases: None,
        unary_op: None,
        binary_op: None,
        call: Some(gen_wrap_call_slot),
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
    }
}

/// `mp_type_gen_instance` (`objgenerator.zig:477-510`): the
/// generator-instance type — flags `0x0080`
/// (`MP_TYPE_FLAG_ITER_IS_ITERNEXT`), name `generator`, slots
/// print (`gen_instance_print`), iter (`gen_instance_iternext` —
/// a generator is its own iterator, so `getiter` returns it
/// unchanged), locals_dict (filled by
/// `gen_instance_locals_dict`). `dict` is the caller-owned
/// locals dict.
pub fn gen_instance_type_data(dict: GcRef) -> TypeData {
    TypeData {
        name: crate::objects::primitives::ops::static_qstr_id(b"generator")
            .expect("generator is a static qstr"),
        flags: 0x0080,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        protocol: None,
        attr: None,
        make_new: None,
        bases: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: Some(|_ctx: &mut CallCtx, self_: Value| Ok(self_)),
        subscr: None,
        buffer: None,
        print: Some(gen_instance_print),
    }
}

// ── BuiltinFn adapters (stubs until C13) ─────────────────────────

/// Adapt a method implementation to the frozen `BuiltinFn` shape.
/// The real implementations need a runner (and, for `close`, the
/// `GeneratorExit` singleton) that only the VM has; until C13
/// grows `CallCtx` into the Vm and wires these, they check the
/// arity (the VM's sig check — the reference binary raises
/// `TypeError("argument num/types mismatch")` for every wrong
/// count, probed 2026-11-17) and then return `PyRaise` (not
/// implemented). The full implementations are `gen_instance_send`
/// / `gen_instance_throw` / `gen_instance_close` /
/// `gen_instance_pend_throw` (explicit runner parameters).
macro_rules! gen_builtin_stub {
    ($name:ident, $min:expr, $max:expr) => {
        pub fn $name(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
            let _ = ctx;
            if args.len() < $min || args.len() > $max {
                return Err(PyError::PyRaise);
            }
            Err(PyError::PyRaise)
        }
    };
}

gen_builtin_stub!(gen_instance_send_builtin, 2, 2);
gen_builtin_stub!(gen_instance_throw_builtin, 2, 4);
gen_builtin_stub!(gen_instance_close_builtin, 1, 1);
gen_builtin_stub!(gen_instance_pend_throw_builtin, 2, 2);

/// `gen_instance_locals_dict` (`objgenerator.zig:386-441`): the
/// instance method table — `send` (fixed 2), `throw` (var 2..4),
/// `close` (fixed 1), `pend_throw` (fixed 2, full config only).
/// The `n_args` encoding is `fun_n_args(min, max)`
/// (`(min << 8) | max`, C7). Objects are allocated with a `None`
/// header type — the bootstrap pattern (the dict/builtin-function
/// types do not exist yet; cf. `int_locals_dict`).
pub fn gen_instance_locals_dict(ctx: &mut CallCtx) -> GcRef {
    let mut entries: Vec<(Value, Value)> = Vec::with_capacity(4);
    for (name, n_args, func) in [
        (
            b"send" as &[u8],
            fun_n_args(2, 2),
            gen_instance_send_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"throw" as &[u8],
            fun_n_args(2, 4),
            gen_instance_throw_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"close" as &[u8],
            fun_n_args(1, 1),
            gen_instance_close_builtin as crate::objects::slots::BuiltinFn,
        ),
        (
            b"pend_throw" as &[u8],
            fun_n_args(2, 2),
            gen_instance_pend_throw_builtin as crate::objects::slots::BuiltinFn,
        ),
    ] {
        let id = crate::objects::primitives::ops::static_qstr_id(name)
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
        entries.push((Value::Qstr(id), Value::Heap(fun)));
    }
    ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(entries),
        }),
    )
}
