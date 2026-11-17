//! Lane C16 stream-protocol tests, part 2: `readinto` /
//! `write`, `readline` / `readlines` / iter, `seek` / `tell` /
//! `flush` / `close` / `exit` / `ioctl`, error paths, and the
//! `STREAM_METHODS` table. Fixture: `stream_fixture.rs`.

use super::stream::{get_stream, is_nonblocking_error, stream_write_adaptor};
use super::stream_fixture::{call_method, Sfx};
use super::stream_methods::{stream_unbuffered_iter, STREAM_METHODS};
use super::stream::{MP_EINVAL, SEEK_CUR, SEEK_END};
use crate::objects::containers::binary::{
    buffer_bytes, ARRAY_TYPECODE_FLAG_RW, BYTEARRAY_TYPECODE,
};
use crate::objects::containers::objarray::new_bytearray;
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{ArrayData, Obj, Value};
use crate::pyerr::PyError;

// ── readinto / write ─────────────────────────────────────────────────────

#[test]
fn stream_readinto_basic() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"hello", 0);
    let dest = s.bytearray(b"_____");
    let n = {
        let mut ctx = s.ctx();
        call_method("readinto", &mut ctx, &[st, dest]).expect("readinto")
    };
    assert_eq!(Sfx::as_int(n), 5);
    let ctx = s.ctx();
    assert_eq!(buffer_bytes(&ctx, dest).unwrap().0, b"hello");
}

#[test]
fn stream_readinto_max_len_and_offset() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"hello", 0);
    let dest = s.bytearray(b"_____");
    // readinto(buf, max_len=2)
    let n = {
        let mut ctx = s.ctx();
        call_method("readinto", &mut ctx, &[st, dest, Value::SmallInt(2)]).expect("readinto")
    };
    assert_eq!(Sfx::as_int(n), 2);
    // readinto(buf, off=3, max_len=2) — the cursor is at 2 now.
    let n = {
        let mut ctx = s.ctx();
        call_method(
            "readinto",
            &mut ctx,
            &[st, dest, Value::SmallInt(3), Value::SmallInt(2)],
        )
        .expect("readinto off")
    };
    assert_eq!(Sfx::as_int(n), 2);
    let ctx = s.ctx();
    assert_eq!(buffer_bytes(&ctx, dest).unwrap().0, b"he_ll".as_slice());
}

#[test]
fn stream_readinto1_once() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"hello", 1);
    let dest = s.bytearray(b"_____");
    let n = {
        let mut ctx = s.ctx();
        call_method("readinto1", &mut ctx, &[st, dest]).expect("readinto1")
    };
    assert_eq!(Sfx::as_int(n), 1);
    let ctx = s.ctx();
    assert_eq!(buffer_bytes(&ctx, dest).unwrap().0, b"h____");
}

#[test]
fn stream_readinto_non_buffer_is_type_error() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"hello", 0);
    let r = {
        let mut ctx = s.ctx();
        call_method("readinto", &mut ctx, &[st, Value::SmallInt(123)])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending().0, Some("TypeError"));
}

#[test]
fn stream_readinto_memoryview_writes_through() {
    // A view over base[1..3]: the write-back must follow the
    // source chain (free * sz) into the base bytearray.
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"xyzw", 0);
    let base = {
        let ba = s.fx.types.bytearray;
        let mut ctx = s.ctx();
        new_bytearray(&mut ctx, ba, b"....")
    };
    let mv = s.fx.memoryview;
    let view = {
        let mut ctx = s.ctx();
        Value::Heap(ctx.alloc(
            Some(mv),
            Obj::Array(ArrayData {
                typecode: BYTEARRAY_TYPECODE | ARRAY_TYPECODE_FLAG_RW,
                free: 1,
                len: 2,
                data: vec![],
                source: Some(base),
            }),
        ))
    };
    let n = {
        let mut ctx = s.ctx();
        call_method("readinto", &mut ctx, &[st, view]).expect("readinto view")
    };
    assert_eq!(Sfx::as_int(n), 2);
    let ctx = s.ctx();
    assert_eq!(
        buffer_bytes(&ctx, Value::Heap(base)).unwrap().0,
        b".xy.".as_slice()
    );
}

