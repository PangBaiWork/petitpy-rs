//! Lane C14 tests (part 1): names, unary/binary dispatch.
//! Part 2 (`runtime_tests_b.rs`) covers method/attribute
//! loading, iteration, resume, and imports; part 3
//! (`runtime_tests_c.rs`) covers the call protocol and unpack.
//!
//! The test env reuses the containers `Fixture` (real int /
//! tuple / list / dict / set / frozenset / range types with
//! real slots), swaps in the REAL `str` / `bytes` type data
//! (the fixture's are plain `make_type_with_print` placeholders
//! with no slots), then runs `build_rom` and
//! `build_exception_types` over them — the same shape the VM
//! (C13) will build on.

use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::containers::objdict::dict_store;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::containers::tests::{empty_dict, Fixture as CFixture};
use crate::objects::obj::BuiltinTypes;
use crate::objects::objtype::objexcept_types::{build_exception_types, ExceptionTypes};
use crate::objects::objtype::rom::build_rom;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BinaryOp, BuiltinFn, UnaryOp};
use crate::objects::strings::objstr_methods_b::{bytes_type_data, str_type_data};
use crate::objects::value::{
    BytesData, FunBuiltinData, GcRef, Obj, StrData, TypeData, Value,
};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::runtime::{Builtins, ExcRefs, RtError, RuntimeSeams};
use crate::runtime::runtime_method::next_builtin_obj;
use crate::runtime::runtime_names::{
    delete_global, delete_name, load_build_class, load_global, load_name, store_global,
    store_name,
};
use crate::runtime::runtime_ops::{binary_op, unary_op};

/// The C14 test env: the containers fixture with real str/bytes
/// types, the ROM registry, the exception registry, the `next`
/// builtin, and an empty builtins/globals pair.
pub(crate) struct Env {
    pub arena: GcArena,
    pub roots: RootSet,
    pub types: BuiltinTypes,
    pub pool: RuntimeQstr,
    pub exc: ExceptionTypes,
    pub refs: ExcRefs,
    pub next_obj: Value,
    pub builtins: Builtins,
    pub globals: Value,
    /// C13's pending-raise slot (CallCtx::new is 5-arg since C13).
    pub pending_raise: Option<PendingRaise>,
    /// The generator-instance type (fixture field, not a
    /// `BuiltinTypes` field — callers take the header type
    /// explicitly).
    pub gen_instance: GcRef,
    /// The `module` type, with its `attr` slot wired to
    /// `module_attr` (wired by `module_type_data` since C15b — the
    /// manual assignment below is now redundant, kept as the
    /// explicit record).
    pub module_type: GcRef,
}

