//! The D2 builtin modules, built at `Vm::new` (split out of
//! `vm.rs`, Lane D2 — `vm.rs` was over the 800-line cap).
//!
//! The original's `mp_init` registers ALL builtin modules at
//! startup; D2 builds the `os` / `sys` modules here (after
//! `build_builtins`, which owns `io` since D1) and pins them
//! in the VM's roots, so they survive GC.

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
