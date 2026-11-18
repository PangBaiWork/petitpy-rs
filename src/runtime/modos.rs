//! Lane D2: the `os` module (`modos.zig`, full config, 766
//! lines) + the fd-level builtins' interception targets (in
//! `modos_fd.rs`).
//!
//! | Zig (`modos.zig`) | Rust | notes |
//! |---|---|---|
//! | `pathArg` (:247-261) | `path_arg` | str/bytes fast path via `str_data_pool`; else `str(obj)` through `call_function_1` (the `str` type's `type_call`); any failure → `OSError(EINVAL)` (the Zig's caller raises EINVAL when `pathArg` returns null, overwriting the `str()` error). Bytes are truncated at the first NUL (C-string semantics, as `open_impl` does) |
//! | `py_os_urandom` (1) | `os_urandom_builtin` | n<0 → EINVAL; n==0 → `b""`; n>4096 → EFBIG (the 4096-byte stack buffer, probed) |
//! | `py_os_open` var (2,3) | stub in `modos_fd.rs` (`OsFdBuiltins.open`), intercepted in `call_function_ctx` | mode default `0o777`; flags via `OpenOptionsExt::custom_flags` + `mode` |
//! | `py_os_close` (1) | stub in `modos_fd.rs` | unknown fd → EBADF (probed) |
//! | `py_os_read` (2) | stub in `modos_fd.rs` | n<0 → EINVAL; n==0 → `b""`; n>4096 → EFBIG (probed) |
//! | `py_os_write` (2) | stub in `modos_fd.rs` | data via `str_data_pool`; non-str/bytes → `TypeError("can't convert to str implicitly")` (probed) |
//! | `py_os_lseek` (3) | stub in `modos_fd.rs` | stdio seek → ESPIPE(29, probed) |
//! | `py_os_fsync` (1) | stub in `modos_fd.rs` | owned fds → `sync_all`; stdio fds → EINVAL (no safe std `fsync` for fd 0/1/2 — the reference calls `fsync(2)`, which returns EINVAL on the pipes/ttys stdout is in the test environment; see §4) |
//! | `py_os_chmod` (2) | `os_chmod_builtin` | `set_permissions(from_mode(mode))` |
//! | `py_os_chown` (3) | `os_chown_builtin` | NOT IMPLEMENTED — no std `chown` API (pure-std rule); raises `NotImplementedError` (the reference chowns; see §4) |
//! | `py_os_getpid` (0) | `os_getpid_builtin` | `std::process::id()` |
//! | `py_os_kill` (2) | `os_kill_builtin` | NOT IMPLEMENTED — no std `kill` API; raises `NotImplementedError` (see §4) |
//! | `py_os_umask` (1) | `os_umask_builtin` | NOT IMPLEMENTED — no std `umask` API; raises `NotImplementedError` (see §4) |
//! | `py_os_getcwd` (0) | `os_getcwd_builtin` | `current_dir`, raw bytes via `OsStrExt` |
//! | `py_os_chdir` (1) | `os_chdir_builtin` | `set_current_dir` |
//! | `py_os_getenv` kwVar(1, `default`) | `os_getenv_builtin` + `os_getenv_builtin_kw` | positional default (args[1]) and `default=` kwarg both work (probed) |
//! | `py_os_environ` (0) | `os_environ_builtin` | a FRESH dict from the process environment on every call (NOT a dict attribute); entries without `=` are skipped (the Zig's `indexOfScalar` guard; `vars_os` agrees) |
//! | `py_os_putenv` (2) | `os_putenv_builtin` | empty name or `=` in the name → EINVAL (probed; `set_var` would panic) |
//! | `py_os_unsetenv` (1) | `os_unsetenv_builtin` | same guards as `putenv` |
//! | `py_os_unlink` (1) | `os_unlink_builtin` | bound as BOTH `remove` and `unlink` (the SAME fun object, per the table comment) |
//! | `py_os_rename` (2) | `os_rename_builtin` | `fs::rename` (the kernel errno is passed through — the reference's `EXDEV(18)` for a cross-device rename of a missing file is kernel behavior, same syscall) |
//! | `py_os_mkdir` var (1,2) | `os_mkdir_builtin` | mode default `0o777`; `DirBuilder::mode` (exact `mkdir(2)` semantics) |
//! | `py_os_rmdir` (1) | `os_rmdir_builtin` | `remove_dir` |
//! | `py_os_stat` / `py_os_lstat` (1) | `os_stat_builtin` / `os_lstat_builtin` | `attrtuple_new` over the SHARED `ctx.types.attrtuple` (probed: `type(os.stat('.')) is type(sys.implementation)`, both `is tuple` False); times are ints (seconds); `u64` fields wrap with `as i64` (the Zig's `@intCast`) |
//! | `py_os_listdir` (1) | `os_listdir_builtin` | `read_dir` (never yields `.`/`..`); entry names as str |
//! | `py_os_readlink` (1) | `os_readlink_builtin` | target truncated to 4095 bytes (the Zig's `buf.len - 1`) |
//! | `py_os_symlink` (2) | `os_symlink_builtin` | `unix::fs::symlink` |
//! | `py_os_access` (2) | `os_access_builtin` | returns bool, never raises — invalid mode bits → `False` (probed: `access("/tmp", 8)` is `False`, not an error); files are probed by opening (root-correct), dirs by mode bits, exec by `0o111` (see the fn's docs) |
//! | `py_os_getattr` (1) | `os_getattr_stub` + `os_getattr_ctx` (intercepted) | `os.path` → import `os_path` (failure propagates); anything else → import `os_ext` (failure swallowed, pending cleared), look the attr up there, else `AttributeError("no attribute '%q'")` |
//!
//! Constants (Linux values, probed 2026-11-18): `O_RDONLY`=0,
//! `O_WRONLY`=1, `O_RDWR`=2, `O_CREAT`=64, `O_TRUNC`=512,
//! `O_APPEND`=1024, `O_EXCL`=128; `F_OK`=0, `R_OK`=4, `W_OK`=2,
//! `X_OK`=1; `SEEK_SET/CUR/END` = 0/1/2 (POSIX). Strings:
//! `sep`="/", `altsep`="" (empty str, NOT None — the Zig's
//! comment says CPython uses None but None isn't a ROM constant
//! they can point at), `extsep`=".", `linesep`="\n",
//! `curdir`=".", `pardir`="..", `name`="posix".
//!
//! The os dict is FIXED (`is_fixed = true`, as the C's ROM
//! dict): `os.foo = 42` and `del os.sep` raise AttributeError
//! (probed 2026-11-18).

