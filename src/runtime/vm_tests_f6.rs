//! Lane F6 regression tests: GC finaliser + heap reclaim (ADR 019).
//!
//! Vendor hooks (all read from the original's Zig):
//! * Finalisers (`src/memory/gc.zig:32-43`, `runFinalisers` 321-360):
//!   an allocation made with `GC_ALLOC_FLAG_HAS_FINALISER` gets its
//!   `__del__` called after marking, before sweeping, protected (an
//!   exception is printed, not raised). Only native types use it
//!   (files, sockets); user classes' `__del__` is never called. The
//!   Rust arena lists unmarked flagged objects in
//!   `pending_finalisers` (`collect_inner`); the VM drains the list
//!   after an explicit `gc.collect()` (`finaliser.rs`, via the
//!   `gc.collect` GcRef-identity interception in
//!   `call_gc_collect_ctx`).
//! * File flag (`file_io.zig:510-511`): only `open()` files are
//!   allocated with `m_malloc_with_finaliser`; stdio objects are
//!   not flagged. Sockets were already flagged (D3,
//!   `modsocket.rs:299`).
//! * Heap reclaim: gc.zig's pool only grows while full, so
//!   `gc_info()`'s total stays at the pool size (1 MiB after the
//!   300k-object case). The Rust `Vec` arena has no "collect on no
//!   room" trigger, so `collect_inner` drops the trailing run of
//!   free slots after the sweep, restoring `mem_alloc() +
//!   mem_free() < 8 MiB`.
//!
//! The `Fx` harness is the `vm_tests.rs` pattern, copied (same as
//! `vm_tests_f5a.rs` and the E2 series).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objexcept::exception_get_value;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{FunBuiltinData, GcRef, InstanceData, MapData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::finaliser::drain_pending_finalisers;
use crate::runtime::vm::{Vm, VmError};
use std::sync::atomic::{AtomicBool, Ordering};

/// A fresh VM per test, plus the read-back helpers (the
/// `vm_tests.rs` `Fx` pattern, copied — that module's `Fx`
/// is private).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        Self { vm: Vm::new() }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => panic!(
                "module raised {}: {:?}",
                self.exc_name(exc),
                self.exc_value(exc)
            ),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return the exception's
    /// (type name, message).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Ok(_) => panic!("module did not raise"),
            Err(VmError::Raised(exc)) => (self.exc_name(exc), self.exc_value(exc)),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        use crate::objects::containers::objdict::dict_get_value;
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    fn gi(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            other => panic!("global {name} is not an int: {other:?}"),
        }
    }

    /// The exception's type name (`"ValueError"`, …).
    fn exc_name(&mut self, exc: Value) -> String {
        use crate::objects::obj::obj_get_type;
        let t = obj_get_type(&self.vm.arena, &self.vm.types, exc)
            .expect("an exception has a type");
        match self.vm.arena.get(t) {
            Obj::Type(d) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(d.name as u16)).into_owned()
            }
            _ => panic!("exception type is not a type object"),
        }
    }

    /// The exception's first arg (the message), as a string.
    fn exc_value(&mut self, exc: Value) -> String {
        let v = exception_get_value(&self.vm.arena, exc);
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("exception value is not a str: {other:?}"),
            },
            other => panic!("exception value is not a str: {other:?}"),
        }
    }
}

/// Serialise the fd-sensitive tests: `cargo test` runs tests in
/// parallel threads, and a sibling test's transient fds would
/// inflate another test's `/dev/fd` count. (The mechanism itself
/// is proven by the standalone probes in `~/workspace/f6_probe/`.)
static FD_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fd_guard() -> std::sync::MutexGuard<'static, ()> {
    FD_GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
#[test]
fn f6_file_finaliser_closes_on_collect() {
    let _guard = fd_guard();
    let mut fx = Fx::new();
    fx.run_ok(
        "import gc, os\n\
         path = '/tmp/f6_test_file_' + str(os.getpid())\n\
         before = len(os.listdir('/dev/fd'))\n\
         for i in range(200):\n\
         \x20   f = open(path, 'w')\n\
         f = None\n\
         gc.collect()\n\
         leaked = len(os.listdir('/dev/fd')) - before\n\
         os.remove(path)\n",
    );
    assert!(fx.gi("leaked") < 10, "dropped files must be closed");
}

