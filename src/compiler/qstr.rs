//! Compiler-track qstr (interned-string) subsystem — pure Rust.
//!
//! Port of `src/compiler/qstr.zig` (which replaces vendor `py/qstr.c`
//! plus the runtime-coupled `src/runtime/qstr.zig`). Key divergences
//! from the Zig original, mirroring the divergences the Zig file itself
//! documents against vendor:
//!
//!   1. Dynamic-pool state lives in a `QstrContext` struct, NOT
//!      file-scope globals — same as Zig. The compiler driver may be
//!      invoked multiple times per process (REPL, test harness) and
//!      global mutable state would leak ids across compile sessions
//!      and break byte-identical .mpy emission on the second run.
//!   2. `qstr` is aliased to `u16` (not vendor's `usize`) — same as
//!      Zig; the qstr id range fits comfortably and the .mpy vlen
//!      encoder/decoder already uses u16 widths.
//!   3. Length check before hash check in `find_strn` — same as Zig
//!      (cheap u8 compare disambiguates faster than hash on the
//!      common miss).
//!   4. Dynamic entries are owned `Vec<u8>` values, not pointers into
//!      allocator chunks: the Zig chunk layout (128-byte slabs) is an
//!      allocation detail with no observable behavior — ids, bytes,
//!      lengths and hashes are identical — and Rust ownership makes
//!      the chunk bookkeeping unnecessary. Dynamic bytes are stored
//!      WITHOUT the trailing NUL the Zig side keeps for C-interop;
//!      `qstr_str` returns `&[u8]` instead of a `[:0]` slice.
//!   5. The comptime QDEF parser is replaced by `build.rs`, which
//!      parses the same vendored header (`vendor/qstrdefs.generated.h`)
//!      at build time and emits `qstr_table.rs` into `OUT_DIR`
//!      (see `build.rs`). The table below is therefore derived, never
//!      hand-copied. Compile-time assertions here keep `N_STATIC` /
//!      `N_MAIN` in lockstep with the generated counts, mirroring the
//!      Zig `comptime` fingerprint block.
//!   6. Zig `Allocator.Error!T` becomes `PyResult<T>` (`Result<T,
//!      PyError>`); allocation failure is reported via `try_reserve`
//!      instead of aborting, so `from_strn` can return
//!      `PyError::OutOfMemory` exactly as the Zig side does.

use crate::pyerr::{PyError, PyResult};

// ── Vendor constants ──────────────────────────────────────────────────────
//
// N_STATIC and N_MAIN come from the QDEF0/QDEF1 partition in
// vendor/qstrdefs.generated.h. They are asserted at compile time below
// against the build.rs-generated counts — a vendor regen that changes
// either count will fail the build with a clear message.

/// A qstr id.
pub type Qstr = u16;

/// Number of QDEF0 entries — the "static" pool (compile-time, unsorted).
/// Ids 0..N_STATIC-1.
pub const N_STATIC: u16 = 228;

/// Number of QDEF1 entries — the "main" pool (compile-time, sorted).
/// Ids N_STATIC..N_STATIC+N_MAIN-1.
///
/// ADR-006 J1 — bumped from 216 to 227 (+11) to absorb the qstrs the
/// `_petitpy_js` module shadow declares (`_petitpy_js`,
/// `_bridge_present`, `_get_global`, `_get_attr`, `_set_attr`, `_call`,
/// `_release`, `_create_string`, `_create_int`, `_to_string`, `_to_int`).
/// J5a — bumped from 227 to 229 (+2) for `_create_callback` +
/// `_set_dispatcher` (Python callback marshaling).
/// These land in the QDEF1 table whether or not -Dfeature-js is on
/// because the qstr scanner reads the shadow .c unconditionally; the
/// runtime gating only affects whether the module object itself is
/// linked, not the qstr table.
/// ADR 019 — 357 (+1) for `__del__` (finalisers, memory/gc.zig).
/// 358 (+1) for `os_path` (`os.path` via os.__getattr__, modules/modos.zig).
/// 359 (+1) for `gcd` (math.gcd, modules/modmath.zig).
/// 362 (+3) for `__cause__`, `__context__`, `__suppress_context__` (PEP 3134).
/// 366 (+4) for `lseek`, `SEEK_SET`, `SEEK_CUR`, `SEEK_END` (modules/modos.zig).
/// 367 (+1) for `_json` (modules/modjson.zig).
pub const N_MAIN: u16 = 385;

