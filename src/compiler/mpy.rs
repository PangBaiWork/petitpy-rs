//! .mpy file format — Rust serializer/deserializer.
//!
//! Port of `src/compiler/mpy.zig` (387 lines). Format reference:
//! vendor `py/persistentcode.{c,h}`. Tracks MPY_VERSION = 6 (current
//! vendor).
//!
//! File layout:
//!   header (4 bytes):
//!     byte 0: 'M' (0x4d) magic
//!     byte 1: MPY_VERSION (6)
//!     byte 2: native arch + sub-version flags (0 = no native)
//!     byte 3: MP_SMALL_INT_BITS (e.g. 31 = 0x1f on 32-bit small int)
//!   if arch_flags != 0: vlen arch_flags
//!   vlen n_qstr
//!   vlen n_obj
//!   for each qstr:
//!     static: vlen (qstr_id << 1) | 1
//!     new:    vlen (len << 1), bytes str[0..len+1] (incl NUL)
//!   for each obj: tagged encoding (see `ObjKind` + `read_obj`/`write_obj`)
//!   raw_code (recursive):
//!     vlen (fun_data_len << 3) | (has_children << 2) | (kind - MP_CODE_BYTECODE)
//!     bytes fun_data[0..fun_data_len]
//!     if has_children: vlen n_children, then each child raw_code
//!
//! Divergences from the Zig original (all deliberate, documented):
//!   1. Owned data: Zig returns allocator-owned slices (`[]const u8`
//!      duped via `gpa.dupe`); Rust stores `Vec<u8>` / `Vec<ObjEntry>` /
//!      `Vec<RawCode>`. No `deinit` is needed — `Drop` frees everything.
//!      Ids, bytes, lengths and structure are identical.
//!   2. `RawCode.kind` is `u8`, not Zig's `u2` (Rust has no 2-bit int).
//!      Only the values `MP_CODE_BYTECODE` (2) and `MP_CODE_NATIVE_PY`
//!      (3) occur in valid files; the reader reproduces Zig's
//!      `@intCast` truncation for out-of-range kind offsets, see
//!      `read_raw_code`.
//!   3. `write` is infallible (`Vec<u8>`) where Zig returns
//!      `Allocator.Error![]u8`: Rust has no fallible-allocator API in
//!      the std `Vec` push path (allocation failure aborts), so the
//!      Zig error arm has no Rust spelling. The reader keeps the
//!      fallible shape (`Result<_, MpyError>`, with `OutOfMemory`
//!      produced via `try_reserve`, exactly as `qstr.rs` does).
//!   4. `QstrEntry` lives here (it is `mpy.QstrEntry` in Zig).
//!      `qstr.rs` re-exports it and its `QstrContext::to_entry`
//!      returns the owned form; the Zig `qstr.zig` conversion returns
//!      borrowed slices, but the emitter only needs the bytes, and one
//!      owned type avoids two same-named types at the merge.

use std::fmt;

/// Current vendor .mpy version (vendor persistentcode.h).
pub const MPY_VERSION: u8 = 6;

/// Magic byte: 'M' (0x4d).
pub const MPY_MAGIC: u8 = b'M';

/// Zero-based index of MP_QSTR_zip among QDEF0 entries (vendor
/// persistentcode.c:48 `#define QSTR_LAST_STATIC MP_QSTR_zip`).
/// Kept in sync with `super::qstr::QSTR_LAST_STATIC` — the test
/// `qstr_last_static_matches_qstr_module` asserts they agree, mirroring
/// the Zig `comptime` assert in `qstr.zig`.
pub const QSTR_LAST_STATIC: u16 = 165;

/// Raw-code kind tags. Stored as a u2 in Zig; vendor encodes
/// (kind - BYTECODE) into the .mpy header so BYTECODE itself is the
/// implicit zero offset.
pub const MP_CODE_BYTECODE: u8 = 2;
pub const MP_CODE_NATIVE_PY: u8 = 3;

