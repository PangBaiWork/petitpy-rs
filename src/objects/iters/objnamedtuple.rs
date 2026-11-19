//! Named tuples (`objnamedtuple.zig`, 252 lines, 0 test blocks).
//!
//! The factory `mp_namedtuple_obj` is a 2-arg builtin
//! (`FunBuiltin_2`): `namedtuple(name, fields)`. The class it
//! builds is a `TypeData` whose slots are the tuple slots plus
//! `namedtuple_make_new` / `namedtuple_print` /
//! `namedtuple_attr`; the class's field names live in
//! `TypeData.namedtuple_fields` (the Zig keeps them in
//! `NamedtupleType`, after the header — see `value.rs`).
//!
//! An instance is a PLAIN TUPLE (`Obj::Tuple`) whose header
//! type is the class (the Zig: `tuple.base.type = type_in`) —
//! the C0 `NamedTupleData` stub is retired by this lane (see
//! `value.rs`). Equality, hashing, iteration and subscripting
//! therefore run through the tuple slots, with the class's
//! `getiter` (which IS `tuple_getiter`) making the
//! `ITER-SLOT` comparison rule work for namedtuples
//! (`P(1, 2) == (1, 2)` — probed).
//!
//! NOT exposed (the ASDICT branch is dead in the Zig —
//! `MICROPY_PY_COLLECTIONS_NAMEDTUPLE__ASDICT=0`):
//! `_asdict`, `_make`, `_replace`, `_fields` (probed:
//! `AttributeError("no attribute '_fields'")` — the field names
//! are NOT visible through attribute lookup, which is why they
//! live in `TypeData`, not in the class dict).
//!
//! The `make_new` signature follows the other lanes':
//! `(ctx, class, args, n_kw)` — the type-call machinery (C12)
//! calls it directly through the `namedtuple_make_new_slot`
//! adapter in `make_new.rs` (D4 wired the slot: C11 left
//! `make_new: None`, so `P(1, 2)` raised "can't create
//! instance" although the Zig attaches `namedtuple_make_new`
//! as the class's slot 1).

use super::iter::{
    value_to_qstr, InternFn, ARG_NUM_TYPES_MISMATCH, IterError,
};
use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::containers::objattrtuple::attrtuple_print_helper;
use crate::objects::containers::objtuple::{
    tuple_binary_op, tuple_getiter, tuple_subscr, tuple_unary_op,
};
use crate::objects::obj::ObjError;
use crate::objects::slots::PrintKind;
use crate::objects::value::{DictData, GcRef, MapData, Obj, TypeData, Value};
use crate::pyerr::PyResult;

/// `MP_TYPE_FLAG_EQ_CHECKS_OTHER_TYPE` (`0x0008`,
/// `mp_mirrors.zig:678`) — the named-tuple class flag.
pub const FLAG_EQ_CHECKS_OTHER_TYPE: u16 = 0x0008;

/// `new_namedtuple_type_2` (`objnamedtuple.zig:124-161`):
/// build the class. `name` is converted with
/// `mp_obj_str_get_qstr` (non-str → `TypeError("can't convert
/// to str implicitly")` — probed for `123` and for `bytes`
/// names); `fields` is a whitespace-separated string (split
/// on ASCII whitespace runs — probed: `'a  b\tc'`, `' a '`),
/// or a tuple/list of names (anything else → `TypeError("expected
/// tuple/list")` — probed for a generator, `None`, `5`).
/// Field names are NOT validated (probed: `'a-b'`, `'1x'`,
/// `''` are all accepted); duplicates are allowed and resolve
/// to the FIRST field (probed).
pub fn new_namedtuple_type(
    ctx: &mut CallCtx,
    name: Value,
    fields: Value,
    intern: InternFn<'_>,
) -> Result<Value, IterError> {
    let name_qstr = value_to_qstr(ctx, name, &mut *intern)?;
    let field_ids = fields_to_qstrs(ctx, fields, &mut *intern)?;
    let dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
    let data = TypeData {
        name: name_qstr,
        flags: FLAG_EQ_CHECKS_OTHER_TYPE,
        base: Some(ctx.types.tuple),
        mro: Vec::new(),
        dict,
        attr: Some(namedtuple_attr_fn),
        make_new: Some(crate::objects::objtype::make_new::namedtuple_make_new_slot),
        bases: None,
        unary_op: Some(tuple_unary_op),
        binary_op: Some(tuple_binary_op),
        call: None,
        getiter: Some(tuple_getiter),
        subscr: Some(tuple_subscr),
        buffer: None,
        print: Some(namedtuple_print),
        namedtuple_fields: Some(field_ids),
        protocol: None,
    };
    Ok(Value::Heap(ctx.alloc(None, Obj::Type(data))))
}

