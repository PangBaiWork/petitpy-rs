//! `vstr` — variable-length string builder (Lane C8b).
//!
//! Port of `src/objects/strings/vstr.zig` (278 lines, 0 test
//! blocks), itself a full Zig port of vendor `py/vstr.c`.
//! `vstr_t` layout (vendor `py/misc.h`): `{ alloc, len, buf,
//! fixed_buf }` — 32 bytes on 64-bit.
//!
//! Rust shape: `alloc`/`len` are tracked explicitly and `buf` is a
//! `Vec<u8>` whose length is the allocation (spare bytes are zero,
//! as in C); `len` counts the used bytes. Growth follows the Zig
//! exactly, including the `roundAlloc` quirk (the mask is a 32-bit
//! `~0U`, zero-extended — bits above 31 are cleared, so a huge
//! `len + size` can SHRINK the allocation; preserved verbatim).
//!
//! `vstr_printf` / `vstr_vprintf` live in `va_ops.c` in the
//! original (va_list code that cannot stay in Zig on
//! aarch64-Linux) — they are NOT part of this port (C18 owns
//! `va_ops.c`).

use crate::objects::strings::objstr_a::StrError;
use std::fmt;

/// `vstr_t` (vendor `py/misc.h`): `{ alloc, len, buf, fixed_buf }`.
#[derive(Clone, Debug)]
pub struct Vstr {
    /// Logical allocation in bytes (the C `alloc` field).
    pub alloc: usize,
    /// Used bytes (the C `len` field).
    pub len: usize,
    /// The buffer; `buf.len() == alloc` except right after `clear`
    /// (the C frees the buffer and leaves `alloc` stale — see
    /// `clear`). Bytes past `len` are zero.
    buf: Vec<u8>,
    /// `true` after `init_fixed_buf`: the buffer is caller-owned
    /// and must never be reallocated or freed (extending raises
    /// bare `RuntimeError`).
    pub fixed_buf: bool,
}

/// `ROUND_ALLOC(a) = (a & ((~0U) - 7)) + 8` — `~0U` is `unsigned
/// int` (32-bit), so the mask zero-extends to `0x00000000FFFFFFF8`
/// (bits >31 are cleared too — a C quirk we preserve exactly).
/// (`vstr.zig`, `roundAlloc`.)
pub fn round_alloc(a: usize) -> usize {
    (a & 0x0000_0000_FFFF_FFF8usize).wrapping_add(8)
}

impl Vstr {
    /// `vstr_init` (`vstr.zig`): `alloc` at least 1, fresh heap
    /// buffer, `len` 0, not fixed.
    pub fn init(alloc_in: usize) -> Vstr {
        let alloc = alloc_in.max(1);
        Vstr {
            alloc,
            len: 0,
            buf: vec![0u8; alloc],
            fixed_buf: false,
        }
    }

    /// `vstr_init_len`: `init(len + 1)` then `len = len`.
    pub fn init_len(len: usize) -> Vstr {
        let mut v = Vstr::init(len.wrapping_add(1));
        v.len = len;
        v
    }

    /// `vstr_init_fixed_buf`: caller-owned buffer, `len` 0, fixed.
    /// The buffer is taken by value; its length is the allocation.
    pub fn init_fixed_buf(buf: Vec<u8>) -> Vstr {
        let alloc = buf.len();
        Vstr {
            alloc,
            len: 0,
            buf,
            fixed_buf: true,
        }
    }

    /// `vstr_new(alloc)`: heap `vstr_t` + `init` (the Zig mallocs
    /// the struct itself; Rust returns the value).
    pub fn new(alloc: usize) -> Vstr {
        Vstr::init(alloc)
    }

