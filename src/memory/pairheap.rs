//! Pairing heap — port of `src/memory/pairheap.zig` (vendor
//! `py/pairheap.c` + `py/pairheap.h`).
//!
//! A pure pairing heap: 3 fns over heap nodes + a less-than
//! comparator. No allocation in the algorithm itself, no GC
//! interaction, no config (pairheap.zig header).
//!
//! Representation — caller evidence first (the brief asked for it):
//! - In petitpy there are NO callers: `main.zig:105` and
//!   `main_vm.zig:101` only `_ = @import("memory/pairheap.zig")` to
//!   force-link the exports; no Zig code calls `mp_pairheap_*`.
//!   `build.zig` does not compile
//!   `vendor/micropython_embed/py/pairheap.c` (nor `modasyncio.c` /
//!   `machine_timer.c`, its vendor callers), so the Zig exports are
//!   the only implementation in the binary and nothing in the binary
//!   calls them.
//! - In vendor MicroPython the heap backs `extmod/modasyncio.c`
//!   (task queue) and `extmod/machine_timer.c` (timer heap): the node
//!   is INTRUSIVE — `mp_pairheap_t` is embedded FIRST in the
//!   task/timer struct, so the node pointer IS the object pointer
//!   (pairheap.h header comment).
//! - Safe Rust cannot do intrusive pointers without `unsafe`, so the
//!   port uses an index arena: `PairHeap<T>` owns `Vec` slots;
//!   `NodeId` (a slot index) plays the role of `mp_pairheap_t*`,
//!   `None` the role of `NULL`. The C `next`-pointer LSB tag
//!   (rightmost child → parent) becomes an explicit `Next` enum:
//!   `None` (top of heap / detached), `Sibling(id)`, `Parent(id)`.
//!   The `base`/`type` field of `mp_pairheap_t` is never read or
//!   written by meld/pairing/delete (verified against both the C and
//!   the Zig sources), so it has no Rust counterpart — the payload
//!   `T` carries whatever the caller needs.
//! - `mp_pairheap_new(lt)` (returns NULL) is `None` as the empty
//!   heap; `mp_pairheap_init_node` (child = child_last = next =
//!   NULL) is folded into node creation (`insert` initialises the
//!   links empty).
//! - `lt` returns C `int` (nonzero = true) in C/Zig; the Rust
//!   comparator is `&dyn Fn(&T, &T) -> bool`, passed to every call
//!   exactly as C passes `lt` to every call (the heap does not
//!   store it).
//!
//! Panic policy (documented, not hidden): the C code segfaults and
//! the Zig code panics on a malformed heap (dangling `next`, a
//! sibling chain with no parent tag, an unknown node id). The Rust
//! port panics with a message naming the violated invariant instead.
//! A well-formed heap — every node reachable from a root, built only
//! by `insert`/`meld`/`pairing`/`delete` — never hits these.

/// Slot index into a `PairHeap`'s arena; plays the role of
/// `mp_pairheap_t*`. `None` plays the role of `NULL`.
pub type NodeId = usize;

/// The `mp_pairheap_t.next` pointer, with its LSB tag made explicit
/// (pairheap.c:29-35): NULL = top of heap, LSB set = last child
/// pointing at its parent, other = a non-last child.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Next {
    /// Top of the heap, or a detached node (`NULL`).
    None,
    /// A child that is not the last (`next` = next sibling).
    Sibling(NodeId),
    /// The last (rightmost) child (`next` = parent, LSB-tagged in C).
    Parent(NodeId),
}

/// One heap node: the payload plus the `mp_pairheap_t` link fields
/// (`child`, `child_last`, `next`). The C `base`/`type` field is
/// absent — no heap function touches it.
struct HeapNode<T> {
    payload: T,
    child: Option<NodeId>,
    child_last: Option<NodeId>,
    next: Next,
}

