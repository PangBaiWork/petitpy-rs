//! Unit tests for the runtime bc port (`src/runtime/bc.rs`).
//!
//! Split out of `bc.rs` so both files stay under the 800-line cap
//! (the Lane RE15 precedent: `src/vendor/re15_tests.rs`). The varint
//! vectors are hand-computed from the Zig `mp_encode_uint` loop; the
//! setup tests compile real functions with the Phase B compiler and
//! check arg binding against the reference binary's observed
//! behaviour (probes t1-t19, recorded in `scripts/bc_result.txt`).

use crate::compiler::bridge::compile_to_mpy;
use crate::compiler::mpy::{self, Module, QstrEntry, RawCode};
use crate::compiler::parser::InputKind;
use crate::memory::gc::{CallCtx, GcArena, PendingRaise, RootSet};
use crate::objects::obj::BuiltinTypes;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, FunBcData, GcRef, MapData, MapEntry, Obj, TypeData, Value};
use crate::runtime::bc::*;
use crate::runtime::qstr::RuntimeQstr;

// ── fixture: arena + qstr pool + singletons ──────────────────

/// A plain (all slots `None`) type object — the setup path only
/// needs `types` for the kwargs/defkw hash-table lookups, whose
/// keys are qstr immediates (they hash by id, no slot involved).
fn plain_type(arena: &mut GcArena, roots: &RootSet, name: &[u8]) -> GcRef {
    let dict = arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::new(),
        }),
        roots,
    );
    arena.alloc(
        None,
        Obj::Type(TypeData {
            name: static_qstr_id(name).expect("type name is a static qstr"),
            flags: 0,
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
            namedtuple_fields: None,
            attr: None,
            make_new: None,
            bases: None,
        }),
        roots,
    )
}

struct Fixture {
    arena: GcArena,
    roots: RootSet,
    pool: RuntimeQstr,
    types: BuiltinTypes,
    pending_raise: Option<PendingRaise>,
    empty_tuple: Value,
    fun: GcRef,
}

impl Fixture {
    fn new() -> Fixture {
        let mut arena = GcArena::new();
        let roots = RootSet::new();
        // The VM-owned `mp_const_empty_tuple` singleton.
        let empty_tuple =
            Value::Heap(arena.alloc(None, Obj::Tuple(Vec::new()), &roots));
        let fun = arena.alloc(
            None,
            Obj::FunBc(FunBcData {
                name: 0,
                n_args: 0,
                code: 0,
                // C10: default args + module context (empty here).
                def_pos: Vec::new(),
                def_kw: None,
                context: None,
            }),
            &roots,
        );
        let mk = |arena: &mut GcArena, name: &[u8]| plain_type(arena, &roots, name);
        let type_ = mk(&mut arena, b"type");
        let types = BuiltinTypes {
            int: mk(&mut arena, b"int"),
            str_: mk(&mut arena, b"str"),
            bytes: mk(&mut arena, b"bytes"),
            bool_: mk(&mut arena, b"bool"),
            none_type: mk(&mut arena, b"NoneType"),
            float: mk(&mut arena, b"float"),
            complex: mk(&mut arena, b"complex"),
            tuple: mk(&mut arena, b"tuple"),
            list: mk(&mut arena, b"list"),
            dict: mk(&mut arena, b"dict"),
            array: mk(&mut arena, b"array"),
            bytearray: mk(&mut arena, b"bytearray"),
            set: mk(&mut arena, b"set"),
            frozenset: mk(&mut arena, b"frozenset"),
            range: mk(&mut arena, b"range"),
            type_,
            object: mk(&mut arena, b"object"),
            super_: mk(&mut arena, b"super"),
            staticmethod: mk(&mut arena, b"staticmethod"),
            classmethod: mk(&mut arena, b"classmethod"),
            boundmeth: mk(&mut arena, b"bound_method"),
            empty_tuple: mk(&mut arena, b"tuple"),
            empty_bytes: mk(&mut arena, b"bytes"),
            native_init_wrapper: mk(&mut arena, b"function"),
            property: mk(&mut arena, b"property"),
        };
        Fixture {
            arena,
            roots,
            pool: RuntimeQstr::new(),
            types,
            pending_raise: None,
            empty_tuple,
            fun,
        }
    }

