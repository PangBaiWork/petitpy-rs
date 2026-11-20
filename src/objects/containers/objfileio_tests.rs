//! Tests for Lane D2, step 2: `open()` + the FILE stream
//! backend (`file_io.zig`).
//!
//! CallCtx level: the `open` NAME binding is D1's (this
//! tree's `open_builtin` is the implementation D1's stub is
//! filled with at rebase), so the tests call `open_builtin`
//! / `open_builtin_kw` directly and drive the protocol fns.
//! Expected values are the reference binary's, probed
//! 2026-11-18 (see `scripts/d2_result.txt` §2).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objfileio::{
    build_stdio_objects, fileio_fileno_builtin, fileio_ioctl, fileio_print, fileio_read,
    fileio_write, open_builtin, open_builtin_kw, textio_attr,
};
use crate::objects::slots::{IoctlArg, PrintKind, SeekState};
use crate::objects::strings::objstr_a::new_str;
use crate::objects::value::Value;
use crate::pyerr::PyError;
use crate::runtime::vm::Vm;
use crate::support::stream::{MP_STREAM_CLOSE, MP_STREAM_FLUSH, MP_STREAM_SEEK, SEEK_END, SEEK_SET};

/// A fresh VM (D2's `Vm::new` already builds the fileio
/// types) plus the read-back helpers.
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        Self { vm: Vm::new() }
    }

    /// A temp file path unique to this process AND this
    /// test (tests run in parallel threads in one process, so
    /// a shared dir races on cleanup), under a fresh temp dir
    /// (removed by the caller).
    fn tmp_path(&self, tag: &str, name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "petitpy_d2_fileio_{}_{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join(name)
    }

    fn open(&mut self, path: &std::path::Path, mode: &str) -> Value {
        let (mut ctx, _state) = self.vm.split();
        let types = ctx.types;
        let p = new_str(&mut ctx, types, path.as_os_str().as_encoded_bytes());
        let m = new_str(&mut ctx, types, mode.as_bytes());
        open_builtin(&mut ctx, &[p, m]).expect("open succeeds")
    }
}

/// The recorded raise after a `PyRaise` return: (name, arg).
fn pending(ctx: &CallCtx) -> (String, Value) {
    let pr = ctx.pending_raise.as_ref().expect("raise recorded");
    (pr.exc_name.to_string(), pr.arg.expect("os error carries its errno"))
}

