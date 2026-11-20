//! Phase H subprocess probes, run against the real built
//! `petitpy-rs` binary. Every probe runs on BOTH builds
//! (default and `--features unsafe-ffi`) and the two runs
//! must agree exactly — Phase H deletes the feature
//! branches (U5 SIGTERM+grace, U8 `pre_exec` dup2), so
//! both builds run the same code here.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests. Unix-only:
//! the probes run `/bin/cat`, `/bin/echo`, `/bin/sh`,
//! `/bin/sleep` and pass real fds across `exec`.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::io::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("h-sub-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn output_to_run(out: Output) -> Run {
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// Run `petitpy-rs -c CODE`, wait, return the output.
fn run_code(code: &str) -> Run {
    let out = Command::new(bin_petitpy())
        .args(["-c", code])
        .output()
        .expect("spawn petitpy-rs");
    output_to_run(out)
}

/// Run `petitpy-rs SCRIPT`, wait, return the output.
fn run_file(path: &Path, cwd: &Path) -> Run {
    let out = Command::new(bin_petitpy())
        .arg(path)
        .current_dir(cwd)
        .output()
        .expect("spawn petitpy-rs");
    output_to_run(out)
}

/// fd-mode stdin: a stream fd as stdin, the child reads
/// what the parent wrote. The stream end is passed as
/// the petitpy process's own stdin (fd 0) — `Command`
/// closes every other fd on `exec`, so a raw fd number
/// would not survive the spawn; the script then uses
/// `stdin=0`. (The peer is dropped after the write, so
/// the child sees EOF.)
#[test]
fn h_fd_stdin_stream() {
    let (mut peer, child_end) = UnixStream::pair().expect("pair");
    let stdin: Stdio = Stdio::from(
        child_end
            .as_fd()
            .try_clone_to_owned()
            .expect("dup stream"),
    );
    drop(child_end);
    let mut child = Command::new(bin_petitpy())
        .args([
            "-c",
            "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/cat\"], None, -1.0, None, None, 0, -1, -4)\nprint(r)",
        ])
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn petitpy-rs");
    peer.write_all(b"hello").expect("write peer");
    drop(peer);
    let out = child.wait_with_output().expect("wait");
    let run = output_to_run(out);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(0, b'hello', None, False)\n");
}

/// fd-mode stdout: a plain file fd as stdout, the child
/// output lands in the file. The fd comes from `os.open`
/// inside the script — the real end-to-end path (petitpy
/// os fd → `_spawn_and_wait` fd mode).
#[test]
fn h_fd_stdout_file() {
    let dir = scratch("stdout-file");
    let script = dir.join("t.py");
    std::fs::write(
        &script,
        "import os, _subprocess\nfd = os.open(\"out.txt\", os.O_WRONLY | os.O_CREAT | os.O_TRUNC)\nr = _subprocess._spawn_and_wait([\"/bin/echo\", \"hi\"], None, -1.0, None, None, -4, fd, -4)\nprint(r)\n",
    )
    .expect("write script");
    let run = run_file(&script, &dir);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(0, None, None, False)\n");
    assert_eq!(std::fs::read(dir.join("out.txt")).expect("read output"), b"hi\n");
}

/// fd-mode stdout, shared offset: the parent writes
/// "AAA" first (offset 3); the child appends after it —
/// the dup shares the offset, so the file is "AAABBB\n",
/// not overwritten from 0.
#[test]
fn h_fd_stdout_shared_offset() {
    let dir = scratch("offset");
    let script = dir.join("t.py");
    std::fs::write(
        &script,
        "import os, _subprocess\nfd = os.open(\"out.txt\", os.O_WRONLY | os.O_CREAT | os.O_TRUNC)\nos.write(fd, b\"AAA\")\nr = _subprocess._spawn_and_wait([\"/bin/sh\", \"-c\", \"echo BBB\"], None, -1.0, None, None, -4, fd, -4)\nprint(r)\n",
    )
    .expect("write script");
    let run = run_file(&script, &dir);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(0, None, None, False)\n");
    assert_eq!(std::fs::read(dir.join("out.txt")).expect("read output"), b"AAABBB\n");
}

