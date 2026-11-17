//! Shared test support for the iters lane.
//!
//! The call/getitem seams are test doubles: the "function" values
//! in tests are `Value::Qstr` tokens (pre-interned names such as
//! `"str"`, `"add"`, `"raises"`), and each test module's seam
//! closure dispatches on the token bytes. The real functions
//! (`str`, `len`, `+`, …) are C10/C13's — the seam is the
//! boundary, and the token dispatch is documented wherever it is
//! used. Nothing here is a production path.

use super::iter::{CallOutcome, IterError, IterSeams, iternext};
use crate::memory::gc::CallCtx;
use crate::objects::value::{Obj, StrData, Value};

/// The token bytes of a `Qstr` function/object value (`None`
/// for any other value).
pub(crate) fn token_bytes<'a>(ctx: &'a CallCtx, v: Value) -> Option<&'a [u8]> {
    match v {
        Value::Qstr(id) => Some(ctx.pool.qstr_str(id as u16)),
        _ => None,
    }
}

/// Allocate a heap `Str` holding `s` (for seam closures that
/// build string results, e.g. the `"str"` token).
pub(crate) fn heap_str(ctx: &mut CallCtx, s: &str) -> Value {
    let t = ctx.types.str_;
    Value::Heap(ctx.alloc(
        Some(t),
        Obj::Str(StrData {
            bytes: s.as_bytes().to_vec(),
        }),
    ))
}

/// Drain an iterator into a `Vec` via [`iternext`].
pub(crate) fn collect_all(
    ctx: &mut CallCtx,
    it: Value,
    seams: &mut IterSeams<'_>,
) -> Result<Vec<Value>, IterError> {
    let mut out = Vec::new();
    while let Some(v) = iternext(ctx, it, seams)? {
        out.push(v);
    }
    Ok(out)
}

/// Seams whose closures panic if called — for tests that must
/// never reach the VM (e.g. pure `getiter` shape tests).
pub(crate) struct PanicSeams {
    call: Box<dyn FnMut(&mut CallCtx, Value, &[Value]) -> CallOutcome>,
    getitem: Box<dyn FnMut(&mut CallCtx, Value, i64) -> CallOutcome>,
}

impl PanicSeams {
    pub(crate) fn new() -> Self {
        PanicSeams {
            call: Box::new(|_, _, _| panic!("test called the call seam unexpectedly")),
            getitem: Box::new(|_, _, _| panic!("test called the getitem seam unexpectedly")),
        }
    }

    pub(crate) fn seams(&mut self) -> IterSeams<'_> {
        IterSeams {
            call: &mut *self.call,
            getitem: &mut *self.getitem,
        }
    }
}
