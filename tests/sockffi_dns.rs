//! S2 (socket FFI: DNS / service / address tools)
//! integration tests: the new `socket` module functions
//! driven through the real binary, with the
//! `unsafe-ffi` feature on. Every test writes a script,
//! runs it as a subprocess, and asserts the printed
//! marker (plus exit code 0 and empty stderr). The peer
//! side is a Rust `std` thread where a connection is
//! needed. Localhost / 127.0.0.1 / ::1 ONLY — no
//! external network (the sandbox has none).
//!
//! Binaries are located through the `CARGO_BIN_EXE_*`
//! variables Cargo sets for integration tests.

#![cfg(feature = "unsafe-ffi")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin_petitpy() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_petitpy-rs"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sockffi-dns-{}-{}", std::process::id(), name));
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

/// Forward DNS: `gethostbyname`, `gethostbyname_ex`,
/// and the FFI `getaddrinfo` path (5-tuples, canonname
/// `''`, type echoed, proto `0` — the pinned shape).
#[test]
fn sockffi_dns_localhost_forward() {
    let dir = scratch("forward");
    let script = write_script(
        &dir,
        "fwd.py",
        r#"
import socket
ip = socket.gethostbyname("localhost")
assert ip == "127.0.0.1" or ip == "::1", ip
name, aliases, ips = socket.gethostbyname_ex("localhost")
assert len(ips) > 0, ips
assert "127.0.0.1" in ips or "::1" in ips, ips
assert aliases == [], aliases
infos = socket.getaddrinfo("localhost", 80)
assert len(infos) > 0, infos
fam, typ, proto, canon, addr = infos[0]
assert fam == socket.AF_INET or fam == socket.AF_INET6, fam
assert typ == socket.SOCK_STREAM, typ
assert proto == 0, proto
assert canon == "", canon
assert addr[1] == 80, addr
infos = socket.getaddrinfo("127.0.0.1", "http")
assert infos[0][0] == socket.AF_INET, infos[0]
assert infos[0][4] == ("127.0.0.1", 80), infos[0]
infos = socket.getaddrinfo(None, 0)
assert len(infos) > 0, infos
print("DNS_FORWARD_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_FORWARD_OK\n");
}

/// Reverse DNS: `gethostbyaddr` and `getnameinfo`
/// (numeric form is deterministic; the hostname form
/// only needs to be non-empty — it comes from
/// /etc/hosts, which maps 127.0.0.1 to `localhost`).
#[test]
fn sockffi_dns_reverse() {
    let dir = scratch("reverse");
    let script = write_script(
        &dir,
        "rev.py",
        r#"
import socket
name, aliases, ips = socket.gethostbyaddr("127.0.0.1")
assert len(name) > 0, name
assert aliases == [], aliases
assert ips == ["127.0.0.1"], ips
host, serv = socket.getnameinfo(
    ("127.0.0.1", 80),
    socket.NI_NUMERICHOST | socket.NI_NUMERICSERV,
)
assert host == "127.0.0.1", host
assert serv == "80", serv
host, serv = socket.getnameinfo(("127.0.0.1", 80), 0)
assert len(host) > 0, host
assert serv == "http", serv
print("DNS_REVERSE_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_REVERSE_OK\n");
}

/// `gethostname` / `getfqdn` are non-empty strings.
#[test]
fn sockffi_dns_gethostname_fqdn() {
    let dir = scratch("hostname");
    let script = write_script(
        &dir,
        "host.py",
        r#"
import socket
name = socket.gethostname()
assert len(name) > 0, name
fqdn = socket.getfqdn()
assert len(fqdn) > 0, fqdn
assert len(socket.getfqdn("")) > 0
print("DNS_HOSTNAME_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_HOSTNAME_OK\n");
}

/// Service names: `getservbyname` / `getservbyport`
/// (FFI first, then the built-in table).
#[test]
fn sockffi_dns_service() {
    let dir = scratch("service");
    let script = write_script(
        &dir,
        "svc.py",
        r#"
import socket
assert socket.getservbyname("http") == 80
assert socket.getservbyname("http", "tcp") == 80
assert socket.getservbyname("http", "udp") == 80
assert socket.getservbyname("ssh") == 22
assert socket.getservbyname("domain", "udp") == 53
assert socket.getservbyport(80) == "http"
assert socket.getservbyport(80, "tcp") == "http"
assert socket.getservbyport(80, "udp") == "http"
assert socket.getservbyport(22) == "ssh"
print("DNS_SERVICE_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_SERVICE_OK\n");
}

/// Address tools: `inet_pton` / `inet_ntop` /
/// `inet_aton` / `inet_ntoa` roundtrips (v4 + v6,
/// including the `inet_aton` short form `127.1`).
#[test]
fn sockffi_dns_inet() {
    let dir = scratch("inet");
    let script = write_script(
        &dir,
        "inet.py",
        r#"
import socket
p = socket.inet_pton(socket.AF_INET, "127.0.0.1")
assert p == b"\x7f\x00\x00\x01", p
assert socket.inet_ntop(socket.AF_INET, p) == "127.0.0.1"
p6 = socket.inet_pton(socket.AF_INET6, "::1")
assert p6 == b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01", p6
assert socket.inet_ntop(socket.AF_INET6, p6) == "::1"
assert socket.inet_aton("127.0.0.1") == b"\x7f\x00\x00\x01"
assert socket.inet_aton("127.1") == b"\x7f\x00\x00\x01"
assert socket.inet_aton("0x7f000001") == b"\x7f\x00\x00\x01"
assert socket.inet_ntoa(b"\x7f\x00\x00\x01") == "127.0.0.1"
assert socket.inet_ntoa(socket.inet_aton("10.20.30.40")) == "10.20.30.40"
print("DNS_INET_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_INET_OK\n");
}

/// Byte-order conversions are pure-Rust swaps: the
/// roundtrip is endian-independent (no absolute value
/// is asserted, so this also passes on big-endian).
#[test]
fn sockffi_dns_hton() {
    let dir = scratch("hton");
    let script = write_script(
        &dir,
        "hton.py",
        r#"
import socket
assert socket.htonl(socket.ntohl(0x12345678)) == 0x12345678
assert socket.ntohl(socket.htonl(0x12345678)) == 0x12345678
assert socket.htons(socket.ntohs(0x1234)) == 0x1234
assert socket.ntohs(socket.htons(0x1234)) == 0x1234
assert socket.htonl(0) == 0
assert socket.htons(0) == 0
assert socket.htonl(socket.ntohl(1)) == 1
print("DNS_HTON_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_HTON_OK\n");
}

/// `socketpair`: a connected pair, data flows both
/// ways, then both ends close.
#[test]
fn sockffi_dns_socketpair() {
    let dir = scratch("socketpair");
    let script = write_script(
        &dir,
        "pair.py",
        r#"
import socket
s1, s2 = socket.socketpair()
s1.sendall(b"hello")
assert s2.recv(5) == b"hello"
s2.sendall(b"world")
assert s1.recv(5) == b"world"
s1.sendall(b"0123456789")
assert s2.recv(10) == b"0123456789"
s1.close()
s2.close()
print("DNS_SOCKETPAIR_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_SOCKETPAIR_OK\n");
}

/// `create_connection` to a local Rust listener: the
/// VM is the client, a Rust thread the server.
#[test]
fn sockffi_dns_create_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("tcp accept");
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).expect("read ping");
        assert_eq!(&buf, b"ping");
        stream.write_all(b"pong").expect("write pong");
    });
    let dir = scratch("create-connection");
    let script = write_script(
        &dir,
        "conn.py",
        &format!(
            r#"
import socket
s = socket.create_connection(("127.0.0.1", {port}))
assert s.getpeername() == ("127.0.0.1", {port}), s.getpeername()
host, p = s.getsockname()
assert host == "127.0.0.1", host
assert p > 0, p
s.sendall(b"ping")
assert s.recv(4) == b"pong"
s.close()
print("DNS_CREATE_CONNECTION_OK")
"#
        ),
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_CREATE_CONNECTION_OK\n");
    server.join().expect("server thread");
}

/// `create_server` accepts a connection from a Rust
/// thread. The server is in the VM, so the port is
/// learned through a file the script writes before
/// `accept` (the script is spawned, not run to
/// completion, so the test can connect while it runs).
#[test]
fn sockffi_dns_create_server() {
    let dir = scratch("create-server");
    let port_file = dir.join("port.txt");
    let port_path = port_file.to_string_lossy().replace('\\', "/");
    // Build #28: the child records its shadow timeout
    // here before `accept`; the fail-fast panic below
    // prints it, so a child-side failure says whether
    // `setblocking(True)` took effect at the Python level
    // (None) or not (0.0). A file, not stdout — the
    // success path asserts the child's stdout exactly.
    let diag_file = dir.join("diag.txt");
    let diag_path = diag_file.to_string_lossy().replace('\\', "/");
    let script = write_script(
        &dir,
        "srv.py",
        &format!(
            r#"
import socket
srv = socket.create_server(("127.0.0.1", 0))
port = srv.getsockname()[1]
f = open("{port_path}", "w")
f.write(str(port))
f.close()
# `create_server` returns a non-blocking socket (the
# CPython default), so `accept` would raise before the
# client connects; make it blocking for the wait (the
# non-blocking default itself is what is under test
# above — the server was created non-blocking).
srv.setblocking(True)
d = open("{diag_path}", "w")
d.write("gettimeout after setblocking(True): " + str(srv.gettimeout()))
d.close()
conn, addr = srv.accept()
data = conn.recv(5)
assert data == b"hello", data
conn.sendall(b"world")
conn.close()
srv.close()
print("DNS_CREATE_SERVER_OK")
"#
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
        .expect("spawn petitpy");
    // The script writes the port file before `accept`.
    let mut port: Option<u16> = None;
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            if let Ok(p) = s.trim().parse::<u16>() {
                port = Some(p);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let port = port.expect("server did not write the port file");
    // The server writes the port file before `setblocking`
    // + `accept`, so a single connect can race the
    // listener setup on a slow runner (the same class as
    // the Build #22 `h_select_listener_never_ready` race):
    // retry the connect (500×10ms, the 72eef1f pattern
    // widened in Build #26 — the 200×10ms budget expired
    // on macos-aarch64 with plain ECONNREFUSED), then
    // fail with the last error. If the child has exited
    // meanwhile, the listener is gone for good: fail fast
    // with the child's output instead, so a child-side
    // failure (e.g. `setblocking` / `accept` raising)
    // does not masquerade as a refused connection.
    let mut stream = None;
    let mut last_err = None;
    for _ in 0..500 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if let Ok(Some(status)) = child.try_wait() {
                    let out = child.wait_with_output().expect("wait for petitpy");
                    let diag = std::fs::read_to_string(&diag_file).unwrap_or_default();
                    panic!(
                        "server child exited ({}) before accept; diag: {}; stdout: {}; stderr: {}",
                        status,
                        diag,
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let mut stream = match stream {
        Some(s) => s,
        None => panic!("connect to server: {:?}", last_err),
    };
    stream.write_all(b"hello").expect("write hello");
    let mut buf = [0u8; 5];
    stream.read_exact(&mut buf).expect("read world");
    assert_eq!(&buf, b"world");
    let out = child.wait_with_output().expect("wait for petitpy");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"DNS_CREATE_SERVER_OK\n");
    assert!(out.stderr.is_empty());
}

/// `getdefaulttimeout` / `setdefaulttimeout`: the
/// module default starts as `None` and is applied to
/// new sockets (a socket created after setting it
/// inherits the timeout).
#[test]
fn sockffi_dns_default_timeout() {
    let dir = scratch("default-timeout");
    let script = write_script(
        &dir,
        "dt.py",
        r#"
import socket
assert socket.getdefaulttimeout() is None
socket.setdefaulttimeout(5.0)
assert socket.getdefaulttimeout() == 5.0
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
assert s.gettimeout() == 5.0, s.gettimeout()
s.close()
socket.setdefaulttimeout(None)
assert socket.getdefaulttimeout() is None
s2 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
assert s2.gettimeout() is None
s2.close()
print("DNS_DEFAULT_TIMEOUT_OK")
"#,
    );
    let r = run_script(&script);
    assert_ok(&r, b"DNS_DEFAULT_TIMEOUT_OK\n");
}
