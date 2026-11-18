//! Compiled code objects (E1 owns; `builtinevex.zig`'s code
//! section + vendor `py/objcode.c` at the BASIC level).
//!
//! `compile()` returns a code object: a thin wrapper around
//! the compiled module function (`FunBc`) — the Zig's
//! `MpObjCodeMinimum = {base, module_fun}`
//! (`builtinevex.zig:9-13`), built by `newCodeMinimum`.
//!
//! The type: vendor `objcode.c` at
//! `MICROPY_PY_BUILTINS_CODE_BASIC` defines `mp_type_code`
//! with name `MP_QSTR_code` and `MP_TYPE_FLAG_NONE` — and NO
//! print slot. The Rust type mirrors that: name `"code"`
//! (a static qstr — it IS in the generated table), flags 0,
//! no slots. Two consequences, both recorded:
//!
//! * `repr(c)` is `<code>`: no print slot, so
//!   `obj_print_helper`'s fallback prints `<{typename}>`
//!   with the static name. The reference prints `<>`
//!   instead — its stub type has name qstr 0. A documented
//!   divergence, same root cause as the `type()` abort
//!   below (the reference's null type pointer).
//! * `type(c)` is `<class 'code'>`. The reference ABORTS
//!   here (rc=134 — `full_stubs.zig`'s `mp_type_code` is a
//!   256-zero-byte stub, so the null type pointer dereferences
//!   in `mp_mirrors.zig:745`); Rust implements the vendor
//!   shape properly instead of reproducing the abort.
//!
//! `codeExecute` itself lives in `builtinevex.rs` (it needs
//! `call_function_ctx`, which the objects tree must not
//! depend on); this module owns the type data + the
//! allocation / exact-type helpers.

use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::value::{CodeData, GcRef, Obj, TypeData, Value};

/// The code type data (`objcode.c`, BASIC level): name
/// `"code"`, flags `MP_TYPE_FLAG_NONE` (0), no slots. The
/// name id is interned by `build_vm_types` (dynamic — `"code"`
/// is not in the static qstr table) and passed in.
pub fn code_type_data(dict: GcRef, name_id: u32) -> TypeData {
    TypeData {
        name: name_id,
        flags: 0,
        base: None,
        mro: Vec::new(),
        dict,
        namedtuple_fields: None,
        protocol: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        attr: None,
        make_new: None,
        bases: None,
    }
}

/// Allocate a code object wrapping `module_fun`
/// (`newCodeMinimum`, `builtinevex.zig:17-19`).
pub fn new_code(ctx: &mut CallCtx, code_type: GcRef, module_fun: GcRef) -> Value {
    Value::Heap(ctx.alloc(Some(code_type), Obj::Code(CodeData { module_fun })))
}

/// `objIsCode` (`builtinevex.zig:22-24`): exact-type check
/// against `mp_type_code` (false under nano — we are full).
pub fn is_code(arena: &GcArena, code_type: GcRef, value: Value) -> bool {
    match value {
        Value::Heap(r) => arena.header(r).type_ == Some(code_type),
        _ => false,
    }
}