impl Env {
    pub(crate) fn new() -> Env {
        let cfx = CFixture::new();
        let mut arena = cfx.arena;
        let mut roots = cfx.roots;
        let mut types0 = cfx.types;
        let pool = cfx.pool;
        let gen_instance = cfx.gen_instance;
        // Swap in the real str/bytes types (the fixture's are
        // plain placeholders with no slots).
        let str_name = static_qstr_id(b"str").expect("str is a static qstr");
        let bytes_name = static_qstr_id(b"bytes").expect("bytes is a static qstr");
        let str_dict = empty_dict(&mut arena, &roots);
        let str_ = arena.alloc(None, Obj::Type(str_type_data(str_name, str_dict)), &roots);
        let bytes_dict = empty_dict(&mut arena, &roots);
        let bytes = arena.alloc(
            None,
            Obj::Type(bytes_type_data(bytes_name, bytes_dict)),
            &roots,
        );
        types0.str_ = str_;
        types0.bytes = bytes;
        roots.push(Value::Heap(str_));
        roots.push(Value::Heap(bytes));
        let mut pending_raise = None;
        let mut ctx = CallCtx::new(&mut arena, &mut roots, &types0, &pool, &mut pending_raise);
        let rom = build_rom(&mut ctx);
        drop(ctx);
        let types = rom.types;
        // Rebuild the ctx over the ROM types before building
        // the exception registry (its headers must be the ROM
        // `type`, not the fixture's plain one).
        let mut ctx = CallCtx::new(&mut arena, &mut roots, &types, &pool, &mut pending_raise);
        let exc = build_exception_types(&mut ctx, types.object);
        let refs = ExcRefs::from_exception_types(&exc);
        let next_obj = next_builtin_obj(&mut ctx);
        // The `module` type (MRO [self, object]) with the
        // `module_attr` load slot wired.
        let module_dict = empty_dict(&mut ctx.arena, &ctx.roots);
        let mut module_td =
            crate::objects::objtype::objmodule::module_type_data(module_dict, types.object);
        module_td.attr = Some(crate::objects::objtype::objmodule::module_attr);
        let module_type = ctx.alloc(Some(types.type_), Obj::Type(module_td));
        ctx.roots.push(Value::Heap(module_type));
        if let Obj::Type(td) = ctx.arena.get_mut(module_type) {
            td.mro = vec![module_type, types.object];
        }
        // An empty globals dict (header = the dict type, so
        // `**` expansion recognises it as an exact dict).
        let g = empty_dict(&mut ctx.arena, &ctx.roots);
        ctx.arena.set_header_type(g, Some(types.dict));
        let globals = Value::Heap(g);
        drop(ctx);
        let builtins = Builtins {
            globals: Value::Heap(empty_dict(&mut arena, &roots)),
            override_dict: None,
        };
        Env {
            arena,
            roots,
            types,
            pool,
            exc,
            refs,
            next_obj,
            builtins,
            globals,
            gen_instance,
            module_type,
            pending_raise,
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool, &mut self.pending_raise)
    }

    /// Resolve a name to a qstr id: static when present, else
    /// interned dynamically (pre-interning before the `ctx` is
    /// built, so the `InternFn` seam can use `find_strn` only).
    pub(crate) fn q(&mut self, name: &[u8]) -> u32 {
        match static_qstr_id(name) {
            Some(id) => id,
            None => self.pool.from_strn(name).expect("fixture qstr interning") as u32,
        }
    }

