//! S3 — socket FFI advanced integration tests: `sendmsg` /
//! `recvmsg`, `fromfd`, `detach`, and the `recvmsg`
//! ancillary rejection, driven through the real binary.
//! Feature-gated (`unsafe-ffi`): the default build has none
//! of these methods (they raise `NotImplementedError` / do
//! not exist there).
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
    let dir = std::env::temp_dir().join(format!("sockffi-adv-{}-{}", std::process::id(), name));
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

fn assert_ok(r: &Run, marker: &[u8]) {
    assert_eq!(r.code, 0, "stderr: {}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.stdout, marker);
    assert!(r.stderr.is_empty());
}

/// `sendmsg` gathers several buffers; `recvmsg` scatters
/// them back, on an `AF_UNIX` stream pair (listener +
/// client + accepted, all in the script).
#[test]
#[cfg(unix)]
fn sockffi_adv_sendmsg_recvmsg_unix_stream() {
    let dir = scratch("msg_stream");
    let sock = dir.join("sock");
    let src = format!(
        "import socket, os\n\
         s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s.bind({:?}); s.listen(1)\n\
         c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         c.connect({:?})\n\
         a, _ = s.accept()\n\
         n = c.sendmsg([b'he', b'llo'])\n\
         data, anc, flags, addr = a.recvmsg(5)\n\
         print(n, len(data), data == b'hello', anc, flags)\n\
         n2 = a.sendmsg([b'ok'])\n\
         data2, anc2, flags2, addr2 = c.recvmsg(2)\n\
         print(n2, len(data2), data2 == b'ok', anc2, flags2)\n\
         a.close(); c.close(); s.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let script = write_script(&dir, "t.py", &src);
    let r = run_script(&script);
    assert_ok(&r, b"5 5 True [] 0\n2 2 True [] 0\n");
}

/// `sendmsg` with an explicit destination address, and
/// `recvmsg` reporting the sender, on `AF_UNIX` datagrams.
#[test]
#[cfg(unix)]
fn sockffi_adv_sendmsg_recvmsg_unix_datagram() {
    let dir = scratch("msg_dgram");
    let sock = dir.join("sock");
    let src = format!(
        "import socket, os\n\
         srv = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\n\
         srv.bind({:?})\n\
         c = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\n\
         n = c.sendmsg([b'hi'], 0, {:?})\n\
         data, anc, flags, addr = srv.recvmsg(2)\n\
         print(n, len(data), data == b'hi', anc, flags)\n\
         srv.close(); c.close(); os.remove({:?})\n",
        sock.to_str().unwrap(),
        sock.to_str().unwrap(),
        sock.to_str().unwrap()
    );
    let script = write_script(&dir, "t.py", &src);
    let r = run_script(&script);
    assert_ok(&r, b"2 2 True [] 0\n");
}

/// `fromfd` wraps a caller-owned fd as-is (no dup — the
/// integration test `socket_fromfd_wraps_fd` requires
/// `fileno() == fd`, even for a non-socket fd).
#[test]
#[cfg(unix)]
fn sockffi_adv_fromfd_wraps_fd() {
    let dir = scratch("fromfd");
    let script = write_script(
        &dir,
        "t.py",
        "import socket, os\n\
         fd = os.open('/dev/null', os.O_RDONLY)\n\
         s = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\n\
         print(s.fileno() == fd)\n\
         s.close()\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"True\n");
}

/// `fromfd` on an invalid fd raises `OSError(EBADF)`.
#[test]
#[cfg(unix)]
fn sockffi_adv_fromfd_invalid_fd() {
    let dir = scratch("fromfd_bad");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ntry:\n    s = socket.fromfd(99999, socket.AF_INET, socket.SOCK_STREAM)\n    print('no raise')\nexcept OSError as e:\n    print(e.args[0])\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"9\n");
}

/// `detach` removes the socket from the table WITHOUT
/// closing it: the instance's fd becomes -1 and the
/// returned fd stays open (reclaimed here with
/// `os.close`, which closes by number).
#[test]
#[cfg(unix)]
fn sockffi_adv_detach() {
    let dir = scratch("detach");
    let script = write_script(
        &dir,
        "t.py",
        "import socket, os\n\
         s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\n\
         fd = s.detach()\n\
         print(s.fileno(), fd > 0)\n\
         os.close(fd)\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"-1 True\n");
}

/// `recvmsg` with a non-zero ancillary buffer size is
/// `NotImplementedError` (never silently dropped).
#[test]
fn sockffi_adv_recvmsg_ancillary_not_implemented() {
    let dir = scratch("recvmsg_anc");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\ntry:\n    s.recvmsg(10, 100)\n    print('no raise')\nexcept NotImplementedError:\n    print('NotImplementedError')\ns.close()\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"NotImplementedError\n");
}
