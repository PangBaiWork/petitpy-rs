//! Minimal error type for the compiler track.
//!
//! Port of the `PyError` half of `src/pyerr.zig`: the Zig original is
//! `error{PyRaise}` (a single error value carried through the
//! `PyError!T` error union, with the pending exception object held in
//! the global `pending_exc`). The full error taxonomy (exception
//! objects, tracebacks, the `pending_exc` transport) arrives with the
//! runtime phases; for now only what the ported code needs exists:
//! `PyRaise` (a Python-level raise was signalled) and `OutOfMemory`
//! (an allocation failed, or a length cap was hit — e.g. qstr's 255
//! byte limit, which the Zig side also reports as `error.OutOfMemory`).

use std::fmt;

/// A Python-level error signalled across the Rust port.
///
/// Mirrors Zig `PyError = error{PyRaise}` (plus the allocator error the
/// Zig qstr code returns for over-long strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyError {
    /// A Python exception was raised (the exception object itself is
    /// carried elsewhere once the runtime exists).
    PyRaise,
    /// An allocation failed, or a size cap was hit.
    OutOfMemory,
}

impl fmt::Display for PyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PyError::PyRaise => write!(f, "Python exception raised"),
            PyError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for PyError {}

/// `Result<T, PyError>` — the Rust spelling of Zig's `PyError!T`.
pub type PyResult<T> = Result<T, PyError>;
