//! The `select` builtin module (`src/modules/modselect.zig`, 216 lines).
//!
//! Ported from the Zig (itself a port of vendor `py/modselect.c`):
//! only `select` — 2 entries (`__name__` + `select`).
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d4_result.txt` §2):
//! - `select(r, w, x[, timeout])` takes three lists of fd
//!   ints; fds `< 0` or `>= FD_SETSIZE` are silently skipped
//!   (the Zig's `fdSet` guard); `timeout` is `None` (block
//!   forever) or seconds (float or int); a negative timeout
//!   raises `OSError(22,)` (`EINVAL`); fewer than 3 args
//!   raises `TypeError("argument num/types mismatch")` (the
//!   sig is `(3, 4)` — the call machinery checks it).
//! - `select([], [], [], 0)` → `([], [], [])`.
//!
//! Two paths (G0):
//! - Default (feature `unsafe-ffi` OFF, pure `std` — there is NO
//!   `select(2)` / `poll(2)` API in `std`): the empty-lists case
//!   is implemented exactly — with `nfds == 0`, `select(2)`
//!   returns after the timeout with three empty sets — so this
//!   port sleeps the timeout and returns three empty lists. (A
//!   `None` timeout sleeps effectively forever, as the reference
//!   blocks forever.) Any non-empty list raises
//!   `NotImplementedError` (`"select.select is not implemented
//!   (no std select API)"`, the D2 `os.kill` stub precedent).
//!   A listener-accept check would destructively consume pending
//!   connections and FIFO/stdin readiness is unknowable without
//!   the fd API — not attempted. The integration tests
//!   (`tests/test_integration_os.py:662+`) exercise exactly
//!   the empty-lists case plus real-fd cases; the real-fd
//!   cases are the documented gap of the default build.
//! - With `--features unsafe-ffi`: the real POSIX `select(2)`
//!   via `extern "C"` (zero new dependencies — the declarations
//!   are written by hand, no libc crate), following
//!   `modselect.zig::py_select` step for step: `populateSet`
//!   skips fd < 0 / >= FD_SETSIZE, `nfds` is max fd + 1, timeout
//!   is a `timeval`, `n < 0` raises `OSError(errno)` with no
//!   EINTR retry (the Zig doesn't retry either), results keep
//!   the input order and the original fd objects. `unsafe` is
//!   confined to the FFI boundary in this file (extern block +
//!   the `select` call + the errno read), each `unsafe` block
//!   carrying an FFI-invariant comment (the F1 rule).
//! - Divergences (both paths): a non-list argument
//!   (`select(1, [], [], 0)`) ABORTS the reference
//!   (`incorrect alignment` panic in `MpObjList.fromObj`,
//!   rc=134); Rust raises `TypeError`
//!   (`"select() argument must be a list"`). An infinite
//!   timeout aborts the reference (`@intFromFloat` panic);
//!   Rust sleeps `Duration::MAX` (default path) or saturates
//!   `tv_sec` to `i64::MAX` (feature path). A NaN timeout
//!   raises `OSError(EINVAL)` (same shape as `time.sleep`'s NaN).

#[cfg(feature = "unsafe-ffi")]
use std::os::raw::c_int;
#[cfg(not(feature = "unsafe-ffi"))]
use std::time::Duration;

use crate::memory::gc::CallCtx;
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
#[cfg(feature = "unsafe-ffi")]
use crate::objects::containers::objlist::list_items_vec;
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::obj_get_float_maybe;
#[cfg(feature = "unsafe-ffi")]
use crate::objects::obj::obj_get_int;
use crate::objects::primitives::ops::{is_list, PrimError};
use crate::objects::value::{GcRef, Value};
#[cfg(not(feature = "unsafe-ffi"))]
use crate::objects::value::Obj;
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

/// `EINVAL` (`modselect.zig`: negative timeout).
const EINVAL: i32 = 22;