#[test]
fn stream_write_basic_and_loop() {
    let mut s = Sfx::new();
    // Empty source, chunk=2: the looped write still pushes all.
    let st = s.stream(s.stream_type, b"", 2);
    let src = s.bytearray(b"hello");
    let n = {
        let mut ctx = s.ctx();
        call_method("write", &mut ctx, &[st, src]).expect("write")
    };
    assert_eq!(Sfx::as_int(n), 5);
    assert_eq!(s.source_bytes(st), b"hello");
}

#[test]
fn stream_write1_once() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"", 2);
    let src = s.bytearray(b"hello");
    let n = {
        let mut ctx = s.ctx();
        call_method("write1", &mut ctx, &[st, src]).expect("write1")
    };
    assert_eq!(Sfx::as_int(n), 2);
    assert_eq!(s.source_bytes(st), b"he");
}

#[test]
fn stream_write_off_and_max_len() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"", 0);
    let src = s.bytearray(b"hello");
    let n = {
        let mut ctx = s.ctx();
        call_method(
            "write",
            &mut ctx,
            &[st, src, Value::SmallInt(1), Value::SmallInt(2)],
        )
        .expect("write off")
    };
    assert_eq!(Sfx::as_int(n), 2);
    assert_eq!(s.source_bytes(st), b"el");
}

#[test]
fn stream_write_adaptor_appends() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"ab", 0);
    {
        let mut ctx = s.ctx();
        stream_write_adaptor(&mut ctx, st, b"cd").expect("adaptor");
    }
    assert_eq!(s.source_bytes(st), b"abcd");
}

// ── readline / readlines / iter ──────────────────────────────────────────

#[test]
fn stream_readline_basic() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"ab\nc", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st]).expect("readline")
    };
    assert_eq!(s.as_bytes(v), b"ab\n");
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st]).expect("readline 2")
    };
    assert_eq!(s.as_bytes(v), b"c");
    // EOF: empty.
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st]).expect("readline eof")
    };
    assert_eq!(s.as_bytes(v), b"");
}

#[test]
fn stream_readline_max_size() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"ab\ncd\n", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, Value::SmallInt(2)])
            .expect("readline(2)")
    };
    assert_eq!(s.as_bytes(v), b"ab");
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, Value::SmallInt(0)])
            .expect("readline(0)")
    };
    assert_eq!(s.as_bytes(v), b"");
}

#[test]
fn stream_readline_smallintvalue_quirks() {
    // `smallIntValue` is a raw tag-unshift: True → 15,
    // False → 7, None → 3 (mirrors.zig immediates).
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789abcdefghij", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, Value::Bool(true)])
            .expect("readline(True)")
    };
    assert_eq!(s.as_bytes(v), b"0123456789abcde");
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, Value::Bool(false)])
            .expect("readline(False)")
    };
    assert_eq!(s.as_bytes(v), b"0123456");
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, Value::None]).expect("readline(None)")
    };
    assert_eq!(s.as_bytes(v), b"012");
}

#[test]
fn stream_readline_heap_int_is_memory_error() {
    // A heap int reads back as a huge pointer-shifted number;
    // its vstr_init fails with MemoryError.
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    let big = {
        let int_t = s.fx.types.int;
        let ctx = s.ctx();
        Value::int(ctx.arena, Some(int_t), ctx.roots, 1i64 << 62)
    };
    assert!(matches!(big, Value::Heap(_)));
    let r = {
        let mut ctx = s.ctx();
        call_method("readline", &mut ctx, &[st, big])
    };
    assert!(matches!(r, Err(PyError::OutOfMemory)));
}

#[test]
fn stream_readlines_basic() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"a\nb\n", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("readlines", &mut ctx, &[st]).expect("readlines")
    };
    let Value::Heap(r) = v else {
        panic!("not a list");
    };
    let items = match s.fx.arena.get(r) {
        Obj::List(l) => l.items.clone(),
        other => panic!("not a list: {other:?}"),
    };
    assert_eq!(items.len(), 2);
    assert_eq!(s.as_bytes(items[0]), b"a\n");
    assert_eq!(s.as_bytes(items[1]), b"b\n");
}

