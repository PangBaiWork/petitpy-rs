//! Lane C12b tests — `objexcept` + `objmodule` + `objobject`.
//!
//! Local fixture only (the shared `containers::tests` fixture is
//! C12a's to touch as well — this lane imports just its
//! `empty_dict` / `make_type` helpers). All reference values were
//! probed on the reference binary 2026-11-17 (`/tmp/c12b_p1.py`,
//! `/tmp/c12b_p2.py`, `/tmp/c12b_p3.py`, `/tmp/c12b_p4.py`,
//! `/tmp/c12b_p5.py`).
//!
//! Borrow note: `Fx::ctx()` borrows the whole fixture mutably,
//! so every test binds the `GcRef`s it needs into locals BEFORE
//! creating its `CallCtx`.

use crate::memory::gc::{CallCtx, GcArena, RootSet};
use crate::objects::containers::objdict::dict_type_data;
use crate::objects::containers::objlist::list_type_data;
use crate::objects::containers::objrange::range_type_data;
use crate::objects::containers::objset::{frozenset_type_data, set_type_data};
use crate::objects::containers::objtuple::tuple_type_data;
use crate::objects::containers::tests::{empty_dict, make_type};
use crate::objects::obj::{BuiltinTypes, FLAG_INSTANCE_TYPE};
use crate::objects::objtype::objexcept::{
    errno_name, exception_add_traceback, exception_attr,
    exception_clear_traceback, exception_get_cause, exception_get_traceback,
    exception_get_value, exception_make_new, exception_match, exception_print,
    exception_set_cause, exception_store_attr, is_exception_instance, is_exception_type,
    is_exception_type_obj, is_native_exception_instance, new_exception, new_exception_args,
    new_exception_msg, type_is_subclass_of, ExcError,
};
use crate::objects::objtype::objexcept_types::{build_exception_types, ExceptionTypes};
use crate::objects::objtype::objmodule::module_type_data;
use crate::objects::objtype::objobject::{object_locals_dict, object_type_data};
use crate::objects::primitives::objbool::bool_type_data;
use crate::objects::primitives::objint::int_type_data;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintFn, PrintKind};
use crate::objects::strings::objstr_methods_b::str_type_data;
use crate::objects::value::{DictData, GcRef, MapData, Obj, StrData, TypeData, Value};
use crate::runtime::qstr::RuntimeQstr;

pub(crate) fn q(name: &[u8]) -> u32 {
    static_qstr_id(name).expect("test qstr is in the static table")
}

pub(crate) struct Fx {
    pub(crate) arena: GcArena,
    pub(crate) roots: RootSet,
    pub(crate) types: BuiltinTypes,
    pub(crate) pool: RuntimeQstr,
    pub(crate) object: GcRef,
    pub(crate) module_type: GcRef,
    pub(crate) exc: ExceptionTypes,
}

