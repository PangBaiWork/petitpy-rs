//! The `select` builtin module — H-select rewrite (pure `std`,
//! both builds; the G0 FFI path is deleted).
//!
//! `select.select(r, w, x[, timeout])` only (the original has
//! no `poll` — see §4). The design follows the Python API
//! (`select.select`), not petitpy's internals (the Phase H
//! rule): fds are resolved once at entry, readiness is
//! probed, and the original input objects are returned in
//! input order.
//!
//! Resolution (`resolve_fd_of`, once per item, up front —
//! CPython validates every fd before waiting):
//! - an int (or bool) is the fd itself; a heap int is
//!   accepted only when it is an *exact* int (a float is NOT
//!   accepted — `obj_get_int_maybe` would take its `__int__`,
//!   but CPython's select rejects non-integers);
//! - a file object contributes its own `File` (Owned only);
//! - a socket instance (the `self_socket` shape: `Instance`
//!   with 4 small-int subobj, `[0]` = fd) resolves through
//!   the socket table;
//! - any other object is tried through its `fileno()` method
//!   (the CPython rule); anything else is a `TypeError`.
//! An int fd then goes through `resolve_fd_number`:
//! - fd `0/1/2` (stdio) and out-of-range numbers → `EBADF`
//!   (there is no safe `std` `File` for stdio; the
//!   `/proc/self/fd` re-open is Linux-only and wrong for
//!   regular files — a separate offset);
//! - a socket fd (see `modsocket::socket_probe`) is
//!   `Unprobable` (stream / listener / unbound / foreign)
//!   or `Bare` (a `fromfd` fd with no owned object —
//!   `EBADF`, the existing I/O-on-foreign-fd rule);
//! - otherwise the fd must be in `state.os_fds` (a cloned
//!   `File` is taken for the monitor); unknown → `EBADF`.
//!
//! Readiness (the probe loop, 2ms quantum):
//! - file fds: a per-fd monitor thread (`support::readiness`)
//!   reads ahead into a shared buffer; an entry is ready
//!   when the buffer is non-empty, EOF was reached, or the
//!   monitor died (a dead monitor means the next read is an
//!   OS read — which surfaces the error or the fresh data
//!   after a seek — so reporting ready is correct). The
//!   monitor is started at resolution, so data written after
//!   the call starts is still seen; the read paths
//!   (`fileio_read`, `os.read`) drain the buffer first, so
//!   no byte is lost or reordered.
//! - socket fds: never ready — `UnixStream::peek` is
//!   UNSTABLE on the stable toolchain (rustc 1.99.1) and a
//!   monitor `recv` would consume bytes the socket `recv`
//!   path does not drain. The unblock (a socket monitor +
//!   `recv` drain in `modsocket.rs`) belongs after the
//!   H-socket rewrite lands (see §4).
//! - wlist: always ready (validated at entry; a pure-`std`
//!   write-readiness probe does not exist) — documented
//!   approximation, see §4.
//! - xlist: always empty (validated at entry).
//! - all three lists empty: sleep the timeout, return three
//!   empty lists (the `select(0, NULL, NULL, NULL, tv)`
//!   behaviour — the one case the old default path had).
//! - `timeout=None` (or infinite) loops forever; a finite
//!   timeout is a deadline (huge values saturate at
//!   ~95 years — `Duration::from_secs_f64` panics on
//!   overflow, and the old code panicked there too).
//!
//! Kept from the old paths (probed on the reference
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - a negative or NaN timeout raises `OSError(22,)`;
//! - a non-list argument raises `TypeError`
//!   (`"select() argument must be a list"`); fewer than 3
//!   args (or any keyword) raises `TypeError`
//!   (`"argument num/types mismatch"` — the intercept does
//!   the arity check the call machinery did for the stub);
//! - the three result lists are DISTINCT (one shared list
//!   would alias mutations across the result positions).
//!
//! Divergences from the reference (kept, as before): a
//! non-list argument ABORTS the reference (`incorrect
//! alignment` panic in `MpObjList.fromObj`, rc=134); Rust
//! raises `TypeError`. An infinite timeout aborts the
//! reference (`@intFromFloat` panic); Rust loops forever.
//! The old feature path's real `select(2)` (with its
//! `FD_SETSIZE` 1024 cap and silent skipping of fd < 0 /
//! >= 1024) is gone: Python has no fd cap, and an unknown
//! fd is now `EBADF` instead of silently skipped. The old
//! default path's `NotImplementedError` on any non-empty
//! list is gone — this is the point of the lane.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objlist::{list_items_vec, new_list};
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::{is_exact_int, obj_get_float_maybe, obj_get_int};
use crate::objects::primitives::ops::{is_list, static_qstr_id, PrimError};
use crate::objects::value::{FileHandle, GcRef, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::builtin_vm::mismatch;
use crate::runtime::raise::raise_os_error;
use crate::runtime::vm::VmState;
use crate::runtime::vm_call::call_function_ctx;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};
use crate::runtime::vm_seams::try_instance_load_attr;
use crate::support::readiness::{ensure_monitor, Monitor};

