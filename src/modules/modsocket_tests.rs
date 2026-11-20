//! Tests for the `socket` module (`modsocket.rs`).
//!
//! The round-trip tests drive a real client from a Rust
//! `std` thread — the reference has no `connect` method,
//! so the client side is always the test harness. Socket
//! paths are unique per test and unlinked before and
//! after. TCP tests use a Rust `TcpListener` on
//! `127.0.0.1:0` and pass its port into the script (the
//! module has no `getsockname`).

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::modules::modsocket::build_socket_module;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// Build #20 macOS errnos (CI run 44764410645): Darwin
/// reports different errnos than Linux for the same
/// failing call, so the tests below take the value from
/// here. The Linux values are the pinned ones; the macOS
/// values are the Darwin errnos observed in Build #20.
/// - `recv` on an unconnected stream socket: Linux
///   `EINVAL` (22), macOS `ENOTCONN` (57).
/// - Second `connect` on a connected socket: Linux
///   `EISCONN` (106), macOS `EISCONN` (56).
/// - `send` / `sendall` on a listener: Linux `ENOTCONN`
///   (107), macOS `ENOTCONN` (57).
/// The other cases in `socket_bind_listen_errors` stay
/// 22 / 36 on both: they are validated module-side
/// (`sock_recv_ffi` rejects `n < 0`, `sock_getsockopt`
/// rejects a bad `buflen`, `parse_unix_path` rejects a
/// long path) — read from the code, not guessed.
///
/// Build #21: the macOS values above hold ONLY for the
/// `unsafe-ffi` build, where the kernel is the state
/// machine (a real `recv` / `connect` / `send` returns
/// the Darwin errno). The default build synthesizes the
/// Linux numbers on every platform (`io_handle` /
/// `sock_connect` raise the `modsocket.rs` constants),
/// so macOS default gave 22 / 106 / 107 and failed
/// against the macOS-only constants. Each constant is
/// therefore (platform × feature) aware — the same
/// pattern as `modd4_tests.rs` (`h_select_*`).
#[cfg(all(target_os = "macos", feature = "unsafe-ffi"))]
const ERR_RECV_UNCONN: &str = "57";
#[cfg(not(all(target_os = "macos", feature = "unsafe-ffi")))]
const ERR_RECV_UNCONN: &str = "22";
#[cfg(all(target_os = "macos", feature = "unsafe-ffi"))]
const ERR_EISCONN: &str = "56";
#[cfg(not(all(target_os = "macos", feature = "unsafe-ffi")))]
const ERR_EISCONN: &str = "106";
#[cfg(all(target_os = "macos", feature = "unsafe-ffi"))]
const ERR_ENOTCONN: &str = "57";
#[cfg(not(all(target_os = "macos", feature = "unsafe-ffi")))]
const ERR_ENOTCONN: &str = "107";

