//! The embedding API (`entry/api.zig`, ADR 022): the `ppy_*`
//! C API from `include/petitpy.h`, as a Rust `cdylib` +
//! `staticlib`.
//!
//! The original keeps interpreter state in process-wide Zig
//! globals (`interps`, `current`, `globals`, the recursive
//! pthread mutex) and swaps it per interpreter (`save` /
//! `load` / `switchTo`). Rust needs none of that: every piece
//! of VM state already lives in the `Vm` (`arena`, `pool`,
//! `code_store`, `modules`, `globals`, `sys_path` …), so
//! each `ppy_interp` just owns its own `Vm` (`Box<Vm>`,
//! heap-allocated so the C handle stays stable). The only
//! process-wide pieces are the interpreter-slot table (an
//! `AtomicU64` bitmask — no second mutex) and the reentrant
//! lock below.
//!
//! Handles are `u32` tags: `interp_index << SLOT_BITS |
//! slot`, `SLOT_BITS = 26`, slot 0 never handed out (api.zig
//! `SLOT_BITS`/`SLOT_MASK`). A handle names a slot in the
//! interpreter's handle table — a `Vec<Option<Value>>` kept
//! as `RootSet.handles`, so the GC traces handle values
//! (api.zig's `roots` are the slots themselves; the Rust
//! `GcArena::collect` takes an explicit `RootSet`, and
//! `handles` is a leg of its `iter()` — see `memory/gc.rs`).
//! A free slot holds `None`; freeing appends the slot to the
//! free list (a scan for the first free slot — same observable
//! behavior as the Zig's explicit free list: freed slots are
//! reused, slot 0 is never handed out).
//!
//! The `run` bracket (api.zig `run` / `produce`): take the
//! reentrant lock, set the current interpreter + the output
//! sink for this thread, clear `interp.err`, run the body,
//! and on a raise store the exception in `interp.err`
//! (SystemExit → `PPY_EXIT` + exit code, else `PPY_ERROR`).
//! The Zig's NLR trampoline is a plain `Result` here — a
//! body returns `Ok(value)` or `Err(exception)`.
//!
//! REENTRANCY (read before touching this file): a host
//! function may call the API again (nested `ppy_*` from
//! `hostCall`), which creates a second `&mut` path to the
//! same `Vm`/`Interp` through the raw `*mut Interp` while
//! the outer call's `&mut` borrow is suspended. The calls
//! are sequential and lock-serialized, and the outer borrow
//! is never used while a nested call is active — the same
//! guarantee the Zig relies on via its process-wide globals.
//! The alternative (restructuring the VM to `UnsafeCell`
//! interior mutability) is out of scope for this lane.
//!
//! `unsafe` in this file is confined to the C-ABI boundary:
//! reading C strings / pointers, writing through
//! out-pointers, and the `extern "C"` exports themselves.
//! Every `unsafe` block carries a one-line comment saying
//! which FFI invariant it upholds (the lane's one exception
//! to the no-`unsafe` rule; the VM core stays `unsafe`-free).
//! The FFI invariants the HOST upholds (from
//! `include/petitpy.h`): pointers passed in are valid for
//! the call; `ppy_config` strings are NUL-terminated;
//! `write` callbacks + userdata stay valid while the
//! interpreter lives; a `ppy_interp` is used by only one
//! thread at a time except through the lock (host functions
//! may re-enter from the same thread).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::compiler::parser::InputKind;
use crate::entry::systemexit::{exit_code, is_system_exit};
use crate::memory::gc::{CallCtx, RootSet};
use crate::objects::callables::objboundmeth::new_bound_meth;
use crate::objects::containers::objdict::{dict_get_value, dict_store};
use crate::objects::containers::objlist::new_list;
use crate::objects::containers::objtuple::new_tuple;
use crate::objects::obj::{
    is_bytes, is_str, obj_get_float_maybe, obj_get_int_maybe, obj_get_type, obj_is_true, obj_len,
};
use crate::objects::obj_ops::{obj_print_exception, obj_subscr, obj_type_name};
use crate::objects::objtype::objexcept::new_exception_msg;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::objtype::subclass::is_subclass_fast;
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{PrintKind, SubscrArg};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{DictData, MapData, Obj, Value};
use crate::runtime::builtinimport::import_impl;
use crate::runtime::vm::{Vm, VmState};
use crate::runtime::vm_ops::{
    call_function, exc_from_obj_error, exc_from_pyerror, type_error, value_error,
};
use crate::runtime::vm_rt_name::{load_attr_ctx, store_attr_ctx};
use crate::runtime::vm_seams::try_instance_subscr;
use crate::support::output::{set_thread_sink, CSink, PpyWriteFn};

/// `ppy_status` (`petitpy.h`): 0 = ok, 1 = error, 2 = exit.
pub const PPY_OK: c_int = 0;
pub const PPY_ERROR: c_int = 1;
pub const PPY_EXIT: c_int = 2;

/// `ppy_handle` (`petitpy.h`): an opaque 32-bit handle.
pub type PpyHandle = u32;

/// `ppy_write_fn` (`petitpy.h`): the host's output sink —
/// `write(userdata, fd, data, len)`.
///
/// `ppy_function` (`petitpy.h`): a host function —
/// `fn(py, userdata, argc, argv, result) -> ppy_status`.
pub type PpyFunction = unsafe extern "C" fn(
    *mut Interp,
    *mut c_void,
    usize,
    *const PpyHandle,
    *mut PpyHandle,
) -> c_int;

/// `ppy_config` (`petitpy.h`): `path` / `argv` are arrays
/// of `path_len` / `argc` NUL-terminated strings; `write` is
/// NULL for the process stdout/stderr.
#[repr(C)]
pub struct PpyConfig {
    pub path: *const *const c_char,
    pub path_len: usize,
    pub argv: *const *const c_char,
    pub argc: usize,
    pub write: Option<PpyWriteFn>,
    pub write_userdata: *mut c_void,
}

/// Handle-tag layout (api.zig): 26 slot bits, the
/// interpreter index in the high bits; at most 64
/// interpreters.
const SLOT_BITS: u32 = 26;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;
const MAX_INTERPS: u32 = 64;

/// One host function (`api.zig` `Host`): the C callback +
/// its userdata.
struct Host {
    fun: PpyFunction,
    userdata: *mut c_void,
}

/// An interpreter (`api.zig` `Interp`): its own `Vm` (its
/// own arena, qstr pool, code store, modules, globals —
/// the Zig's `save`/`load`/`switchTo` machinery is NOT
/// ported; it exists only because the original keeps
/// interpreter state in process-wide variables), its handle
/// tag, its `__main__` globals dict, its host functions,
/// the stored error, its exit code, its output sink, and a
/// cache of type-name C strings (the returned pointer must
/// stay valid for the interpreter's lifetime).
pub struct Interp {
    vm: Box<Vm>,
    index: u32,
    tag: u32,
    main_globals: crate::objects::value::GcRef,
    hosts: Vec<Host>,
    err: Option<Value>,
    exit_code: c_int,
    write: Option<CSink>,
    type_names: HashMap<String, CString>,
}

/// The reentrant process-wide lock (api.zig's ONE recursive
/// pthread mutex; `std::Mutex` is NOT recursive, so
/// reentrancy is built from a plain `Mutex` + the owning
/// thread id + a depth counter — the header's deadlock note
/// ("a host function that waits for another thread which
/// itself calls the API") is preserved in the doc comment
/// above). The lock is taken on the outermost entry from a
/// new thread; nested calls from the same thread (a host
/// function calling the API) bump the depth and skip the
/// mutex.
static PROCESS_LOCK: Mutex<()> = Mutex::new(());
/// The thread id holding the lock, 0 when free (a small
/// number assigned per thread on first use — see
/// `thread_num`; `ThreadId::as_u64` is unstable, so the
/// lock does not use `ThreadId` at all).
static LOCK_OWNER: AtomicU64 = AtomicU64::new(0);
/// Nesting depth of the owning thread's lock.
static LOCK_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// The live-interpreter bitmask (api.zig `interps`): bit `i`
/// set while interpreter `i` is alive.
static INTERP_BITS: AtomicU64 = AtomicU64::new(0);

