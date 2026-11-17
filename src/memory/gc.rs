//! Garbage collector — mark-sweep arena (C4: `gc.zig` landed).
//!
//! Port of `src/memory/gc.zig` (569 lines; vendor `py/gc.c`'s heap
//! design, re-implemented in Zig). The arena never moves objects, so
//! `GcRef` indices stay valid for an object's lifetime.
//!
//! # What maps 1:1
//!
//! | gc.zig | Rust |
//! |---|---|
//! | `BYTES_PER_BLOCK` (32) / block-rounded sizes | `BYTES_PER_BLOCK`, `blocks_for()` |
//! | `MAX_ALLOC` (16 MiB cap) | `MAX_ALLOC` (enforced by `realloc` + `alloc` helpers) |
//! | `GC_ALLOC_FLAG_HAS_FINALISER` | `ObjHeader::FLAG_FINALISER` |
//! | `gc_alloc` / `allocMaybe` / `allocWith` | `GcArena::alloc` / `alloc_with_finaliser` |
//! | `gc_free` | `GcArena::free` (stale frees ignored, as in Zig) |
//! | `gc_realloc` / `reallocMaybe` | `GcArena::realloc` |
//! | `gc_nbytes` | `GcArena::nbytes` |
//! | `gc_info` (+ vendor `gc_info_t`) | `GcArena::info` -> `GcInfo` |
//! | `gc_collect` (export) | `GcArena::collect` (lock check first) |
//! | `collect()` (internal) | `GcArena::collect_inner` (no lock check; callers guarantee it) |
//! | `gc_lock` / `gc_unlock` / `gc_is_locked` | `lock` / `unlock` / `is_locked` |
//! | `gc_init` | `gc_init` (auto-collect on + `NPY_GC_STRESS` env) |
//! | `enterPython` / `leavePython` | `enter_python` / `leave_python` |
//! | `gc_collect_start` / `gc_collect_end` / `gc_sweep_all` | no-op methods of the same names |
//! | `freeAreas` | `free_areas` (resets the arena) |
//! | `runFinalisers` | pending-finaliser list (`take_pending_finalisers`), see below |
//! | `alloc_count` / `stress_every` | `alloc_count` / `threshold` (stress rule, see below) |
//! | `allocated_blocks` (blocks since last collect) | folded into `allocated_bytes` accounting |
//! | `lock_depth` / `python_depth` | fields of the same names |
//!
//! # Deliberate deviations (all grounded in the original)
//!
//! * **Roots are explicit** (design doc D6): gc.zig scans the data
//!   segment + C stack conservatively (`scanRange` / `dataSegments` /
//!   `collectFrom`); safe Rust cannot scan the native stack, so the
//!   collector marks from the `RootSet` passed to `alloc`/`collect`
//!   (the VM value stack, globals, builtins, `pending_exc` — i.e. the
//!   `mp_state_ctx` root-pointer section + pystack that vendor's
//!   `gc_collect_start` traces via `gc_collect_root`). `gc_collect_root`
//!   itself is an empty no-op in gc.zig, so it has no arena method:
//!   its Rust counterpart is `RootSet::push` (adding to `extra`).
//! * **No `stack_top == null` early return**: gc.zig's `collect()`
//!   returns without collecting when no stack top is set yet
//!   ("stack-only objects would be freed"). Rust roots are always
//!   explicit, so the collection always runs.
//! * **`threshold` is the stress rule, not the vendor threshold**:
//!   gc.zig has no `GC_THRESHOLD` / `gc_alloc_amount` mechanism
//!   (vendor `py/gc.c:787-788, 880` does; gc.zig replaced it with
//!   "collect when an allocation finds no room" + `NPY_GC_STRESS`).
//!   `threshold > 0` therefore means "collect every `threshold`
//!   allocations" (`allocWith`'s `stress_every` block, `gc.zig:490-493`);
//!   `threshold <= 0` means stress off (the default `-1`). The
//!   Python-visible `gc.threshold()` in `modgc.zig` is a separate
//!   no-op that ignores its argument — the D lane must NOT wire it
//!   to this field.
//! * **No "collect on no room"**: the Rust arena is a `Vec`, it never
//!   runs out of room, so there is no `allocBlocks`-failure trigger
//!   and no `addArea` / "grow when more than half in use" rule. The
//!   only automatic trigger is the stress rule above.
//! * **Finalisers run outside the sweep**: gc.zig's `runFinalisers`
//!   calls `__del__` between marking and sweeping (with the GC
//!   locked and `finalising` set, so blocks allocated by a finaliser
//!   are created marked). The Rust arena cannot call `__del__` —
//!   that needs the VM's call machinery (`mp_call_function_1_protected`
//!   in Zig). Instead `collect()` moves unmarked finaliser objects
//!   (flag cleared first, exactly as gc.zig does) onto a pending
//!   list, marks them + their transitive children live for this
//!   collection so `__del__` still sees an intact object, and the VM
//!   drains the list with `take_pending_finalisers()`, runs `__del__`
//!   through its normal call path, then frees each object. Only
//!   native types use finalisers (files, sockets); user classes'
//!   `__del__` is never called, as in the original. The flag is
//!   cleared when the object is listed, so a finaliser runs at most
//!   once. `pending_exc` save/restore around the call (gc.zig
//!   `runFinalisers`) is the VM hook's job — the arena holds no
//!   `pending_exc` (it lives in `VmState`, C13).
//! * **`collect()` returns the freed-object count**: gc.zig's
//!   `gc_collect()` returns `void` and Python `gc.collect()` returns
//!   `0` always (`modgc.zig`: `PY_GC_COLLECT_RETVAL=1`, "the bumping
//!   gc.zig allocator never sets gc_collected; always 0"). The Rust
//!   count is the frozen C0 API (CPython-like); the D-lane `gc`
//!   module must return `0` from Python `gc.collect()`, not this count.
//! * **Block accounting is per-slot**: each arena slot records its
//!   block count (`n_blocks`, from `Obj::heap_bytes`, block-rounded).
//!   `mem_alloc()` is the live block bytes, `mem_free()` the free
//!   slot bytes — the same numbers `gc_info` reports as `used`/`free`
//!   (`modgc.zig`'s `mem_alloc`/`mem_free` read exactly those).
//!   Rust-heap buffers inside payloads (`Vec` contents) live outside
//!   the arena, unlike the original where every buffer is a GC
//!   allocation, so `mem_alloc()` counts object slots only.
//! * **No `gc_dump` / `gc_debug` / `gcCollectDebug`**: gc.zig has no
//!   such functions (verified by grep); its only diagnostic is
//!   `gc_info`, ported as `info()`.
//! * **No `gc_free_all`**: it does not exist in gc.zig, petitpy `src/`,
//!   or vendor `py/gc.c` (verified by grep); the actual function is
//!   `freeAreas`, ported as `free_areas()`.
//! * **wasm branches are not coded**: gc.zig's `collect_wanted` /
//!   half-heap `leavePython` rule / safe-point collection are
//!   `is_wasm`-gated; wasm is out of scope for petitpy-rs (native
//!   only), so on native `gc_collect()` never defers.
//! * **`ObjHeader::FLAG_MARK` is unused**: the mark bit lives in the
//!   collection's local bitmap (the ATB `MARK` kind's Rust form),
//!   not in the header; `FLAG_FINALISER` is used.
//!
//! # Header-type tracing (C0 fix, grounded in gc.zig)
//!
//! gc.zig's `scanRange` follows every word of a live allocation,
//! including its first word — the type pointer (`MpObjBase.type`).
//! The C0 skeleton only traced `Obj::children()` (payload), so a
//! type object reachable only through instance headers could be
//! freed while its instances survived. `collect_inner` therefore
//! also marks each live slot's `header.type_`.

