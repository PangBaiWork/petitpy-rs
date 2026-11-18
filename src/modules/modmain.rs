//! The `__main__` module wiring (Lane D4).
//!
//! `__main__` is not a module file — it is the entry
//! module, a singleton wrapping the script's globals dict
//! (`dict_main`). The original never caches it in
//! `sys.modules`: `sys.modules` is EMPTY at script start,
//! `import __main__` works and `__main__.x` reads the
//! script globals, `__main__.__name__` is `'__main__'`,
//! but `sys.modules['__main__']` raises `KeyError`
//! (all probed on the reference binary 2026-11-18).
//!
//! The wiring lives in three places (this file documents
//! them; the code is in `vm_eval.rs` / `builtinimport.rs`):
//! - `load_module_from_mpy` sets `state.d4.main_module`
//!   to the entry module when a module is loaded.
//! - `register_code_inner` skips the `state.modules` insert
//!   AND the `sys.modules` mirror insert when the load name
//!   is `"__main__"`.
//! - `process_component` special-cases a top-level
//!   `"__main__"` (before the builtin lookup): it returns
//!   `state.d4.main_module` when set, else
//!   `ModuleNotFoundError`.
//!
//! No module is built here — `Vm::new` does not load the
//! entry module (Phase E entry work owns the CLI load, which
//! will load the script as `"__main__"`).

/// The `__main__` module name (a static qstr).
pub const MAIN_MODULE_NAME: &str = "__main__";