/// `EBADF` (9): bad / unknown / stdio fd (see the module docs).
const EBADF: i32 = 9;

/// `EINVAL` (22): negative timeout.
const EINVAL: i32 = 22;

/// `EIO` (5): fallback errno when `std::io::Error` carries no
/// OS code (should not happen for real file I/O).
const EIO: i32 = 5;

/// The probe quantum: readiness is re-checked this often
/// while waiting (the 2ms quantum the brief fixes).
const PROBE_QUANTUM: Duration = Duration::from_millis(2);

/// Huge finite timeouts saturate here (~95 years):
/// `Duration::from_secs_f64` panics on overflow, and a
/// timeout this large is "forever" for any caller.
const MAX_TIMEOUT_SECS: f64 = 3.0e9;

/// `PyError` → the exception `Value` (the `modos_fd::exc`
/// shape).
fn exc(ctx: &mut CallCtx, state: &VmState, e: PyError) -> Value {
    exc_from_pyerror(ctx, state, e)
}

/// An `OSError(errno)` exception `Value`, built in two
/// sequential steps — a nested `exc(ctx, state,
/// raise_os_error(ctx, n))` double-borrows `ctx`.
fn errno_exc(ctx: &mut CallCtx, state: &VmState, n: i32) -> Value {
    let pe = raise_os_error(ctx, n);
    exc(ctx, state, pe)
}

/// `Some(n)` when `v` is an int (small, heap, or bool);
/// `None` otherwise (the caller falls through to object
/// resolution). A bignum overflow propagates as
/// `OverflowError`. Deliberately NOT `obj_get_int_maybe`:
/// that takes `__int__`, so a float would read back as its
/// truncated int — but CPython's select rejects
/// non-integers.
fn int_item(ctx: &mut CallCtx, state: &VmState, v: Value) -> Result<Option<i64>, Value> {
    match v {
        Value::SmallInt(n) => Ok(Some(n)),
        Value::Bool(b) => Ok(Some(b as i64)),
        Value::Heap(_) if is_exact_int(ctx.arena, ctx.types, v) => {
            match obj_get_int(ctx, ctx.types, v) {
                Ok(n) => Ok(Some(n)),
                Err(e) => {
                    let pe = ctx.record_typed(e);
                    Err(exc(ctx, state, pe))
                }
            }
        }
        _ => Ok(None),
    }
}

/// The fd behind one `select` item, resolved without
/// starting a monitor (validation, and the shared core of
/// read resolution). `File` carries a cloned `File` for the
/// monitor start; `Socket` is just the fd number.
enum ResolvedFd {
    Socket,
    File { fd: i32, file: std::fs::File },
}