use crate::memory::gc::CallCtx;
use crate::objects::builtin_types::BuiltinTypes;
use crate::objects::containers::objattrtuple::attrtuple_new;
use crate::objects::containers::objdict::{dict_new, dict_store};
use crate::objects::containers::objlist::new_list;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::ops::arg_as_int;
use crate::objects::strings::objstr_a::{new_bytes, new_str, str_data_pool};
use crate::objects::value::{FunBuiltinData, Obj, Value};
use crate::pyerr::{PyError, PyResult};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::slots::{BuiltinFn, BuiltinKwFn};
use crate::runtime::raise::raise_os_error;
use crate::runtime::runtime_call::call_function_1;
use crate::runtime::vm::VmState;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

pub const EINVAL: i32 = 22;
pub const EFBIG: i32 = 27;
pub const EBADF: i32 = 9;
const EIO: i32 = 5;

/// Map an `io::Error` to the OSError the original raises:
/// the raw OS errno when present, `EIO` when the error has
/// none (e.g. a NUL byte in a path — unreachable here since
/// `path_arg` truncates at NUL first).
pub(crate) fn os_err(ctx: &mut CallCtx, e: &std::io::Error) -> PyError {
    raise_os_error(ctx, e.raw_os_error().unwrap_or(EIO))
}