    /// qstr id of `name` in this fixture's pool (same interning
    /// the module qstr table was resolved through).
    fn q(&mut self, name: &str) -> u32 {
        u32::from(self.pool.from_strn(name.as_bytes()).unwrap())
    }
}

/// Compile `source` (a module defining exactly one function) and
/// return that function's `RawCode` plus the resolved module qstr
/// table (prelude index -> qstr id), built the way the C loader
/// builds `context->constants.qstr_table` from the `.mpy` qstr
/// section.
fn compile_fun(source: &str, pool: &mut RuntimeQstr) -> (RawCode, Vec<u16>) {
    let bytes =
        compile_to_mpy(source, InputKind::FileInput).expect("test source must compile");
    let Module { qstrs, rc, .. } = mpy::read(&bytes).expect("test .mpy must read");
    assert_eq!(
        rc.children.len(),
        1,
        "test module must define exactly one function"
    );
    let fun = rc.children.into_iter().next().unwrap();
    let mut table = Vec::with_capacity(qstrs.len());
    for e in &qstrs {
        match e {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => {
                table.push(pool.from_strn(bytes).expect("qstr intern"))
            }
        }
    }
    (fun, table)
}

/// `n_state` for a `RawCode`, decoded from its sig prelude — the
/// same decode the C caller does (`DECODE_CODESTATE_SIZE`).
fn n_state_of(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    prelude_sig_decode(&mut cursor).n_state
}

/// Expected final `ip`: prelude bytes + source-info bytes + cell
/// bytes, measured by the decode cursors.
fn expected_ip(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    let _sig = prelude_sig_decode(&mut cursor);
    let size = prelude_size_decode(&mut cursor);
    (fun.fun_data.len() - cursor.len()) + size.n_info + size.n_cell
}

/// Cell count from the size prelude (lets the cell test assert
/// its own premise).
fn n_cell_of(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    let _sig = prelude_sig_decode(&mut cursor);
    prelude_size_decode(&mut cursor).n_cell
}

/// Call `setup_code_state` for a compiled function.
fn setup(
    fx: &mut Fixture,
    fun: &RawCode,
    qstr_table: &[u16],
    extra_args: &[Value],
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<CodeState, SetupError> {
    let n_state = n_state_of(fun);
    let mut ctx = CallCtx::new(
        &mut fx.arena,
        &mut fx.roots,
        &fx.types,
        &fx.pool,
        &mut fx.pending_raise,
    );
    setup_code_state(
        fx.fun,
        fun,
        qstr_table,
        extra_args,
        fx.empty_tuple,
        n_state,
        n_args,
        n_kw,
        args,
        &mut ctx,
    )
}

/// Value of positional arg `i` (slot `n_state - 1 - i`).
fn arg(cs: &CodeState, i: usize) -> Option<Value> {
    cs.state[cs.n_state - 1 - i]
}

/// Read a heap object out of the fixture arena.
fn get<'a>(fx: &'a Fixture, r: GcRef) -> &'a Obj {
    fx.arena.get(r)
}

// ── 1. varint codec ──────────────────────────────────────────