// The current interpreter for THIS thread (api.zig
// `current`): set by the API bracket, so `call_host_ctx`
// (reached from VM dispatch, which has no `Interp` in
// scope) can find the host table. Cleared on exit.
thread_local! {
    static CURRENT_INTERP: Cell<*mut Interp> = const { Cell::new(std::ptr::null_mut()) };
}

// The `MutexGuard` held by the outermost lock entry on
// THIS thread (the mutex is non-recursive, so the guard
// must be kept alive until the outermost exit — it cannot
// live on the stack of `api_lock`, which returns).
// `MutexGuard<'static, ()>` is sound here: the mutex is a
// `static` and the guard is only ever dropped by the same
// thread that took it, at the outermost `api_unlock`.
thread_local! {
    static LOCK_GUARD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
}

// A stable per-thread number for the reentrant lock:
// `ThreadId::as_u64` is unstable (`#67939`), so each
// thread takes the next number from a counter on first
// use (0 = never assigned; the counter starts at 1, so 0
// is never handed out and doubles as "lock free").
thread_local! {
    static THREAD_NUM: Cell<u64> = const { Cell::new(0) };
}
static NEXT_THREAD_NUM: AtomicU64 = AtomicU64::new(1);

fn thread_num() -> u64 {
    THREAD_NUM.with(|t| {
        let n = t.get();
        if n != 0 {
            return n;
        }
        let n = NEXT_THREAD_NUM.fetch_add(1, Ordering::Relaxed);
        t.set(n);
        n
    })
}

/// Take the process-wide lock (reentrant for the owning
/// thread; blocking for any other thread).
fn api_lock() {
    let tid = thread_num();
    if LOCK_OWNER.load(Ordering::Acquire) == tid {
        LOCK_DEPTH.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let guard = PROCESS_LOCK.lock().expect("embedding lock poisoned");
    LOCK_OWNER.store(tid, Ordering::Release);
    LOCK_DEPTH.store(1, Ordering::Relaxed);
    LOCK_GUARD.with(|g| *g.borrow_mut() = Some(guard));
}

/// Release the process-wide lock (a nested exit just drops
/// the depth; the outermost exit drops the guard).
fn api_unlock() {
    let depth = LOCK_DEPTH.fetch_sub(1, Ordering::Relaxed);
    if depth > 1 {
        return;
    }
    LOCK_OWNER.store(0, Ordering::Release);
    LOCK_GUARD.with(|g| *g.borrow_mut() = None);
}

/// RAII guard for the thread-local current interpreter
/// (restores the previous one — a nested API call from a
/// host function sees the same interpreter, so the previous
/// value is always the same pointer, but the restore keeps
/// the invariant exact).
struct CurrentGuard {
    prev: *mut Interp,
}

impl CurrentGuard {
    fn new(py: *mut Interp) -> Self {
        let prev = CURRENT_INTERP.with(|c| c.replace(py));
        CurrentGuard { prev }
    }
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        CURRENT_INTERP.with(|c| c.set(self.prev));
    }
}

/// RAII guard for the thread-local output sink (restores the
/// previous sink — a nested API call from a host function
/// installs the same sink again, so the previous value is
/// always the same, but the restore keeps the invariant
/// exact).
struct SinkGuard {
    prev: Option<CSink>,
}

impl SinkGuard {
    fn new(sink: Option<CSink>) -> Self {
        let prev = set_thread_sink(sink);
        SinkGuard { prev }
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        set_thread_sink(self.prev);
    }
}

/// Allocate a free interpreter index (a clear bit in
/// `INTERP_BITS`), or `None` when all 64 are live.
fn alloc_interp_index() -> Option<u32> {
    debug_assert_eq!(MAX_INTERPS, u64::BITS, "one bit per interpreter");
    loop {
        let bits = INTERP_BITS.load(Ordering::Acquire);
        // All 64 bits are interpreter slots (`MAX_INTERPS ==
        // 64 == u64::BITS`), so the free mask is just `!bits`
        // — `1u64 << 64` would overflow.
        let free = !bits;
        if free == 0 {
            return None;
        }
        let idx = free.trailing_zeros();
        let next = bits | (1u64 << idx);
        if INTERP_BITS
            .compare_exchange_weak(bits, next, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return Some(idx);
        }
    }
}

/// Release an interpreter index.
fn free_interp_index(idx: u32) {
    INTERP_BITS.fetch_and(!(1u64 << idx), Ordering::AcqRel);
}

/// The current interpreter for this thread, or NULL outside
/// an API call.
fn current_interp() -> *mut Interp {
    CURRENT_INTERP.with(|c| c.get())
}

/// Allocate a handle for `value` in `roots.handles`: the
/// first free slot (slot 0 is never handed out — it stays
/// `None` forever), growing the table when full. The value
/// is a GC root from this moment (the slot is a leg of
/// `RootSet::iter()`).
fn new_handle(roots: &mut RootSet, tag: u32, value: Value) -> PpyHandle {
    let handles = &mut roots.handles;
    // Slot 0 is reserved: keep it `None` and never hand it out.
    if handles.is_empty() {
        handles.push(None);
    }
    for (slot, entry) in handles.iter_mut().enumerate().skip(1) {
        if entry.is_none() {
            *entry = Some(value);
            return tag | (slot as u32);
        }
    }
    handles.push(Some(value));
    tag | ((handles.len() - 1) as u32)
}

/// The value `h` names, or `None` when `h` is not a live
/// handle of this interpreter (bad tag, slot 0, out of
/// range, or a freed slot — api.zig `slotOf`).
fn handle_value(roots: &RootSet, tag: u32, h: PpyHandle) -> Option<Value> {
    if h & !SLOT_MASK != tag {
        return None;
    }
    let slot = (h & SLOT_MASK) as usize;
    if slot == 0 {
        return None;
    }
    roots.handles.get(slot).and_then(|e| *e)
}

/// Free `h` (api.zig `ppy_release`): a bad handle is a
/// silent no-op (the Zig checks `slotOf` and returns).
fn release_handle(roots: &mut RootSet, tag: u32, h: PpyHandle) {
    if h & !SLOT_MASK != tag {
        return;
    }
    let slot = (h & SLOT_MASK) as usize;
    if slot == 0 {
        return;
    }
    if let Some(entry) = roots.handles.get_mut(slot) {
        *entry = None;
    }
}

/// The `run` / `produce` bracket (api.zig `run` +
/// `produce`): lock, set the current interpreter + the
/// output sink for this thread, clear `interp.err`, run the
/// body, and map the result to a status — storing the
/// exception in `interp.err` on failure (SystemExit →
/// `PPY_EXIT` + exit code, else `PPY_ERROR`). `out` is
/// zeroed FIRST (before the body runs — the C tests check
/// `r == 0` after a failed `ppy_get_global`), and a new
/// handle is stored in it on success when the body returns
/// `Some(value)` (`None` = the call has no handle to hand
/// out). A NULL `out` means the caller doesn't want a
/// handle. `py` must be a live interpreter from `ppy_new`
/// (the caller upholds this — `ppy_*` wrappers check for
/// NULL before calling).
fn run_bracket(
    py: *mut Interp,
    out: *mut PpyHandle,
    body: impl FnOnce(&mut Interp) -> Result<Option<Value>, Value>,
) -> c_int {
    api_lock();
    // FFI invariant: `py` is a live interpreter from `ppy_new`
    // (every `ppy_*` wrapper checks for NULL first).
    let interp = unsafe { &mut *py };
    let _current = CurrentGuard::new(py);
    let _sink = SinkGuard::new(interp.write);
    if !out.is_null() {
        // FFI invariant: `out` points to a writable `ppy_handle`.
        unsafe { *out = 0 };
    }
    interp.err = None;
    let result = body(interp);
    match result {
        Ok(Some(value)) => {
            if !out.is_null() {
                let h = new_handle(&mut interp.vm.roots, interp.tag, value);
                // FFI invariant: `out` points to a writable `ppy_handle`.
                unsafe { *out = h };
            }
            api_unlock();
            PPY_OK
        }
        Ok(None) => {
            api_unlock();
            PPY_OK
        }
        Err(exc) => {
            interp.err = Some(exc);
            let code = {
                let (ctx, state) = interp.vm.split();
                if is_system_exit(&ctx, state, exc) {
                    Some(exit_code(&ctx, exc))
                } else {
                    None
                }
            };
            api_unlock();
            match code {
                Some(c) => {
                    // The split borrow above has ended, so the
                    // exit code can be stored back now (`interp`
                    // is still in scope — no raw write needed).
                    interp.exit_code = c;
                    PPY_EXIT
                }
                None => PPY_ERROR,
            }
        }
    }
}

