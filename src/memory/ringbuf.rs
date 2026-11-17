//! Ring buffer — port of `src/memory/ringbuf.zig` (vendor
//! `py/ringbuf.c` plus the `ringbuf_*` static inlines of
//! `py/ringbuf.h`).
//!
//! `ringbuf_t` is a stable non-config struct: `u8*` + 3×`u16`
//! (ringbuf.zig header). Exact C int widths are mirrored: `u16`
//! fields, `u32` temporaries, `usize` lengths. The Rust struct owns
//! its buffer (`Vec<u8>`); the C struct borrows an external array,
//! but no caller in petitpy ever builds a `ringbuf_t` (grep: zero
//! uses in `src/`), so ownership lives here until a consumer needs a
//! borrowed form.
//!
//! One deliberate corner: `get16` advances `iget` by 2 with `u16`
//! wrapping arithmetic (`wrapping_add`), matching C's `uint16_t`
//! wrap. (Zig's `r.iget += 2` would panic in a debug build when
//! `size == u16::MAX` and `iget == size - 1`; that corner is
//! unreachable from any petitpy caller and C wraps, so the port
//! follows C. See the result file.)
//!
//! NOT ported: the single-byte `ringbuf_get` / `ringbuf_peek` /
//! `ringbuf_put` static inlines of `ringbuf.h` — they are not in
//! `ringbuf.zig` (the port target), and no Zig caller uses them.
//! `free` / `avail` ARE ported (they are the `ringbuf.h` inlines the
//! Zig file reimplements as private `rb_free` / `rb_avail`).

/// Ring buffer: Zig `ringbuf_t` (ringbuf.zig).
///
/// Capacity is `size - 1`: one slot is always kept empty so that
/// full (`iget == iput` after a put would collide) can be told apart
/// from empty (`iget == iput`).
#[derive(Clone, Debug)]
pub struct RingBuf {
    buf: Vec<u8>,
    size: u16,
    iget: u16,
    iput: u16,
}

impl RingBuf {
    /// `ringbuf_alloc(r, sz)`: fresh buffer of `size` bytes, empty.
    pub fn new(size: u16) -> Self {
        RingBuf {
            buf: vec![0; size as usize],
            size,
            iget: 0,
            iput: 0,
        }
    }

    /// Static-initialisation form (`ringbuf_t buf = {buf_array,
    /// sizeof(buf_array)}`): take over an existing byte array.
    pub fn with_buf(buf: Vec<u8>) -> Self {
        let size = buf.len() as u16;
        RingBuf {
            buf,
            size,
            iget: 0,
            iput: 0,
        }
    }

    /// Buffer size (`ringbuf_t.size`).
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Read index (`ringbuf_t.iget`).
    pub fn iget(&self) -> u16 {
        self.iget
    }

    /// Write index (`ringbuf_t.iput`).
    pub fn iput(&self) -> u16 {
        self.iput
    }

    /// `ringbuf_free` (ringbuf.h): free slots.
    pub fn free(&self) -> usize {
        (self.size as usize + self.iget as usize - self.iput as usize - 1) % self.size as usize
    }

    /// `ringbuf_avail` (ringbuf.h): bytes available to read.
    pub fn avail(&self) -> usize {
        (self.size as usize + self.iput as usize - self.iget as usize) % self.size as usize
    }

    /// `ringbuf_memcpy_get_internal` (ringbuf.h): copy `data.len()`
    /// bytes out of the ring, advancing `iget`. No bounds checking —
    /// callers must check `avail` first (`get_bytes` does).
    fn memcpy_get_internal(&mut self, data: &mut [u8]) {
        let data_len = data.len();
        let mut iget: u32 = self.iget as u32;
        let iget_a: u32 = ((iget as usize + data_len) % self.size as usize) as u32;
        let mut off = 0usize;
        if iget_a < iget {
            let tail = (self.size as u32 - iget) as usize;
            data[off..off + tail]
                .copy_from_slice(&self.buf[iget as usize..iget as usize + tail]);
            off += tail;
            iget = 0;
        }
        let n = (iget_a - iget) as usize;
        data[off..off + n].copy_from_slice(&self.buf[iget as usize..iget as usize + n]);
        self.iget = iget_a as u16;
    }