#[test]
fn varint_encode_decode_vectors() {
    // (value, expected bytes) — hand-computed from the Zig
    // `mp_encode_uint` loop (7-bit chunks, MSB continuation).
    let vectors: &[(usize, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (127, &[0x7f]),
        (128, &[0x81, 0x00]),
        (300, &[0x82, 0x2c]),
        (16383, &[0xff, 0x7f]),
        (16384, &[0x81, 0x80, 0x00]),
        (268_435_456, &[0x81, 0x80, 0x80, 0x80, 0x00]), // 2^28
        (
            72_057_594_037_927_936,
            &[0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00],
        ), // 2^56
        (
            usize::MAX,
            &[0x81, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ), // 10-byte max
    ];
    assert_eq!(MP_ENCODE_UINT_MAX_BYTES, 10);
    for (v, want) in vectors {
        // Emit-side encoder (the reused `mp_encode_uint`).
        let mut buf = [0u8; MP_ENCODE_UINT_MAX_BYTES];
        let got = encode_uint(&mut buf, *v);
        assert_eq!(got, *want, "encode_uint({})", v);
        assert!(got.len() <= MP_ENCODE_UINT_MAX_BYTES);
        // Runtime decode of the emit encoder's bytes.
        let mut cursor: &[u8] = got;
        assert_eq!(decode_uint(&mut cursor), *v);
        assert!(cursor.is_empty(), "decode must consume all bytes");
        assert_eq!(decode_uint_value(got), *v);
    }
}

#[test]
fn varint_skip_steps_past_one_value() {
    let mut buf = [0u8; MP_ENCODE_UINT_MAX_BYTES];
    let a = encode_uint(&mut buf, 300).to_vec();
    let mut buf2 = [0u8; MP_ENCODE_UINT_MAX_BYTES];
    let b = encode_uint(&mut buf2, 5).to_vec();
    let mut joined = a.clone();
    joined.extend_from_slice(&b);
    let rest = decode_uint_skip(&joined);
    assert_eq!(rest, &b[..]);
    assert_eq!(decode_uint_value(rest), 5);
    assert!(decode_uint_skip(rest).is_empty());
    // skip over a 1-byte varint, then decode the next.
    let mut cursor: &[u8] = &joined;
    let first = decode_uint(&mut cursor);
    assert_eq!(first, 300);
    assert_eq!(decode_uint(&mut cursor), 5);
}

#[test]
fn prelude_sig_decode_vectors() {
    // Hand-computed from `MP_BC_PRELUDE_SIG_DECODE_INTO` (bc.h).
    let mut cur: &[u8] = &[0x00];
    let s = prelude_sig_decode(&mut cur);
    assert_eq!((s.n_state, s.n_exc_stack, s.scope_flags, s.n_pos_args, s.n_kwonly_args, s.n_def_pos_args), (1, 0, 0, 0, 0, 0));
    assert!(cur.is_empty());
    // 0x03: A = 3.
    let mut cur: &[u8] = &[0x03];
    let s = prelude_sig_decode(&mut cur);
    assert_eq!((s.n_state, s.n_pos_args), (1, 3));
    // [0x80, 0x30]: continuation, S |= 0x30 -> S = 48 -> n_state 49.
    let mut cur: &[u8] = &[0x80, 0x30];
    let s = prelude_sig_decode(&mut cur);
    assert_eq!(s.n_state, 49);
    assert!(cur.is_empty());
}

#[test]
fn prelude_size_decode_vectors() {
    // Hand-computed from `MP_BC_PRELUDE_SIZE_DECODE_INTO` (bc.h).
    let mut cur: &[u8] = &[0x00];
    let s = prelude_size_decode(&mut cur);
    assert_eq!((s.n_info, s.n_cell), (0, 0));
    let mut cur: &[u8] = &[0x02];
    assert_eq!(prelude_size_decode(&mut cur).n_info, 1);
    let mut cur: &[u8] = &[0x01];
    assert_eq!(prelude_size_decode(&mut cur).n_cell, 1);
    // [0x81, 0x03]: byte 0 gives C bit 0 and I bits 0-5 = 0;
    // byte 1 gives C bit 1 and I bits 6-11 = 1 -> I = 64, C = 3.
    let mut cur: &[u8] = &[0x81, 0x03];
    let s = prelude_size_decode(&mut cur);
    assert_eq!((s.n_info, s.n_cell), (64, 3));
}

// ── 2. setup_code_state ──────────────────────────────────────

#[test]
fn setup_positional_only() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, b):\n    return a\n", &mut fx.pool);
    let cs = setup(&mut fx, &fun, &table, &[], 2, 0, &[Value::SmallInt(1), Value::SmallInt(2)])
        .expect("positional call must succeed");
    assert_eq!(arg(&cs, 0), Some(Value::SmallInt(1)));
    assert_eq!(arg(&cs, 1), Some(Value::SmallInt(2)));
    assert_eq!(cs.exc_sp_idx, 0);
    assert_eq!(cs.ip, expected_ip(&fun));
    // Every non-arg slot stays NULL.
    for (i, slot) in cs.state.iter().enumerate() {
        let is_arg_slot = i == cs.n_state - 1 || i == cs.n_state - 2;
        if !is_arg_slot {
            assert!(slot.is_none(), "slot {} must stay NULL", i);
        }
    }
}

