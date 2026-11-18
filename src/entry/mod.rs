//! The entry points (E1 owns; `src/main.zig` +
//! `src/main_vm.zig` + `src/entry/`).
//!
//! The two binaries are thin shells over this module:
//! `petitpy-rs` (the `petitpy` CLI: `--version` / `--help` /
//! `--emit-mpy` / `-c` / script / REPL) and `petitpy-vm`
//! (the VM-only loader). The shared hosts live here:
//! `embed_exec` (compile-then-exec, with the exit-code
//! contract + traceback streams) and `embed_run_mpy` (load a
//! `.mpy` buffer and run it).
//!
//! The printed program name and version are the ORIGINAL's
//! (`petitpy 0.9.0` / `petitpy-vm 0.9.0`) even though the
//! binaries are named `petitpy-rs` / `petitpy-vm` —
//! `main.zig` / `main_vm.zig` print those strings
//! byte-for-byte, and the brief pins them.

/// The petitpy version the CLIs print (`build_options.version`
/// in the original — the binary is `petitpy-rs 0.1.0` by Cargo,
/// but the user-facing version is the original's `0.9.0`).
pub const VERSION: &str = "0.9.0";

/// Map a `std::io::Error` from opening a file to the Zig
/// `@errorName` (`main.zig`'s `can't open file '{s}': {s}` /
/// `main_vm.zig`'s propagated `error: {s}`): FileNotFound /
/// PermissionDenied / IsDir for the common cases (probed),
/// the raw OS error number's Zig name for the rest, and the OS
/// message as a last resort (unprobed — see
/// `scripts/e1_result.txt` §4). NOTE for the vm binary: the
/// Zig runtime ALSO prints a stack trace after the `error:`
/// line — that trace is Zig-runtime noise (addresses + std
/// source paths) and is not reproduced; the stable first line
/// is (see `scripts/e1_result.txt` §3).
pub fn io_error_name(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(2) => "FileNotFound".to_string(),
        Some(13) => "PermissionDenied".to_string(),
        Some(21) => "IsDir".to_string(),
        Some(20) => "NotDir".to_string(),
        Some(36) => "NameTooLong".to_string(),
        Some(40) => "SymLinkLoop".to_string(),
        Some(22) => "InvalidArgument".to_string(),
        _ => e.to_string(),
    }
}

pub mod api;
pub mod base64;
pub mod embed_exec;
pub mod embed_run_mpy;
pub mod systemexit;

#[cfg(test)]
mod api_tests;