/// `run_bracket` for bodies that hand a value back as a
/// handle (api.zig `produce`).
fn run_produce(
    py: *mut Interp,
    out: *mut PpyHandle,
    body: impl FnOnce(&mut Interp) -> Result<Value, Value>,
) -> c_int {
    run_bracket(py, out, |interp| body(interp).map(Some))
}

/// `run_bracket` for bodies that write their result through
/// out-pointers and hand no handle back (api.zig `run`
/// with a `void` body — `ppy_to_*`, `ppy_len`).
fn run_status(py: *mut Interp, body: impl FnOnce(&mut Interp) -> Result<(), Value>) -> c_int {
    run_bracket(py, std::ptr::null_mut(), |interp| {
        body(interp).map(|()| None)
    })
}

/// Intern `bytes` as a qstr id (api.zig `qstr`); an
/// interning failure is `MemoryError("out of memory")`.
fn intern_qstr_id(ctx: &mut CallCtx, state: &VmState, bytes: &[u8]) -> Result<u32, Value> {
    ctx.pool.from_strn(bytes).map(u32::from).map_err(|_| {
        new_exception_msg(ctx, state.exc.memory_error, "out of memory")
    })
}

/// Read a NUL-terminated C string as bytes.
/// FFI invariant: `p` is a valid NUL-terminated C string
/// from the host.
unsafe fn c_str_bytes<'a>(p: *const c_char) -> &'a [u8] {
    // FFI invariant: `p` is a valid NUL-terminated C string.
    unsafe { CStr::from_ptr(p).to_bytes() }
}

/// Read a `ppy_config` (NULL = the default: empty
/// sys.path / argv, process stdout/stderr). The Zig's
/// `ppy_new` takes the paths / argv as-is; the Rust side
/// copies them into `String`s (sys.path / sys.argv are
/// `Vec<String>`).
unsafe fn read_config(config: *const PpyConfig) -> (Vec<String>, Vec<String>, Option<CSink>) {
    if config.is_null() {
        return (Vec::new(), Vec::new(), None);
    }
    // FFI invariant: `config` points to a valid `ppy_config`.
    let cfg = unsafe { &*config };
    // FFI invariant: `cfg` is a valid `ppy_config` — its
    // arrays hold `path_len` / `argc` valid pointers.
    let paths = unsafe { read_str_array(cfg.path, cfg.path_len) };
    let argv = unsafe { read_str_array(cfg.argv, cfg.argc) };
    let write = cfg
        .write
        .map(|f| CSink { write: f, userdata: cfg.write_userdata });
    (paths, argv, write)
}

/// Read a `ppy_config` string array (`n` entries) as
/// `String`s. A NULL array is empty; a NULL entry is an
/// empty string (defensive — the Zig would fault on NULL).
unsafe fn read_str_array(arr: *const *const c_char, n: usize) -> Vec<String> {
    if arr.is_null() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // FFI invariant: `arr` points to `n` valid pointers.
        let p = unsafe { *arr.add(i) };
        if p.is_null() {
            out.push(String::new());
        } else {
            // FFI invariant: `p` is a non-NULL entry of the
            // config array, hence a valid NUL-terminated C string.
            out.push(String::from_utf8_lossy(unsafe { c_str_bytes(p) }).into_owned());
        }
    }
    out
}

/// The object a pre-resolved handle names, or a
/// `ValueError("invalid handle")` (the `obj_of` twin for
/// bodies that resolve handles before splitting the VM —
/// resolving first avoids borrowing `ctx` both ways at once).
fn obj_or_invalid(
    ctx: &mut CallCtx,
    state: &VmState,
    v: Option<Value>,
) -> Result<Value, Value> {
    v.ok_or_else(|| value_error(ctx, state, "invalid handle"))
}

/// The host-call dispatch (api.zig `hostCall`), reached
/// from `call_function_ctx`'s interception of the shared
/// `host_call_builtin` (see `VmState.host_call_builtin`):
/// `args[0]` is the bound self — the host's index in the
/// interpreter's `hosts` list; `args[1..n_args]` are the
/// positionals. Keyword arguments are rejected exactly as
/// a plain var builtin rejects them (`call_fun_builtin`'s
/// `arg_check_num` with `takes_kw = false`). The arguments
/// become TEMPORARY handles for the call (released before
/// returning); a host returning non-OK takes the exception
/// from `ppy_raise()` or from the failed API call, else
/// `RuntimeError("host function failed")`; a result handle
/// of 0 means `None`.
pub(crate) fn call_host_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    if n_kw > 0 {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    let idx = match args.first() {
        Some(Value::SmallInt(i)) if *i >= 0 => *i as usize,
        _ => return Err(type_error(ctx, state, "host function called without a host index")),
    };
    let ip = current_interp();
    if ip.is_null() {
        return Err(new_exception_msg(
            ctx,
            state.exc.runtime_error,
            "host function called outside an API call",
        ));
    }
    // FFI invariant: the current interpreter pointer was set
    // by the API bracket from a live `ppy_new` result.
    let (fun, userdata, tag) = unsafe {
        match (&*ip).hosts.get(idx) {
            Some(h) => (h.fun, h.userdata, (&*ip).tag),
            None => {
                return Err(type_error(
                    ctx,
                    state,
                    "host function called without a host index",
                ));
            }
        }
    };
    let argc = n_args - 1;
    let mut argv: Vec<PpyHandle> = Vec::with_capacity(argc);
    for a in &args[1..n_args] {
        argv.push(new_handle(ctx.roots, tag, *a));
    }
    let mut result: PpyHandle = 0;
    // FFI invariant: the host callback + userdata are valid
    // for the interpreter's lifetime; `argv` holds `argc` handles.
    let status = unsafe { fun(ip, userdata, argc, argv.as_ptr(), &mut result) };
    for h in &argv {
        release_handle(ctx.roots, tag, *h);
    }
    if status != PPY_OK {
        // FFI invariant: the current interpreter pointer is
        // still the live interpreter from the bracket.
        let exc = unsafe { (&mut *ip).err.take() }.unwrap_or_else(|| {
            new_exception_msg(ctx, state.exc.runtime_error, "host function failed")
        });
        return Err(exc);
    }
    if result == 0 {
        return Ok(Value::None);
    }
    match handle_value(ctx.roots, tag, result) {
        Some(v) => {
            release_handle(ctx.roots, tag, result);
            Ok(v)
        }
        None => Err(value_error(ctx, state, "invalid handle")),
    }
}

/// `mp_obj_new_module` (objmodule.zig:81-99): look the name
/// up in `sys.modules` first and return the registered
/// object as-is; else create a module and register it in
/// both `state.modules` and `sys.modules` (so `import host`
/// finds it).
fn get_or_create_module(ctx: &mut CallCtx, state: &mut VmState, name_id: u32) -> Value {
    if let Some(mirror) = state.sys_modules_dict {
        if let Ok(v) = dict_get_value(ctx, Value::Heap(mirror), Value::Qstr(name_id)) {
            return v;
        }
    }
    let module_val = new_module(ctx, name_id, state.vm_types.module);
    if let Value::Heap(r) = module_val {
        state.modules.insert(name_id, r);
        if let Some(mirror) = state.sys_modules_dict {
            let _ = dict_store(ctx, Value::Heap(mirror), Value::Qstr(name_id), module_val);
        }
    }
    module_val
}

