//! GC tests (Lane C4). Separated from `gc.rs` so both files stay
//! under the 800-line cap; the tests exercise only the public API.

use crate::memory::gc::*;
use crate::objects::value::{GcRef, Obj, Value};

fn alloc_float(arena: &mut GcArena, roots: &RootSet, f: f64) -> GcRef {
    arena.alloc(None, Obj::Float(f), roots)
}

fn alloc_tuple3(arena: &mut GcArena, roots: &RootSet) -> GcRef {
    // heap_bytes = 16 + 3*8 = 40 -> 2 blocks (64 bytes).
    arena.alloc(
        None,
        Obj::Tuple(vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]),
        roots,
    )
}

#[test]
fn alloc_get_roundtrip() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let r = alloc_float(&mut arena, &roots, 2.5);
    match arena.get(r) {
        Obj::Float(f) => assert_eq!(*f, 2.5),
        other => panic!("expected Float, got {:?}", other),
    }
    assert_eq!(arena.len(), 1);
}

#[test]
fn collect_frees_unreachable() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0);
    let _b = alloc_float(&mut arena, &roots, 2.0);
    assert_eq!(arena.len(), 2);
    let freed = arena.collect(&roots);
    assert_eq!(freed, 2);
    assert_eq!(arena.len(), 0);
}

#[test]
fn collect_keeps_rooted_and_transitively_reachable() {
    let mut arena = GcArena::new();
    let mut roots = RootSet::new();
    let inner = alloc_float(&mut arena, &roots, 9.0);
    let outer = arena.alloc(None, Obj::Tuple(vec![Value::Heap(inner)]), &roots);
    roots.push(Value::Heap(outer));
    let _dead = alloc_float(&mut arena, &roots, 3.0);
    assert_eq!(arena.len(), 3);
    let freed = arena.collect(&roots);
    assert_eq!(freed, 1);
    assert_eq!(arena.len(), 2);
    match arena.get(inner) {
        Obj::Float(f) => assert_eq!(*f, 9.0),
        _ => panic!("inner should survive"),
    }
}

#[test]
fn collect_reclaims_cycles() {
    // A list containing itself: unreachable cycle must be freed
    // (refcounting alone could not do this — the mark-sweep can).
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let r = arena.alloc(
        None,
        Obj::List(crate::objects::value::ListData { items: Vec::new() }),
        &roots,
    );
    if let Obj::List(l) = arena.get_mut(r) {
        l.items.push(Value::Heap(r));
    }
    assert_eq!(arena.len(), 1);
    let freed = arena.collect(&roots);
    assert_eq!(freed, 1);
    assert_eq!(arena.len(), 0);
}

#[test]
fn freed_slot_is_reused() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let a = alloc_float(&mut arena, &roots, 1.0);
    arena.collect(&roots);
    let b = alloc_float(&mut arena, &roots, 2.0);
    assert_eq!(a, b, "free-list reuse (id reuse after free, as in C)");
    match arena.get(b) {
        Obj::Float(f) => assert_eq!(*f, 2.0),
        _ => panic!("expected Float"),
    }
}

#[test]
fn block_accounting_mem_alloc_mem_free() {
    // Float: heap_bytes 32 -> 1 block (32 bytes). Tuple3: 40 -> 2 blocks (64).
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let a = alloc_float(&mut arena, &roots, 1.0);
    assert_eq!(arena.mem_alloc(), 32);
    assert_eq!(arena.mem_free(), 0);
    let _t = alloc_tuple3(&mut arena, &roots);
    assert_eq!(arena.mem_alloc(), 96);
    arena.free(a);
    assert_eq!(arena.mem_alloc(), 64);
    assert_eq!(arena.mem_free(), 32);
    // mem_alloc + mem_free == info total.
    let info = arena.info();
    assert_eq!(info.total, 96);
    assert_eq!(info.used, 64);
    assert_eq!(info.free, 32);
}

