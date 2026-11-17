//! Set hash table (C9a: `src/objects/containers/map.zig`, set part).
//!
//! Split out of `map.rs` (C9a-split: the 800-line cap). `MpSet` /
//! `set_lookup` are the nano-style open-addressing table operations
//! for the set type (the set TYPE itself is C9c's). The map half
//! (`MpMap` lookup / rehash / cache) stays in `map.rs`; the set half
//! hashes through the same `hash_of` and compares through the same
//! `obj_equal` as the map half.

use crate::memory::gc::CallCtx;
use crate::objects::obj::BuiltinTypes;
use crate::objects::obj_ops::obj_equal;
use crate::objects::value::Value;
use crate::objects::containers::map::{get_hash_alloc_geq, hash_of, LookupKind, MapError};

// ── `mp_set_*` (set underlying hash table) ──────────────────────────

/// One slot of an `MpSet` table: empty (NULL), tombstone (SENTINEL),
/// or a live element. The Zig stores bare `Obj` words with NULL /
/// SENTINEL markers; Rust needs the third state explicit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetSlot {
    /// NULL — an empty slot (search-terminating).
    Empty,
    /// SENTINEL — a deleted slot (probe steps over it).
    Tombstone,
    /// A live element.
    Live(Value),
}

/// A set's hash table (`mp_set_t`, map.zig: `alloc`, `used`,
/// `table`). Nano-style open addressing in both configs (unlike the
/// compact dict layout). The set TYPE (`Obj::Set`) is C9c's — this
/// is only the table operations.
#[derive(Clone, Debug)]
pub struct MpSet {
    pub alloc: usize,
    pub used: usize,
    pub table: Vec<SetSlot>,
}

impl MpSet {
    /// `mp_set_init(set, n)`.
    pub fn new(n: usize) -> MpSet {
        MpSet {
            alloc: n,
            used: 0,
            table: vec![SetSlot::Empty; n],
        }
    }

    /// `mp_set_clear`: drop the table, reset to empty.
    pub fn clear(&mut self) {
        self.table.clear();
        self.table.shrink_to_fit();
        self.alloc = 0;
        self.used = 0;
    }

    /// `mp_set_remove_first`: the first live element in TABLE order
    /// (not insertion order), tombstoned/removed; `None` when empty.
    pub fn remove_first(&mut self) -> Option<Value> {
        let mut pos = 0;
        while pos < self.alloc {
            if let SetSlot::Live(v) = self.table[pos] {
                self.used -= 1;
                // If the next slot is empty, set this one to NULL
                // (search-terminating); else SENTINEL (keep the chain).
                let next = self.table[(pos + 1) % self.alloc];
                self.table[pos] = if next == SetSlot::Empty {
                    SetSlot::Empty
                } else {
                    SetSlot::Tombstone
                };
                return Some(v);
            }
            pos += 1;
        }
        None
    }
}

/// `setRehash` (map.zig): grow via `getHashAllocGeq(alloc + 1)` and
/// replay the live elements.
fn set_rehash(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
) -> Result<(), MapError> {
    let old_table = std::mem::take(&mut set.table);
    let old_alloc = set.alloc;
    set.alloc = get_hash_alloc_geq(set.alloc + 1);
    set.used = 0;
    set.table = vec![SetSlot::Empty; set.alloc];
    for slot in old_table.into_iter().take(old_alloc) {
        if let SetSlot::Live(k) = slot {
            let _ = set_lookup(ctx, types, set, k, LookupKind::Add)?;
        }
    }
    Ok(())
}

/// `mp_set_lookup` (map.zig): find (or add / remove) `index` in the
/// set table. Returns the found/added element, or `None` when absent
/// (LOOKUP/REMOVE miss). `lookup_kind` is bit-tested (ADD=1,
/// REMOVE=2 — the combined kind 3 is `AddOrRemove`). Unlike the map
/// lookup there is no cache and no qstr fast path: the hash always
/// goes through `mp_unary_op` HASH (here `hash_of`, same values)
/// and equality is always `mp_obj_equal`.
pub fn set_lookup(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    set: &mut MpSet,
    index: Value,
    lookup_kind: LookupKind,
) -> Result<Option<Value>, MapError> {
    let kind = lookup_kind as i32;
    let want_add = (kind & LookupKind::Add as i32) != 0;
    let want_remove = (kind & LookupKind::Remove as i32) != 0;
    if set.alloc == 0 {
        if want_add {
            set_rehash(ctx, types, set)?;
        } else {
            return Ok(None);
        }
    }
    // The hash is computed once, before any mutation (it can raise).
    // NOTE: the Zig hashes here with `mp_unary_op` directly (no qstr
    // fast path in `mp_set_lookup`) — `hash_of` applies the same
    // `qstr_hash` for qstrs, so the values agree.
    let hash = hash_of(ctx, types, index)?;
    let mut pos = hash % set.alloc;
    let mut start_pos = pos;
    let mut avail_slot: Option<usize> = None;
    loop {
        match set.table[pos] {
            SetSlot::Empty => {
                // Found a NULL slot — index is not in the table.
                if want_add {
                    let dest = avail_slot.unwrap_or(pos);
                    set.used += 1;
                    set.table[dest] = SetSlot::Live(index);
                    return Ok(Some(index));
                }
                return Ok(None);
            }
            SetSlot::Tombstone => {
                if avail_slot.is_none() {
                    avail_slot = Some(pos);
                }
            }
            SetSlot::Live(elem) => {
                if obj_equal(ctx, types, elem, index).map_err(MapError::from)? {
                    if want_remove {
                        set.used -= 1;
                        let next = set.table[(pos + 1) % set.alloc];
                        set.table[pos] = if next == SetSlot::Empty {
                            SetSlot::Empty
                        } else {
                            SetSlot::Tombstone
                        };
                    }
                    return Ok(Some(elem));
                }
            }
        }
        pos = (pos + 1) % set.alloc;
        if pos == start_pos {
            // Search wrapped — index not in the table.
            if want_add {
                if let Some(s) = avail_slot {
                    set.used += 1;
                    set.table[s] = SetSlot::Live(index);
                    return Ok(Some(index));
                }
                // Not enough room — rehash and restart.
                set_rehash(ctx, types, set)?;
                pos = hash % set.alloc;
                start_pos = pos;
                avail_slot = None;
            } else {
                return Ok(None);
            }
        }
    }
}
