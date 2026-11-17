//! Shared fixture for the stream-protocol tests
//! (`stream_tests.rs` / `stream_tests_b.rs`).
//!
//! The backends are test doubles over an `Obj::Iterator`
//! carrier (`{ source: bytearray, index: pos, extra:
//! [chunk] }` — the C11 iterator convention): the source
//! bytearray holds the stream's bytes, `index` is the read
//! cursor, and `extra[0]` caps the bytes per backend call
//! (0 = unlimited) so partial reads/writes are observable.
//! The stream object's TYPE carries the protocol
//! (`TypeData.protocol`); five types cover the matrix
//! (binary / text / write-only / read-only / failing).

use super::stream::*;
use super::stream_methods::STREAM_METHODS;
use crate::memory::gc::CallCtx;
use crate::objects::containers::binary::buffer_bytes;
use crate::objects::containers::objarray::new_bytearray;
use crate::objects::containers::tests::{empty_dict, Fixture};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::{IoctlArg, StreamP};
use crate::objects::value::{GcRef, IteratorData, Obj, TypeData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_os_error;

// ── Fake backends ────────────────────────────────────────────────────────

/// `(source bytearray, read cursor, per-call chunk cap)`.
fn iter_parts(ctx: &CallCtx, stream: Value) -> (Value, usize, usize) {
    let Value::Heap(r) = stream else {
        panic!("stream is not a heap object");
    };
    match ctx.arena.get(r) {
        Obj::Iterator(d) => {
            let chunk = match d.extra.first() {
                Some(Value::SmallInt(n)) => *n as usize,
                _ => 0,
            };
            (d.source, d.index, chunk)
        }
        _ => panic!("stream is not an iterator carrier"),
    }
}

fn set_cursor(ctx: &mut CallCtx, stream: Value, pos: usize) {
    let Value::Heap(r) = stream else {
        panic!("stream is not a heap object");
    };
    if let Obj::Iterator(d) = ctx.arena.get_mut(r) {
        d.index = pos;
    }
}

fn fake_read(ctx: &mut CallCtx, stream: Value, buf: &mut [u8]) -> PyResult<usize> {
    let (src, pos, chunk) = iter_parts(ctx, stream);
    let (bytes, _) = buffer_bytes(ctx, src).expect("source is a bytearray");
    let avail = bytes.len().saturating_sub(pos);
    let mut n = avail.min(buf.len());
    if chunk != 0 {
        n = n.min(chunk);
    }
    buf[..n].copy_from_slice(&bytes[pos..pos + n]);
    set_cursor(ctx, stream, pos + n);
    Ok(n)
}

fn fake_write(ctx: &mut CallCtx, stream: Value, buf: &[u8]) -> PyResult<usize> {
    let (src, _pos, chunk) = iter_parts(ctx, stream);
    let mut n = buf.len();
    if chunk != 0 {
        n = n.min(chunk);
    }
    let Value::Heap(src_r) = src else {
        return Err(PyError::PyRaise);
    };
    match ctx.arena.get_mut(src_r) {
        Obj::Array(a) => {
            a.data.extend_from_slice(&buf[..n]);
            a.len = a.data.len();
            Ok(n)
        }
        _ => Err(PyError::PyRaise),
    }
}

fn fake_ioctl(ctx: &mut CallCtx, stream: Value, request: u32, arg: IoctlArg) -> PyResult<usize> {
    match request {
        MP_STREAM_FLUSH => Ok(0),
        MP_STREAM_CLOSE => Ok(0),
        MP_STREAM_SEEK => {
            let IoctlArg::Seek(state) = arg else {
                return Err(raise_os_error(ctx, MP_EINVAL));
            };
            let (src, pos, _) = iter_parts(ctx, stream);
            let (bytes, _) = buffer_bytes(ctx, src).expect("source is a bytearray");
            let len = bytes.len() as i64;
            let new_pos = match state.whence {
                SEEK_SET => state.offset,
                SEEK_CUR => pos as i64 + state.offset,
                SEEK_END => len + state.offset,
                _ => return Err(raise_os_error(ctx, MP_EINVAL)),
            };
            let new_pos = new_pos.clamp(0, len);
            state.result = new_pos;
            set_cursor(ctx, stream, new_pos as usize);
            Ok(0)
        }
        // Unknown request: echo the caller's value back (so the
        // test can assert the value reached the backend).
        _ => Ok(match arg {
            IoctlArg::Raw(v) => v,
            _ => 0,
        }),
    }
}

fn fail_read(ctx: &mut CallCtx, _stream: Value, _buf: &mut [u8]) -> PyResult<usize> {
    Err(raise_os_error(ctx, 5))
}

fn fail_write(ctx: &mut CallCtx, _stream: Value, _buf: &[u8]) -> PyResult<usize> {
    Err(raise_os_error(ctx, 6))
}

fn fail_ioctl(ctx: &mut CallCtx, _stream: Value, _req: u32, _arg: IoctlArg) -> PyResult<usize> {
    Err(raise_os_error(ctx, 7))
}

fn failing_stream_p() -> StreamP {
    StreamP::new(Some(fail_read), Some(fail_write), Some(fail_ioctl), false)
}

// ── Fixture ──────────────────────────────────────────────────────────────

fn make_stream_type(fx: &mut Fixture, p: StreamP) -> GcRef {
    let dict = empty_dict(&mut fx.arena, &fx.roots);
    let data = TypeData {
        name: static_qstr_id(b"str").expect("str is a static qstr"),
        flags: 0,
        base: None,
        bases: None,
        mro: Vec::new(),
        dict,
        attr: None,
        make_new: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: None,
        namedtuple_fields: None,
        protocol: Some(p),
    };
    fx.arena
        .alloc(Some(fx.types.type_), Obj::Type(data), &fx.roots)
}

pub(crate) struct Sfx {
    pub(crate) fx: Fixture,
    pub(crate) stream_type: GcRef,
    pub(crate) text_type: GcRef,
    pub(crate) write_only_type: GcRef,
    pub(crate) read_only_type: GcRef,
    pub(crate) failing_type: GcRef,
}

impl Sfx {
    pub(crate) fn new() -> Self {
        let mut fx = Fixture::new();
        let stream_type = make_stream_type(
            &mut fx,
            StreamP::new(Some(fake_read), Some(fake_write), Some(fake_ioctl), false),
        );
        let text_type = make_stream_type(
            &mut fx,
            StreamP::new(Some(fake_read), Some(fake_write), Some(fake_ioctl), true),
        );
        let write_only_type = make_stream_type(
            &mut fx,
            StreamP::new(None, Some(fake_write), Some(fake_ioctl), false),
        );
        let read_only_type = make_stream_type(
            &mut fx,
            StreamP::new(Some(fake_read), None, Some(fake_ioctl), false),
        );
        let failing_type = make_stream_type(&mut fx, failing_stream_p());
        for t in [
            stream_type,
            text_type,
            write_only_type,
            read_only_type,
            failing_type,
        ] {
            fx.roots.push(Value::Heap(t));
        }
        Sfx {
            fx,
            stream_type,
            text_type,
            write_only_type,
            read_only_type,
            failing_type,
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        self.fx.ctx()
    }

    /// A stream object over `data` (`chunk` caps the bytes per
    /// backend call; 0 = unlimited).
    pub(crate) fn stream(&mut self, type_: GcRef, data: &[u8], chunk: usize) -> Value {
        let ba = self.fx.types.bytearray;
        let barr = {
            let mut ctx = self.ctx();
            new_bytearray(&mut ctx, ba, data)
        };
        let it = IteratorData {
            source: Value::Heap(barr),
            index: 0,
            extra: vec![Value::SmallInt(chunk as i64)],
        };
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(Some(type_), Obj::Iterator(it)))
    }

    /// A fresh bytearray value (the type ref is hoisted before
    /// the `CallCtx` is built — see the `Fixture::ctx`
    /// borrowing note in `heap_str`).
    pub(crate) fn bytearray(&mut self, data: &[u8]) -> Value {
        let ba = self.fx.types.bytearray;
        let mut ctx = self.ctx();
        Value::Heap(new_bytearray(&mut ctx, ba, data))
    }

    /// The source bytearray's current bytes (the sink's
    /// contents for write tests).
    pub(crate) fn source_bytes(&mut self, stream: Value) -> Vec<u8> {
        let (src, _, _) = {
            let ctx = self.ctx();
            iter_parts(&ctx, stream)
        };
        let ctx = self.ctx();
        buffer_bytes(&ctx, src).expect("source is a bytearray").0
    }

    /// The value as bytes (`Qstr` statics included — `new_str`
    /// interns them).
    pub(crate) fn as_bytes(&mut self, v: Value) -> Vec<u8> {
        match v {
            Value::Heap(r) => match self.fx.arena.get(r) {
                Obj::Bytes(b) => b.bytes.clone(),
                Obj::Str(s) => s.bytes.clone(),
                other => panic!("not bytes/str: {other:?}"),
            },
            Value::Qstr(q) => self.fx.pool.qstr_str(q as u16).to_vec(),
            other => panic!("not bytes/str: {other:?}"),
        }
    }

    pub(crate) fn as_int(v: Value) -> i64 {
        match v {
            Value::SmallInt(n) => n,
            other => panic!("not a small int: {other:?}"),
        }
    }

    /// `(exc_name, message)` of the last recorded raise.
    pub(crate) fn pending(&self) -> (Option<&'static str>, Option<String>) {
        match &self.fx.pending_raise {
            Some(p) => (Some(p.exc_name), p.message.clone()),
            None => (None, None),
        }
    }

    /// `(exc_name, int arg)` of the last recorded raise.
    pub(crate) fn pending_arg(&self) -> (Option<&'static str>, Option<i64>) {
        match &self.fx.pending_raise {
            Some(p) => (
                Some(p.exc_name),
                match p.arg {
                    Some(Value::SmallInt(n)) => Some(n),
                    _ => None,
                },
            ),
            None => (None, None),
        }
    }

    pub(crate) fn clear_pending(&mut self) {
        self.fx.pending_raise = None;
    }
}

/// Call one `STREAM_METHODS` entry by name (exercises the
/// table dispatch, not just the body).
pub(crate) fn call_method(name: &str, ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let entry = STREAM_METHODS
        .iter()
        .find(|e| e.name == name)
        .expect("stream method exists");
    (entry.func)(ctx, args)
}

