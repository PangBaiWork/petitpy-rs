//! Lane C14 tests (part 2): method/attribute loading,
//! iteration, resume, imports. Part 1 (`runtime_tests.rs`)
//! covers names, unary/binary dispatch, the call protocol,
//! and unpack; the `Env` lives there.

use super::runtime_tests::{echo, seven, Env};
use crate::memory::gc::CallCtx;
use crate::objects::callables::objgenerator::{GenError, GenResume, ResumeKind};
use crate::objects::value::MapData;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objexcept::{
    is_exception_instance, new_exception,
};
use crate::objects::objtype::objmodule::new_module;
use crate::objects::value::{
    BoundMethData, DictData, GenState, GeneratorData, ModuleData, Obj, Value,
};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::runtime::{Builtins, RtError, RuntimeSeams};
use crate::runtime::runtime_call::call_function_n_kw;
use crate::runtime::runtime_iter::{getiter, iternext, iternext_allow_raise, IterNext};
use crate::runtime::runtime_method::{
    builtin_next, load_attr, load_method, load_method_maybe, load_method_protected,
    next_builtin_obj, store_attr,
};
use crate::runtime::runtime_names::dict_lookup_qstr;
use crate::runtime::runtime_resume::{make_raise_obj, resume, resume_gen, ResumeOutcome};
use crate::runtime::runtime_import::{import_all, import_from, import_name};

/// A `FunBuiltin` body returning 42 (for the import tests).
fn forty_two(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Ok(Value::SmallInt(42))
}

fn gen_value(env: &mut Env) -> Value {
    let gen_t = env.gen_instance;
    let mut ctx = env.ctx();
    Value::Heap(ctx.alloc(
        Some(gen_t),
        Obj::Generator(GeneratorData {
            fun: gen_t,
            state: GenState::New,
            pend_throw: None,
            stop_value: None,
        }),
    ))
}

// ── method / attribute loading ──────────────────────────────────────

#[test]
fn load_method_maybe_class_and_missing() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let object = env.types.object;
    let inst = env.instance_of(object);
    let class_q = env.q(b"__class__");
    let nope = env.q(b"__nope__");
    let mut ctx = env.ctx();
    let dest = load_method_maybe(&mut ctx, inst, class_q, next_obj)
        .unwrap()
        .expect("__class__");
    assert_eq!(dest.func, Value::Heap(object));
    assert_eq!(dest.self_, None);
    assert!(load_method_maybe(&mut ctx, inst, nope, next_obj).unwrap().is_none());
    let err = load_method(&mut ctx, inst, nope, next_obj).unwrap_err();
    assert_eq!(err.exc_name(), "AttributeError");
    assert!(err.message().contains("__nope__"), "msg: {}", err.message());
}

#[test]
fn load_method_maybe_next_fast_path_on_generator() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let gen = gen_value(&mut env);
    let next_q = env.q(b"__next__");
    let mut ctx = env.ctx();
    let dest = load_method_maybe(&mut ctx, gen, next_q, next_obj)
        .unwrap()
        .expect("__next__ fast path");
    assert_eq!(dest.func, next_obj);
    assert_eq!(dest.self_, Some(gen));
}

#[test]
fn load_method_maybe_native_method_binds() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // A FunBuiltin installed in the LIST type's dict binds
    // (list is not an instance type — the C12a payload rule).
    let m_q = env.q(b"m");
    let m = env.fun_v(b"m", 0, 0, seven);
    env.set_local(env.types.list, m_q, m);
    let list = env.list_v(&[Value::SmallInt(1)]);
    let mut ctx = env.ctx();
    let dest = load_method_maybe(&mut ctx, list, m_q, next_obj)
        .unwrap()
        .expect("list.m");
    assert_eq!(dest.func, m);
    assert_eq!(dest.self_, Some(list));
}

