//! H-select binary probes: `select.select` end to end
//! through the built `petitpy-rs` binary (both builds —
//! this file has NO cfg gate; the Unix-only tests are
//! `#[cfg(unix)]` and the rest run everywhere).
//!
//! Every probe spawns the binary as a subprocess
//! (`CARGO_BIN_EXE_petitpy-rs`), writes a script, and
//! asserts exact stdout (so the default- and feature-build
//! outputs must be byte-identical — the result file's §3
//! diff is empty by construction). No unbounded waits:
//! every child wait and every harness open/connect has a
//! deadline and kills the child on timeout.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests (the
//! `g6b_socksub.rs` pattern).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("h-select-{}-{}", std::process::id(), name));
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

/// Spawn the binary on `script` (stdin null, stdout/stderr
/// piped) and wait for it — at most 15s, then kill it and
/// fail (never hang).
fn run_script(script: &Path) -> Run {
    let child = Command::new(bin_petitpy())
        .arg(script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = wait_child(child);
    Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    }
}

/// Wait for `child` (poll `try_wait`, 15s deadline), then
/// kill it and panic on timeout.
fn wait_child(mut child: Child) -> std::process::Output {
    let start = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(_) => return child.wait_with_output().unwrap(),
            None => {
                if start.elapsed() > Duration::from_secs(15) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("petitpy-rs did not exit within 15s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn assert_run(r: &Run, expected_stdout: &[u8]) {
    assert_eq!(r.code, 0, "rc != 0, stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, expected_stdout);
    assert!(r.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&r.stderr));
}

// ── FIFO probes (Unix only) ──────────────────────────────────────────
// A FIFO's read end only blocks (instead of returning EOF)
// while a writer holds the write end open, so the harness
// opens the write end in a helper thread (a blocking open
// rendezvouses with the script's blocking read-open) and
// hands the result back through a channel with a 5s
// deadline. The script's `os.open(O_RDONLY)` and the
// harness's blocking `open(O_WRONLY)` complete together —
// no race, no `ENXIO` retry loop (a non-blocking writer
// open can never succeed while the reader is still blocked
// inside its own open).

#[cfg(unix)]
fn mkfifo(path: &Path) {
    let ok = Command::new("mkfifo").arg(path).status().unwrap().success();
    assert!(ok, "mkfifo failed for {}", path.display());
}

/// Open `fifo` for writing (blocking) in a helper thread
/// and return the `File` through a channel, at most 5s.
/// On timeout the child (if given) is killed and the test
/// fails — the blocked thread is abandoned (it dies with
/// the test process).
#[cfg(unix)]
fn open_writer(fifo: &Path, child: &mut Child) -> std::fs::File {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<std::fs::File>();
    let path = fifo.to_path_buf();
    std::thread::spawn(move || {
        let f = std::fs::File::create(&path).unwrap();
        let _ = tx.send(f);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(f) => f,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("harness write-open did not complete within 5s");
        }
    }
}

/// Spawn the binary on `script`, open the FIFO for writing
/// in a helper thread, and run `with_writer` once the write
/// end is open (it writes `data`, or holds the end open).
/// The child is waited on (15s deadline, killed on
/// timeout) and its exact output returned.
#[cfg(unix)]
fn fifo_probe(dir: &Path, script_src: &str, with_writer: impl FnOnce(std::fs::File)) -> Run {
    let fifo = dir.join("fifo");
    mkfifo(&fifo);
    let script = write_script(dir, "t.py", script_src);
    let mut child = Command::new(bin_petitpy())
        .arg(&script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let writer = open_writer(&fifo, &mut child);
    // The write end is open (so the script's read-open has
    // completed and a writer is present); do the test's
    // write/hold, then wait for the child.
    with_writer(writer);
    let out = wait_child(child);
    Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    }
}

/// A FIFO with data: the monitor reads it ahead, `select`
/// reports ready, and the bytes arrive in order through
/// `os.read` (the drain path — no byte lost to the monitor).
#[test]
#[cfg(unix)]
fn h_select_fifo_data_ready() {
    use std::io::Write;
    let dir = scratch("fifo_data");
    let fifo = dir.join("fifo");
    let src = format!(
        "import os, select\n\
         fd = os.open({:?}, os.O_RDONLY)\n\
         r, _, _ = select.select([fd], [], [], 2)\n\
         print(len(r))\n\
         data = os.read(fd, 5)\n\
         print(len(data))\n\
         print(data == b'hello')\n\
         os.close(fd)\n",
        fifo.to_str().unwrap()
    );
    let r = fifo_probe(
        &dir,
        &src,
        |mut w| {
            w.write_all(b"hello").unwrap();
            drop(w);
        },
    );
    assert_run(&r, b"1\n5\nTrue\n");
}

/// A FIFO with a writer present but no data: the monitor's
/// read blocks, so `select` reports not-ready (timeout 0.2).
/// The writer end is held open for the whole probe —
/// without a writer the read would return EOF (ready).
#[test]
#[cfg(unix)]
fn h_select_fifo_no_data_not_ready() {
    let dir = scratch("fifo_nodata");
    let fifo = dir.join("fifo");
    mkfifo(&fifo);
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import os, select\n\
             fd = os.open({:?}, os.O_RDONLY)\n\
             r, _, _ = select.select([fd], [], [], 0.2)\n\
             print(len(r))\n\
             os.close(fd)\n",
            fifo.to_str().unwrap()
        ),
    );
    let mut child = Command::new(bin_petitpy())
        .arg(&script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Hold the write end open (no write) until the child
    // exits; drop it afterwards.
    let writer = open_writer(&fifo, &mut child);
    let out = wait_child(child);
    drop(writer);
    let r = Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    };
    assert_run(&r, b"0\n");
}