/// `FD_SETSIZE` — the Zig's `fdSet` guard (`modselect.zig`:
/// `populateSet` skips fd < 0 or fd >= FD_SETSIZE).
/// Feature path only; the default path never builds an fd_set.
#[cfg(feature = "unsafe-ffi")]
const FD_SETSIZE: i64 = 1024;

/// A glibc `fd_set`: 1024 bits = 128 bytes. `c_long` is 8 bytes on
/// both Linux x86_64 and Linux aarch64, so the layout is identical
/// on both (`modselect.zig` computes `bits_per_long` the same way);
/// bit `fd` is at word `fd / 64`, bit `fd % 64`.
#[cfg(feature = "unsafe-ffi")]
type FdSet = [u64; 16];

/// Set bit `fd` (`FD_SET`). Caller guarantees `0 <= fd < 1024`.
#[cfg(feature = "unsafe-ffi")]
fn fd_set(set: &mut FdSet, fd: i64) {
    set[(fd / 64) as usize] |= 1u64 << (fd % 64);
}

/// Test bit `fd` (`FD_ISSET`). Caller guarantees `0 <= fd < 1024`.
#[cfg(feature = "unsafe-ffi")]
fn fd_is_set(set: &FdSet, fd: i64) -> bool {
    set[(fd / 64) as usize] & (1u64 << (fd % 64)) != 0
}

/// glibc `struct timeval`: `time_t` + `suseconds_t`, 8 + 8 bytes
/// on both Linux x86_64 and Linux aarch64.
#[cfg(feature = "unsafe-ffi")]
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

/// Timeout seconds → timeval, following `modselect.zig` (`whole =
/// trunc(secs)`, `tv_usec = (secs - whole) * 1_000_000`); the usec
/// field is clamped to `[0, 999999]` so float error can never
/// produce 1_000_000. `f64 as i64` saturates — huge/infinite
/// timeouts land on `i64::MAX` (the reference aborts there; Rust
/// sleeps effectively forever instead — the pre-G0 divergence,
/// kept as-is).
#[cfg(feature = "unsafe-ffi")]
fn timeval_from_secs(secs: f64) -> Timeval {
    let whole = secs.trunc();
    let usec = ((secs - whole) * 1_000_000.0) as i64;
    Timeval {
        tv_sec: whole as i64,
        tv_usec: usec.clamp(0, 999_999),
    }
}

/// Raw POSIX `select(2)` + errno access (glibc, Linux x86_64 and
/// aarch64). Only compiled with the `unsafe-ffi` feature; zero new
/// dependencies — the declarations are written by hand, no libc crate.
#[cfg(feature = "unsafe-ffi")]
mod ffi {
    use std::os::raw::c_int;

    use super::{FdSet, Timeval};

    // FFI invariant: `select` is the glibc POSIX select(2). Each
    // set pointer is either null or points at a caller-owned `FdSet`
    // that outlives the call; `nfds` is the highest fd in any set +
    // 1 (0 when all sets are null); the timeout pointer is either
    // null or points at a caller-owned `Timeval` that outlives the
    // call. The call may block; it writes only into the pointed-to
    // sets.
    extern "C" {
        pub fn select(
            nfds: c_int,
            readfds: *mut FdSet,
            writefds: *mut FdSet,
            exceptfds: *mut FdSet,
            timeout: *mut Timeval,
        ) -> c_int;
    }

    // FFI invariant: glibc's `__errno_location` returns a pointer
    // to the calling thread's errno cell; it is never null, and the
    // cell is only read (never written) by the caller.
    extern "C" {
        pub fn __errno_location() -> *mut c_int;
    }
}

/// The length of a list value (`None` when not a list —
/// the caller checks `is_list` first). Default path only.
#[cfg(not(feature = "unsafe-ffi"))]
fn list_len(ctx: &CallCtx, v: Value) -> Option<usize> {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::List(l) => Some(l.items.len()),
            _ => None,
        },
        _ => None,
    }
}