    /// The `InternFn` seam over a pool reference (find-only;
    /// names must be pre-interned with `q()` first). Built from
    /// `ctx.pool` at the call site — never from `&env` while a
    /// `ctx` is alive.
    pub(crate) fn intern_seam(pool: &RuntimeQstr) -> impl FnMut(&[u8]) -> PyResult<u32> + '_ {
        move |b: &[u8]| {
            let id = pool.find_strn(b);
            if id != 0 {
                Ok(id as u32)
            } else {
                Err(PyError::PyRaise)
            }
        }
    }

    /// A heap `Str` value.
    pub(crate) fn str_v(&mut self, s: &[u8]) -> Value {
        let t = self.types.str_;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(
            Some(t),
            Obj::Str(StrData {
                bytes: s.to_vec(),
            }),
        ))
    }

    /// A heap `Bytes` value.
    pub(crate) fn bytes_v(&mut self, s: &[u8]) -> Value {
        let t = self.types.bytes;
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(
            Some(t),
            Obj::Bytes(BytesData {
                bytes: s.to_vec(),
            }),
        ))
    }

    pub(crate) fn list_v(&mut self, items: &[Value]) -> Value {
        let t = self.types.list;
        let mut ctx = self.ctx();
        Value::Heap(new_list(&mut ctx, t, items))
    }

    pub(crate) fn tuple_v(&mut self, items: &[Value]) -> Value {
        let t = self.types.tuple;
        let mut ctx = self.ctx();
        Value::Heap(new_tuple(&mut ctx, t, items))
    }

    /// An exact dict (header = the dict type) with the pairs.
    pub(crate) fn dict_v(&mut self, pairs: &[(Value, Value)]) -> Value {
        let dict_t = self.types.dict;
        let mut ctx = self.ctx();
        let d = empty_dict(&mut ctx.arena, &ctx.roots);
        ctx.arena.set_header_type(d, Some(dict_t));
        let dv = Value::Heap(d);
        for (k, v) in pairs {
            dict_store(&mut ctx, dv, *k, *v).expect("fixture dict store");
        }
        dv
    }

    /// A `FunBuiltin` with a `None` header (the bootstrap
    /// pattern — `fun_builtin_N` type objects do not exist yet).
    pub(crate) fn fun_v(&mut self, name: &[u8], min: u16, max: u16, func: BuiltinFn) -> Value {
        let name_id = self.q(name);
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(
            None,
            Obj::FunBuiltin(FunBuiltinData {
                name: name_id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: None,
            }),
        ))
    }

    /// A user type (MRO as given) with the `instance_attr`
    /// load slot wired — the shape `typecreate` builds for
    /// `class X: ...`.
    pub(crate) fn user_type(&mut self, name: &[u8], base: Option<GcRef>, mro: Vec<GcRef>) -> GcRef {
        let name = self.q(name);
        let dict = empty_dict(&mut self.arena, &self.roots);
        let data = TypeData {
            name,
            flags: crate::objects::objtype::typecreate::TYPE_BASE_FLAGS,
            base,
            bases: None,
            mro,
            dict,
            namedtuple_fields: None,
            protocol: None,
            print: None,
            unary_op: None,
            binary_op: None,
            subscr: None,
            buffer: None,
            getiter: None,
            call: None,
            attr: Some(crate::objects::objtype::instance::instance_attr),
            make_new: None,
        };
        let r = self.arena.alloc(Some(self.types.type_), Obj::Type(data), &self.roots);
        self.roots.push(Value::Heap(r));
        r
    }

    /// An instance of `type_` (no-arg construction).
    pub(crate) fn instance_of(&mut self, type_: GcRef) -> Value {
        let mut ctx = self.ctx();
        crate::objects::objtype::instance_create::instance_make_new(&mut ctx, type_, &[], 0, None)
            .expect("instance")
    }

    /// Store `value` under `key` in a type's locals dict.
    pub(crate) fn set_local(&mut self, type_: GcRef, key: u32, value: Value) {
        let dict = match self.arena.get(type_) {
            Obj::Type(t) => Value::Heap(t.dict),
            _ => panic!("set_local on a non-type"),
        };
        let mut ctx = self.ctx();
        dict_store(&mut ctx, dict, Value::Qstr(key), value).expect("set_local");
    }

    /// `binary_op` with this env's refs/builtins/stub seams.
    pub(crate) fn binop(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, RtError> {
        let refs = self.refs;
        let next_obj = self.next_obj;
        let builtins = self.builtins.clone();
        let globals = self.globals;
        let mut ctx = self.ctx();
        binary_op(
            &mut ctx,
            op,
            lhs,
            rhs,
            &refs,
            next_obj,
            &builtins,
            &mut RuntimeSeams::stub(),
            globals,
        )
    }

    pub(crate) fn unop(&mut self, op: UnaryOp, arg: Value) -> Result<Option<Value>, RtError> {
        let mut ctx = self.ctx();
        unary_op(&mut ctx, op, arg)
    }
}

/// A `FunBuiltin` body that always returns the small int 7.
pub(crate) fn seven(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(7))
}

/// A `FunBuiltin` body that returns its first argument.
pub(crate) fn echo(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[0])
}

// ── names ───────────────────────────────────────────────────────────

