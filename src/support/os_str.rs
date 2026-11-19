//! Byte-oriented `OsStr` helpers shared by the `os` module, file I/O,
//! `sys.path` setup and the `zigpy-compile` binary.
//!
//! petitpy filenames are byte strings (the Zig interns raw bytes as
//! qstrs). On Unix, `OsStr` is bytes natively (`OsStrExt`). On Windows,
//! paths are UTF-16, so the byte form is the lossy UTF-8 rendering —
//! the same lossy rule the CLI already uses for non-UTF-8 arguments
//! (P7a), and the only option without a UTF-16 path model. Callers
//! that need exact bytes on Unix get them; on Windows they get the
//! lossy rendering.

use std::ffi::{OsStr, OsString};

/// Build an `OsString` from raw bytes: exact on Unix, lossy UTF-8 on Windows.
pub fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        OsStr::from_bytes(bytes).to_owned()
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Render an `OsStr` as bytes: exact on Unix, lossy UTF-8 on Windows.
pub fn os_str_as_bytes(s: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().into_owned().into_bytes()
    }
}
