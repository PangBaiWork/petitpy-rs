//! Tests for the C15a runtime-glue modules that share one
//! fixture: `raise` (`raise.zig`), `opmethods`
//! (`opmethods.zig`), `runtime_utils` (`runtime_utils.zig`),
//! and `builtinevex` (`builtinevex.zig`). The `argcheck` module
//! has its own test file (`argcheck_tests.rs`) because its grid
//! test is large.

use crate::memory::gc::CallCtx;
use crate::objects::containers::tests::Fixture;
use crate::objects::value::Value;
use crate::pyerr::PyError;
use crate::runtime::raise::*;

fn ctx_of<'a>(fx: &'a mut Fixture) -> CallCtx<'a> {
    CallCtx::new(
        &mut fx.arena,
        &mut fx.roots,
        &fx.types,
        &fx.pool,
        &mut fx.pending_raise,
    )
}

fn pending(fx: &Fixture) -> (String, Option<Value>, Option<String>) {
    let p = fx.pending_raise.as_ref().expect("a raise must be recorded");
    (
        p.exc_name.to_string(),
        p.arg,
        p.message.clone(),
    )
}

// ── raise.zig ────────────────────────────────────────────────────────

#[test]
fn raise_bare_type_error() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    let e = raise_type_error_null(&mut ctx);
    assert_eq!(e, PyError::PyRaise);
    assert_eq!(pending(&fx), ("TypeError".to_string(), None, None));
}

#[test]
fn raise_os_error_carries_errno() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    let e = raise_os_error(&mut ctx, 2);
    assert_eq!(e, PyError::PyRaise);
    assert_eq!(
        pending(&fx),
        ("OSError".to_string(), Some(Value::SmallInt(2)), None)
    );
}

#[test]
fn raise_malloc_fail() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(malloc_fail(&mut ctx), PyError::PyRaise);
    assert_eq!(
        pending(&fx),
        (
            "MemoryError".to_string(),
            None,
            Some("memory allocation failed".to_string())
        )
    );
}

#[test]
fn raise_msg_and_varg_are_literal() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(
        raise_msg(&mut ctx, "ValueError", Some("bad value")),
        PyError::PyRaise
    );
    assert_eq!(
        pending(&fx),
        ("ValueError".to_string(), None, Some("bad value".to_string()))
    );
    // A None message is the bare exception (the Zig's msg == NULL
    // branch).
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(raise_value_error(&mut ctx, None), PyError::PyRaise);
    assert_eq!(pending(&fx), ("ValueError".to_string(), None, None));
    // varg formatting is deferred in the Zig: the literal format
    // string is raised unchanged, `%` and all.
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(
        raise_msg_varg(&mut ctx, "TypeError", "%d items"),
        PyError::PyRaise
    );
    assert_eq!(
        pending(&fx),
        ("TypeError".to_string(), None, Some("%d items".to_string()))
    );
}

#[test]
fn raise_int_conversion_and_type_arg() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(raise_type_error_int_conversion(&mut ctx), PyError::PyRaise);
    assert_eq!(
        pending(&fx),
        (
            "TypeError".to_string(),
            None,
            Some("can't convert to int".to_string())
        )
    );
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(
        raise_type_arg(&mut ctx, "TypeError", Value::SmallInt(3)),
        PyError::PyRaise
    );
    assert_eq!(
        pending(&fx),
        ("TypeError".to_string(), Some(Value::SmallInt(3)), None)
    );
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(raise_not_implemented(&mut ctx, None), PyError::PyRaise);
    assert_eq!(pending(&fx), ("NotImplementedError".to_string(), None, None));
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(raise_bad_compile_mode(&mut ctx), PyError::PyRaise);
    assert_eq!(
        pending(&fx),
        ("ValueError".to_string(), None, Some("bad compile mode".to_string()))
    );
}

#[test]
fn raise_first_record_wins() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    let _ = raise_value_error(&mut ctx, Some("first"));
    let _ = raise_type_error(&mut ctx, Some("second"));
    assert_eq!(
        pending(&fx),
        ("ValueError".to_string(), None, Some("first".to_string()))
    );
}

// ── opmethods.zig ────────────────────────────────────────────────────

use crate::objects::containers::objdict_methods::DICT_METHODS;
use crate::objects::containers::tests::empty_dict;
use crate::objects::obj::FLAG_INSTANCE_TYPE;
use crate::objects::value::{DictData, InstanceData, MapData, Obj, TypeData};
use crate::runtime::opmethods::*;