/// Pairing heap arena: owns the nodes, links by `NodeId`.
pub struct PairHeap<T> {
    nodes: Vec<Option<HeapNode<T>>>,
    free: Vec<NodeId>,
}

impl<T> PairHeap<T> {
    /// Empty heap (`mp_pairheap_new` returns NULL; the empty heap
    /// here is `None` as a root, this is just the arena).
    pub fn new() -> Self {
        PairHeap {
            nodes: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Number of live nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|s| s.is_some()).count()
    }

    /// No live nodes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `mp_pairheap_init_node` + allocation: create a node holding
    /// `payload` with empty links, and return its id. A vacant slot
    /// left by `take` is reused first.
    pub fn insert(&mut self, payload: T) -> NodeId {
        let node = HeapNode {
            payload,
            child: None,
            child_last: None,
            next: Next::None,
        };
        if let Some(id) = self.free.pop() {
            self.nodes[id] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    /// Payload of a live node, or `None` for an unknown/taken id.
    pub fn get(&self, id: NodeId) -> Option<&T> {
        self.nodes
            .get(id)
            .and_then(|s| s.as_ref())
            .map(|n| &n.payload)
    }

    /// Take a detached node out of the arena, returning its payload
    /// and freeing the slot for reuse. Only valid for a node that is
    /// not currently in a heap — i.e. one whose links were cleared by
    /// `delete`, or one never melded in. Taking a node that is still
    /// in a heap corrupts the heap (the C equivalent is freeing a
    /// node that is still linked in: equally a caller bug).
    pub fn take(&mut self, id: NodeId) -> Option<T> {
        let slot = self.nodes.get_mut(id)?;
        let node = slot.take()?;
        self.free.push(id);
        Some(node.payload)
    }

    fn node(&self, id: NodeId) -> &HeapNode<T> {
        self.nodes
            .get(id)
            .and_then(|s| s.as_ref())
            .expect("pairheap: unknown node id")
    }

    fn node_mut(&mut self, id: NodeId) -> &mut HeapNode<T> {
        self.nodes
            .get_mut(id)
            .and_then(|s| s.as_mut())
            .expect("pairheap: unknown node id")
    }

    fn child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).child
    }

