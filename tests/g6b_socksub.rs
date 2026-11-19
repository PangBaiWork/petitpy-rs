//! G6b subprocess tests: `socket`/`subprocess` behaviors
//! that need the real binary (`--features unsafe-ffi`).
//! Every test spawns the built `petitpy-rs` binary as a
//! subprocess — `fromfd` takes ownership of a real fd,
//! so the close must happen in a child process, not in
//! the test harness. All tests are feature-gated; on the
//! default build this file contributes zero tests.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

#![cfg(feature = "unsafe-ffi")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("g6b-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_script(dir: &Path, name: &str, src: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

struct Run {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: i32,
}

fn run_script(script: &Path) -> Run {
    let out = Command::new(bin_petitpy())
        .arg(script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    }
}

/// `socket.fromfd` takes ownership (G6b U4): after
/// `s.close()` the fd is really closed, so `os.close(fd)`
/// is `EBADF` — exactly like the reference (probed).
/// On the default build the fd survives `s.close()`.
#[test]
fn g6b_fromfd_close_ebadf() {
    let dir = scratch("fromfd_close");
    let script = write_script(
        &dir,
        "t.py",
        "import socket, os\nfd = os.open('/dev/null', os.O_RDONLY)\ns = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ntry:\n    os.close(fd)\n    print('os.close after s.close: ok')\nexcept OSError as e:\n    print('os.close after s.close:', e.args[0])\n",
    );
    let r = run_script(&script);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"os.close after s.close: 9\n");
    assert!(r.stderr.is_empty());
}
