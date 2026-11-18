//! Shared `/etc/passwd` + `/etc/group` parsing for the `pwd`
//! and `grp` modules (Lane D4).
//!
//! The Zig reads the same files through `open` + `readline`
//! (`modpwd.zig`, `modgrp.zig`); Rust reads them with
//! `std::fs::read_to_string` — the same data, the same field
//! order. Unparseable files (missing, unreadable) yield an
//! empty table, so every lookup raises `KeyError` (the Zig
//! would `mp_raise_OSError(ENOENT)` on a missing file — the
//! files always exist on a normal Linux system; the empty
//! table keeps the behaviour total without a second error
//! path).

/// Read a `:`-separated system database file into rows of
/// fields. Malformed lines (fewer than 2 fields) are skipped.
pub(crate) fn read_etc_file(path: &str) -> Vec<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.split(':').map(str::to_string).collect())
            .filter(|row: &Vec<String>| row.len() >= 2)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Find the first row whose `field` equals `needle`.
pub(crate) fn find_row<'a>(
    rows: &'a [Vec<String>],
    field: usize,
    needle: &str,
) -> Option<&'a Vec<String>> {
    rows.iter()
        .find(|row| row.len() > field && row[field] == needle)
}
