//! GC probe (Lane C4): allocate a batch of objects — plain, rooted,
//! finaliser-bearing, cyclic, multi-block — run `collect()`, and
//! check the freed count and the block accounting against gc.zig's
//! rules. Run with `cargo run --release --example gc_probe`.
//!
//! Expected numbers (32-byte blocks; Float = 1 block, Tuple3 = 2):
//!   before: 13 objects, mem_alloc 448, mem_free 0
//!   collect frees 8 (5 plain floats + 2-cycle + big tuple)
//!   after:  5 live (2 rooted floats + dummy type + finaliser tuple
//!            + its child), mem_alloc 160, mem_free 288, total 448,
//!            1 pending finaliser, info max_free 5 blocks.

use petitpy_rs::memory::gc::{GcArena, RootSet};
use petitpy_rs::objects::value::{GcRef, ListData, Obj, Value};

fn main() {
    let mut arena = GcArena::new();
    let mut roots = RootSet::new();

    // 5 plain unrooted floats (1 block each).
    for i in 0..5 {
        arena.alloc(None, Obj::Float(i as f64), &roots);
    }
    // 2 rooted floats.
    for i in 0..2 {
        let r = arena.alloc(None, Obj::Float(100.0 + i as f64), &roots);
        roots.push(Value::Heap(r));
    }
    // Finaliser tuple with a float child, unrooted (1 block each).
    // The tuple needs a type to be listed for __del__ (gc.zig
    // runFinalisers skips typeless objects); a plain Float stands in.
    let ty = arena.alloc(None, Obj::Float(0.0), &roots);
    let child = arena.alloc(None, Obj::Float(9.0), &roots);
    let fin = arena.alloc_with_finaliser(
        Some(ty),
        Obj::Tuple(vec![Value::Heap(child)]),
        &roots,
    );
    // Unreachable 2-cycle: A contains B, B contains A.
    let a: GcRef = arena.alloc(None, Obj::List(ListData { items: Vec::new() }), &roots);
    let b: GcRef = arena.alloc(None, Obj::List(ListData { items: Vec::new() }), &roots);
    if let Obj::List(l) = arena.get_mut(a) {
        l.items.push(Value::Heap(b));
    }
    if let Obj::List(l) = arena.get_mut(b) {
        l.items.push(Value::Heap(a));
    }
    // Big unrooted tuple: 3 items -> 40 bytes -> 2 blocks.
    let _big = arena.alloc(
        None,
        Obj::Tuple(vec![Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)]),
        &roots,
    );

    println!("before collect: live={} mem_alloc={} mem_free={}",
        arena.len(), arena.mem_alloc(), arena.mem_free());
    assert_eq!(arena.len(), 13);
    assert_eq!(arena.mem_alloc(), 448);
    assert_eq!(arena.mem_free(), 0);

    let freed = arena.collect(&roots);
    let info = arena.info();
    println!("collect freed={} live={} mem_alloc={} mem_free={} total={}",
        freed, arena.len(), arena.mem_alloc(), arena.mem_free(), info.total);
    println!("info: used={} free={} max_free={} num_1block={} num_2block={} max_block={}",
        info.used, info.free, info.max_free, info.num_1block, info.num_2block, info.max_block);
    let pending = arena.take_pending_finalisers();
    println!("pending finalisers={} (slot {:?})", pending.len(), pending.first());
    println!("finaliser child alive: {}", arena.is_live(child));

    assert_eq!(freed, 8, "5 floats + 2-cycle + big tuple");
    assert_eq!(arena.len(), 5);
    assert_eq!(arena.mem_alloc(), 160);
    assert_eq!(arena.mem_free(), 288);
    assert_eq!(info.total, 448);
    assert_eq!(info.used, 160);
    assert_eq!(info.free, 288);
    assert_eq!(info.max_free, 5, "free run of the 5 plain floats");
    assert_eq!(info.num_1block, 5);
    assert_eq!(info.num_2block, 0);
    assert_eq!(info.max_block, 1);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0], fin);
    assert!(arena.is_live(child), "finaliser child survives for __del__");
    assert!(arena.is_live(fin));
    assert!(arena.is_live(ty), "dummy type survives via header trace");

    println!("PROBE OK");
}