/// Run source as `eval`/`exec` in `__main__`'s namespace
/// (globals AND locals are the interpreter's `main_globals`
/// dict — api.zig `evalExec`).
fn eval_exec_body(
    interp: &mut Interp,
    src: &[u8],
    name: &str,
    kind: InputKind,
) -> Result<Value, Value> {
    let globals = interp.main_globals;
    interp
        .vm
        .eval_exec_named(src, kind, name, globals, globals)
}

/// Import the top-level package, then walk down the dotted
/// path with attribute loads to reach the leaf module
/// (api.zig `import`).
fn import_body(interp: &mut Interp, name_bytes: &[u8]) -> Result<Value, Value> {
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let types = ctx.types;
    let name_val = new_str(&mut ctx, types, name_bytes);
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    let args = [
        name_val,
        Value::None,
        Value::None,
        Value::None,
        Value::SmallInt(0),
    ];
    let top = import_impl(&mut ctx, state, &args, &mut intern)?;
    let mut module = top;
    for part in name_bytes.split(|&b| b == b'.').skip(1) {
        let part_id =
            intern(part).map_err(|e| exc_from_pyerror(&mut ctx, state, e))?;
        module = load_attr_ctx(&mut ctx, state, module, part_id)?;
    }
    Ok(module)
}

/// Subscript get/set (api.zig `getItem` / `setItem`):
/// an instance tries its `__getitem__` / `__setitem__`
/// first, with the VM's real call hook — the dispatch's
/// `LOAD_SUBSCR` / `STORE_SUBSCR` shape — then the slot
/// path (`obj_subscr`). A set returns the stored value's
/// slot result (the `obj_subscr` Set arm's value, which the
/// caller ignores — `ppy_setitem` has no out-param).
fn subscr_body(
    interp: &mut Interp,
    base_h: PpyHandle,
    index_h: PpyHandle,
    arg: SubscrArg,
) -> Result<Value, Value> {
    let tag = interp.tag;
    let base = handle_value(&interp.vm.roots, tag, base_h);
    let index = handle_value(&interp.vm.roots, tag, index_h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let base = obj_or_invalid(&mut ctx, state, base)?;
    let index = obj_or_invalid(&mut ctx, state, index)?;
    match try_instance_subscr(&mut ctx, state, base, index, arg) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => {
            let types = ctx.types;
            obj_subscr(&mut ctx, types, base, index, arg)
                .map_err(|e| exc_from_obj_error(&mut ctx, state, e))
        }
        Err(exc) => Err(exc),
    }
}

/// Call a callable with positional args only (api.zig
/// `call`): the args are resolved from their handles
/// first, then the call runs through the VM's real call
/// path (`call_function` — the same path Python calls use,
/// so bound methods, classes and user `__call__` all work).
fn call_body(
    interp: &mut Interp,
    fun_h: PpyHandle,
    n: usize,
    argv: *const PpyHandle,
) -> Result<Value, Value> {
    let tag = interp.tag;
    let fun = handle_value(&interp.vm.roots, tag, fun_h);
    let mut args: Vec<Option<Value>> = Vec::with_capacity(n);
    for i in 0..n {
        // FFI invariant: `argv` points to `n` valid handles
        // (the wrapper rejects a NULL `argv` with `n > 0`).
        let h = unsafe { *argv.add(i) };
        args.push(handle_value(&interp.vm.roots, tag, h));
    }
    let vm = &mut *interp.vm;
    let (fun, args) = {
        let (mut ctx, state) = vm.split();
        let fun = obj_or_invalid(&mut ctx, state, fun)?;
        let mut resolved = Vec::with_capacity(n);
        for a in args {
            resolved.push(obj_or_invalid(&mut ctx, state, a)?);
        }
        (fun, resolved)
    };
    call_function(vm, fun, n, 0, &args)
}

/// Register a host function under `module.name` (api.zig
/// `defineFunction`): the Python side is ONE shared builtin
/// (`host_call_builtin`) whose `self` is the host's index
/// in the interpreter's `hosts` list; the module is created
/// if needed, so `import module` finds it.
fn define_function_body(
    interp: &mut Interp,
    module: &[u8],
    name: &[u8],
    fun: PpyFunction,
    userdata: *mut c_void,
) -> Result<Value, Value> {
    interp.hosts.push(Host { fun, userdata });
    let idx = interp.hosts.len() - 1;
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let module_id = intern_qstr_id(&mut ctx, state, module)?;
    let module_val = get_or_create_module(&mut ctx, state, module_id);
    let name_id = intern_qstr_id(&mut ctx, state, name)?;
    let bound = Value::Heap(new_bound_meth(
        &mut ctx,
        state.vm_types.bound_meth,
        state.host_call_builtin,
        Value::small_int(idx as i64)
            .expect("a host index always fits the small-int window"),
    ));
    store_attr_ctx(&mut ctx, state, module_val, name_id, Some(bound))?;
    Ok(module_val)
}

/// Raise `type_name(message)` (api.zig `raiseNamed`):
/// the type is looked up in the builtins module (a lookup
/// failure propagates as-is); not an exception type →
/// `TypeError("not an exception type")`. The body returns
/// `Err` in EVERY path — `ppy_raise` always returns ERROR
/// (the Zig's `_ = produce(...); return ERROR;`).
fn raise_body(interp: &mut Interp, type_name: &[u8], message: &[u8]) -> Result<Value, Value> {
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let builtins_id = static_qstr_id(b"builtins").unwrap_or(0);
    let builtins_mod = state
        .builtin_modules
        .iter()
        .find(|(id, _)| *id == builtins_id)
        .map(|(_, v)| *v)
        .unwrap_or(Value::None);
    let type_id = intern_qstr_id(&mut ctx, state, type_name)?;
    let t = load_attr_ctx(&mut ctx, state, builtins_mod, type_id)?;
    let types = ctx.types;
    let is_exc_type = match obj_get_type(ctx.arena, types, t) {
        Some(tr) if tr == types.type_ => match t {
            Value::Heap(r) => {
                is_subclass_fast(ctx.arena, Value::Heap(r), Value::Heap(state.exc.base_exception))
            }
            _ => false,
        },
        _ => false,
    };
    if !is_exc_type {
        return Err(type_error(&mut ctx, state, "not an exception type"));
    }
    let type_ref = match t {
        Value::Heap(r) => r,
        _ => unreachable!("a type value is always a heap object"),
    };
    let msg = String::from_utf8_lossy(message);
    Err(new_exception_msg(&mut ctx, type_ref, &msg))
}