impl Fx {
    pub(crate) fn new() -> Fx {
        let mut arena = GcArena::new();
        let mut roots = RootSet::new();
        let pool = RuntimeQstr::new();
        // `type` (bootstrap root, header None — the ObjHeader doc
        // pattern: the type type's own type is itself).
        let type_dict = empty_dict(&mut arena, &roots);
        let type_ = arena.alloc(
            None,
            Obj::Type(TypeData {
                name: q(b"type"),
                flags: 0,
                base: None,
                mro: Vec::new(),
                dict: type_dict,
                unary_op: None,
                binary_op: None,
                call: None,
                getiter: None,
                subscr: None,
                buffer: None,
                print: None,
            }),
            &roots,
        );
        roots.push(Value::Heap(type_));
        // Real slotted types where a slot matters (int/str/bool/
        // dict/tuple/list/set/frozenset/range); plain types for
        // the rest. (Each dict is bound first: `arena.alloc` and
        // `empty_dict` cannot both borrow `arena` mutably in one
        // expression.)
        let d = empty_dict(&mut arena, &roots);
        let int = arena.alloc(None, Obj::Type(int_type_data(d)), &roots);
        roots.push(Value::Heap(int));
        let d = empty_dict(&mut arena, &roots);
        let str_ = arena.alloc(None, Obj::Type(str_type_data(q(b"str"), d)), &roots);
        roots.push(Value::Heap(str_));
        let bytes = make_type(&mut arena, &roots, b"bytes");
        roots.push(Value::Heap(bytes));
        let d = empty_dict(&mut arena, &roots);
        let bool_ = arena.alloc(None, Obj::Type(bool_type_data(d)), &roots);
        roots.push(Value::Heap(bool_));
        let none_type = make_type(&mut arena, &roots, b"NoneType");
        roots.push(Value::Heap(none_type));
        let float = make_type(&mut arena, &roots, b"float");
        roots.push(Value::Heap(float));
        let complex = make_type(&mut arena, &roots, b"complex");
        roots.push(Value::Heap(complex));
        let d = empty_dict(&mut arena, &roots);
        let tuple = arena.alloc(None, Obj::Type(tuple_type_data(d)), &roots);
        roots.push(Value::Heap(tuple));
        let d = empty_dict(&mut arena, &roots);
        let list = arena.alloc(None, Obj::Type(list_type_data(d)), &roots);
        roots.push(Value::Heap(list));
        let d = empty_dict(&mut arena, &roots);
        let dict = arena.alloc(None, Obj::Type(dict_type_data(d)), &roots);
        roots.push(Value::Heap(dict));
        let d = empty_dict(&mut arena, &roots);
        let set = arena.alloc(None, Obj::Type(set_type_data(d)), &roots);
        roots.push(Value::Heap(set));
        let d = empty_dict(&mut arena, &roots);
        let frozenset = arena.alloc(None, Obj::Type(frozenset_type_data(d)), &roots);
        roots.push(Value::Heap(frozenset));
        let d = empty_dict(&mut arena, &roots);
        let range = arena.alloc(None, Obj::Type(range_type_data(d)), &roots);
        roots.push(Value::Heap(range));
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
            dict,
            set,
            frozenset,
            range,
            type_,
        };
        let mut ctx = CallCtx::new(&mut arena, &mut roots, &types, &pool);
        // `object` with its real locals dict; MRO [self].
        let locals = object_locals_dict(&mut ctx);
        let object = ctx.alloc(Some(type_), Obj::Type(object_type_data(locals)));
        ctx.roots.push(Value::Heap(object));
        if let Obj::Type(td) = ctx.arena.get_mut(object) {
            td.mro = vec![object];
        }
        // The 28 exception types (MROs patched by the builder).
        let exc = build_exception_types(&mut ctx, object);
        // `module` type; MRO [self, object].
        let module_dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
        let module_type = ctx.alloc(
            Some(type_),
            Obj::Type(module_type_data(module_dict, object)),
        );
        ctx.roots.push(Value::Heap(module_type));
        if let Obj::Type(td) = ctx.arena.get_mut(module_type) {
            td.mro = vec![module_type, object];
        }
        drop(ctx);
        Fx {
            arena,
            roots,
            types,
            pool,
            object,
            module_type,
            exc,
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        CallCtx::new(&mut self.arena, &mut self.roots, &self.types, &self.pool)
    }

    /// Intern a DYNAMIC qstr (not in the static table) — e.g.
    /// user identifiers like `"foo"` or `"f.py"`. Must be called
    /// before `ctx()` (the pool is borrowed by the `CallCtx`).
    pub(crate) fn dq(&mut self, name: &[u8]) -> u32 {
        self.pool.from_strn(name).expect("dynamic qstr interning") as u32
    }

    /// A heap `str` with the real header type.
    pub(crate) fn mk_str(&mut self, s: &str) -> Value {
        let str_ = self.types.str_;
        let mut ctx = self.ctx();
        let r = ctx.alloc(
            Some(str_),
            Obj::Str(StrData { bytes: s.as_bytes().to_vec() }),
        );
        Value::Heap(r)
    }