/// `pathArg` (`modos.zig:247-261`): str/bytes fast path, else
/// the `str()` conversion; any failure → `OSError(EINVAL)`.
/// The bytes are truncated at the first NUL (C-string
/// semantics — the Zig hands a `[*:0]const u8` to the
/// syscalls, and `open_impl` does the same).
pub(crate) fn path_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
    if let Some(b) = str_data_pool(ctx.arena, ctx.pool, v) {
        return Ok(truncate_nul(b));
    }
    let s = call_function_1(ctx, Value::Heap(ctx.types.str_), v)
        .map_err(|_| raise_os_error(ctx, EINVAL))?;
    str_data_pool(ctx.arena, ctx.pool, s)
        .map(truncate_nul)
        .ok_or_else(|| raise_os_error(ctx, EINVAL))
}

fn truncate_nul(mut b: Vec<u8>) -> Vec<u8> {
    if let Some(i) = b.iter().position(|&c| c == 0) {
        b.truncate(i);
    }
    b
}

/// `mp_obj_get_int` for an os argument (`arg_as_int` +
/// `record_typed` — the C13b error convention).
pub(crate) fn arg_int(ctx: &mut CallCtx, v: Value) -> PyResult<i64> {
    arg_as_int(ctx.arena, ctx.types, v).map_err(|e| ctx.record_typed(e))
}

/// A `NotImplementedError` for the three syscalls pure std
/// cannot express (`chown`/`kill`/`umask` — see §4).
fn not_impl(ctx: &mut CallCtx, what: &'static str) -> PyError {
    ctx.record_raise("NotImplementedError", Some(what));
    PyError::PyRaise
}

/// Allocate a `FunBuiltin` for an os entry (the
/// `locals_dict_from_entries` construction, `vm_types.rs:98`).
fn mk_fun(
    ctx: &mut CallCtx,
    fun_builtin_var: crate::objects::value::GcRef,
    name: &str,
    min: u16,
    max: u16,
    func: BuiltinFn,
    kw_func: Option<BuiltinKwFn>,
) -> Value {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("os member name is not a static qstr: {}", name));
    Value::Heap(ctx.alloc(
        Some(fun_builtin_var),
        Obj::FunBuiltin(FunBuiltinData {
            name: id,
            n_args: fun_n_args(min, max),
            func,
            kw_func,
        }),
    ))
}

/// Store one entry in the os dict (panics only when the name
/// is not a static qstr — a build-time fact).
fn store(ctx: &mut CallCtx, dict: Value, name: &str, value: Value) {
    let id = static_qstr_id(name.as_bytes())
        .unwrap_or_else(|| panic!("os member name is not a static qstr: {}", name));
    dict_store(ctx, dict, Value::Qstr(id), value)
        .map_err(|e| ctx.record_typed(e))
        .expect("os dict store cannot fail on a fresh dict");
}

/// `os.urandom(n)` (`modos.zig:271-290`).
pub fn os_urandom_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    use std::io::Read;
    let n = arg_int(ctx, args[0])?;
    if n < 0 {
        return Err(raise_os_error(ctx, EINVAL));
    }
    if n == 0 {
        return Ok(new_bytes(ctx, ctx.types, &[]));
    }
    if n > 4096 {
        return Err(raise_os_error(ctx, EFBIG));
    }
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| os_err(ctx, &e))?;
    let mut buf = vec![0u8; n as usize];
    let mut off = 0usize;
    while off < buf.len() {
        match f.read(&mut buf[off..]) {
            Ok(0) => return Err(raise_os_error(ctx, EIO)),
            Ok(got) => off += got,
            Err(e) => return Err(os_err(ctx, &e)),
        }
    }
    Ok(new_bytes(ctx, ctx.types, &buf))
}

/// `os.getcwd()` (`modos.zig:398-402`).
pub fn os_getcwd_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let cwd = std::env::current_dir().map_err(|e| os_err(ctx, &e))?;
    Ok(new_str(ctx, ctx.types, cwd.as_os_str().as_bytes()))
}