#[test]
fn load_name_locals_then_globals_then_builtins() {
    let mut env = Env::new();
    let x = env.q(b"x");
    let locals = env.dict_v(&[(Value::Qstr(x), Value::SmallInt(1))]);
    let globals = env.dict_v(&[(Value::Qstr(x), Value::SmallInt(2))]);
    let builtins_d = env.dict_v(&[(Value::Qstr(x), Value::SmallInt(3))]);
    let locals2 = env.dict_v(&[]);
    let globals2 = env.dict_v(&[]);
    env.builtins.globals = builtins_d;
    let b = env.builtins.clone();
    let mut ctx = env.ctx();
    assert_eq!(load_name(&mut ctx, x, locals, globals, &b).unwrap(), Value::SmallInt(1));
    assert_eq!(
        load_name(&mut ctx, x, locals2, globals2, &b).unwrap(),
        Value::SmallInt(3)
    );
    // Globals beat builtins.
    let mut ctx = env.ctx();
    assert_eq!(
        load_name(&mut ctx, x, locals2, globals, &b).unwrap(),
        Value::SmallInt(2)
    );
}

#[test]
fn load_name_missing_is_name_error() {
    let mut env = Env::new();
    let x = env.q(b"nope");
    let locals = env.dict_v(&[]);
    let globals = env.dict_v(&[]);
    let b = env.builtins.clone();
    let mut ctx = env.ctx();
    let err = load_name(&mut ctx, x, locals, globals, &b).unwrap_err();
    assert_eq!(err.exc_name(), "NameError");
    assert!(err.message().contains("nope"), "msg: {}", err.message());
}

#[test]
fn load_global_globals_first_override_fills_miss() {
    let mut env = Env::new();
    let x = env.q(b"x");
    let y = env.q(b"y");
    let globals = env.dict_v(&[(Value::Qstr(x), Value::SmallInt(2))]);
    let od = env.dict_v(&[
        (Value::Qstr(x), Value::SmallInt(9)),
        (Value::Qstr(y), Value::SmallInt(7)),
    ]);
    env.builtins.override_dict = Some(od);
    let builtins_d = env.dict_v(&[]);
    env.builtins.globals = builtins_d;
    let b = env.builtins.clone();
    let mut ctx = env.ctx();
    // `mp_load_global` checks globals FIRST, then the override
    // dict, then builtins globals (`runtime.zig:301-325`) — the
    // override only fills a miss.
    assert_eq!(load_global(&mut ctx, x, globals, &b).unwrap(), Value::SmallInt(2));
    assert_eq!(load_global(&mut ctx, y, globals, &b).unwrap(), Value::SmallInt(7));
}

#[test]
fn store_and_delete_name_global() {
    let mut env = Env::new();
    let x = env.q(b"x");
    let locals = env.dict_v(&[]);
    let globals = env.dict_v(&[]);
    let b = env.builtins.clone();
    let mut ctx = env.ctx();
    store_name(&mut ctx, locals, x, Value::SmallInt(5));
    assert_eq!(load_name(&mut ctx, x, locals, globals, &b).unwrap(), Value::SmallInt(5));
    delete_name(&mut ctx, locals, x);
    assert!(load_name(&mut ctx, x, locals, globals, &b).is_err());
    store_global(&mut ctx, globals, x, Value::SmallInt(6));
    assert_eq!(load_global(&mut ctx, x, globals, &b).unwrap(), Value::SmallInt(6));
    delete_global(&mut ctx, globals, x);
    assert!(load_global(&mut ctx, x, globals, &b).is_err());
}

#[test]
fn load_build_class_default_and_override() {
    let mut env = Env::new();
    let default = env.fun_v(b"bc", 0, 0, seven);
    let bc_q = env.q(b"__build_class__");
    let custom = env.fun_v(b"custom", 0, 0, seven);
    let od = env.dict_v(&[(Value::Qstr(bc_q), custom)]);
    let b = env.builtins.clone();
    env.builtins.override_dict = Some(od);
    let b2 = env.builtins.clone();
    let mut ctx = env.ctx();
    assert_eq!(
        load_build_class(&mut ctx, &b, default).unwrap(),
        default
    );
    assert_eq!(load_build_class(&mut ctx, &b2, default).unwrap(), custom);
}

// ── unary ops ───────────────────────────────────────────────────────