/// Raise an exception object as-is (api.zig
/// `raiseObject`): the object must be an exception
/// instance (its type a `BaseException` subclass), else
/// `TypeError("not an exception")`. Like `raise_body`, the
/// body returns `Err` in every path — `ppy_raise_exception`
/// always returns ERROR.
fn raise_exception_body(interp: &mut Interp, h: PpyHandle) -> Result<Value, Value> {
    let tag = interp.tag;
    let o = handle_value(&interp.vm.roots, tag, h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let o = obj_or_invalid(&mut ctx, state, o)?;
    let types = ctx.types;
    let is_exc = match obj_get_type(ctx.arena, types, o) {
        Some(tr) => {
            is_subclass_fast(ctx.arena, Value::Heap(tr), Value::Heap(state.exc.base_exception))
        }
        None => false,
    };
    if !is_exc {
        return Err(type_error(&mut ctx, state, "not an exception"));
    }
    Err(o)
}

/// Format the stored exception via the vendor's exception
/// printer into a new str (api.zig `formatException`).
fn format_exception_body(interp: &mut Interp, h: PpyHandle) -> Result<Value, Value> {
    let tag = interp.tag;
    let exc = handle_value(&interp.vm.roots, tag, h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let exc = obj_or_invalid(&mut ctx, state, exc)?;
    let types = ctx.types;
    let pool = ctx.pool;
    let mut out = String::new();
    obj_print_exception(&mut ctx, types, exc, &|id| pool.qstr_str(id as u16), &mut out);
    let v = new_str(&mut ctx, types, out.as_bytes());
    Ok(v)
}

/// The stable data pointer for `ppy_to_str` / `ppy_to_bytes`
/// (api.zig `toData`): a qstr's pool bytes, or an exact
/// `str` / `bytes` object's buffer. The pointer stays valid
/// while the handle is held (the handle is a GC root, and
/// `str` / `bytes` are immutable). Anything else →
/// `TypeError("expected a str" / "expected bytes")`.
fn data_ptr_body(interp: &mut Interp, h: PpyHandle, want_str: bool) -> Result<(*const u8, usize), Value> {
    let tag = interp.tag;
    let o = handle_value(&interp.vm.roots, tag, h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let o = obj_or_invalid(&mut ctx, state, o)?;
    let msg = if want_str {
        "expected a str"
    } else {
        "expected bytes"
    };
    match o {
        Value::Qstr(id) if want_str => {
            let b = ctx.pool.qstr_str(id as u16);
            Ok((b.as_ptr(), b.len()))
        }
        Value::Heap(r) => {
            let exact = if want_str {
                is_str(ctx.arena, ctx.types, o)
            } else {
                is_bytes(ctx.arena, ctx.types, o)
            };
            if !exact {
                return Err(type_error(&mut ctx, state, msg));
            }
            match ctx.arena.get(r) {
                Obj::Str(s) if want_str => Ok((s.bytes.as_ptr(), s.bytes.len())),
                Obj::Bytes(b) if !want_str => Ok((b.bytes.as_ptr(), b.bytes.len())),
                _ => Err(type_error(&mut ctx, state, msg)),
            }
        }
        _ => Err(type_error(&mut ctx, state, msg)),
    }
}

/// `str(h)` / `repr(h)` (api.zig `printed`): the vendor's
/// object printer into a new str.
fn printed_body(interp: &mut Interp, h: PpyHandle, kind: PrintKind) -> Result<Value, Value> {
    let tag = interp.tag;
    let o = handle_value(&interp.vm.roots, tag, h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let o = obj_or_invalid(&mut ctx, state, o)?;
    let types = ctx.types;
    let mut out = String::new();
    // G4/H10: the hook-aware printer — a FunBc `__str__` /
    // `__repr__` runs through the VM's real call hook (the
    // ctx-only `obj_print_helper` falls back to `<A object at
    // …>`), and exception args render their dunders too. A
    // raising dunder falls back to `<B object at …>` with
    // PPY_OK (slot semantics); the ORIGINAL aborts (rc=134,
    // null-pointer panic at `mp_mirrors.zig:745` — the
    // documented abort-vs-fallback divergence, do not
    // reproduce).
    crate::runtime::builtin_str::print_helper_hook(&mut ctx, state, o, kind, &mut out);
    Ok(new_str(&mut ctx, types, out.as_bytes()))
}

/// Get a `__main__` global (api.zig `getGlobal`): a missing
/// name is `ValueError("name is not defined in __main__")`.
fn get_global_body(interp: &mut Interp, name: &[u8]) -> Result<Value, Value> {
    let globals = interp.main_globals;
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let name_id = intern_qstr_id(&mut ctx, state, name)?;
    match dict_get_value(&mut ctx, Value::Heap(globals), Value::Qstr(name_id)) {
        Ok(v) => Ok(v),
        Err(_) => Err(value_error(
            &mut ctx,
            state,
            "name is not defined in __main__",
        )),
    }
}

/// Set a `__main__` global (api.zig `setGlobal`): the
/// Zig stores through `mp_obj_subscr` on the main dict, so
/// this does the same (a fixed-dict error would surface as
/// the slot's `TypeError` — `__main__`'s dict is never
/// fixed, so it cannot fire in practice).
fn set_global_body(interp: &mut Interp, name: &[u8], value_h: PpyHandle) -> Result<Value, Value> {
    let tag = interp.tag;
    let v = handle_value(&interp.vm.roots, tag, value_h);
    let globals = interp.main_globals;
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let v = obj_or_invalid(&mut ctx, state, v)?;
    let name_id = intern_qstr_id(&mut ctx, state, name)?;
    let types = ctx.types;
    obj_subscr(
        &mut ctx,
        types,
        Value::Heap(globals),
        Value::Qstr(name_id),
        SubscrArg::Set(v),
    )
    .map_err(|e| exc_from_obj_error(&mut ctx, state, e))?;
    Ok(Value::None)
}

/// Load an attribute (api.zig `getAttr`).
fn getattr_body(interp: &mut Interp, obj_h: PpyHandle, name: &[u8]) -> Result<Value, Value> {
    let tag = interp.tag;
    let o = handle_value(&interp.vm.roots, tag, obj_h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let o = obj_or_invalid(&mut ctx, state, o)?;
    let name_id = intern_qstr_id(&mut ctx, state, name)?;
    load_attr_ctx(&mut ctx, state, o, name_id)
}

/// Store an attribute (api.zig `setAttr`).
fn setattr_body(
    interp: &mut Interp,
    obj_h: PpyHandle,
    name: &[u8],
    value_h: PpyHandle,
) -> Result<Value, Value> {
    let tag = interp.tag;
    let o = handle_value(&interp.vm.roots, tag, obj_h);
    let v = handle_value(&interp.vm.roots, tag, value_h);
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let o = obj_or_invalid(&mut ctx, state, o)?;
    let v = obj_or_invalid(&mut ctx, state, v)?;
    let name_id = intern_qstr_id(&mut ctx, state, name)?;
    store_attr_ctx(&mut ctx, state, o, name_id, Some(v))?;
    Ok(Value::None)
}

/// Build a list / tuple from handles (api.zig `listOf`):
/// every handle is validated first (a bad one is
/// `ValueError("invalid handle")` — the Zig's `listOf`
/// validates each handle through `obj` before building).
fn new_seq_body(
    interp: &mut Interp,
    n: usize,
    items: *const PpyHandle,
    tuple: bool,
) -> Result<Value, Value> {
    let tag = interp.tag;
    let mut values: Vec<Option<Value>> = Vec::with_capacity(n);
    for i in 0..n {
        // FFI invariant: `items` points to `n` valid handles
        // (the wrapper rejects a NULL `items` with `n > 0`).
        let h = unsafe { *items.add(i) };
        values.push(handle_value(&interp.vm.roots, tag, h));
    }
    let vm = &mut *interp.vm;
    let (mut ctx, state) = vm.split();
    let types = ctx.types;
    let mut resolved = Vec::with_capacity(n);
    for v in values {
        resolved.push(obj_or_invalid(&mut ctx, state, v)?);
    }
    if tuple {
        Ok(Value::Heap(new_tuple(&mut ctx, types.tuple, &resolved)))
    } else {
        Ok(Value::Heap(new_list(&mut ctx, types.list, &resolved)))
    }
}

// ── The `ppy_*` exports (`include/petitpy.h`) ──────────

/// `ppy_api_version`: the API version this library
/// implements (`PPY_API_VERSION`, 1).
#[no_mangle]
pub extern "C" fn ppy_api_version() -> c_int {
    1
}

/// `ppy_new`: create an interpreter. A NULL config is
/// allowed (empty sys.path / argv, process stdout/stderr).
/// `sys.path` holds ONLY what the host asks for (cleared
/// first, then the config's paths — no PYTHONPATH /
/// PETITPY_LIB wiring; the frozen stdlib is found before
/// sys.path). Returns NULL when no interpreter slot is free
/// (64 max) or `__main__` cannot be registered.
#[no_mangle]
pub unsafe extern "C" fn ppy_new(config: *const PpyConfig) -> *mut Interp {
    api_lock();
    let Some(index) = alloc_interp_index() else {
        api_unlock();
        return std::ptr::null_mut();
    };
    // FFI invariant: `config` is NULL or points to a valid `ppy_config`.
    let (paths, argv, write) = unsafe { read_config(config) };
    let mut vm = Box::new(Vm::new());
    // `gc_init` reads `NPY_GC_STRESS` into the arena's
    // stress threshold — the original calls it from
    // `mp_embed_init` for every entry point, and the
    // embedding stress run (`NPY_GC_STRESS=10`) only catches
    // unrooted handles when it takes effect. G2 (P2) made
    // `Vm::new` call it; the call here stays as a harmless
    // second read (same env value).
    vm.arena.gc_init();
    // Register `__main__` (an empty run creates the module +
    // its dict and records `d4.main_module`); the globals
    // dict is the interpreter's `main_globals`.
    let main_globals = match vm.run_main_source(b"", "<stdin>", None) {
        Ok(Value::Heap(r)) => match vm.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => {
                free_interp_index(index);
                api_unlock();
                return std::ptr::null_mut();
            }
        },
        _ => {
            free_interp_index(index);
            api_unlock();
            return std::ptr::null_mut();
        }
    };
    // G2 (P7a): `set_sys_path` / `set_sys_argv` take raw
    // bytes now. The config strings are already lossy at the
    // `read_config` level (F1's documented caveat — C strings
    // arrive via `from_utf8_lossy`); the conversion here adds
    // no further loss.
    vm.set_sys_path(paths.into_iter().map(|s| s.into_bytes()).collect());
    vm.set_sys_argv(argv.into_iter().map(|s| s.into_bytes()).collect());
    let interp = Box::new(Interp {
        vm,
        index,
        tag: index << SLOT_BITS,
        main_globals,
        hosts: Vec::new(),
        err: None,
        exit_code: 0,
        write,
        type_names: HashMap::new(),
    });
    api_unlock();
    Box::into_raw(interp)
}

/// `ppy_free`: flush stdout/stderr, drop the interpreter
/// (its `Vm`, handle table, hosts and type-name cache) and
/// release its slot. A NULL pointer is a silent no-op
/// (defensive — the Zig would fault on NULL).
#[no_mangle]
pub unsafe extern "C" fn ppy_free(py: *mut Interp) {
    if py.is_null() {
        return;
    }
    api_lock();
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::Write::flush(&mut std::io::stderr());
    // FFI invariant: `py` is a live interpreter from `ppy_new`,
    // not already freed.
    let interp = unsafe { Box::from_raw(py) };
    free_interp_index(interp.index);
    drop(interp);
    api_unlock();
}

/// `ppy_exec`: compile with FILE_INPUT (kind 1) and the
/// host-given filename (or `"<string>"` when NULL), then run
/// in `__main__`'s namespace. No result handle — the
/// status + `ppy_fetch_error` carry the outcome.
#[no_mangle]
pub unsafe extern "C" fn ppy_exec(
    py: *mut Interp,
    src: *const c_char,
    len: usize,
    filename: *const c_char,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if src.is_null() && len > 0 {
        return PPY_ERROR;
    }
    // FFI invariant: `src` points to `len` readable bytes
    // (or is NULL with `len == 0`); `filename` is NULL or a
    // valid NUL-terminated C string.
    let src_bytes: &[u8] = if src.is_null() {
        &[]
    } else {
        // FFI invariant: `src` points to `len` readable bytes.
        unsafe { std::slice::from_raw_parts(src as *const u8, len) }
    };
    let name_owned;
    let name: &str = if filename.is_null() {
        "<string>"
    } else {
        // `from_utf8_lossy` borrows a temporary otherwise —
        // bind the owned string first so `name` outlives it.
        // FFI invariant: `filename` is a valid NUL-terminated C string.
        name_owned = String::from_utf8_lossy(unsafe { c_str_bytes(filename) });
        &name_owned
    };
    run_bracket(py, std::ptr::null_mut(), |interp| {
        eval_exec_body(interp, src_bytes, name, InputKind::FileInput).map(|_| None)
    })
}

/// `ppy_eval`: compile with EVAL_INPUT (kind 2), name
/// `"<string>"`, run in `__main__`'s namespace, and hand
/// the value back.
#[no_mangle]
pub unsafe extern "C" fn ppy_eval(
    py: *mut Interp,
    src: *const c_char,
    len: usize,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if src.is_null() && len > 0 {
        return PPY_ERROR;
    }
    // FFI invariant: `src` points to `len` readable bytes
    // (or is NULL with `len == 0`).
    let src_bytes: &[u8] = if src.is_null() {
        &[]
    } else {
        // FFI invariant: `src` points to `len` readable bytes.
        unsafe { std::slice::from_raw_parts(src as *const u8, len) }
    };
    run_produce(py, result, |interp| {
        eval_exec_body(interp, src_bytes, "<string>", InputKind::EvalInput)
    })
}

/// `ppy_import`: import the top-level package, then walk
/// down the dotted path to the leaf module.
#[no_mangle]
pub unsafe extern "C" fn ppy_import(
    py: *mut Interp,
    name: *const c_char,
    module: *mut PpyHandle,
) -> c_int {
    if py.is_null() || name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `name` is a valid NUL-terminated C string.
    let name_bytes = unsafe { c_str_bytes(name) };
    run_produce(py, module, |interp| import_body(interp, name_bytes))
}

/// `ppy_new_none`.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_none(py: *mut Interp, result: *mut PpyHandle) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |_| Ok(Value::None))
}

/// `ppy_new_bool`.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_bool(
    py: *mut Interp,
    value: bool,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |_| Ok(Value::Bool(value)))
}

/// `ppy_new_int`: a small int is an immediate; a big one is
/// a heap `Int` (the 62-bit small-int window — `Value::int`
/// decides).
#[no_mangle]
pub unsafe extern "C" fn ppy_new_int(
    py: *mut Interp,
    value: i64,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        Ok(Value::int(
            &mut interp.vm.arena,
            Some(interp.vm.types.int),
            &interp.vm.roots,
            value,
        ))
    })
}

