//! The VM's builtin interception chain — split out of
//! `vm_call.rs` (D2 merge integration: `vm_call.rs` was 823
//! lines). `call_function_ctx` checks the stateful builtins by
//! GcRef identity BEFORE the type-call dispatch: `next`,
//! `eval`/`exec`/`compile`, `__build_class__`, `__import__`,
//! `help`, the D1 stateful builtins, and the D2 `sys`/`os`
//! builtins all need VM state a `FunBuiltin` slot cannot see.
//! Returns `Some(result)` when `fun` is one of them, `None`
//! otherwise (the caller falls through to the type dispatch).

use crate::compiler::parser::InputKind;
use crate::memory::gc::CallCtx;
use crate::objects::value::Value;
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::{call_build_class_ctx, call_next_ctx};
use crate::runtime::vm_ops::exc_from_pyerror;

/// Intercept a stateful builtin by GcRef identity. `Some` =
/// intercepted (the result is final); `None` = ordinary value,
/// fall through to the type-call dispatch.
pub(crate) fn intercept_builtin_call(
    ctx: &mut CallCtx,
    state: &mut VmState,
    fun: Value,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Option<Result<Value, Value>> {
    // The `next` builtin is intercepted by GcRef identity (it
    // needs the VM's `iternext` seams — see `call_next`).
    if fun == state.next_builtin {
        return Some(call_next_ctx(ctx, state, n_args, args));
    }
    // The `eval`/`exec`/`compile` builtins are intercepted the
    // same way (C15a): they need the VM's globals/locals and
    // code registration, which a `FunBuiltin` slot cannot see.
    // (`eval` is `EvalInput`, `exec` is `FileInput` —
    // `compile_glue`'s mode table; the two differ only in
    // whether the body value is returned.)
    if fun == state.eval_builtin {
        return Some(crate::runtime::builtinevex::call_eval_exec_ctx(
            ctx,
            state,
            InputKind::EvalInput,
            n_args,
            n_kw,
            args,
        ));
    }
    if fun == state.exec_builtin {
        return Some(crate::runtime::builtinevex::call_eval_exec_ctx(
            ctx,
            state,
            InputKind::FileInput,
            n_args,
            n_kw,
            args,
        ));
    }
    if fun == state.compile_builtin {
        return Some(crate::runtime::builtinevex::call_compile_ctx(ctx, state, n_args, n_kw, args));
    }
    // `__build_class__` is intercepted the same way (C13b: it needs
    // the VM's frame execution + the locals override — see
    // `call_build_class_ctx`).
    if fun == state.build_class_builtin {
        return Some(call_build_class_ctx(ctx, state, n_args, n_kw, args));
    }
    // The `__import__` builtin is intercepted the same way
    // (C15b): it needs the VM's module registry + `sys.path`,
    // which a `FunBuiltin` slot cannot see.
    if fun == state.import_builtin {
        return Some(crate::runtime::builtinimport::call_import_ctx(
            ctx, state, n_args, n_kw, args,
        ));
    }
    // The `help` builtin is intercepted the same way (C18b):
    // the `"modules"` path needs the VM's module registries,
    // which a `FunBuiltin` slot cannot see.
    if fun == state.help_builtin {
        return Some(crate::runtime::builtinhelp::call_help_ctx(
            ctx, state, n_args, n_kw, args,
        ));
    }
    // The D1 stateful builtins are intercepted the same way
    // (`builtin_vm.rs`): `globals` / `locals` need the VM's
    // dicts; `dir` / `sorted` / `min` / `max` / `getattr` /
    // `setattr` / `hasattr` / `delattr` / `iter` / `all` /
    // `any` / `sum` need the dispatch loop's iteration seams
    // and the ability to call user functions.
    if fun == state.globals_builtin {
        return Some(crate::runtime::builtin_vm::call_globals_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.locals_builtin {
        return Some(crate::runtime::builtin_vm::call_locals_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.dir_builtin {
        return Some(crate::runtime::builtin_vm::call_dir_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.sorted_builtin {
        return Some(crate::runtime::builtin_vm::call_sorted_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.min_builtin {
        return Some(crate::runtime::builtin_vm::call_min_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.max_builtin {
        return Some(crate::runtime::builtin_vm::call_max_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.getattr_builtin {
        return Some(crate::runtime::builtin_vm::call_getattr_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.setattr_builtin {
        return Some(crate::runtime::builtin_vm::call_setattr_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.hasattr_builtin {
        return Some(crate::runtime::builtin_vm::call_hasattr_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.delattr_builtin {
        return Some(crate::runtime::builtin_vm::call_delattr_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.iter_builtin {
        return Some(crate::runtime::builtin_vm::call_iter_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.all_builtin {
        return Some(crate::runtime::builtin_vm::call_all_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.any_builtin {
        return Some(crate::runtime::builtin_vm::call_any_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.sum_builtin {
        return Some(crate::runtime::builtin_vm::call_sum_ctx(ctx, state, n_args, n_kw, args));
    }
    // `sys.print_exception` is intercepted the same way (D2):
    // its default target is the VM's `sys_stdout`, which a
    // `FunBuiltin` slot cannot see.
    if fun == state.sys_print_exception_builtin {
        return Some(
            crate::runtime::modsys::print_exception_ctx(ctx, state, n_args, n_kw, args)
                .map_err(|e| exc_from_pyerror(ctx, state, e)),
        );
    }
    // The `os` fd builtins are intercepted the same way (D2):
    // they need `state.os_fds`, which a `FunBuiltin` slot
    // cannot see.
    if fun == state.os_fd_builtins.open {
        return Some(crate::runtime::modos_fd::os_open_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.os_fd_builtins.close {
        return Some(crate::runtime::modos_fd::os_close_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.os_fd_builtins.read {
        return Some(crate::runtime::modos_fd::os_read_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.os_fd_builtins.write {
        return Some(crate::runtime::modos_fd::os_write_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.os_fd_builtins.lseek {
        return Some(crate::runtime::modos_fd::os_lseek_ctx(ctx, state, n_args, n_kw, args));
    }
    if fun == state.os_fd_builtins.fsync {
        return Some(crate::runtime::modos_fd::os_fsync_ctx(ctx, state, n_args, n_kw, args));
    }
    // `os.__getattr__` is intercepted the same way (D2): it
    // imports `os_path` / `os_ext` through the VM's import
    // machinery, which a `FunBuiltin` slot cannot see.
    if fun == state.os_getattr_builtin {
        return Some(crate::runtime::modos::os_getattr_ctx(ctx, state, n_args, n_kw, args));
    }
    None
}