    /// `ringbuf_memcpy_put_internal` (ringbuf.h): copy `data.len()`
    /// bytes into the ring, advancing `iput`. No bounds checking —
    /// callers must check `free` first (`put_bytes` does).
    fn memcpy_put_internal(&mut self, data: &[u8]) {
        let data_len = data.len();
        let mut iput: u32 = self.iput as u32;
        let iput_a: u32 = ((iput as usize + data_len) % self.size as usize) as u32;
        let mut off = 0usize;
        if iput_a < iput {
            let tail = (self.size as u32 - iput) as usize;
            self.buf[iput as usize..iput as usize + tail]
                .copy_from_slice(&data[off..off + tail]);
            off += tail;
            iput = 0;
        }
        let n = (iput_a - iput) as usize;
        self.buf[iput as usize..iput as usize + n]
            .copy_from_slice(&data[off..off + n]);
        self.iput = iput_a as u16;
    }

    /// `ringbuf_peek16`: big-endian `u16` at the head, or `-1` if
    /// fewer than 2 bytes are available. Does not advance `iget`.
    pub fn peek16(&self) -> i32 {
        if self.iget == self.iput {
            return -1;
        }
        let mut iget_a: u32 = self.iget as u32 + 1;
        if iget_a == self.size as u32 {
            iget_a = 0;
        }
        if iget_a == self.iput as u32 {
            return -1;
        }
        ((self.buf[self.iget as usize] as i32) << 8) | self.buf[iget_a as usize] as i32
    }

    /// `ringbuf_get16`: `peek16` + advance `iget` by 2 (mod `size`).
    pub fn get16(&mut self) -> i32 {
        let v = self.peek16();
        if v == -1 {
            return v;
        }
        self.iget = self.iget.wrapping_add(2);
        if self.iget >= self.size {
            self.iget -= self.size;
        }
        v
    }

    /// `ringbuf_put16`: big-endian `u16` at the tail; `0` on success,
    /// `-1` if fewer than 2 free slots.
    pub fn put16(&mut self, v: u16) -> i32 {
        let mut iput_a: u32 = self.iput as u32 + 1;
        if iput_a == self.size as u32 {
            iput_a = 0;
        }
        if iput_a == self.iget as u32 {
            return -1;
        }
        let mut iput_b: u32 = iput_a + 1;
        if iput_b == self.size as u32 {
            iput_b = 0;
        }
        if iput_b == self.iget as u32 {
            return -1;
        }
        self.buf[self.iput as usize] = ((v >> 8) & 0xff) as u8;
        self.buf[iput_a as usize] = (v & 0xff) as u8;
        self.iput = iput_b as u16;
        0
    }

    /// `ringbuf_get_bytes`: `0` on success, `-1` if not enough data
    /// available (try again later), `-2` if the request is larger
    /// than the buffer and can never succeed.
    pub fn get_bytes(&mut self, data: &mut [u8]) -> i32 {
        let data_len = data.len();
        if self.avail() < data_len {
            return if self.size as usize <= data_len { -2 } else { -1 };
        }
        self.memcpy_get_internal(data);
        0
    }

