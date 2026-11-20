//! G6a subprocess tests: `os` syscalls that need the real binary
//! (`--features unsafe-ffi`). Every test spawns the built
//! `petitpy-rs` binary as a subprocess — closing fd 1 or mutating
//! the process umask in-process would break the test harness
//! itself. All tests are feature-gated; on the default build this
//! file contributes zero tests.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*` variables
//! Cargo sets for integration tests.

#![cfg(feature = "unsafe-ffi")]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("g6a-{}-{}", std::process::id(), name));
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

fn run_script(script: &Path, stdin_file: Option<&File>, stdout_file: Option<&File>) -> Run {
    let mut cmd = Command::new(bin_petitpy());
    cmd.arg(script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB");
    match stdin_file {
        Some(f) => {
            cmd.stdin(Stdio::from(f.try_clone().unwrap()));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    match stdout_file {
        Some(f) => {
            cmd.stdout(Stdio::from(f.try_clone().unwrap()));
        }
        None => {
            cmd.stdout(Stdio::piped());
        }
    }
    cmd.stderr(Stdio::piped());
    let out = cmd.output().unwrap();
    Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    }
}

/// `os.close(0)` frees fd 0 for reuse, and a later read on it
/// raises EBADF (G6a U3). Subprocess: closing fd 0 in-process
/// would poison parallel lib tests (their `os.open` could
/// then get fd 0 and hit the stdio arms).
#[test]
fn g6a_close_stdin_ebadf() {
    let dir = scratch("close_stdin");
    // Any readable file gives a valid fd; /dev/null does not
    // exist on Windows, so open the script itself.
    let path = dir.join("t.py").to_string_lossy().replace('\\', "/");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import os\nos.close(0)\ntry:\n    os.read(0, 5)\nexcept OSError as e:\n    print('read 0:', e.args[0])\nfd = os.open({path:?}, os.O_RDONLY)\nprint('reused fd:', fd)\n",
        ),
    );
    let r = run_script(&script, None, None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"read 0: 9\nreused fd: 0\n");
    assert!(r.stderr.is_empty());
}

/// `os.close(1)` frees fd 1 for reuse (G6a U3): the next
/// `os.open` gets fd 1. The result is reported through a file
/// (stdout is /dev/null by then, by construction).
#[test]
fn g6a_close_stdio_reuses_fd() {
    let dir = scratch("close_stdio");
    let out_path = dir.join("reused.txt");
    // /dev/null does not exist on Windows: open the script
    // itself. Backslashes in the interpolated paths are
    // escapes inside the petitpy string literals — use
    // forward slashes.
    let self_path = dir.join("t.py").to_string_lossy().replace('\\', "/");
    let out = out_path.to_string_lossy().replace('\\', "/");
    let src = format!(
        "import os\nos.close(1)\nfd = os.open('{self_path}', os.O_RDONLY)\n\
         f = open('{out}', 'w')\nf.write('reused fd: %d\\n' % fd)\nf.close()\n",
    );
    let script = write_script(&dir, "t.py", &src);
    let r = run_script(&script, None, None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert!(r.stdout.is_empty());
    assert!(r.stderr.is_empty());
    assert_eq!(
        std::fs::read_to_string(&out_path).unwrap(),
        "reused fd: 1\n"
    );
}

/// `os.close` on an `open()` file's fd (G6a U3).
#[test]
fn g6a_close_open_file_fd() {
    let dir = scratch("close_openfd");
    // /dev/null does not exist on Windows: open the script itself.
    let path = dir.join("t.py").to_string_lossy().replace('\\', "/");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "f = open({path:?}, 'r')\nimport os\nos.close(f.fileno())\nprint('os.close done')\n",
        ),
    );
    let r = run_script(&script, None, None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"os.close done\n");
    assert!(r.stderr.is_empty());
}

/// `os.fsync(1)` with stdout redirected to a regular file (G6a U9).
#[test]
fn g6a_fsync_stdout_file() {
    let dir = scratch("fsync_stdout");
    let out_path = dir.join("out.txt");
    let out_file = File::create(&out_path).unwrap();
    let script = write_script(&dir, "t.py", "import os\nos.fsync(1)\nprint('fsync ok')\n");
    let r = run_script(&script, None, Some(&out_file));
    drop(out_file);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert!(r.stderr.is_empty());
    assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "fsync ok\n");
}

/// `os.lseek` on stdin redirected from a regular file (G6a U9).
#[test]
fn g6a_lseek_stdin_file() {
    let dir = scratch("lseek_stdin");
    let in_path = dir.join("in.txt");
    std::fs::write(&in_path, b"0123456789").unwrap();
    let in_file = File::open(&in_path).unwrap();
    let script = write_script(
        &dir,
        "t.py",
        "import os\nprint(os.lseek(0, 0, 0))\nprint(os.lseek(0, 2, 0))\nprint(os.lseek(0, 0, 0))\n",
    );
    let r = run_script(&script, Some(&in_file), None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"0\n2\n0\n");
    assert!(r.stderr.is_empty());
}

/// `os.lseek(1, 0, 0)` with stdout redirected to a regular file (G6a U9).
#[test]
fn g6a_lseek_stdout_file() {
    let dir = scratch("lseek_stdout");
    let out_path = dir.join("out.txt");
    let out_file = File::create(&out_path).unwrap();
    let script = write_script(&dir, "t.py", "import os\nprint(os.lseek(1, 0, 0))\n");
    let r = run_script(&script, None, Some(&out_file));
    drop(out_file);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert!(r.stderr.is_empty());
    assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "0\n");
}

/// `os.umask` round-trip through the real syscall (G6a U2),
/// in a subprocess (the umask is process-global).
#[test]
fn g6a_umask_roundtrip() {
    let dir = scratch("umask");
    let script = write_script(
        &dir,
        "t.py",
        "import os\nprev = os.umask(0o022)\ncur = os.umask(prev)\nassert cur == 0o022\nprint('umask ok')\n",
    );
    let r = run_script(&script, None, None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, b"umask ok\n");
    assert!(r.stderr.is_empty());
}

/// Stale-fd semantics (G6a U3): after `os.close` on an
/// `open()` file's fd, the next `os.open` reuses the number
/// and a flush through the stale file object lands in the
/// wrong file — exactly like the reference (probed).
#[test]
fn g6a_stale_fd_reuse() {
    let dir = scratch("stale_fd");
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    // Backslashes in the interpolated paths are escapes
    // inside the petitpy string literals — use forward slashes.
    let a_path = a.to_string_lossy().replace('\\', "/");
    let b_path = b.to_string_lossy().replace('\\', "/");
    let src = format!(
        "import os\nf = open('{}', 'w')\nfd = f.fileno()\nos.close(fd)\n\
         g = os.open('{}', os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n\
         print('g fd', g)\nf.write('HELLO')\nf.flush()\nprint('flush ok')\nf.close()\nprint('close ok')\n\
         try:\n    os.close(g)\nexcept OSError as e:\n    print('close g:', e.args[0])\n",
        a_path,
        b_path
    );
    let script = write_script(&dir, "t.py", &src);
    let r = run_script(&script, None, None);
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(
        r.stdout,
        b"g fd 3\nflush ok\nclose ok\nclose g: 9\n",
        "stderr: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(r.stderr.is_empty());
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "HELLO");
}