/// `ppy_new_float`.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_float(
    py: *mut Interp,
    value: f64,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        let (mut ctx, _state) = interp.vm.split();
        let types = ctx.types;
        Ok(new_float(&mut ctx, types, value))
    })
}

/// `ppy_new_str`: `utf8` is `len` bytes (not necessarily
/// NUL-terminated). A NULL `utf8` is an empty string
/// (defensive).
#[no_mangle]
pub unsafe extern "C" fn ppy_new_str(
    py: *mut Interp,
    utf8: *const c_char,
    len: usize,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if utf8.is_null() && len > 0 {
        return PPY_ERROR;
    }
    // FFI invariant: `utf8` points to `len` readable bytes
    // (or is NULL with `len == 0`).
    let data: &[u8] = if utf8.is_null() {
        &[]
    } else {
        // FFI invariant: `utf8` points to `len` readable bytes.
        unsafe { std::slice::from_raw_parts(utf8 as *const u8, len) }
    };
    run_produce(py, result, |interp| {
        let (mut ctx, _state) = interp.vm.split();
        let types = ctx.types;
        Ok(new_str(&mut ctx, types, data))
    })
}

/// `ppy_new_bytes`: `data` is `len` raw bytes. A NULL
/// `data` is empty bytes (defensive).
#[no_mangle]
pub unsafe extern "C" fn ppy_new_bytes(
    py: *mut Interp,
    data: *const c_void,
    len: usize,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if data.is_null() && len > 0 {
        return PPY_ERROR;
    }
    // FFI invariant: `data` points to `len` readable bytes
    // (or is NULL with `len == 0`).
    let bytes: &[u8] = if data.is_null() {
        &[]
    } else {
        // FFI invariant: `data` points to `len` readable bytes.
        unsafe { std::slice::from_raw_parts(data as *const u8, len) }
    };
    run_produce(py, result, |interp| {
        let (mut ctx, _state) = interp.vm.split();
        let types = ctx.types;
        Ok(new_bytes(&mut ctx, types, bytes))
    })
}

/// `ppy_new_list`: build a list from `n` handles.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_list(
    py: *mut Interp,
    n: usize,
    items: *const PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if items.is_null() && n > 0 {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        new_seq_body(interp, n, items, false)
    })
}