/// A dict value `{'a': 42}` with the real slotted dict type as
/// its header (so the subscr slot is the real dict one).
fn dict_a42(fx: &mut Fixture) -> (Value, u32) {
    let qa = u32::from(fx.pool.from_strn(b"a").unwrap());
    let map = MapData::from_pairs(vec![(Value::Qstr(qa), Value::SmallInt(42))]);
    let dict_type = fx.types.dict;
    let d = fx
        .arena
        .alloc(Some(dict_type), Obj::Dict(DictData { map }), &fx.roots);
    (Value::Heap(d), qa)
}

/// `class E(dict)` — a minimal instance type: the
/// `FLAG_INSTANCE_TYPE` flag and `bases = (dict,)` are what
/// `count_native_bases` walks (`mro` is not consulted there, so
/// it stays empty — this fixture only exercises `native_base`
/// and subscr dispatch, never class lookup).
fn subclass_dict_type(fx: &mut Fixture) -> crate::objects::value::GcRef {
    let dict_type = fx.types.dict;
    let bases = fx
        .arena
        .alloc(None, Obj::Tuple(vec![Value::Heap(dict_type)]), &fx.roots);
    let name = u32::from(fx.pool.from_strn(b"E").unwrap());
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    fx.arena.alloc(
        None,
        Obj::Type(TypeData {
            name,
            flags: FLAG_INSTANCE_TYPE,
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
            make_new: None,
            bases: Some(bases),
            namedtuple_fields: None,
        }),
        &fx.roots,
    )
}

/// An instance of `class E(dict)` whose native base is `dict_obj`.
fn subclass_instance(fx: &mut Fixture, class: crate::objects::value::GcRef, dict_obj: Value) -> Value {
    let inst = fx.arena.alloc(
        Some(class),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj: vec![dict_obj],
        }),
        &fx.roots,
    );
    Value::Heap(inst)
}

#[test]
fn native_base_passthrough() {
    let mut fx = Fixture::new();
    let (dict, _) = dict_a42(&mut fx);
    // An instance of a plain type (no FLAG_INSTANCE_TYPE) is
    // returned as it is — the Zig's flag check comes first.
    let object = fx.types.object;
    let inst = fx.arena.alloc(
        Some(object),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj: Vec::new(),
        }),
        &fx.roots,
    );
    let inst = Value::Heap(inst);
    let mut ctx = ctx_of(&mut fx);
    // Non-instances are returned as they are.
    assert_eq!(native_base(&mut ctx, Value::SmallInt(5)), Value::SmallInt(5));
    assert_eq!(native_base(&mut ctx, Value::None), Value::None);
    assert_eq!(native_base(&mut ctx, dict), dict);
    assert_eq!(native_base(&mut ctx, inst), inst);
}

#[test]
fn native_base_unwraps_subclass_instance() {
    let mut fx = Fixture::new();
    let (dict, _) = dict_a42(&mut fx);
    let class = subclass_dict_type(&mut fx);
    let inst = subclass_instance(&mut fx, class, dict);
    // An empty subobj cannot happen through `new_instance`, but
    // it must not index out of bounds — it falls back to self.
    let empty = fx.arena.alloc(
        Some(class),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj: Vec::new(),
        }),
        &fx.roots,
    );
    let empty = Value::Heap(empty);
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(native_base(&mut ctx, inst), dict);
    assert_eq!(native_base(&mut ctx, empty), empty);
}

#[test]
fn subscr_ops_through_native_base() {
    let mut fx = Fixture::new();
    let (dict, qa) = dict_a42(&mut fx);
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let class = subclass_dict_type(&mut fx);
    let inst = subclass_instance(&mut fx, class, dict);
    let mut ctx = ctx_of(&mut fx);
    // `inst['a']` reads the NATIVE dict, not the instance —
    // the instance itself has no subscr slot.
    assert_eq!(getitem_op(&mut ctx, inst, Value::Qstr(qa)), Ok(Value::SmallInt(42)));
    // Set and delete go through the same unwrapping.
    assert_eq!(
        setitem_op(&mut ctx, inst, Value::Qstr(qb), Value::SmallInt(7)),
        Ok(Value::None)
    );
    assert_eq!(getitem_op(&mut ctx, inst, Value::Qstr(qb)), Ok(Value::SmallInt(7)));
    assert_eq!(delitem_op(&mut ctx, inst, Value::Qstr(qa)), Ok(Value::None));
    // A missing key raises KeyError (the dict slot's own error
    // propagates through the op).
    let mut ctx = ctx_of(&mut fx);
    assert!(getitem_op(&mut ctx, inst, Value::Qstr(qa)).is_err());
    assert_eq!(ctx.pending_raise.as_ref().unwrap().exc_name, "KeyError");
}