/// `os.chdir(path)` (`modos.zig:404-408`).
pub fn os_chdir_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    std::env::set_current_dir(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

fn getenv_impl(
    ctx: &mut CallCtx,
    name: Value,
    default: Option<Value>,
) -> PyResult<Value> {
    let name = path_arg(ctx, name)?;
    match std::env::var_os(std::ffi::OsStr::from_bytes(&name)) {
        Some(v) => Ok(new_str(ctx, ctx.types, v.as_bytes())),
        None => Ok(default.unwrap_or(Value::None)),
    }
}

/// `os.getenv(name[, default])` — the positional form
/// (`modos.zig:410-416`).
pub fn os_getenv_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let default = args.get(1).copied();
    getenv_impl(ctx, args[0], default)
}

/// `os.getenv(name, default=...)` — the keyword form
/// (`kwVar(1, &.{0, MP_QSTR_default}, ...)`).
pub fn os_getenv_builtin_kw(
    ctx: &mut CallCtx,
    args: &[Value],
    kwargs: &[(u32, Value)],
) -> PyResult<Value> {
    let default_id = static_qstr_id(b"default").expect("default is a static qstr");
    let mut default = args.get(1).copied();
    for (k, v) in kwargs {
        if *k == default_id {
            default = Some(*v);
        }
    }
    getenv_impl(ctx, args[0], default)
}

/// `os._environ()` (`modos.zig:238-246`): a FRESH dict from
/// the process environment on every call.
pub fn os_environ_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    // A fresh dict on every call (the Zig returns a new dict
    // from the process environment each time). Built with
    // `dict_new` — a hand-rolled `ctx.alloc(None, ...)` leaves
    // the dict headerless, so type dispatch cannot see it as a
    // dict (`==` fell back to identity, `[...]` raised
    // "object isn't subscriptable").
    let vars: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        std::env::vars_os().collect();
    let dict_v = dict_new(ctx, vars.len());
    let types: &BuiltinTypes = ctx.types;
    for (k, v) in vars {
        let key = new_str(ctx, types, k.as_bytes());
        let val = new_str(ctx, types, v.as_bytes());
        dict_store(ctx, dict_v, key, val).map_err(|e| ctx.record_typed(e))?;
    }
    Ok(dict_v)
}

/// `os.putenv(name, value)` (`modos.zig:418-423`).
pub fn os_putenv_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = path_arg(ctx, args[0])?;
    let value = path_arg(ctx, args[1])?;
    if name.is_empty() || name.contains(&b'=') {
        return Err(raise_os_error(ctx, EINVAL));
    }
    std::env::set_var(
        std::ffi::OsStr::from_bytes(&name),
        std::ffi::OsStr::from_bytes(&value),
    );
    Ok(Value::None)
}

/// `os.unsetenv(name)` (`modos.zig:425-429`).
pub fn os_unsetenv_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let name = path_arg(ctx, args[0])?;
    if name.is_empty() || name.contains(&b'=') {
        return Err(raise_os_error(ctx, EINVAL));
    }
    std::env::remove_var(std::ffi::OsStr::from_bytes(&name));
    Ok(Value::None)
}

