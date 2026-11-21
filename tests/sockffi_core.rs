//! Socket-FFI core tests (S1): the `SocketState::Ffi`
//! backend driven through the real binary (feature
//! `unsafe-ffi`). Every test writes a script, runs it as
//! a subprocess, and asserts the printed marker (plus
//! exit code 0 and empty stderr). The peer side is a Rust
//! `std` thread.
//!
//! `AF_INET` UDP and the connect-timeout branch are
//! `#[ignore]`d: this VM's sandbox returns `EPERM` for
//! UDP loopback `sendto` and for a connect to a
//! non-loopback address (verified with standalone `std`
//! probes). Run the ignored tests explicitly (`cargo test
//! --features unsafe-ffi -- --ignored`) on an
//! unsandboxed host.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

#![cfg(feature = "unsafe-ffi")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sockffi-{}-{}", std::process::id(), name));
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

/// The FFI socket has a real fd at construction, so
/// `fileno()` is stable across `bind()` (the pure-`std`
/// placeholder changes fd at `bind()`).
#[test]
fn sockffi_fileno_stable_across_bind() {
    let dir = scratch("fileno");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\nf0 = s.fileno()\ns.bind(('127.0.0.1', 0))\nf1 = s.fileno()\nassert f0 == f1\nassert f0 > 2\ns.close()\nprint('OK')\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
}

/// TCP roundtrip: the VM is the client, a Rust thread
/// the server. Exercises `connect` / `send` / `recv`
/// through the FFI backend.
#[test]
fn sockffi_tcp_roundtrip() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).expect("server reads");
        assert_eq!(&buf, b"hello");
        stream.write_all(b"world").expect("server writes");
    });
    let dir = scratch("tcp_roundtrip");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\ns.send(b'hello')\ndata = s.recv(10)\nassert data == b'world'\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// TCP accept: the VM is the server, a Rust thread the
