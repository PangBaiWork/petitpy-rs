//! H-socket integration tests: the new pure-`std` `socket`
//! module driven through the real binary. Every test writes
//! a script, runs it as a subprocess, and asserts the
//! printed marker (plus exit code 0 and empty stderr).
//! The peer side is a Rust `std` thread.
//!
//! `AF_INET` UDP is NOT tested here: this VM's sandbox
//! returns `EPERM` for UDP loopback `sendto` (verified
//! with a standalone `std` probe); the datagram path is
//! covered by `h_socket_unix_datagram` (`AF_UNIX`) and by
//! the ignored `h_socket_udp_inet` below. Run the ignored
//! test explicitly (`cargo test -- --ignored`) on an
//! unsandboxed host.
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("h-socket-{}-{}", std::process::id(), name));
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

/// TCP echo + a large `sendall` (100_000 bytes — far past
/// the old 4096 cap): the VM is the client, a Rust thread
/// the server.
#[test]
fn h_socket_tcp_echo_large() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut got = 0usize;
        let mut buf = [0u8; 8192];
        while got < 100_000 {
            let n = stream.read(&mut buf).expect("server reads");
            assert!(n > 0, "server hit EOF at {got} bytes");
            got += n;
        }
        assert_eq!(got, 100_000);
        stream.write_all(b"ok").expect("server writes");
    });
    let dir = scratch("tcp_echo_large");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\ns.sendall(b'x' * 100000)\ndata = s.recv(10)\nassert data == b'ok'\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// `AF_UNIX` echo: the VM is the server, a Rust thread
/// the client.
#[cfg(unix)]
#[test]
fn h_socket_unix_echo() {
    use std::os::unix::net::UnixStream;
    let dir = scratch("unix_echo");
    let path = dir.join("s.sock");
    let path_str = path.to_str().unwrap().to_string();
    let path_owned = path_str.clone();
    let client = std::thread::spawn(move || {
        let mut stream = None;
        for _ in 0..200 {
            match UnixStream::connect(&path_owned) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("client connects");
        stream.write_all(b"hello").expect("client writes");
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).expect("client reads");
        assert_eq!(&buf, b"world");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path_str}')\ns.listen(1)\nclient, addr = s.accept()\nassert addr == b''\ndata = client.recv(100)\nassert data == b'hello'\nclient.sendall(b'world')\nclient.close()\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    client.join().expect("client thread");
}

/// `AF_UNIX` datagram `sendto` / `recvfrom`: the VM binds
/// a datagram socket and exchanges one datagram each way
/// with a Rust `UnixDatagram`; the reply's address is the
/// Rust socket's path bytes (the sender's address).
#[cfg(unix)]
#[test]
fn h_socket_unix_datagram() {
    use std::os::unix::net::UnixDatagram;
    let dir = scratch("unix_datagram");
    let server_path = dir.join("server.sock");
    let client_path = dir.join("client.sock");
    let server_str = server_path.to_str().unwrap().to_string();
    let client_str = client_path.to_str().unwrap().to_string();
    let sock = UnixDatagram::bind(&server_path).expect("udp bind");
    let server = std::thread::spawn(move || {
        let mut buf = [0u8; 100];
        let (n, src) = sock.recv_from(&mut buf).expect("udp recv");
        assert_eq!(&buf[..n], b"ping");
        sock.send_to(b"pong", src.as_pathname().unwrap())
            .expect("udp send");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\ns.bind(b'{client_str}')\nn = s.sendto(b'ping', b'{server_str}')\nassert n == 4\ndata, addr = s.recvfrom(100)\nassert data == b'pong'\nassert addr == b'{server_str}'\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// `AF_INET` UDP `sendto` / `recvfrom`. IGNORED — see the
/// file header (sandbox `EPERM` on UDP loopback).
#[ignore]
#[test]
fn h_socket_udp_inet() {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
    let port = sock.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let mut buf = [0u8; 100];
        let (n, src) = sock.recv_from(&mut buf).expect("udp recv");
        assert_eq!(&buf[..n], b"ping");
        sock.send_to(b"pong", src).expect("udp send");
    });
    let dir = scratch("udp_inet");
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\nn = s.sendto(b'ping', ('127.0.0.1', {port}))\nassert n == 4\ndata, addr = s.recvfrom(100)\nassert data == b'pong'\nassert addr[0] == '127.0.0.1'\nassert addr[1] == {port}\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    server.join().expect("server thread");
}

/// `getaddrinfo`: 5-tuples, empty `canonname`, int and
/// service-name ports, `None` host, bad family, unknown
/// service.
#[test]
fn h_socket_getaddrinfo() {
    let dir = scratch("getaddrinfo");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ninfos = socket.getaddrinfo('localhost', 80)\nassert len(infos) > 0\nfam, typ, proto, canon, addr = infos[0]\nassert fam == socket.AF_INET or fam == socket.AF_INET6\nassert typ == socket.SOCK_STREAM\nassert proto == 0\nassert canon == ''\nassert addr[1] == 80\ninfos = socket.getaddrinfo('127.0.0.1', 'http')\nassert infos[0][0] == socket.AF_INET\nassert infos[0][4] == ('127.0.0.1', 80)\ninfos = socket.getaddrinfo(None, 0)\nassert len(infos) > 0\ntry:\n    socket.getaddrinfo('localhost', 80, 999)\n    print('BAD FAMILY: no error')\nexcept OSError as e:\n    assert e.args[0] == 97\ntry:\n    socket.getaddrinfo('localhost', 'nosuchservice')\n    print('BAD SERVICE: no error')\nexcept OSError as e:\n    assert e.args[0] == -2\nprint('OK')\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
}

/// `settimeout` / `gettimeout` round-trip and the
/// `getsockopt` / `setsockopt` shadow state, through the
/// real binary.
#[test]
fn h_socket_timeout_sockopt() {
    let dir = scratch("timeout_sockopt");
    let script = write_script(
        &dir,
        "t.py",
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\nassert s.gettimeout() is None\ns.settimeout(1.5)\nassert s.gettimeout() == 1.5\ns.settimeout(0)\nassert s.gettimeout() == 0.0\ns.setblocking(True)\nassert s.gettimeout() is None\nassert s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4) == b'\\x00\\x00\\x00\\x00'\nassert s.getsockopt(socket.IPPROTO_IP, socket.IP_TTL, 4) == b'\\x40\\x00\\x00\\x00'\ns.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)\nassert s.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 4) == b'\\x01\\x00\\x00\\x00'\ntry:\n    s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)\n    print('PEERCRED: no error')\nexcept OSError as e:\n    assert e.args[0] == 92\ns.close()\nprint('OK')\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
}