/// The `fields` argument of the factory, converted to qstr ids
/// (see [`new_namedtuple_type`]).
fn fields_to_qstrs(
    ctx: &mut CallCtx,
    fields: Value,
    intern: InternFn<'_>,
) -> Result<Vec<u32>, IterError> {
    match fields {
        Value::Qstr(id) => {
            let bytes = ctx.pool.qstr_str(id as u16).to_vec();
            split_and_intern(&bytes, intern)
        }
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Str(s) => {
                let bytes = s.bytes.clone();
                split_and_intern(&bytes, intern)
            }
            Obj::Tuple(items) => {
                let items = items.clone();
                items_to_qstrs(ctx, &items, intern)
            }
            Obj::List(l) => {
                let items = l.items.clone();
                items_to_qstrs(ctx, &items, intern)
            }
            _ => Err(IterError::Obj(ObjError::ExpectedTupleList)),
        },
        _ => Err(IterError::Obj(ObjError::ExpectedTupleList)),
    }
}

/// Split on ASCII whitespace runs and intern each piece (the
/// Zig's `mp_obj_str_split` with no separator).
fn split_and_intern(bytes: &[u8], intern: InternFn<'_>) -> Result<Vec<u32>, IterError> {
    let mut out = Vec::new();
    for piece in bytes.split(|b: &u8| b.is_ascii_whitespace()) {
        if !piece.is_empty() {
            out.push(intern(piece).map_err(IterError::from)?);
        }
    }
    Ok(out)
}

fn items_to_qstrs(
    ctx: &mut CallCtx,
    items: &[Value],
    intern: InternFn<'_>,
) -> Result<Vec<u32>, IterError> {
    let mut out = Vec::with_capacity(items.len());
    for &item in items {
        out.push(value_to_qstr(ctx, item, &mut *intern)?);
    }
    Ok(out)
}

/// The `AttrFn` adapter over the load-only `namedtuple_attr`
/// (the `attrtuple_attr_fn` precedent, D2): the Zig's
/// namedtuple class carries the attr slot
/// (`objnamedtuple.zig:196,214`), but C11 left `attr: None`
/// in `new_namedtuple_type`, so `P(1, 2).x` raised
/// `AttributeError` at the VM level. D4 wires it.
pub fn namedtuple_attr_fn(
    ctx: &mut CallCtx,
    self_v: Value,
    attr: u32,
) -> PyResult<Option<Value>> {
    Ok(namedtuple_attr(ctx.arena, self_v, attr))
}

/// The `namedtuple(name, fields)` factory as a plain
/// `BuiltinFn` (`mp_namedtuple_obj`, `FunBuiltin_2` —
/// `objnamedtuple.zig:238-240`): builds the class via
/// `new_namedtuple_type`, interning keyword/field names
/// through the pool (the `make_new_slot` precedent in
/// `make_new.rs`). The `collections` module binds this.
pub fn namedtuple_factory_builtin(
    ctx: &mut CallCtx,
    args: &[Value],
) -> PyResult<Value> {
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    new_namedtuple_type(ctx, args[0], args[1], &mut intern)
        .map_err(|e| ctx.record_typed(e))
}
pub fn namedtuple_fields_of(arena: &GcArena, class: GcRef) -> Option<Vec<u32>> {
    match arena.get(class) {
        Obj::Type(d) => d.namedtuple_fields.clone(),
        _ => None,
    }
}