/// Resolve a bare fd number (see the module docs).
fn resolve_fd_number(ctx: &mut CallCtx, state: &mut VmState, fd: i64) -> Result<ResolvedFd, Value> {
    if !(0..=(i32::MAX as i64)).contains(&fd) || fd <= 2 {
        return Err(errno_exc(ctx, state, EBADF));
    }
    let fd = fd as i32;
    // A socket fd (owned by a socket object, or a
    // foreign/dup'd socket): pure `std` cannot probe it.
    #[cfg(unix)]
    {
        use crate::modules::modsocket::{socket_probe, SocketProbe};
        if let Some(p) = socket_probe(fd) {
            return match p {
                SocketProbe::Bare => Err(errno_exc(ctx, state, EBADF)),
                SocketProbe::Unprobable => Ok(ResolvedFd::Socket),
            };
        }
    }
    // A runtime os fd: clone its `File` for the monitor.
    match state.os_fds.get(&fd) {
        Some(f) => match f.try_clone() {
            Ok(file) => Ok(ResolvedFd::File { fd, file }),
            Err(e) => {
                let pe = raise_os_error(ctx, e.raw_os_error().unwrap_or(EIO));
                Err(exc(ctx, state, pe))
            }
        },
        None => Err(errno_exc(ctx, state, EBADF)),
    }
}

/// What `resolve_fd_of` classified the item as. The arena
/// borrow ends before any `&mut ctx` call below (the
/// classification is extracted into owned values first).
enum ItemKind {
    File {
        fd: i32,
        closed: bool,
        handle: Option<std::rc::Rc<std::cell::RefCell<std::io::BufWriter<std::fs::File>>>>,
    },
    SocketFd(i64),
    Other,
}

/// Resolve one `select` item to its fd (see the module
/// docs). Raises `EBADF` for an unresolvable fd and
/// `TypeError` for an object with no `fileno()`.
fn resolve_fd_of(ctx: &mut CallCtx, state: &mut VmState, item: Value) -> Result<ResolvedFd, Value> {
    // An int (or bool) is the fd itself.
    if let Some(fd) = int_item(ctx, state, item)? {
        return resolve_fd_number(ctx, state, fd);
    }
    let kind = match item {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::File(d) => {
                let handle = match &d.handle {
                    FileHandle::Owned(rc) => Some(rc.clone()),
                    _ => None,
                };
                ItemKind::File {
                    fd: d.fd,
                    closed: d.closed,
                    handle,
                }
            }
            // A socket instance (the `self_socket` shape:
            // `Instance` with 4 small-int subobj, [0] = fd).
            Obj::Instance(d) if d.subobj.len() == 4
                && d.subobj.iter().all(|v| matches!(v, Value::SmallInt(_))) =>
            {
                match d.subobj[0] {
                    Value::SmallInt(fd) => ItemKind::SocketFd(fd),
                    _ => ItemKind::Other, // unreachable (guard checked all four)
                }
            }
            _ => ItemKind::Other,
        },
        _ => ItemKind::Other,
    };
    match kind {
        ItemKind::File { fd, closed, handle } => {
            if closed {
                return Err(errno_exc(ctx, state, EBADF));
            }
            match handle {
                Some(rc) => {
                    let file = rc.borrow().get_ref().try_clone().map_err(|e| {
                        let pe = raise_os_error(ctx, e.raw_os_error().unwrap_or(EIO));
                        exc(ctx, state, pe)
                    })?;
                    Ok(ResolvedFd::File { fd, file })
                }
                None => Err(errno_exc(ctx, state, EBADF)),
            }
        }
        ItemKind::SocketFd(fd) => resolve_fd_number(ctx, state, fd),
        ItemKind::Other => {
            // Any other object: try its `fileno()` method
            // (the CPython rule — an object with `fileno()`
            // is accepted).
            if let Some(fileno) =
                try_instance_load_attr(ctx, state, item, fileno_qstr())?
            {
                let v = call_function_ctx(ctx, state, fileno, 0, 0, &[])?;
                return match int_item(ctx, state, v)? {
                    Some(fd) => resolve_fd_number(ctx, state, fd),
                    None => Err(type_error(ctx, state, "fileno() returned a non-integer")),
                };
            }
            Err(type_error(
                ctx,
                state,
                "select() argument must be a file descriptor or an object with a fileno() method",
            ))
        }
    }
}

