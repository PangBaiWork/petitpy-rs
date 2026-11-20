//! G6b subprocess tests: `socket`/`subprocess` behaviors
//! that need the real binary. `fromfd` is CUT in H-socket
//! (safe Rust cannot take ownership of a raw fd): the name
//! stays, every call raises `NotImplementedError` on both
//! builds. This test runs the built `petitpy-rs` binary as
//! a subprocess and asserts exactly that.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

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

/// `socket.fromfd` is CUT in H-socket: every call raises
/// `NotImplementedError`, on both builds (the old G6b U4
/// behavior — the socket taking ownership of the fd and
/// `close()` really closing it — is gone with the FFI
/// implementation).
#[test]
fn g6b_fromfd_not_implemented() {
    let dir = scratch("fromfd_notimpl");
    // Any readable file gives a valid fd for fromfd; /dev/null
    // does not exist on Windows, so open the script itself.
    let path = dir
        .join("t.py")
        .to_string_lossy()
        .replace('\\', "/");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket, os\nfd = os.open({path:?}, os.O_RDONLY)\ntry:\n    s = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\n    print('fromfd: no error')\nexcept NotImplementedError as e:\n    print('fromfd:', e.args[0])\n"
        ),
    );
    let r = run_script(&script);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"fromfd: socket.fromfd is not implemented\n");
    assert!(r.stderr.is_empty());
}