/// `ppy_new_tuple`: build a tuple from `n` handles.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_tuple(
    py: *mut Interp,
    n: usize,
    items: *const PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if items.is_null() && n > 0 {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| new_seq_body(interp, n, items, true))
}

/// `ppy_new_dict`: an empty dict.
#[no_mangle]
pub unsafe extern "C" fn ppy_new_dict(py: *mut Interp, result: *mut PpyHandle) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        let (mut ctx, _state) = interp.vm.split();
        let types = ctx.types;
        Ok(Value::Heap(ctx.alloc(
            Some(types.dict),
            Obj::Dict(DictData {
                map: MapData::new(),
            }),
        )))
    })
}

/// `ppy_to_bool`: the truth value (`__bool__` /
/// `__len__` dispatch — `obj_is_true`).
#[no_mangle]
pub unsafe extern "C" fn ppy_to_bool(
    py: *mut Interp,
    h: PpyHandle,
    value: *mut bool,
) -> c_int {
    if py.is_null() || value.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let tag = interp.tag;
        let o = handle_value(&interp.vm.roots, tag, h);
        let vm = &mut *interp.vm;
        let (mut ctx, state) = vm.split();
        let o = obj_or_invalid(&mut ctx, state, o)?;
        let types = ctx.types;
        let b = obj_is_true(&mut ctx, types, o)
            .map_err(|e| exc_from_pyerror(&mut ctx, state, e))?;
        // FFI invariant: `value` points to a writable `bool`.
        unsafe { *value = b };
        Ok(())
    })
}

/// `ppy_to_int`: `TypeError("expected an int")` ONLY when
/// the value is not an int at all. A big int that does not
/// fit i64 surfaces as the `OverflowError` the int
/// conversion itself raises (the vendor
/// `mp_obj_get_int_maybe` raises it; `raise()` does not
/// overwrite a pending exception — api.zig `toInt`).
#[no_mangle]
pub unsafe extern "C" fn ppy_to_int(
    py: *mut Interp,
    h: PpyHandle,
    value: *mut i64,
) -> c_int {
    if py.is_null() || value.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let tag = interp.tag;
        let o = handle_value(&interp.vm.roots, tag, h);
        let vm = &mut *interp.vm;
        let (mut ctx, state) = vm.split();
        let o = obj_or_invalid(&mut ctx, state, o)?;
        let types = ctx.types;
        match obj_get_int_maybe(&mut ctx, types, o) {
            Ok(Some(n)) => {
                // FFI invariant: `value` points to a writable `int64_t`.
                unsafe { *value = n };
                Ok(())
            }
            Ok(None) => Err(type_error(&mut ctx, state, "expected an int")),
            Err(e) => Err(exc_from_obj_error(&mut ctx, state, e)),
        }
    })
}

/// `ppy_to_float`: a non-float is `TypeError("expected a
/// float")`.
#[no_mangle]
pub unsafe extern "C" fn ppy_to_float(
    py: *mut Interp,
    h: PpyHandle,
    value: *mut f64,
) -> c_int {
    if py.is_null() || value.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let tag = interp.tag;
        let o = handle_value(&interp.vm.roots, tag, h);
        let vm = &mut *interp.vm;
        let (mut ctx, state) = vm.split();
        let o = obj_or_invalid(&mut ctx, state, o)?;
        let types = ctx.types;
        match obj_get_float_maybe(&mut ctx, types, o) {
            Some(f) => {
                // FFI invariant: `value` points to a writable `double`.
                unsafe { *value = f };
                Ok(())
            }
            None => Err(type_error(&mut ctx, state, "expected a float")),
        }
    })
}

/// `ppy_to_str`: the pointer stays valid while the handle
/// is held (see `data_ptr_body`).
#[no_mangle]
pub unsafe extern "C" fn ppy_to_str(
    py: *mut Interp,
    h: PpyHandle,
    utf8: *mut *const c_char,
    len: *mut usize,
) -> c_int {
    if py.is_null() || utf8.is_null() || len.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let (ptr, n) = data_ptr_body(interp, h, true)?;
        // FFI invariant: `utf8` / `len` point to writable slots.
        unsafe {
            *utf8 = ptr as *const c_char;
            *len = n;
        }
        Ok(())
    })
}

/// `ppy_to_bytes`: the pointer stays valid while the handle
/// is held (see `data_ptr_body`).
#[no_mangle]
pub unsafe extern "C" fn ppy_to_bytes(
    py: *mut Interp,
    h: PpyHandle,
    data: *mut *const c_void,
    len: *mut usize,
) -> c_int {
    if py.is_null() || data.is_null() || len.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let (ptr, n) = data_ptr_body(interp, h, false)?;
        // FFI invariant: `data` / `len` point to writable slots.
        unsafe {
            *data = ptr as *const c_void;
            *len = n;
        }
        Ok(())
    })
}

/// `ppy_str`: `str(h)`.
#[no_mangle]
pub unsafe extern "C" fn ppy_str(
    py: *mut Interp,
    h: PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        printed_body(interp, h, PrintKind::Str)
    })
}

/// `ppy_repr`: `repr(h)`.
#[no_mangle]
pub unsafe extern "C" fn ppy_repr(
    py: *mut Interp,
    h: PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        printed_body(interp, h, PrintKind::Repr)
    })
}

/// `ppy_type_name`: the type's name as a C string, cached
/// per name in the interpreter (the pointer stays valid for
/// the interpreter's lifetime). NULL for a bad handle, and
/// for a name that cannot be a C string (interior NUL).
/// This does NOT go through `produce`: it locks + enters
/// directly (api.zig).
#[no_mangle]
pub unsafe extern "C" fn ppy_type_name(py: *mut Interp, h: PpyHandle) -> *const c_char {
    if py.is_null() {
        return std::ptr::null();
    }
    api_lock();
    // FFI invariant: `py` is a live interpreter from `ppy_new`.
    let interp = unsafe { &mut *py };
    let result = match handle_value(&interp.vm.roots, interp.tag, h) {
        None => std::ptr::null(),
        Some(v) => {
            let name = {
                let (ctx, _state) = interp.vm.split();
                obj_type_name(&ctx, v)
            };
            if name.as_bytes().contains(&0) {
                std::ptr::null()
            } else {
                let entry = interp
                    .type_names
                    .entry(name.clone())
                    .or_insert_with(|| CString::new(name.as_str()).unwrap_or_default());
                entry.as_ptr()
            }
        }
    };
    api_unlock();
    result
}

/// `ppy_len`: `len(h)` into `*len`.
#[no_mangle]
pub unsafe extern "C" fn ppy_len(
    py: *mut Interp,
    h: PpyHandle,
    len: *mut usize,
) -> c_int {
    if py.is_null() || len.is_null() {
        return PPY_ERROR;
    }
    run_status(py, |interp| {
        let tag = interp.tag;
        let o = handle_value(&interp.vm.roots, tag, h);
        let vm = &mut *interp.vm;
        let (mut ctx, state) = vm.split();
        let o = obj_or_invalid(&mut ctx, state, o)?;
        let types = ctx.types;
        let n = obj_len(&mut ctx, types, o)
            .map_err(|e| exc_from_obj_error(&mut ctx, state, e))?;
        let n = obj_get_int_maybe(&mut ctx, types, n)
            .map_err(|e| exc_from_obj_error(&mut ctx, state, e))?
            .unwrap_or(0);
        // FFI invariant: `len` points to a writable `size_t`.
        unsafe { *len = n as usize };
        Ok(())
    })
}

/// `ppy_getitem`: `c[key]`.
#[no_mangle]
pub unsafe extern "C" fn ppy_getitem(
    py: *mut Interp,
    c: PpyHandle,
    key: PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        subscr_body(interp, c, key, SubscrArg::Get)
    })
}