#[test]
fn info_counts() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0); // 1 block
    let _b = alloc_tuple3(&mut arena, &roots); // 2 blocks
    let c = alloc_float(&mut arena, &roots, 3.0); // 1 block
    arena.free(c); // one free run of 1 block at the end
    let info = arena.info();
    assert_eq!(info.used, 96); // 1 + 2 blocks
    assert_eq!(info.free, 32);
    assert_eq!(info.total, 128);
    assert_eq!(info.max_free, 1);
    assert_eq!(info.num_1block, 1);
    assert_eq!(info.num_2block, 1);
    assert_eq!(info.max_block, 2);
    // A longer free run: free the first float too.
    arena.free(GcRef(0));
    let info = arena.info();
    assert_eq!(info.free, 64);
    assert_eq!(info.max_free, 1); // runs are not adjacent (2-block live between)
}

#[test]
fn threshold_stress_collects_every_n() {
    // gc.zig allocWith stress block: alloc_count % stress_every == 0 -> collect.
    let mut arena = GcArena::new();
    arena.threshold = 2;
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0);
    assert_eq!(arena.len(), 1);
    let _b = alloc_float(&mut arena, &roots, 2.0); // collect runs first: a freed
    assert_eq!(arena.len(), 1);
    let _c = alloc_float(&mut arena, &roots, 3.0);
    assert_eq!(arena.len(), 2);
    let _d = alloc_float(&mut arena, &roots, 4.0); // collect runs first: b, c freed
    assert_eq!(arena.len(), 1);
    // alloc_count resets on collect; the stress counter does not:
    // after 4 allocs with threshold 2, exactly 2 stress collects ran.
    assert_eq!(arena.alloc_count, 1);
}

#[test]
fn threshold_off_and_gated() {
    // threshold <= 0: no stress collection.
    for t in [-1, 0] {
        let mut arena = GcArena::new();
        arena.threshold = t;
        let roots = RootSet::new();
        for _ in 0..5 {
            alloc_float(&mut arena, &roots, 1.0);
        }
        assert_eq!(arena.len(), 5);
    }
    // auto_collect disabled gates the stress rule (autoCollect()).
    let mut arena = GcArena::new();
    arena.threshold = 1;
    arena.set_auto_collect_enabled(false);
    let roots = RootSet::new();
    for _ in 0..3 {
        alloc_float(&mut arena, &roots, 1.0);
    }
    assert_eq!(arena.len(), 3);
    // A lock gates it too (autoCollect() requires lock_depth == 0).
    let mut arena = GcArena::new();
    arena.threshold = 1;
    arena.lock();
    let roots = RootSet::new();
    for _ in 0..3 {
        alloc_float(&mut arena, &roots, 1.0);
    }
    assert_eq!(arena.len(), 3);
    arena.unlock();
}

#[test]
fn collect_respects_lock() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0);
    arena.lock();
    assert!(arena.is_locked());
    assert_eq!(arena.collect(&roots), 0);
    assert_eq!(arena.len(), 1);
    arena.unlock();
    assert!(!arena.is_locked());
    assert_eq!(arena.collect(&roots), 1);
    assert_eq!(arena.len(), 0);
}

#[test]
#[should_panic(expected = "gc_unlock without a matching gc_lock")]
fn unlock_without_lock_panics() {
    let mut arena = GcArena::new();
    arena.unlock();
}

#[test]
fn collect_returns_freed_count() {
    // NOTE: the count is the Rust API (CPython-like). The original's
    // gc_collect() returns void and Python gc.collect() returns 0
    // (modgc.zig) — the D lane must return 0, not this count.
    let mut arena = GcArena::new();
    let mut roots = RootSet::new();
    let keep = alloc_float(&mut arena, &roots, 1.0);
    roots.push(Value::Heap(keep));
    let _a = alloc_float(&mut arena, &roots, 2.0);
    let _b = alloc_float(&mut arena, &roots, 3.0);
    let _c = alloc_float(&mut arena, &roots, 4.0);
    assert_eq!(arena.collect(&roots), 3);
    assert_eq!(arena.len(), 1);
}