#[test]
fn subscr_missing_slot_is_generic_error() {
    let mut fx = Fixture::new();
    let mut ctx = ctx_of(&mut fx);
    // None's type has no subscr slot: the Zig would panic in
    // `callSubscr`; Rust records the generic error instead.
    assert!(getitem_op(&mut ctx, Value::None, Value::SmallInt(0)).is_err());
    assert_eq!(
        ctx.pending_raise.as_ref().unwrap().message.as_deref(),
        Some("object isn't subscriptable")
    );
}

#[test]
fn contains_through_binary_slot() {
    let mut fx = Fixture::new();
    let (dict, qa) = dict_a42(&mut fx);
    let qz = u32::from(fx.pool.from_strn(b"z").unwrap());
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(contains_op(&mut ctx, dict, Value::Qstr(qa)), Ok(true));
    assert_eq!(contains_op(&mut ctx, dict, Value::Qstr(qz)), Ok(false));
    // No CONTAINS slot: the generic `binop_unsupported` TypeError.
    let mut ctx = ctx_of(&mut fx);
    assert!(contains_op(&mut ctx, Value::None, Value::SmallInt(0)).is_err());
    assert_eq!(
        ctx.pending_raise.as_ref().unwrap().message.as_deref(),
        Some("unsupported types for __contains__: 'NoneType', 'int'")
    );
}

#[test]
fn dict_op_adapters_and_table() {
    let mut fx = Fixture::new();
    let (dict, qa) = dict_a42(&mut fx);
    let qb = u32::from(fx.pool.from_strn(b"b").unwrap());
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(
        dict_getitem_builtin(&mut ctx, &[dict, Value::Qstr(qa)]),
        Ok(Value::SmallInt(42))
    );
    assert_eq!(
        dict_setitem_builtin(&mut ctx, &[dict, Value::Qstr(qb), Value::SmallInt(7)]),
        Ok(Value::None)
    );
    assert_eq!(
        dict_getitem_builtin(&mut ctx, &[dict, Value::Qstr(qb)]),
        Ok(Value::SmallInt(7))
    );
    assert_eq!(
        dict_delitem_builtin(&mut ctx, &[dict, Value::Qstr(qb)]),
        Ok(Value::None)
    );
    // Wrong arity → the terse mismatch (canonical sig check).
    let mut ctx = ctx_of(&mut fx);
    assert!(dict_getitem_builtin(&mut ctx, &[dict]).is_err());
    assert_eq!(
        ctx.pending_raise.as_ref().unwrap().message.as_deref(),
        Some("argument num/types mismatch")
    );
    // `__contains__` has an adapter but no table entry (the Zig
    // leaves `mp_op_contains_obj` unreferenced).
    let mut ctx = ctx_of(&mut fx);
    assert_eq!(
        dict_contains_builtin(&mut ctx, &[dict, Value::Qstr(qa)]),
        Ok(Value::Bool(true))
    );
    // The three op entries are the last rows of the dict table,
    // with the Zig's 2-/3-/2-arity.
    let names: Vec<(&str, u16, u16)> = DICT_METHODS
        .iter()
        .map(|e| (e.name, e.min, e.max))
        .collect();
    assert!(names.contains(&("__getitem__", 2, 2)));
    assert!(names.contains(&("__setitem__", 3, 3)));
    assert!(names.contains(&("__delitem__", 2, 2)));
    assert!(!names.iter().any(|(n, _, _)| *n == "__contains__"));
}

// ── runtime_utils.zig ────────────────────────────────────────────────

use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, GcRef};
use crate::pyerr::PyResult;
use crate::runtime::runtime_utils::*;
use crate::runtime::vm::Vm;

#[test]
fn mul_overflow_matches_checked_mul() {
    // Edge values, cross-checked against `checked_mul` (the
    // ground truth) for both fns — they coincide on x86-64.
    let edges: &[i64] = &[
        i64::MIN,
        i64::MIN + 1,
        -3037000500,
        -2,
        -1,
        0,
        1,
        2,
        3037000499,
        3037000500,
        i64::MAX - 1,
        i64::MAX,
    ];
    for &x in edges {
        for &y in edges {
            assert_eq!(mul_ll_overflow(x, y), x.checked_mul(y), "ll {x} * {y}");
            assert_eq!(mul_mp_int_t_overflow(x, y), x.checked_mul(y), "mp {x} * {y}");
        }
    }
    // A dense small grid (no overflow anywhere in range).
    for x in -64i64..=64 {
        for y in -64i64..=64 {
            assert_eq!(mul_ll_overflow(x, y), Some(x * y));
        }
    }
    // The `i64::MIN / -1` division never occurs (see the module
    // docs): these pairs must not panic, only report overflow.
    assert_eq!(mul_ll_overflow(i64::MIN, -1), None);
    assert_eq!(mul_ll_overflow(-1, i64::MIN), None);
    assert_eq!(mul_ll_overflow(i64::MIN, 1), Some(i64::MIN));
    assert_eq!(mul_ll_overflow(i64::MAX, 1), Some(i64::MAX));
}