/// The real `select(2)` path (`unsafe-ffi` only), following
/// `modselect.zig::py_select` step for step: `populateSet` skips
/// fd < 0 / >= FD_SETSIZE, `nfds` is max fd + 1, timeout is a
/// timeval, `n < 0` raises OSError(errno) with no EINTR retry,
/// results keep the input order and the original fd objects.
#[cfg(feature = "unsafe-ffi")]
fn select_real(ctx: &mut CallCtx, args: &[Value], secs: Option<f64>) -> PyResult<Value> {
    let mut rset: FdSet = [0; 16];
    let mut wset: FdSet = [0; 16];
    let mut xset: FdSet = [0; 16];
    let mut maxfd: i64 = -1;
    for (list, set) in [
        (args[0], &mut rset),
        (args[1], &mut wset),
        (args[2], &mut xset),
    ] {
        // `mp_obj_get_int` per item, exactly like the Zig's
        // `populateSet` (`@intCast(mp_obj_get_int(...))`).
        for item in list_items_vec(ctx, list) {
            let fd = obj_get_int(ctx, ctx.types, item).map_err(|e| ctx.record_typed(e))?;
            if fd < 0 || fd >= FD_SETSIZE {
                continue;
            }
            fd_set(set, fd);
            if fd > maxfd {
                maxfd = fd;
            }
        }
    }
    let nfds: c_int = (maxfd + 1) as c_int;
    let mut tv = secs.map(timeval_from_secs);
    // All lists empty → nfds == 0 and all three sets NULL:
    // `select(0, NULL, NULL, NULL, tv)` just sleeps the timeout,
    // matching the default path's empty-lists behaviour.
    let (r_ptr, w_ptr, x_ptr): (*mut FdSet, *mut FdSet, *mut FdSet) = if maxfd < 0 {
        (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } else {
        (&mut rset, &mut wset, &mut xset)
    };
    let tv_ptr = match &mut tv {
        Some(t) => t as *mut Timeval,
        None => std::ptr::null_mut(),
    };
    // FFI invariant: all pointers satisfy the `ffi::select`
    // contract above (null or caller-owned, live for the call);
    // `nfds` is max fd + 1. No Rust reference is held across the
    // call, so the kernel writing into the sets cannot alias
    // anything the borrow checker protects.
    let n = unsafe { ffi::select(nfds, r_ptr, w_ptr, x_ptr, tv_ptr) };
    if n < 0 {
        // FFI invariant: errno is read immediately after the failed
        // call, before any other call can touch this thread's errno
        // cell; `__errno_location` never returns null (see above).
        let errno = unsafe { *ffi::__errno_location() };
        return Err(raise_os_error(ctx, errno));
    }
    // `filterReady` per list, in input order, keeping the original
    // fd objects (`list.items[i]` itself, like the Zig).
    let mut out: [Vec<Value>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (i, (list, set)) in [(args[0], &rset), (args[1], &wset), (args[2], &xset)]
        .into_iter()
        .enumerate()
    {
        for item in list_items_vec(ctx, list) {
            let fd = obj_get_int(ctx, ctx.types, item).map_err(|e| ctx.record_typed(e))?;
            if fd < 0 || fd >= FD_SETSIZE {
                continue;
            }
            if fd_is_set(set, fd) {
                out[i].push(item);
            }
        }
    }
    let r = Value::Heap(new_list(ctx, ctx.types.list, &out[0]));
    let w = Value::Heap(new_list(ctx, ctx.types.list, &out[1]));
    let x = Value::Heap(new_list(ctx, ctx.types.list, &out[2]));
    let tuple = Value::Heap(new_tuple(ctx, ctx.types.tuple, &[r, w, x]));
    Ok(tuple)
}

/// `py_select` (`modselect.zig`): `select(rlist, wlist,
/// xlist[, timeout])` — see the module docs for the two paths.
fn select_select_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    // Shared pre-checks (both paths, unchanged semantics).
    for v in &args[..3] {
        if !is_list(ctx.arena, ctx.types, *v) {
            ctx.record_raise("TypeError", Some("select() argument must be a list"));
            return Err(PyError::PyRaise);
        }
    }
    let timeout = if args.len() > 3 { args[3] } else { Value::None };
    let secs = match timeout {
        Value::None => None,
        v => match obj_get_float_maybe(ctx, ctx.types, v) {
            Some(f) => Some(f),
            None => return Err(ctx.record_typed(PrimError::FloatConversion)),
        },
    };
    if let Some(f) = secs {
        if f < 0.0 || f.is_nan() {
            return Err(raise_os_error(ctx, EINVAL));
        }
    }
    #[cfg(feature = "unsafe-ffi")]
    {
        return select_real(ctx, args, secs);
    }
    #[cfg(not(feature = "unsafe-ffi"))]
    {
        if args[..3].iter().any(|v| list_len(ctx, *v).unwrap_or(0) > 0) {
            ctx.record_raise(
                "NotImplementedError",
                Some("select.select is not implemented (no std select API)"),
            );
            return Err(PyError::PyRaise);
        }
        // Empty lists: `select(2)` with `nfds == 0` returns after
        // the timeout with three empty sets — sleep the timeout
        // (`None` = forever, `Duration::MAX`).
        match secs {
            Some(f) if f.is_finite() => std::thread::sleep(Duration::from_secs_f64(f)),
            _ => std::thread::sleep(Duration::MAX),
        }
        // Three DISTINCT empty lists (one shared list would
        // alias mutations across the three result positions).
        let r = Value::Heap(new_list(ctx, ctx.types.list, &[]));
        let w = Value::Heap(new_list(ctx, ctx.types.list, &[]));
        let x = Value::Heap(new_list(ctx, ctx.types.list, &[]));
        let tuple = Value::Heap(new_tuple(ctx, ctx.types.tuple, &[r, w, x]));
        Ok(tuple)
    }
}

/// `build_select_module` (`modselect.zig` table): 2 entries.
pub fn build_select_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let (module, dict) = new_d4_module(ctx, "select", module_type);
    let select = mk_fun(ctx, fun_builtin_var, "select", 3, 4, select_select_builtin, None);
    store(ctx, dict, "select", select);
    fix_dict(ctx, dict);
    Ok(module)
}

