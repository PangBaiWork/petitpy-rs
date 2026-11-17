//! Runtime-track qstr (interned-string) subsystem — pure Rust.
//!
//! Port of `src/runtime/qstr.zig` (which replaces vendor `py/qstr.c`).
//! The runtime track shares the static table with the compiler track
//! (A1) through `crate::qstr_table`: one table, one numbering
//! (N_STATIC 228, N_MAIN 385, N_TOTAL 613), so a qstr id means the
//! same string on either side. Nothing here re-parses
//! `qstrdefs.generated.h` — the table is generated once by `build.rs`
//! and shared.
//!
//! Divergences from the Zig original (same class as A1's, kept
//! consistent with the compiler track where the two files agree):
//!
//!   1. Dynamic-pool state lives in a `RuntimeQstr` struct, NOT
//!      file-scope globals — same as A1's `QstrContext`. The Zig
//!      file keeps `dynamic_entries` / `dynamic_chunks` / `chunk_used`
//!      as file-scope globals with an `interp_state` tuple of
//!      pointers for the embedding API to save/load ("each interpreter
//!      interns its own"); the struct IS that state in Rust — owned,
//!      movable, one per interpreter — so no separate `interp_state`
//!      constant is needed.
//!   2. Dynamic entries are owned `Vec<u8>` values, not pointers into
//!      128-byte chunk slabs (`dynAlloc` / `CHUNK_INIT`): the Zig
//!      chunk layout is an allocation detail with no observable
//!      behavior — ids, bytes, lengths and hashes are identical —
//!      and Rust ownership makes the chunk bookkeeping unnecessary.
//!      Dynamic bytes are stored WITHOUT the trailing NUL the Zig
//!      side keeps for C-interop; `qstr_str` / `qstr_data` return
//!      `&[u8]` instead of `[:0]` / raw pointers.
//!
//!      C15b: dynamic bytes are LEAKED (`Box::leak`) and the entry
//!      table is a `RefCell<Vec<…>>`, so `from_strn` takes `&self`
//!      instead of `&mut self`. The Zig interns freely from anywhere
//!      (`qstr_from_strn` grows the global pool); the Rust pool is
//!      shared through `CallCtx`, so the dispatch path (imports,
//!      `eval`) could not intern at all — every imported module
//!      defines new names, and the read-only fallback failed them
//!      with `MemoryError`. The leak is sound: each entry's bytes
//!      live in their own heap allocation, so pushing to the entry
//!      table never moves an already-returned slice (the table holds
//!      only `&'static` pointers); `qstr_str` copies the `&'static`
//!      reference out from under a short-lived shared borrow.
//!      The leak is bounded by the number of distinct dynamic names
//!      (≤255 bytes each); `reset()` drops the entries but the
//!      leaked buffers stay allocated, as the Zig's chunks do until
//!      `qstr_init`. `RuntimeQstr` is therefore `!Sync` (a
//!      `RefCell`); the VM is single-threaded, so nothing requires
//!      `Sync`.
//!   3. `qstr` is aliased to `u16` (not vendor's `usize`) — same as
//!      A1; the qstr id range fits comfortably.
//!   4. `from_strn` returns `PyResult<Qstr>` (`Err(OutOfMemory)` on a
//!      >255-byte name) instead of the Zig `@panic("qstr_from_strn:
//!      name too long")` — same convention as A1's compiler track.
//!      Out-of-range dynamic ids still panic in the accessors
//!      (`qstr_str` / `qstr_len` / `qstr_hash` / `qstr_data`),
//!      mirroring the Zig `@panic` in the same places.
//!   5. The comptime QDEF parser (`ParsedDefs` / `parseLine` /
//!      `decodeCString` / `StaticStrings` / `StaticHashes` /
//!      `StaticLengths`) is not re-implemented: the shared table in
//!      `crate::qstr_table` already is that parse, done once by
//!      `build.rs`.
//!   6. `qstr_data` returns `&[u8]`; the length the Zig caller reads
//!      through `len_out` is the slice's `.len()`.
//!
//! Stale numbers in the Zig file's header comments (N_MAIN=216,
//! N_TOTAL=444) are NOT reproduced: the vendored header actually
//! carries 385 main entries (613 total), proved by Lane M's Zig probe
//! and by the build-time assertions in `crate::qstr_table`.
//!
//! Not ported (declaration-only, no behavior): the trailing
//! `qstr_dynamic` struct (`extern var dynamic_entries: anyopaque`)
//! — the comment above it says the qstr_total override it belonged
//! to was removed.

use crate::compiler::mpy::QstrEntry;
use crate::pyerr::{PyError, PyResult};
use crate::qstr_table::{Qstr, QSTR_HASHES, QSTR_LENGTHS, QSTR_STRINGS};
use std::cell::RefCell;

