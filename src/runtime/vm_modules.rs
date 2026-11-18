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
use crate::modules::modjson::build_json_module;
use crate::modules::modmath::build_math_module;
use crate::modules::modmicropython::build_micropython_module;
use crate::modules::modpwd::build_pwd_module;
use crate::modules::modre::build_re_module;
use crate::modules::modselect::build_select_module;
use crate::modules::modsignal::build_signal_module;
use crate::modules::modsocket::build_socket_module;
use crate::modules::modstruct::build_struct_module;
use crate::modules::modsubprocess::build_subprocess_module;
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

/// Build the D3 modules, register them, store the `socket`
/// type in `vm.types`, and pin all three modules in the VM's
/// roots. (E1 — D3 built the builders but did not register
/// them; the frozen `json.py` / `subprocess.py` layers needed
/// only `_json` / `_subprocess`, which D5 registered.)
///
/// - `math` and `socket` register as BUILTIN modules
///   (`MP_REGISTER_MODULE` in the original —
///   `modmath.zig:13`, `modsocket.zig:49`; probed: `import
///   umath` / `import usocket` fail on the reference, so the
///   u-strip does not resolve them).
/// - `struct` registers as EXTENSIBLE
///   (`MP_REGISTER_EXTENSIBLE_MODULE`, `modstruct.zig:18`;
///   probed: `import ustruct` resolves to it).
/// - The socket builder also returns the socket type, stored
///   in `vm.types.socket` (after `drop(ctx)` — the D5 `re`
///   precedent).
///
/// The builders return `PyResult` (the D3 shape) but cannot
/// fail on a fresh dict — the `expect` documents that (the
/// same shape as the D2 `store` helper).
pub fn build_d3_modules(vm: &mut Vm) {
    let (mut ctx, state) = vm.split();
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let math = build_math_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D3 math module build cannot fail");
    let (socket, socket_type) = build_socket_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D3 socket module build cannot fail");
    let struct_mod = build_struct_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D3 struct module build cannot fail");
    for (name, module) in [("math", math), ("socket", socket)] {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("D3 module name is not a static qstr: {name}"));
        state.builtin_modules.push((id, module));
    }
    let struct_id = static_qstr_id(b"struct").expect("struct is a static qstr");
    state.extensible_modules.push((struct_id, struct_mod));
    drop(ctx);
    // The socket type ref (the D5 `re` precedent — stored
    // after `drop(ctx)` since `ctx.types` borrows `vm.types`).
    vm.types.socket = Some(socket_type);
    for module in [math, socket, struct_mod] {
        vm.pin(module);
    }
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
    let (gc, gc_collect) = build_gc_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D4 gc module build cannot fail");
    // F6: `gc.collect` is intercepted by GcRef identity so the
    // finaliser drain runs with full VM state (see
    // `call_gc_collect_ctx`).
    state.gc_collect_builtin = gc_collect;
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

/// Build the D5 modules, register them, store the `re` type
/// refs in `vm.types`, and pin all three modules in the VM's
/// roots.
///
/// - `_json` and `_subprocess` (D3's builders) register as
///   BUILTIN modules — both are `MP_REGISTER_MODULE` in the
///   original, so `import _json` / `import _subprocess` finds
///   them before any filesystem search, and the frozen
///   `json.py` / `subprocess.py` layers (which `import _json`
///   / `import _subprocess`) resolve through them. The other
///   three D3 builders (math/socket/struct) are registered by
///   E1's `build_d3_modules` (above).
/// - `re` registers as EXTENSIBLE under the name `re` ONLY
///   (`MP_REGISTER_EXTENSIBLE_MODULE(MP_QSTR_re, ...)`).
///   `import ure` resolves to the SAME module object through
///   the import path's existing u-strip (D4) — no second
///   registration (registering both would wrongly list `ure`
///   as a separate module).
///
/// The builders return `PyResult` (the D3 shape) but cannot
/// fail on a fresh dict — the `expect` documents that (the
/// same shape as the D2 `store` helper).
pub fn build_d5_modules(vm: &mut Vm) {
    let (mut ctx, state) = vm.split();
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let json = build_json_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D5 _json module build cannot fail");
    let subprocess = build_subprocess_module(&mut ctx, fun_builtin_var, module_type)
        .expect("D5 _subprocess module build cannot fail");
    let (re, re_type, match_type, sub_fn) =
        build_re_module(&mut ctx, fun_builtin_var, module_type)
            .expect("D5 re module build cannot fail");
    for (name, module) in [("_json", json), ("_subprocess", subprocess)] {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("D5 module name is not a static qstr: {name}"));
        state.builtin_modules.push((id, module));
    }
    let re_id = static_qstr_id(b"re").expect("re is a static qstr");
    state.extensible_modules.push((re_id, re));
    state.re_sub_builtin = sub_fn;
    drop(ctx);
    // The `re` / `match` type refs (the D3 `socket`
    // precedent — stored after `drop(ctx)` since
    // `ctx.types` borrows `vm.types`).
    vm.types.re_type = Some(re_type);
    vm.types.match_type = Some(match_type);
    for module in [json, subprocess, re] {
        vm.pin(module);
    }
}
