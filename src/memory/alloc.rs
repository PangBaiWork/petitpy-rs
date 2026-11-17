//! Raw byte-buffer allocation — port of `src/memory/alloc.zig`
//! (54 lines; vendor `py/malloc.c`'s `m_*` family on top of the
//! collector in `memory/gc.zig`, as vendor routes it through `gc.c`).
//!
//! Signatures match `MICROPY_MALLOC_USES_ALLOCATED_SIZE=0`.
//!
//! # Rust shape
//!
//! The original's `m_*` functions return raw `?*anyopaque` pointers
//! into the GC heap. Rust has no such pointers: non-GC byte buffers
//! are `Vec<u8>` on the Rust heap, and GC objects are allocated
//! through `GcArena` with a typed `Obj` payload. This module ports
//! the `m_*` family's *semantics* for the `Vec<u8>` case:
//!
//! | alloc.zig | Rust |
//! |---|---|
//! | `m_malloc(n)` | `malloc(n)`: zeroed buffer; `None` on failure |
//! | `m_malloc_maybe(n)` | `malloc_maybe(n)`: same, no failure hook |
//! | `m_malloc0(n)` | `malloc0(n)`: identical to `malloc` (blocks are always zeroed) |
//! | `m_realloc(p, n)` | `realloc(buf, n)`: `allow_move = true` |
//! | `m_realloc_maybe(p, n, allow_move)` | `realloc_maybe(buf, n, allow_move)` |
//! | `m_free(p)` | no counterpart: Rust drops by ownership |
//! | `m_malloc_with_finaliser(n)` | no `Vec` counterpart: the GC-backed
//! version is `GcArena::alloc_with_finaliser` (a `Vec` has no `__del__`) |
//!
//! # Failure semantics (verified against the originals)
//!
//! * Failure is `None` / `Err(original)`, never a panic: callers
//!   must check, as in C ("Allocators return null on failure;
//!   callers must check the null result (and `pyerr.pending_exc`)
//!   and propagate", alloc.zig header). `m_malloc_fail` itself is
//!   NOT ported as a function: it sets `pyerr.pending_exc`, which
//!   lives in `VmState` (C13); Rust callers map `None` to
//!   `PyError::OutOfMemory`.
//! * Zero-byte requests get a block (`alloc.zig`: "callers treat null
//!   as failure, as they did with `libc malloc(0)`"; `gc.zig`
//!   `allocWith` rounds to at least 1 block): `malloc(0)` returns
//!   `Some` (an empty `Vec`), never `None`.
//! * The 16 MiB cap (`gc.zig` `MAX_ALLOC`) is the only failure
//!   source here: `n > MAX_ALLOC` -> `None` / `Err`.
//! * `realloc` failure preserves the original buffer, untouched
//!   (`reallocMaybe` returns null without touching `p`; `m_realloc`
//!   then calls `m_malloc_fail`). Growth zero-fills the new region
//!   (`gc.zig` `reallocMaybe` memsets the grown blocks).
//! * `m_realloc(p, 0)` goes through `reallocMaybe`, which rounds to
//!   at least 1 block — it keeps a (zero-length) buffer and returns
//!   `Ok`. This differs from `gc_realloc(p, 0)`, which frees and
//!   returns null (see `GcArena::realloc`).
//! * `realloc_maybe` with `allow_move = false` only shrinks: a `Vec`
//!   grow cannot be guaranteed in place, so growth returns
//!   `Err(original)` (mirrors `reallocMaybe` failing when the
//!   following blocks are not free).

use crate::memory::gc::MAX_ALLOC;

/// `m_malloc`: zeroed buffer of `n` bytes; `None` when `n > MAX_ALLOC`.
/// Zero-byte requests return `Some` (an empty buffer), never `None`.
pub fn malloc(n: usize) -> Option<Vec<u8>> {
    if n > MAX_ALLOC {
        return None;
    }
    Some(vec![0; n])
}

/// `m_malloc_maybe`: like `malloc`, but with no failure hook on
/// failure (in the original the only difference from `m_malloc` is
/// the `m_malloc_fail` call, which Rust callers handle as
/// `PyError::OutOfMemory`).
pub fn malloc_maybe(n: usize) -> Option<Vec<u8>> {
    malloc(n)
}