#[test]
fn setup_positional_defaults() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, b=2):\n    return (a, b)\n", &mut fx.pool);
    let defaults = [Value::SmallInt(2)];
    // f(1) -> (1, 2): default fills b.
    let cs = setup(&mut fx, &fun, &table, &defaults, 1, 0, &[Value::SmallInt(1)])
        .expect("default call must succeed");
    assert_eq!(arg(&cs, 0), Some(Value::SmallInt(1)));
    assert_eq!(arg(&cs, 1), Some(Value::SmallInt(2)));
    assert_eq!(cs.ip, expected_ip(&fun));
    // f() -> too few (no-kw branch): TERSE mismatch (binary t2).
    let err = setup(&mut fx, &fun, &table, &defaults, 0, 0, &[]).unwrap_err();
    assert_eq!(err, SetupError::ArgCountMismatch);
    assert_eq!(err.message(&fx.pool), "argument num/types mismatch");
    // f(1, 2, 3) -> too many, no *args (binary t1 shape).
    let err = setup(
        &mut fx,
        &fun,
        &table,
        &defaults,
        3,
        0,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    )
    .unwrap_err();
    assert_eq!(err, SetupError::ArgCountMismatch);
}

#[test]
fn setup_varargs() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *args):\n    return args\n", &mut fx.pool);
    // f(1, 2, 3) -> args = (2, 3) (binary t11).
    let cs = setup(
        &mut fx,
        &fun,
        &table,
        &[],
        3,
        0,
        &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
    )
    .expect("varargs call must succeed");
    assert_eq!(arg(&cs, 0), Some(Value::SmallInt(1)));
    // The *args slot sits directly below the named args (with 1
    // named arg, that is the slot `arg(&cs, 1)` addresses).
    let args_ref = arg(&cs, 1).expect("args slot must be filled");
    match get(&fx, args_ref.as_heap().unwrap()) {
        Obj::Tuple(items) => {
            assert_eq!(items, &[Value::SmallInt(2), Value::SmallInt(3)])
        }
        other => panic!("expected varargs tuple, got {:?}", other),
    }
    // f(1) -> args is the empty-tuple singleton (binary t10:
    // `f(*a): return a is ()` is True).
    let cs = setup(&mut fx, &fun, &table, &[], 1, 0, &[Value::SmallInt(1)])
        .expect("empty varargs call must succeed");
    assert_eq!(arg(&cs, 1), Some(fx.empty_tuple));
}

#[test]
fn setup_kwargs_full_combo() {
    let mut fx = Fixture::new();
    let (fun, table) =
        compile_fun("def f(a, b=2, *args, k=9, **kw):\n    return (a, b, args, k, kw)\n", &mut fx.pool);
    let k_id = fx.q("k");
    let z_id = fx.q("z");
    let defaults = [Value::SmallInt(2)];
    // f(1, 3, 4, k=5, z=6) -> (1, 3, (4,), 5, {'z': 6}) (binary t12).
    let args = [
        Value::SmallInt(1),
        Value::SmallInt(3),
        Value::SmallInt(4),
        Value::Qstr(k_id),
        Value::SmallInt(5),
        Value::Qstr(z_id),
        Value::SmallInt(6),
    ];
    let cs = setup(&mut fx, &fun, &table, &defaults, 3, 2, &args)
        .expect("full-combo call must succeed");
    assert_eq!(arg(&cs, 0), Some(Value::SmallInt(1)));
    assert_eq!(arg(&cs, 1), Some(Value::SmallInt(3)));
    // *args slot is below the named args (a, b, k).
    let vpk = cs.n_state - 1 - 2 - 1;
    match get(&fx, cs.state[vpk].unwrap().as_heap().unwrap()) {
        Obj::Tuple(items) => assert_eq!(items, &[Value::SmallInt(4)]),
        other => panic!("expected varargs tuple, got {:?}", other),
    }
    assert_eq!(arg(&cs, 2), Some(Value::SmallInt(5))); // k
    // **kw slot is below *args.
    match get(&fx, cs.state[vpk - 1].unwrap().as_heap().unwrap()) {
        Obj::Dict(d) => {
            assert_eq!(
                d.map.entries,
                vec![MapEntry {
                    key: Some(Value::Qstr(z_id)),
                    value: Value::SmallInt(6),
                }]
            )
        }
        other => panic!("expected kwargs dict, got {:?}", other),
    }
    assert_eq!(cs.ip, expected_ip(&fun));
}

