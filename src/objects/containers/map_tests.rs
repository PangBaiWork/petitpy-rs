//! Lane C9a map tests — growth / rehash / tombstones / the lookup
//! cache / `hash_of` / the set table, against the `map.zig`
//! semantics recorded in `map.rs`.
//!
//! Key hashes are small ints (hash == itself), bools (hash 1/0 via
//! the bool→int slot), `None` (identity hash 6 — the `OBJ_NONE`
//! word, as probed on the reference binary), qstrs (the pool hash)
//! and heap `Str`s (`qstr_compute_hash` directly).

use super::map::*;
use super::objdict::dict_new;
use super::tests::Fixture;
use crate::objects::obj::obj_id;
use crate::objects::value::{MapData, Obj, Value};
use crate::qstr_table::qstr_compute_hash;

fn map_of_fx(fx: &Fixture, d: Value) -> MapData {
    match d {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Dict(d) => d.map.clone(),
            _ => panic!("not a dict"),
        },
        _ => panic!("not heap"),
    }
}

#[test]
fn map_from_pairs_is_ordered() {
    let m = MapData::from_pairs(vec![
        (Value::SmallInt(3), Value::SmallInt(30)),
        (Value::SmallInt(1), Value::SmallInt(10)),
        (Value::SmallInt(2), Value::SmallInt(20)),
    ]);
    assert!(m.is_ordered);
    assert_eq!(m.used, 3);
    let mut cur = 0;
    let mut keys = Vec::new();
    while let Some(i) = m.next_live(&mut cur) {
        keys.push(m.entries[i].key.unwrap());
    }
    assert_eq!(
        keys,
        vec![Value::SmallInt(3), Value::SmallInt(1), Value::SmallInt(2)]
    );
}

#[test]
fn map_growth_keeps_all_keys_in_order() {
    let mut fx = Fixture::new();
    let d = dict_new(&mut fx.ctx(), 0);
    let r = d.as_heap().unwrap();
    // 200 int keys: crosses the u8→u16 index width (alloc passes
    // 169) and several rehashes.
    {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        for i in 0..200i64 {
            match map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Add) {
                Ok(LookupOutcome::Added(j)) => map_set_value(ctx.arena, r, j, Value::SmallInt(i * 10)),
                other => panic!("insert {i}: {other:?}"),
            }
        }
    }
    let m = map_of_fx(&fx, d);
    assert_eq!(m.used, 200);
    assert!(m.alloc >= 200, "alloc grew past the key count: {}", m.alloc);
    // `entries` is a PACKED vec (`len == fill`, tombstones included),
    // not an `alloc`-sized slot array — the rehash builds it with
    // `with_capacity(new_alloc)` and replays only the live entries.
    assert_eq!(m.entries.len(), m.used);
    assert!(m.alloc >= m.entries.len());
    // All keys found, insertion order preserved.
    let mut cur = 0;
    let mut keys = Vec::new();
    while let Some(i) = m.next_live(&mut cur) {
        keys.push(m.entries[i].key.unwrap());
    }
    assert_eq!(keys.len(), 200);
    assert_eq!(keys[0], Value::SmallInt(0));
    assert_eq!(keys[199], Value::SmallInt(199));
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    for i in 0..200i64 {
        match map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Lookup) {
            Ok(LookupOutcome::Found(j)) => {
                assert_eq!(map_value(ctx.arena, r, j), Value::SmallInt(i * 10))
            }
            other => panic!("lookup {i}: {other:?}"),
        }
    }
}