/// Errors the .mpy reader can return. Mirrors Zig `mpy.Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpyError {
    /// First byte is not `MPY_MAGIC`.
    InvalidMagic,
    /// Second byte is not `MPY_VERSION`.
    InvalidVersion,
    /// Ran past the end of the input while reading.
    UnexpectedEof,
    /// Unknown object tag (e.g. `fun_table`) or otherwise unsupported.
    UnsupportedFeature,
    /// An allocation failed. Produced via `try_reserve` (see `qstr.rs`
    /// for the same treatment); never produced for well-formed input
    /// of sane size.
    OutOfMemory,
}

impl fmt::Display for MpyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MpyError::InvalidMagic => write!(f, "invalid .mpy magic"),
            MpyError::InvalidVersion => write!(f, "unsupported .mpy version"),
            MpyError::UnexpectedEof => write!(f, "unexpected end of .mpy data"),
            MpyError::UnsupportedFeature => write!(f, "unsupported .mpy feature"),
            MpyError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for MpyError {}

/// `Result<T, MpyError>` — the Rust spelling of Zig's `mpy.Error!T`.
pub type MpyResult<T> = Result<T, MpyError>;

/// Object kinds in the constants table. Mirrors Zig `ObjKind` (`enum(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjKind {
    FunTable = 0,
    None = 1,
    False = 2,
    True = 3,
    Ellipsis = 4,
    Str = 5,
    Bytes = 6,
    Int = 7,
    Float = 8,
    Complex = 9,
    Tuple = 10,
}

/// A qstr entry in the .mpy file. Either a reference to a static qstr (by id)
/// or a fresh literal string with bytes carried inline.
/// Mirrors Zig `mpy.QstrEntry` (owned form — see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QstrEntry {
    /// id <= QSTR_LAST_STATIC.
    Static(u16),
    /// Bytes (without NUL terminator).
    New(Vec<u8>),
}

/// A constant-table object entry. Mirrors Zig `mpy.ObjEntry` (owned form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjEntry {
    None,
    False,
    True,
    Ellipsis,
    /// Text (without NUL).
    Str(Vec<u8>),
    /// Raw bytes (without NUL).
    Bytes(Vec<u8>),
    /// Text representation (vendor uses "TODO" — text-based).
    Int(Vec<u8>),
    /// Text representation.
    Float(Vec<u8>),
    Complex(Vec<u8>),
    Tuple(Vec<ObjEntry>),
}

/// A raw code blob — bytecode (or native code) for one function/scope.
/// Mirrors Zig `mpy.RawCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCode {
    /// MP_CODE_BYTECODE=2, MP_CODE_NATIVE_PY=3, etc.
    pub kind: u8,
    /// Bytecode + prelude.
    pub fun_data: Vec<u8>,
    /// Nested functions (often empty).
    pub children: Vec<RawCode>,
}

/// A complete loaded .mpy file. Mirrors Zig `mpy.Module`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub version: u8,
    pub feature_flags: u8,
    pub small_int_bits: u8,
    /// Present iff (feature_flags & ARCH_FLAGS_BIT) — set during
    /// reading if needed. (Vendor: MPY_FEATURE_ARCH_FLAGS = 1 << 6.)
    pub arch_flags: Option<u64>,
    pub qstrs: Vec<QstrEntry>,
    pub objs: Vec<ObjEntry>,
    pub rc: RawCode,
}

/// Duplicate bytes, reporting allocation failure instead of aborting
/// (mirrors Zig's `gpa.dupe`, whose failure surfaces as `OutOfMemory`).
fn dupe_bytes(b: &[u8]) -> MpyResult<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve(b.len()).map_err(|_| MpyError::OutOfMemory)?;
    v.extend_from_slice(b);
    Ok(v)
}

/// Reserve `n` slots in a fresh `Vec`, reporting allocation failure
/// instead of aborting (mirrors Zig's `gpa.alloc` failure path).
fn alloc_vec<T>(n: usize) -> MpyResult<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve(n).map_err(|_| MpyError::OutOfMemory)?;
    Ok(v)
}