use crate::objects::obj::BuiltinTypes;
use crate::objects::value::{GcRef, Obj, ObjHeader, Value};
use crate::pyerr::PyResult;
use crate::runtime::qstr::RuntimeQstr;

/// Bytes per GC block (`gc.zig`: `4 * @sizeOf(usize)`, 64-bit).
pub const BYTES_PER_BLOCK: usize = 32;

/// Per-allocation cap (`gc.zig`: `MAX_ALLOC = 16 * 1024 * 1024`).
/// `allocWith` returns null above it; `realloc` likewise.
pub const MAX_ALLOC: usize = 16 * 1024 * 1024;

/// `gc_alloc` flag bit (`gc.zig`: `GC_ALLOC_FLAG_HAS_FINALISER = 1`).
pub const GC_ALLOC_FLAG_HAS_FINALISER: u32 = 1;

/// Blocks for `n_bytes` (`gc.zig` `allocWith`: `max(1, ceil(n / BYTES_PER_BLOCK))`).
pub fn blocks_for(n_bytes: usize) -> usize {
    ((n_bytes + BYTES_PER_BLOCK - 1) / BYTES_PER_BLOCK).max(1)
}

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
/// `types` is the C13 growth done early by C7r: the thin slot
/// adapters need the builtin-type registry to call the rich slot
/// functions, and a `fn` pointer cannot capture it — so it rides on
/// the context (the C5/C6 explicit-parameter precedent, threaded
/// through the context instead of every call site).
/// `pool` is the same growth done by C9a, for the same reason: the
/// map/dict hash path needs the runtime qstr pool (`qstr_hash` for
/// dynamic ids — `map.zig:349` calls the C `qstr_hash`, which reads
/// the pool), and the slot/builtin signatures cannot carry it. It is
/// a shared reference: interning (`from_strn`) stays the VM's job
/// (C13 owns the `&mut` pool); lanes only read hashes/bytes through
/// it. No thread-local pool (the C8a draft's `INSTALLED_TYPES` /
/// `PENDING_STR_ERROR` thread-locals were flagged at review — shared
/// state rides on the context, not on globals).
pub struct CallCtx<'a> {
    pub arena: &'a mut GcArena,
    pub roots: &'a mut RootSet,
    pub types: &'a BuiltinTypes,
    pub pool: &'a RuntimeQstr,
}