/// A fresh VM with `socket` registered, plus the read-back
/// helpers (the `objstringio_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_socket(&mut vm);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => self.raised_parts(exc),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn raised_parts(&mut self, exc: Value) -> (String, String) {
        let (name, arg) = match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => {
                    let t = d.type_;
                    let name = match self.vm.arena.get(t) {
                        Obj::Type(td) => String::from_utf8_lossy(
                            self.vm.pool.qstr_str(td.name as u16),
                        )
                        .into_owned(),
                        _ => panic!("exception type is not a type"),
                    };
                    (name, d.args.first().copied())
                }
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        };
        let msg = match arg {
            Some(v) => self.val_str(v),
            None => String::new(),
        };
        (name, msg)
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    fn gbytes(&mut self, name: &str) -> Vec<u8> {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Bytes(b) => b.bytes.clone(),
                other => panic!("global {name} is not bytes: {other:?}"),
            },
            Some(Value::Qstr(q)) => self.vm.pool.qstr_str(q as u16).to_vec(),
            other => panic!("global {name} is not bytes: {other:?}"),
        }
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String`.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                Obj::Bytes(b) => String::from_utf8_lossy(&b.bytes).into_owned(),
                other => panic!("value is not a str: {other:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::None => "None".to_string(),
        }
    }
}

fn setup_socket(vm: &mut Vm) -> GcRef {
    let fun_var = vm.state.vm_types.fun_builtin_var;
    let module_type = vm.state.vm_types.module;
    let (m, t, ft) = {
        let (mut ctx, state) = vm.split();
        let (m, t, ft) =
            build_socket_module(&mut ctx, fun_var, module_type).expect("socket builds");
        let id = static_qstr_id(b"socket").expect("socket qstr");
        state.builtin_modules.push((id, m));
        (m, t, ft)
    };
    vm.types.socket = Some(t);
    vm.types.socketfile = Some(ft);
    vm.pin(m);
    t
}

fn unlink(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// Spawn a Rust TCP server on `127.0.0.1:0`; returns
/// (port, thread handle). The thread accepts one
/// connection, runs `f` on it, then exits.
fn tcp_server<F>(f: F) -> (u16, std::thread::JoinHandle<()>)
where
    F: FnOnce(std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("tcp listen");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("tcp accept");
        f(stream);
    });
    (port, handle)
}

/// The `__del__` entry is the SAME `FunBuiltin` object as
/// `close` (`modsocket.zig:318`), checked at the Rust level
/// (a VM-level `is` would compare fresh bound methods).
#[test]
fn socket_close_is_del() {
    let mut vm = Vm::new();
    let t = setup_socket(&mut vm);
    let (locals, close_v, del_v) = {
        let (mut ctx, _state) = vm.split();
        let dict = match ctx.arena.get(t) {
            Obj::Type(d) => d.dict,
            _ => panic!("socket type is not a type"),
        };
        let dict_v = Value::Heap(dict);
        let close_id = static_qstr_id(b"close").expect("close qstr");
        let del_id = static_qstr_id(b"__del__").expect("__del__ qstr");
        let close_v = dict_get_value(&mut ctx, dict_v, Value::Qstr(close_id))
            .expect("close in locals");
        let del_v = dict_get_value(&mut ctx, dict_v, Value::Qstr(del_id))
            .expect("__del__ in locals");
        (dict_v, close_v, del_v)
    };
    let _ = locals;
    assert_eq!(close_v, del_v);
}

#[test]
fn socket_constants() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
assert socket.__name__ == 'socket'
assert socket.AF_UNSPEC == 0
assert socket.AF_UNIX == 1
assert socket.AF_INET == 2
assert socket.AF_INET6 == 10
assert socket.SOCK_STREAM == 1
assert socket.SOCK_DGRAM == 2
assert socket.SOL_SOCKET == 1
assert socket.SO_REUSEADDR == 2
assert socket.SO_BROADCAST == 6
assert socket.SO_RCVBUF == 8
assert socket.SO_SNDBUF == 7
assert socket.SO_RCVTIMEO == 20
assert socket.SO_SNDTIMEO == 21
assert socket.SO_PEERCRED == 17
assert socket.IPPROTO_IP == 0
assert socket.IPPROTO_TCP == 6
assert socket.IPPROTO_UDP == 17
assert socket.TCP_NODELAY == 1
assert socket.IP_TTL == 2
assert socket.SHUT_RD == 0
assert socket.SHUT_WR == 1
assert socket.SHUT_RDWR == 2
assert str(socket.socket) == '<function>'
assert str(socket.fromfd) == '<function>'
assert str(socket.getaddrinfo) == '<function>'
"#,
    );
}

#[test]
fn socket_ctor_and_type() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
assert s.fileno() >= 0
assert str(s) == '<socket>'
assert str(type(s)) == "<class 'socket'>"
assert s.fileno() == s.fileno()
s.close()
assert s.fileno() == -1
"#,
    );
}

/// The S2 DNS / service / address module functions are
/// shipped now (Lane S2): `gethostname` /
/// `gethostbyname` (the two this test used to pin as
/// absent) plus the rest of the S2 surface. Accessing
/// any of them must not raise `AttributeError`.
#[test]
fn socket_dns_shipped() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
socket.gethostname
socket.gethostbyname
socket.gethostbyname_ex
socket.gethostbyaddr
socket.getnameinfo
socket.getfqdn
socket.getservbyname
socket.getservbyport
socket.create_connection
socket.create_server
socket.socketpair
socket.getdefaulttimeout
socket.setdefaulttimeout
socket.htonl
socket.htons
socket.ntohl
socket.ntohs
socket.inet_aton
socket.inet_ntoa
socket.inet_pton
socket.inet_ntop
"#,
    );
}

#[test]
fn socket_bind_listen_errors() {
    let mut fx = Fx::new();
    // Non-str path, listen/recv before bind, bad sizes,
    // bad getsockopt buflens. (The old INET-bind case is
    // gone: INET bind works now. The old `recv(4097)` →
    // `EFBIG` case is gone too: the 4096 cap was the Zig's
    // stack buffer, CPython has none — an unbound stream
    // `recv` is `EINVAL` at any size.)
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind(123)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.listen(1)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(10)",
            ("OSError", ERR_RECV_UNCONN),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(-1)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(4097)",
            ("OSError", ERR_RECV_UNCONN),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.getsockopt(1, 17, 0)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.getsockopt(1, 17, 4097)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('x' * 109)",
            ("OSError", "36"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

/// `getsockopt` / `setsockopt` are shadow state now (see
/// the module docs): `SO_PEERCRED` and unknown options are
/// `ENOPROTOOPT`; unset `SO_RCVBUF` / `SO_SNDBUF` are
/// `ENOPROTOOPT`; the other known options return their
/// shadow value or their known default.
#[test]
fn socket_getsockopt_shadow() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4) == b'\x00\x00\x00\x00'
assert s.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 4) == b'\x00\x00\x00\x00'
assert s.getsockopt(socket.IPPROTO_IP, socket.IP_TTL, 4) == b'\x40\x00\x00\x00'
assert len(s.getsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, 16)) == 16
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 2) == b'\x00\x00'
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4) == b'\x01\x00\x00\x00'
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
assert s.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 4) == b'\x01\x00\x00\x00'
s.setsockopt(socket.IPPROTO_IP, socket.IP_TTL, 128)
assert s.getsockopt(socket.IPPROTO_IP, socket.IP_TTL, 4) == b'\x80\x00\x00\x00'
s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4) == b'\x00\x10\x00\x00'
s.close()
"#,
    );
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, 9999, 4)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.setsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 1)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.setsockopt(socket.SOL_SOCKET, 9999, 1)",
            ("OSError", "92"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

/// `SO_RCVTIMEO` / `SO_SNDTIMEO` take a 16-byte `timeval`
/// (an int is `EINVAL`); the value round-trips through
/// `getsockopt`. A raw `setsockopt` timeout does NOT
/// change `gettimeout()` (CPython: only `settimeout`
/// updates it).
#[test]
fn socket_sockopt_timeouts() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
tv = b'\x01\x00\x00\x00\x00\x00\x00\x00\x20\xa1\x07\x00\x00\x00\x00\x00'
s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, tv)
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, 16) == tv
assert s.gettimeout() is None
s.close()
"#,
    );
    assert_eq!(
        fx.raised_msg(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.setsockopt(socket.SOL_SOCKET, socket.SO_RCVTIMEO, 1)"
        ),
        ("OSError".to_string(), "22".to_string())
    );
}

/// `sendall` takes str/bytes only: a bytearray is the
/// TypeError "can't convert to str implicitly" (probed on
/// the reference). The data conversion happens before any
/// fd check, so this needs no connection.
#[test]
fn socket_sendall_type_errors() {
    let mut fx = Fx::new();
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.sendall(bytearray(b'hi'))",
            ("TypeError", "can't convert to str implicitly"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.sendall(123)",
            ("TypeError", "can't convert to str implicitly"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

#[test]
fn socket_setblocking() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.setblocking(True)
s.setblocking(False)
s.setblocking(0)
s.close()
"#,
    );
    assert_eq!(
        fx.raised_msg(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.setblocking('x')"
        ),
        ("TypeError".to_string(), "can't convert to int".to_string())
    );
}

/// `settimeout` / `gettimeout` round-trip exactly through
/// the shadow state; bad values are `ValueError` /
/// `TypeError`.
#[test]
fn socket_timeout_roundtrip() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
assert s.gettimeout() is None
s.settimeout(1.5)
assert s.gettimeout() == 1.5
s.settimeout(0)
assert s.gettimeout() == 0.0
s.setblocking(True)
assert s.gettimeout() is None
s.setblocking(False)
assert s.gettimeout() == 0.0
s.settimeout(None)
assert s.gettimeout() is None
s.close()
"#,
    );
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.settimeout(-1)",
            ("ValueError", "Timeout value must be a non-negative number"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.settimeout('x')",
            ("TypeError", "can't convert to float"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

#[test]
fn socket_close_semantics() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
assert s.fileno() >= 0
s.close()
assert s.fileno() == -1
s.close()
assert s.fileno() == -1
"#,
    );
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ns.recv(1)",
            ("OSError", "9"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ns.sendall(b'x')",
            ("OSError", "9"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ns.listen(1)",
            ("OSError", "9"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ns.getsockopt(1, 17, 12)",
            ("OSError", "9"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.close()\ns.setblocking(True)",
            ("OSError", "9"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
}

/// `bind` accepts a bytes path (probed on the reference).
#[test]
fn socket_bind_bytes_path() {
    let path = "/tmp/petitpy-d3-sock-bytes.sock";
    unlink(path);
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind(b'{path}')\ns.listen(1)\ns.close()\n"
    ));
    unlink(path);
}

/// Full round trip: server binds/listens/accepts, the
/// client (a Rust `std` thread) connects, sends `hello`,
/// reads `world` back. The client retries the connect
/// until the socket file exists.
#[test]
fn socket_roundtrip() {
    let path = "/tmp/petitpy-d3-sock-roundtrip.sock";
    unlink(path);
    let path_owned = path.to_string();
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
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\nclient, addr = s.accept()\nassert addr == b''\ndata = client.recv(100)\nclient.sendall(b'world')\nclient.close()\ns.close()\n"
    ));
    assert_eq!(fx.gbytes("data"), b"hello");
    client.join().expect("client thread");
    unlink(path);
}

/// TCP echo: the VM is the client, a Rust thread the
/// server. Also pins `recv` returning exactly what the
/// peer sent.
#[test]
fn socket_tcp_echo() {
    let (port, server) = tcp_server(|mut s| {
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).expect("server reads");
        assert_eq!(&buf, b"hello");
        s.write_all(b"world").expect("server writes");
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\ns.sendall(b'hello')\ndata = s.recv(100)\nassert data == b'world'\ns.close()\n"
    ));
    assert_eq!(fx.gbytes("data"), b"world");
    server.join().expect("server thread");
}

/// UDP `sendto` / `recvfrom` over `AF_UNIX` datagrams:
/// the VM binds a datagram socket, sends to a Rust
/// `UnixDatagram`, which replies; the reply's address is
/// the Rust socket's path bytes (the sender's address).
/// (The `AF_INET` version is
/// `socket_udp_inet_sendto_recvfrom`, ignored — this VM's
/// sandbox returns `EPERM` for UDP loopback `sendto`,
/// verified with a standalone `std` probe; the datagram
/// state machine and the `sendto` / `recvfrom` path are
/// the same for both families.)
#[cfg(unix)]
#[test]
fn socket_unix_datagram_sendto_recvfrom() {
    use std::os::unix::net::UnixDatagram;
    let server_path = "/tmp/petitpy-h-sock-udp-server.sock";
    let client_path = "/tmp/petitpy-h-sock-udp-client.sock";
    unlink(server_path);
    unlink(client_path);
    let sock = UnixDatagram::bind(server_path).expect("udp bind");
    let server = std::thread::spawn(move || {
        let mut buf = [0u8; 100];
        let (n, src) = sock.recv_from(&mut buf).expect("udp recv");
        assert_eq!(&buf[..n], b"ping");
        sock.send_to(b"pong", src.as_pathname().unwrap())
            .expect("udp send");
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\ns.bind(b'{client_path}')\nn = s.sendto(b'ping', b'{server_path}')\nassert n == 4\ndata, addr = s.recvfrom(100)\nassert data == b'pong'\nassert addr == b'{server_path}'\ns.close()\n"
    ));
    server.join().expect("server thread");
    unlink(server_path);
    unlink(client_path);
}

/// `AF_INET` UDP `sendto` / `recvfrom`. IGNORED: this
/// VM's sandbox returns `EPERM` (os error 1) for UDP
/// loopback `sendto` — verified with a standalone `std`
/// probe (`UdpSocket::send_to` to `127.0.0.1` fails the
/// same way, no petitpy code involved), so the test
/// cannot pass here. The datagram path is covered by
/// `socket_unix_datagram_sendto_recvfrom`; run this one
/// explicitly (`cargo test -- --ignored`) on an
/// unsandboxed host.
#[ignore]
#[test]
fn socket_udp_inet_sendto_recvfrom() {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
    let port = sock.local_addr().expect("local addr").port();
    let server = std::thread::spawn(move || {
        let mut buf = [0u8; 100];
        let (n, src) = sock.recv_from(&mut buf).expect("udp recv");
        assert_eq!(&buf[..n], b"ping");
        sock.send_to(b"pong", src).expect("udp send");
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\nn = s.sendto(b'ping', ('127.0.0.1', {port}))\nassert n == 4\ndata, addr = s.recvfrom(100)\nassert data == b'pong'\nassert addr[0] == '127.0.0.1'\nassert addr[1] == {port}\ns.close()\n"
    ));
    server.join().expect("server thread");
}

/// `send` / `sendall` on a listener is `ENOTCONN` (107,
/// CPython) — this fixes the G6b §4 leftover, where it
/// was `EINVAL`. Each case binds its own path: the first
/// script's listener stays alive (a module global in the
/// shared `Fx` VM), so a second `bind` on the same path
/// would be `EADDRINUSE`.
#[test]
fn socket_listener_send_enotconn() {
    let mut fx = Fx::new();
    for method in ["send", "sendall"] {
        let path = format!("/tmp/petitpy-h-sock-listenersend-{method}.sock");
        unlink(&path);
        assert_eq!(
            fx.raised_msg(&format!(
                "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\ns.{method}(b'x')"
            )),
            ("OSError".to_string(), ERR_ENOTCONN.to_string()),
            "for {method}"
        );
        unlink(&path);
    }
}

/// `connect` on a listener is `EINVAL`; a second
/// `connect` on a connected socket is `EISCONN` (106).
#[test]
fn socket_connect_errors() {
    let path = "/tmp/petitpy-h-sock-connerr.sock";
    unlink(path);
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(&format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\ns.connect(('127.0.0.1', 9))"
        )),
        ("OSError".to_string(), "22".to_string())
    );
    unlink(path);
    let (port, server) = tcp_server(|s| drop(s));
    assert_eq!(
        fx.raised_msg(&format!(
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.connect(('127.0.0.1', {port}))\ns.connect(('127.0.0.1', {port}))"
        )),
        ("OSError".to_string(), ERR_EISCONN.to_string())
    );
    server.join().expect("server thread");
}

/// `dup()` shares the connection (a new fd): the dup
/// sends, closes, and the original still sends.
#[test]
fn socket_dup() {
    let path = "/tmp/petitpy-h-sock-dup.sock";
    unlink(path);
    let path_owned = path.to_string();
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
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).expect("client reads 1");
        assert_eq!(&buf, b"hello");
        stream.read_exact(&mut buf).expect("client reads 2");
        assert_eq!(&buf, b"again");
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\nclient, addr = s.accept()\nd = client.dup()\nassert d.fileno() != client.fileno()\nd.sendall(b'hello')\nd.close()\nclient.sendall(b'again')\nclient.close()\ns.close()\n"
    ));
    client.join().expect("client thread");
    unlink(path);
}

/// `makefile('rwb')`: the VM reads a line through the
/// file object and writes a reply through it.
#[test]
fn socket_makefile() {
    let path = "/tmp/petitpy-h-sock-makefile.sock";
    unlink(path);
    let path_owned = path.to_string();
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
        stream.read_exact(&mut buf).expect("client reads");
        assert_eq!(&buf, b"world\n");
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\nclient, addr = s.accept()\nf = client.makefile('rwb')\nline = f.readline()\nassert line == b'hello\\n'\nf.write(b'world\\n')\nf.flush()\nf.close()\nclient.close()\ns.close()\n"
    ));
    assert_eq!(fx.gbytes("line"), b"hello\n");
    client.join().expect("client thread");
    unlink(path);
}

/// `getaddrinfo`: list of 5-tuples, `canonname` is `''`,
/// int and service-name ports, `None` host, bad family
/// is `EAFNOSUPPORT`, an unknown service is
/// `OSError(-2)` (CPython's `gaierror` errno).
#[test]
fn socket_getaddrinfo() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
infos = socket.getaddrinfo('localhost', 80)
assert len(infos) > 0
fam, typ, proto, canon, addr = infos[0]
assert fam == socket.AF_INET or fam == socket.AF_INET6
assert typ == socket.SOCK_STREAM
assert proto == 0
assert canon == ''
assert addr[1] == 80
infos = socket.getaddrinfo('127.0.0.1', 'http')
assert infos[0][0] == socket.AF_INET
assert infos[0][4] == ('127.0.0.1', 80)
infos = socket.getaddrinfo(None, 0)
assert len(infos) > 0
"#,
    );
    // `EAFNOSUPPORT` is the hardcoded Python-facing 97
    // (`modsocket.rs`), not the kernel value — so this
    // assert passes on macOS as-is (verified for
    // Build #20; Darwin's kernel value is 47).
    assert_eq!(
        fx.raised_msg("import socket\nsocket.getaddrinfo('localhost', 80, 999)"),
        ("OSError".to_string(), "97".to_string())
    );
    assert_eq!(
        fx.raised_msg("import socket\nsocket.getaddrinfo('localhost', 'nosuchservice')"),
        ("OSError".to_string(), "-2".to_string())
    );
}

/// `fromfd` is CUT in H-socket on the DEFAULT build: the
/// name stays, every call raises `NotImplementedError` (safe
/// Rust cannot take ownership of a raw fd). Under
/// `unsafe-ffi` (S3) it wraps the fd as-is (no dup — the
/// integration test `socket_fromfd_wraps_fd` requires
/// `fileno() == fd`); an invalid fd is `OSError(EBADF)`.
#[test]
fn socket_fromfd() {
    let mut fx = Fx::new();
    #[cfg(not(feature = "unsafe-ffi"))]
    for (src, want) in [
        (
            "import socket\nsocket.fromfd(0, socket.AF_UNIX, socket.SOCK_STREAM)",
            ("NotImplementedError", "socket.fromfd is not implemented"),
        ),
        (
            "import socket\nsocket.fromfd(-1, socket.AF_UNIX, socket.SOCK_STREAM)",
            ("NotImplementedError", "socket.fromfd is not implemented"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "for {src:?}"
        );
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        // A valid fd wraps as-is (`fileno() == fd`); the
        // socket takes ownership, so `close()` closes the fd
        // (a temp file here, never stdin — closing fd 0 would
        // close the test process's stdin).
        let path = std::env::temp_dir()
            .join(format!("petitpy_fromfd_sock_{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let p = path.to_string_lossy().replace('\\', "/");
        // `detach()`, NOT `close()`, releases the socket: the
        // fd is ALSO owned by the VM's `os_fds` table (it came
        // from `os.open`), and `close()` here would close it
        // twice — the stale `os_fds` File aborts the debug
        // build with an IO Safety violation when the VM drops
        // (the `fromfd` double-ownership hazard). `detach()`
        // removes the socket from the table without closing,
        // so the fd is closed exactly once, by `os_fds`.
        fx.run_ok(&format!(
            "import socket, os\nfd = os.open({p:?}, os.O_RDONLY)\ns = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)\nok = s.fileno() == fd and s.detach() == fd and s.fileno() == -1\n",
        ));
        assert_eq!(fx.g("ok"), Some(Value::Bool(true)));
        let _ = std::fs::remove_file(&path);
        // An invalid fd is `OSError(EBADF)`.
        assert_eq!(
            fx.raised_msg(
                "import socket\nsocket.fromfd(-1, socket.AF_UNIX, socket.SOCK_STREAM)"
            ),
            ("OSError".to_string(), "9".to_string()),
        );
    }
}

/// `fileno()` across `bind()`: on the default build it
/// CHANGES (the placeholder fd is swapped for the
/// listener — the documented pure-`std` divergence from
/// the reference, which keeps one fd). Under
/// `unsafe-ffi` the socket has a real fd at
/// construction, so `fileno()` is STABLE (the reference
/// behaviour). (The old feature-only
/// `socket_fileno_stable_on_bind` was removed when both
/// builds used the placeholder; the FFI implementation
/// restores stability for the feature build.)
#[test]
fn socket_fileno_changes_on_bind() {
    let path = "/tmp/petitpy-d3-sock-fdchange.sock";
    unlink(path);
    // `==` under `unsafe-ffi` (stable fd), `!=` on the
    // default build (placeholder swap).
    #[cfg(feature = "unsafe-ffi")]
    let cmp = "==";
    #[cfg(not(feature = "unsafe-ffi"))]
    let cmp = "!=";
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\nbefore = s.fileno()\ns.bind('{path}')\nafter = s.fileno()\nassert before >= 0\nassert after >= 0\nassert before {cmp} after\ns.close()\n"
    ));
    unlink(path);
}

/// The constructor fails at once now, on both builds:
/// bad family is `EAFNOSUPPORT` (97), bad type is
/// `EINVAL` (22), bad proto is `EPROTONOSUPPORT` (93).
#[test]
fn socket_ctor_bad_family() {
    let mut fx = Fx::new();
    for fam in [0, -1, 999] {
        assert_eq!(
            fx.raised_msg(&format!(
                "import socket\nsocket.socket({fam}, socket.SOCK_STREAM)"
            )),
            ("OSError".to_string(), "97".to_string()),
            "for family {fam}"
        );
    }
    assert_eq!(
        fx.raised_msg("import socket\nsocket.socket(socket.AF_INET, 999)"),
        ("OSError".to_string(), "22".to_string())
    );
    assert_eq!(
        fx.raised_msg(
            "import socket\nsocket.socket(socket.AF_INET, socket.SOCK_STREAM, 999)"
        ),
        ("OSError".to_string(), "93".to_string())
    );
}
