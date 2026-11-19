//! Support utilities (`src/support/` — Lane C16).
//!
//! Ports of the Zig `support/*` files that are not already covered
//! elsewhere in the tree:
//! - `stream` — the stream protocol (`py/stream.c`, `stream.zig`)
//! - `readline` — the `input()` / REPL line reader (`readline.zig`)
//! - `output` — fd 1/2 writes + the embedding-host sink (`output.zig`)
//! - `errno` — portable `errno` access (`errno_compat.zig`)
//! - `smallint` — `mp_small_int_modulo` / `mp_small_int_floor_divide`
//!   (`smallint.zig`)
//!
//! Already ported elsewhere (mapped here, not duplicated):
//! - `qstr_hash.zig` → `crate::qstr_table::qstr_compute_hash` (C1)
//! - `unicode.zig` → `crate::objects::strings::unicode` (C8a);
//!   `unichar_isident` was the one missing function and is added
//!   there by this lane.

pub mod errno;
pub mod ffi;
pub mod output;
pub mod readline;
pub mod smallint;
pub mod stream;
pub mod stream_methods;

#[cfg(test)]
mod stream_fixture;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod stream_tests_b;
#[cfg(test)]
mod tests;
