//! Tests for the `socket` module (`modsocket.rs`).
//!
//! The round-trip test drives a real client from a Rust
//! `std` thread (`UnixStream::connect`) — the reference has
//! no `connect` method, so the client side is always the
//! test harness. Socket paths are unique per test and
//! unlinked before and after.

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::modules::modsocket::build_socket_module;
use crate::objects::containers::objdict::dict_get_value;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{GcRef, Obj, Value};
use crate::runtime::vm::{Vm, VmError};

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
    let (m, t) = {
        let (mut ctx, state) = vm.split();
        let (m, t) =
            build_socket_module(&mut ctx, fun_var, module_type).expect("socket builds");
        let id = static_qstr_id(b"socket").expect("socket qstr");
        state.builtin_modules.push((id, m));
        (m, t)
    };
    vm.types.socket = Some(t);
    vm.pin(m);
    t
}

fn unlink(path: &str) {
    let _ = std::fs::remove_file(path);
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
assert socket.AF_UNIX == 1
assert socket.AF_INET == 2
assert socket.AF_INET6 == 10
assert socket.SOCK_STREAM == 1
assert socket.SOCK_DGRAM == 2
assert socket.SOL_SOCKET == 1
assert socket.SO_REUSEADDR == 2
assert socket.SO_PEERCRED == 17
assert str(socket.socket) == '<function>'
assert str(socket.fromfd) == '<function>'
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

/// The not-shipped names are all `AttributeError`
/// (`modsocket.zig` header: connect, sendto, recvfrom,
/// setsockopt, makefile, gethostname, gethostbyname).
#[test]
fn socket_no_unshipped() {
    let mut fx = Fx::new();
    // `getattr` is not bound in this worktree (D1's job), so
    // probe the names with bare attribute loads instead.
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
ok = True
try:
    s.connect
    ok = False
except AttributeError:
    pass
try:
    s.sendto
    ok = False
except AttributeError:
    pass
try:
    s.recvfrom
    ok = False
except AttributeError:
    pass
try:
    s.setsockopt
    ok = False
except AttributeError:
    pass
try:
    s.makefile
    ok = False
except AttributeError:
    pass
try:
    socket.gethostname
    ok = False
except AttributeError:
    pass
try:
    socket.gethostbyname
    ok = False
except AttributeError:
    pass
assert ok
s.close()
"#,
    );
}

