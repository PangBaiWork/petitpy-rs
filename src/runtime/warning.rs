//! `mp_warning` / `mp_emitter_warning` (vendor `py/warning.c`).
//!
//! Live under `MICROPY_WARNINGS` (full config — gated on
//! `PETITPY_FEATURE_WARNING`, preset default on,
//! `mpconfigport.h:338`). The only in-tree caller today is
//! the str-vs-bytes comparison warning (`obj_ops.rs`, via
//! `warn_str_bytes_cmp`); `mp_emitter_warning` has no Rust
//! caller yet (the C's only callers are the emitter's
//! code-size pass — no Zig call sites were found either).
//! Output is `MICROPY_ERROR_PRINTER`, i.e. fd 2
//! (`MP_STDERR_PRINT`, `mpprint.rs:673`).

use crate::objects::strings::mpprint::{mp_print_str, Print, MP_STDERR_PRINT};

/// `MP_PASS_CODE_SIZE` (3, vendor `py/emit.h:45`): the only
/// pass `mp_emitter_warning` fires on.
pub const MP_PASS_CODE_SIZE: u32 = 3;

/// The warning line, pure for testability:
/// `{category or "Warning"}: {msg}\n` (`warning.c:36-46`).
/// The C formats `msg` with `mp_vprintf` (a format string);
/// Rust callers pass the already-formatted message.
pub fn format_warning(category: Option<&str>, msg: &str) -> String {
    format!("{}: {msg}\n", category.unwrap_or("Warning"))
}

/// `mp_warning` (`warning.c:36-46`): `NULL` category →
/// `"Warning"`; writes to stderr, ignoring write errors
/// (the C's `mp_print_str` return is ignored the same way).
pub fn mp_warning(category: Option<&str>, msg: &str) {
    let line = format_warning(category, msg);
    let mut sink = MP_STDERR_PRINT;
    let mut print = Print::new(&mut sink);
    let _ = mp_print_str(&mut print, line.as_bytes());
}

/// `mp_emitter_warning` (`warning.c:52-56`): only fires for
/// the code-size pass.
pub fn mp_emitter_warning(pass: u32, msg: &str) {
    if pass == MP_PASS_CODE_SIZE {
        mp_warning(None, msg);
    }
}