/// The `fileno` qstr id (`fileno` is a static qstr — it is
/// in `FILEIO_LOCALS` and `SOCKET_LOCALS`).
fn fileno_qstr() -> u32 {
    static_qstr_id(b"fileno").expect("fileno is a static qstr")
}

/// One resolved read-list entry: the original input object
/// (returned in the result, in input order) plus what the
/// probe loop needs.
enum Entry {
    /// A socket fd: pure `std` cannot probe it, so it never
    /// reports ready (see the module docs).
    Socket { item: Value },
    /// A file fd with its monitor (kept `Arc`: the table
    /// entry may be removed while the monitor still holds
    /// drainable bytes).
    File { item: Value, mon: Arc<Monitor> },
}

impl Entry {
    fn item(&self) -> Value {
        match self {
            Entry::Socket { item } => *item,
            Entry::File { item, .. } => *item,
        }
    }

    /// Ready when the monitor holds bytes, EOF was reached,
    /// or the monitor died (the next read is then an OS read
    /// — which surfaces the error, or the fresh data after a
    /// seek — so reporting ready is correct).
    fn ready(&self) -> bool {
        match self {
            Entry::Socket { .. } => false,
            Entry::File { mon, .. } => {
                mon.buffered_len() > 0 || mon.is_eof() || mon.is_dead()
            }
        }
    }
}

/// Resolve a read-list item (starting its monitor when it is
/// a file fd).
fn resolve_read(ctx: &mut CallCtx, state: &mut VmState, item: Value) -> Result<Entry, Value> {
    match resolve_fd_of(ctx, state, item)? {
        ResolvedFd::Socket => Ok(Entry::Socket { item }),
        ResolvedFd::File { fd, file } => {
            let mon = ensure_monitor(fd, &file).map_err(|e| {
                let pe = raise_os_error(ctx, e.raw_os_error().unwrap_or(EIO));
                exc(ctx, state, pe)
            })?;
            Ok(Entry::File { item, mon })
        }
    }
}

/// Validate a write/except-list item (no monitor is started
/// — the wlist is always-ready and the xlist always empty,
/// so only the fd's existence is checked).
fn resolve_validate(ctx: &mut CallCtx, state: &mut VmState, item: Value) -> Result<(), Value> {
    resolve_fd_of(ctx, state, item)?;
    Ok(())
}

/// Build the `(r, w, x)` result tuple (three DISTINCT lists
/// — one shared list would alias mutations across the three
/// result positions).
fn result(ctx: &mut CallCtx, r: Vec<Value>, w: Vec<Value>) -> Value {
    let r = Value::Heap(new_list(ctx, ctx.types.list, &r));
    let w = Value::Heap(new_list(ctx, ctx.types.list, &w));
    let x = Value::Heap(new_list(ctx, ctx.types.list, &[]));
    Value::Heap(new_tuple(ctx, ctx.types.tuple, &[r, w, x]))
}

/// Sleep the empty-lists timeout (`None` = forever).
fn sleep_timeout(secs: Option<f64>) {
    match secs {
        Some(f) if f.is_finite() => {
            std::thread::sleep(Duration::from_secs_f64(f.min(MAX_TIMEOUT_SECS)))
        }
        _ => std::thread::sleep(Duration::MAX),
    }
}

