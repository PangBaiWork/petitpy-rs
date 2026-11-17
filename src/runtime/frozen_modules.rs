//! Frozen-module table (`runtime/frozen_modules.zig`, 74 lines).
//!
//! Modules ship as pre-compiled `.mpy` byte buffers, baked into the
//! binary. Under the default build (`MICROPY_FROZEN_TINY_ENABLED=0`,
//! i.e. until `make freeze` has run) the table is EMPTY — frozen
//! lookup always returns `None` and `sys.path` filesystem search is
//! the only external-import mechanism (`frozen_modules.zig:17-20`).
//!
//! The Zig table is a comptime `[_]Entry{ name, bytes }` carved out
//! of a bundled blob; the Rust form is an owned `Vec` of
//! `(name, bytes)` pairs, filled by the host (Phase D freezes the
//! petitpy-lib bundle via `build.rs` / `include_bytes!`).
//! `lookup` / `is_package` are the two pub fns of the Zig file
//! (`:47`, `:61`).

/// A frozen-module table: `(dotted name, .mpy bytes)` pairs.
///
/// The reference build freezes `json` and `types` (ADR-003 L2); the
/// Rust table starts empty and is filled by the host via
/// `Vm::set_frozen_modules` (Phase D).
#[derive(Clone, Debug, Default)]
pub struct FrozenTable {
    entries: Vec<(String, Vec<u8>)>,
}

impl FrozenTable {
    /// An empty table (the default build — `frozen_modules.zig:47`:
    /// `if (gen.table.len == 0) return null`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a table from `(name, bytes)` pairs.
    pub fn from_entries(entries: Vec<(String, Vec<u8>)>) -> Self {
        FrozenTable { entries }
    }

    /// `lookup` (`frozen_modules.zig:47`): the `.mpy` bytes for a
    /// module name, or `None` when the module is not frozen.
    /// Compared by exact name bytes (`std.mem.eql` in the Zig).
    pub fn lookup(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    /// `isPackage` (`frozen_modules.zig:61`): a frozen module is a
    /// package when some frozen entry sits under it (`collections`
    /// with `collections.abc`).
    pub fn is_package(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| {
            n.len() > name.len()
                && n.as_bytes()[name.len()] == b'.'
                && n.starts_with(name)
        })
    }

    /// Number of frozen entries (tests / diagnostics only).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The frozen module names, in table order
    /// (`mp_help_add_from_names` iterates the same list; the C
    /// strips a trailing `.py` — Rust names are stored
    /// without it).
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Whether the table is empty (the default build).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_lookup_is_none() {
        let t = FrozenTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.lookup("json"), None);
        assert!(!t.is_package("json"));
    }

    #[test]
    fn lookup_and_is_package() {
        let t = FrozenTable::from_entries(vec![
            ("json".to_string(), vec![1, 2, 3]),
            ("collections.abc".to_string(), vec![4, 5]),
        ]);
        assert_eq!(t.lookup("json"), Some([1, 2, 3].as_slice()));
        assert_eq!(t.lookup("collections"), None);
        assert_eq!(t.lookup("collections.abc"), Some([4, 5].as_slice()));
        // `collections` is a package (an entry sits under it).
        assert!(t.is_package("collections"));
        // `json` is not (no entry under it); `collections.abc`
        // is not (nothing sits under the leaf).
        assert!(!t.is_package("json"));
        assert!(!t.is_package("collections.abc"));
        // Prefix without the dot does not count.
        assert!(!t.is_package("collect"));
    }
}