    /// An instance-type `TypeData` (flags 0x0200), header `type`.
    /// The type name is a DYNAMIC qstr (a user class name is never
    /// in the static table).
    pub(crate) fn mk_instance_type(&mut self, name: &[u8]) -> GcRef {
        let object = self.object;
        let type_ = self.types.type_;
        let name_id = self.pool.from_strn(name).expect("dynamic type name") as u32;
        let mut ctx = self.ctx();
        let dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
        let data = TypeData {
            name: name_id,
            flags: FLAG_INSTANCE_TYPE,
            base: Some(object),
            mro: vec![object],
            dict,
            unary_op: None,
            binary_op: None,
            call: None,
            getiter: None,
            subscr: None,
            buffer: None,
            print: None,
        };
        let r = ctx.alloc(Some(type_), Obj::Type(data));
        ctx.roots.push(Value::Heap(r));
        r
    }
}

/// Render a value through a print fn into a String.
pub(crate) fn render(ctx: &mut CallCtx, v: Value, kind: PrintKind, f: PrintFn) -> String {
    let mut s = String::new();
    f(ctx, v, kind, &mut s);
    s
}

/// Read the `args` of an exception value.
fn exc_args(arena: &GcArena, exc: Value) -> Vec<Value> {
    match exc {
        Value::Heap(r) => match arena.get(r) {
            Obj::Exception(e) => e.args.clone(),
            _ => panic!("not an exception"),
        },
        _ => panic!("not an exception"),
    }
}

// ── objexcept: make_new / constructors ───────────────────────────────

#[test]
fn make_new_stores_args() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let mut ctx = fx.ctx();
    let e = exception_make_new(&mut ctx, ve, &[Value::SmallInt(1), Value::SmallInt(2)], 0)
        .expect("make_new");
    assert_eq!(exc_args(ctx.arena, e), vec![Value::SmallInt(1), Value::SmallInt(2)]);
    assert_eq!(ctx.arena.header(match e { Value::Heap(r) => r, _ => unreachable!() }).type_, Some(ve));
}

#[test]
fn make_new_no_args_and_kw_mismatch() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let mut ctx = fx.ctx();
    let e = exception_make_new(&mut ctx, ve, &[], 0).expect("0-arg make_new");
    assert!(exc_args(ctx.arena, e).is_empty());
    assert!(matches!(
        exception_make_new(&mut ctx, ve, &[Value::SmallInt(1)], 1),
        Err(ExcError::TypeError(_))
    ));
}

#[test]
fn new_exception_msg_builds_str_arg() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let mut ctx = fx.ctx();
    let e = new_exception_msg(&mut ctx, ve, "x");
    let args = exc_args(ctx.arena, e);
    assert_eq!(args.len(), 1);
    assert!(matches!(&args[0], Value::Heap(r) if matches!(ctx.arena.get(*r), Obj::Str(s) if s.bytes == b"x")));
}

#[test]
fn new_exception_and_args_helpers() {
    let mut fx = Fx::new();
    let te = fx.exc.type_error;
    let ke = fx.exc.key_error;
    let k = fx.dq(b"k");
    let mut ctx = fx.ctx();
    let e0 = new_exception(&mut ctx, te);
    assert!(exc_args(ctx.arena, e0).is_empty());
    let e1 = new_exception_args(&mut ctx, ke, &[Value::Qstr(k)]);
    assert_eq!(exc_args(ctx.arena, e1), vec![Value::Qstr(k)]);
}

// ── objexcept: traceback / cause / attr ──────────────────────────────