#[test]
fn open_text_write_read_roundtrip() {
    let mut fx = Fx::new();
    let path = fx.tmp_path("text", "f.txt");
    let f = fx.open(&path, "w");
    let (mut ctx, _state) = fx.vm.split();
    assert_eq!(fileio_write(&mut ctx, f, b"hello"), Ok(5));
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_FLUSH, IoctlArg::None),
        Ok(0)
    );
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    let f = fx.open(&path, "r");
    let (mut ctx, _state) = fx.vm.split();
    let mut buf = [0u8; 16];
    assert_eq!(fileio_read(&mut ctx, f, &mut buf), Ok(5));
    assert_eq!(&buf[..5], b"hello");
    assert_eq!(fileio_read(&mut ctx, f, &mut buf), Ok(0));
    let mut st = SeekState::new(0, SEEK_SET);
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_SEEK, IoctlArg::Seek(&mut st)),
        Ok(0)
    );
    // The backend reports the new absolute offset in
    // `SeekState.result` (the field `stream_seek` reads —
    // same as the StringIO backend); the old assertion on
    // `st.offset` pinned the file backend writing the
    // result into the input field, which made `tell()`
    // return 0 on real files (G5/P3).
    assert_eq!(st.result, 0);
    assert_eq!(fileio_read(&mut ctx, f, &mut buf[..2]), Ok(2));
    assert_eq!(&buf[..2], b"he");
    let mut st = SeekState::new(-1, SEEK_END);
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_SEEK, IoctlArg::Seek(&mut st)),
        Ok(0)
    );
    assert_eq!(st.result, 4);
    assert_eq!(fileio_read(&mut ctx, f, &mut buf[..1]), Ok(1));
    assert_eq!(&buf[..1], b"o");
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    std::fs::remove_file(&path).expect("cleanup");
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn open_binary_roundtrip() {
    let mut fx = Fx::new();
    let path = fx.tmp_path("bin", "f.bin");
    let f = fx.open(&path, "wb");
    let (mut ctx, _state) = fx.vm.split();
    assert_eq!(fileio_write(&mut ctx, f, b"\x00\xff\x01"), Ok(3));
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    let f = fx.open(&path, "rb");
    let (mut ctx, _state) = fx.vm.split();
    let mut buf = [0u8; 8];
    assert_eq!(fileio_read(&mut ctx, f, &mut buf), Ok(3));
    assert_eq!(&buf[..3], b"\x00\xff\x01");
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    std::fs::remove_file(&path).expect("cleanup");
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn open_missing_raises_enoent() {
    let mut fx = Fx::new();
    // The missing file must sit in an EXISTING directory: a
    // path whose parent is also missing reports "path not
    // found" (3) on Windows instead of ENOENT (2).
    let path = std::env::temp_dir().join(format!(
        "petitpy_d2_fileio_{}_nope.txt",
        std::process::id()
    ));
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let p = new_str(&mut ctx, types, path.as_os_str().as_encoded_bytes());
    let m = new_str(&mut ctx, types, b"r");
    let err = open_builtin(&mut ctx, &[p, m]);
    assert_eq!(err, Err(PyError::PyRaise));
    assert_eq!(pending(&ctx), ("OSError".to_string(), Value::SmallInt(2)));
}

#[test]
fn open_bad_mode_raises_einval() {
    let mut fx = Fx::new();
    let path = fx.tmp_path("badmode", "f.txt");
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let p = new_str(&mut ctx, types, path.as_os_str().as_encoded_bytes());
    let m = new_str(&mut ctx, types, b"z");
    let err = open_builtin(&mut ctx, &[p, m]);
    assert_eq!(err, Err(PyError::PyRaise));
    assert_eq!(pending(&ctx), ("OSError".to_string(), Value::SmallInt(22)));
    drop(ctx);
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn open_kwargs_ignored() {
    // `open(p, encoding="utf-8")` works on the reference —
    // the kwargs are discarded (probed).
    let mut fx = Fx::new();
    let path = fx.tmp_path("kwargs", "f.txt");
    let (mut ctx, _state) = fx.vm.split();
    let types = ctx.types;
    let p = new_str(&mut ctx, types, path.as_os_str().as_encoded_bytes());
    let m = new_str(&mut ctx, types, b"w");
    let v = new_str(&mut ctx, types, b"utf-8");
    let f = open_builtin_kw(&mut ctx, &[p, m], &[(0, v)]).expect("kwargs ignored");
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    std::fs::remove_file(&path).expect("cleanup");
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn closed_ops_raise_ebadf() {
    let mut fx = Fx::new();
    let path = fx.tmp_path("closed", "f.txt");
    let f = fx.open(&path, "w");
    let (mut ctx, _state) = fx.vm.split();
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    let mut buf = [0u8; 4];
    assert_eq!(fileio_read(&mut ctx, f, &mut buf), Err(PyError::PyRaise));
    assert_eq!(pending(&ctx), ("OSError".to_string(), Value::SmallInt(9)));
    drop(ctx);
    let (mut ctx, _state) = fx.vm.split();
    let err = fileio_fileno_builtin(&mut ctx, &[f]);
    assert_eq!(err, Err(PyError::PyRaise));
    assert_eq!(pending(&ctx), ("OSError".to_string(), Value::SmallInt(9)));
    drop(ctx);
    let (mut ctx, _state) = fx.vm.split();
    let mut s = String::new();
    fileio_print(&mut ctx, f, PrintKind::Str, &mut s);
    assert_eq!(s, "<io.TextIOWrapper -1>");
    drop(ctx);
    std::fs::remove_file(&path).expect("cleanup");
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn fileno_and_print_forms() {
    let mut fx = Fx::new();
    let path = fx.tmp_path("fileno", "f.txt");
    let f = fx.open(&path, "w");
    let (mut ctx, _state) = fx.vm.split();
    let fd = fileio_fileno_builtin(&mut ctx, &[f]).expect("fileno");
    match fd {
        Value::SmallInt(n) => assert!(n >= 3, "a real fd: {n}"),
        other => panic!("fileno is not an int: {other:?}"),
    }
    let mut s = String::new();
    fileio_print(&mut ctx, f, PrintKind::Str, &mut s);
    assert!(s.starts_with("<io.TextIOWrapper "), "{s}");
    assert!(s.ends_with('>'), "{s}");
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    let f = fx.open(&path, "wb");
    let (mut ctx, _state) = fx.vm.split();
    let mut s = String::new();
    fileio_print(&mut ctx, f, PrintKind::Str, &mut s);
    assert!(s.starts_with("<io.FileIO "), "{s}");
    assert_eq!(
        fileio_ioctl(&mut ctx, f, MP_STREAM_CLOSE, IoctlArg::None),
        Ok(0)
    );
    drop(ctx);
    std::fs::remove_file(&path).expect("cleanup");
    std::fs::remove_dir(path.parent().unwrap()).expect("cleanup");
}

#[test]
fn stdio_objects_shapes() {
    let mut fx = Fx::new();
    let (mut ctx, _state) = fx.vm.split();
    let (fileio, textio) = (ctx.types.fileio, ctx.types.textio);
    let (stdin, stdout, stderr) = build_stdio_objects(&mut ctx, fileio, textio);
    for (obj, fd) in [(stdin, 0), (stdout, 1), (stderr, 2)] {
        let got = fileio_fileno_builtin(&mut ctx, &[obj]).expect("fileno");
        assert_eq!(got, Value::SmallInt(fd));
        let mut s = String::new();
        fileio_print(&mut ctx, obj, PrintKind::Str, &mut s);
        assert_eq!(s, format!("<io.TextIOWrapper {fd}>"));
    }
    // `.buffer` exists on the stdio text objects (the FileIO
    // sibling, same fd) and is the identical object on repeat
    // access; a plain opened file has no `.buffer`.
    let buffer_id = ctx.pool.from_strn(b"buffer").expect("intern buffer") as u32;
    let buf = textio_attr(&mut ctx, stdin, buffer_id)
        .expect("attr ok")
        .expect("stdin has .buffer");
    let got = fileio_fileno_builtin(&mut ctx, &[buf]).expect("buffer fileno");
    assert_eq!(got, Value::SmallInt(0));
    let buf2 = textio_attr(&mut ctx, stdin, buffer_id)
        .expect("attr ok")
        .expect("stdin has .buffer");
    assert_eq!(buf, buf2);
    let mut s = String::new();
    fileio_print(&mut ctx, buf, PrintKind::Str, &mut s);
    assert_eq!(s, "<io.FileIO 0>");
    drop(ctx);
}