/// `timeout=None` blocks until the fd is ready (the data
/// arrives ~at once, so this returns promptly).
#[test]
#[cfg(unix)]
fn h_select_fifo_timeout_none_ready() {
    use std::io::Write;
    let dir = scratch("fifo_none");
    let fifo = dir.join("fifo");
    let src = format!(
        "import os, select\n\
         fd = os.open({:?}, os.O_RDONLY)\n\
         r, _, _ = select.select([fd], [], [], None)\n\
         print(len(r))\n\
         os.close(fd)\n",
        fifo.to_str().unwrap()
    );
    let r = fifo_probe(
        &dir,
        &src,
        |mut w| {
            w.write_all(b"hello").unwrap();
            drop(w);
        },
    );
    assert_run(&r, b"1\n");
}

// ── Socket probes (Unix only) ────────────────────────────────────────
// The harness connects after a 300ms sleep (the
// `modsocket_tests.rs` pattern), retrying the connect for
// up to 5s; the script blocks in `accept()` meanwhile.
// Sockets are NEVER ready in pure `std` (no stable peek,
// a monitor `recv` would consume bytes) — these probes pin
// that limitation: even with 5 bytes waiting, `select`
// reports not-ready, and the following `recv` proves the
// data was there.

#[cfg(unix)]
fn socket_probe(dir: &Path, script_src: &str, send_data: bool) -> Run {
    use std::os::unix::net::UnixStream;
    let sock_path = dir.join("sock");
    let script = write_script(dir, "t.py", script_src);
    let mut child = Command::new(bin_petitpy())
        .arg(&script)
        .env_remove("PYTHONPATH")
        .env_remove("PETITPY_LIB")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Let the script reach accept(), then connect (retry
    // up to 5s — the bind may not have happened yet).
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(&sock_path) {
            Ok(s) => break s,
            Err(_) => {
                if start.elapsed() > Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("harness connect did not succeed within 5s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    if send_data {
        use std::io::Write;
        stream.write_all(b"hello").unwrap();
    }
    let out = wait_child(child);
    drop(stream);
    Run {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code().unwrap_or(-1),
    }
}

/// An accepted socket WITH data waiting: `select` still
/// reports not-ready on the DEFAULT build (the pinned
/// limitation), and the following `recv` proves the 5 bytes
/// were there. Under `unsafe-ffi` (socket FFI lane S3) the
/// socket IS reported ready — see
/// `h_select_socket_with_data_ready` below.
#[test]
#[cfg(all(unix, not(feature = "unsafe-ffi")))]
fn h_select_socket_with_data_not_ready() {
    let dir = scratch("sock_data");
    let sock = dir.join("sock");
    let src = format!(
        "import socket, select, os\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({:?}); s.listen(1)\n\
         c, _ = s.accept()\n\
         r, _, _ = select.select([c.fileno()], [], [], 0.5)\n\
         print(len(r))\n\
         data = c.recv(5)\n\
         print(len(data))\n\
         c.close(); s.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let r = socket_probe(&dir, &src, true);
    assert_run(&r, b"0\n5\n");
}

/// An accepted socket WITH data waiting: under `unsafe-ffi`
/// (socket FFI lane S3) `select` reports it ready — the
/// `poll(2)` probe in `modsocket::socket_readable` is
/// non-destructive, so the following `recv` still gets the
/// 5 bytes.
#[test]
#[cfg(all(unix, feature = "unsafe-ffi"))]
fn h_select_socket_with_data_ready() {
    let dir = scratch("sock_data_ready");
    let sock = dir.join("sock");
    let src = format!(
        "import socket, select, os\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({:?}); s.listen(1)\n\
         c, _ = s.accept()\n\
         r, _, _ = select.select([c.fileno()], [], [], 0.5)\n\
         print(len(r))\n\
         data = c.recv(5)\n\
         print(len(data))\n\
         c.close(); s.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let r = socket_probe(&dir, &src, true);
    assert_run(&r, b"1\n5\n");
}

/// An accepted socket with no data: not-ready as well.
#[test]
#[cfg(unix)]
fn h_select_socket_no_data_not_ready() {
    let dir = scratch("sock_nodata");
    let sock = dir.join("sock");
    let src = format!(
        "import socket, select, os\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({:?}); s.listen(1)\n\
         c, _ = s.accept()\n\
         r, _, _ = select.select([c], [], [], 0.2)\n\
         print(len(r))\n\
         c.close(); s.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let r = socket_probe(&dir, &src, false);
    assert_run(&r, b"0\n");
}

// ── Cross-platform probes ────────────────────────────────────────────

/// A bad fd (-1) raises `OSError(EBADF)`, on both builds.
#[test]
fn h_select_bad_fd_minus_one() {
    let dir = scratch("badfd");
    let script = write_script(
        &dir,
        "t.py",
        // No `\` line continuations here: a trailing `\`
        // strips the next line's leading whitespace, which
        // would eat the `try:` block's indentation.
        "import select\ntry:\n    select.select([-1], [], [], 0)\n    print('no raise')\nexcept OSError as e:\n    print(e.args[0])\n",
    );
    let r = run_script(&script);
    assert_run(&r, b"9\n");
}

/// A bad fd in the wlist / xlist is validated too (both
/// raise `EBADF` — the lists are only "always ready" /
/// "always empty" for resolvable fds).
#[test]
fn h_select_bad_fd_wlist_xlist() {
    let dir = scratch("badfd_wx");
    let script = write_script(
        &dir,
        "t.py",
        // No `\` line continuations: they would eat the
        // `try:` blocks' indentation (see above).
        "import select\ntry:\n    select.select([], [9999], [], 0)\n    print('w no raise')\nexcept OSError as e:\n    print(e.args[0])\ntry:\n    select.select([], [], [9999], 0)\n    print('x no raise')\nexcept OSError as e:\n    print(e.args[0])\n",
    );
    let r = run_script(&script);
    assert_run(&r, b"9\n9\n");
}

/// `/dev/null` (Unix): ready at once (EOF), the wlist
/// reports it, the xlist stays empty — and the result
/// keeps the ORIGINAL file object (`r[0] is f`).
#[test]
#[cfg(unix)]
fn h_select_devnull_file_object_identity() {
    let dir = scratch("devnull_ident");
    let script = write_script(
        &dir,
        "t.py",
        "import select\n\
         f = open('/dev/null', 'rb')\n\
         r, w, x = select.select([f], [f], [f], 0)\n\
         print(len(r))\n\
         print(len(w))\n\
         print(len(x))\n\
         print(r[0] is f)\n\
         f.close()\n",
    );
    let r = run_script(&script);
    assert_run(&r, b"1\n1\n0\nTrue\n");
}

/// A fd >= 1024 is accepted (Python has no FD_SETSIZE cap)
/// and reported ready when it is at EOF.
#[test]
#[cfg(unix)]
fn h_select_high_fd_over_1024() {
    let dir = scratch("highfd");
    let script = write_script(
        &dir,
        "t.py",
        // No `\` line continuations: they would eat the
        // `while:` / `if:` / `for:` blocks' indentation.
        "import os, select\nfds = []\nwhile True:\n    fd = os.open('/dev/null', os.O_RDONLY)\n    if fd >= 1024:\n        break\n    fds.append(fd)\nr, _, _ = select.select([fd], [], [], 0)\nprint(len(r))\nfor f in fds:\n    os.close(f)\nos.close(fd)\n",
    );
    let r = run_script(&script);
    assert_run(&r, b"1\n");
}

/// Sanity: `/dev/null` ready, a listener never ready (even
/// with a 0.2s timeout and no connection at all).
#[test]
#[cfg(unix)]
fn h_select_devnull_and_listener_sanity() {
    let dir = scratch("sanity");
    let sock = dir.join("sock");
    let src = format!(
        "import os, socket, select\n\
         fd = os.open('/dev/null', os.O_RDONLY)\n\
         r, _, _ = select.select([fd], [], [], 0)\n\
         print(len(r))\n\
         os.close(fd)\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({:?}); s.listen(1)\n\
         r, _, _ = select.select([s.fileno()], [], [], 0.2)\n\
         print(len(r))\n\
         s.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let script = write_script(&dir, "t.py", &src);
    let r = run_script(&script);
    assert_run(&r, b"1\n0\n");
}