    fn child_last(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).child_last
    }

    fn next(&self, id: NodeId) -> Next {
        self.node(id).next
    }

    fn set_child(&mut self, id: NodeId, c: Option<NodeId>) {
        self.node_mut(id).child = c;
    }

    fn set_child_last(&mut self, id: NodeId, c: Option<NodeId>) {
        self.node_mut(id).child_last = c;
    }

    fn set_next(&mut self, id: NodeId, n: Next) {
        self.node_mut(id).next = n;
    }

    fn less_than(&self, lt: &dyn Fn(&T, &T) -> bool, a: NodeId, b: NodeId) -> bool {
        lt(&self.node(a).payload, &self.node(b).payload)
    }

    /// Follow a child-list `next` link while pairing: a sibling
    /// advances the walk, a parent tag ends it. `None` means a
    /// malformed list (a child whose `next` was never set); C would
    /// dereference NULL here and Zig would panic on `child.?`, so
    /// the Rust port panics too.
    fn advance(&self, id: NodeId) -> Option<NodeId> {
        match self.next(id) {
            Next::Sibling(n) => Some(n),
            Next::Parent(_) => None,
            Next::None => panic!("pairheap: malformed child list (node has no next link)"),
        }
    }

    /// `mp_pairheap_meld`: O(1), stable. Melds two heaps (either may
    /// be `None`) and returns the new root.
    pub fn meld(
        &mut self,
        lt: &dyn Fn(&T, &T) -> bool,
        heap1: Option<NodeId>,
        heap2: Option<NodeId>,
    ) -> Option<NodeId> {
        let (Some(h1), Some(h2)) = (heap1, heap2) else {
            return heap1.or(heap2);
        };
        if self.less_than(lt, h1, h2) {
            if self.child(h1).is_none() {
                self.set_child(h1, Some(h2));
            } else {
                let last = self.child_last(h1).expect("pairheap: child without child_last");
                self.set_next(last, Next::Sibling(h2));
            }
            self.set_child_last(h1, Some(h2));
            self.set_next(h2, Next::Parent(h1));
            Some(h1)
        } else {
            let h2_child = self.child(h2);
            self.set_next(
                h1,
                match h2_child {
                    Some(c) => Next::Sibling(c),
                    None => Next::None,
                },
            );
            self.set_child(h2, Some(h1));
            if h2_child.is_none() {
                self.set_child_last(h2, Some(h1));
                self.set_next(h1, Next::Parent(h2));
            }
            Some(h2)
        }
    }

    /// `mp_pairheap_pairing`: amortised O(log N), stable. Pairs up
    /// the child list left-to-right and melds the pairs; returns the
    /// new root (`None` iff the child list was empty).
    pub fn pairing(
        &mut self,
        lt: &dyn Fn(&T, &T) -> bool,
        child_in: Option<NodeId>,
    ) -> Option<NodeId> {
        let mut child = match child_in {
            None => return None,
            Some(c) => Some(c),
        };
        let mut heap: Option<NodeId> = None;
        while let Some(cur) = child {
            let mut n1 = cur;
            child = self.advance(cur);
            self.set_next(n1, Next::None);
            if let Some(cur2) = child {
                let n2 = cur2;
                child = self.advance(cur2);
                self.set_next(n2, Next::None);
                n1 = self.meld(lt, Some(n1), Some(n2)).expect("pairheap: meld of two nodes");
            }
            heap = self.meld(lt, heap, Some(n1));
        }
        // The list had at least one node, so at least one meld ran
        // and `heap` is non-null here.
        let h = heap.expect("pairheap: pairing produced an empty heap from a non-empty child list");
        self.set_next(h, Next::None);
        Some(h)
    }

    /// `mp_pairheap_delete`: amortised O(log N), stable. Removes
    /// `node` from `heap` and returns the new root. The removed
    /// node's links are cleared; its payload stays in the arena
    /// until `take` reclaims it. Deleting a node that is not in the
    /// heap returns the heap unchanged.
    pub fn delete(
        &mut self,
        lt: &dyn Fn(&T, &T) -> bool,
        heap: Option<NodeId>,
        node: NodeId,
    ) -> Option<NodeId> {
        // Simple case of the top being the node to delete.
        if Some(node) == heap {
            let child = self.child(node);
            self.set_child(node, None);
            return self.pairing(lt, child);
        }
        // Case where node is not in the heap.
        if self.next(node) == Next::None {
            return heap;
        }
        // Find parent of node: walk the sibling chain to the
        // rightmost sibling, whose `next` is the parent tag.
        let mut parent = node;
        while let Next::Sibling(n) = self.next(parent) {
            parent = n;
        }
        let par = match self.next(parent) {
            Next::Parent(p) => p,
            _ => panic!("pairheap: malformed sibling chain (no parent tag)"),
        };
        let nd_child = self.child(node);
        let nd_next = self.next(node);
        if Some(node) == self.child(par) && nd_child.is_none() {
            // First child, no children of its own: unlink it.
            match nd_next {
                Next::Parent(_) => self.set_child(par, None),
                Next::Sibling(n) => self.set_child(par, Some(n)),
                Next::None => panic!("pairheap: deleted node lost its next link"),
            }
            self.set_next(node, Next::None);
            return heap;
        }
        let next: Next;
        let n_node: NodeId;
        if Some(node) == self.child(par) {
            // First child with children: replace it with the pairing
            // of its children.
            let child = nd_child;
            next = nd_next;
            self.set_child(node, None);
            self.set_next(node, Next::None);
            // `child` is non-null here (the childless case returned
            // above), so pairing returns non-null.
            n_node = self
                .pairing(lt, child)
                .expect("pairheap: pairing of non-empty child list");
            self.set_child(par, Some(n_node));
        } else {
            // Middle/last child: find the preceding sibling.
            let mut n = self
                .child(par)
                .expect("pairheap: parent without children while deleting");
            while self.next(n) != Next::Sibling(node) {
                n = match self.next(n) {
                    Next::Sibling(m) => m,
                    _ => panic!("pairheap: malformed sibling chain while deleting"),
                };
            }
            let child = nd_child;
            next = nd_next;
            self.set_child(node, None);
            self.set_next(node, Next::None);
            match self.pairing(lt, child) {
                Some(p) => {
                    n_node = p;
                    self.set_next(n, Next::Sibling(n_node));
                }
                None => {
                    n_node = n;
                }
            }
        }
        self.set_next(n_node, next);
        if let Next::Parent(_) = next {
            self.set_child_last(par, Some(n_node));
        }
        heap
    }
}

