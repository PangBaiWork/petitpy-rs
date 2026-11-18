//! The D2 builtin modules, built at `Vm::new` (split out of
//! `vm.rs`, Lane D2 — `vm.rs` was over the 800-line cap).
//!
//! The original's `mp_init` registers ALL builtin modules at
//! startup; D2 builds the `os` / `sys` modules here (after
//! `build_builtins`, which owns `io` since D1) and pins them
//! in the VM's roots, so they survive GC.
//!
//! Lane D4 adds `build_d4_modules` (same shape): the 11
//! small modules. Seven register as BUILTIN modules
//! (`time`, `select`, `gc`, `signal`, `pwd`, `grp`,
//! `micropython`); three register as EXTENSIBLE (`errno`,
//! `collections`, `array`) — those three are the ones the
//! reference resolves through a `u`-prefix strip
//! (`import uerrno` / `uarray` / `ucollections` — probed
//! 2026-11-18; `uos`, `ugc`, `utime`, `uselect`,
//! `usignal`, `upwd`, `ugrp`, `umicropython` all fail on
//! the reference, so their modules are builtin, not
//! extensible).

use crate::modules::modarray::build_array_module;
use crate::modules::modcollections::build_collections_module;
use crate::modules::moderrno::build_errno_module;
use crate::modules::modgc::build_gc_module;
use crate::modules::modgrp::build_grp_module;
use crate::modules::modmicropython::build_micropython_module;
use crate::modules::modpwd::build_pwd_module;
use crate::modules::modselect::build_select_module;
use crate::modules::modsignal::build_signal_module;
use crate::modules::modtime::build_time_module;
use crate::objects::containers::objfileio::build_stdio_objects;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::Value;
use crate::runtime::modos::build_os_module;
use crate::runtime::modsys::build_sys_module;
use crate::runtime::vm::Vm;

/// Build the `sys` module (with the stdio objects) and the
/// `os` module, register both in `state.builtin_modules`,
/// and pin them in the VM's roots.
pub fn build_d2_modules(vm: &mut Vm) {
    let (mut ctx, state) = vm.split();
    let fileio = ctx.types.fileio;
    let textio = ctx.types.textio;
    let attrtuple = ctx.types.attrtuple;
    let (stdin, stdout, stderr) = build_stdio_objects(&mut ctx, fileio, textio);
    let sys = build_sys_module(&mut ctx, state, stdin, stdout, stderr);
    let sys_id = static_qstr_id(b"sys").expect("sys is a static qstr");
    state.builtin_modules.push((sys_id, sys));
    let os = build_os_module(&mut ctx, state);
    let os_id = static_qstr_id(b"os").expect("os is a static qstr");
    state.builtin_modules.push((os_id, os));
    drop(ctx);
    vm.pin(Value::Heap(attrtuple));
    vm.pin(sys);
    vm.pin(os);
}

/// Build the D4 small modules, register them in
/// `state.builtin_modules` (7) / `state.extensible_modules`
/// (3), store the `collections` type refs in
/// `vm.types`, and pin all ten modules in the VM's roots.
///
/// The builders return `PyResult` (the D3 shape) but cannot
/// fail on a fresh dict — the `expect` documents that (the
/// same shape as the D2 `store` helper).
pub fn build_d4_modules(vm: &mut Vm) {
    let (mut ctx, state) = vm.split();
    // Reserve the 64 signal-handler root slots (one per
    // signum — the `D4State::signal_base` doc): the handler
    // table lives outside the arena, so each handler needs a
    // GC root; reserving them once (below `extra_base`, the
    // `vm.pin` shape) lets `signal()` replace a handler
    // without growing the root set on every call.
    let signal_base = state.extra_base;
    for _ in 0..crate::runtime::d4state::NSIG {
        ctx.roots.extra.insert(state.extra_base, Value::None);
        state.extra_base += 1;
    }
    state.d4.signal_base = signal_base;
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let time = build_time_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 time module build cannot fail");
    let select = build_select_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 select module build cannot fail");
    let gc = build_gc_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 gc module build cannot fail");
    let signal = build_signal_module(&mut ctx, &mut *state);
    let pwd = build_pwd_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 pwd module build cannot fail");
    let grp = build_grp_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 grp module build cannot fail");
    let micropython = build_micropython_module(&mut ctx, &mut *state);
    let errno = build_errno_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 errno module build cannot fail");
    let (collections, deque_type, ordereddict_type, namedtuple_fn) =
        build_collections_module(&mut ctx, fun_builtin_var, module_type)
            .expect("D4 collections module build cannot fail");
    let array = build_array_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 array module build cannot fail");
    for (name, module) in [
        ("time", time),
        ("select", select),
        ("gc", gc),
        ("signal", signal),
        ("pwd", pwd),
        ("grp", grp),
        ("micropython", micropython),
    ] {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("D4 module name is not a static qstr: {name}"));
        state.builtin_modules.push((id, module));
    }
    for (name, module) in [("errno", errno), ("collections", collections), ("array", array)] {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("D4 module name is not a static qstr: {name}"));
        state.extensible_modules.push((id, module));
    }
    drop(ctx);
    // The `collections` type refs (the D3 `socket` precedent —
    // stored after `drop(ctx)` since `ctx.types` borrows
    // `vm.types`).
    vm.types.deque = Some(deque_type);
    vm.types.ordereddict = Some(ordereddict_type);
    if let Value::Heap(r) = namedtuple_fn {
        vm.types.namedtuple_fn = Some(r);
    }
    for module in [time, select, gc, signal, pwd, grp, micropython, errno, collections, array] {
        vm.pin(module);
    }
}
