//! Shared static qstr table — one table for both qstr tracks.
//!
//! The static qstr pool (QDEF0 "static" + QDEF1 "main" entries from
//! `vendor/qstrdefs.generated.h`) is generated at build time by
//! `build.rs`, which parses the vendored header — the same header the
//! Zig originals parse at comptime — and is shared by:
//!   * `crate::compiler::qstr` (A1, the compiler track: `QstrContext`)
//!   * `crate::runtime::qstr` (Lane C1, the runtime track: `RuntimeQstr`)
//!
//! Both tracks see the same numbering (N_STATIC 228, N_MAIN 385,
//! N_TOTAL 613), so a qstr id means the same string on either side.
//! The table lives here — in neither track — so the runtime (the
//! lower layer) never imports from the compiler. `compiler::qstr`
//! re-exports everything below, so existing `compiler::qstr::*`
//! paths keep working unchanged.
//!
//! Also home to the shared `Qstr` id type and the djb2 hash, which
//! both tracks use: the vendor-precomputed hashes in `QSTR_HASHES`
//! must line up with runtime hashes for the same string.

// ── Static-pool data (build-time generated) ───────────────────────────────
//
// Layout (mirrors the Zig StaticStrings/Hashes/Lengths):
//   ids 0..N_STATIC-1        -> QDEF0 entries, in file order
//   ids N_STATIC..N_TOTAL-1  -> QDEF1 entries, in file order
include!(concat!(env!("OUT_DIR"), "/qstr_table.rs"));

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