impl<T> Default for PairHeap<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift64 (seed 20261117, the project's probe
    /// convention). No `rand` crate: pure std only.
    fn next_rand(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn min_lt(a: &i64, b: &i64) -> bool {
        a < b
    }

    fn max_lt(a: &i64, b: &i64) -> bool {
        a > b
    }

    /// Delete-min the whole heap; returns the payloads in removal order.
    fn drain_min(heap: &mut PairHeap<i64>, mut root: Option<NodeId>) -> Vec<i64> {
        let mut out = Vec::new();
        while let Some(r) = root {
            let v = *heap.get(r).unwrap();
            out.push(v);
            root = heap.delete(&min_lt, Some(r), r);
            heap.take(r);
        }
        out
    }

    #[test]
    fn meld_orders_two_nodes_and_handles_empty() {
        let mut h: PairHeap<i64> = PairHeap::new();
        let a = h.insert(5);
        let b = h.insert(3);
        assert_eq!(h.meld(&min_lt, None, Some(a)), Some(a));
        assert_eq!(h.meld(&min_lt, Some(a), None), Some(a));
        assert_eq!(h.meld(&min_lt, None, None), None);
        let root = h.meld(&min_lt, Some(a), Some(b)).unwrap();
        assert_eq!(*h.get(root).unwrap(), 3);
        // The loser is the root's only child.
        assert_eq!(h.delete(&min_lt, Some(root), root), Some(a));
        assert_eq!(*h.get(a).unwrap(), 5);
    }

    #[test]
    fn delete_root_with_one_child() {
        // The pairing-of-a-single-child case: the child list has one
        // node whose `next` is the parent tag, so pairing must still
        // return it (not crash on an empty meld).
        let mut h: PairHeap<i64> = PairHeap::new();
        let a = h.insert(1);
        let b = h.insert(2);
        let root = h.meld(&min_lt, Some(a), Some(b)).unwrap();
        let root2 = h.delete(&min_lt, Some(root), root).unwrap();
        assert_eq!(*h.get(root2).unwrap(), 2);
        assert_eq!(h.delete(&min_lt, Some(root2), root2), None);
    }

    #[test]
    fn delete_min_yields_sorted_order() {
        let mut rng: u64 = 20261117;
        let mut h: PairHeap<i64> = PairHeap::new();
        let mut values = Vec::new();
        let mut root: Option<NodeId> = None;
        for _ in 0..200 {
            let v = (next_rand(&mut rng) % 1000) as i64;
            values.push(v);
            let id = h.insert(v);
            root = h.meld(&min_lt, root, Some(id));
        }
        values.sort_unstable();
        assert_eq!(drain_min(&mut h, root), values);
        assert!(h.is_empty());
    }

    #[test]
    fn delete_arbitrary_nodes_keeps_heap_order() {
        let mut rng: u64 = 20261117;
        let mut h: PairHeap<i64> = PairHeap::new();
        let mut live: Vec<(NodeId, i64)> = Vec::new();
        let mut root: Option<NodeId> = None;
        for _ in 0..100 {
            let v = (next_rand(&mut rng) % 1000) as i64;
            let id = h.insert(v);
            live.push((id, v));
            root = h.meld(&min_lt, root, Some(id));
        }
        // Delete 30 arbitrary non-root nodes.
        let mut removed = 0;
        while removed < 30 {
            let idx = (next_rand(&mut rng) % live.len() as u64) as usize;
            let (id, _) = live[idx];
            if Some(id) == root {
                continue;
            }
            root = h.delete(&min_lt, root, id);
            let payload = h.take(id).unwrap();
            assert_eq!(payload, live[idx].1);
            live.swap_remove(idx);
            removed += 1;
        }
        let mut expected: Vec<i64> = live.iter().map(|(_, v)| *v).collect();
        expected.sort_unstable();
        assert_eq!(drain_min(&mut h, root), expected);
    }

    #[test]
    fn delete_node_not_in_heap_returns_heap_unchanged() {
        let mut h: PairHeap<i64> = PairHeap::new();
        let a = h.insert(1);
        let b = h.insert(2);
        let root = h.meld(&min_lt, Some(a), Some(b)).unwrap();
        let outsider = h.insert(0); // never melded in: next == None
        assert_eq!(h.delete(&min_lt, Some(root), outsider), Some(root));
        assert_eq!(*h.get(root).unwrap(), 1);
        assert_eq!(h.len(), 3);
        // The outsider is still takeable with its payload intact.
        assert_eq!(h.take(outsider), Some(0));
    }

    #[test]
    fn meld_two_heaps_then_drain_sorted() {
        let mut rng: u64 = 20261117;
        let mut h: PairHeap<i64> = PairHeap::new();
        let mut root1: Option<NodeId> = None;
        let mut root2: Option<NodeId> = None;
        let mut values = Vec::new();
        for _ in 0..50 {
            let v = (next_rand(&mut rng) % 500) as i64;
            values.push(v);
            let id = h.insert(v);
            root1 = h.meld(&min_lt, root1, Some(id));
        }
        for _ in 0..50 {
            let v = (next_rand(&mut rng) % 500) as i64;
            values.push(v);
            let id = h.insert(v);
            root2 = h.meld(&min_lt, root2, Some(id));
        }
        let root = h.meld(&min_lt, root1, root2).unwrap();
        values.sort_unstable();
        assert_eq!(drain_min(&mut h, Some(root)), values);
    }

    #[test]
    fn max_heap_via_reversed_comparator() {
        let mut h: PairHeap<i64> = PairHeap::new();
        let mut root: Option<NodeId> = None;
        for v in [3, 1, 4, 1, 5, 9, 2, 6] {
            let id = h.insert(v);
            root = h.meld(&max_lt, root, Some(id));
        }
        let mut out = Vec::new();
        while let Some(r) = root {
            out.push(*h.get(r).unwrap());
            root = h.delete(&max_lt, Some(r), r);
            h.take(r);
        }
        assert_eq!(out, vec![9, 6, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn empty_heap_ops() {
        let mut h: PairHeap<i64> = PairHeap::new();
        assert_eq!(h.pairing(&min_lt, None), None);
        assert!(h.is_empty());
        let a = h.insert(7);
        // Deleting the only node: root case, no children.
        assert_eq!(h.delete(&min_lt, Some(a), a), None);
        assert_eq!(h.take(a), Some(7));
        assert_eq!(h.get(a), None);
        assert_eq!(h.take(a), None);
    }

    #[test]
    fn take_reuses_slots() {
        let mut h: PairHeap<i64> = PairHeap::new();
        let a = h.insert(1);
        let b = h.insert(2);
        assert_eq!(h.take(a), Some(1));
        let c = h.insert(3);
        assert_eq!(c, a); // vacant slot reused
        assert_eq!(*h.get(b).unwrap(), 2);
        assert_eq!(*h.get(c).unwrap(), 3);
    }
}