/// `makefile('rwb')` + `dup()`: the VM is the server, a
/// Rust thread the client. The accepted socket is read
/// through a file object, replied to through it, then the
/// dup sends the last word.
#[cfg(unix)]
#[test]
fn h_socket_makefile_dup() {
    use std::os::unix::net::UnixStream;
    let dir = scratch("makefile_dup");
    let path = dir.join("s.sock");
    let path_str = path.to_str().unwrap().to_string();
    let path_owned = path_str.clone();
    let client = std::thread::spawn(move || {
        let mut stream = None;
        for _ in 0..200 {
            match UnixStream::connect(&path_owned) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("client connects");
        stream.write_all(b"hello\n").expect("client writes");
        let mut buf = [0u8; 6];
        stream.read_exact(&mut buf).expect("client reads 1");
        assert_eq!(&buf, b"world\n");
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).expect("client reads 2");
        assert_eq!(&buf, b"again");
    });
    let script = write_script(
        &dir,
        "t.py",
        &format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path_str}')\ns.listen(1)\nclient, addr = s.accept()\nf = client.makefile('rwb')\nline = f.readline()\nassert line == b'hello\\n'\nf.write(b'world\\n')\nf.flush()\nf.close()\nd = client.dup()\nassert d.fileno() != client.fileno()\nd.sendall(b'again')\nd.close()\nclient.close()\ns.close()\nprint('OK')\n"
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"OK\n");
    client.join().expect("client thread");
}

/// `fromfd` raises `NotImplementedError` on both builds.
#[test]
fn h_socket_fromfd() {
    let dir = scratch("fromfd");
    let script = write_script(
        &dir,
        "t.py",
        "import socket, os\nfd = os.open('/dev/null', os.O_RDONLY)\ntry:\n    s = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\n    print('fromfd: no error')\nexcept NotImplementedError as e:\n    print('fromfd:', e.args[0])\n",
    );
    let r = run_script(&script);
    assert_ok(&r, b"fromfd: socket.fromfd is not implemented\n");
}