/// `ppy_setitem`: `c[key] = value` (no result handle).
#[no_mangle]
pub unsafe extern "C" fn ppy_setitem(
    py: *mut Interp,
    c: PpyHandle,
    key: PpyHandle,
    value: PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_bracket(py, std::ptr::null_mut(), |interp| {
        let tag = interp.tag;
        let v = handle_value(&interp.vm.roots, tag, value);
        subscr_body(interp, c, key, SubscrArg::Set(
            // The value handle is validated here (the set arm
            // needs the value itself, not its handle).
            match v {
                Some(v) => v,
                None => {
                    let vm = &mut *interp.vm;
                    let (mut ctx, state) = vm.split();
                    return Err(value_error(&mut ctx, state, "invalid handle"));
                }
            },
        ))
        .map(|_| None)
    })
}

/// `ppy_call`: call `fn` with `argc` positional args.
#[no_mangle]
pub unsafe extern "C" fn ppy_call(
    py: *mut Interp,
    fun: PpyHandle,
    argc: usize,
    argv: *const PpyHandle,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    if argv.is_null() && argc > 0 {
        return PPY_ERROR;
    }
    run_produce(py, result, |interp| {
        call_body(interp, fun, argc, argv)
    })
}

/// `ppy_getattr`: `getattr(obj, name)`.
#[no_mangle]
pub unsafe extern "C" fn ppy_getattr(
    py: *mut Interp,
    obj: PpyHandle,
    name: *const c_char,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() || name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `name` is a valid NUL-terminated C string.
    let name_bytes = unsafe { c_str_bytes(name) };
    run_produce(py, result, |interp| {
        getattr_body(interp, obj, name_bytes)
    })
}

/// `ppy_setattr`: `setattr(obj, name, value)`.
#[no_mangle]
pub unsafe extern "C" fn ppy_setattr(
    py: *mut Interp,
    obj: PpyHandle,
    name: *const c_char,
    value: PpyHandle,
) -> c_int {
    if py.is_null() || name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `name` is a valid NUL-terminated C string.
    let name_bytes = unsafe { c_str_bytes(name) };
    run_bracket(py, std::ptr::null_mut(), |interp| {
        setattr_body(interp, obj, name_bytes, value).map(|_| None)
    })
}

/// `ppy_get_global`: a `__main__` global.
#[no_mangle]
pub unsafe extern "C" fn ppy_get_global(
    py: *mut Interp,
    name: *const c_char,
    result: *mut PpyHandle,
) -> c_int {
    if py.is_null() || name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `name` is a valid NUL-terminated C string.
    let name_bytes = unsafe { c_str_bytes(name) };
    run_produce(py, result, |interp| {
        get_global_body(interp, name_bytes)
    })
}

/// `ppy_set_global`: store into `__main__`'s dict.
#[no_mangle]
pub unsafe extern "C" fn ppy_set_global(
    py: *mut Interp,
    name: *const c_char,
    value: PpyHandle,
) -> c_int {
    if py.is_null() || name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `name` is a valid NUL-terminated C string.
    let name_bytes = unsafe { c_str_bytes(name) };
    run_bracket(py, std::ptr::null_mut(), |interp| {
        set_global_body(interp, name_bytes, value).map(|_| None)
    })
}

/// `ppy_dup`: a second handle to the same value.
#[no_mangle]
pub unsafe extern "C" fn ppy_dup(
    py: *mut Interp,
    h: PpyHandle,
    copy: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, copy, |interp| {
        let tag = interp.tag;
        let o = handle_value(&interp.vm.roots, tag, h);
        let vm = &mut *interp.vm;
        let (mut ctx, state) = vm.split();
        obj_or_invalid(&mut ctx, state, o)
    })
}

/// `ppy_release`: free a handle (a bad handle is a silent
/// no-op — api.zig). A NULL interpreter is a silent no-op.
#[no_mangle]
pub unsafe extern "C" fn ppy_release(py: *mut Interp, h: PpyHandle) {
    if py.is_null() {
        return;
    }
    api_lock();
    // FFI invariant: `py` is a live interpreter from `ppy_new`.
    let interp = unsafe { &mut *py };
    release_handle(&mut interp.vm.roots, interp.tag, h);
    api_unlock();
}

/// `ppy_define_function`: register a host function under
/// `module.name` (the module is created if needed).
#[no_mangle]
pub unsafe extern "C" fn ppy_define_function(
    py: *mut Interp,
    module: *const c_char,
    name: *const c_char,
    fun: PpyFunction,
    userdata: *mut c_void,
) -> c_int {
    if py.is_null() || module.is_null() || name.is_null() || fun as usize == 0 {
        return PPY_ERROR;
    }
    // FFI invariant: `module` / `name` are valid NUL-terminated
    // C strings; `fun` is a callable host function.
    let module_bytes = unsafe { c_str_bytes(module) };
    let name_bytes = unsafe { c_str_bytes(name) };
    run_bracket(py, std::ptr::null_mut(), |interp| {
        define_function_body(interp, module_bytes, name_bytes, fun, userdata).map(|_| None)
    })
}

/// `ppy_raise`: raise `type_name(message)` — always
/// returns `PPY_ERROR` (the Zig's `_ = produce(...);
/// return ERROR;`).
#[no_mangle]
pub unsafe extern "C" fn ppy_raise(
    py: *mut Interp,
    type_name: *const c_char,
    message: *const c_char,
) -> c_int {
    if py.is_null() || type_name.is_null() {
        return PPY_ERROR;
    }
    // FFI invariant: `type_name` is a valid NUL-terminated C
    // string; `message` is NULL (empty) or one too.
    let type_bytes = unsafe { c_str_bytes(type_name) };
    let msg_bytes: &[u8] = if message.is_null() {
        &[]
    } else {
        // FFI invariant: `message` is a valid NUL-terminated C string.
        unsafe { c_str_bytes(message) }
    };
    let _ = run_bracket(py, std::ptr::null_mut(), |interp| {
        raise_body(interp, type_bytes, msg_bytes).map(|_| None)
    });
    PPY_ERROR
}

/// `ppy_raise_exception`: raise an exception object as-is
/// — always returns `PPY_ERROR`.
#[no_mangle]
pub unsafe extern "C" fn ppy_raise_exception(py: *mut Interp, exception: PpyHandle) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    let _ = run_bracket(py, std::ptr::null_mut(), |interp| {
        raise_exception_body(interp, exception).map(|_| None)
    });
    PPY_ERROR
}

/// `ppy_fetch_error`: move the stored exception out (and
/// clear it) into a handle; `PPY_OK` + a 0 handle when
/// there is none.
#[no_mangle]
pub unsafe extern "C" fn ppy_fetch_error(
    py: *mut Interp,
    exception: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_OK;
    }
    api_lock();
    if !exception.is_null() {
        // FFI invariant: `exception` points to a writable `ppy_handle`.
        unsafe { *exception = 0 };
    }
    // FFI invariant: `py` is a live interpreter from `ppy_new`.
    let interp = unsafe { &mut *py };
    let Some(exc) = interp.err.take() else {
        api_unlock();
        return PPY_OK;
    };
    let h = new_handle(&mut interp.vm.roots, interp.tag, exc);
    if !exception.is_null() {
        // FFI invariant: `exception` points to a writable `ppy_handle`.
        unsafe { *exception = h };
    }
    api_unlock();
    PPY_OK
}

/// `ppy_format_exception`: format the exception via the
/// vendor's exception printer into a new str.
#[no_mangle]
pub unsafe extern "C" fn ppy_format_exception(
    py: *mut Interp,
    exception: PpyHandle,
    text: *mut PpyHandle,
) -> c_int {
    if py.is_null() {
        return PPY_ERROR;
    }
    run_produce(py, text, |interp| {
        format_exception_body(interp, exception)
    })
}

/// `ppy_exit_code`: the exit code after `PPY_EXIT` (0 when
/// there was none). Takes the lock (the Zig reads it
/// unlocked; locking is safe because the lock is reentrant).
#[no_mangle]
pub unsafe extern "C" fn ppy_exit_code(py: *mut Interp) -> c_int {
    if py.is_null() {
        return 0;
    }
    api_lock();
    // FFI invariant: `py` is a live interpreter from `ppy_new`.
    let code = unsafe { (*py).exit_code };
    api_unlock();
    code
}