/// Total compile-time qstr count. Dynamic pool ids start here.
pub const N_TOTAL: u16 = N_STATIC + N_MAIN;

/// Boundary used by .mpy persistentcode encoding (vendor
/// persistentcode.c:48 `#define QSTR_LAST_STATIC MP_QSTR_zip`). Qstrs
/// with id <= QSTR_LAST_STATIC are encoded inline as `.static`
/// references; everything else is emitted as `.new` (inline bytes).
///
/// 165 == zero-based index of MP_QSTR_zip among QDEF0 entries (verified
/// against genhdr/qstrdefs.generated.h line 168 / QDEF0 #166).
pub const QSTR_LAST_STATIC: u16 = 165;

// ── Static-pool data (build-time generated) ───────────────────────────────
//
// Layout (mirrors the Zig StaticStrings/Hashes/Lengths):
//   ids 0..N_STATIC-1        -> QDEF0 entries, in file order
//   ids N_STATIC..N_TOTAL-1  -> QDEF1 entries, in file order
include!(concat!(env!("OUT_DIR"), "/qstr_table.rs"));

// Compile-time consistency check: keep these constants in lockstep with
// the generated table (mirrors the Zig `comptime` fingerprint block).
const _: () = assert!(GEN_N_STATIC == N_STATIC as usize, "N_STATIC drift");
const _: () = assert!(GEN_N_MAIN == N_MAIN as usize, "N_MAIN drift");
const _: () = assert!(QSTR_STRINGS.len() == N_TOTAL as usize);
const _: () = assert!(QSTR_HASHES.len() == N_TOTAL as usize);
const _: () = assert!(QSTR_LENGTHS.len() == N_TOTAL as usize);

// ── Hash function ─────────────────────────────────────────────────────────
//
// djb2 — must match vendor qstr.c:64-76 byte-for-byte so that
// vendor-precomputed hashes in QSTR_HASHES line up with runtime hashes
// for the same string. Result is masked to 16 bits and bumped to 1 if
// it would otherwise be 0 (vendor uses 0 as "hash not computed").

pub fn qstr_compute_hash(data: &[u8]) -> u16 {
    let mut hash: u32 = 5381;
    for &b in data {
        hash = (hash.wrapping_shl(5).wrapping_add(hash)) ^ b as u32; // hash*33 ^ byte
    }
    hash &= 0xffff;
    if hash == 0 {
        hash = 1;
    }
    hash as u16
}

// ── Dynamic pool: QstrContext ─────────────────────────────────────────────
//
// One context = one compile session. The driver creates it with
// `QstrContext::new()`, calls `from_str(...)` during parsing, then
// drops it at the end. `reset()` lets a test harness clear the
// dynamic side without re-allocating the context itself.

struct DynamicEntry {
    bytes: Vec<u8>,
    hash: u16,
    len: u8,
}

/// Interned-string context: the dynamic qstr pool for one compile session.
///
/// Static-pool data (`QSTR_STRINGS` etc.) is shared and immutable;
/// only the dynamic entries live here.
pub struct QstrContext {
    entries: Vec<DynamicEntry>,
}

impl QstrContext {
    pub fn new() -> Self {
        QstrContext { entries: Vec::new() }
    }

    /// Drop all dynamic state but keep the context usable. Newly
    /// interned qstrs will re-start numbering at N_TOTAL.
    pub fn reset(&mut self) {
        self.entries.clear();
    }

    /// Number of dynamic entries currently interned.
    pub fn dynamic_len(&self) -> usize {
        self.entries.len()
    }