#[test]
fn unary_small_int_ops() {
    let mut env = Env::new();
    assert_eq!(env.unop(UnaryOp::Positive, Value::SmallInt(5)).unwrap(), Some(Value::SmallInt(5)));
    assert_eq!(env.unop(UnaryOp::Negative, Value::SmallInt(5)).unwrap(), Some(Value::SmallInt(-5)));
    assert_eq!(env.unop(UnaryOp::Invert, Value::SmallInt(5)).unwrap(), Some(Value::SmallInt(-6)));
    assert_eq!(env.unop(UnaryOp::Abs, Value::SmallInt(-5)).unwrap(), Some(Value::SmallInt(5)));
    assert_eq!(env.unop(UnaryOp::Bool, Value::SmallInt(0)).unwrap(), Some(Value::Bool(false)));
    assert_eq!(env.unop(UnaryOp::Bool, Value::SmallInt(3)).unwrap(), Some(Value::Bool(true)));
    assert_eq!(env.unop(UnaryOp::Hash, Value::SmallInt(5)).unwrap(), Some(Value::SmallInt(5)));
    assert_eq!(env.unop(UnaryOp::Not, Value::SmallInt(0)).unwrap(), Some(Value::Bool(true)));
    assert_eq!(env.unop(UnaryOp::Not, Value::SmallInt(1)).unwrap(), Some(Value::Bool(false)));
    // `-SMALL_INT_MIN` = 2^62 leaves the window → heap int, and
    // the value is still exactly right.
    let neg = env.unop(UnaryOp::Negative, Value::SmallInt(-4611686018427387904)).unwrap().unwrap();
    assert!(matches!(neg, Value::Heap(_)));
    let int_t = env.types.int;
    let ctx = env.ctx();
    let want = Value::int(ctx.arena, Some(int_t), ctx.roots, 4611686018427387904);
    drop(ctx);
    assert_eq!(env.binop(BinaryOp::Equal, neg, want).unwrap(), Value::Bool(true));
}

#[test]
fn unary_hash_str_bytes_equal() {
    let mut env = Env::new();
    let s = env.str_v(b"abc");
    let b = env.bytes_v(b"abc");
    let hs = env.unop(UnaryOp::Hash, s).unwrap().unwrap();
    let hb = env.unop(UnaryOp::Hash, b).unwrap().unwrap();
    assert_eq!(hs, hb);
    assert_eq!(hs, Value::SmallInt(12933));
}

#[test]
fn unary_maybe_returns_none_and_len_errors() {
    let mut env = Env::new();
    // `int.__float__` — INT_MAYBE on a small int is the identity.
    assert_eq!(
        env.unop(UnaryOp::IntMaybe, Value::SmallInt(5)).unwrap(),
        Some(Value::SmallInt(5))
    );
    // `len(5)` — the small-int `else` in `mp_unary_op` is
    // INVERT (`// assert(op == MP_UNARY_OP_INVERT)`,
    // `runtime.zig:395-400`), so LEN on a small int returns
    // `~5 == -6`. The `len(5)` → "object has no len" error
    // (probed) comes from the `len` builtin's own
    // `mp_obj_len_maybe` check, not from `mp_unary_op`.
    assert_eq!(
        env.unop(UnaryOp::Len, Value::SmallInt(5)).unwrap(),
        Some(Value::SmallInt(-6))
    );
}

// ── binary ops ──────────────────────────────────────────────────────