#[test]
fn stream_unbuffered_iter_basic() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"a\n", 0);
    let first = {
        let mut ctx = s.ctx();
        stream_unbuffered_iter(&mut ctx, st).expect("iter 1")
    };
    assert_eq!(first.map(|v| s.as_bytes(v)), Some(b"a\n".to_vec()));
    let second = {
        let mut ctx = s.ctx();
        stream_unbuffered_iter(&mut ctx, st).expect("iter 2")
    };
    assert!(second.is_none());
}

// ── seek / tell / flush / close / exit / ioctl ──────────────────────────

#[test]
fn stream_seek_and_tell() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("seek", &mut ctx, &[st, Value::SmallInt(2)]).expect("seek(2)")
    };
    assert_eq!(Sfx::as_int(v), 2);
    let v = {
        let mut ctx = s.ctx();
        call_method("tell", &mut ctx, &[st]).expect("tell")
    };
    assert_eq!(Sfx::as_int(v), 2);
    // SEEK_CUR (+1) and SEEK_END (-2).
    let v = {
        let mut ctx = s.ctx();
        call_method("seek", &mut ctx, &[st, Value::SmallInt(1), Value::SmallInt(i64::from(SEEK_CUR))])
            .expect("seek cur")
    };
    assert_eq!(Sfx::as_int(v), 3);
    let v = {
        let mut ctx = s.ctx();
        call_method("seek", &mut ctx, &[st, Value::SmallInt(-2), Value::SmallInt(i64::from(SEEK_END))])
            .expect("seek end")
    };
    assert_eq!(Sfx::as_int(v), 8);
    // The cursor really moved: the next read starts at 8.
    let v = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st]).expect("readall")
    };
    assert_eq!(s.as_bytes(v), b"89");
}

#[test]
fn stream_seek_negative_set_is_einval() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"0123456789", 0);
    let r = {
        let mut ctx = s.ctx();
        call_method("seek", &mut ctx, &[st, Value::SmallInt(-1)])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending_arg(), (Some("OSError"), Some(MP_EINVAL as i64)));
}

#[test]
fn stream_flush_close_exit() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    let v = {
        let mut ctx = s.ctx();
        call_method("flush", &mut ctx, &[st]).expect("flush")
    };
    assert!(matches!(v, Value::None));
    let v = {
        let mut ctx = s.ctx();
        call_method("close", &mut ctx, &[st]).expect("close")
    };
    assert!(matches!(v, Value::None));
    let v = {
        let mut ctx = s.ctx();
        call_method(
            "__exit__",
            &mut ctx,
            &[st, Value::None, Value::None, Value::None],
        )
        .expect("exit")
    };
    assert!(matches!(v, Value::None));
}

#[test]
fn stream_ioctl_roundtrip() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    // An int arg comes back as the truncated int.
    let v = {
        let mut ctx = s.ctx();
        call_method("ioctl", &mut ctx, &[st, Value::SmallInt(99), Value::SmallInt(7)])
            .expect("ioctl int")
    };
    assert_eq!(Sfx::as_int(v), 7);
    // A buffer arg comes back as its pointer (a real address,
    // not the small int it would be without a buffer).
    let buf = s.bytearray(b"xy");
    let v = {
        let mut ctx = s.ctx();
        call_method("ioctl", &mut ctx, &[st, Value::SmallInt(99), buf]).expect("ioctl buf")
    };
    assert!(Sfx::as_int(v) > 1_000_000);
}

// ── protocol resolution + errors ─────────────────────────────────────────

#[test]
fn stream_get_stream_raise_no_protocol() {
    // A plain bytes value has no protocol slot.
    let mut s = Sfx::new();
    let plain = s.bytearray(b"abc");
    let r = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[plain])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(
        s.pending(),
        (
            Some("OSError"),
            Some("stream operation not supported".to_string())
        )
    );
}