/// `namedtuple_make_new` (`objnamedtuple.zig:163-238`): the
/// class call. Requires `n_args + n_kw == num_fields` — the
/// count is checked FIRST (probed: `P(z=1)` reports the count
/// error, not the unknown keyword). Positionals fill in order;
/// the rest fill from keywords by name; an unknown keyword
/// raises `TypeError("unexpected keyword argument '<name>'")`;
/// a slot filled twice raises `TypeError("function got
/// multiple values for argument '<name>'")`. The instance is a
/// plain tuple with header type = the class.
pub fn namedtuple_make_new(
    ctx: &mut CallCtx,
    class: GcRef,
    args: &[Value],
    n_kw: usize,
    intern: InternFn<'_>,
) -> Result<Value, IterError> {
    let fields = namedtuple_fields_of(ctx.arena, class)
        .ok_or(IterError::PyRaise)?;
    let num_fields = fields.len();
    let Some(n_pos) = args.len().checked_sub(n_kw * 2) else {
        return Err(IterError::TypeError(ARG_NUM_TYPES_MISMATCH));
    };
    if n_pos + n_kw != num_fields {
        return Err(IterError::WrongArgCount {
            expected: num_fields,
            got: n_pos + n_kw,
        });
    }
    // `OBJ_NULL` sentinels in the Zig; `None` here (a field
    // value may legitimately be `None`).
    let mut items: Vec<Option<Value>> = vec![None; num_fields];
    for (i, &v) in args[..n_pos].iter().enumerate() {
        items[i] = Some(v);
    }
    for pair in args[n_pos..].chunks(2) {
        let (key, val) = (pair[0], pair[1]);
        let key_qstr = value_to_qstr(ctx, key, &mut *intern)?;
        let Some(idx) = fields.iter().position(|&f| f == key_qstr) else {
            return Err(IterError::UnexpectedKwArg(key_qstr));
        };
        if items[idx].is_some() {
            return Err(IterError::MultipleValues(key_qstr));
        }
        items[idx] = Some(val);
    }
    let items: Vec<Value> = items.into_iter().map(|v| v.expect("count check fills every slot")).collect();
    // The Zig ALWAYS allocates a fresh tuple with the class as
    // its header (`namedtupleMakeNewImpl`, objnamedtuple.zig) —
    // even for 0 fields. `new_tuple` would return the shared
    // `()` singleton (header type `tuple`), so `TupEmpty()`
    // printed `()` instead of `TupEmpty()`.
    Ok(Value::Heap(ctx.alloc(Some(class), Obj::Tuple(items))))
}

/// `namedtuple_attr` (`objnamedtuple.zig:102-122`), LOAD only
/// — mirrors C9c's `attrtuple_attr`. `findField` scans in
/// order and returns the FIRST match (duplicates resolve to
/// the first field). `None` = not a field (C12 falls through
/// to the next lookup). The store/delete path raises
/// `AttributeError("can't set attribute")` in the Zig
/// (`raiseAttrReadOnly`); the store dispatch is C12's, which
/// must raise that error and must NOT write through here.
/// (The reference binary's `P(1,2).x = 5` reports
/// `AttributeError("no attribute 'x'")` from the VM's setattr
/// path, not the slot's message — recorded in the result file.)
pub fn namedtuple_attr(arena: &GcArena, self_v: Value, attr: u32) -> Option<Value> {
    let Value::Heap(r) = self_v else {
        return None;
    };
    let items = match arena.get(r) {
        Obj::Tuple(items) => items.clone(),
        _ => return None,
    };
    let class = arena.header(r).type_?;
    let fields = namedtuple_fields_of(arena, class)?;
    fields
        .iter()
        .position(|&f| f == attr)
        .map(|i| items[i])
}

/// `namedtuple_print` (`objnamedtuple.zig:69-78`): the type
/// name (`%q`) then `mp_obj_attrtuple_print_helper(print,
/// fields, tuple)`.
pub fn namedtuple_print(
    ctx: &mut CallCtx,
    v: Value,
    _kind: PrintKind,
    sink: &mut dyn crate::objects::slots::ByteSink,
) {
    let (class, items) = match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Tuple(items) => (ctx.arena.header(r).type_, items.clone()),
            _ => return,
        },
        _ => return,
    };
    let Some(class) = class else {
        return;
    };
    let (name, fields) = match ctx.arena.get(class) {
        Obj::Type(d) => (d.name, d.namedtuple_fields.clone().unwrap_or_default()),
        _ => return,
    };
    let name_bytes = ctx.pool.qstr_str(name as u16).to_vec();
    let _ = sink.write_str(&String::from_utf8_lossy(&name_bytes));
    attrtuple_print_helper(ctx, &fields, &items, sink);
}

#[cfg(test)]
mod tests {
    use super::super::iter::IterError;
    use super::super::tests::{collect_all, PanicSeams};
    use super::{
        namedtuple_attr, namedtuple_make_new, namedtuple_print, new_namedtuple_type,
    };
    use crate::objects::containers::objtuple::tuple_binary_op;
    use crate::objects::containers::tests::Fixture;
    use crate::objects::obj::ObjError;
    use crate::objects::slots::{BinaryOp, PrintKind};
    use crate::objects::value::{Obj, Value};
    use crate::pyerr::PyResult;
    use crate::runtime::qstr::RuntimeQstr;