/// Re-exported so `runtime::qstr::*` is a complete mirror of the Zig
/// file's public surface (the Zig file defines these itself).
pub use crate::qstr_table::{qstr_compute_hash, N_MAIN, N_STATIC, N_TOTAL};

/// One dynamic entry: the interned bytes (a leaked heap
/// allocation — see the module docs), its hash and length.
struct DynamicEntry {
    bytes: &'static [u8],
    hash: u16,
    len: u8,
}

/// Interned-string state for one interpreter: the dynamic qstr pool.
///
/// Static-pool data (`QSTR_STRINGS` etc.) is shared and immutable;
/// only the dynamic entries live here. One `RuntimeQstr` = one
/// interpreter's interning state (the Zig `interp_state`).
///
/// `reset()` is the Zig `qstr_init` (safe to call multiple times:
/// it drops all dynamic state and renumbers fresh ids from N_TOTAL).
pub struct RuntimeQstr {
    entries: RefCell<Vec<DynamicEntry>>,
}

impl RuntimeQstr {
    pub fn new() -> Self {
        RuntimeQstr {
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Drop all dynamic state but keep the state usable. Newly
    /// interned qstrs will re-start numbering at N_TOTAL.
    /// (Zig: `qstr_init` — frees the chunks and clears the entries.)
    pub fn reset(&mut self) {
        self.entries.get_mut().clear();
    }

    /// Number of dynamic entries currently interned.
    pub fn dynamic_len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Total qstr count (static + main + dynamic). Mirrors vendor's
    /// `QSTR_TOTAL()` macro for callers (e.g. builtin_dir) that walk
    /// all qstrs. (Zig: `npy_qstr_total`.)
    pub fn total(&self) -> usize {
        N_TOTAL as usize + self.entries.borrow().len()
    }

    /// Search all pools for a string. Returns 0 (MP_QSTRnull) on miss.
    /// Special case: empty input returns 1 (MP_QSTR_), matching vendor.
    ///
    /// Search order: dynamic (newest first, so LIFO dedup), then the
    /// static + main pools, linear. The dynamic-first order matches
    /// the Zig file ("newer wins on collisions") and vendor's
    /// MP_STATE_VM(last_pool) chain walk in qstr.c:262-306.
    /// (Zig: `qstr_find_strn`; hash is checked before length here,
    /// exactly as the runtime Zig file does — the compiler track
    /// checks length first. Both must match for a hit, so the order
    /// is unobservable.)
    pub fn find_strn(&self, s: &[u8]) -> Qstr {
        if s.is_empty() {
            return 1; // MP_QSTR_
        }
        let want_hash = qstr_compute_hash(s);
        if s.len() > 255 {
            return 0; // can't possibly match (we cap at u8)
        }
        let want_len: u8 = s.len() as u8;

        // 1) dynamic pool, newest-first
        let entries = self.entries.borrow();
        for (di, e) in entries.iter().enumerate().rev() {
            if e.hash == want_hash && e.len == want_len && e.bytes == s {
                return (N_TOTAL as usize + di) as Qstr;
            }
        }

        // 2) static + main pools, linear. (The mains pool is sorted
        // in C — the Zig file notes we could bsearch it once profiling
        // demands; for now the linear scan is fine.)
        for (i, bytes) in QSTR_STRINGS.iter().enumerate() {
            if QSTR_HASHES[i] == want_hash && QSTR_LENGTHS[i] == want_len && *bytes == s {
                return i as Qstr;
            }
        }
        0 // MP_QSTRnull
    }

    /// Intern a string. Returns the existing id on hit; allocates a
    /// new dynamic entry on miss. New dynamic ids start at N_TOTAL.
    /// (Zig: `qstr_from_strn` — copies into chunk storage,
    /// NUL-terminates, appends the entry.)
    ///
    /// Takes `&self` (C15b): the pool is shared through `CallCtx`,
    /// and the dispatch path must intern (imports define new names).
    /// The bytes are leaked (see the module docs) so a push can
    /// never invalidate a slice `qstr_str` already returned.
    pub fn from_strn(&self, s: &[u8]) -> PyResult<Qstr> {
        let found = self.find_strn(s);
        if found != 0 {
            return Ok(found);
        }
        if s.len() > 255 {
            return Err(PyError::OutOfMemory); // u8 length limit on entries
        }

        // Reserve the slot before leaking, so a failed reserve
        // leaks nothing.
        {
            let mut entries = self.entries.borrow_mut();
            entries
                .try_reserve(1)
                .map_err(|_| PyError::OutOfMemory)?;
        }
        let boxed: Box<[u8]> = s.into();
        let bytes: &'static [u8] = Box::leak(boxed);
        let mut entries = self.entries.borrow_mut();
        entries.push(DynamicEntry {
            hash: qstr_compute_hash(s),
            len: s.len() as u8,
            bytes,
        });
        Ok((N_TOTAL as usize + entries.len() - 1) as Qstr)
    }

    /// Convenience wrapper for `&str` input. (Zig: `qstr_from_str`,
    /// which takes a NUL-terminated C string.)
    pub fn from_str(&self, s: &str) -> PyResult<Qstr> {
        self.from_strn(s.as_bytes())
    }

    /// Return the bytes for `id`. Length is `qstr_len(id)`.
    ///
    /// Panics on an out-of-range dynamic id, mirroring the Zig
    /// `@panic("qstr_str: out-of-range qstr")` in the same place.
    pub fn qstr_str(&self, id: Qstr) -> &[u8] {
        if id < N_TOTAL {
            return QSTR_STRINGS[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        let entries = self.entries.borrow();
        assert!(idx < entries.len(), "qstr_str: out-of-range qstr");
        // Copy the `&'static` reference out from under the shared
        // borrow (the guard drops here; the bytes live on — see
        // the module docs), then shorten it to `&self`.
        let bytes: &'static [u8] = entries[idx].bytes;
        bytes
    }

    pub fn qstr_len(&self, id: Qstr) -> u8 {
        if id < N_TOTAL {
            return QSTR_LENGTHS[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        let entries = self.entries.borrow();
        assert!(idx < entries.len(), "qstr_len: out-of-range qstr");
        entries[idx].len
    }

    pub fn qstr_hash(&self, id: Qstr) -> u16 {
        if id < N_TOTAL {
            return QSTR_HASHES[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        let entries = self.entries.borrow();
        assert!(idx < entries.len(), "qstr_hash: out-of-range qstr");
        entries[idx].hash
    }

    /// Return the bytes for `id` (length is the slice's `.len()`).
    /// (Zig: `qstr_data` — returns the pointer and writes the length
    /// through `len_out`.)
    pub fn qstr_data(&self, id: Qstr) -> &[u8] {
        self.qstr_str(id)
    }
}

impl Default for RuntimeQstr {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a module's `.mpy` qstr table to ids: static entries
/// pass through, `New` bytes are interned into the pool.
/// Takes `&RuntimeQstr` (C15b): `from_strn` interns through
/// `&self` now, so the dispatch path resolves tables with the
/// shared `CallCtx.pool` — no `&mut self.pool` split dance.
pub fn resolve_qstr_table(
    pool: &RuntimeQstr,
    qstrs: &[QstrEntry],
) -> PyResult<Vec<u16>> {
    let mut table = Vec::with_capacity(qstrs.len());
    for q in qstrs {
        match q {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => table.push(pool.from_strn(bytes)? as u16),
        }
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Static table, seen from the runtime track ─────────────────────
    // (The Zig runtime file has 0 test blocks; these pin the shared
    // table as the runtime side sees it.)

    #[test]
    fn static_table_fingerprint_matches_vendor() {
        assert_eq!(228u16, N_STATIC);
        assert_eq!(385u16, N_MAIN);
        assert_eq!(613u16, N_TOTAL);
    }

    #[test]
    fn find_strn_static_ids_match_compiler_track() {
        // Same ids the compiler track pins (A1): the two tracks must
        // agree on every static id.
        let rt = RuntimeQstr::new();
        assert_eq!(1 as Qstr, rt.find_strn(b""));
        assert_eq!(0 as Qstr, rt.find_strn(b"__no_such_qstr_xyzzy__"));
        assert_eq!(17 as Qstr, rt.find_strn(b"__init__"));
        assert_eq!(21 as Qstr, rt.find_strn(b"__main__"));
        assert_eq!(123 as Qstr, rt.find_strn(b"print"));
        assert_eq!(165 as Qstr, rt.find_strn(b"zip"));
        assert_eq!(612 as Qstr, rt.find_strn(b"{:#b}"));
    }

    #[test]
    fn djb2_matches_vendor_precomputed_hashes() {
        let rt = RuntimeQstr::new();
        assert_eq!(QSTR_HASHES[17], qstr_compute_hash(b"__init__"));
        assert_eq!(QSTR_HASHES[123], qstr_compute_hash(b"print"));
        assert_eq!(QSTR_HASHES[165], qstr_compute_hash(b"zip"));
        assert_eq!(QSTR_HASHES[123], rt.qstr_hash(123));
    }

    // ── Dynamic interning ─────────────────────────────────────────────

    #[test]
    fn from_strn_dynamic_ids_start_at_n_total_and_dedup() {
        let rt = RuntimeQstr::new();
        let a = rt.from_strn(b"my_new_identifier").unwrap();
        assert_eq!(N_TOTAL, a);

        // Second call: same id, no new entry.
        let a2 = rt.from_strn(b"my_new_identifier").unwrap();
        assert_eq!(a, a2);
        assert_eq!(1, rt.dynamic_len());

        // Different string: next id.
        let b = rt.from_strn(b"another_one").unwrap();
        assert_eq!(N_TOTAL + 1, b);
        assert_eq!(2, rt.dynamic_len());

        // All dynamic ids are >= 613.
        assert!(a >= N_TOTAL && b >= N_TOTAL);
    }

    #[test]
    fn find_strn_finds_dynamic_entries() {
        let rt = RuntimeQstr::new();
        let id = rt.from_strn(b"runtime_only_name").unwrap();
        assert_eq!(id, rt.find_strn(b"runtime_only_name"));
        // A fresh state does not know it.
        let rt2 = RuntimeQstr::new();
        assert_eq!(0 as Qstr, rt2.find_strn(b"runtime_only_name"));
    }

    #[test]
    fn dynamic_accessors_round_trip() {
        let rt = RuntimeQstr::new();
        let id = rt.from_strn(b"hello_world").unwrap();
        assert_eq!(11u8, rt.qstr_len(id));
        assert_eq!(b"hello_world", rt.qstr_str(id));
        assert_eq!(b"hello_world", rt.qstr_data(id));
        assert_eq!(11, rt.qstr_data(id).len());
        assert_eq!(qstr_compute_hash(b"hello_world"), rt.qstr_hash(id));
    }

    #[test]
    fn static_strings_never_create_dynamic_entries() {
        let rt = RuntimeQstr::new();
        assert_eq!(Ok(17 as Qstr), rt.from_strn(b"__init__"));
        assert_eq!(Ok(123 as Qstr), rt.from_strn(b"print"));
        assert_eq!(0, rt.dynamic_len());
        assert_eq!(613, rt.total());
    }

    #[test]
    fn total_counts_static_plus_dynamic() {
        let rt = RuntimeQstr::new();
        assert_eq!(613, rt.total());
        rt.from_strn(b"one").unwrap();
        rt.from_strn(b"two").unwrap();
        assert_eq!(615, rt.total());
        assert_eq!(2, rt.dynamic_len());
    }

    #[test]
    fn reset_renumbers_from_n_total() {
        let mut rt = RuntimeQstr::new();
        rt.from_strn(b"first").unwrap();
        rt.from_strn(b"second").unwrap();
        assert_eq!(2, rt.dynamic_len());

        rt.reset();
        assert_eq!(0, rt.dynamic_len());
        assert_eq!(613, rt.total());

        let id = rt.from_strn(b"third").unwrap();
        assert_eq!(N_TOTAL, id);
    }

    #[test]
    fn long_string_255_ok_256_rejected() {
        let rt = RuntimeQstr::new();
        let buf255 = [b'x'; 255];
        let id = rt.from_strn(&buf255).unwrap();
        assert_eq!(255u8, rt.qstr_len(id));
        assert_eq!(0 as Qstr, rt.find_strn(&[b'y'; 256]));

        let buf256 = [b'y'; 256];
        assert_eq!(Err(PyError::OutOfMemory), rt.from_strn(&buf256));
    }

    #[test]
    #[should_panic(expected = "out-of-range qstr")]
    fn qstr_str_out_of_range_panics() {
        let rt = RuntimeQstr::new();
        let _ = rt.qstr_str(N_TOTAL);
    }

    #[test]
    fn full_static_pool_self_consistent_from_runtime() {
        // Same sweep as the compiler track's full-pool test, through
        // the runtime accessors: every static entry's length and hash
        // match its bytes, `find_strn` returns its own id, and
        // re-interning returns the same id without creating a dynamic
        // entry. (Id 0, MP_QSTRnull, is exempt from the hash check:
        // the vendor table records hash 0 for it by definition.)
        let rt = RuntimeQstr::new();
        for id in 0..N_TOTAL {
            // Copy out: the borrow of `rt` cannot be held across the
            // `&mut self` calls below.
            let bytes = rt.qstr_str(id).to_vec();
            assert_eq!(bytes.len() as u8, rt.qstr_len(id));
            if id != 0 {
                assert_eq!(QSTR_HASHES[id as usize], rt.qstr_hash(id));
            }
            if !bytes.is_empty() {
                assert_eq!(id, rt.find_strn(&bytes));
                assert_eq!(Ok(id), rt.from_strn(&bytes));
            }
        }
        assert_eq!(0, rt.dynamic_len());
    }
}