#[test]
fn stream_missing_direction_raises() {
    // Write-only: read raises; read-only: write raises.
    let mut s = Sfx::new();
    let wo = s.stream(s.write_only_type, b"", 0);
    let r = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[wo])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending().0, Some("OSError"));
    s.clear_pending();
    let ro = s.stream(s.read_only_type, b"abc", 0);
    let src = s.bytearray(b"x");
    let r = {
        let mut ctx = s.ctx();
        call_method("write", &mut ctx, &[ro, src])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending().0, Some("OSError"));
}

#[test]
fn stream_failing_backend_propagates_oserror() {
    let mut s = Sfx::new();
    let st = s.stream(s.failing_type, b"abc", 0);
    let r = {
        let mut ctx = s.ctx();
        call_method("read", &mut ctx, &[st])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending_arg(), (Some("OSError"), Some(5)));
    s.clear_pending();
    let src = s.bytearray(b"x");
    let r = {
        let mut ctx = s.ctx();
        call_method("write", &mut ctx, &[st, src])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending_arg(), (Some("OSError"), Some(6)));
    s.clear_pending();
    let r = {
        let mut ctx = s.ctx();
        call_method("flush", &mut ctx, &[st])
    };
    assert!(matches!(r, Err(PyError::PyRaise)));
    assert_eq!(s.pending_arg(), (Some("OSError"), Some(7)));
}

#[test]
fn stream_is_nonblocking_error_is_false() {
    assert!(!is_nonblocking_error(11));
    assert!(!is_nonblocking_error(0));
}

#[test]
fn stream_methods_table_shape() {
    assert_eq!(STREAM_METHODS.len(), 14);
    let expected: &[(&str, u16, u16)] = &[
        ("read", 1, 2),
        ("read1", 1, 2),
        ("write", 2, 4),
        ("write1", 2, 4),
        ("readinto", 2, 3),
        ("readinto1", 2, 3),
        ("readline", 1, 2),
        ("readlines", 1, 1),
        ("close", 1, 1),
        ("__exit__", 4, 4),
        ("seek", 2, 3),
        ("tell", 1, 1),
        ("flush", 1, 1),
        ("ioctl", 2, 3),
    ];
    for (entry, (name, min, max)) in STREAM_METHODS.iter().zip(expected.iter()) {
        assert_eq!(entry.name, *name);
        assert_eq!(entry.min, *min);
        assert_eq!(entry.max, *max);
        assert!(entry.kw_func.is_none());
        // Most method names are static qstrs (D lanes build
        // the locals dict from this table) — except `read1` /
        // `write1` / `readinto1`, which are NOT in the static
        // table either (checked in qstrdefs.generated.h: the
        // original interns them dynamically at type setup, so
        // D must resolve those three via the runtime pool).
        let dynamic = ["read1", "write1", "readinto1"];
        if dynamic.contains(&entry.name) {
            assert!(
                static_qstr_id(entry.name.as_bytes()).is_none(),
                "{} unexpectedly became a static qstr",
                entry.name
            );
        } else {
            assert!(
                static_qstr_id(entry.name.as_bytes()).is_some(),
                "{} is not a static qstr",
                entry.name
            );
        }
    }
}

#[test]
fn stream_get_stream_returns_protocol() {
    let mut s = Sfx::new();
    let st = s.stream(s.stream_type, b"abc", 0);
    let ctx = s.ctx();
    let p = get_stream(&ctx, st).expect("protocol present");
    assert!(p.read.is_some());
    assert!(p.write.is_some());
    assert!(p.ioctl.is_some());
    assert!(!p.is_text);
    let tx = s.stream(s.text_type, b"abc", 0);
    let ctx = s.ctx();
    assert!(get_stream(&ctx, tx).unwrap().is_text);
    // A plain value has no protocol.
    let plain = Value::SmallInt(5);
    let ctx = s.ctx();
    assert!(get_stream(&ctx, plain).is_none());
    // get_stream resolves through obj_get_type: the
    // stream's header type is the fake stream type.
    let ctx = s.ctx();
    assert_eq!(obj_get_type(ctx.arena, ctx.types, st), Some(s.stream_type));
}