    /// The intern seam for these tests: a `find_strn` lookup
    /// over the fixture pool (all test names/fields are
    /// pre-interned — see the module docs in `iter.rs`). Takes
    /// the pool reference directly (not the `CallCtx`) so the
    /// closure and the `&mut CallCtx` can coexist.
    fn intern_seam(pool: &RuntimeQstr) -> impl FnMut(&[u8]) -> PyResult<u32> + '_ {
        move |b: &[u8]| {
            let id = pool.find_strn(b);
            if id == 0 {
                panic!("test intern seam: uninterned name {}", String::from_utf8_lossy(b))
            } else {
                Ok(id as u32)
            }
        }
    }

    /// Build the class `P(x, y)`; returns `(class, x_id, y_id)`.
    fn make_p(fx: &mut Fixture) -> (Value, u32, u32) {
        let p_id = fx.pool.from_str("P").unwrap() as u32;
        let x_id = fx.pool.from_str("x").unwrap() as u32;
        let y_id = fx.pool.from_str("y").unwrap() as u32;
        let xy_id = fx.pool.from_str("x y").unwrap() as u32;
        let mut ctx = fx.ctx();
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(
            &mut ctx,
            Value::Qstr(p_id),
            Value::Qstr(xy_id),
            &mut intern,
        )
        .unwrap();
        (class, x_id, y_id)
    }

    #[test]
    fn namedtuple_factory() {
        let mut fx = Fixture::new();
        let (class, x_id, y_id) = make_p(&mut fx);
        let p_id = fx.pool.find_strn(b"P") as u32;
        let tuple_t = fx.types.tuple;
        let ctx = fx.ctx();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => panic!("factory did not return a heap class"),
        };
        match ctx.arena.get(class_ref) {
            Obj::Type(d) => {
                assert_eq!(d.name, p_id);
                assert_eq!(d.flags, 0x0008);
                assert_eq!(d.base, Some(tuple_t));
                assert_eq!(d.namedtuple_fields, Some(vec![x_id, y_id]));
                assert!(d.unary_op.is_some());
                assert!(d.binary_op.is_some());
                assert!(d.getiter.is_some());
                assert!(d.subscr.is_some());
                assert!(d.print.is_some());
            }
            _ => panic!("factory did not return a type"),
        }
    }

    #[test]
    fn namedtuple_fields_forms() {
        let mut fx = Fixture::new();
        let t_id = fx.pool.from_str("T").unwrap() as u32;
        let a_id = fx.pool.from_str("a").unwrap() as u32;
        let b_id = fx.pool.from_str("b").unwrap() as u32;
        let c_id = fx.pool.from_str("c").unwrap() as u32;
        let abc_id = fx.pool.from_str("a  b\tc").unwrap() as u32;
        let edge_id = fx.pool.from_str(" a ").unwrap() as u32;
        let empty_id = fx.pool.find_strn(b"") as u32;
        let fields = fx.heap_tuple(vec![Value::Qstr(a_id), Value::Qstr(b_id)]);
        let mut ctx = fx.ctx();
        // Whitespace-separated string (probed).
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(&mut ctx, Value::Qstr(t_id), Value::Qstr(abc_id), &mut intern)
            .unwrap();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        match ctx.arena.get(class_ref) {
            Obj::Type(d) => assert_eq!(
                d.namedtuple_fields,
                Some(vec![a_id, b_id, c_id])
            ),
            _ => panic!("not a type"),
        }
        // Leading/trailing whitespace is stripped.
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(&mut ctx, Value::Qstr(t_id), Value::Qstr(edge_id), &mut intern)
            .unwrap();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        match ctx.arena.get(class_ref) {
            Obj::Type(d) => assert_eq!(d.namedtuple_fields, Some(vec![a_id])),
            _ => panic!("not a type"),
        }
        // Tuple / list of names.
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(&mut ctx, Value::Qstr(t_id), fields, &mut intern).unwrap();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        match ctx.arena.get(class_ref) {
            Obj::Type(d) => assert_eq!(d.namedtuple_fields, Some(vec![a_id, b_id])),
            _ => panic!("not a type"),
        }
        // Empty fields.
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(
            &mut ctx,
            Value::Qstr(t_id),
            Value::Qstr(empty_id),
            &mut intern,
        )
        .unwrap();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        match ctx.arena.get(class_ref) {
            Obj::Type(d) => assert_eq!(d.namedtuple_fields, Some(vec![])),
            _ => panic!("not a type"),
        }
    }

    #[test]
    fn namedtuple_factory_errors() {
        let mut fx = Fixture::new();
        let t_id = fx.pool.from_str("T").unwrap() as u32;
        let x_id = fx.pool.from_str("x").unwrap() as u32;
        let b = fx.heap_bytes(b"T");
        let fields = fx.heap_tuple(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let mut ctx = fx.ctx();
        // Int name → "can't convert to str implicitly" (probed).
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            new_namedtuple_type(&mut ctx, Value::SmallInt(123), Value::Qstr(x_id), &mut intern),
            Err(IterError::TypeError("can't convert to str implicitly"))
        );
        // Bytes name → same (probed).
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            new_namedtuple_type(&mut ctx, b, Value::Qstr(x_id), &mut intern),
            Err(IterError::TypeError("can't convert to str implicitly"))
        );
        // Fields not tuple/list/str → "expected tuple/list".
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            new_namedtuple_type(&mut ctx, Value::Qstr(t_id), Value::SmallInt(5), &mut intern),
            Err(IterError::Obj(ObjError::ExpectedTupleList))
        );
        // Int field name → "can't convert to str implicitly".
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            new_namedtuple_type(&mut ctx, Value::Qstr(t_id), fields, &mut intern),
            Err(IterError::TypeError("can't convert to str implicitly"))
        );
    }

    #[test]
    fn namedtuple_make_new_behaviour() {
        let mut fx = Fixture::new();
        let (class, x_id, y_id) = make_p(&mut fx);
        let z_id = fx.pool.from_str("z").unwrap() as u32;
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        let mut ctx = fx.ctx();
        let mut intern = intern_seam(ctx.pool);
        // P(1, 2) → a plain tuple with header type P.
        let p = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(1), Value::SmallInt(2)],
            0,
            &mut intern,
        )
        .unwrap();
        match p {
            Value::Heap(r) => {
                assert_eq!(ctx.arena.header(r).type_, Some(class_ref));
                match ctx.arena.get(r) {
                    Obj::Tuple(items) => {
                        assert_eq!(items, &[Value::SmallInt(1), Value::SmallInt(2)])
                    }
                    _ => panic!("instance is not a tuple"),
                }
            }
            _ => panic!("instance is not heap"),
        }
        // Keywords.
        let mut intern = intern_seam(ctx.pool);
        let p = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[
                Value::Qstr(x_id),
                Value::SmallInt(1),
                Value::Qstr(y_id),
                Value::SmallInt(2),
            ],
            2,
            &mut intern,
        )
        .unwrap();
        match p {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => {
                    assert_eq!(items, &[Value::SmallInt(1), Value::SmallInt(2)])
                }
                _ => panic!("instance is not a tuple"),
            },
            _ => panic!("instance is not heap"),
        }
        // Mixed positional + keyword.
        let mut intern = intern_seam(ctx.pool);
        let p = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(1), Value::Qstr(y_id), Value::SmallInt(2)],
            1,
            &mut intern,
        )
        .unwrap();
        match p {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => {
                    assert_eq!(items, &[Value::SmallInt(1), Value::SmallInt(2)])
                }
                _ => panic!("instance is not a tuple"),
            },
            _ => panic!("instance is not heap"),
        }
        // Count errors (checked FIRST — probed).
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            namedtuple_make_new(&mut ctx, class_ref, &[Value::SmallInt(1)], 0, &mut intern),
            Err(IterError::WrongArgCount {
                expected: 2,
                got: 1
            })
        );
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            namedtuple_make_new(
                &mut ctx,
                class_ref,
                &[Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)],
                0,
                &mut intern
            ),
            Err(IterError::WrongArgCount {
                expected: 2,
                got: 3
            })
        );
        // Unknown keyword (count matches first).
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            namedtuple_make_new(
                &mut ctx,
                class_ref,
                &[Value::Qstr(x_id), Value::SmallInt(1), Value::Qstr(z_id), Value::SmallInt(2)],
                2,
                &mut intern
            ),
            Err(IterError::UnexpectedKwArg(z_id))
        );
        // Duplicate → multiple values.
        let mut intern = intern_seam(ctx.pool);
        assert_eq!(
            namedtuple_make_new(
                &mut ctx,
                class_ref,
                &[Value::SmallInt(1), Value::Qstr(x_id), Value::SmallInt(2)],
                1,
                &mut intern
            ),
            Err(IterError::MultipleValues(x_id))
        );
    }

    #[test]
    fn namedtuple_attr_and_print() {
        let mut fx = Fixture::new();
        let (class, x_id, y_id) = make_p(&mut fx);
        let bogus = fx.pool.from_str("bogus").unwrap() as u32;
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        let mut ctx = fx.ctx();
        let mut intern = intern_seam(ctx.pool);
        let p = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(1), Value::SmallInt(2)],
            0,
            &mut intern,
        )
        .unwrap();
        // .x → 1, .y → 2, unknown → None (first match wins is
        // covered by the duplicate-field test below).
        assert_eq!(namedtuple_attr(ctx.arena, p, x_id), Some(Value::SmallInt(1)));
        assert_eq!(namedtuple_attr(ctx.arena, p, y_id), Some(Value::SmallInt(2)));
        assert_eq!(namedtuple_attr(ctx.arena, p, bogus), None);
        // print: `P(x=1, y=2)` (probed).
        let mut out = String::new();
        namedtuple_print(&mut ctx, p, PrintKind::Repr, &mut out);
        assert_eq!(out, "P(x=1, y=2)");
    }

    #[test]
    fn namedtuple_duplicate_fields() {
        let mut fx = Fixture::new();
        let r_id = fx.pool.from_str("R").unwrap() as u32;
        let a_id = fx.pool.from_str("a").unwrap() as u32;
        let fields = fx.heap_list(vec![Value::Qstr(a_id), Value::Qstr(a_id)]);
        let mut ctx = fx.ctx();
        let mut intern = intern_seam(ctx.pool);
        let class = new_namedtuple_type(&mut ctx, Value::Qstr(r_id), fields, &mut intern).unwrap();
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        let mut intern = intern_seam(ctx.pool);
        let r = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(1), Value::SmallInt(2)],
            0,
            &mut intern,
        )
        .unwrap();
        // First match wins (probed: `R(['a', 'a']).a == 1`).
        assert_eq!(namedtuple_attr(ctx.arena, r, a_id), Some(Value::SmallInt(1)));
        // Keyword fill also hits the first slot.
        let mut intern = intern_seam(ctx.pool);
        let r2 = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(9), Value::Qstr(a_id), Value::SmallInt(8)],
            1,
            &mut intern,
        );
        // n_pos(1) + n_kw(1) == 2 fields; the kw 'a' fills slot
        // 0 — but slot 0 is already filled positionally → the
        // duplicate error.
        assert_eq!(r2, Err(IterError::MultipleValues(a_id)));
    }

    #[test]
    fn namedtuple_tuple_behaviour() {
        let mut fx = Fixture::new();
        let (class, _x, _y) = make_p(&mut fx);
        let class_ref = match class {
            Value::Heap(r) => r,
            _ => unreachable!(),
        };
        let plain = fx.heap_tuple(vec![Value::SmallInt(1), Value::SmallInt(2)]);
        let three = fx.heap_tuple(vec![Value::SmallInt(3)]);
        let mut ctx = fx.ctx();
        let mut intern = intern_seam(ctx.pool);
        let p = namedtuple_make_new(
            &mut ctx,
            class_ref,
            &[Value::SmallInt(1), Value::SmallInt(2)],
            0,
            &mut intern,
        )
        .unwrap();
        // P(1,2) == (1,2) — the ITER-SLOT comparison rule (the
        // class's getiter IS tuple_getiter; probed).
        let eq = tuple_binary_op(&mut ctx, BinaryOp::Equal, p, plain).unwrap();
        assert_eq!(eq, Value::Bool(true));
        let eq2 = tuple_binary_op(&mut ctx, BinaryOp::Equal, p, p).unwrap();
        assert_eq!(eq2, Value::Bool(true));
        // P(1,2) + (3,) → plain tuple (1,2,3) (probed).
        let sum = tuple_binary_op(&mut ctx, BinaryOp::Add, p, three).unwrap();
        match sum {
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Tuple(items) => assert_eq!(
                    items,
                    &[
                        Value::SmallInt(1),
                        Value::SmallInt(2),
                        Value::SmallInt(3)
                    ]
                ),
                _ => panic!("sum is not a tuple"),
            },
            _ => panic!("sum is not heap"),
        }
        // Iteration through the unified dispatch.
        let mut ps = PanicSeams::new();
        let it = super::super::iter::getiter(&mut ctx, p).unwrap();
        let items = collect_all(&mut ctx, it, &mut ps.seams()).unwrap();
        assert_eq!(items, vec![Value::SmallInt(1), Value::SmallInt(2)]);
    }
}
