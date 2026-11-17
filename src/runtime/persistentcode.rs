//! Runtime `.mpy` loading (`runtime/persistentcode.zig`, 475 lines).
//!
//! The Zig file is the LOAD path of vendor `py/persistentcode.c`
//! (lines 159–539). Almost all of it was already ported elsewhere
//! — see `scripts/import_result.txt` §1a for the per-function table:
//! the byte/varint reader is Lane M's `mpy::Reader`, the prelude
//! decoders are C5's `bc::prelude_sig_decode` /
//! `prelude_size_decode`, `load_qstr`/`load_obj` are `mpy::read` +
//! `vm_raise::obj_from_entry`, and `load_raw_code` is
//! `mpy::read_raw_code` + `CodeStore::register_module`.
//!
//! What remains — and what this module owns — is the top-level
//! entry `mp_raw_code_load_mem` (`persistentcode.zig:469`): read
//! `.mpy` bytes, validate the header, resolve the qstr table,
//! convert the const-object table (pinning each value as it is
//! converted, exactly as `register_code_inner` did), and hand back
//! the loaded code. The header check the Zig does and Lane M's
//! `mpy::read` does NOT (`persistentcode.zig:419-426`): the arch
//! must be `MP_NATIVE_ARCH_NONE` (feature byte 2, decoded as
//! `(header[2] >> 2) & 0x2F == 0`) and the small-int width
//! (`header[3]`) must not exceed ours (63); either failure raises
//! `ValueError("incompatible .mpy file")`.
//!
//! The reader-based entry `mp_raw_code_load` (`:449`) has no Rust
//! caller — the reader form is C-ABI surface (`MpReader`
//! callbacks); Rust always loads from `&[u8]`. Not ported, no stub.

use crate::compiler::mpy::{self, QstrEntry, RawCode};
use crate::memory::gc::CallCtx;
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::value::Value;
use crate::pyerr::PyResult;
use crate::runtime::qstr::RuntimeQstr;
use crate::runtime::vm::{VmError, VmState};

/// `MP_SMALL_INT_BITS` (`persistentcode.zig:88`): 64-bit target.
const SMALL_INT_BITS: u8 = 63;

/// The loaded form of one `.mpy` file: the top-level raw code, the
/// resolved qstr table, and the converted const-object table — the
/// Rust form of the Zig's `MpCompiledModule` after
/// `mp_raw_code_load_mem` returns (`cm.rc` + `cm.context`'s qstr
/// and obj tables). The const table values are pinned in
/// `roots.extra` (see `raw_code_load_mem`).
pub struct LoadedCode {
    /// The top-level raw code (`cm.rc`).
    pub rc: RawCode,
    /// The resolved qstr table (static ids pass through, `New`
    /// entries were interned by the caller).
    pub qstr_table: Vec<u16>,
    /// The converted const objects (`ctx.constants.obj_table`),
    /// pinned in `roots.extra` at `state.extra_base`.
    pub const_table: Vec<Value>,
}

/// `mp_raw_code_load_mem` (`persistentcode.zig:469`): load a parsed
/// `.mpy` module into a `LoadedCode`.
///
/// The caller reads the module (`mpy::read`) and resolves the qstr
/// table first — both callers intern through the shared pool
/// (`from_strn` takes `&self` since C15b) — and passes the
/// resolved table in. Header validation is done here, on the parsed
/// `Module`: magic + version were already checked by `mpy::read`;
/// arch and small-int width are checked here
/// (`persistentcode.zig:419-426`). A bad header raises
/// `ValueError("incompatible .mpy file")` via `VmError::Raised`
/// (the exception object itself — `VmError` has a `Raised` arm).
pub fn raw_code_load_mem(
    ctx: &mut CallCtx,
    state: &mut VmState,
    module: mpy::Module,
    qstr_table: Vec<u16>,
) -> Result<LoadedCode, VmError> {
    // Header check (`persistentcode.zig:415-426`): arch =
    // `(header[2] >> 2) & 0x2F` must be NONE (0); small-int bits
    // (`header[3]`) must not exceed ours.
    let arch = (module.feature_flags >> 2) & 0x2F;
    if arch != 0 || module.small_int_bits > SMALL_INT_BITS {
        let exc = new_exception_msg(
            ctx,
            state.exc.value_error,
            "incompatible .mpy file",
        );
        return Err(VmError::Raised(exc));
    }
    // Convert the const-object table once (`load_obj` per entry,
    // via `obj_from_entry`) and pin every value in `roots.extra`
    // (inserted at `extra_base`, so `rebuild_frame_roots` keeps
    // them) — `LOAD_CONST_OBJ` must push the SAME object on every
    // load, as the Zig's `obj_table` does. Each value is pinned
    // immediately after conversion so a GC during the next
    // conversion cannot collect it. (Moved here from
    // `register_code_inner`, behaviour unchanged.)
    let mut const_table: Vec<Value> = Vec::with_capacity(module.objs.len());
    for entry in &module.objs {
        let types = ctx.types;
        let v = crate::runtime::vm_raise::obj_from_entry(
            ctx,
            types,
            state.ellipsis,
            entry,
        )
        .map_err(|_| VmError::OutOfMemory)?;
        ctx.roots.extra.insert(state.extra_base, v);
        state.extra_base += 1;
        const_table.push(v);
    }
    Ok(LoadedCode {
        rc: module.rc,
        qstr_table,
        const_table,
    })
}

/// Resolve a `.mpy` module's qstr table through the shared pool
/// (interns `New` entries — see `qstr::resolve_qstr_table`).
/// Thin wrapper over `resolve_qstr_table` for `raw_code_load_mem`
/// callers.
pub fn resolve_qstr_table_vm(
    pool: &RuntimeQstr,
    qstrs: &[QstrEntry],
) -> PyResult<Vec<u16>> {
    crate::runtime::qstr::resolve_qstr_table(pool, qstrs)
}