    /// The used bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len.min(self.buf.len())]
    }

    /// Mutable access to the whole buffer — for `init_len`-built
    /// vstrs whose `len` already covers the allocation (`str_center`
    /// / `strPadOneSide` fill the buffer, then hand the vstr to
    /// `new_str_type_from_vstr`).
    pub fn as_bytes_mut_for_init(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    /// `vstr_clear`: frees the buffer (fixed buffers are NOT freed
    /// in C — but the pointer is still nulled). `alloc`/`len` are
    /// left untouched, exactly as in C; a cleared `Vstr` must be
    /// re-initialised before reuse (all Zig callers do so — the
    /// `new_str_type_from_vstr` qstr-hit path resets `alloc` to 0
    /// right after clearing).
    pub fn clear(&mut self) {
        self.buf = Vec::new();
    }

    /// `vstr_free`: `clear` + drop the struct (Rust drops the
    /// buffer on `Drop`; the explicit `clear` mirrors the C's
    /// `m_free(buf)` for fixed_buf symmetry — a fixed buffer is
    /// still owned by this `Vstr` in Rust, so it is dropped here).
    pub fn free(self) {
        // Dropping `self` frees the buffer; `clear` first mirrors
        // the C order (free buf, then free the struct).
        let mut v = self;
        v.clear();
    }

    /// `vstrExtendImpl` / `vstr_extend`: grow the allocation by
    /// `size` bytes exactly (`alloc += size`) and return the start
    /// index of the new space (the old `alloc`). Fixed buffer →
    /// bare `RuntimeError`.
    pub fn extend(&mut self, size: usize) -> Result<usize, StrError> {
        if self.fixed_buf {
            return Err(StrError::RuntimeError);
        }
        let old_alloc = self.alloc;
        self.alloc = self.alloc.wrapping_add(size);
        self.buf.resize(self.alloc, 0);
        Ok(old_alloc)
    }

    /// `vstrEnsureExtraImpl`: ensure room for `size` more bytes;
    /// grows to `roundAlloc(len + size + 16)` when needed. Fixed
    /// buffer → bare `RuntimeError`. (The defensive `buf.len()`
    /// check covers a `clear`ed buffer whose stale `alloc` would
    /// otherwise skip the realloc — a state the Zig never writes
    /// into; see `clear`.)
    pub fn ensure_extra(&mut self, size: usize) -> Result<(), StrError> {
        if self.len.wrapping_add(size) > self.alloc || self.buf.len() < self.len.wrapping_add(size) {
            if self.fixed_buf {
                return Err(StrError::RuntimeError);
            }
            let new_alloc = round_alloc(self.len.wrapping_add(size).wrapping_add(16));
            self.alloc = new_alloc;
            self.buf.resize(new_alloc, 0);
        }
        Ok(())
    }

    /// `vstr_hint_size`: `ensure_extra(size)` (raises are bridged
    /// away in the Zig export — the Rust caller sees the error).
    pub fn hint_size(&mut self, size: usize) -> Result<(), StrError> {
        self.ensure_extra(size)
    }

    /// `vstrAddLenImpl` / `vstr_add_len`: reserve `len` bytes and
    /// return their start index.
    pub fn add_len(&mut self, len: usize) -> Result<usize, StrError> {
        self.ensure_extra(len)?;
        let start = self.len;
        self.len = self.len.wrapping_add(len);
        Ok(start)
    }

    /// `vstr_null_terminated_str`: ensure a spare byte, NUL it, and
    /// return the bytes INCLUDING the terminator (the C returns
    /// `buf`, a NUL-terminated `char *`).
    pub fn null_terminated_str(&mut self) -> Result<&[u8], StrError> {
        if self.alloc == self.len {
            self.extend(1)?;
        }
        // `buf.len() == alloc > len` here (the `ensure_extra`
        // defensive path also covers a `clear`ed buffer); the byte
        // at `len` is zero (fresh spare, or the resize fill).
        if self.buf.len() > self.len {
            self.buf[self.len] = 0;
            Ok(&self.buf[..=self.len])
        } else {
            self.ensure_extra(1)?;
            self.buf[self.len] = 0;
            Ok(&self.buf[..=self.len])
        }
    }

    /// The NUL-terminated bytes as a fresh `Vec` (convenience for
    /// the format-spec parser, which wants a `0` sentinel past the
    /// last real byte).
    pub fn nul_terminated_vec(&mut self) -> Result<Vec<u8>, StrError> {
        let mut v = self.as_bytes().to_vec();
        v.push(0);
        Ok(v)
    }

    /// `vstrAddByteImpl` / `vstr_add_byte`: append one byte.
    pub fn add_byte(&mut self, b: u8) -> Result<(), StrError> {
        let at = self.add_len(1)?;
        self.buf[at] = b;
        Ok(())
    }

    /// `vstrAddCharImpl` / `vstr_add_char` (full, `STR_UNICODE=1`):
    /// append a code point as UTF-8 (the nano branch truncates to
    /// one byte — not live under full).
    pub fn add_char(&mut self, ch: u32) -> Result<(), StrError> {
        if ch < 0x80 {
            self.add_byte(ch as u8)
        } else if ch < 0x800 {
            self.add_byte(((ch >> 6) as u8) | 0xC0)?;
            self.add_byte(((ch & 0x3F) as u8) | 0x80)
        } else if ch < 0x10000 {
            self.add_byte(((ch >> 12) as u8) | 0xE0)?;
            self.add_byte((((ch >> 6) & 0x3F) as u8) | 0x80)?;
            self.add_byte(((ch & 0x3F) as u8) | 0x80)
        } else {
            self.add_byte(((ch >> 18) as u8) | 0xF0)?;
            self.add_byte((((ch >> 12) & 0x3F) as u8) | 0x80)?;
            self.add_byte((((ch >> 6) & 0x3F) as u8) | 0x80)?;
            self.add_byte(((ch & 0x3F) as u8) | 0x80)
        }
    }

    /// `vstrAddStrnImpl` / `vstr_add_strn`: append `len` bytes.
    pub fn add_strn(&mut self, s: &[u8]) -> Result<(), StrError> {
        let at = self.add_len(s.len())?;
        self.buf[at..at + s.len()].copy_from_slice(s);
        Ok(())
    }

    /// `vstrAddStrImpl` / `vstr_add_str`: append a NUL-terminated
    /// byte string (the Rust caller passes the bytes without the
    /// NUL — same content).
    pub fn add_str(&mut self, s: &[u8]) -> Result<(), StrError> {
        self.add_strn(s)
    }

    /// `vstrInsBlankBytesImpl`: make `byte_len` blank bytes at
    /// `byte_pos` (clamped to `len`) and return the position.
    fn ins_blank_bytes(&mut self, byte_pos_in: usize, byte_len: usize) -> Result<usize, StrError> {
        let l = self.len;
        let byte_pos = byte_pos_in.min(l);
        if byte_len > 0 {
            self.ensure_extra(byte_len)?;
            let base = byte_pos;
            let n = l - byte_pos;
            if n != 0 {
                self.buf.copy_within(base..base + n, base + byte_len);
            }
            self.len = self.len.wrapping_add(byte_len);
        }
        Ok(byte_pos)
    }

    /// `vstrInsByteImpl` / `vstr_ins_byte`: insert one byte.
    pub fn ins_byte(&mut self, byte_pos: usize, b: u8) -> Result<(), StrError> {
        let at = self.ins_blank_bytes(byte_pos, 1)?;
        self.buf[at] = b;
        Ok(())
    }

    /// `vstrInsCharImpl` / `vstr_ins_char`: insert one byte (the
    /// Zig truncates the char — `s[0] = @truncate(chr)`).
    pub fn ins_char(&mut self, char_pos: usize, chr: u32) -> Result<(), StrError> {
        let at = self.ins_blank_bytes(char_pos, 1)?;
        self.buf[at] = chr as u8;
        Ok(())
    }

    /// `vstr_cut_out_bytes`: delete `bytes_to_cut` bytes at
    /// `byte_pos` (no-op past the end; clamps to the tail).
    pub fn cut_out_bytes(&mut self, byte_pos: usize, bytes_to_cut: usize) {
        let l = self.len;
        if byte_pos >= l {
            return;
        } else if byte_pos + bytes_to_cut >= l {
            self.len = byte_pos;
        } else {
            let n = l - byte_pos - bytes_to_cut;
            self.buf.copy_within(byte_pos + bytes_to_cut..byte_pos + bytes_to_cut + n, byte_pos);
            self.len -= bytes_to_cut;
        }
    }

    /// `vstr_cut_head_bytes`: `cut_out_bytes(0, n)`.
    pub fn cut_head_bytes(&mut self, bytes_to_cut: usize) {
        self.cut_out_bytes(0, bytes_to_cut);
    }

    /// `vstr_cut_tail_bytes`: drop the last `len` bytes (all when
    /// `len` exceeds the length).
    pub fn cut_tail_bytes(&mut self, len: usize) {
        if len > self.len {
            self.len = 0;
        } else {
            self.len -= len;
        }
    }
}

/// A `fmt::Write` sink over a `&mut Vstr` — the Rust shape of the
/// Zig `mp_print_t` that `vstr_init_print` installs (`print.data =
/// vstr`, `print.print_strn = vstr_add_strn`). Used by the format
/// machinery's temporary bundles (`vspPrintHelperIntoTemp`, the
/// `{!r}`/`{!s}` conversion, `makeNewCase1`-style printing).
/// Writes cannot fail on a non-fixed `Vstr` (allocation is
/// infallible in this port — OOM aborts, as elsewhere); a fixed
/// buffer maps to `fmt::Error`.
pub struct VstrSink<'a>(pub &'a mut Vstr);

impl fmt::Write for VstrSink<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.add_strn(s.as_bytes()).map_err(|_| fmt::Error)
    }
}