#[test]
fn finaliser_runs_once() {
    // A finaliser object needs a type to be listed (gc.zig
    // runFinalisers skips typeless objects); a plain Float stands
    // in as the "type" here and survives via the header trace.
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let ty = alloc_float(&mut arena, &roots, 0.0);
    let _f = arena.alloc_with_finaliser(Some(ty), Obj::Float(1.0), &roots);
    assert_eq!(arena.collect(&roots), 0); // not freed: pending, not swept
    assert_eq!(arena.len(), 2);
    let pending = arena.take_pending_finalisers();
    assert_eq!(pending.len(), 1);
    // Simulate the VM holding the object while running __del__:
    // root it; the flag was cleared, so a second collect must not
    // re-list it.
    let mut roots2 = RootSet::new();
    roots2.push(Value::Heap(pending[0]));
    assert_eq!(arena.collect(&roots2), 0);
    assert!(arena.take_pending_finalisers().is_empty());
    // The VM frees it after running __del__; the stand-in type is
    // freed by the next collection once nothing references it.
    arena.free(pending[0]);
    assert_eq!(arena.len(), 1);
    assert_eq!(arena.collect(&roots), 1);
    assert_eq!(arena.len(), 0);
}

#[test]
fn finaliser_reachable_keeps_flag() {
    let mut arena = GcArena::new();
    let mut roots = RootSet::new();
    let ty = alloc_float(&mut arena, &roots, 0.0);
    let f = arena.alloc_with_finaliser(Some(ty), Obj::Float(1.0), &roots);
    roots.push(Value::Heap(f));
    assert_eq!(arena.collect(&roots), 0);
    assert!(arena.take_pending_finalisers().is_empty());
    assert_ne!(
        arena.header(f).flags & ObjHeader::FLAG_FINALISER,
        0,
        "marked finaliser keeps its flag (gc.zig: only .head runs)"
    );
    roots.extra.clear();
    assert_eq!(arena.collect(&roots), 0);
    assert_eq!(arena.take_pending_finalisers().len(), 1);
}

#[test]
fn finaliser_children_survive() {
    // The finaliser object's children must stay intact for __del__
    // (gc.zig runs finalisers before the sweep).
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let ty = alloc_float(&mut arena, &roots, 0.0);
    let child = alloc_float(&mut arena, &roots, 9.0);
    let _f = arena.alloc_with_finaliser(
        Some(ty),
        Obj::Tuple(vec![Value::Heap(child)]),
        &roots,
    );
    assert_eq!(arena.collect(&roots), 0);
    assert_eq!(arena.len(), 3);
    assert_eq!(arena.take_pending_finalisers().len(), 1);
    match arena.get(child) {
        Obj::Float(f) => assert_eq!(*f, 9.0),
        _ => panic!("child must survive for __del__"),
    }
}

#[test]
fn finaliser_typeless_skipped() {
    // gc.zig runFinalisers: `if type word == 0 continue` — no type
    // yet means no __del__; the object is freed by the sweep.
    // Contrast: a finaliser WITH a type is listed, not freed.
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let _typeless = arena.alloc_with_finaliser(None, Obj::Float(1.0), &roots);
    let t = alloc_float(&mut arena, &roots, 0.0); // stands in as a "type"
    let _typed = arena.alloc_with_finaliser(Some(t), Obj::Float(2.0), &roots);
    let freed = arena.collect(&roots);
    // Only the typeless object is freed: the typed one is listed
    // (not swept), and its "type" survives via the header trace.
    assert_eq!(freed, 1, "typeless finaliser freed without __del__");
    assert_eq!(arena.take_pending_finalisers().len(), 1, "typed one listed");
    assert_eq!(arena.len(), 2);
}

#[test]
fn header_type_is_traced() {
    // C0 fix: a type reachable ONLY through instance headers must
    // survive (gc.zig scanRange follows the first word = type ptr).
    let mut arena = GcArena::new();
    let mut roots = RootSet::new();
    let dict = arena.alloc(
        None,
        Obj::Dict(crate::objects::value::DictData {
            map: crate::objects::value::MapData::new(),
        }),
        &roots,
    );
    let ty = arena.alloc(
        None,
        Obj::Type(crate::objects::value::TypeData {
            name: 0,
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
        }),
        &roots,
    );
    let inst = arena.alloc(Some(ty), Obj::Float(1.0), &roots);
    roots.push(Value::Heap(inst)); // only the instance is rooted
    assert_eq!(arena.len(), 3);
    assert_eq!(arena.collect(&roots), 0);
    assert_eq!(arena.len(), 3);
    assert!(arena.is_live(ty) && arena.is_live(dict) && arena.is_live(inst));
}