#[test]
fn setup_kwarg_overrides_default() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, b=10):\n    return (a, b)\n", &mut fx.pool);
    let b_id = fx.q("b");
    let defaults = [Value::SmallInt(10)];
    // f(1, b=20) -> (1, 20) (binary t19).
    let args = [Value::SmallInt(1), Value::Qstr(b_id), Value::SmallInt(20)];
    let cs = setup(&mut fx, &fun, &table, &defaults, 1, 1, &args)
        .expect("kwarg call must succeed");
    assert_eq!(arg(&cs, 0), Some(Value::SmallInt(1)));
    assert_eq!(arg(&cs, 1), Some(Value::SmallInt(20)));
}

#[test]
fn setup_missing_positional_via_kw() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, b=2):\n    return a\n", &mut fx.pool);
    let b_id = fx.q("b");
    let defaults = [Value::SmallInt(2)];
    // f(b=3) -> "function missing required positional argument #0"
    // (binary t3).
    let args = [Value::Qstr(b_id), Value::SmallInt(3)];
    let err = setup(&mut fx, &fun, &table, &defaults, 0, 1, &args).unwrap_err();
    assert_eq!(err, SetupError::MissingPositional { index: 0 });
    assert_eq!(
        err.message(&fx.pool),
        "function missing required positional argument #0"
    );

    let (fun2, table2) =
        compile_fun("def f(a, b, c=3):\n    return a\n", &mut fx.pool);
    let c_id = fx.q("c");
    let defaults2 = [Value::SmallInt(3)];
    // f(c=4) -> "... argument #1" (binary t4).
    let args2 = [Value::Qstr(c_id), Value::SmallInt(4)];
    let err = setup(&mut fx, &fun2, &table2, &defaults2, 0, 1, &args2).unwrap_err();
    assert_eq!(err, SetupError::MissingPositional { index: 1 });
    assert_eq!(
        err.message(&fx.pool),
        "function missing required positional argument #1"
    );
}

#[test]
fn setup_unexpected_keyword() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a):\n    return a\n", &mut fx.pool);
    let a_id = fx.q("a");
    let z_id = fx.q("z");
    // f(a=1, z=2) -> "unexpected keyword argument" (binary t5).
    let args = [
        Value::Qstr(a_id),
        Value::SmallInt(1),
        Value::Qstr(z_id),
        Value::SmallInt(2),
    ];
    let err = setup(&mut fx, &fun, &table, &[], 0, 2, &args).unwrap_err();
    assert_eq!(err, SetupError::UnexpectedKeyword);
    assert_eq!(err.message(&fx.pool), "unexpected keyword argument");
}

#[test]
fn setup_multiple_values_positional() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, b):\n    return a\n", &mut fx.pool);
    let a_id = fx.q("a");
    // f(1, a=2) -> "function got multiple values for argument 'a'"
    // (binary t6).
    let args = [Value::SmallInt(1), Value::Qstr(a_id), Value::SmallInt(2)];
    let err = setup(&mut fx, &fun, &table, &[], 1, 1, &args).unwrap_err();
    assert_eq!(err, SetupError::MultipleValues { qstr: a_id });
    assert_eq!(
        err.message(&fx.pool),
        "function got multiple values for argument 'a'"
    );
}

#[test]
fn setup_missing_keyword_only() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *, k):\n    return a\n", &mut fx.pool);
    // f(1) -> "function missing keyword-only argument" (binary t7).
    let err = setup(&mut fx, &fun, &table, &[], 1, 0, &[Value::SmallInt(1)]).unwrap_err();
    assert_eq!(err, SetupError::MissingKeywordOnly);
    assert_eq!(
        err.message(&fx.pool),
        "function missing keyword-only argument"
    );
}