#[test]
fn socket_bind_listen_errors() {
    let mut fx = Fx::new();
    // Non-AF_UNIX bind, non-str path, listen/recv before
    // bind, bad sizes, bad getsockopt buflens.
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_INET, socket.SOCK_STREAM)\ns.bind(('127.0.0.1', 0))",
            ("OSError", "97"),
        ),
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
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(-1)",
            ("OSError", "22"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(4097)",
            ("OSError", "27"),
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

/// `getsockopt` has no stable-`std` implementation: after
/// the buflen validation every option is `ENOPROTOOPT`
/// (the reference returns the real bytes — see the module
/// docs; this pins the documented limitation). Default
/// build only — under `unsafe-ffi` the real `getsockopt(2)`
/// runs (see `socket_getsockopt_real`).
#[cfg(not(feature = "unsafe-ffi"))]
#[test]
fn socket_getsockopt_enoprotoopt() {
    let mut fx = Fx::new();
    for (src, want) in [
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)",
            ("OSError", "92"),
        ),
        (
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4)",
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

/// `getsockopt` under `unsafe-ffi` runs the real
/// `getsockopt(2)` (U7): `SO_PEERCRED` on an unconnected
/// socket is the 12-byte `ucred` (pid 0, uid -1, gid -1 —
/// probed on the reference), `SO_REUSEADDR` is 4 zero
/// bytes, and an unknown optname is `ENOPROTOOPT`
/// (probed on the reference).
#[cfg(feature = "unsafe-ffi")]
#[test]
fn socket_getsockopt_real() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
peercred = s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
reuse = s.getsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 4)
s.close()
"#,
    );
    assert_eq!(
        fx.gbytes("peercred"),
        [0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255]
    );
    assert_eq!(fx.gbytes("reuse"), [0, 0, 0, 0]);
    assert_eq!(
        fx.raised_msg(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.getsockopt(socket.SOL_SOCKET, 9999, 4)"
        ),
        ("OSError".to_string(), "92".to_string())
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

/// `fromfd(-1, ...)` behaves like a closed socket (the
/// Zig wraps the fd directly and fails at use); `fromfd`
/// on a real fd wraps it as `SocketState::Foreign` (F5b):
/// `fileno()` returns the fd, `close()` drops the entry
/// and stores `-1`. Under `unsafe-ffi` the socket OWNS
/// the fd (U4) and `close()` closes it — the reference
/// closes it too (probed: `os.close(fd)` after
/// `s.close()` is EBADF; see the subprocess test
/// `g6b_fromfd_close_ebadf`). On the default build the
/// OS fd is NOT closed (the documented divergence, see
/// the module docs and `scripts/f5b_result.txt` §4).
#[test]
fn socket_fromfd() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import socket\ns = socket.fromfd(-1, socket.AF_UNIX, socket.SOCK_STREAM)\nassert s.fileno() == -1\n",
    );
    assert_eq!(
        fx.raised_msg(
            "import socket\ns = socket.fromfd(-1, socket.AF_UNIX, socket.SOCK_STREAM)\ns.recv(1)"
        ),
        ("OSError".to_string(), "9".to_string())
    );
    let (a, _b) = UnixStream::pair().expect("pair");
    let fd = a.as_raw_fd();
    fx.run_ok(&format!(
        "import socket\ns = socket.fromfd({fd}, socket.AF_UNIX, socket.SOCK_STREAM)\nassert s.fileno() == {fd}\ns.close()\nassert s.fileno() == -1\n"
    ));
    #[cfg(feature = "unsafe-ffi")]
    {
        // The socket owns the fd now (U4): `s.close()`
        // already closed it, so the Rust-side handle must
        // NOT close it again — forget it instead (the
        // "fd is really closed" assertion is the
        // subprocess test `g6b_fromfd_close_ebadf`).
        std::mem::forget(a);
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        // The caller's fd is still open (the divergence):
        // the pair's other end can still send through it.
        drop(a);
    }
}

/// `fileno()` changes across `bind()` (the placeholder fd
/// is swapped for the listener — the documented pure-`std`
/// divergence from the reference, which keeps one fd).
/// Default build only — under `unsafe-ffi` the fd never
/// changes (see `socket_fileno_stable_on_bind`).
#[cfg(not(feature = "unsafe-ffi"))]
#[test]
fn socket_fileno_changes_on_bind() {
    let path = "/tmp/petitpy-d3-sock-fdchange.sock";
    unlink(path);
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\nbefore = s.fileno()\ns.bind('{path}')\nafter = s.fileno()\nassert before >= 0\nassert after >= 0\nassert before != after\ns.close()\n"
    ));
    unlink(path);
}

/// `fileno()` is stable across `bind()` under
/// `unsafe-ffi` (U6): the constructor calls the real
/// `socket(2)` and `bind()` runs the real `bind(2)` on
/// the same fd — the reference keeps one fd throughout.
#[cfg(feature = "unsafe-ffi")]
#[test]
fn socket_fileno_stable_on_bind() {
    let path = "/tmp/petitpy-d3-sock-fdstable.sock";
    unlink(path);
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\nbefore = s.fileno()\ns.bind('{path}')\nafter = s.fileno()\nassert before >= 0\nassert before == after\ns.close()\n"
    ));
    unlink(path);
}

