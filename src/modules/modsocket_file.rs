//! The `socketfile` object returned by `sock.makefile()`:
//! `read` / `readline` / `write` / `flush` / `close` over the
//! buffered reader/writer pair in `SocketFileState`
//! (`modsocket.rs`). Split out of `modsocket.rs` so that file
//! stays under the 2500-line cap; this module is NOT
//! feature-gated (the file object exists in both builds).

use std::io::{BufRead, Read, Write};

use crate::memory::gc::CallCtx;
use crate::modules::modsocket::{
    int_arg, io_os_error, self_file, set_file_id, socket_files, str_arg,
};
use crate::objects::strings::objstr_a::new_bytes;
use crate::objects::value::Value;
use crate::pyerr::{PyError, PyResult};

/// A small int, infallible for socket fields (all fit the
/// 62-bit window).
fn small(n: i64) -> Value {
    Value::small_int(n).expect("socket field fits the small-int window")
}

/// `f.read([n]) -> bytes`: `n < 0` (or omitted) reads to
/// EOF; `read(0)` is `b''`. The reader is taken out of
/// the table for the call (no lock across blocking I/O)
/// and put back afterwards. A closed file or a
/// write-only file is `ValueError`.
pub(crate) fn file_read(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let n = match args.get(1) {
        Some(v) => int_arg(ctx, *v)?,
        None => -1,
    };
    let mut reader = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_read {
            ctx.record_raise("ValueError", Some("not readable"));
            return Err(PyError::PyRaise);
        }
        match st.reader.take() {
            Some(r) => r,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let mut buf = Vec::new();
    let res = if n < 0 {
        reader.read_to_end(&mut buf)
    } else {
        let mut limited = reader.take(n as u64);
        let r = limited.read_to_end(&mut buf);
        reader = limited.into_inner();
        r
    };
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.reader = Some(reader);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf))
}

/// `f.readline([n]) -> bytes`: one line up to and
/// including `'\n'` (or EOF); `n >= 0` caps the bytes.
/// Same take/put-back and error rules as `read`.
pub(crate) fn file_readline(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let n = match args.get(1) {
        Some(v) => int_arg(ctx, *v)?,
        None => -1,
    };
    let mut reader = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_read {
            ctx.record_raise("ValueError", Some("not readable"));
            return Err(PyError::PyRaise);
        }
        match st.reader.take() {
            Some(r) => r,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let mut buf = Vec::new();
    let res = if n < 0 {
        reader.read_until(b'\n', &mut buf)
    } else {
        let mut limited = reader.take(n as u64);
        let r = limited.read_until(b'\n', &mut buf);
        reader = limited.into_inner();
        r
    };
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.reader = Some(reader);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(new_bytes(ctx, ctx.types, &buf))
}

/// `f.write(bytes) -> int`: the data must be str/bytes
/// (a bytearray is TypeError "can't convert to str
/// implicitly"); returns the bytes written. A closed
/// file or a read-only file is `ValueError`.
pub(crate) fn file_write(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let data = str_arg(ctx, args[1])?;
    let mut writer = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        if !st.can_write {
            ctx.record_raise("ValueError", Some("not writable"));
            return Err(PyError::PyRaise);
        }
        match st.writer.take() {
            Some(w) => w,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let res = writer.write_all(&data);
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.writer = Some(writer);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(small(data.len() as i64))
}

/// `f.flush() -> None`: flushes the writer. A closed
/// file is `ValueError`.
pub(crate) fn file_flush(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (_r, id) = self_file(ctx, args)?;
    let mut writer = {
        let mut files = socket_files();
        let Some(st) = files.get_mut(&(id as u64)) else {
            ctx.record_raise("ValueError", Some("I/O operation on closed file."));
            return Err(PyError::PyRaise);
        };
        match st.writer.take() {
            Some(w) => w,
            None => {
                ctx.record_raise("ValueError", Some("I/O operation on closed file."));
                return Err(PyError::PyRaise);
            }
        }
    };
    let res = writer.flush();
    {
        let mut files = socket_files();
        if let Some(st) = files.get_mut(&(id as u64)) {
            st.writer = Some(writer);
        }
    }
    res.map_err(|e| io_os_error(ctx, e))?;
    Ok(Value::None)
}

/// `f.close() -> None` (idempotent): flushes the writer,
/// drops the table entry (closing both dups), and stores
/// `-1`.
pub(crate) fn file_close(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let (r, id) = self_file(ctx, args)?;
    let mut files = socket_files();
    if let Some(mut st) = files.remove(&(id as u64)) {
        if let Some(mut w) = st.writer.take() {
            let _ = w.flush();
        }
    }
    drop(files);
    set_file_id(ctx, r, -1);
    Ok(Value::None)
}