#[test]
fn setup_missing_kwonly_with_defkwargs() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *, k, m=5):\n    return a\n", &mut fx.pool);
    let m_id = fx.q("m");
    // defkwargs dict: {m: 5}; extra_args = [defkwargs dict]
    // (n_def_pos_args == 0, so it sits at index 0).
    let defkw = Value::Heap(fx.arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![(Value::Qstr(m_id), Value::SmallInt(5))]),
        }),
        &fx.roots,
    ));
    let extra = [defkw];
    // f(1) -> "function missing required keyword argument 'k'"
    // (binary t8); m is filled from the defkwargs dict.
    let err = setup(&mut fx, &fun, &table, &extra, 1, 0, &[Value::SmallInt(1)]).unwrap_err();
    assert_eq!(err, SetupError::MissingKeyword { qstr: fx.q("k") });
    assert_eq!(
        err.message(&fx.pool),
        "function missing required keyword argument 'k'"
    );
}

#[test]
fn setup_missing_second_kwonly() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *, k, m):\n    return a\n", &mut fx.pool);
    let k_id = fx.q("k");
    // f(1, k=2) -> "... 'm'" (binary t9).
    let args = [Value::SmallInt(1), Value::Qstr(k_id), Value::SmallInt(2)];
    let err = setup(&mut fx, &fun, &table, &[], 1, 1, &args).unwrap_err();
    assert_eq!(err, SetupError::MissingKeyword { qstr: fx.q("m") });
    assert_eq!(
        err.message(&fx.pool),
        "function missing required keyword argument 'm'"
    );
}

#[test]
fn setup_kwonly_default_from_defkwargs() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *, k=7):\n    return k\n", &mut fx.pool);
    let k_id = fx.q("k");
    let defkw = Value::Heap(fx.arena.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![(Value::Qstr(k_id), Value::SmallInt(7))]),
        }),
        &fx.roots,
    ));
    let extra = [defkw];
    // f(1) -> k = 7 from the defkwargs dict.
    let cs = setup(&mut fx, &fun, &table, &extra, 1, 0, &[Value::SmallInt(1)])
        .expect("kwonly-default call must succeed");
    assert_eq!(arg(&cs, 1), Some(Value::SmallInt(7)));
}

#[test]
fn setup_duplicate_kwarg_in_dict() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(**kw):\n    return kw\n", &mut fx.pool);
    let z_id = fx.q("z");
    // f(z=1, z=2) -> multiple values via the dict path (binary t16).
    let args = [
        Value::Qstr(z_id),
        Value::SmallInt(1),
        Value::Qstr(z_id),
        Value::SmallInt(2),
    ];
    let err = setup(&mut fx, &fun, &table, &[], 0, 2, &args).unwrap_err();
    assert_eq!(err, SetupError::MultipleValues { qstr: z_id });
    assert_eq!(
        err.message(&fx.pool),
        "function got multiple values for argument 'z'"
    );
}

#[test]
fn setup_empty_varargs_and_kwargs() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun("def f(a, *args, **kw):\n    return (args, kw)\n", &mut fx.pool);
    // f(1) -> ((), {}) (binary t17).
    let cs = setup(&mut fx, &fun, &table, &[], 1, 0, &[Value::SmallInt(1)])
        .expect("empty varargs call must succeed");
    let vpk = cs.n_state - 1 - 1 - 0; // *args slot (1 named arg)
    assert_eq!(cs.state[vpk], Some(fx.empty_tuple));
    match get(&fx, cs.state[vpk - 1].unwrap().as_heap().unwrap()) {
        Obj::Dict(d) => assert!(d.map.entries.is_empty()),
        other => panic!("expected empty kwargs dict, got {:?}", other),
    }
}

#[test]
fn setup_closure_cell() {
    let mut fx = Fixture::new();
    let (fun, table) = compile_fun(
        "def f(x):\n    def g():\n        return x\n    return g\n",
        &mut fx.pool,
    );
    // Premise: x is a closed-over cell in f (n_cell == 1).
    assert_eq!(n_cell_of(&fun), 1, "x must be a cell in f");
    let cs = setup(&mut fx, &fun, &table, &[], 1, 0, &[Value::SmallInt(42)])
        .expect("closure call must succeed");
    assert_eq!(cs.ip, expected_ip(&fun));
    match get(&fx, arg(&cs, 0).unwrap().as_heap().unwrap()) {
        Obj::Cell(c) => assert_eq!(c.value, Some(Value::SmallInt(42))),
        other => panic!("expected a Cell wrapping x, got {:?}", other),
    }
}
