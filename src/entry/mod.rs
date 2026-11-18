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

/// Map a `std::io::Error` from reading a file to the Zig
/// `@errorName` (`main.zig`'s `can't open file '{s}': {s}` /
/// `main_vm.zig`'s propagated `error: {s}`). Both CLIs read the
/// file with one `readFileAlloc[Options]` call (open + read),
/// so the table is the union of the two errno switches in the
/// Zig 0.16.0 stdlib that call implements:
/// `lib/std/Io/Dispatch.zig` `dirOpenFile` (~line 2598) for the
/// open half and `fileReadStreaming` (~line 1766) for the read
/// half. Rows marked PROBED were verified against the reference
/// binary (F2 probes in `~/workspace/f2_probe/`); the rest are
/// read verbatim from that switch — they cannot be triggered
/// through the CLI (see §4 of `scripts/f2_result.txt`), but the
/// mapping is source-cited, not guessed. The fallback is
/// `Unexpected`: any errno outside both switches goes through
/// `posix.unexpectedErrno` (`lib/std/posix.zig:1669`), which
/// returns `error.Unexpected`.
///
/// NOTE for the vm binary: the Zig runtime ALSO prints a stack
/// trace after the `error:` line — that trace is Zig-runtime
/// noise (addresses + std source paths) and is not reproduced;
/// the stable first line is (see `scripts/e1_result.txt` §3).
pub fn io_error_name(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(1) => "PermissionDenied",       // EPERM
        Some(2) => "FileNotFound",           // ENOENT, PROBED
        Some(5) => "InputOutput",            // EIO (read half)
        Some(6) => "NoDevice",               // ENXIO, PROBED
        Some(11) => "WouldBlock",            // EAGAIN
        Some(12) => "SystemResources",       // ENOMEM
        Some(13) => "AccessDenied",          // EACCES, PROBED (E1's
        // "PermissionDenied" was never triggered; the reference
        // prints AccessDenied)
        Some(16) => "DeviceBusy",            // EBUSY
        Some(17) => "PathAlreadyExists",     // EEXIST
        Some(19) => "NoDevice",              // ENODEV
        Some(20) => "NotDir",                // ENOTDIR, PROBED
        Some(21) => "IsDir",                 // EISDIR, PROBED
        Some(22) => "BadPathName",           // EINVAL (E1's
        // "InvalidArgument" is not in the Zig switch at all)
        Some(23) => "SystemFdQuotaExceeded", // ENFILE
        Some(24) => "ProcessFdQuotaExceeded", // EMFILE, PROBED
        Some(26) => "FileBusy",              // ETXTBSY
        Some(27) => "FileTooBig",            // EFBIG
        Some(28) => "NoSpaceLeft",           // ENOSPC
        Some(36) => "NameTooLong",           // ENAMETOOLONG, PROBED
        Some(40) => "SymLinkLoop",           // ELOOP, PROBED
        Some(75) => "FileTooBig",            // EOVERFLOW
        Some(95) => "FileLocksUnsupported",  // EOPNOTSUPP
        _ => "Unexpected",
    }
    .to_string()
}

#[cfg(test)]
mod io_error_name_tests {
    use super::io_error_name;

    /// The whole table, pinned: (errno, Zig `@errorName`).
    /// Probed rows carry the F2 probe; source-cited rows carry
    /// the Zig 0.16.0 `dirOpenFile` / `fileReadStreaming` switch
    /// they were read from (see the fn docs).
    #[test]
    fn io_error_name_table() {
        let rows: &[(i32, &str)] = &[
            (1, "PermissionDenied"),
            (2, "FileNotFound"),
            (5, "InputOutput"),
            (6, "NoDevice"),
            (11, "WouldBlock"),
            (12, "SystemResources"),
            (13, "AccessDenied"),
            (16, "DeviceBusy"),
            (17, "PathAlreadyExists"),
            (19, "NoDevice"),
            (20, "NotDir"),
            (21, "IsDir"),
            (22, "BadPathName"),
            (23, "SystemFdQuotaExceeded"),
            (24, "ProcessFdQuotaExceeded"),
            (26, "FileBusy"),
            (27, "FileTooBig"),
            (28, "NoSpaceLeft"),
            (36, "NameTooLong"),
            (40, "SymLinkLoop"),
            (75, "FileTooBig"),
            (95, "FileLocksUnsupported"),
        ];
        for &(errno, name) in rows {
            let e = std::io::Error::from_raw_os_error(errno);
            assert_eq!(io_error_name(&e), name, "errno {errno}");
        }
        // The fallback is the Zig name, not the OS message:
        // any errno outside both switches is `Unexpected`.
        for errno in [3, 4, 9, 30, 110] {
            let e = std::io::Error::from_raw_os_error(errno);
            assert_eq!(io_error_name(&e), "Unexpected", "errno {errno}");
        }
    }
}

pub mod api;
pub mod base64;
pub mod embed_exec;
pub mod embed_run_mpy;
pub mod systemexit;

#[cfg(test)]
mod api_tests;
