//! In-memory source reader — port of `src/frontend/reader.zig`
//! (vendor `py/reader.c`, mem-reader part only).
//!
//! Under nano (`MICROPY_READER_POSIX=0`, `MICROPY_HAS_FILE_READER=0`)
//! only the in-memory reader is live: 4 fns, no raises, no MP_STATE
//! (reader.zig header). The Zig `Reader` / `ReaderMem` extern structs
//! hold raw pointers; the Rust port replaces them with a borrowed or
//! owned byte slice (`Cow`), so no `unsafe` is needed.
//!
//! Petitpy's Zig semantics (NOT vendor C's — the divergence is real
//! and is what the running binary does, so the Rust port follows Zig):
//! - `free_len == 0` means the buffer is borrowed ("ROM" in petitpy's
//!   Zig sense): never freed on close, and `try_read_rom` may expose it.
//! - `free_len > 0` means the buffer is owned: freed on close, and
//!   `try_read_rom` always returns `None`.
//! Vendor C instead uses the `MP_READER_IS_ROM = (size_t)-1` sentinel:
//! ROM ⟺ `free_len == MP_READER_IS_ROM`, and C's `try_read_rom` has NO
//! bounds check. `MP_READER_IS_ROM` is never passed anywhere in
//! petitpy (grep: zero uses in `src/`), and the only live caller,
//! `persistentcode.zig:465`, passes `free_len = 0`.
//! Zig's `try_read_rom` additionally returns `None` when fewer than
//! `len` bytes remain (a bounds check C lacks).

use std::borrow::Cow;

/// `MP_READER_EOF = (mp_uint_t)(-1)` (reader.h).
pub const MP_READER_EOF: usize = usize::MAX;

/// In-memory reader: Zig `ReaderMem` + `Reader` (reader.zig).
///
/// The buffer is the `[beg, end)` byte range; `pos` is `cur - beg`.
/// Borrowed ⟺ Zig `free_len == 0` (ROM); Owned ⟺ Zig `free_len > 0`.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: Cow<'a, [u8]>,
    pos: usize,
}

impl<'a> Reader<'a> {
    /// `mp_reader_new_mem(reader, buf, len, free_len = 0)`: borrowed
    /// buffer, never freed on close, ROM-exposable via `try_read_rom`.
    pub fn new_mem(buf: &'a [u8]) -> Self {
        Reader {
            buf: Cow::Borrowed(buf),
            pos: 0,
        }
    }

    /// `mp_reader_new_mem(reader, buf, len, free_len > 0)`: owned
    /// buffer, freed on close (`Vec` Drop), never ROM-exposable.
    /// Zig's `free_len` is the length passed to `m_free`; the Rust
    /// equivalent is ownership of the whole `Vec` (the readable
    /// range is the `Vec`'s full length).
    pub fn new_mem_owned(buf: Vec<u8>) -> Self {
        Reader {
            buf: Cow::Owned(buf),
            pos: 0,
        }
    }

    /// Bytes not yet read (`end - cur`).
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// `mp_reader_mem_readbyte`: the next byte, or `None` at end of
    /// stream (`MP_READER_EOF`). Calling again after EOF keeps
    /// returning `None` (reader.h contract).
    pub fn read_byte(&mut self) -> Option<u8> {
        if self.pos < self.buf.len() {
            let b = self.buf[self.pos];
            self.pos += 1;
            Some(b)
        } else {
            None
        }
    }

    /// `mp_reader_mem_readbyte` in its raw form: the byte as `usize`,
    /// or `MP_READER_EOF` at end of stream.
    pub fn read_byte_raw(&mut self) -> usize {
        match self.read_byte() {
            Some(b) => b as usize,
            None => MP_READER_EOF,
        }
    }

    /// `mp_reader_mem_close`: frees the buffer iff `free_len > 0`
    /// (owned); the borrowed buffer is untouched. Consuming: dropping
    /// the `Reader` does exactly the same thing (`Vec` Drop).
    pub fn close(self) {
        // `Cow::Owned` is freed by `Vec`'s Drop; `Cow::Borrowed` is
        // untouched. Nothing else to do.
    }

    /// `mp_reader_try_read_rom`: `Some(&[u8])` iff this reader is a
    /// mem reader with `free_len == 0` (borrowed) and at least `len`
    /// bytes remain; advances `cur` by `len`. Otherwise `None`.
    /// (Zig adds the remaining-length check that vendor C lacks.)
    pub fn try_read_rom(&mut self, len: usize) -> Option<&[u8]> {
        if !matches!(self.buf, Cow::Borrowed(_)) {
            return None;
        }
        if self.buf.len() - self.pos < len {
            return None;
        }
        let start = self.pos;
        self.pos += len;
        Some(&self.buf[start..start + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_byte_reads_sequentially_and_eof_is_sticky() {
        let mut r = Reader::new_mem(b"abc");
        assert_eq!(r.remaining(), 3);
        assert_eq!(r.read_byte(), Some(b'a'));
        assert_eq!(r.read_byte(), Some(b'b'));
        assert_eq!(r.remaining(), 1);
        assert_eq!(r.read_byte(), Some(b'c'));
        assert_eq!(r.read_byte(), None);
        assert_eq!(r.read_byte(), None);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn read_byte_raw_returns_eof_sentinel() {
        let mut r = Reader::new_mem(b"a");
        assert_eq!(r.read_byte_raw(), b'a' as usize);
        assert_eq!(r.read_byte_raw(), MP_READER_EOF);
        assert_eq!(MP_READER_EOF, usize::MAX);
    }

    #[test]
    fn try_read_rom_borrowed_slices_and_advances() {
        let mut r = Reader::new_mem(b"hello world");
        assert_eq!(r.try_read_rom(5), Some(&b"hello"[..]));
        assert_eq!(r.read_byte(), Some(b' '));
        assert_eq!(r.try_read_rom(5), Some(&b"world"[..]));
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.try_read_rom(1), None);
    }

    #[test]
    fn try_read_rom_too_long_returns_none_without_advancing() {
        let mut r = Reader::new_mem(b"abc");
        assert_eq!(r.try_read_rom(4), None);
        assert_eq!(r.remaining(), 3);
        assert_eq!(r.read_byte(), Some(b'a'));
    }

    #[test]
    fn try_read_rom_zero_len_on_empty_buffer() {
        let mut r = Reader::new_mem(b"");
        assert_eq!(r.read_byte(), None);
        assert_eq!(r.try_read_rom(0), Some(&[][..]));
    }

    #[test]
    fn try_read_rom_owned_returns_none_but_reads_still_work() {
        let mut r = Reader::new_mem_owned(b"hello".to_vec());
        assert_eq!(r.try_read_rom(1), None);
        assert_eq!(r.read_byte(), Some(b'h'));
        assert_eq!(r.try_read_rom(2), None);
        assert_eq!(r.read_byte(), Some(b'e'));
    }

    #[test]
    fn close_after_read_is_a_noop() {
        let mut r = Reader::new_mem(b"ab");
        assert_eq!(r.read_byte(), Some(b'a'));
        r.close();
        let mut owned = Reader::new_mem_owned(b"xy".to_vec());
        assert_eq!(owned.read_byte(), Some(b'x'));
        owned.close();
    }
}