// ──────────────────────────────────────────────────────────────────────
// Reader

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn read_byte(&mut self) -> MpyResult<u8> {
        if self.pos >= self.data.len() {
            return Err(MpyError::UnexpectedEof);
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_bytes(&mut self, n: usize) -> MpyResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(MpyError::UnexpectedEof)?;
        if end > self.data.len() {
            return Err(MpyError::UnexpectedEof);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Variable-length unsigned int: 7 bits per byte, high bit =
    /// continuation. Big-endian (most significant chunks first).
    fn read_uint(&mut self) -> MpyResult<u64> {
        let mut n: u64 = 0;
        loop {
            let b = self.read_byte()?;
            n = (n << 7) | (u64::from(b & 0x7f));
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(n)
    }
}

/// Read a whole .mpy file. Mirrors Zig `mpy.read`.
pub fn read(data: &[u8]) -> MpyResult<Module> {
    let mut r = Reader { data, pos: 0 };

    // Header
    let magic = r.read_byte()?;
    if magic != MPY_MAGIC {
        return Err(MpyError::InvalidMagic);
    }
    let version = r.read_byte()?;
    if version != MPY_VERSION {
        return Err(MpyError::InvalidVersion);
    }
    let feature_flags = r.read_byte()?;
    let small_int_bits = r.read_byte()?;

    let mut arch_flags: Option<u64> = None;
    // Bit 0 of feature_flags = sub_version (encoded with arch); bit 7 =
    // ARCH_FLAGS. We don't decode the meaning here — just preserve for
    // round-trip. The exact condition for the optional `arch_flags`
    // field is: bit 6 of feature_flags.
    // (vendor: MPY_FEATURE_ARCH_FLAGS = 1 << 6; tested with
    // cm->arch_flags != 0)
    if feature_flags & 0x40 != 0 {
        arch_flags = Some(r.read_uint()?);
    }

    let n_qstr = r.read_uint()?;
    let n_obj = r.read_uint()?;

    // Qstrs
    let mut qstrs = alloc_vec::<QstrEntry>(n_qstr as usize)?;
    for _ in 0..n_qstr {
        let v = r.read_uint()?;
        if v & 1 != 0 {
            // Static qstr
            qstrs.push(QstrEntry::Static((v >> 1) as u16));
        } else {
            let len = (v >> 1) as usize;
            // bytes are len + 1 (incl trailing NUL); we copy just the len bytes
            let raw = r.read_bytes(len.checked_add(1).ok_or(MpyError::UnexpectedEof)?)?;
            qstrs.push(QstrEntry::New(dupe_bytes(&raw[..len])?));
        }
    }

    // Objects
    let mut objs = alloc_vec::<ObjEntry>(n_obj as usize)?;
    for _ in 0..n_obj {
        objs.push(read_obj(&mut r)?);
    }

    // Outer raw code
    let rc = read_raw_code(&mut r)?;

    Ok(Module {
        version,
        feature_flags,
        small_int_bits,
        arch_flags,
        qstrs,
        objs,
        rc,
    })
}

fn read_obj(r: &mut Reader<'_>) -> MpyResult<ObjEntry> {
    let tag = r.read_byte()?;
    match tag {
        x if x == ObjKind::None as u8 => Ok(ObjEntry::None),
        x if x == ObjKind::False as u8 => Ok(ObjEntry::False),
        x if x == ObjKind::True as u8 => Ok(ObjEntry::True),
        x if x == ObjKind::Ellipsis as u8 => Ok(ObjEntry::Ellipsis),
        x if x == ObjKind::Str as u8 || x == ObjKind::Bytes as u8 => {
            let len = r.read_uint()?;
            let raw = r.read_bytes(
                (len as usize)
                    .checked_add(1)
                    .ok_or(MpyError::UnexpectedEof)?,
            )?; // incl NUL
            let owned = dupe_bytes(&raw[..len as usize])?;
            if tag == ObjKind::Str as u8 {
                Ok(ObjEntry::Str(owned))
            } else {
                Ok(ObjEntry::Bytes(owned))
            }
        }
        x if x == ObjKind::Int as u8
            || x == ObjKind::Float as u8
            || x == ObjKind::Complex as u8 =>
        {
            let len = r.read_uint()?;
            let raw = r.read_bytes(len as usize)?;
            let owned = dupe_bytes(raw)?;
            if tag == ObjKind::Int as u8 {
                Ok(ObjEntry::Int(owned))
            } else if tag == ObjKind::Float as u8 {
                Ok(ObjEntry::Float(owned))
            } else {
                Ok(ObjEntry::Complex(owned))
            }
        }
        x if x == ObjKind::Tuple as u8 => {
            let len = r.read_uint()?;
            let mut items = alloc_vec::<ObjEntry>(len as usize)?;
            for _ in 0..len {
                items.push(read_obj(r)?);
            }
            Ok(ObjEntry::Tuple(items))
        }
        _ => Err(MpyError::UnsupportedFeature), // fun_table or unknown
    }
}

fn read_raw_code(r: &mut Reader<'_>) -> MpyResult<RawCode> {
    let hdr = r.read_uint()?;
    // Disk encodes (kind - MP_CODE_BYTECODE) in the low 2 bits; lift it
    // back to the absolute kind so RawCode.kind matches its docstring.
    let kind_offset: u8 = (hdr & 0x3) as u8;
    let has_children = hdr & 0x4 != 0;
    let fun_data_len = (hdr >> 3) as usize;

    let fun_data_raw = r.read_bytes(fun_data_len)?;
    let fun_data = dupe_bytes(fun_data_raw)?;

    let mut children: Vec<RawCode> = Vec::new();
    if has_children {
        let n = r.read_uint()?;
        children = alloc_vec::<RawCode>(n as usize)?;
        for _ in 0..n {
            children.push(read_raw_code(r)?);
        }
    }

    Ok(RawCode {
        // Zig: `@intCast(@as(u8, kind_offset) + MP_CODE_BYTECODE)` — the
        // u8 sum is truncated back to u2, i.e. (kind_offset + 2) mod 4.
        // Valid files only carry offsets 0 (BYTECODE) and 1 (NATIVE_PY);
        // the mask reproduces Zig's truncation for out-of-range offsets.
        kind: kind_offset.wrapping_add(MP_CODE_BYTECODE) & 0x3,
        fun_data,
        children,
    })
}

// ──────────────────────────────────────────────────────────────────────
// Writer

/// Write a module back to .mpy bytes. Mirrors Zig `mpy.write` (whose
/// `Allocator.Error` arm has no Rust spelling — see the module docs).
pub fn write(m: &Module) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();

    // Header
    buf.extend_from_slice(&[MPY_MAGIC, m.version, m.feature_flags, m.small_int_bits]);

    if let Some(af) = m.arch_flags {
        write_uint(&mut buf, af);
    }

    write_uint(&mut buf, m.qstrs.len() as u64);
    write_uint(&mut buf, m.objs.len() as u64);

    for q in &m.qstrs {
        match q {
            QstrEntry::Static(id) => write_uint(&mut buf, (u64::from(*id) << 1) | 1),
            QstrEntry::New(bytes) => {
                write_uint(&mut buf, (bytes.len() as u64) << 1);
                buf.extend_from_slice(bytes);
                buf.push(0); // NUL terminator
            }
        }
    }

    for o in &m.objs {
        write_obj(&mut buf, o);
    }

    write_raw_code(&mut buf, &m.rc);

    buf
}

fn write_uint(buf: &mut Vec<u8>, n_in: u64) {
    // Big-endian 7-bit chunks, high bit = continuation.
    let mut tmp: [u8; 10] = [0; 10];
    let mut p: usize = tmp.len();
    p -= 1;
    tmp[p] = (n_in & 0x7f) as u8;
    let mut n = n_in >> 7;
    while n != 0 {
        p -= 1;
        tmp[p] = ((n & 0x7f) as u8) | 0x80;
        n >>= 7;
    }
    buf.extend_from_slice(&tmp[p..]);
}

fn write_obj(buf: &mut Vec<u8>, o: &ObjEntry) {
    match o {
        ObjEntry::None => buf.push(ObjKind::None as u8),
        ObjEntry::False => buf.push(ObjKind::False as u8),
        ObjEntry::True => buf.push(ObjKind::True as u8),
        ObjEntry::Ellipsis => buf.push(ObjKind::Ellipsis as u8),
        ObjEntry::Str(s) => {
            buf.push(ObjKind::Str as u8);
            write_uint(buf, s.len() as u64);
            buf.extend_from_slice(s);
            buf.push(0);
        }
        ObjEntry::Bytes(b) => {
            buf.push(ObjKind::Bytes as u8);
            write_uint(buf, b.len() as u64);
            buf.extend_from_slice(b);
            buf.push(0);
        }
        ObjEntry::Int(s) => {
            buf.push(ObjKind::Int as u8);
            write_uint(buf, s.len() as u64);
            buf.extend_from_slice(s);
        }
        ObjEntry::Float(s) => {
            buf.push(ObjKind::Float as u8);
            write_uint(buf, s.len() as u64);
            buf.extend_from_slice(s);
        }
        ObjEntry::Complex(s) => {
            buf.push(ObjKind::Complex as u8);
            write_uint(buf, s.len() as u64);
            buf.extend_from_slice(s);
        }
        ObjEntry::Tuple(t) => {
            buf.push(ObjKind::Tuple as u8);
            write_uint(buf, t.len() as u64);
            for it in t {
                write_obj(buf, it);
            }
        }
    }
}

fn write_raw_code(buf: &mut Vec<u8>, rc: &RawCode) {
    let has_children = !rc.children.is_empty();
    // RawCode.kind stores the absolute MP_CODE_* value; encode the
    // offset (kind - MP_CODE_BYTECODE) into the low 2 bits. Zig does
    // u2 subtraction: only the low 2 bits of the result reach the
    // header, so `wrapping_sub` reproduces it exactly; the debug_assert
    // mirrors Zig's Debug-mode panic on kind < MP_CODE_BYTECODE.
    debug_assert!(
        rc.kind >= MP_CODE_BYTECODE,
        "raw code kind {} below MP_CODE_BYTECODE",
        rc.kind
    );
    let kind_offset: u8 = rc.kind.wrapping_sub(MP_CODE_BYTECODE);
    let hdr: u64 = ((rc.fun_data.len() as u64) << 3)
        | ((u64::from(has_children)) << 2)
        | u64::from(kind_offset);
    write_uint(buf, hdr);
    buf.extend_from_slice(&rc.fun_data);
    if has_children {
        write_uint(buf, rc.children.len() as u64);
        for c in &rc.children {
            write_raw_code(buf, c);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::compiler::qstr;

    /// Locate the original's frozen .mpy fixtures. Same rule as the
    /// qstr tests use for the vendored header: `PETITPY_ORIG` wins,
    /// otherwise `../petitpy-orig` next to the worktree, otherwise
    /// `$HOME/workspace/petitpy-orig`. Last resort is the 23 frozen
    /// .mpy files committed in this repo (`frozen/`), byte-identical
    /// to the original's build-frozen output (see
    /// scripts/check_frozen.py) — that makes the tests
    /// self-contained, so they run on any checkout (CI included)
    /// without a sibling petitpy-orig. Panics with a clear message
    /// if none exists — a missing fixture dir is a setup failure,
    /// not a passing test.
    pub(crate) fn fixture_dir() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("PETITPY_ORIG") {
            let d = std::path::PathBuf::from(p).join("build-frozen");
            if d.is_dir() {
                return d;
            }
        }
        let rel = std::path::PathBuf::from("../petitpy-orig/build-frozen");
        if rel.is_dir() {
            return rel;
        }
        if let Ok(home) = std::env::var("HOME") {
            let d = std::path::PathBuf::from(home)
                .join("workspace/petitpy-orig/build-frozen");
            if d.is_dir() {
                return d;
            }
        }
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frozen");
        if d.is_dir() {
            return d;
        }
        panic!("cannot locate petitpy-orig/build-frozen fixtures (set PETITPY_ORIG)");
    }

    /// Sorted list of the 23 frozen fixture paths.
    pub(crate) fn frozen_files() -> Vec<std::path::PathBuf> {
        let dir = fixture_dir();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read fixture dir {}: {}", dir.display(), e))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "mpy").unwrap_or(false))
            .collect();
        files.sort();
        assert_eq!(files.len(), 23, "expected 23 frozen .mpy fixtures");
        files
    }

    #[test]
    fn qstr_last_static_matches_qstr_module() {
        // Mirrors the Zig comptime assert in qstr.zig:
        // `if (mpy.QSTR_LAST_STATIC != QSTR_LAST_STATIC)` — the two
        // qstr tables (compiler-side and .mpy) must agree on the cutoff.
        assert_eq!(QSTR_LAST_STATIC, qstr::QSTR_LAST_STATIC);
        assert_eq!(QSTR_LAST_STATIC, 165);
    }

    #[test]
    fn varint_round_trip_vectors() {
        // read_uint/write_uint must be exact inverses, incl. the
        // multi-byte boundaries (0x7f/0x80) and large values.
        for n in [
            0u64,
            1,
            0x7f,
            0x80,
            0x3fff,
            0x4000,
            0x1f_ffff,
            0x20_0000,
            0xffff_ffff,
            0xffff_ffff_ffff_ffff,
        ] {
            let mut buf = Vec::new();
            write_uint(&mut buf, n);
            let mut r = Reader {
                data: &buf,
                pos: 0,
            };
            assert_eq!(r.read_uint().unwrap(), n, "varint round-trip for {n}");
            assert_eq!(r.pos, buf.len());
        }
        // Known encodings (big-endian 7-bit chunks, high bit set on all
        // but the last byte): 0 -> [0x00], 300 -> [0x82, 0x2c].
        let mut buf = Vec::new();
        write_uint(&mut buf, 300);
        assert_eq!(buf, vec![0x82, 0x2c]);
    }

    #[test]
    fn read_rejects_bad_magic() {
        let mut data = vec![0x58, MPY_VERSION, 0, 31]; // 'X' instead of 'M'
        assert_eq!(read(&data), Err(MpyError::InvalidMagic));
        data[0] = MPY_MAGIC;
        data[1] = 5; // wrong version
        assert_eq!(read(&data), Err(MpyError::InvalidVersion));
    }

    #[test]
    fn read_rejects_truncated_inputs() {
        // Empty, header-only, and mid-qstr truncation all hit EOF.
        assert_eq!(read(&[]), Err(MpyError::UnexpectedEof));
        assert_eq!(read(&[MPY_MAGIC, MPY_VERSION, 0]), Err(MpyError::UnexpectedEof));
        // Header claims 1 qstr but no qstr bytes follow.
        assert_eq!(
            read(&[MPY_MAGIC, MPY_VERSION, 0, 31, 1, 0]),
            Err(MpyError::UnexpectedEof)
        );
        // Qstr says .new len 3 but only 1 byte follows.
        assert_eq!(
            read(&[MPY_MAGIC, MPY_VERSION, 0, 31, 1, 0, 6, b'a']),
            Err(MpyError::UnexpectedEof)
        );
    }

    #[test]
    fn read_rejects_fun_table_tag() {
        // ObjKind::fun_table (tag 0) has no ObjEntry variant; the Zig
        // reader returns UnsupportedFeature for it (and any unknown tag).
        let data = [
            MPY_MAGIC, MPY_VERSION, 0, 31, // header
            0, 1, // n_qstr=0, n_obj=1
            0, // tag 0 = fun_table
        ];
        assert_eq!(read(&data), Err(MpyError::UnsupportedFeature));
        let mut data2 = data.to_vec();
        data2[6] = 99; // unknown tag
        assert_eq!(read(&data2), Err(MpyError::UnsupportedFeature));
    }

    #[test]
    fn minimal_module_round_trips() {
        // Hand-built smallest valid module: header + 0 qstrs + 0 objs +
        // raw code with empty fun_data and no children.
        // raw-code hdr: (fun_data_len 0 << 3) | (has_children 0 << 2) |
        // kind offset 0 -> kind MP_CODE_BYTECODE.
        let data = [MPY_MAGIC, MPY_VERSION, 0, 31, 0, 0, 0];
        let m = read(&data).unwrap();
        assert_eq!(m.version, MPY_VERSION);
        assert_eq!(m.feature_flags, 0);
        assert_eq!(m.small_int_bits, 31);
        assert_eq!(m.arch_flags, None);
        assert!(m.qstrs.is_empty() && m.objs.is_empty());
        assert_eq!(m.rc.kind, MP_CODE_BYTECODE);
        assert!(m.rc.fun_data.is_empty() && m.rc.children.is_empty());
        assert_eq!(write(&m), data.to_vec());
    }

    #[test]
    fn arch_flags_round_trip() {
        // feature_flags bit 6 set -> an arch_flags vlen follows the header.
        // Raw-code hdr 0x00: empty fun_data, no children, kind BYTECODE.
        let mut data = vec![MPY_MAGIC, MPY_VERSION, 0x40, 31, 0x2c, 0, 0, 0x00];
        let m = read(&data).unwrap();
        assert_eq!(m.arch_flags, Some(0x2c));
        assert_eq!(write(&m), data);
        // ...but the writer only emits arch_flags when the Module says so:
        // a Module with arch_flags set and bit 6 clear in feature_flags
        // still writes the field (same as Zig — the writer keys off the
        // Option, not the flag bits).
        let mut m2 = m.clone();
        m2.feature_flags = 0;
        let out = write(&m2);
        data[2] = 0;
        assert_eq!(out, data);
    }

    #[test]
    fn all_obj_kinds_round_trip() {
        // One module carrying every ObjEntry variant, incl. a nested tuple.
        let m = Module {
            version: MPY_VERSION,
            feature_flags: 0,
            small_int_bits: 31,
            arch_flags: None,
            qstrs: vec![
                QstrEntry::Static(0),
                QstrEntry::Static(QSTR_LAST_STATIC),
                QstrEntry::New(b"hello".to_vec()),
                QstrEntry::New(b"".to_vec()),
            ],
            objs: vec![
                ObjEntry::None,
                ObjEntry::False,
                ObjEntry::True,
                ObjEntry::Ellipsis,
                ObjEntry::Str(b"a\x00b".to_vec()),
                ObjEntry::Bytes(b"\x00\xff".to_vec()),
                ObjEntry::Int(b"12345678901234567890".to_vec()),
                ObjEntry::Float(b"1.5e300".to_vec()),
                ObjEntry::Complex(b"1+2j".to_vec()),
                ObjEntry::Tuple(vec![
                    ObjEntry::Int(b"7".to_vec()),
                    ObjEntry::Tuple(vec![]),
                    ObjEntry::Str(b"nested".to_vec()),
                ]),
            ],
            rc: RawCode {
                kind: MP_CODE_NATIVE_PY,
                fun_data: vec![1, 2, 3],
                children: vec![RawCode {
                    kind: MP_CODE_BYTECODE,
                    fun_data: vec![9],
                    children: vec![],
                }],
            },
        };
        let bytes = write(&m);
        let m2 = read(&bytes).unwrap();
        assert_eq!(m, m2);
        assert_eq!(write(&m2), bytes);
    }

    #[test]
    fn frozen_files_round_trip_byte_identical() {
        // The core acceptance: Rust read -> Rust write reproduces the
        // original's bytes for all 23 frozen modules.
        let mut total = 0usize;
        for f in frozen_files() {
            let data = std::fs::read(&f).unwrap();
            let m = read(&data)
                .unwrap_or_else(|e| panic!("Rust read failed on {}: {:?}", f.display(), e));
            let out = write(&m);
            assert_eq!(
                out, data,
                "round-trip mismatch on {} (in={} out={})",
                f.display(),
                data.len(),
                out.len()
            );
            // And the parsed form is stable: a second read gives the same Module.
            assert_eq!(read(&out).unwrap(), m);
            total += data.len();
        }
        assert!(total > 0);
    }

    #[test]
    fn frozen_files_parse_shape() {
        // Every frozen file parses to the expected header and a
        // well-formed tree (no empty fun_data at the top, qstr ids in
        // range, obj tags all known).
        for f in frozen_files() {
            let data = std::fs::read(&f).unwrap();
            let m = read(&data).unwrap();
            assert_eq!(m.version, MPY_VERSION);
            assert_eq!(m.feature_flags, 0);
            assert_eq!(m.small_int_bits, 31);
            assert_eq!(m.arch_flags, None);
            assert_eq!(m.rc.kind, MP_CODE_BYTECODE);
            assert!(!m.rc.fun_data.is_empty(), "empty top fun_data in {}", f.display());
            for q in &m.qstrs {
                if let QstrEntry::Static(id) = q {
                    assert!(*id <= QSTR_LAST_STATIC, "static id {id} above cutoff");
                }
            }
        }
    }
}