#[test]
fn map_tombstones_compact_on_rehash() {
    let mut fx = Fixture::new();
    let d = dict_new(&mut fx.ctx(), 0);
    let r = d.as_heap().unwrap();
    {
        let mut ctx = fx.ctx();
    let ctx = &mut ctx;
        for i in 0..20i64 {
            assert!(matches!(
                map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Add),
                Ok(LookupOutcome::Added(_))
            ));
        }
        // Remove the even keys — 10 tombstones.
        for i in (0..20i64).step_by(2) {
            match map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Remove) {
                Ok(LookupOutcome::Removed((k, _))) => assert_eq!(k, Value::SmallInt(i)),
                other => panic!("remove {i}: {other:?}"),
            }
        }
        // Fill again: `fill == alloc` forces a rehash that drops
        // the tombstones.
        for i in 20..30i64 {
            assert!(matches!(
                map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Add),
                Ok(LookupOutcome::Added(_))
            ));
        }
    }
    let m = map_of_fx(&fx, d);
    assert_eq!(m.used, 20);
    let tombstones = m.entries.iter().filter(|e| e.key.is_none()).count();
    assert_eq!(tombstones, 0, "rehash dropped the tombstones");
    // Odd keys 1..19 plus new keys 20..29, all present.
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    for i in (1..20i64).step_by(2).chain(20..30) {
        assert!(
            matches!(
                map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Lookup),
                Ok(LookupOutcome::Found(_))
            ),
            "key {i} lost after compaction"
        );
    }
    // Removed keys stay missing.
    for i in (0..20i64).step_by(2) {
        assert!(
            matches!(
                map_lookup(ctx, ctx.types, r, Value::SmallInt(i), LookupKind::Lookup),
                Ok(LookupOutcome::NotFound)
            ),
            "removed key {i} came back"
        );
    }
}

#[test]
fn map_lookup_cache_fast_path_and_invalidation() {
    let mut fx = Fixture::new();
    let d = dict_new(&mut fx.ctx(), 0);
    let r = d.as_heap().unwrap();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let k = Value::SmallInt(7);
    assert!(matches!(
        map_lookup(ctx, ctx.types, r, k, LookupKind::Add),
        Ok(LookupOutcome::Added(_))
    ));
    map_set_value(ctx.arena, r, 0, Value::SmallInt(70));
    // Second lookup hits the cache (same key, same slot).
    assert!(matches!(
        map_lookup(ctx, ctx.types, r, k, LookupKind::Lookup),
        Ok(LookupOutcome::Found(0))
    ));
    // Remove the key: the cache entry is stale but the key
    // comparison rejects it.
    assert!(matches!(
        map_lookup(ctx, ctx.types, r, k, LookupKind::Remove),
        Ok(LookupOutcome::Removed(_))
    ));
    assert!(matches!(
        map_lookup(ctx, ctx.types, r, k, LookupKind::Lookup),
        Ok(LookupOutcome::NotFound)
    ));
}

#[test]
fn map_hash_of_int_bool_none_qstr_str() {
    let mut fx = Fixture::new();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // Small int hashes to itself.
    assert_eq!(hash_of(ctx, ctx.types, Value::SmallInt(42)).unwrap(), 42);
    assert_eq!(
        hash_of(ctx, ctx.types, Value::SmallInt(-5)).unwrap(),
        (-5i64) as usize
    );
    // Bool via the bool→int HASH slot.
    assert_eq!(hash_of(ctx, ctx.types, Value::Bool(true)).unwrap(), 1);
    assert_eq!(hash_of(ctx, ctx.types, Value::Bool(false)).unwrap(), 0);
    // `None` has no HASH slot — identity hash, the OBJ_NONE word.
    assert_eq!(hash_of(ctx, ctx.types, Value::None).unwrap(), 6);
    assert_eq!(obj_id(Value::None), Value::SmallInt(6));
    // Qstr via the pool (== `qstr_compute_hash` of its bytes).
    let q = Value::Qstr(crate::objects::primitives::ops::static_qstr_id(b"dict").unwrap() as u32);
    assert_eq!(
        hash_of(ctx, ctx.types, q).unwrap(),
        qstr_compute_hash(b"dict") as usize
    );
    // Heap `Str`: same hash as the qstr of the same bytes.
    let s = fx.heap_str(b"abc");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        hash_of(ctx, ctx.types, s).unwrap(),
        qstr_compute_hash(b"abc") as usize
    );
    // Heap `Bytes`: same hash function over its bytes.
    let b = fx.heap_bytes(b"ab");
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    assert_eq!(
        hash_of(ctx, ctx.types, b).unwrap(),
        qstr_compute_hash(b"ab") as usize
    );
}