#[test]
fn traceback_add_get_clear() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let f = fx.dq(b"f.py");
    let g = fx.dq(b"g.py");
    let mut ctx = fx.ctx();
    let e = new_exception(&mut ctx, ve);
    exception_add_traceback(&mut ctx, e, f, 3, 0);
    exception_add_traceback(&mut ctx, e, g, 7, 1);
    let tb = exception_get_traceback(ctx.arena, e);
    assert_eq!(tb.len(), 2);
    assert_eq!(tb[0].line, 3);
    assert_eq!(tb[1].line, 7);
    exception_clear_traceback(&mut ctx, e);
    assert!(exception_get_traceback(ctx.arena, e).is_empty());
}

#[test]
fn cause_and_suppress_context() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let ke = fx.exc.key_error;
    let mut ctx = fx.ctx();
    let e = new_exception(&mut ctx, ve);
    // Fresh exception: cause None, suppress False (probed).
    assert_eq!(exception_get_cause(ctx.arena, e), Value::None);
    assert_eq!(
        exception_attr(&mut ctx, e, q(b"__suppress_context__")).unwrap(),
        Some(Value::Bool(false))
    );
    let cause = new_exception_msg(&mut ctx, ke, "k");
    exception_set_cause(&mut ctx, e, cause);
    assert_eq!(exception_get_cause(ctx.arena, e), cause);
    assert_eq!(
        exception_attr(&mut ctx, e, q(b"__cause__")).unwrap(),
        Some(cause)
    );
    assert_eq!(
        exception_attr(&mut ctx, e, q(b"__suppress_context__")).unwrap(),
        Some(Value::Bool(true))
    );
    // __context__ is always None (probed).
    assert_eq!(
        exception_attr(&mut ctx, e, q(b"__context__")).unwrap(),
        Some(Value::None)
    );
}

#[test]
fn attr_args_is_fresh_tuple_each_time() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let mut ctx = fx.ctx();
    let e = new_exception_args(&mut ctx, ve, &[Value::SmallInt(1)]);
    let a1 = exception_attr(&mut ctx, e, q(b"args")).unwrap().expect("args");
    let a2 = exception_attr(&mut ctx, e, q(b"args")).unwrap().expect("args");
    // Fresh tuple per access (identity divergence, documented).
    assert_ne!(a1, a2);
    for a in [a1, a2] {
        assert!(matches!(a, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Tuple(items) if items == &[Value::SmallInt(1)])));
    }
}

#[test]
fn attr_traceback_load_is_none_and_store_clears() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let f = fx.dq(b"f.py");
    let nope = fx.dq(b"__nope__");
    let mut ctx = fx.ctx();
    let e = new_exception(&mut ctx, ve);
    exception_add_traceback(&mut ctx, e, f, 3, 0);
    // No load branch for __traceback__ (probed: AttributeError).
    assert_eq!(exception_attr(&mut ctx, e, q(b"__traceback__")).unwrap(), None);
    assert!(exception_store_attr(&mut ctx, e, q(b"__traceback__"), Some(Value::None)).unwrap());
    assert!(exception_get_traceback(ctx.arena, e).is_empty());
    assert!(!exception_store_attr(&mut ctx, e, nope, Some(Value::None)).unwrap());
}

#[test]
fn attr_value_and_errno_aliases() {
    let mut fx = Fx::new();
    let si = fx.exc.stop_iteration;
    let oe = fx.exc.os_error;
    let mut ctx = fx.ctx();
    let stop = new_exception_args(&mut ctx, si, &[Value::SmallInt(42)]);
    assert_eq!(exception_get_value(ctx.arena, stop), Value::SmallInt(42));
    assert_eq!(
        exception_attr(&mut ctx, stop, q(b"value")).unwrap(),
        Some(Value::SmallInt(42))
    );
    let os = new_exception_args(&mut ctx, oe, &[Value::SmallInt(2)]);
    assert_eq!(
        exception_attr(&mut ctx, os, q(b"errno")).unwrap(),
        Some(Value::SmallInt(2))
    );
}

// ── objexcept: printing ──────────────────────────────────────────────