/// client. The VM writes its ephemeral port to a file
/// (it binds port 0), the client polls the file, then
/// connects. Also exercises `getsockname()`.
#[test]
fn sockffi_tcp_accept() {
    let dir = scratch("tcp_accept");
    let port_file = dir.join("port.txt");
    let port_file_str = port_file.to_str().unwrap().to_string();
    // Windows paths carry backslashes, which are escape
    // sequences inside the generated Python string
    // literal (`\U` in `C:\Users\...` is a SyntaxError);
    // forward slashes work on every platform (same fix
    // as g6a_os / h_socket / sockffi_dns).
    let port_file_py = port_file.to_string_lossy().replace('\\', "/");
    let client = std::thread::spawn(move || {
        let mut port = None;
        for _ in 0..500 {
            if let Ok(t) = std::fs::read_to_string(&port_file_str) {
                if let Ok(p) = t.trim().parse::<u16>() {
                    port = Some(p);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let port = port.expect("VM wrote its port");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
        stream.write_all(b"ping").expect("client writes");
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).expect("client reads");
        assert_eq!(&buf, b"pong");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.bind(('127.0.0.1', 0))\ns.listen(1)\nport = s.getsockname()[1]\nf = open('{port_file}', 'w')\nf.write(str(port))\nf.close()\nconn, addr = s.accept()\nassert addr[0] == '127.0.0.1'\ndata = conn.recv(10)\nassert data == b'ping'\nconn.send(b'pong')\nconn.close()\ns.close()\nprint('OK')\n",
            port_file = port_file_py
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    client.join().expect("client thread");
}

/// `AF_UNIX` datagram roundtrip (the live datagram
/// path — `AF_INET` UDP is sandbox-blocked, see the
/// ignored test below). The VM is the server, a Rust
/// thread the client. The client BINDS its own path so
/// it has an address the VM can reply to (an unbound
/// peer is unnamed — see the test below).
#[cfg(unix)]
#[test]
fn sockffi_udp_unix_datagram() {
    use std::os::unix::net::UnixDatagram;
    let dir = scratch("unix_dgram");
    let path = dir.join("s.sock");
    let path_str = path.to_str().unwrap().to_string();
    let client_path = dir.join("c.sock");
    let client_path_str = client_path.to_str().unwrap().to_string();
    let server_path = path_str.clone();
    let client = std::thread::spawn(move || {
        let sock = UnixDatagram::bind(&client_path_str).expect("bind client");
        for _ in 0..200 {
            if std::path::Path::new(&server_path).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        sock.send_to(b"ping", &server_path).expect("client sends");
        let mut buf = [0u8; 4];
        let (n, _) = sock.recv_from(&mut buf).expect("client recvs");
        assert_eq!(&buf[..n], b"pong");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\ns.bind('{path}')\ndata, addr = s.recvfrom(10)\nassert data == b'ping'\ns.sendto(b'pong', addr)\ns.close()\nprint('OK')\n",
            path = path.to_str().unwrap()
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    client.join().expect("client thread");
}

/// `AF_UNIX` datagram from an UNBOUND peer: `recvfrom`
/// returns the data with an empty address (`b''`), not
/// an error (the peer is unnamed — `std` reports
/// `as_pathname() == None` there too).
#[cfg(unix)]
#[test]
fn sockffi_udp_unix_unnamed_peer() {
    use std::os::unix::net::UnixDatagram;
    let dir = scratch("unix_dgram_unnamed");
    let path = dir.join("s.sock");
    let path_str = path.to_str().unwrap().to_string();
    let client = std::thread::spawn(move || {
        let sock = UnixDatagram::unbound().expect("unbound");
        for _ in 0..200 {
            if std::path::Path::new(&path_str).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        sock.send_to(b"ping", &path_str).expect("client sends");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\ns.bind('{path}')\ndata, addr = s.recvfrom(10)\nassert data == b'ping'\nassert addr == b''\ns.close()\nprint('OK')\n",
            path = path.to_str().unwrap()
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    client.join().expect("client thread");
}

/// `AF_INET` UDP roundtrip. Ignored: the sandbox
/// returns `EPERM` for UDP loopback `sendto`.
#[test]
#[ignore]
fn sockffi_udp_inet() {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
    let port = sock.local_addr().expect("local addr").port();
    let peer = std::thread::spawn(move || {
        let mut buf = [0u8; 4];
        let (n, src) = sock.recv_from(&mut buf).expect("peer recvs");
        assert_eq!(&buf[..n], b"ping");
        sock.send_to(b"pong", src).expect("peer sends");
    });
    let dir = scratch("udp_inet");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\ns.sendto(b'ping', ('127.0.0.1', {port}))\ndata, addr = s.recvfrom(10)\nassert data == b'pong'\nassert addr[0] == '127.0.0.1'\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    peer.join().expect("peer thread");
}

/// `setsockopt` / `getsockopt` round-trips
/// (`SO_REUSEADDR`, `TCP_NODELAY`, `SO_RCVTIMEO` as a
/// 16-byte `timeval` for 1s). `getsockopt` needs a
/// `buflen` and returns bytes (petitpy has no int form).
/// The shadow is the source of truth, so the values come
/// back exactly.
#[test]
fn sockffi_setsockopt_getsockopt() {
    let dir = scratch("sockopt");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\nassert s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4) == b'\\x01\\x00\\x00\\x00'\ns.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)\nassert s.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 4) == b'\\x01\\x00\\x00\\x00'\ntv = b'\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00'\ns.setsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, tv)\nassert s.getsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, 16) == tv\ns.close()\nprint('OK')\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
}

/// A connect WITH a timeout set goes through
/// `connect_timeout` (nonblocking + `poll` + `SO_ERROR`
/// check) and succeeds against a listening server.
#[test]
fn sockffi_connect_with_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).expect("server reads");
        assert_eq!(&buf, b"ping");
    });
    let dir = scratch("connect_timeout");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.settimeout(5.0)\ns.connect(('127.0.0.1', {port}))\ns.send(b'ping')\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// A connect to a blackhole address times out with
/// `ETIMEDOUT` (110). Ignored: the sandbox returns
/// `EPERM` for any non-loopback connect, so this only
/// runs on an unsandboxed host.
#[test]
#[ignore]
fn sockffi_connect_timeout_blackhole() {
    let dir = scratch("connect_blackhole");
    let script = write_script(
        &dir,
        "t.py",
        "import socket, time\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.settimeout(1.0)\nt0 = time.time()\ntry:\n    s.connect(('10.255.255.1', 80))\n    print('CONNECTED')\nexcept OSError as e:\n    dt = time.time() - t0\n    assert e.args[0] == 110, e.args[0]\n    assert dt >= 0.9, dt\n    print('OK')\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
}

/// `getpeername` / `getsockname` and `shutdown`:
/// after `shutdown(SHUT_WR)` the peer sees EOF, so a
/// `recv` returns `b''`.
#[test]
fn sockffi_shutdown_getpeername_getsockname() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut buf = [0u8; 1];
        let n = stream.read(&mut buf).expect("server reads");
        assert_eq!(n, 0, "server sees EOF after SHUT_WR");
    });
    let dir = scratch("shutdown");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\npeer = s.getpeername()\nassert peer == ('127.0.0.1', {port})\nname = s.getsockname()\nassert name[0] == '127.0.0.1'\ns.shutdown(socket.SHUT_WR)\ndata = s.recv(10)\nassert data == b''\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// `dup()`: the duplicate is a new fd for the same
/// connection — it still sends after the original is
/// closed.
#[test]
fn sockffi_dup() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).expect("server reads");
        assert_eq!(&buf, b"hi");
    });
    let dir = scratch("dup");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\nd = s.dup()\nassert d.fileno() != s.fileno()\ns.close()\nd.send(b'hi')\nd.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}