#[cfg(all(test, feature = "unsafe-ffi"))]
mod tests {
    use super::*;

    #[test]
    fn fd_set_bits() {
        let mut s: FdSet = [0; 16];
        fd_set(&mut s, 0);
        fd_set(&mut s, 63);
        fd_set(&mut s, 64);
        fd_set(&mut s, 1023);
        assert!(fd_is_set(&s, 0));
        assert!(fd_is_set(&s, 63));
        assert!(fd_is_set(&s, 64));
        assert!(fd_is_set(&s, 1023));
        assert!(!fd_is_set(&s, 1));
        assert!(!fd_is_set(&s, 62));
        assert!(!fd_is_set(&s, 65));
        assert!(!fd_is_set(&s, 1022));
        assert_eq!(s[0], (1u64 << 0) | (1u64 << 63));
        assert_eq!(s[1], 1u64);
        assert_eq!(s[15], 1u64 << 63);
        // A fresh set has no bits set (`FD_ZERO` is the `[0; 16]`
        // initialiser in `select_real`).
        let fresh: FdSet = [0; 16];
        assert!(fresh.iter().all(|&w| w == 0));
    }

    #[test]
    fn timeval_conversion() {
        let t = timeval_from_secs(0.2);
        assert_eq!((t.tv_sec, t.tv_usec), (0, 200_000));
        let t = timeval_from_secs(1.5);
        assert_eq!((t.tv_sec, t.tv_usec), (1, 500_000));
        let t = timeval_from_secs(0.0);
        assert_eq!((t.tv_sec, t.tv_usec), (0, 0));
        // Float error must never produce tv_usec == 1_000_000.
        let t = timeval_from_secs(2.9999999);
        assert_eq!(t.tv_sec, 2);
        assert!((0..=999_999).contains(&t.tv_usec));
    }
}