#[test]
fn print_repr_value_error_one_arg() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let mut ctx = fx.ctx();
    let e = new_exception_msg(&mut ctx, ve, "x");
    // Probed: repr(ValueError('x')) == "ValueError('x',)".
    assert_eq!(render(&mut ctx, e, PrintKind::Repr, exception_print), "ValueError('x',)");
}

#[test]
fn print_str_key_error_single_arg() {
    let mut fx = Fx::new();
    let ke = fx.exc.key_error;
    let mut ctx = fx.ctx();
    let e = new_exception_msg(&mut ctx, ke, "k");
    // Probed: str(KeyError('k')) == 'k' (NOT "'k'").
    assert_eq!(render(&mut ctx, e, PrintKind::Str, exception_print), "k");
}

#[test]
fn print_str_type_error_two_args_tuple_form() {
    let mut fx = Fx::new();
    let te = fx.exc.type_error;
    let a = fx.mk_str("a");
    let b = fx.mk_str("b");
    let mut ctx = fx.ctx();
    let e = new_exception_args(&mut ctx, te, &[a, b]);
    // Probed: str(TypeError('a','b')) == "('a', 'b')".
    assert_eq!(render(&mut ctx, e, PrintKind::Str, exception_print), "('a', 'b')");
}

#[test]
fn print_str_oserror_known_errno() {
    let mut fx = Fx::new();
    let oe = fx.exc.os_error;
    let msg = fx.mk_str("x");
    let mut ctx = fx.ctx();
    let e = new_exception_args(&mut ctx, oe, &[Value::SmallInt(2), msg]);
    // Probed: str(OSError(2, 'x')) == '[Errno 2] ENOENT: x'.
    assert_eq!(render(&mut ctx, e, PrintKind::Str, exception_print), "[Errno 2] ENOENT: x");
    let e1 = new_exception_args(&mut ctx, oe, &[Value::SmallInt(2)]);
    assert_eq!(render(&mut ctx, e1, PrintKind::Str, exception_print), "[Errno 2] ENOENT");
}

#[test]
fn print_str_oserror_unknown_errno_tuple_form() {
    let mut fx = Fx::new();
    let oe = fx.exc.os_error;
    let msg = fx.mk_str("x");
    let mut ctx = fx.ctx();
    let e = new_exception_args(&mut ctx, oe, &[Value::SmallInt(999), msg]);
    // Probed: unknown errno 999 falls back to the tuple form.
    assert_eq!(render(&mut ctx, e, PrintKind::Str, exception_print), "(999, 'x')");
}

#[test]
fn print_repr_no_args_and_errno_table() {
    let mut fx = Fx::new();
    let oe = fx.exc.os_error;
    let mut ctx = fx.ctx();
    let e = new_exception(&mut ctx, oe);
    assert_eq!(render(&mut ctx, e, PrintKind::Repr, exception_print), "OSError()");
    assert_eq!(errno_name(2), Some("ENOENT"));
    assert_eq!(errno_name(111), Some("ECONNREFUSED"));
    assert_eq!(errno_name(999), None);
}

// ── objexcept: hierarchy / predicates / match ────────────────────────

#[test]
fn hierarchy_mros() {
    let fx = Fx::new();
    let mro = |t: GcRef| match fx.arena.get(t) {
        Obj::Type(td) => td.mro.clone(),
        _ => panic!("not a type"),
    };
    // Probed: ValueError.__mro__ == (ValueError, Exception,
    // BaseException, object).
    assert_eq!(
        mro(fx.exc.value_error),
        vec![fx.exc.value_error, fx.exc.exception, fx.exc.base_exception, fx.object]
    );
    assert_eq!(
        mro(fx.exc.key_error),
        vec![fx.exc.key_error, fx.exc.lookup_error, fx.exc.exception, fx.exc.base_exception, fx.object]
    );
    assert_eq!(mro(fx.exc.base_exception), vec![fx.exc.base_exception, fx.object]);
    assert_eq!(mro(fx.object), vec![fx.object]);
}