#[test]
fn binary_small_int_arithmetic() {
    let mut env = Env::new();
    let two = Value::SmallInt(2);
    let three = Value::SmallInt(3);
    assert_eq!(env.binop(BinaryOp::Add, two, three).unwrap(), Value::SmallInt(5));
    assert_eq!(env.binop(BinaryOp::Subtract, three, two).unwrap(), Value::SmallInt(1));
    assert_eq!(env.binop(BinaryOp::Multiply, two, three).unwrap(), Value::SmallInt(6));
    assert_eq!(env.binop(BinaryOp::FloorDivide, Value::SmallInt(10), three).unwrap(), Value::SmallInt(3));
    assert_eq!(env.binop(BinaryOp::Modulo, Value::SmallInt(10), three).unwrap(), Value::SmallInt(1));
    assert_eq!(env.binop(BinaryOp::Modulo, Value::SmallInt(-8), three).unwrap(), Value::SmallInt(1));
    assert_eq!(env.binop(BinaryOp::Modulo, Value::SmallInt(8), Value::SmallInt(-3)).unwrap(), Value::SmallInt(-1));
    // Floored `//` takes the divisor's sign too: `8 // -3 == -3`,
    // `(-8) // 3 == -3` (Euclidean division would give -2).
    assert_eq!(env.binop(BinaryOp::FloorDivide, Value::SmallInt(8), Value::SmallInt(-3)).unwrap(), Value::SmallInt(-3));
    assert_eq!(env.binop(BinaryOp::FloorDivide, Value::SmallInt(-8), three).unwrap(), Value::SmallInt(-3));
    assert_eq!(env.binop(BinaryOp::Power, two, Value::SmallInt(10)).unwrap(), Value::SmallInt(1024));
    // `1 << 62` goes big (1 > SMALL_INT_MAX >> 62): a HEAP int
    // 2**62, not a small int — probed value
    // `4611686018427387904`, checked numerically.
    let sh = env.binop(BinaryOp::Lshift, Value::SmallInt(1), Value::SmallInt(62)).unwrap();
    assert!(matches!(sh, Value::Heap(_)), "1 << 62 must be a heap int, got {sh:?}");
    assert_eq!(
        env.binop(BinaryOp::Equal, sh, Value::SmallInt(4611686018427387904)).unwrap(),
        Value::Bool(true)
    );
    // DIVMOD quirk (probed, p8): `divmod(-(2**62), -1)` →
    // `(-4611686018427387904, 0)` — the quotient mistags to
    // -2**62, while `//` alone gives the correct 2**62.
    let dm = env
        .binop(BinaryOp::Divmod, Value::SmallInt(-4611686018427387904), Value::SmallInt(-1))
        .unwrap();
    let want_dm = env.tuple_v(&[Value::SmallInt(-4611686018427387904), Value::SmallInt(0)]);
    assert_eq!(env.binop(BinaryOp::Equal, dm, want_dm).unwrap(), Value::Bool(true));
    let fl = env
        .binop(BinaryOp::FloorDivide, Value::SmallInt(-4611686018427387904), Value::SmallInt(-1))
        .unwrap();
    assert!(matches!(fl, Value::Heap(_)), "// -1 must be a heap int 2**62, got {fl:?}");
    assert_eq!(
        env.binop(BinaryOp::Equal, fl, Value::SmallInt(4611686018427387904)).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(env.binop(BinaryOp::Rshift, Value::SmallInt(5), Value::SmallInt(100)).unwrap(), Value::SmallInt(0));
    assert_eq!(env.binop(BinaryOp::Less, two, three).unwrap(), Value::Bool(true));
    assert_eq!(env.binop(BinaryOp::More, two, three).unwrap(), Value::Bool(false));
    assert_eq!(env.binop(BinaryOp::Equal, two, two).unwrap(), Value::Bool(true));
    assert_eq!(env.binop(BinaryOp::NotEqual, two, three).unwrap(), Value::Bool(true));
    assert_eq!(env.binop(BinaryOp::Is, two, two).unwrap(), Value::Bool(true));
    // `is not` is lowered by the emitter to `is` + `not` —
    // `BinaryOp::IsNot` never reaches `mp_binary_op` (the Zig
    // would raise `binopUnsupported` for it too).
    let is_res = env.binop(BinaryOp::Is, two, three).unwrap();
    assert_eq!(env.unop(UnaryOp::Not, is_res).unwrap(), Some(Value::Bool(true)));
}

#[test]
fn binary_true_divide_and_negative_power_are_floats() {
    let mut env = Env::new();
    let one = Value::SmallInt(1);
    let two = Value::SmallInt(2);
    // `1 / 2` and `2 ** -1` are inlined f64 ops in runtime.zig.
    let half = env.binop(BinaryOp::TrueDivide, one, two).unwrap();
    assert!(matches!(half, Value::Heap(_)));
    assert_eq!(env.binop(BinaryOp::Equal, half, half).unwrap(), Value::Bool(true));
    let p = env.binop(BinaryOp::Power, two, Value::SmallInt(-1)).unwrap();
    assert!(matches!(p, Value::Heap(_)));
    // `0 ** 0 == 1`, `1 + True == 2`.
    assert_eq!(env.binop(BinaryOp::Power, Value::SmallInt(0), Value::SmallInt(0)).unwrap(), Value::SmallInt(1));
    assert_eq!(env.binop(BinaryOp::Add, one, Value::Bool(true)).unwrap(), Value::SmallInt(2));
}

#[test]
fn binary_divmod_and_go_big() {
    let mut env = Env::new();
    let dm = env.binop(BinaryOp::Divmod, Value::SmallInt(10), Value::SmallInt(3)).unwrap();
    let want = env.tuple_v(&[Value::SmallInt(3), Value::SmallInt(1)]);
    assert_eq!(env.binop(BinaryOp::Equal, dm, want).unwrap(), Value::Bool(true));
    // `1 << 63` and `(2**62) * 2` escape the small-int window
    // (`go_big`) and still compare equal to the big values.
    let big = env.binop(BinaryOp::Lshift, Value::SmallInt(1), Value::SmallInt(63)).unwrap();
    assert!(matches!(big, Value::Heap(_)));
    let big2 = env.binop(BinaryOp::Multiply, Value::SmallInt(4611686018427387904), Value::SmallInt(2)).unwrap();
    assert_eq!(env.binop(BinaryOp::Equal, big, big2).unwrap(), Value::Bool(true));
}

#[test]
fn binary_zero_division_errors() {
    let mut env = Env::new();
    for op in [BinaryOp::FloorDivide, BinaryOp::TrueDivide, BinaryOp::Modulo] {
        let err = env.binop(op, Value::SmallInt(1), Value::SmallInt(0)).unwrap_err();
        assert_eq!(err.exc_name(), "ZeroDivisionError");
        assert_eq!(err.message(), "divide by zero");
    }
    let err = env.binop(BinaryOp::Lshift, Value::SmallInt(1), Value::SmallInt(-1)).unwrap_err();
    assert_eq!(err.exc_name(), "ValueError");
}

#[test]
fn binary_int_times_sequence_delegates() {
    let mut env = Env::new();
    // `2 * (1, 2)` and `2 * [1]` — the int slot declines and
    // the generic loop recurses swapped (C7 `extra_cases`).
    let tup = env.tuple_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let got = env.binop(BinaryOp::Multiply, Value::SmallInt(2), tup).unwrap();
    let want = env.tuple_v(&[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(1), Value::SmallInt(2)]);
    assert_eq!(env.binop(BinaryOp::Equal, got, want).unwrap(), Value::Bool(true));
    let list = env.list_v(&[Value::SmallInt(1)]);
    let got = env.binop(BinaryOp::Multiply, Value::SmallInt(2), list).unwrap();
    let want = env.list_v(&[Value::SmallInt(1), Value::SmallInt(1)]);
    assert_eq!(env.binop(BinaryOp::Equal, got, want).unwrap(), Value::Bool(true));
    // `2 * "ab"` — same delegation through the str slot.
    let s = env.str_v(b"ab");
    let got = env.binop(BinaryOp::Multiply, Value::SmallInt(2), s).unwrap();
    let want = env.str_v(b"abab");
    assert_eq!(env.binop(BinaryOp::Equal, got, want).unwrap(), Value::Bool(true));
}

#[test]
fn binary_tuple_add_and_contains() {
    let mut env = Env::new();
    let a = env.tuple_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let b = env.tuple_v(&[Value::SmallInt(3)]);
    let got = env.binop(BinaryOp::Add, a, b).unwrap();
    let want = env.tuple_v(&[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]);
    assert_eq!(env.binop(BinaryOp::Equal, got, want).unwrap(), Value::Bool(true));
    let list = env.list_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    assert_eq!(env.binop(BinaryOp::In, Value::SmallInt(1), list).unwrap(), Value::Bool(true));
    // `not in` is lowered by the emitter to `in` + `not` —
    // `BinaryOp::NotIn` never reaches `mp_binary_op` (the Zig
    // would raise `binopUnsupported` for it too), so the
    // negation is tested through `unary_op(NOT, ...)`.
    let nine_in = env.binop(BinaryOp::In, Value::SmallInt(9), list).unwrap();
    assert_eq!(
        env.unop(UnaryOp::Not, nine_in).unwrap(),
        Some(Value::Bool(true))
    );
    let a_q = env.q(b"a");
    let d = env.dict_v(&[(Value::Qstr(a_q), Value::SmallInt(1))]);
    assert_eq!(env.binop(BinaryOp::In, Value::Qstr(a_q), d).unwrap(), Value::Bool(true));
}

#[test]
fn binary_unsupported_messages() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // `1 in 2` — the small-int chain catches IN before the
    // swap, so the op name formats empty (probed).
    let err = env.binop(BinaryOp::In, Value::SmallInt(1), Value::SmallInt(2)).unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "unsupported types for : 'int', 'int'");
    // `len + 1` — a header-None FunBuiltin names "function".
    let err = env.binop(BinaryOp::Add, next_obj, Value::SmallInt(1)).unwrap_err();
    assert_eq!(err.message(), "unsupported types for __add__: 'function', 'int'");
    // `None + 1`, `1 + None`.
    let err = env.binop(BinaryOp::Add, Value::None, Value::SmallInt(1)).unwrap_err();
    assert_eq!(err.message(), "unsupported types for __add__: 'NoneType', 'int'");
    let err = env.binop(BinaryOp::Add, Value::SmallInt(1), Value::None).unwrap_err();
    assert_eq!(err.message(), "unsupported types for __add__: 'int', 'NoneType'");
    // `"a" - 1` — the str slot declines.
    let s = env.str_v(b"a");
    let err = env.binop(BinaryOp::Subtract, s, Value::SmallInt(1)).unwrap_err();
    assert_eq!(err.message(), "unsupported types for __sub__: 'str', 'int'");
}