/// The constructor fails for a bad family/type under
/// `unsafe-ffi` (U6 — the real `socket(2)` fails
/// `EAFNOSUPPORT`, probed on the reference); on the
/// default build it succeeds over the placeholder and
/// the failure surfaces at `bind()` instead.
#[test]
fn socket_ctor_bad_family() {
    let mut fx = Fx::new();
    #[cfg(feature = "unsafe-ffi")]
    for fam in [0, -1, 999] {
        assert_eq!(
            fx.raised_msg(&format!(
                "import socket\nsocket.socket({fam}, socket.SOCK_STREAM)"
            )),
            ("OSError".to_string(), "97".to_string()),
            "for family {fam}"
        );
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    fx.run_ok(
        "import socket\ns = socket.socket(0, socket.SOCK_STREAM)\nassert s.fileno() >= 0\ns.close()\n",
    );
}

/// `accept()` on a bound-but-not-listening socket is
/// `EINVAL` under `unsafe-ffi` (U6 — the kernel decides;
/// probed on the reference). Feature only: on the
/// default build `bind()` already listens, so the same
/// program would block in `accept()`.
#[cfg(feature = "unsafe-ffi")]
#[test]
fn socket_accept_on_bound_not_listening() {
    let path = "/tmp/petitpy-d3-sock-boundaccept.sock";
    unlink(path);
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg(&format!(
            "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.accept()"
        )),
        ("OSError".to_string(), "22".to_string())
    );
    unlink(path);
}

/// `bind("")` binds an abstract socket under
/// `unsafe-ffi` (U6 — the all-zeros address, probed on
/// the reference): a client connecting to the empty
/// abstract name is accepted and can send. Feature only:
/// on the default build `bind("")` is `ENOENT`.
/// NOTE: the client address must be 107 zero name bytes
/// (`from_abstract_name(&[0; 107])`, addrlen 110) — the
/// kernel matches abstract names by full length, so the
/// 0-byte name does NOT match the server's 110-byte
/// all-zeros bind (probed: a 1-byte name is refused
/// with `ECONNREFUSED`, the 108-byte name connects).
#[cfg(feature = "unsafe-ffi")]
#[test]
fn socket_bind_empty_abstract() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;
    let addr = SocketAddr::from_abstract_name(&[0u8; 107]).expect("empty abstract addr");
    let client = std::thread::spawn(move || {
        // Retry like `socket_roundtrip`: the VM may not
        // have bound yet when this thread first runs.
        let mut stream = None;
        for _ in 0..200 {
            match UnixStream::connect_addr(&addr) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("client connects");
        stream.write_all(b"hello").expect("client writes");
    });
    let mut fx = Fx::new();
    fx.run_ok(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('')\ns.listen(1)\nclient, addr = s.accept()\nassert addr == b''\ndata = client.recv(100)\nclient.close()\ns.close()\n",
    );
    assert_eq!(fx.gbytes("data"), b"hello");
    client.join().expect("client thread");
}

/// `recv` works on a `fromfd` socket under `unsafe-ffi`
/// (U4 — the foreign fd is owned, so it dups): the
/// server accepts a client, wraps the accepted fd with
/// `fromfd`, and reads through the wrapper. Feature only:
/// on the default build I/O on a foreign fd is `EBADF`.
#[cfg(feature = "unsafe-ffi")]
#[test]
fn socket_fromfd_recv() {
    let path = "/tmp/petitpy-d3-sock-fromfdrecv.sock";
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
    });
    let mut fx = Fx::new();
    fx.run_ok(&format!(
        "import socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\ns.bind('{path}')\ns.listen(1)\nclient, addr = s.accept()\nwrapped = socket.fromfd(client.fileno(), socket.AF_UNIX, socket.SOCK_STREAM)\ndata = wrapped.recv(100)\nwrapped.close()\nclient.close()\ns.close()\n"
    ));
    assert_eq!(fx.gbytes("data"), b"hello");
    client.join().expect("client thread");
    unlink(path);
}
