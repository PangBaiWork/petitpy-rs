//! Lane D4 state: the per-VM mutable state the small modules
//! need that a plain `BuiltinFn` cannot see.
//!
//! * `micropython.opt_level` — `mp_optimise_value` (`mpstate.zig:178`,
//!   `usize`, init `0` — `runtime.zig:262`). Read/written by the
//!   intercepted `opt_level` builtin (`vm_call_intercept.rs`).
//! * `__main__` — the entry module (`modmain.zig`: the singleton
//!   module wrapping `dict_main`). Set by `load_module_from_mpy`
//!   (`vm_eval.rs`); `import __main__` reads it (`builtinimport.rs`).
//!   It is deliberately NOT entered in `state.modules` / the
//!   `sys.modules` mirror — the reference's `sys.modules` never
//!   contains `__main__` (probed: `sys.modules['__main__']` is
//!   `KeyError`).
//! * `signal` — the handler table (`modsignal.zig`: `[NSIG]Obj`,
//!   `NSIG = 64`, initialised to `None`; `signal()` stores,
//!   `getsignal()` returns, both intercepted in
//!   `vm_call_intercept.rs`). Handlers are pinned in `roots.extra`
//!   when stored (a handler is otherwise reachable only through
//!   this table, so an unpinned one could be collected).

use crate::objects::value::Value;

/// `NSIG` (`modsignal.zig`): the handler table size.
pub const NSIG: usize = 64;

/// Per-VM state for the Lane D4 small modules.
pub struct D4State {
    /// `mp_optimise_value` (`micropython.opt_level`).
    pub optimise_value: usize,
    /// The entry (`__main__`) module, if one has been loaded.
    pub main_module: Option<crate::objects::value::GcRef>,
    /// `signal` handler table (indexed by signum).
    pub signal_handlers: [Value; NSIG],
    /// The base index in `roots.extra` of the reserved
    /// 64-slot signal-handler root region (one root per
    /// signum, so `signal()` replaces a handler without
    /// growing the root set on every call — the original's
    /// fixed `[NSIG]Obj` table). Reserved by
    /// `build_d4_modules` at `Vm::new` (below `extra_base`,
    /// so `rebuild_frame_roots` keeps it); `0` before then
    /// (never a valid base — D2 pins 3 values first).
    pub signal_base: usize,
    /// The intercepted `micropython.opt_level` builtin.
    pub opt_level_builtin: Value,
    /// The intercepted `signal.signal` builtin.
    pub signal_builtin: Value,
    /// The intercepted `signal.getsignal` builtin.
    pub getsignal_builtin: Value,
}

impl Default for D4State {
    fn default() -> Self {
        D4State {
            optimise_value: 0,
            main_module: None,
            signal_handlers: [Value::None; NSIG],
            signal_base: 0,
            // Overwritten by `build_d4_modules` (the real
            // `FunBuiltin` values); `Value::None` is only the
            // pre-registration sentinel — the interception
            // compares by identity, and `None` never equals a
            // heap `FunBuiltin`.
            opt_level_builtin: Value::None,
            signal_builtin: Value::None,
            getsignal_builtin: Value::None,
        }
    }
}
