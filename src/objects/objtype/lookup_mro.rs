//! C3 MRO for types (`objtype.zig:960-1040`): `mroOf` / `c3Mro`
//! plus the `__mro__` cache lookup. Split out of `lookup.rs` (E2
//! integration: that file grew past the 800-line cap).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::type_data;
use crate::objects::value::{GcRef, Obj, TypeData, Value};

use super::errors::ObjTypeError;
use super::subclass::is_exact_type;

/// C3's bound on bases (`objtype.zig:992`, "ponytail: a fixed
/// bound on bases").
pub const MAX_BASES: usize = 15;

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

fn mro_qstr() -> u32 {
    crate::objects::primitives::ops::static_qstr_id(b"__mro__")
        .expect("objtype dunder qstr is in the static table")
}