    /// Search all pools for a string. Returns 0 (MP_QSTRnull) on miss.
    /// Special case: empty input returns 1 (MP_QSTR_), matching vendor.
    ///
    /// Search order: dynamic (newest first, so LIFO dedup), then static,
    /// then main. The dynamic-first order matches vendor's
    /// MP_STATE_VM(last_pool) chain walk in qstr.c:262-306.
    pub fn find_strn(&self, s: &[u8]) -> Qstr {
        if s.is_empty() {
            return 1; // MP_QSTR_
        }
        if s.len() > 255 {
            return 0; // u8 length cap — can't be in any pool
        }
        let want_len: u8 = s.len() as u8;
        let want_hash = qstr_compute_hash(s);

        // 1) dynamic pool, newest-first
        for (di, e) in self.entries.iter().enumerate().rev() {
            // Length check first: cheaper than hash, disambiguates the common miss.
            if e.len == want_len && e.hash == want_hash && e.bytes == s {
                return (N_TOTAL as usize + di) as Qstr;
            }
        }

        // 2 + 3) static + main pools, linear. (vendor bsearches the sorted
        // main pool; not worth the complexity until profiling demands.)
        for (i, bytes) in QSTR_STRINGS.iter().enumerate() {
            if QSTR_LENGTHS[i] == want_len && QSTR_HASHES[i] == want_hash && *bytes == s {
                return i as Qstr;
            }
        }
        0 // MP_QSTRnull
    }

    /// Intern a string. Returns the existing id on hit; allocates a new
    /// dynamic entry on miss. New dynamic ids start at N_TOTAL.
    pub fn from_strn(&mut self, s: &[u8]) -> PyResult<Qstr> {
        let found = self.find_strn(s);
        if found != 0 {
            return Ok(found);
        }
        if s.len() > 255 {
            return Err(PyError::OutOfMemory); // u8 length limit on entries
        }

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(s.len())
            .map_err(|_| PyError::OutOfMemory)?;
        bytes.extend_from_slice(s);
        self.entries
            .try_reserve(1)
            .map_err(|_| PyError::OutOfMemory)?;
        self.entries.push(DynamicEntry {
            hash: qstr_compute_hash(s),
            len: s.len() as u8,
            bytes,
        });
        Ok((N_TOTAL as usize + self.entries.len() - 1) as Qstr)
    }

    /// Convenience wrapper for `&str` input.
    pub fn from_str(&mut self, s: &str) -> PyResult<Qstr> {
        self.from_strn(s.as_bytes())
    }