/// `os.remove(path)` / `os.unlink(path)` — the SAME fun
/// object for both names (`modos.zig:431-435`).
pub fn os_unlink_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    std::fs::remove_file(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// `os.rename(src, dst)` (`modos.zig:437-442`).
pub fn os_rename_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let src = path_arg(ctx, args[0])?;
    let dst = path_arg(ctx, args[1])?;
    std::fs::rename(
        std::ffi::OsStr::from_bytes(&src),
        std::ffi::OsStr::from_bytes(&dst),
    )
    .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// `os.mkdir(path[, mode])` (`modos.zig:444-449`).
pub fn os_mkdir_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    use std::os::unix::fs::DirBuilderExt;
    let path = path_arg(ctx, args[0])?;
    let mode: u32 = match args.get(1) {
        Some(v) => arg_int(ctx, *v)? as u32,
        None => 0o777,
    };
    std::fs::DirBuilder::new()
        .mode(mode)
        .create(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// `os.rmdir(path)` (`modos.zig:451-455`).
pub fn os_rmdir_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    std::fs::remove_dir(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// The `stat_result` field names, in the CPython-compatible
/// order (`modos.zig:459-470`).
fn stat_fields() -> Vec<u32> {
    [
        "st_mode", "st_ino", "st_dev", "st_nlink", "st_uid", "st_gid", "st_size", "st_atime",
        "st_mtime", "st_ctime",
    ]
    .iter()
    .map(|n| static_qstr_id(n.as_bytes()).expect("stat field is a static qstr"))
    .collect()
}

/// `buildStatResult` (`modos.zig:499-512`): the 10 fields as
/// ints (times are seconds, not floats — the upstream
/// unix-port rule). `u64` fields wrap with `as i64` (the
/// Zig's `@intCast`); out-of-window values allocate a heap
/// int (the Zig tag-wraps instead — unobservable for real
/// files; see §4).
fn stat_result(ctx: &mut CallCtx, md: &std::fs::Metadata) -> Value {
    let types: &BuiltinTypes = ctx.types;
    let int = |ctx: &mut CallCtx, n: i64| Value::int(ctx.arena, Some(types.int), ctx.roots, n);
    let items = vec![
        int(ctx, md.mode() as i64),
        int(ctx, md.ino() as i64),
        int(ctx, md.dev() as i64),
        int(ctx, md.nlink() as i64),
        int(ctx, md.uid() as i64),
        int(ctx, md.gid() as i64),
        int(ctx, md.size() as i64),
        int(ctx, md.atime()),
        int(ctx, md.mtime()),
        int(ctx, md.ctime()),
    ];
    attrtuple_new(ctx, types.attrtuple, stat_fields(), items)
}

/// `os.stat(path)` (`modos.zig:514-519`).
pub fn os_stat_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    let md = std::fs::metadata(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(stat_result(ctx, &md))
}

/// `os.lstat(path)` (`modos.zig:521-526`).
pub fn os_lstat_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    let md = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    Ok(stat_result(ctx, &md))
}

/// `os.listdir(path)` (`modos.zig:530-551`).
pub fn os_listdir_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    let rd = std::fs::read_dir(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    let mut items = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| os_err(ctx, &e))?;
        items.push(new_str(
            ctx,
            ctx.types,
            entry.file_name().as_bytes(),
        ));
    }
    Ok(Value::Heap(new_list(ctx, ctx.types.list, &items)))
}

/// `os.readlink(path)` (`modos.zig:553-562`): the target,
/// truncated to 4095 bytes (the Zig's `buf.len - 1`).
pub fn os_readlink_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    let target = std::fs::read_link(std::ffi::OsStr::from_bytes(&path))
        .map_err(|e| os_err(ctx, &e))?;
    let mut bytes = target.as_os_str().as_bytes().to_vec();
    bytes.truncate(4095);
    Ok(new_str(ctx, ctx.types, &bytes))
}

/// `os.symlink(target, linkpath)` (`modos.zig:564-568`).
pub fn os_symlink_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let target = path_arg(ctx, args[0])?;
    let linkpath = path_arg(ctx, args[1])?;
    std::os::unix::fs::symlink(
        std::ffi::OsStr::from_bytes(&target),
        std::ffi::OsStr::from_bytes(&linkpath),
    )
    .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// `os.access(path, mode)` (`modos.zig:573-580`): returns
/// bool, never raises (invalid mode bits → `False`, probed —
/// the Zig's `access(path, mode) == 0` with `errno == EINVAL`
/// still yields `False`). Pure std has no `access(2)`, so the
/// check is approximated: `F_OK` is existence (following
/// symlinks, as `access` does); `R_OK`/`W_OK` on a file probe
/// by opening (root-correct — the kernel decides); on a
/// directory they read the mode bits (opening a directory for
/// write fails with EISDIR even when writes are allowed);
/// `X_OK` reads the `0o111` bits. See §4 for the residual
/// divergence (uid/gid-relative permission checks).
pub fn os_access_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let path = path_arg(ctx, args[0])?;
    let mode = arg_int(ctx, args[1])?;
    let path = std::ffi::OsStr::from_bytes(&path);
    let ok = access_impl(path, mode);
    Ok(Value::Bool(ok))
}