/// `m_malloc0`: identical to `m_malloc` — "blocks are always zeroed"
/// (alloc.zig).
pub fn malloc0(n: usize) -> Option<Vec<u8>> {
    malloc(n)
}

/// `m_realloc` (`allow_move = true`): resize preserving content,
/// zero-filling growth. On failure returns `Err` with the original
/// buffer untouched.
pub fn realloc(buf: Vec<u8>, n: usize) -> Result<Vec<u8>, Vec<u8>> {
    realloc_maybe(buf, n, true)
}

/// `m_realloc_maybe`: resize preserving content, zero-filling growth.
/// With `allow_move = false` only shrinking succeeds; growth returns
/// `Err` with the original buffer untouched. `n == 0` returns an
/// empty buffer (`Ok`), mirroring `reallocMaybe`'s 1-block minimum
/// (unlike `gc_realloc`, which frees on 0).
pub fn realloc_maybe(mut buf: Vec<u8>, n: usize, allow_move: bool) -> Result<Vec<u8>, Vec<u8>> {
    if n > MAX_ALLOC {
        return Err(buf);
    }
    if n > buf.len() && !allow_move {
        return Err(buf);
    }
    buf.resize(n, 0);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malloc_is_zeroed() {
        let b = malloc(16).expect("malloc(16) succeeds");
        assert_eq!(b.len(), 16);
        assert!(b.iter().all(|&x| x == 0));
    }

    #[test]
    fn malloc_zero_bytes_is_some() {
        // Zero-byte requests get a block (alloc.zig header comment).
        let b = malloc(0).expect("malloc(0) returns Some");
        assert_eq!(b.len(), 0);
        assert!(malloc_maybe(0).is_some());
    }

    #[test]
    fn malloc_over_max_alloc_is_none() {
        assert!(malloc(MAX_ALLOC + 1).is_none());
        assert!(malloc_maybe(MAX_ALLOC + 1).is_none());
        assert!(malloc0(MAX_ALLOC + 1).is_none());
        let b = malloc(MAX_ALLOC).expect("exactly MAX_ALLOC succeeds");
        assert_eq!(b.len(), MAX_ALLOC);
    }

    #[test]
    fn malloc0_matches_malloc() {
        let b = malloc0(8).expect("malloc0(8) succeeds");
        assert_eq!(b.len(), 8);
        assert!(b.iter().all(|&x| x == 0));
    }

    #[test]
    fn realloc_grows_zero_filled() {
        let buf = vec![1u8, 2, 3];
        let grown = realloc(buf, 6).expect("realloc grow succeeds");
        assert_eq!(grown, vec![1, 2, 3, 0, 0, 0]);
    }

    #[test]
    fn realloc_shrinks() {
        let buf = vec![1u8, 2, 3, 4];
        let shrunk = realloc(buf, 2).expect("realloc shrink succeeds");
        assert_eq!(shrunk, vec![1, 2]);
    }

    #[test]
    fn realloc_failure_keeps_original() {
        let buf = vec![1u8, 2, 3];
        match realloc(buf, MAX_ALLOC + 1) {
            Err(orig) => assert_eq!(orig, vec![1, 2, 3]),
            Ok(_) => panic!("realloc over MAX_ALLOC must fail"),
        }
    }

    #[test]
    fn realloc_maybe_no_move_grow_fails() {
        let buf = vec![1u8, 2, 3];
        match realloc_maybe(buf, 6, false) {
            Err(orig) => assert_eq!(orig, vec![1, 2, 3]),
            Ok(_) => panic!("no-move grow must fail"),
        }
        let buf = vec![1u8, 2, 3, 4];
        let shrunk = realloc_maybe(buf, 2, false).expect("no-move shrink succeeds");
        assert_eq!(shrunk, vec![1, 2]);
    }

    #[test]
    fn realloc_maybe_zero_keeps_buffer() {
        // m_realloc goes through reallocMaybe (1-block minimum), unlike
        // gc_realloc which frees on 0.
        let buf = vec![1u8, 2, 3];
        let emptied = realloc_maybe(buf, 0, true).expect("realloc to 0 succeeds");
        assert_eq!(emptied.len(), 0);
    }
}