impl<'a> CallCtx<'a> {
    pub fn new(
        arena: &'a mut GcArena,
        roots: &'a mut RootSet,
        types: &'a BuiltinTypes,
        pool: &'a RuntimeQstr,
    ) -> CallCtx<'a> {
        CallCtx { arena, roots, types, pool }
    }

    /// Allocate an object, rooted at this context's roots.
    pub fn alloc(&mut self, type_: Option<GcRef>, obj: Obj) -> GcRef {
        // Borrow split: roots are borrowed by alloc, arena by self.
        let roots = &*self.roots;
        self.arena.alloc(type_, obj, roots)
    }
}

struct Slot {
    header: ObjHeader,
    payload: Option<Obj>,
    /// Allocation size in blocks (`gc.zig` ATB: head + tails).
    /// Kept for free slots too (free capacity for `info()`).
    n_blocks: usize,
}

/// `gc_info` (`gc.zig` `Info`, vendor `gc_info_t`): total/used/free
/// in bytes, the rest in blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcInfo {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    pub max_free: usize,
    pub num_1block: usize,
    pub num_2block: usize,
    pub max_block: usize,
}

/// Mark-sweep arena over `Obj` slots.
pub struct GcArena {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Bytes currently held by live objects, block-rounded
    /// (`gc.zig` `gc_info().used`; `modgc.zig`'s `mem_alloc`).
    allocated_bytes: usize,
    /// Allocations since the last collection.
    pub alloc_count: usize,
    /// Stress trigger (`gc.zig` `stress_every`, `NPY_GC_STRESS`):
    /// collect every `threshold` allocations when `> 0`; `<= 0`
    /// means off (default `-1`). NOT the vendor `gc_alloc_threshold`
    /// mechanism (gc.zig does not implement it) and NOT Python
    /// `gc.threshold()` (a no-op in `modgc.zig`).
    pub threshold: i64,
    /// Allocation lock depth (`gc_lock` in the original).
    pub lock_depth: usize,
    /// Auto-collect switch (`mpstate.zig` `gc_auto_collect_enabled`,
    /// set to 1 by `gc_init`; gates the stress rule only — an
    /// explicit `collect()` always collects).
    pub auto_collect_enabled: bool,
    /// `enterPython` / `leavePython` nesting depth.
    pub python_depth: usize,
    /// Monotonic allocation counter for the stress rule (`gc.zig`
    /// `alloc_count`, which — unlike `alloc_count` here — is never
    /// reset by a collection, so `stress % threshold == 0` keeps its
    /// phase across explicit collections).
    stress_alloc_count: usize,
    /// Unmarked finaliser objects from the last collection, in slot
    /// order; the VM drains this, runs their `__del__`, then frees them.
    pending_finalisers: Vec<GcRef>,
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
            // Post-`gc_init` state: "CPython + MicroPython both start
            // with gc enabled" (gc.zig gc_init). `gc_init()` re-asserts it.
            auto_collect_enabled: true,
            python_depth: 0,
            stress_alloc_count: 0,
            pending_finalisers: Vec::new(),
        }
    }

    /// `gc_init` (`gc.zig:178-193`): the start/end pool arguments are
    /// unused (areas come from calloc); sets `gc_auto_collect_enabled`
    /// to 1 and, native only, reads `NPY_GC_STRESS` into `stress_every`
    /// (parse failure -> 0).
    pub fn gc_init(&mut self) {
        self.auto_collect_enabled = true;
        self.threshold = std::env::var("NPY_GC_STRESS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|n| n as i64)
            .unwrap_or(0);
    }

    /// Number of live objects.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.payload.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `r` names a live slot.
    pub fn is_live(&self, r: GcRef) -> bool {
        self.slots
            .get(r.0 as usize)
            .map(|s| s.payload.is_some())
            .unwrap_or(false)
    }

    /// `gc_lock` (`gc.zig:195`): locking only defers collection;
    /// allocation stays allowed (unlike vendor, which returns NULL
    /// from `gc_alloc` while locked).
    pub fn lock(&mut self) {
        self.lock_depth += 1;
    }

    /// `gc_unlock` (`gc.zig:198`).
    pub fn unlock(&mut self) {
        self.lock_depth = self
            .lock_depth
            .checked_sub(1)
            .expect("gc_unlock without a matching gc_lock");
    }

    /// `gc_is_locked` (`gc.zig:201`).
    pub fn is_locked(&self) -> bool {
        self.lock_depth != 0
    }

    /// Whether auto-collection is enabled (`modgc.zig`
    /// `py_gc_isenabled`; the D lane wires `gc.enable/disable` here).
    pub fn is_auto_collect_enabled(&self) -> bool {
        self.auto_collect_enabled
    }

    /// Set the auto-collect switch (`modgc.zig` `py_gc_enable` /
    /// `py_gc_disable` poke `gc_auto_collect_enabled`).
    pub fn set_auto_collect_enabled(&mut self, enabled: bool) {
        self.auto_collect_enabled = enabled;
    }

    /// `enterPython` (`gc.zig:215`): brackets each entry into Python
    /// from outside (script, REPL line, `.mpy` run, JS callback).
    /// Entries nest when Python calls back out and in again.
    pub fn enter_python(&mut self) {
        self.python_depth += 1;
    }

    /// `leavePython` (`gc.zig:218-224`): the wasm half-heap rule
    /// (`collect_wanted or allocated_blocks * 2 > total_blocks` at
    /// depth 0) is `is_wasm`-gated and has no native counterpart —
    /// on native this only decrements the depth.
    pub fn leave_python(&mut self) {
        self.python_depth = self
            .python_depth
            .checked_sub(1)
            .expect("leave_python without a matching enter_python");
    }

    /// `gc_collect_start` / `gc_collect_end` (`gc.zig:226-227`): empty
    /// no-ops, copied verbatim.
    pub fn collect_start(&mut self) {}
    pub fn collect_end(&mut self) {}

    /// `gc_sweep_all` (`gc.zig:232`): empty no-op, copied verbatim.
    pub fn sweep_all(&mut self) {}

    /// `freeAreas` (`gc.zig:145-147`): returns the heap's areas to
    /// the system for an interpreter that is destroyed; the caller
    /// then resets the variables. In Rust that is a full reset to a
    /// fresh arena.
    pub fn free_areas(&mut self) {
        *self = GcArena::new();
    }

    /// Allocate `obj` with header type `type_` (`None` only while
    /// bootstrapping the first type objects). `roots` must name every
    /// live value at this point; the stress rule may collect first
    /// (`allocWith`'s `stress_every` block, `gc.zig:490-493`).
    pub fn alloc(&mut self, type_: Option<GcRef>, obj: Obj, roots: &RootSet) -> GcRef {
        self.alloc_inner(type_, obj, roots, false)
    }

    /// `allocWith(n, true)` / `m_malloc_with_finaliser`: allocate with
    /// the finaliser flag set, so `collect()` lists the object for its
    /// `__del__` when it becomes unreachable.
    pub fn alloc_with_finaliser(
        &mut self,
        type_: Option<GcRef>,
        obj: Obj,
        roots: &RootSet,
    ) -> GcRef {
        self.alloc_inner(type_, obj, roots, true)
    }

    fn alloc_inner(
        &mut self,
        type_: Option<GcRef>,
        obj: Obj,
        roots: &RootSet,
        finaliser: bool,
    ) -> GcRef {
        // Stress rule first, exactly as gc.zig allocWith: only when
        // stress is on and auto-collection is allowed (lock_depth == 0
        // and gc_auto_collect_enabled != 0).
        if self.threshold > 0 && self.auto_collect_enabled && self.lock_depth == 0 {
            self.stress_alloc_count += 1;
            if self.stress_alloc_count % self.threshold as usize == 0 {
                self.collect_inner(roots);
            }
        }
        let n_blocks = blocks_for(obj.heap_bytes());
        self.allocated_bytes += n_blocks * BYTES_PER_BLOCK;
        self.alloc_count += 1;
        let idx = match self.free.pop() {
            Some(i) => i as usize,
            None => {
                self.slots.push(Slot {
                    header: ObjHeader::new(type_),
                    payload: None,
                    n_blocks: 0,
                });
                self.slots.len() - 1
            }
        };
        let mut header = ObjHeader::new(type_);
        if finaliser {
            header.flags |= ObjHeader::FLAG_FINALISER;
        }
        self.slots[idx].header = header;
        self.slots[idx].payload = Some(obj);
        self.slots[idx].n_blocks = n_blocks;
        GcRef(idx as u32)
    }

    pub fn get(&self, r: GcRef) -> &Obj {
        self.slots[r.0 as usize]
            .payload
            .as_ref()
            .expect("GcRef points at a collected slot")
    }

    pub fn get_mut(&mut self, r: GcRef) -> &mut Obj {
        self.slots[r.0 as usize]
            .payload
            .as_mut()
            .expect("GcRef points at a collected slot")
    }

    pub fn header(&self, r: GcRef) -> ObjHeader {
        self.slots[r.0 as usize].header
    }

    /// Set a slot's header type after allocation. Needed for
    /// self-referential type objects: `mp_type_type`'s header type
    /// is itself (objtype.zig `mp_type_type`), so the Rom registry
    /// allocates it first and then points its header at itself.
    /// C12 adds it.
    pub fn set_header_type(&mut self, r: GcRef, type_: Option<GcRef>) {
        self.slots[r.0 as usize].header.type_ = type_;
    }

    /// Bytes held by live objects, block-rounded (`gc.zig`
    /// `gc_info().used`; `modgc.zig`'s `gc.mem_alloc()`).
    pub fn mem_alloc(&self) -> usize {
        self.allocated_bytes
    }

    /// Free heap bytes, block-rounded (`gc.zig` `gc_info().free`;
    /// `modgc.zig`'s `gc.mem_free()`). The sum over free slots of
    /// their block sizes — the GC_SPLIT_HEAP_AUTO=0 reading.
    pub fn mem_free(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.payload.is_none())
            .map(|s| s.n_blocks * BYTES_PER_BLOCK)
            .sum()
    }

    /// `gc_info` (`gc.zig:245-270`): walks the heap run by run; a
    /// live allocation is one run (head + tails), a free run is
    /// consecutive free blocks. total/used/free in bytes, the rest in
    /// blocks.
    pub fn info(&self) -> GcInfo {
        let mut info = GcInfo::default();
        let mut i = 0;
        while i < self.slots.len() {
            if self.slots[i].payload.is_none() {
                let mut len = 0;
                while i < self.slots.len() && self.slots[i].payload.is_none() {
                    len += self.slots[i].n_blocks;
                    i += 1;
                }
                info.free += len;
                info.max_free = info.max_free.max(len);
            } else {
                let len = self.slots[i].n_blocks;
                info.used += len;
                info.max_block = info.max_block.max(len);
                if len == 1 {
                    info.num_1block += 1;
                }
                if len == 2 {
                    info.num_2block += 1;
                }
                i += 1;
            }
        }
        info.used *= BYTES_PER_BLOCK;
        info.free *= BYTES_PER_BLOCK;
        info.total = info.used + info.free;
        info
    }

    /// `gc_nbytes` (`gc.zig:563-569`): the allocation's size in bytes
    /// (block-rounded); 0 for a dead or out-of-range ref (a non-head
    /// block in Zig).
    pub fn nbytes(&self, r: GcRef) -> usize {
        match self.slots.get(r.0 as usize) {
            Some(s) if s.payload.is_some() => s.n_blocks * BYTES_PER_BLOCK,
            _ => 0,
        }
    }

    /// `gc_free` (`gc.zig:543-549`): frees the allocation at once
    /// (the sweep is not involved, and no finaliser runs — only
    /// `collect()`'s finaliser pass calls `__del__`). A stale free
    /// (dead slot, or a pointer outside the heap) is ignored, as in Zig.
    pub fn free(&mut self, r: GcRef) {
        let i = r.0 as usize;
        if i >= self.slots.len() || self.slots[i].payload.is_none() {
            return;
        }
        self.allocated_bytes -= self.slots[i].n_blocks * BYTES_PER_BLOCK;
        self.slots[i].payload = None;
        self.slots[i].header.flags = 0;
        self.free.push(r.0);
    }

    /// `gc_realloc` / `reallocMaybe` (`gc.zig:508-561`): resize the
    /// allocation to `n_bytes` (block-rounded). `n_bytes == 0` frees
    /// and returns `None` (the `gc_realloc` rule; note `m_realloc`
    /// instead goes through `reallocMaybe` and keeps one block — see
    /// `alloc::realloc`). Above `MAX_ALLOC` returns `None` with the
    /// object untouched. Rust slots have no neighbouring blocks, so
    /// growing in place always succeeds — the `allow_move` fallback
    /// (move + copy) never triggers, and the ref is always preserved.
    /// The payload itself is unchanged; only the recorded block size
    /// moves (shrinking frees the tail blocks, `freeFrom` in Zig).
    pub fn realloc(&mut self, r: GcRef, n_bytes: usize, _allow_move: bool) -> Option<GcRef> {
        if n_bytes == 0 {
            self.free(r);
            return None;
        }
        if n_bytes > MAX_ALLOC {
            return None;
        }
        let i = r.0 as usize;
        if i >= self.slots.len() || self.slots[i].payload.is_none() {
            return None;
        }
        let have = self.slots[i].n_blocks;
        let want = blocks_for(n_bytes);
        if want != have {
            let old_bytes = have * BYTES_PER_BLOCK;
            let new_bytes = want * BYTES_PER_BLOCK;
            if new_bytes > old_bytes {
                self.allocated_bytes += new_bytes - old_bytes;
            } else {
                self.allocated_bytes -= old_bytes - new_bytes;
            }
            self.slots[i].n_blocks = want;
        }
        Some(r)
    }

    /// Drain the pending-finaliser list from the last collection, in
    /// slot order. The VM runs each object's `__del__` through its
    /// normal call path, then frees it (`free()`).
    pub fn take_pending_finalisers(&mut self) -> Vec<GcRef> {
        std::mem::take(&mut self.pending_finalisers)
    }

    /// `gc_collect` (`gc.zig:205-212`): locked -> no-op (returns 0);
    /// wasm + live Python frames -> defer (`collect_wanted`, wasm-only,
    /// not applicable on native); otherwise collect.
    ///
    /// Returns the number of objects freed. NOTE: the original's
    /// `gc_collect()` returns `void` and Python `gc.collect()` returns
    /// `0` always (`modgc.zig`); the count is the frozen Rust API
    /// (CPython-like) and must NOT be surfaced as Python `gc.collect()`.
    pub fn collect(&mut self, roots: &RootSet) -> usize {
        if self.lock_depth != 0 {
            return 0;
        }
        self.collect_inner(roots)
    }

    /// The internal `collect()` (`gc.zig:273-285`): mark from the
    /// explicit roots, run the finaliser pass, sweep. No lock check —
    /// callers guarantee it (`collect()` checks; the stress path in
    /// `alloc_inner` checks `auto_collect_enabled && lock_depth == 0`).
    /// No `stack_top == null` early return: Rust roots are always explicit.
    fn collect_inner(&mut self, roots: &RootSet) -> usize {
        self.alloc_count = 0;
        let n = self.slots.len();
        let mut marked = vec![false; n];
        // Mark from the roots (payload children + header type).
        let seeds: Vec<GcRef> = roots
            .iter()
            .filter_map(|v| v.as_heap())
            .collect();
        self.mark_closure(&mut marked, &seeds);
        // Finaliser pass (gc.zig runFinalisers): every unmarked object
        // allocated with a finaliser gets its flag cleared first, is
        // marked live together with its transitive children (so __del__
        // still sees an intact object), and is listed for the VM.
        // Typeless objects (no type yet) are skipped, as in Zig
        // (`if type word == 0 continue`), and freed by the sweep.
        let mut pending = Vec::new();
        for i in 0..n {
            if self.slots[i].payload.is_some()
                && !marked[i]
                && self.slots[i].header.flags & ObjHeader::FLAG_FINALISER != 0
                && self.slots[i].header.type_.is_some()
            {
                self.slots[i].header.flags &= !ObjHeader::FLAG_FINALISER;
                self.mark_closure(&mut marked, &[GcRef(i as u32)]);
                pending.push(GcRef(i as u32));
            }
        }
        self.pending_finalisers.extend(pending);
        // Sweep: unmarked heads and their tails become free, marks
        // become heads (gc.zig collectFrom's sweep).
        let mut freed = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.payload.is_some() && !marked[i] {
                self.allocated_bytes -= slot.n_blocks * BYTES_PER_BLOCK;
                slot.payload = None;
                slot.header.flags = 0;
                self.free.push(i as u32);
                freed += 1;
            }
        }
        freed
    }

    /// Mark `seeds` and everything reachable from them (payload
    /// children via `Obj::children()` plus each object's header
    /// `type_` — the first word of every allocation, which gc.zig's
    /// `scanRange` follows). Iterative worklist, as in `collectFrom`.
    fn mark_closure(&self, marked: &mut [bool], seeds: &[GcRef]) {
        let n = self.slots.len();
        let mut work: Vec<u32> = Vec::new();
        for &r in seeds {
            let i = r.0 as usize;
            if i < n && !marked[i] && self.slots[i].payload.is_some() {
                marked[i] = true;
                work.push(r.0);
            }
        }
        while let Some(i) = work.pop() {
            let slot = &self.slots[i as usize];
            if let Some(obj) = &slot.payload {
                for child in obj.children() {
                    if let Value::Heap(r) = child {
                        let j = r.0 as usize;
                        if j < n && !marked[j] && self.slots[j].payload.is_some() {
                            marked[j] = true;
                            work.push(r.0);
                        }
                    }
                }
            }
            if let Some(t) = slot.header.type_ {
                let j = t.0 as usize;
                if j < n && !marked[j] && self.slots[j].payload.is_some() {
                    marked[j] = true;
                    work.push(t.0);
                }
            }
        }
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
#[path = "gc_tests.rs"]
mod tests;