    /// Return the bytes for `id`. Length is `qstr_len(id)`.
    ///
    /// Panics on an out-of-range dynamic id, mirroring the Zig
    /// `debug.assert` in the same place.
    pub fn qstr_str(&self, id: Qstr) -> &[u8] {
        if id < N_TOTAL {
            return QSTR_STRINGS[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        assert!(idx < self.entries.len(), "dynamic qstr id out of range");
        &self.entries[idx].bytes
    }

    pub fn qstr_len(&self, id: Qstr) -> u8 {
        if id < N_TOTAL {
            return QSTR_LENGTHS[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        assert!(idx < self.entries.len(), "dynamic qstr id out of range");
        self.entries[idx].len
    }

    pub fn qstr_hash(&self, id: Qstr) -> u16 {
        if id < N_TOTAL {
            return QSTR_HASHES[id as usize];
        }
        let idx = (id - N_TOTAL) as usize;
        assert!(idx < self.entries.len(), "dynamic qstr id out of range");
        self.entries[idx].hash
    }

    /// Boundary helper for the .mpy emitter (moves to `mpy.rs` when that
    /// module is ported; defined here because the Zig original defines
    /// the conversion in qstr.zig).
    ///
    /// Encoding rule (vendor persistentcode.c:574): ids
    /// <= QSTR_LAST_STATIC are emitted as `.static` (cheap vlen
    /// reference); everything else is `.new` (inline bytes). For
    /// dynamic ids the bytes live in the context, so the returned
    /// slice borrows from this context.
    pub fn to_entry(&self, id: Qstr) -> QstrEntry<'_> {
        if id <= QSTR_LAST_STATIC {
            return QstrEntry::Static(id);
        }
        if id < N_TOTAL {
            return QstrEntry::New(QSTR_STRINGS[id as usize]);
        }
        let idx = (id - N_TOTAL) as usize;
        assert!(idx < self.entries.len(), "dynamic qstr id out of range");
        QstrEntry::New(&self.entries[idx].bytes)
    }
}

impl Default for QstrContext {
    fn default() -> Self {
        Self::new()
    }
}

/// One entry of a .mpy file's qstr table (mirrors `mpy.QstrEntry`).
///
/// The enum lives here until `mpy.rs` is ported (Phase B); the Zig
/// original likewise defines the conversion in qstr.zig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QstrEntry<'a> {
    /// Cheap vlen reference to a compile-time qstr id.
    Static(Qstr),
    /// Inline bytes (compile-time qstr above the static cutoff, or a
    /// dynamic qstr).
    New(&'a [u8]),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Ported verbatim from the `test` blocks in qstr.zig ──────────────
    // (Test names are the Zig test names, snake_cased.)

    #[test]
    fn comptime_pool_fingerprint_matches_vendor() {
        assert_eq!(228u16, N_STATIC);
        assert_eq!(385u16, N_MAIN);
        assert_eq!(613u16, N_TOTAL);
    }

    #[test]
    fn well_known_qstr_ids_match_vendor() {
        // These ids are pinned to the vendor qstrdefs.generated.h commit
        // currently checked in. Any drift here means the vendor was bumped
        // without updating consumers — surface it loudly.
        // (The Zig original pins 7 ids; this port pins 26, including the
        // first/last entries and several module names, per the A1 brief.)
        let cases: &[(u16, &[u8])] = &[
            (0, b""),
            (1, b""),
            (17, b"__init__"),
            (21, b"__main__"),
            (23, b"__name__"),
            (63, b"builtins"),
            (75, b"dict"),
            (94, b"int"),
            (108, b"list"),
            (115, b"micropython"),
            (123, b"print"),
            (151, b"str"),
            (165, b"zip"),
            (334, b"_json"),
            (355, b"array"),
            (376, b"collections"),
            (427, b"gc"),
            (462, b"io"),
            (489, b"math"),
            (511, b"os"),
            (539, b"re"),
            (564, b"socket"),
            (584, b"struct"),
            (589, b"sys"),
            (596, b"time"),
            (612, b"{:#b}"),
        ];
        assert_eq!(26, cases.len());
        for &(id, want) in cases {
            assert_eq!(want, QSTR_STRINGS[id as usize]);
            assert_eq!(want.len() as u8, QSTR_LENGTHS[id as usize]);
        }
        assert_eq!(165u16, QSTR_LAST_STATIC);
    }

    #[test]
    fn djb2_runtime_hashes_match_vendor_precomputed() {
        // Three known vectors. If djb2 drifts, find_strn would miss
        // every static lookup — easier to catch here.
        assert_eq!(QSTR_HASHES[17], qstr_compute_hash(b"__init__"));
        assert_eq!(QSTR_HASHES[123], qstr_compute_hash(b"print"));
        assert_eq!(QSTR_HASHES[QSTR_LAST_STATIC as usize], qstr_compute_hash(b"zip"));
    }

    #[test]
    fn find_strn_empty_miss_static_hit() {
        let ctx = QstrContext::new();
        assert_eq!(1 as Qstr, ctx.find_strn(b""));
        assert_eq!(0 as Qstr, ctx.find_strn(b"__no_such_qstr_xyzzy__"));
        assert_eq!(17 as Qstr, ctx.find_strn(b"__init__"));
        assert_eq!(QSTR_LAST_STATIC, ctx.find_strn(b"zip"));
    }

    #[test]
    fn from_strn_new_entry_gets_n_total_and_dedups() {
        let mut ctx = QstrContext::new();
        let a = ctx.from_strn(b"my_new_identifier").unwrap();
        assert_eq!(N_TOTAL, a);

        // Second call: same id, no new entry.
        let a2 = ctx.from_strn(b"my_new_identifier").unwrap();
        assert_eq!(a, a2);
        assert_eq!(1, ctx.entries.len());

        // Different string: next id.
        let b = ctx.from_strn(b"another_one").unwrap();
        assert_eq!(N_TOTAL + 1, b);
    }

    #[test]
    fn accessors_dynamic_round_trip() {
        let mut ctx = QstrContext::new();
        let id = ctx.from_strn(b"hello_world").unwrap();
        assert_eq!(11u8, ctx.qstr_len(id));
        assert_eq!(b"hello_world", ctx.qstr_str(id));
        assert_eq!(qstr_compute_hash(b"hello_world"), ctx.qstr_hash(id));
    }

    #[test]
    fn reset_dynamic_ids_renumber_from_n_total() {
        let mut ctx = QstrContext::new();
        ctx.from_strn(b"first").unwrap();
        ctx.from_strn(b"second").unwrap();
        assert_eq!(2, ctx.entries.len());

        ctx.reset();
        assert_eq!(0, ctx.entries.len());

        let id = ctx.from_strn(b"third").unwrap();
        assert_eq!(N_TOTAL, id);
    }

    #[test]
    fn qstr_to_entry_static_cutoff_and_new_fallback() {
        let mut ctx = QstrContext::new();

        // id <= QSTR_LAST_STATIC -> .static
        let e_zip = ctx.to_entry(QSTR_LAST_STATIC);
        assert_eq!(QstrEntry::Static(QSTR_LAST_STATIC), e_zip);

        // QSTR_LAST_STATIC < id < N_TOTAL -> .new (inline static bytes)
        let above = QSTR_LAST_STATIC + 5;
        let e_above = ctx.to_entry(above);
        assert_eq!(QstrEntry::New(QSTR_STRINGS[above as usize]), e_above);

        // dynamic -> .new (inline dynamic bytes)
        let dyn_id = ctx.from_strn(b"dynamic_q").unwrap();
        let e_dyn = ctx.to_entry(dyn_id);
        assert_eq!(QstrEntry::New(b"dynamic_q"), e_dyn);
    }

    #[test]
    fn long_string_255_ok_256_rejected() {
        let mut ctx = QstrContext::new();
        let buf255 = [b'x'; 255];
        let id = ctx.from_strn(&buf255).unwrap();
        assert_eq!(255u8, ctx.qstr_len(id));

        let buf256 = [b'y'; 256];
        assert_eq!(Err(PyError::OutOfMemory), ctx.from_strn(&buf256));
    }

    // ── Extra evidence (own tests, not part of the Zig bar) ─────────────

    /// Every static-pool entry is self-consistent: declared length and
    /// hash match the bytes (djb2), `find_strn` returns its own id, and
    /// re-interning returns the same id. This is the `.static`
    /// assertion of `qstr_test.zig` applied to the whole pool.
    /// (Id 0, MP_QSTRnull, is exempt from the hash check: the vendor
    /// table records hash 0 for it by definition — 0 is vendor's
    /// "hash not computed" sentinel — while djb2("") is 5381. Empty
    /// input short-circuits to id 1 in `find_strn`, so id 0's hash is
    /// never consulted for lookup.)
    #[test]
    fn full_static_pool_self_consistent() {
        let mut ctx = QstrContext::new();
        for id in 0..N_TOTAL {
            let bytes = QSTR_STRINGS[id as usize];
            assert_eq!(bytes.len() as u8, QSTR_LENGTHS[id as usize]);
            if id != 0 {
                assert_eq!(QSTR_HASHES[id as usize], qstr_compute_hash(bytes));
            }
            if !bytes.is_empty() {
                assert_eq!(id, ctx.find_strn(bytes));
                assert_eq!(Ok(id), ctx.from_strn(bytes));
            }
        }
        // No dynamic entries were created: every static string dedups
        // against the static pool.
        assert_eq!(0, ctx.entries.len());
    }

    /// The generated table reproduces the vendored header exactly.
    /// Re-parses `vendor/qstrdefs.generated.h` independently (same
    /// grammar as build.rs, separate implementation) and compares
    /// every entry. This is the check that makes hand-copying
    /// impossible: any drift between header and table fails here.
    #[test]
    fn generated_table_matches_vendored_header() {
        let src = include_str!("../../vendor/qstrdefs.generated.h");
        let mut statics: Vec<(Vec<u8>, u16, u8)> = Vec::new();
        let mut mains: Vec<(Vec<u8>, u16, u8)> = Vec::new();
        for line in src.lines() {
            let (is0, is1) = (
                line.starts_with("QDEF0("),
                line.starts_with("QDEF1("),
            );
            if !is0 && !is1 {
                continue;
            }
            let entry = parse_qdef_line(line);
            if is0 {
                statics.push(entry);
            } else {
                mains.push(entry);
            }
        }
        assert_eq!(N_STATIC as usize, statics.len());
        assert_eq!(N_MAIN as usize, mains.len());
        for (i, (bytes, hash, len)) in statics.iter().chain(mains.iter()).enumerate() {
            assert_eq!(bytes.as_slice(), QSTR_STRINGS[i], "strings[{i}]");
            assert_eq!(*hash, QSTR_HASHES[i], "hashes[{i}]");
            assert_eq!(*len, QSTR_LENGTHS[i], "lengths[{i}]");
        }
    }

    fn parse_qdef_line(line: &str) -> (Vec<u8>, u16, u8) {
        let b = line.as_bytes();
        let open = b.iter().position(|&c| c == b'(').unwrap();
        let close = b.iter().rposition(|&c| c == b')').unwrap();
        let body = &b[open + 1..close];
        let mut i = body.iter().position(|&c| c == b',').unwrap() + 1;
        let skip = |mut j: usize| {
            while j < body.len() && (body[j] == b' ' || body[j] == b'\t') {
                j += 1;
            }
            j
        };
        let hs = skip(i);
        let he = hs + body[hs..].iter().position(|&c| c == b',').unwrap();
        let hash: u16 = std::str::from_utf8(&body[hs..he]).unwrap().trim().parse().unwrap();
        i = he + 1;
        let ls = skip(i);
        let le = ls + body[ls..].iter().position(|&c| c == b',').unwrap();
        let len: u8 = std::str::from_utf8(&body[ls..le]).unwrap().trim().parse().unwrap();
        i = le + 1;
        let ss = skip(i);
        assert_eq!(body[ss], b'"');
        let se = body.iter().rposition(|&c| c == b'"').unwrap();
        let mut out = Vec::new();
        let mut j = ss + 1;
        while j < se {
            if body[j] != b'\\' {
                out.push(body[j]);
                j += 1;
                continue;
            }
            j += 1;
            match body[j] {
                b'n' => {
                    out.push(b'\n');
                    j += 1;
                }
                b't' => {
                    out.push(b'\t');
                    j += 1;
                }
                b'r' => {
                    out.push(b'\r');
                    j += 1;
                }
                b'0' => {
                    out.push(0);
                    j += 1;
                }
                b'\\' => {
                    out.push(b'\\');
                    j += 1;
                }
                b'"' => {
                    out.push(b'"');
                    j += 1;
                }
                b'\'' => {
                    out.push(b'\'');
                    j += 1;
                }
                b'x' => {
                    let hex = std::str::from_utf8(&body[j + 1..j + 3]).unwrap();
                    out.push(u8::from_str_radix(hex, 16).unwrap());
                    j += 3;
                }
                other => panic!("unhandled escape \\{}", other as char),
            }
        }
        assert_eq!(out.len(), len as usize, "declared len mismatch: {line}");
        (out, hash, len)
    }
}
