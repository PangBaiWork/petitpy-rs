//! The VM-only entry (`entry/embed_run_mpy.zig` +
//! `main_vm.zig`): load a `.mpy` buffer and execute its
//! top-level function.
//!
//! No compile step for the entry `.mpy` itself; `eval` /
//! `exec` / `compile` and `.py` imports inside it work — the
//! compiler is linked (the old compiler separation was removed:
//! it saved only ~14KB of ~2.1MB).

use crate::entry::embed_exec::exit_status_for;
use crate::runtime::vm::{Vm, VmError};

/// `npy_embed_run_mpy` (`entry/embed_run_mpy.zig`): load +
/// execute an in-memory `.mpy` as `__main__`. Exit-code
/// contract matches `run_source`: 0 clean / SystemExit() /
/// None / 0; 1 uncaught non-SystemExit (traceback printed —
/// to STDOUT here, `mp_plat_print`, NOT stderr — the probed
/// asymmetry) or SystemExit with a non-int arg; N =
/// SystemExit(N) int, low 8 bits.
pub fn run_mpy(vm: &mut Vm, data: &[u8]) -> i32 {
    match vm.load_module_from_mpy(data, "__main__") {
        Ok(_) => 0,
        Err(VmError::Raised(exc)) => exit_status_for(vm, exc, false),
        Err(e) => {
            // A loader failure (bad `.mpy` header etc.): the
            // reference raises `ValueError("incompatible .mpy
            // file")` via `raiseIncompatibleMpy` and prints its
            // traceback to stdout, exit 1.
            let exc = {
                let (mut ctx, state) = vm.split();
                crate::runtime::compile_bridge::vm_error_to_value(&mut ctx, state, e)
            };
            exit_status_for(vm, exc, false)
        }
    }
}