#[test]
fn binary_float_arm_waits_for_c18() {
    let mut env = Env::new();
    // The float type is a plain placeholder (no binary slot) —
    // `1 + 2.5` raises the vendor-NORMAL TypeError until C18
    // wires the float slot. The exact C18 cases are listed in
    // the result file.
    let float_t = env.types.float;
    let mut ctx = env.ctx();
    let f = Value::Heap(ctx.alloc(Some(float_t), Obj::Float(2.5)));
    drop(ctx);
    let err = env.binop(BinaryOp::Add, Value::SmallInt(1), f).unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
}

#[test]
fn binary_exception_match() {
    let mut env = Env::new();
    let si_ref = env.refs.stop_iteration;
    let si_t = Value::Heap(si_ref);
    let ve_t = Value::Heap(env.exc.by_name("ValueError").unwrap());
    let mut ctx = env.ctx();
    let ve = crate::objects::objtype::objexcept::new_exception(
        &mut ctx,
        si_ref,
    );
    drop(ctx);
    // A StopIteration instance matches StopIteration (single
    // type) and a tuple containing it; not ValueError.
    assert_eq!(env.binop(BinaryOp::ExceptionMatch, ve, si_t).unwrap(), Value::Bool(true));
    assert_eq!(env.binop(BinaryOp::ExceptionMatch, ve, ve_t).unwrap(), Value::Bool(false));
    let tup = env.tuple_v(&[ve_t, si_t]);
    assert_eq!(env.binop(BinaryOp::ExceptionMatch, ve, tup).unwrap(), Value::Bool(true));
}