#[test]
fn subclass_predicates() {
    let fx = Fx::new();
    assert!(type_is_subclass_of(&fx.arena, fx.exc.value_error, fx.exc.exception));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.value_error, fx.exc.base_exception));
    assert!(!type_is_subclass_of(&fx.arena, fx.exc.value_error, fx.exc.type_error));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.exception, fx.exc.base_exception));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.base_exception, fx.object));
    assert!(type_is_subclass_of(&fx.arena, fx.object, fx.object));
    // Probed: KeyboardInterrupt / SystemExit are NOT Exception
    // subclasses (they hang off BaseException).
    assert!(!type_is_subclass_of(&fx.arena, fx.exc.keyboard_interrupt, fx.exc.exception));
    assert!(!type_is_subclass_of(&fx.arena, fx.exc.system_exit, fx.exc.exception));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.keyboard_interrupt, fx.exc.base_exception));
    // Probed: StopAsyncIteration IS an Exception subclass.
    assert!(type_is_subclass_of(&fx.arena, fx.exc.stop_async_iteration, fx.exc.exception));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.unicode_error, fx.exc.value_error));
    assert!(type_is_subclass_of(&fx.arena, fx.exc.module_not_found_error, fx.exc.import_error));
}

#[test]
fn is_exception_type_and_instance() {
    let mut fx = Fx::new();
    let base = fx.exc.base_exception;
    let ve = fx.exc.value_error;
    let int_t = fx.types.int;
    let mut ctx = fx.ctx();
    assert!(is_exception_type(ctx.arena, Value::Heap(ve), base));
    assert!(!is_exception_type(ctx.arena, Value::Heap(int_t), base));
    assert!(!is_exception_type(ctx.arena, Value::SmallInt(1), base));
    let inst = new_exception(&mut ctx, ve);
    assert!(is_exception_instance(ctx.arena, inst, base));
    assert!(is_native_exception_instance(ctx.arena, inst, base));
    assert!(!is_exception_instance(ctx.arena, Value::SmallInt(1), base));
    assert!(!is_exception_instance(ctx.arena, Value::Heap(ve), base));
    // is_exception_type_obj: the print-slot identity (C9c precedent).
    assert!(is_exception_type_obj(ctx.arena, ve));
    assert!(!is_exception_type_obj(ctx.arena, int_t));
}

#[test]
fn exception_match_classinfo() {
    let mut fx = Fx::new();
    let ve = fx.exc.value_error;
    let ex = fx.exc.exception;
    let be = fx.exc.base_exception;
    let te = fx.exc.type_error;
    let int_t = fx.types.int;
    let tuple_t = fx.types.tuple;
    let mut ctx = fx.ctx();
    let inst = new_exception(&mut ctx, ve);
    assert!(exception_match(ctx.arena, inst, Value::Heap(ve)));
    assert!(exception_match(ctx.arena, inst, Value::Heap(ex)));
    assert!(exception_match(ctx.arena, inst, Value::Heap(be)));
    assert!(!exception_match(ctx.arena, inst, Value::Heap(te)));
    let tup = ctx.alloc(
        Some(tuple_t),
        Obj::Tuple(vec![Value::Heap(te), Value::Heap(ve)]),
    );
    assert!(exception_match(ctx.arena, inst, Value::Heap(tup)));
    assert!(!exception_match(ctx.arena, inst, Value::Heap(int_t)));
    assert!(!exception_match(ctx.arena, Value::SmallInt(1), Value::Heap(ve)));
}

#[test]
fn generator_exit_instance_is_exception() {
    let mut fx = Fx::new();
    let ge_t = fx.exc.generator_exit;
    let base = fx.exc.base_exception;
    let mut ctx = fx.ctx();
    // C10b handover item 4: the generator's close() raises this.
    let ge = crate::objects::objtype::objexcept_types::generator_exit_instance(&mut ctx, ge_t);
    assert!(is_exception_instance(ctx.arena, ge, base));
    assert!(exc_args(ctx.arena, ge).is_empty());
}
