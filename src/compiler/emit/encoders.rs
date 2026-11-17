//! Variable-length integer encoders. Wire-byte identical to vendor
//! `mp_encode_uint` / the signed encoder in `emit_write_bytecode_byte_int`.
//!
//! Port of `encodeUint` / `encodeInt` in `src/compiler/emit.zig`.

use super::consts::DUMMY_DATA_SIZE;

/// Big-endian 7-bit chunked unsigned VLI. Writes into `buf` (must hold
/// at least `DUMMY_DATA_SIZE` bytes) starting from the end; returns the
/// slice of bytes actually written.
pub fn encode_uint(buf: &mut [u8], val_in: usize) -> &[u8] {
    debug_assert!(buf.len() >= DUMMY_DATA_SIZE);
    let mut val = val_in;
    let mut tmp = [0u8; DUMMY_DATA_SIZE];
    let mut p = tmp.len();
    p -= 1;
    tmp[p] = (val & 0x7f) as u8;
    val >>= 7;
    while val != 0 {
        p -= 1;
        tmp[p] = ((val & 0x7f) as u8) | 0x80;
        val >>= 7;
    }
    let written = tmp.len() - p;
    buf[..written].copy_from_slice(&tmp[p..]);
    &buf[..written]
}

/// Signed VLI: same encoding as `encode_uint`, except the
/// sign-extension trick from vendor `emitbc.c` — pads with `0x7f` /
/// `0x00` if the high bit would otherwise be ambiguous with the value
/// bit. Mirrors `emit_write_bytecode_byte_int` exactly.
pub fn encode_int(buf: &mut [u8], val_in: isize) -> &[u8] {
    debug_assert!(buf.len() >= DUMMY_DATA_SIZE);
    let mut num = val_in;
    let mut tmp = [0u8; DUMMY_DATA_SIZE];
    let mut p = tmp.len();
    loop {
        p -= 1;
        tmp[p] = ((num as usize) & 0x7f) as u8;
        num >>= 7;
        if num == 0 || num == -1 {
            break;
        }
    }
    if num == -1 && (tmp[p] & 0x40) == 0 {
        p -= 1;
        tmp[p] = 0x7f;
    } else if num == 0 && (tmp[p] & 0x40) != 0 {
        p -= 1;
        tmp[p] = 0;
    }
    // Bytes are stored with high-bit continuation marker; only the last
    // (least-significant) byte gets bit 7 clear.
    let mut i = p;
    let mut oi = 0;
    while i != tmp.len() - 1 {
        buf[oi] = tmp[i] | 0x80;
        oi += 1;
        i += 1;
    }
    buf[oi] = tmp[i];
    oi += 1;
    &buf[..oi]
}