    /// `ringbuf_put_bytes`: `0` on success, `-1` if not enough free
    /// space, `-2` if the request is larger than the buffer.
    pub fn put_bytes(&mut self, data: &[u8]) -> i32 {
        let data_len = data.len();
        if self.free() < data_len {
            return if self.size as usize <= data_len { -2 } else { -1 };
        }
        self.memcpy_put_internal(data);
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get16_roundtrip_and_empty() {
        let mut r = RingBuf::new(8);
        assert_eq!(r.avail(), 0);
        assert_eq!(r.free(), 7);
        assert_eq!(r.peek16(), -1);
        assert_eq!(r.get16(), -1);
        assert_eq!(r.put16(0x1234), 0);
        assert_eq!(r.avail(), 2);
        assert_eq!(r.free(), 5);
        assert_eq!(r.peek16(), 0x1234);
        assert_eq!(r.peek16(), 0x1234);
        assert_eq!(r.get16(), 0x1234);
        assert_eq!(r.avail(), 0);
        assert_eq!(r.peek16(), -1);
        assert_eq!(r.get16(), -1);
    }

    #[test]
    fn put16_needs_two_free_slots() {
        // size 4 holds at most 3 bytes: one put16 (2 bytes) fits,
        // a second needs 2 free slots but only 1 is free.
        let mut r = RingBuf::new(4);
        assert_eq!(r.put16(0xAAAA), 0);
        assert_eq!(r.put16(0xBBBB), -1);
        assert_eq!(r.get16(), 0xAAAA);
        assert_eq!(r.put16(0xBBBB), 0);
        assert_eq!(r.get16(), 0xBBBB);
    }

    #[test]
    fn get16_wraps_iget_and_put16_wraps_iput() {
        let mut r = RingBuf::new(6);
        assert_eq!(r.put16(0xAAAA), 0);
        assert_eq!(r.put16(0xBBBB), 0);
        assert_eq!(r.iput(), 4);
        assert_eq!(r.get16(), 0xAAAA);
        assert_eq!(r.iget(), 2);
        assert_eq!(r.put16(0xCCCC), 0);
        assert_eq!(r.iput(), 0); // wrapped past the end
        assert_eq!(r.get16(), 0xBBBB);
        assert_eq!(r.iget(), 4);
        assert_eq!(r.get16(), 0xCCCC);
        assert_eq!(r.iget(), 0); // wrapped past the end
        assert_eq!(r.avail(), 0);
    }

    #[test]
    fn put_get_bytes_roundtrip_and_error_codes() {
        let mut r = RingBuf::new(4);
        let mut out = [0u8; 2];
        // Empty: not enough data (-1); request >= size can never work (-2).
        assert_eq!(r.get_bytes(&mut out), -1);
        assert_eq!(r.get_bytes(&mut [0u8; 4]), -2);
        // Too big to ever fit (-2); too big for now but possible (-1).
        assert_eq!(r.put_bytes(&[1, 2, 3, 4]), -2);
        assert_eq!(r.put_bytes(&[1, 2, 3]), 0);
        assert_eq!(r.put_bytes(&[9]), -1);
        assert_eq!(r.avail(), 3);
        assert_eq!(r.free(), 0);
        let mut got = [0u8; 3];
        assert_eq!(r.get_bytes(&mut got), 0);
        assert_eq!(got, [1, 2, 3]);
        assert_eq!(r.avail(), 0);
    }

    #[test]
    fn put_get_bytes_wrap_around() {
        let mut r = RingBuf::new(8);
        assert_eq!(r.put_bytes(b"ABCDE"), 0);
        assert_eq!(r.iput(), 5);
        let mut first = [0u8; 3];
        assert_eq!(r.get_bytes(&mut first), 0);
        assert_eq!(first, *b"ABC");
        assert_eq!(r.iget(), 3);
        // 5 free slots: 4 bytes fit, and iput wraps past the end.
        assert_eq!(r.put_bytes(b"FGHI"), 0);
        assert_eq!(r.iput(), 1);
        assert_eq!(r.avail(), 6);
        let mut rest = [0u8; 6];
        assert_eq!(r.get_bytes(&mut rest), 0);
        assert_eq!(rest, *b"DEFGHI");
        assert_eq!(r.avail(), 0);
    }

    #[test]
    fn single_byte_capacity_is_size_minus_one() {
        // size 2 holds at most 1 byte.
        let mut r = RingBuf::new(2);
        assert_eq!(r.free(), 1);
        assert_eq!(r.put_bytes(&[7]), 0);
        assert_eq!(r.free(), 0);
        assert_eq!(r.put_bytes(&[8]), -1);
        assert_eq!(r.peek16(), -1); // only 1 byte: no full u16
        let mut got = [0u8; 1];
        assert_eq!(r.get_bytes(&mut got), 0);
        assert_eq!(got, [7]);
    }

    #[test]
    fn with_buf_takes_over_array() {
        let mut r = RingBuf::with_buf(vec![0u8; 6]);
        assert_eq!(r.size(), 6);
        assert_eq!(r.put16(0x0102), 0);
        assert_eq!(r.get16(), 0x0102);
    }
}