#[test]
fn load_method_maybe_user_method_does_not_bind_builtin() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // A FunBuiltin in a USER class dict does NOT bind (the
    // Zig's `mp_convert_member_lookup`: a builtin fn on an
    // instance type is a staticmethod — CPython agrees, a
    // builtin function is not a binding descriptor).
    let object = env.types.object;
    let klass = env.user_type(b"K", Some(object), vec![]);
    {
        let ctx = env.ctx();
        if let Obj::Type(t) = ctx.arena.get_mut(klass) {
            t.mro = vec![klass, object];
        }
    }
    let m_q = env.q(b"m");
    let m = env.fun_v(b"m", 0, 0, seven);
    env.set_local(klass, m_q, m);
    let inst = env.instance_of(klass);
    let mut ctx = env.ctx();
    let dest = load_method_maybe(&mut ctx, inst, m_q, next_obj)
        .unwrap()
        .expect("K().m");
    assert_eq!(dest.func, m);
    assert_eq!(dest.self_, None);
}

#[test]
fn load_method_protected_swallows_pyraise() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    // A type whose attr slot always raises: protected swallows
    // it (the corpus `builtin_getattr.py` case) for both
    // `catch_all_exc` values.
    fn raising_attr(
        _ctx: &mut CallCtx,
        _obj: Value,
        _attr: u32,
    ) -> PyResult<Option<Value>> {
        Err(PyError::PyRaise)
    }
    let object = env.types.object;
    let klass = env.user_type(b"R", Some(object), vec![]);
    {
        let ctx = env.ctx();
        if let Obj::Type(t) = ctx.arena.get_mut(klass) {
            t.mro = vec![klass, object];
            t.attr = Some(raising_attr);
        }
    }
    let inst = env.instance_of(klass);
    let attr_q = env.q(b"anything");
    let plain = env.instance_of(object);
    let mut ctx = env.ctx();
    assert!(load_method_protected(&mut ctx, inst, attr_q, false, next_obj)
        .unwrap()
        .is_none());
    assert!(load_method_protected(&mut ctx, inst, attr_q, true, next_obj)
        .unwrap()
        .is_none());
    // A missing attr on a plain object is also `None`.
    assert!(load_method_protected(&mut ctx, plain, attr_q, false, next_obj)
        .unwrap()
        .is_none());
}

#[test]
fn load_attr_binds_and_plain_value() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let m_q = env.q(b"m");
    let m = env.fun_v(b"m", 0, 0, seven);
    env.set_local(env.types.list, m_q, m);
    let v_q = env.q(b"v");
    env.set_local(env.types.list, v_q, Value::SmallInt(9));
    let list = env.list_v(&[]);
    let mut ctx = env.ctx();
    let bound = load_attr(&mut ctx, list, m_q, next_obj).unwrap();
    match bound {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::BoundMeth(BoundMethData { func, self_ }) => {
                assert_eq!(*func, m);
                assert_eq!(*self_, list);
            }
            _ => panic!("expected a bound method, got {bound:?}"),
        },
        _ => panic!("expected a heap value"),
    }
    assert_eq!(load_attr(&mut ctx, list, v_q, next_obj).unwrap(), Value::SmallInt(9));
}

