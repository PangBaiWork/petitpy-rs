//! The standard-library builtin modules (Lane D3: modjson,
//! modmath, modsocket, modstruct, mod_subprocess).
//!
//! Each module follows the `build_io_module` pattern from
//! `src/objects/containers/objstringio_types.rs`: a
//! `build_<name>_module(ctx, fun_builtin_var, module_type) ->
//! PyResult<Value>` builder returning the module object.
//! Registration into `VmState.builtin_modules` (so `import <name>`
//! finds them) is Lane D1's, not the builders'.

pub mod modarray;
pub mod modcollections;
pub mod moderrno;
pub mod modgc;
pub mod modgrp;
pub mod modjson;
pub mod modmain;
pub mod modmath;
pub mod modmicropython;
pub mod modpwd;
pub mod modre;
pub mod modre_match;
pub mod modre_types;
pub mod modselect;
pub mod modsignal;
pub mod modsocket;
#[cfg(feature = "unsafe-ffi")]
pub mod modsocket_ffi;
pub mod modstruct;
pub mod modsubprocess;
pub mod modtime;
pub(crate) mod sysdb;

#[cfg(test)]
mod modjson_tests;
#[cfg(test)]
mod modmath_tests;
#[cfg(all(test, unix))]
mod modsocket_tests;
#[cfg(test)]
mod modstruct_tests;
#[cfg(all(test, unix))]
mod modsubprocess_tests;
#[cfg(test)]
mod modd4_tests;
#[cfg(test)]
mod modre_tests;
#[cfg(test)]
mod modre_frozen_tests;

// ── Lane D4 shared builder helpers ──────────────────────────────────
// The D4 small modules share the D2/D3 builder shape
// (`build_<name>_module(ctx, fun_builtin_var, module_type) ->
// PyResult<Value>`, `new_module` + fixed dict); these helpers
// are the common pieces (D2's `modos.rs` `mk_fun` / `store`
// inlined per module there — here they are shared so the
// eleven small builders stay short).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn};
use crate::objects::value::{FunBuiltinData, GcRef, Obj, Value};

/// Build one `FunBuiltin` value (D2's `modos.rs` `mk_fun`).
pub(crate) fn mk_fun(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    name: &str,
    min: u16,
    max: u16,
    func: BuiltinFn,
    kw_func: Option<BuiltinKwFn>,
) -> Value {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("D4 module member name is not a static qstr: {name}"));
    Value::Heap(ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(min, max),
            func,
            kw_func,
        }),
    ))
}

/// Store one entry in a fresh module dict (D2's `modos.rs`
/// `store`): cannot fail on a fresh dict.
pub(crate) fn store(ctx: &mut CallCtx, dict: Value, name: &str, value: Value) {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("D4 module member name is not a static qstr: {name}"));
    dict_store(ctx, dict, Value::Qstr(id), value)
        .map_err(|e| ctx.record_typed(e))
        .expect("D4 module dict store cannot fail on a fresh dict");
}

/// Fix a module dict (the C's ROM dict, `is_fixed = 1` —
/// the D2 `modos.rs` / `modsys.rs` pattern).
pub(crate) fn fix_dict(ctx: &mut CallCtx, dict: Value) {
    if let Value::Heap(r) = dict {
        if let Obj::Dict(d) = ctx.arena.get_mut(r) {
            d.map.is_fixed = true;
        }
    }
}

/// Allocate the module and return `(module, dict_value)`
/// (the `new_module` + dict-extract pair every D4 builder
/// repeats).
pub(crate) fn new_d4_module(
    ctx: &mut CallCtx,
    name: &str,
    module_type: GcRef,
) -> (Value, Value) {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("D4 module name is not a static qstr: {name}"));
    let module = new_module(ctx, id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Value::Heap(m.dict),
            _ => unreachable!("new_module returns a module"),
        },
        _ => unreachable!("new_module returns a heap value"),
    };
    (module, dict)
}