/// fd-mode stdout to a SOCKET fd: the child output
/// arrives on the pair's peer. (The old default build
/// failed this with `ENXIO` on the `/proc` re-open.)
/// The socket end is passed as the petitpy process's own
/// stdin (fd 0) — see `h_fd_stdin_stream` — and the
/// script uses it as `stdout=0`.
#[test]
fn h_fd_socket() {
    let (mut peer, child_end) = UnixStream::pair().expect("pair");
    let stdin: Stdio = Stdio::from(
        child_end
            .as_fd()
            .try_clone_to_owned()
            .expect("dup stream"),
    );
    drop(child_end);
    let mut child = Command::new(bin_petitpy())
        .args([
            "-c",
            "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/echo\", \"hi\"], None, -1.0, None, None, -4, 0, -4)\nprint(r)",
        ])
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn petitpy-rs");
    let mut buf = Vec::new();
    peer.read_to_end(&mut buf).expect("read peer");
    let out = child.wait_with_output().expect("wait");
    let run = output_to_run(out);
    assert_eq!(buf, b"hi\n");
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(0, None, None, False)\n");
}

/// Timeout: `sleep 10` with `timeout=0.2` → timed out,
/// returncode -9 (`SIGKILL`, no grace) on both builds.
#[test]
fn h_timeout() {
    let start = Instant::now();
    let run = run_code(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/sleep\", \"10\"], None, 0.2, None, None, -4, -4, -4)\nprint(r)",
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(-9, None, None, True)\n");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "timed-out call took {:?}, expected well under 5s",
        start.elapsed()
    );
}

/// Spawn failure: a missing command reports 127 with
/// empty captured output.
#[test]
fn h_spawn_fail() {
    let run = run_code(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/nonexistent-cmd-xyz\"], None, -1.0, None, None, -4, -1, -1)\nprint(r)",
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "(127, b'', b'', False)\n");
}

/// An fd that does not resolve is `OSError(EBADF)`.
#[test]
fn h_fd_bad() {
    let run = run_code(
        "import _subprocess\nr = _subprocess._spawn_and_wait([\"/bin/echo\", \"hi\"], None, -1.0, None, None, -4, 9999, -4)\nprint(r)",
    );
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("OSError: [Errno 9] EBADF"), "stderr: {}", run.stderr);
}

/// The frozen `subprocess.py` surface end to end:
/// `run` + `capture_output`, `call`, `check_output`,
/// `check=True` → `CalledProcessError`, `timeout` →
/// `TimeoutExpired`, and a file object as stdout (its
/// `fileno()` goes through the fd mode).
#[test]
fn h_frozen_subprocess() {
    let dir = scratch("frozen");
    let script = dir.join("t.py");
    std::fs::write(
        &script,
        r#"import subprocess
r = subprocess.run(["/bin/echo", "hi"], capture_output=True)
print(r.returncode, r.stdout, r.stderr)
print(subprocess.call(["/bin/sh", "-c", "exit 0"]))
print(subprocess.check_output(["/bin/echo", "yo"]))
try:
    subprocess.run(["/bin/sh", "-c", "exit 1"], check=True)
except subprocess.CalledProcessError as e:
    print("CalledProcessError", e.returncode)
try:
    subprocess.run(["/bin/sleep", "5"], timeout=0.2)
except subprocess.TimeoutExpired as e:
    print("TimeoutExpired", e.timeout)
f = open("out2.txt", "w")
subprocess.run(["/bin/echo", "file"], stdout=f)
f.close()
print(open("out2.txt").read())
"#,
    )
    .expect("write script");
    let run = run_file(&script, &dir);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(
        run.stdout,
        "0 b'hi\\n' b''\n0\nb'yo\\n'\nCalledProcessError 1\nTimeoutExpired 0.2\nfile\n\n"
    );
}