#[test]
fn enter_leave_python() {
    let mut arena = GcArena::new();
    arena.enter_python();
    arena.enter_python();
    assert_eq!(arena.python_depth, 2);
    arena.leave_python();
    assert_eq!(arena.python_depth, 1);
    arena.leave_python();
    assert_eq!(arena.python_depth, 0);
}

#[test]
#[should_panic(expected = "leave_python without a matching enter_python")]
fn leave_python_without_enter_panics() {
    let mut arena = GcArena::new();
    arena.leave_python();
}

#[test]
fn free_ignores_stale() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let a = alloc_float(&mut arena, &roots, 1.0);
    arena.free(a);
    assert_eq!(arena.len(), 0);
    arena.free(a); // second free: no-op (gc.zig: kind != .head ignored)
    arena.free(GcRef(9999)); // outside the heap: no-op
    assert_eq!(arena.len(), 0);
    assert_eq!(arena.mem_alloc(), 0);
}

#[test]
fn realloc_shrink_grow() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let t = alloc_tuple3(&mut arena, &roots); // 2 blocks, 64 bytes
    assert_eq!(arena.nbytes(t), 64);
    assert_eq!(arena.mem_alloc(), 64);
    // Shrink to 1 block: same ref, accounting drops.
    assert_eq!(arena.realloc(t, 32, false), Some(t));
    assert_eq!(arena.nbytes(t), 32);
    assert_eq!(arena.mem_alloc(), 32);
    // Grow in place: Rust slots have no neighbours, so this always
    // succeeds and the ref is preserved (gc.zig would need free
    // following blocks or a move).
    assert_eq!(arena.realloc(t, 200, false), Some(t));
    assert_eq!(arena.nbytes(t), 224); // ceil(200/32) = 7 blocks
    assert_eq!(arena.mem_alloc(), 224);
    // Over MAX_ALLOC: None, object untouched.
    assert_eq!(arena.realloc(t, MAX_ALLOC + 1, true), None);
    assert_eq!(arena.nbytes(t), 224);
    // n_bytes == 0: gc_realloc rule — free + None.
    assert_eq!(arena.realloc(t, 0, true), None);
    assert_eq!(arena.len(), 0);
    assert_eq!(arena.mem_alloc(), 0);
    // Dead slot: None.
    assert_eq!(arena.realloc(t, 32, true), None);
}

#[test]
fn nbytes() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let a = alloc_float(&mut arena, &roots, 1.0);
    assert_eq!(arena.nbytes(a), 32);
    assert_eq!(arena.nbytes(GcRef(9999)), 0);
    arena.free(a);
    assert_eq!(arena.nbytes(a), 0);
}

#[test]
fn gc_init_reads_stress_env() {
    // gc.zig gc_init: NPY_GC_STRESS -> stress_every (parse failure -> 0).
    std::env::set_var("NPY_GC_STRESS", "3");
    let mut arena = GcArena::new();
    arena.gc_init();
    assert_eq!(arena.threshold, 3);
    assert!(arena.is_auto_collect_enabled());
    std::env::set_var("NPY_GC_STRESS", "not-a-number");
    arena.gc_init();
    assert_eq!(arena.threshold, 0);
    std::env::remove_var("NPY_GC_STRESS");
    arena.gc_init();
    assert_eq!(arena.threshold, 0);
}

#[test]
fn free_areas_resets() {
    let mut arena = GcArena::new();
    arena.threshold = 5;
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0);
    assert_eq!(arena.len(), 1);
    arena.free_areas();
    assert_eq!(arena.len(), 0);
    assert_eq!(arena.mem_alloc(), 0);
    assert_eq!(arena.mem_free(), 0);
    assert_eq!(arena.threshold, -1);
    assert!(arena.is_auto_collect_enabled());
}

#[test]
fn collect_start_end_sweep_all_noop() {
    let mut arena = GcArena::new();
    let roots = RootSet::new();
    let _a = alloc_float(&mut arena, &roots, 1.0);
    arena.collect_start();
    arena.collect_end();
    arena.sweep_all();
    assert_eq!(arena.len(), 1);
}
