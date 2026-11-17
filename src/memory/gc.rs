//! Garbage collector — mark-sweep arena (C0 skeleton).
//!
//! Port of the *design* of `src/memory/gc.zig` (vendor `py/gc.c`'s heap
//! design). One deliberate deviation, documented in
//! `docs/value-obj-design.md` D6: the Zig GC scans roots conservatively
//! (data segment + shadow stack); safe Rust cannot scan the native
//! stack, so **roots are explicit** — every live `Value` must be
//! reachable from the `RootSet` passed to `alloc`/`collect` at every
//! allocation point. A `Value` held only in a Rust local across an
//! `alloc` call is a leak-the-other-way bug (it may be collected);
//! the VM keeps all live values on its value stack, which is rooted.
//!
//! C4 (GC lane) ports `gc.zig` exactly: block accounting for
//! `mem_alloc`/`mem_free`, finalisers, `gc_collect_root`, the
//! allocation-count trigger rule. This skeleton's accounting is
//! approximate (`Obj::heap_bytes`) and its auto-collect default is off
//! (`threshold == -1`); both are C4's.
//!
//! The arena never moves objects, so `GcRef` indices stay valid for an
//! object's lifetime.

use crate::objects::value::{Obj, ObjHeader, Value};
use crate::pyerr::PyResult;

/// Explicit GC roots: everything the collector must keep alive.
/// The VM owns the `RootSet` (value stack, globals, builtins,
/// `pending_exc`, extra pinned values).
#[derive(Clone, Debug, Default)]
pub struct RootSet {
    /// VM value-stack values (the main root source).
    pub stack: Vec<Value>,
    /// Extra pinned values (globals, exception state, temporaries a
    /// builtin roots across an allocation).
    pub extra: Vec<Value>,
}

impl RootSet {
    pub fn new() -> RootSet {
        RootSet::default()
    }

    pub fn push(&mut self, v: Value) {
        self.extra.push(v);
    }

    /// All roots as one iterator.
    pub fn iter(&self) -> impl Iterator<Item = Value> + '_ {
        self.stack.iter().chain(self.extra.iter()).copied()
    }
}

/// Call context handed to type-slot and built-in functions: the arena
/// plus the roots they must use for every allocation. The VM builds one
/// per operation from its own arena + roots; C13 grows this struct
/// into the full `Vm` (the fn-pointer slot types stay unchanged).
pub struct CallCtx<'a> {
    pub arena: &'a mut GcArena,
    pub roots: &'a mut RootSet,
}

impl<'a> CallCtx<'a> {
    pub fn new(arena: &'a mut GcArena, roots: &'a mut RootSet) -> CallCtx<'a> {
        CallCtx { arena, roots }
    }

    /// Allocate an object, rooted at this context's roots.
    pub fn alloc(&mut self, type_: Option<crate::objects::value::GcRef>, obj: Obj) -> crate::objects::value::GcRef {
        // Borrow split: roots are borrowed by alloc, arena by self.
        let roots = &*self.roots;
        self.arena.alloc(type_, obj, roots)
    }
}

struct Slot {
    header: ObjHeader,
    payload: Option<Obj>,
}

/// Mark-sweep arena over `Obj` slots.
pub struct GcArena {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Bytes currently held by live objects (approximate;
    /// `Obj::heap_bytes` — C4 refines to block accounting).
    allocated_bytes: usize,
    /// Allocations since the last collection.
    pub alloc_count: usize,
    /// Auto-collect after this many allocations (`-1` = off in the
    /// skeleton; C4 lands the original's trigger rule).
    pub threshold: i64,
    /// Allocation lock depth (`gc_lock` in the original).
    pub lock_depth: usize,
}

impl GcArena {
    pub fn new() -> GcArena {
        GcArena {
            slots: Vec::new(),
            free: Vec::new(),
            allocated_bytes: 0,
            alloc_count: 0,
            threshold: -1,
            lock_depth: 0,
        }
    }

    /// Number of live objects.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.payload.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Allocate `obj` with header type `type_` (`None` only while
    /// bootstrapping the first type objects). May trigger a collection
    /// when the threshold is reached; `roots` must name every live
    /// value at this point.
    pub fn alloc(
        &mut self,
        type_: Option<crate::objects::value::GcRef>,
        obj: Obj,
        roots: &RootSet,
    ) -> crate::objects::value::GcRef {
        if self.threshold >= 0 && self.alloc_count as i64 >= self.threshold {
            self.collect(roots);
        }
        self.allocated_bytes += obj.heap_bytes();
        self.alloc_count += 1;
        let idx = match self.free.pop() {
            Some(i) => i as usize,
            None => {
                self.slots.push(Slot {
                    header: ObjHeader::new(type_),
                    payload: None,
                });
                self.slots.len() - 1
            }
        };
        self.slots[idx].header = ObjHeader::new(type_);
        self.slots[idx].payload = Some(obj);
        crate::objects::value::GcRef(idx as u32)
    }

    pub fn get(&self, r: crate::objects::value::GcRef) -> &Obj {
        self.slots[r.0 as usize]
            .payload
            .as_ref()
            .expect("GcRef points at a collected slot")
    }

    pub fn get_mut(&mut self, r: crate::objects::value::GcRef) -> &mut Obj {
        self.slots[r.0 as usize]
            .payload
            .as_mut()
            .expect("GcRef points at a collected slot")
    }

    pub fn header(&self, r: crate::objects::value::GcRef) -> ObjHeader {
        self.slots[r.0 as usize].header
    }

    /// Bytes held by live objects (approximate — see module docs).
    pub fn mem_alloc(&self) -> usize {
        self.allocated_bytes
    }

    /// Free heap bytes (approximate — see module docs).
    pub fn mem_free(&self) -> usize {
        self.slots.capacity().saturating_mul(32).saturating_sub(self.allocated_bytes)
    }

    /// Mark-sweep collection. Returns the number of objects freed
    /// (the original `gc_collect()` return semantics).
    pub fn collect(&mut self, roots: &RootSet) -> usize {
        // Mark: iterative worklist from the roots.
        let n = self.slots.len();
        let mut marked = vec![false; n];
        let mut work: Vec<u32> = Vec::new();
        for v in roots.iter() {
            if let Value::Heap(r) = v {
                if (r.0 as usize) < n && !marked[r.0 as usize] {
                    marked[r.0 as usize] = true;
                    work.push(r.0);
                }
            }
        }
        while let Some(i) = work.pop() {
            if let Some(obj) = &self.slots[i as usize].payload {
                for child in obj.children() {
                    if let Value::Heap(r) = child {
                        if (r.0 as usize) < n && !marked[r.0 as usize] {
                            marked[r.0 as usize] = true;
                            work.push(r.0);
                        }
                    }
                }
            }
        }
        // Sweep: free unmarked live slots.
        let mut freed = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.payload.is_some() && !marked[i] {
                self.allocated_bytes -= slot.payload.as_ref().unwrap().heap_bytes();
                slot.payload = None;
                self.free.push(i as u32);
                freed += 1;
            }
        }
        self.alloc_count = 0;
        freed
    }
}

impl Default for GcArena {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps `PyResult` reachable from this module's public surface.
#[allow(dead_code)]
fn _assert_pyresult(_: PyResult<()>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::value::{GcRef, Obj, Value};

    fn alloc_float(arena: &mut GcArena, roots: &RootSet, f: f64) -> GcRef {
        arena.alloc(None, Obj::Float(f), roots)
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
}