/// `select(rlist, wlist, xlist[, timeout])` — the intercept
/// body (see the module docs).
fn select_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    // The call machinery did this for the stub; the intercept
    // bypasses it, so the check lives here.
    if n_kw != 0 || !(3..=4).contains(&n_args) {
        return Err(mismatch(ctx, state));
    }
    for v in &args[..3] {
        if !is_list(ctx.arena, ctx.types, *v) {
            return Err(type_error(ctx, state, "select() argument must be a list"));
        }
    }
    let timeout = if n_args > 3 { args[3] } else { Value::None };
    let secs = match timeout {
        Value::None => None,
        v => match obj_get_float_maybe(ctx, ctx.types, v) {
            Some(f) => Some(f),
            None => {
                let pe = ctx.record_typed(PrimError::FloatConversion);
                return Err(exc(ctx, state, pe));
            }
        },
    };
    if let Some(f) = secs {
        if f < 0.0 || f.is_nan() {
            return Err(errno_exc(ctx, state, EINVAL));
        }
    }
    // Resolve all three lists up front (CPython validates
    // every fd before waiting).
    let mut r_entries = Vec::new();
    for item in list_items_vec(ctx, args[0]) {
        r_entries.push(resolve_read(ctx, state, item)?);
    }
    let mut w_items = Vec::new();
    for item in list_items_vec(ctx, args[1]) {
        resolve_validate(ctx, state, item)?;
        w_items.push(item);
    }
    for item in list_items_vec(ctx, args[2]) {
        resolve_validate(ctx, state, item)?;
    }
    // All three lists empty: sleep the timeout, return three
    // empty lists (the `select(0, NULL, NULL, NULL, tv)`
    // behaviour — the one case the old default path had).
    if r_entries.is_empty() && w_items.is_empty() {
        sleep_timeout(secs);
        return Ok(result(ctx, Vec::new(), Vec::new()));
    }
    // Probe loop: ready = a read entry with buffered bytes /
    // EOF / a dead monitor, or a non-empty wlist
    // (always-ready approximation).
    let deadline = match secs {
        Some(f) if f.is_finite() => {
            Some(Instant::now() + Duration::from_secs_f64(f.min(MAX_TIMEOUT_SECS)))
        }
        _ => None,
    };
    loop {
        let mut ready_r = Vec::new();
        for e in &r_entries {
            if e.ready() {
                ready_r.push(e.item());
            }
        }
        if !ready_r.is_empty() || !w_items.is_empty() {
            return Ok(result(ctx, ready_r, std::mem::take(&mut w_items)));
        }
        match deadline {
            None => std::thread::sleep(PROBE_QUANTUM),
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return Ok(result(ctx, Vec::new(), Vec::new()));
                }
                std::thread::sleep((d - now).min(PROBE_QUANTUM));
            }
        }
    }
}

/// The intercept entry (`call_function_ctx` calls this when
/// `fun == state.select_builtin`).
pub fn call_select_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    select_ctx(ctx, state, n_args, n_kw, args)
}

/// The `FunBuiltin` slot for `select.select`. Unreachable
/// through the VM — `call_function_ctx` intercepts the
/// builtin by GcRef identity first (it needs
/// `state.os_fds`, which this slot cannot see). Kept as a
/// clear error for any direct call (the D2 `os.kill` stub
/// precedent: a stub that raises beats a silent wrong
/// answer).
fn select_select_stub(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    ctx.record_raise(
        "NotImplementedError",
        Some("select.select called without VM state"),
    );
    Err(PyError::PyRaise)
}

/// `build_select_module` (`modselect.zig` table): 2 entries.
/// Returns `(module, select_fn)` — the caller stores the fn
/// in `VmState.select_builtin` for the GcRef-identity
/// intercept (the `build_gc_module` shape).
pub fn build_select_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, Value)> {
    let (module, dict) = new_d4_module(ctx, "select", module_type);
    let select = mk_fun(ctx, fun_builtin_var, "select", 3, 4, select_select_stub, None);
    store(ctx, dict, "select", select);
    fix_dict(ctx, dict);
    Ok((module, select))
}