#[test]
fn map_hash_of_unhashable_and_unslotted() {
    let mut fx = Fixture::new();
    // A dict key: the dict type HAS a unary_op slot that returns
    // NULL for HASH → `TypeError("unsupported type for operator")`.
    let d = fx.dict();
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    match hash_of(ctx, ctx.types, d) {
        Err(MapError::TypeError(msg)) => {
            assert_eq!(msg, "unsupported type for operator")
        }
        other => panic!("dict key hash: {other:?}"),
    }
    // A tuple value: the fixture's tuple type has NO unary_op slot
    // → identity hash (the `obj_id` word).
    let t = fx.heap_tuple(vec![Value::SmallInt(1)]);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    let h = hash_of(ctx, ctx.types, t).unwrap();
    assert_eq!(h, obj_id(t).as_small_int().unwrap() as usize);
}

#[test]
fn map_set_table_ops() {
    let mut fx = Fixture::new();
    let mut set = MpSet::new(0);
    let mut ctx = fx.ctx();
    let ctx = &mut ctx;
    // ADD 1, 2, 3 (`set_lookup` returns `Some(elem)` when found or
    // added, `None` on a miss; `used` distinguishes the two).
    for i in 1..=3i64 {
        assert_eq!(
            set_lookup(ctx, ctx.types, &mut set, Value::SmallInt(i), LookupKind::Add)
                .unwrap(),
            Some(Value::SmallInt(i))
        );
    }
    assert_eq!(set.used, 3);
    // Adding again is a no-op (already present — `used` unchanged).
    assert_eq!(
        set_lookup(ctx, ctx.types, &mut set, Value::SmallInt(2), LookupKind::Add).unwrap(),
        Some(Value::SmallInt(2))
    );
    assert_eq!(set.used, 3);
    // LOOKUP: present / absent.
    assert_eq!(
        set_lookup(
            ctx,
            ctx.types,
            &mut set,
            Value::SmallInt(2),
            LookupKind::Lookup
        )
        .unwrap(),
        Some(Value::SmallInt(2))
    );
    assert_eq!(
        set_lookup(
            ctx,
            ctx.types,
            &mut set,
            Value::SmallInt(9),
            LookupKind::Lookup
        )
        .unwrap(),
        None
    );
    // REMOVE + re-add.
    assert_eq!(
        set_lookup(
            ctx,
            ctx.types,
            &mut set,
            Value::SmallInt(2),
            LookupKind::Remove
        )
        .unwrap(),
        Some(Value::SmallInt(2))
    );
    assert_eq!(set.used, 2);
    assert_eq!(
        set_lookup(
            ctx,
            ctx.types,
            &mut set,
            Value::SmallInt(2),
            LookupKind::Lookup
        )
        .unwrap(),
        None
    );
    assert_eq!(
        set_lookup(ctx, ctx.types, &mut set, Value::SmallInt(2), LookupKind::Add).unwrap(),
        Some(Value::SmallInt(2))
    );
    assert_eq!(set.used, 3);
    // `remove_first` (the `mp_set_remove_first` order trap: the
    // FIRST live element in TABLE order — keys 1/2/3 hash to slots
    // 1/2/3, so it is 1 here).
    assert_eq!(set.remove_first(), Some(Value::SmallInt(1)));
    assert_eq!(set.used, 2);
    set.clear();
    assert_eq!(set.used, 0);
    assert_eq!(set.alloc, 0);
}

#[test]
fn map_hash_alloc_sizes_cover_growth() {
    // The 34-entry table (map.zig `hash_allocation_sizes`) brackets
    // every growth step the tests above exercise.
    assert_eq!(HASH_ALLOCATION_SIZES.len(), 34);
    // The table itself (not the compact-dict growth formula):
    // 0, 2, 4, 6, 8, 10, 12, 17, 23, …, 10861, 16273, ….
    assert_eq!(get_hash_alloc_geq(0), 0);
    assert_eq!(get_hash_alloc_geq(1), 2);
    assert_eq!(get_hash_alloc_geq(17), 17);
    assert_eq!(get_hash_alloc_geq(200), 223);
    assert_eq!(get_hash_alloc_geq(10_000), 10861);
    // Past the table: the `(x + x/2) | 1` fallback.
    assert_eq!(get_hash_alloc_geq(100_000), 150001);
}
