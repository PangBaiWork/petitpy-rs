//! The frozen petitpy-lib bundle (Lane D5 — ADR-003 L2).
//!
//! The `.mpy` bytes are produced by `scripts/freeze_modules.py`
//! (the original's `tools/freeze_modules.py` analogue) and checked
//! in under `frozen/`; `build.rs` emits `OUT_DIR/frozen_table.rs`
//! with the `FROZEN_ENTRIES` table (`include_bytes!` slices, in
//! the original's MODULES order). When any file is missing the
//! table is empty (the Zig's `-Dfrozen-tiny=false` arm).

include!(concat!(env!("OUT_DIR"), "/frozen_table.rs"));

/// The frozen table for a fresh `VmState` (D5): the baked
/// `FROZEN_ENTRIES` copied into C15b's `FrozenTable` shape
/// (~38KB per `Vm::new` — acceptable, no churn to the type).
pub fn frozen_table() -> crate::runtime::frozen_modules::FrozenTable {
    crate::runtime::frozen_modules::FrozenTable::from_entries(
        FROZEN_ENTRIES
            .iter()
            .map(|(n, b)| (n.to_string(), b.to_vec()))
            .collect(),
    )
}