#[cfg(unix)]
#[test]
fn f6_socket_finaliser_closes_on_collect() {
    let _guard = fd_guard();
    let mut fx = Fx::new();
    fx.run_ok(
        "import gc, os, socket\n\
         before = len(os.listdir('/dev/fd'))\n\
         for i in range(200):\n\
         \x20   s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         s = None\n\
         gc.collect()\n\
         leaked = len(os.listdir('/dev/fd')) - before\n",
    );
    assert!(fx.gi("leaked") < 10, "dropped sockets must be closed");
}

#[test]
fn f6_user_del_not_called() {
    // gc.zig: only native types use finalisers; user classes'
    // `__del__` is never called, as in the vendor.
    let mut fx = Fx::new();
    fx.run_ok(
        "import gc\n\
         called = []\n\
         class P:\n\
         \x20   def __del__(self):\n\
         \x20       called.append(1)\n\
         for i in range(10):\n\
         \x20   P()\n\
         gc.collect()\n\
         ok = len(called) == 0\n",
    );
    assert!(fx.gb("ok"), "user __del__ must not run");
}

#[test]
fn f6_gc_collect_returns_zero_and_arity() {
    let mut fx = Fx::new();
    fx.run_ok("import gc\nok = gc.collect() == 0\n");
    assert!(fx.gb("ok"), "gc.collect() returns 0");
    let (name, msg) = fx.raised_msg("import gc\ngc.collect(1)\n");
    assert_eq!(name, "TypeError");
    assert_eq!(msg, "argument num/types mismatch");
}

#[test]
fn f6_live_file_survives_collect() {
    // A still-reachable file is NOT finalised by gc.collect().
    let _guard = fd_guard();
    let mut fx = Fx::new();
    let tmp = std::env::temp_dir().to_string_lossy().replace('\\', "/");
    fx.run_ok(&format!(
        "import gc, os\n\
         path = \"{tmp}/f6_test_live_\" + str(os.getpid())\n\
         f = open(path, 'w')\n\
         f.write('hello')\n\
         gc.collect()\n\
         f.write(' world')\n\
         f.close()\n\
         content = open(path).read()\n\
         os.remove(path)\n",
    ));
    // Read the content back through the `content` global.
    match fx.g("content") {
        Some(Value::Heap(r)) => match fx.vm.arena.get(r) {
            Obj::Str(s) => assert_eq!(&s.bytes, b"hello world"),
            other => panic!("content is not a str: {other:?}"),
        },
        other => panic!("content global missing: {other:?}"),
    }
}

#[test]
fn f6_reclaim_case() {
    // The `gc_reclaims_garbage` case verbatim (300k dropped
    // instances; heap < 8 MiB after collect).
    let mut fx = Fx::new();
    fx.run_ok(
        "import gc, sys\n\
         class P:\n\
         \x20   def __init__(self, x):\n\
         \x20       self.x = x\n\
         for i in range(300000):\n\
         \x20   P(i)\n\
         gc.collect()\n\
         heap = gc.mem_alloc() + gc.mem_free()\n\
         ok = sys.maxsize < 2**32 or 0 < heap < 8 * 1024 * 1024\n",
    );
    assert!(fx.gb("ok"), "heap must be < 8 MiB after collect");
}

/// Whether the test `__del__` below ran (a Rust-side flag: the
/// raising builtin cannot touch Python globals, it only has a
/// `CallCtx`).
static DEL_RAN: AtomicBool = AtomicBool::new(false);

/// A raising `__del__` body (a `FunBuiltin`, like the native
/// close bodies): records the raise and returns the signal.
fn raise_del(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    DEL_RAN.store(true, Ordering::SeqCst);
    ctx.record_raise("ValueError", Some("boom-del"));
    Err(PyError::PyRaise)
}