fn access_impl(path: &std::ffi::OsStr, mode: i64) -> bool {
    // Any bit outside F_OK|R_OK|W_OK|X_OK → access(2)
    // fails with EINVAL → False.
    if mode & !0b111 != 0 {
        return false;
    }
    let md = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if mode == 0 {
        return true;
    }
    let mode_bits = md.mode();
    let is_dir = md.is_dir();
    if mode & 4 != 0 {
        let readable = if is_dir {
            mode_bits & 0o444 != 0
        } else {
            std::fs::File::open(path).is_ok()
        };
        if !readable {
            return false;
        }
    }
    if mode & 2 != 0 {
        let writable = if is_dir {
            mode_bits & 0o222 != 0
        } else {
            std::fs::OpenOptions::new().write(true).open(path).is_ok()
        };
        if !writable {
            return false;
        }
    }
    if mode & 1 != 0 && mode_bits & 0o111 == 0 {
        return false;
    }
    true
}

/// `os.chmod(path, mode)` (`modos.zig:357-362`).
pub fn os_chmod_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    use std::os::unix::fs::PermissionsExt;
    let path = path_arg(ctx, args[0])?;
    let mode = arg_int(ctx, args[1])? as u32;
    std::fs::set_permissions(
        std::ffi::OsStr::from_bytes(&path),
        std::fs::Permissions::from_mode(mode),
    )
    .map_err(|e| os_err(ctx, &e))?;
    Ok(Value::None)
}

/// `os.chown(path, uid, gid)` — NOT IMPLEMENTED (no std API).
pub fn os_chown_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(not_impl(ctx, "os.chown is not implemented (no std chown API)"))
}

/// `os.getpid()` (`modos.zig:374-376`).
pub fn os_getpid_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx;
    Ok(Value::SmallInt(std::process::id() as i64))
}

/// `os.kill(pid, sig)` — NOT IMPLEMENTED (no std API).
pub fn os_kill_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(not_impl(ctx, "os.kill is not implemented (no std kill API)"))
}

/// `os.umask(mask)` — NOT IMPLEMENTED (no std API).
pub fn os_umask_builtin(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(not_impl(ctx, "os.umask is not implemented (no std umask API)"))
}

/// The `os.__getattr__` stub — intercepted by GcRef identity
/// in `call_function_ctx` (it needs the VM's import
/// machinery, which a plain `BuiltinFn` cannot reach). Called
/// directly only when the interception is bypassed — a bare
/// raise, like the other stubs.
pub fn os_getattr_stub(_ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    Err(PyError::PyRaise)
}

/// `py_os_getattr` (`modos.zig:226-237`): PEP 562 module
/// `__getattr__`. `os.path` imports `os_path` (failure
/// propagates, as the reference probe shows —
/// `ModuleNotFoundError: No module named 'os_path'`); any
/// other missing attr imports `os_ext`, swallowing ANY
/// failure (pending raises cleared, as the Zig's
/// `pyerr.pending_exc = 0` does), then looks the attr up
/// there; a miss raises `AttributeError("no attribute '%q'")`.
/// Called from the `call_function_ctx` interception of
/// `state.os_getattr_builtin` (the VM's `load_method_maybe_ctx`
/// runs the module `__getattr__` fun through that interception
/// — see the seam note there).
pub fn os_getattr_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    use crate::objects::objtype::objexcept::new_exception_msg;
    use crate::runtime::builtinimport::import_impl;
    use crate::runtime::import_search::load_attr_maybe;
    if n_args != 1 || n_kw != 0 {
        return Err(crate::runtime::vm_ops::type_error(
            ctx,
            state,
            "argument num/types mismatch",
        ));
    }
    let attr_bytes = str_data_pool(ctx.arena, ctx.pool, args[0])
        .unwrap_or_default();
    let attr_id = ctx
        .pool
        .from_strn(&attr_bytes)
        .map(u32::from)
        .unwrap_or(u32::MAX);
    let pool = ctx.pool;
    let mut intern = |b: &[u8]| pool.from_strn(b).map(u32::from);
    let path_id = static_qstr_id(b"path").expect("path is a static qstr");
    if attr_id == path_id {
        return import_impl(ctx, state, &[Value::Qstr(
            static_qstr_id(b"os_path").expect("os_path is a static qstr"),
        )], &mut intern);
    }
    let ext_id = static_qstr_id(b"os_ext").expect("os_ext is a static qstr");
    let ext = match import_impl(ctx, state, &[Value::Qstr(ext_id)], &mut intern) {
        Ok(m) => m,
        Err(_) => {
            // Swallow ANY failure (the Zig clears the pending
            // exception and treats the import as absent).
            let _ = ctx.take_pending_raise();
            state.pending_exc = None;
            let name =
                String::from_utf8_lossy(ctx.pool.qstr_str(attr_id as u16)).into_owned();
            return Err(new_exception_msg(
                ctx,
                state.exc.attribute_error,
                &format!("no attribute '{name}'"),
            ));
        }
    };
    match load_attr_maybe(ctx, state, ext, attr_id) {
        Ok((Some(v), _)) => Ok(v),
        Ok((None, _)) => {
            let name =
                String::from_utf8_lossy(ctx.pool.qstr_str(attr_id as u16)).into_owned();
            Err(new_exception_msg(
                ctx,
                state.exc.attribute_error,
                &format!("no attribute '{name}'"),
            ))
        }
        Err(e) => Err(e),
    }
}