#[test]
fn store_attr_instance_type_module() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let refs = env.refs;
    let globals = env.globals;
    let builtins0 = env.builtins.clone();
    let a_q = env.q(b"a");
    let nope = env.q(b"nope");
    // Instance: store then load back.
    let object = env.types.object;
    let klass = env.user_type(b"S", Some(object), vec![]);
    {
        let ctx = env.ctx();
        if let Obj::Type(t) = ctx.arena.get_mut(klass) {
            t.mro = vec![klass, object];
        }
    }
    let inst = env.instance_of(klass);
    {
        let mut ctx = env.ctx();
        let mut builtins = builtins0.clone();
        let mut seams = RuntimeSeams::stub();
        store_attr(
            &mut ctx,
            inst,
            a_q,
            Some(Value::SmallInt(4)),
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap();
        assert_eq!(load_attr(&mut ctx, inst, a_q, next_obj).unwrap(), Value::SmallInt(4));
    }
    // A small int has nowhere to store.
    {
        let mut ctx = env.ctx();
        let mut builtins = builtins0.clone();
        let mut seams = RuntimeSeams::stub();
        let err = store_attr(
            &mut ctx,
            Value::SmallInt(1),
            a_q,
            Some(Value::SmallInt(4)),
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap_err();
        assert_eq!(err.exc_name(), "AttributeError");
    }
    // Type: store into the type's dict, then load it.
    let klass_v = Value::Heap(klass);
    {
        let mut ctx = env.ctx();
        let mut builtins = builtins0.clone();
        let mut seams = RuntimeSeams::stub();
        store_attr(
            &mut ctx,
            klass_v,
            a_q,
            Some(Value::SmallInt(8)),
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap();
        assert_eq!(load_attr(&mut ctx, klass_v, a_q, next_obj).unwrap(), Value::SmallInt(8));
    }
    // Module: store then load; delete-missing on a non-fixed
    // dict raises KeyError (probed on the reference binary).
    let mt = env.module_type;
    let mymod_q = env.q(b"mymod");
    let m = {
        let mut ctx = env.ctx();
        new_module(&mut ctx, mymod_q, mt)
    };
    {
        let mut ctx = env.ctx();
        let mut builtins = builtins0.clone();
        let mut seams = RuntimeSeams::stub();
        store_attr(
            &mut ctx,
            m,
            a_q,
            Some(Value::SmallInt(3)),
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap();
        assert_eq!(load_attr(&mut ctx, m, a_q, next_obj).unwrap(), Value::SmallInt(3));
        let err = store_attr(
            &mut ctx,
            m,
            nope,
            None,
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap_err();
        match err {
            RtError::Raised(v) => {
                assert!(is_exception_instance(ctx.arena, v, refs.key_error));
            }
            other => panic!("expected Raised(KeyError), got {other:?}"),
        }
    }
    // A fixed-dict module (sys) refuses the store.
    let sys_q = env.q(b"sys");
    let dict_t = env.types.dict;
    let fixed = {
        let mut ctx = env.ctx();
        let map = MapData::new_fixed(vec![]);
        let d = ctx.alloc(Some(dict_t), Obj::Dict(DictData { map }));
        Value::Heap(ctx.alloc(
            Some(mt),
            Obj::Module(ModuleData {
                name: sys_q,
                dict: d,
                delegation: None,
            }),
        ))
    };
    {
        let mut ctx = env.ctx();
        let mut builtins = builtins0.clone();
        let mut seams = RuntimeSeams::stub();
        let err = store_attr(
            &mut ctx,
            fixed,
            a_q,
            Some(Value::SmallInt(1)),
            &refs,
            next_obj,
            &mut builtins,
            &mut seams,
            globals,
            None,
        )
        .unwrap_err();
        assert_eq!(err.exc_name(), "AttributeError");
    }
}

// ── iteration ───────────────────────────────────────────────────────

#[test]
fn getiter_kinds() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let gen_t = env.gen_instance;
    let list = env.list_v(&[Value::SmallInt(1)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    assert!(matches!(it, Value::Heap(r) if matches!(ctx.arena.get(r), Obj::Iterator(_))));
    // An iterator is its own iterator.
    assert_eq!(getiter(&mut ctx, it, next_obj, &mut seams).unwrap(), it);
    // A generator value is its own iterator (ITERNEXT flag).
    let gen = Value::Heap(ctx.alloc(
        Some(gen_t),
        Obj::Generator(GeneratorData {
            fun: gen_t,
            state: GenState::New,
            pend_throw: None,
            stop_value: None,
        }),
    ));
    assert_eq!(getiter(&mut ctx, gen, next_obj, &mut seams).unwrap(), gen);
    // A small int is not iterable.
    let err = getiter(&mut ctx, Value::SmallInt(1), next_obj, &mut seams).unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "object not iterable");
}

#[test]
fn iternext_list_and_dict() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let list = env.list_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    assert_eq!(iternext(&mut ctx, it, next_obj, &mut seams).unwrap(), IterNext::Value(Value::SmallInt(1)));
    assert_eq!(iternext(&mut ctx, it, next_obj, &mut seams).unwrap(), IterNext::Value(Value::SmallInt(2)));
    assert_eq!(iternext(&mut ctx, it, next_obj, &mut seams).unwrap(), IterNext::Stop(None));
    // A dict iterates its keys.
    drop(ctx);
    let k = env.q(b"k");
    let d = env.dict_v(&[(Value::Qstr(k), Value::SmallInt(1))]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, d, next_obj, &mut seams).unwrap();
    assert_eq!(
        iternext(&mut ctx, it, next_obj, &mut seams).unwrap(),
        IterNext::Value(Value::Qstr(k))
    );
    assert_eq!(iternext(&mut ctx, it, next_obj, &mut seams).unwrap(), IterNext::Stop(None));
}

#[test]
fn builtin_next_one_and_two_args() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let list = env.list_v(&[Value::SmallInt(1), Value::SmallInt(2)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    assert_eq!(builtin_next(&mut ctx, &[it], next_obj, &mut seams).unwrap(), Value::SmallInt(1));
    assert_eq!(builtin_next(&mut ctx, &[it], next_obj, &mut seams).unwrap(), Value::SmallInt(2));
    // 1-arg on exhaustion raises StopIteration (bare).
    let err = builtin_next(&mut ctx, &[it], next_obj, &mut seams).unwrap_err();
    assert_eq!(err, RtError::StopIteration(None));
    // 2-arg on exhaustion returns the default.
    assert_eq!(
        builtin_next(&mut ctx, &[it, Value::SmallInt(99)], next_obj, &mut seams).unwrap(),
        Value::SmallInt(99)
    );
    // `next` through the builtin object itself (the adapter).
    drop(ctx);
    let list = env.list_v(&[Value::SmallInt(7)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    let next_b = next_builtin_obj(&mut ctx);
    let first = call_function_n_kw(&mut ctx, next_b, 1, 0, &[it]).unwrap();
    assert_eq!(first, Value::SmallInt(7));
}

#[test]
fn iternext_allow_raise_matches_plain_for_native() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let list = env.list_v(&[Value::SmallInt(1)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    assert_eq!(
        iternext_allow_raise(&mut ctx, it, next_obj, &mut seams).unwrap(),
        IterNext::Value(Value::SmallInt(1))
    );
    assert_eq!(
        iternext_allow_raise(&mut ctx, it, next_obj, &mut seams).unwrap(),
        IterNext::Stop(None)
    );
    // Not an iterator → TypeError in both.
    let err = iternext_allow_raise(&mut ctx, Value::SmallInt(1), next_obj, &mut seams).unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "object not an iterator");
}

// ── resume ──────────────────────────────────────────────────────────

#[test]
fn resume_gen_canned_yield_normal_error() {
    let mut env = Env::new();
    let gen = gen_value(&mut env);
    let mut ctx = env.ctx();
    // Canned `gen_resume`: yields 10.
    fn import_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
        Err(PyError::PyRaise)
    }
    let resume_yield = |_ctx: &mut CallCtx,
                            _gen: Value,
                            _send: Value,
                            _throw: Option<Value>|
     -> Result<GenResume, GenError> {
        Ok(GenResume {
            kind: ResumeKind::Yield,
            value: Value::SmallInt(10),
        })
    };
    let mut seams = RuntimeSeams {
        import_default: Box::new(import_stub),
        gen_resume: Box::new(resume_yield),
    };
    assert_eq!(
        resume_gen(&mut ctx, gen, Value::None, None, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(10))
    );
    // A `StopIteration(99)` error maps to `Normal(99)`.
    let resume_stop = |_ctx: &mut CallCtx,
                           _gen: Value,
                           _send: Value,
                           _throw: Option<Value>|
     -> Result<GenResume, GenError> {
        Err(GenError::StopIteration(Some(Value::SmallInt(99))))
    };
    let mut seams = RuntimeSeams {
        import_default: Box::new(import_stub),
        gen_resume: Box::new(resume_stop),
    };
    assert_eq!(
        resume_gen(&mut ctx, gen, Value::None, None, &mut seams).unwrap(),
        ResumeOutcome::Normal(Value::SmallInt(99))
    );
    // `AlreadyExecuting` maps to `ValueError("generator already
    // executing")` via `gen_error_to_rt`.
    let resume_busy = |_ctx: &mut CallCtx,
                           _gen: Value,
                           _send: Value,
                           _throw: Option<Value>|
     -> Result<GenResume, GenError> {
        Err(GenError::AlreadyExecuting)
    };
    let mut seams = RuntimeSeams {
        import_default: Box::new(import_stub),
        gen_resume: Box::new(resume_busy),
    };
    let err = resume_gen(&mut ctx, gen, Value::None, None, &mut seams).unwrap_err();
    assert_eq!(err.exc_name(), "ValueError");
    assert_eq!(err.message(), "generator already executing");
}

#[test]
fn resume_iternext_fast_path_and_send_missing() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let refs = env.refs;
    let list = env.list_v(&[Value::SmallInt(5), Value::SmallInt(6)]);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    let it = getiter(&mut ctx, list, next_obj, &mut seams).unwrap();
    assert_eq!(
        resume(&mut ctx, it, Value::None, None, next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(5))
    );
    assert_eq!(
        resume(&mut ctx, it, Value::None, None, next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(6))
    );
    assert_eq!(
        resume(&mut ctx, it, Value::None, None, next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Normal(Value::None)
    );
    // `send(5)` on a plain iterator: no `send` method.
    let err = resume(&mut ctx, it, Value::SmallInt(5), None, next_obj, &refs, &mut seams).unwrap_err();
    assert_eq!(err.exc_name(), "AttributeError");
}

#[test]
fn resume_dunder_next_and_send_paths() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let refs = env.refs;
    let object = env.types.object;
    let klass = env.user_type(b"U", Some(object), vec![]);
    {
        let ctx = env.ctx();
        if let Obj::Type(t) = ctx.arena.get_mut(klass) {
            t.mro = vec![klass, object];
        }
    }
    let next_q = env.q(b"__next__");
    let send_q = env.q(b"send");
    let throw_q = env.q(b"throw");
    let close_q = env.q(b"close");
    let next_fn = env.fun_v(b"next", 0, 0, seven);
    env.set_local(klass, next_q, next_fn);
    let send_fn = env.fun_v(b"send", 1, 1, echo);
    env.set_local(klass, send_q, send_fn);
    let throw_fn = env.fun_v(b"throw", 1, 1, seven);
    env.set_local(klass, throw_q, throw_fn);
    let close_fn = env.fun_v(b"close", 0, 0, seven);
    env.set_local(klass, close_q, close_fn);
    let inst = env.instance_of(klass);
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    // `__next__` path.
    assert_eq!(
        resume(&mut ctx, inst, Value::None, None, next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(7))
    );
    // `send` path echoes the sent value.
    assert_eq!(
        resume(&mut ctx, inst, Value::SmallInt(42), None, next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(42))
    );
    // `throw` path (non-GeneratorExit) calls `throw`.
    let exc_v = new_exception(&mut ctx, refs.stop_iteration);
    assert_eq!(
        resume(&mut ctx, inst, Value::None, Some(exc_v), next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Yield(Value::SmallInt(7))
    );
    // `GeneratorExit` → `close()` → NORMAL.
    let ge = new_exception(&mut ctx, refs.generator_exit);
    assert_eq!(
        resume(&mut ctx, inst, Value::None, Some(ge), next_obj, &refs, &mut seams).unwrap(),
        ResumeOutcome::Normal(Value::SmallInt(7))
    );
}

#[test]
fn resume_throw_tail_pep479_and_make_raise() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let refs = env.refs;
    // A bare object with no `throw`/`close`: throwing a
    // StopIteration instance → PEP 479 RuntimeError object.
    let object = env.types.object;
    let plain = env.instance_of(object);
    let si = {
        let mut ctx = env.ctx();
        new_exception(&mut ctx, refs.stop_iteration)
    };
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    match resume(&mut ctx, plain, Value::None, Some(si), next_obj, &refs, &mut seams).unwrap() {
        ResumeOutcome::Exception(v) => {
            assert!(is_exception_instance(ctx.arena, v, refs.runtime_error));
        }
        other => panic!("expected Exception(RuntimeError), got {other:?}"),
    }
    // Throwing a non-exception → `make_raise_obj` TypeError.
    let err = resume(&mut ctx, plain, Value::None, Some(Value::SmallInt(5)), next_obj, &refs, &mut seams)
        .unwrap_err();
    assert_eq!(err.exc_name(), "TypeError");
    assert_eq!(err.message(), "exceptions must derive from BaseException");
    // `make_raise_obj` on a type instantiates it; on an
    // instance returns it.
    let t = {
        let mut ctx = env.ctx();
        make_raise_obj(&mut ctx, Value::Heap(refs.stop_iteration), &refs).unwrap()
    };
    assert!(is_exception_instance(&env.arena, t, refs.stop_iteration));
    let inst = {
        let mut ctx = env.ctx();
        new_exception(&mut ctx, refs.stop_iteration)
    };
    let mut ctx = env.ctx();
    assert_eq!(make_raise_obj(&mut ctx, inst, &refs).unwrap(), inst);
}

// ── imports ─────────────────────────────────────────────────────────

#[test]
fn import_name_override_and_default_seam() {
    let mut env = Env::new();
    let name_q = env.q(b"typing");
    let imp_q = env.q(b"__import__");
    let custom = env.fun_v(b"imp", 5, 5, forty_two);
    let od = env.dict_v(&[(Value::Qstr(imp_q), custom)]);
    let globals = env.globals;
    let mut builtins = Builtins {
        globals: env.builtins.globals,
        override_dict: Some(od),
    };
    let mut ctx = env.ctx();
    let mut seams = RuntimeSeams::stub();
    assert_eq!(
        import_name(&mut ctx, name_q, Value::None, Value::SmallInt(0), globals, &builtins, &mut seams).unwrap(),
        Value::SmallInt(42)
    );
    // No override → the `import_default` seam (canned 43).
    let import_43 = |_ctx: &mut CallCtx, _args: &[Value]| -> PyResult<Value> {
        Ok(Value::SmallInt(43))
    };
    let gen_stub = |_ctx: &mut CallCtx,
                        _gen: Value,
                        _send: Value,
                        _throw: Option<Value>|
     -> Result<GenResume, GenError> {
        Err(GenError::PyRaise)
    };
    let mut seams = RuntimeSeams {
        import_default: Box::new(import_43),
        gen_resume: Box::new(gen_stub),
    };
    builtins.override_dict = None;
    assert_eq!(
        import_name(&mut ctx, name_q, Value::None, Value::SmallInt(0), globals, &builtins, &mut seams).unwrap(),
        Value::SmallInt(43)
    );
}

#[test]
fn import_from_attr_and_submodule_fallback() {
    let mut env = Env::new();
    let next_obj = env.next_obj;
    let globals = env.globals;
    let builtins = env.builtins.clone();
    let mt = env.module_type;
    let pkg_q = env.q(b"pkg");
    let sub_q = env.q(b"sub");
    let missing = env.q(b"missing");
    let path_q = env.q(b"__path__");
    let name_q = env.q(b"__name__");
    // Pre-intern the dotted name: the `InternFn` seam is
    // find-only (the pool is shared), so a name the test never
    // interned cannot be produced by `import_from`.
    let _dot_q = env.q(b"pkg.missing");
    let m = {
        let mut ctx = env.ctx();
        new_module(&mut ctx, pkg_q, mt)
    };
    let mut ctx = env.ctx();
    let dict = match m {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(md) => Value::Heap(md.dict),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    dict_store(&mut ctx, dict, Value::Qstr(sub_q), Value::SmallInt(11)).expect("store");
    // Attribute present → the value.
    let mut seams = RuntimeSeams::stub();
    let pool = ctx.pool;
    let mut intern = Env::intern_seam(pool);
    assert_eq!(
        import_from(&mut ctx, m, sub_q, globals, &builtins, &mut intern, next_obj, &mut seams).unwrap(),
        Value::SmallInt(11)
    );
    // Missing, no `__path__` → ImportError.
    let err = import_from(&mut ctx, m, missing, globals, &builtins, &mut intern, next_obj, &mut seams)
        .unwrap_err();
    assert_eq!(err.exc_name(), "ImportError");
    assert!(err.message().contains("missing"), "msg: {}", err.message());
    // With `__path__` + `__name__` → the dotted import goes
    // through the seam, which records the name.
    dict_store(&mut ctx, dict, Value::Qstr(path_q), Value::SmallInt(0)).expect("store path");
    dict_store(&mut ctx, dict, Value::Qstr(name_q), Value::Qstr(pkg_q)).expect("store name");
    let mut recorded: Vec<Value> = Vec::new();
    let import_rec = |_ctx: &mut CallCtx, args: &[Value]| -> PyResult<Value> {
        recorded.push(args[0]);
        Ok(Value::SmallInt(44))
    };
    let gen_stub = |_ctx: &mut CallCtx,
                        _gen: Value,
                        _send: Value,
                        _throw: Option<Value>|
     -> Result<GenResume, GenError> {
        Err(GenError::PyRaise)
    };
    let mut seams = RuntimeSeams {
        import_default: Box::new(import_rec),
        gen_resume: Box::new(gen_stub),
    };
    let got = import_from(&mut ctx, m, missing, globals, &builtins, &mut intern, next_obj, &mut seams).unwrap();
    assert_eq!(got, Value::SmallInt(44));
    drop(seams); // releases the closure's borrow of `recorded`
    assert_eq!(recorded.len(), 1);
    let dot = match recorded[0] {
        Value::Qstr(id) => String::from_utf8_lossy(ctx.pool.qstr_str(id as u16)).into_owned(),
        other => panic!("expected a qstr name, got {other:?}"),
    };
    assert_eq!(dot, "pkg.missing");
}

#[test]
fn import_all_stores_non_underscore_names() {
    let mut env = Env::new();
    let mt = env.module_type;
    let a_q = env.q(b"a");
    let b_q = env.q(b"_b");
    let c_q = env.q(b"c");
    let m_q = env.q(b"m");
    let m = {
        let mut ctx = env.ctx();
        new_module(&mut ctx, m_q, mt)
    };
    let mut ctx = env.ctx();
    let dict = match m {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(md) => Value::Heap(md.dict),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    dict_store(&mut ctx, dict, Value::Qstr(a_q), Value::SmallInt(1)).expect("store");
    dict_store(&mut ctx, dict, Value::Qstr(b_q), Value::SmallInt(2)).expect("store");
    dict_store(&mut ctx, dict, Value::Qstr(c_q), Value::SmallInt(3)).expect("store");
    drop(ctx);
    let locals = env.dict_v(&[]);
    let mut ctx = env.ctx();
    let pool = ctx.pool;
    let mut intern = Env::intern_seam(pool);
    import_all(&mut ctx, m, locals, &mut intern).unwrap();
    assert_eq!(dict_lookup_qstr(&mut ctx, locals, a_q).unwrap(), Some(Value::SmallInt(1)));
    assert_eq!(dict_lookup_qstr(&mut ctx, locals, b_q).unwrap(), None);
    assert_eq!(dict_lookup_qstr(&mut ctx, locals, c_q).unwrap(), Some(Value::SmallInt(3)));
}
