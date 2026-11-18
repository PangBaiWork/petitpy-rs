//! The `array` builtin module (`src/modules/modarray.zig`, 66 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modarray.c`):
//! only `array` — 2 entries (`__name__` + `array`). The
//! `array` name binds the C9b array TYPE object
//! (`ctx.types.array`) — the module adds no functions of its
//! own (the Zig's table is just `MP_ROM_QSTR(MP_QSTR_array)`
//! → the type).
//!
//! Registered EXTENSIBLE: `import uarray` strips the `u`
//! and returns this module (probed on the reference binary
//! 2026-11-18).

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, new_d4_module, store};
use crate::objects::value::{GcRef, Value};
use crate::pyerr::PyResult;

/// `build_array_module` (`modarray.zig` table): 2 entries.
pub fn build_array_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let _ = fun_builtin_var;
    let (module, dict) = new_d4_module(ctx, "array", module_type);
    store(ctx, dict, "array", Value::Heap(ctx.types.array));
    fix_dict(ctx, dict);
    Ok(module)
}