fn ident_builtin(_ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    Ok(args[0])
}

fn boom_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(raise_type_error(ctx, Some("boom")))
}

fn ok_but_pending_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    ctx.record_raise("ValueError", Some("late"));
    Ok(args[0])
}

fn make_fun_builtin(vm: &mut Vm, func: crate::objects::slots::BuiltinFn) -> Value {
    make_fun_builtin_n(vm, func, 1, 1)
}

fn make_fun_builtin_n(
    vm: &mut Vm,
    func: crate::objects::slots::BuiltinFn,
    min: u16,
    max: u16,
) -> Value {
    let name = static_qstr_id(b"len").unwrap();
    let r: GcRef = vm.arena.alloc(
        None,
        Obj::FunBuiltin(FunBuiltinData {
            name,
            n_args: fun_n_args(min, max),
            func,
            kw_func: None,
        }),
        &vm.roots,
    );
    Value::Heap(r)
}

#[test]
fn protected_call_success_and_raise() {
    let mut vm = Vm::new();
    let ident = make_fun_builtin(&mut vm, ident_builtin);
    let ident2 = make_fun_builtin_n(&mut vm, ident_builtin, 2, 2);
    let boom = make_fun_builtin(&mut vm, boom_builtin);
    let mut ctx = CallCtx::new(
        &mut vm.arena,
        &mut vm.roots,
        &vm.types,
        &vm.pool,
        &mut vm.pending_raise,
    );
    // Success: the value passes through, nothing is printed.
    let mut sink = String::new();
    let r = call_function_1_protected_sink(&mut ctx, &vm.state, ident, Value::SmallInt(7), &mut sink);
    assert_eq!(r, Some(Value::SmallInt(7)));
    assert!(sink.is_empty());
    // A raise: printed into the sink, None returned, pending
    // drained (a fresh exception has an empty traceback, so no
    // "Traceback..." header — just the exception line).
    let mut sink = String::new();
    let r = call_function_1_protected_sink(&mut ctx, &vm.state, boom, Value::SmallInt(7), &mut sink);
    assert_eq!(r, None);
    assert!(sink.contains("TypeError: boom"), "sink was: {sink}");
    assert!(!ctx.has_pending_raise());
    // Two-arg form, same shape (needs a 2-arg builtin).
    let mut sink = String::new();
    let r = call_function_2_protected_sink(
        &mut ctx,
        &vm.state,
        ident2,
        Value::SmallInt(1),
        Value::SmallInt(2),
        &mut sink,
    );
    assert_eq!(r, Some(Value::SmallInt(1)));
    assert!(sink.is_empty());
}

#[test]
fn protected_call_pending_after_ok_and_bare_error() {
    let mut vm = Vm::new();
    let ok_pending = make_fun_builtin(&mut vm, ok_but_pending_builtin);
    let mut ctx = CallCtx::new(
        &mut vm.arena,
        &mut vm.roots,
        &vm.types,
        &vm.pool,
        &mut vm.pending_raise,
    );
    // The Zig checks `pending_exc` AFTER the call whatever it
    // returned: a value returned with a raise pending is still
    // printed and dropped.
    let mut sink = String::new();
    let r =
        call_function_1_protected_sink(&mut ctx, &vm.state, ok_pending, Value::SmallInt(1), &mut sink);
    assert_eq!(r, None);
    assert!(sink.contains("ValueError: late"), "sink was: {sink}");
    assert!(!ctx.has_pending_raise());
    // A bare `RtError` with nothing pending (not callable) is
    // built from the error and printed.
    let mut sink = String::new();
    let r = call_function_1_protected_sink(
        &mut ctx,
        &vm.state,
        Value::SmallInt(5),
        Value::SmallInt(1),
        &mut sink,
    );
    assert_eq!(r, None);
    assert!(sink.contains("TypeError: object not callable"), "sink was: {sink}");
    assert!(!ctx.has_pending_raise());
}