/// Build a class `P` with a raising `FunBuiltin` `__del__` in
/// its dict, then allocate one flagged instance of it (unrooted)
/// and return its `GcRef`. The `__del__` sets `DEL_RAN`.
/// A raising `__del__` is unobservable from Python on the
/// reference (native `close` never raises; patching or
/// subclassing `__del__` is blocked identically on both
/// binaries), so the protected-call path is exercised here
/// directly.
fn install_raising_del_instance(fx: &mut Fx) -> GcRef {
    fx.run_ok("class P:\n    pass\n");
    let p_type = match fx.g("P") {
        Some(Value::Heap(r)) => r,
        other => panic!("P is not a heap value: {other:?}"),
    };
    let (mut ctx, _state) = fx.vm.split();
    // Install the raising `__del__` in P's dict.
    let del_q = static_qstr_id(b"__del__").expect("__del__ is a static qstr");
    // n_args (0, 0): a `FunBuiltin` on a user (instance) type
    // is a staticmethod by the vendor's payload rule
    // (`convert_member_lookup`, lookup.rs:149-154 — probed:
    // a `FunBuiltin` `__init__` on a user type is unbound),
    // so the drain calls it with 0 args. Production native
    // `__del__` bodies (file/socket close) live on native
    // types and are bound (1 arg); the arity here reflects
    // the lookup result, not the production signature.
    let fun = Value::Heap(ctx.alloc(
        None,
        Obj::FunBuiltin(FunBuiltinData {
            name: del_q,
            n_args: fun_n_args(0, 0),
            func: raise_del,
            kw_func: None,
        }),
    ));
    let dict = match ctx.arena.get(p_type) {
        Obj::Type(d) => Value::Heap(d.dict),
        _ => panic!("P is not a type"),
    };
    dict_store(&mut ctx, dict, Value::Qstr(del_q), fun).expect("store __del__");
    // Allocate a flagged instance; do NOT root it.
    let roots = &*ctx.roots;
    ctx.arena.alloc_with_finaliser(
        Some(p_type),
        Obj::Instance(InstanceData {
            members: MapData::new(),
            subobj: vec![],
        }),
        roots,
    )
}

#[test]
fn f6_raising_del_is_printed_and_swallowed() {
    // The raise must be swallowed (no pending raise, the object
    // freed, the VM still usable) — the traceback print goes to
    // the test process's stdout as fd 1 (the protected-call
    // shape; see `print_raised` in `finaliser.rs`), visible
    // with --nocapture, not asserted here (see the sink test
    // below for the asserted destination).
    DEL_RAN.store(false, Ordering::SeqCst);
    let mut fx = Fx::new();
    let inst = install_raising_del_instance(&mut fx);
    {
        let (mut ctx, state) = fx.vm.split();
        let _ = ctx.arena.collect(ctx.roots);
        drain_pending_finalisers(&mut ctx, state);
        assert!(DEL_RAN.load(Ordering::SeqCst), "__del__ must have run");
        assert!(
            !ctx.has_pending_raise(),
            "a __del__ raise must not leak into pending_raise"
        );
        assert_eq!(ctx.arena.nbytes(inst), 0, "object freed after __del__");
    }
    // The VM keeps working after a swallowed finaliser raise.
    fx.run_ok("x = 1 + 1\n");
}

/// P8 (G5): the `__del__` raise print goes through the
/// embedding sink as fd 1. A recording sink, installed for the
/// duration of the drain, must receive exactly one write, as
/// fd 1, carrying the exception type and message.
static SINK_LOG: std::sync::Mutex<Vec<(i32, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C" fn record_sink(
    _userdata: *mut std::ffi::c_void,
    fd: i32,
    data: *const u8,
    len: usize,
) {
    let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    SINK_LOG.lock().unwrap().push((fd, bytes));
}

#[test]
fn f6_raising_del_goes_to_sink_as_fd1() {
    use crate::support::output::{CSink, set_thread_sink};
    SINK_LOG.lock().unwrap().clear();
    DEL_RAN.store(false, Ordering::SeqCst);
    let mut fx = Fx::new();
    let inst = install_raising_del_instance(&mut fx);
    let prev = set_thread_sink(Some(CSink {
        write: record_sink,
        userdata: std::ptr::null_mut(),
    }));
    {
        let (mut ctx, state) = fx.vm.split();
        let _ = ctx.arena.collect(ctx.roots);
        drain_pending_finalisers(&mut ctx, state);
    }
    set_thread_sink(prev);
    assert!(DEL_RAN.load(Ordering::SeqCst), "__del__ must have run");
    let log = SINK_LOG.lock().unwrap();
    assert_eq!(log.len(), 1, "sink must receive exactly one write, got {log:?}");
    assert_eq!(log[0].0, 1, "finaliser raise must go to fd 1, got {}", log[0].0);
    let text = String::from_utf8_lossy(&log[0].1);
    assert!(
        text.contains("ValueError"),
        "sink text must name the exception, got {text}"
    );
    assert!(
        text.contains("boom-del"),
        "sink text must carry the message, got {text}"
    );
    drop(log);
    let (ctx, _state) = fx.vm.split();
    assert_eq!(ctx.arena.nbytes(inst), 0, "object freed after __del__");
    fx.run_ok("x = 1 + 1\n");
}