/// Build the `os` module object (`modos.zig:636-706`, 60
/// entries, in the table's order). The dict is FIXED (the C's
/// ROM dict — `os.foo = 42` / `del os.sep` raise
/// AttributeError, probed). Writes `state.os_fd_builtins` /
/// `state.os_getattr_builtin` and returns the module.
pub fn build_os_module(ctx: &mut CallCtx, state: &mut VmState) -> Value {
    use crate::runtime::modos_fd::fd_stub;
    let types: &BuiltinTypes = ctx.types;
    let fun_builtin_var = state.vm_types.fun_builtin_var;
    let module_type = state.vm_types.module;
    let os_id = static_qstr_id(b"os").expect("os is a static qstr");
    let module = new_module(ctx, os_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => Value::Heap(m.dict),
            _ => unreachable!("new_module returns a module"),
        },
        _ => unreachable!("new_module returns a heap value"),
    };
    let f = |ctx: &mut CallCtx,
             name: &str,
             min: u16,
             max: u16,
             func: BuiltinFn,
             kw: Option<BuiltinKwFn>|
     -> Value { mk_fun(ctx, fun_builtin_var, name, min, max, func, kw) };
    // Bind each value before its `store` — `store(ctx, dict,
    // name, f(ctx, ...))` double-borrows `ctx` (the nested
    // closure call defeats two-phase borrows); modsys binds
    // every value first for the same reason.
    macro_rules! put {
        ($name:expr, $v:expr) => {{
            let v = $v;
            store(ctx, dict, $name, v);
        }};
    }
    put!("urandom", f(ctx, "urandom", 1, 1, os_urandom_builtin, None));
    // The six fd builtins are stubs (their real bodies live in
    // `modos_fd.rs` and need `state.os_fds`); their Values are
    // recorded on `state.os_fd_builtins` for the interception.
    let fd_open = fd_stub(ctx, fun_builtin_var, "open", 2, 3);
    let fd_close = fd_stub(ctx, fun_builtin_var, "close", 1, 1);
    let fd_read = fd_stub(ctx, fun_builtin_var, "read", 2, 2);
    let fd_write = fd_stub(ctx, fun_builtin_var, "write", 2, 2);
    let fd_lseek = fd_stub(ctx, fun_builtin_var, "lseek", 3, 3);
    let fd_fsync = fd_stub(ctx, fun_builtin_var, "fsync", 1, 1);
    state.os_fd_builtins = crate::runtime::vm::OsFdBuiltins {
        open: fd_open,
        close: fd_close,
        read: fd_read,
        write: fd_write,
        lseek: fd_lseek,
        fsync: fd_fsync,
    };
    put!("open", fd_open);
    put!("close", fd_close);
    put!("read", fd_read);
    put!("write", fd_write);
    put!("lseek", fd_lseek);
    put!("fsync", fd_fsync);
    put!("chmod", f(ctx, "chmod", 2, 2, os_chmod_builtin, None));
    put!("chown", f(ctx, "chown", 3, 3, os_chown_builtin, None));
    put!("getpid", f(ctx, "getpid", 0, 0, os_getpid_builtin, None));
    put!("kill", f(ctx, "kill", 2, 2, os_kill_builtin, None));
    put!("umask", f(ctx, "umask", 1, 1, os_umask_builtin, None));
    put!("getcwd", f(ctx, "getcwd", 0, 0, os_getcwd_builtin, None));
    put!("chdir", f(ctx, "chdir", 1, 1, os_chdir_builtin, None));
    put!(
        "getenv",
        f(ctx, "getenv", 1, 2, os_getenv_builtin, Some(os_getenv_builtin_kw))
    );
    put!("_environ", f(ctx, "_environ", 0, 0, os_environ_builtin, None));
    put!("putenv", f(ctx, "putenv", 2, 2, os_putenv_builtin, None));
    put!("unsetenv", f(ctx, "unsetenv", 1, 1, os_unsetenv_builtin, None));
    // `remove` and `unlink` are the SAME fun object.
    let unlink = f(ctx, "unlink", 1, 1, os_unlink_builtin, None);
    put!("remove", unlink);
    put!("unlink", unlink);
    put!("rename", f(ctx, "rename", 2, 2, os_rename_builtin, None));
    put!("mkdir", f(ctx, "mkdir", 1, 2, os_mkdir_builtin, None));
    put!("rmdir", f(ctx, "rmdir", 1, 1, os_rmdir_builtin, None));
    put!("stat", f(ctx, "stat", 1, 1, os_stat_builtin, None));
    put!("lstat", f(ctx, "lstat", 1, 1, os_lstat_builtin, None));
    put!("listdir", f(ctx, "listdir", 1, 1, os_listdir_builtin, None));
    put!("readlink", f(ctx, "readlink", 1, 1, os_readlink_builtin, None));
    put!("symlink", f(ctx, "symlink", 2, 2, os_symlink_builtin, None));
    put!("access", f(ctx, "access", 2, 2, os_access_builtin, None));
    // Constants (Linux values, probed).
    let c = |ctx: &mut CallCtx, name: &str, v: i64| store(ctx, dict, name, Value::SmallInt(v));
    c(ctx, "F_OK", 0);
    c(ctx, "R_OK", 4);
    c(ctx, "W_OK", 2);
    c(ctx, "X_OK", 1);
    c(ctx, "O_RDONLY", 0);
    c(ctx, "O_WRONLY", 1);
    c(ctx, "O_RDWR", 2);
    c(ctx, "O_CREAT", 64);
    c(ctx, "O_TRUNC", 512);
    c(ctx, "O_APPEND", 1024);
    c(ctx, "O_EXCL", 128);
    c(ctx, "SEEK_SET", 0);
    c(ctx, "SEEK_CUR", 1);
    c(ctx, "SEEK_END", 2);
    // Strings (ROM constants).
    let s = |ctx: &mut CallCtx, name: &str, v: &[u8]| {
        let value = new_str(ctx, types, v);
        store(ctx, dict, name, value)
    };
    s(ctx, "sep", b"/");
    s(ctx, "altsep", b"");
    s(ctx, "extsep", b".");
    s(ctx, "linesep", b"\n");
    s(ctx, "curdir", b".");
    s(ctx, "pardir", b"..");
    s(ctx, "name", b"posix");
    // `__getattr__` (PEP 562) — a stub, intercepted in
    // `call_function_ctx` (it needs the import machinery).
    let getattr = f(ctx, "__getattr__", 1, 1, os_getattr_stub, None);
    state.os_getattr_builtin = getattr;
    put!("__getattr__", getattr);
    // Fix the dict (the C's ROM dict).
    if let Value::Heap(r) = dict {
        if let Obj::Dict(d) = ctx.arena.get_mut(r) {
            d.map.is_fixed = true;
        }
    }
    module
}
