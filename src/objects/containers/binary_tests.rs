//! Tests for `binary.rs` — differential against the reference
//! binary where a probe exists (see the probe notes in the test
//! bodies), plus the `binary.zig` size tables.

use super::binary::*;
use super::tests::Fixture;
use crate::memory::gc::GcArena;
use crate::objects::value::{Obj, Value, SMALL_INT_MAX};

/// Extract a `u64` from an int `Value` (small or heap) via its
/// little-endian magnitude bytes.
fn value_as_u64(arena: &GcArena, v: Value) -> u64 {
    match v {
        Value::SmallInt(n) => n as u64,
        Value::Heap(r) => match arena.get(r) {
            Obj::Int(z) => {
                let mut buf = [0u8; 8];
                let _ = z.as_bytes(false, false, &mut buf);
                u64::from_le_bytes(buf)
            }
            _ => panic!("not an int: {v:?}"),
        },
        _ => panic!("not an int: {v:?}"),
    }
}

#[test]
fn get_size_struct_layout() {
    // '<' (and '>'): fixed struct layout — l/L are 4 bytes.
    assert_eq!(binary_get_size(b'<', b'b'), Ok((1, 1)));
    assert_eq!(binary_get_size(b'<', b'B'), Ok((1, 1)));
    assert_eq!(binary_get_size(b'<', b'h'), Ok((2, 1)));
    assert_eq!(binary_get_size(b'<', b'H'), Ok((2, 1)));
    assert_eq!(binary_get_size(b'<', b'i'), Ok((4, 1)));
    assert_eq!(binary_get_size(b'<', b'I'), Ok((4, 1)));
    assert_eq!(binary_get_size(b'<', b'l'), Ok((4, 1)));
    assert_eq!(binary_get_size(b'<', b'L'), Ok((4, 1)));
    assert_eq!(binary_get_size(b'<', b'q'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'<', b'Q'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'<', b'P'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'<', b'O'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'<', b'S'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'<', b'e'), Ok((2, 1)));
    assert_eq!(binary_get_size(b'<', b'f'), Ok((4, 1)));
    assert_eq!(binary_get_size(b'<', b'd'), Ok((8, 1)));
    assert_eq!(binary_get_size(b'>', b'l'), Ok((4, 1)));
    // Bad typecodes raise ValueError("bad typecode").
    assert_eq!(
        binary_get_size(b'<', b'x'),
        Err(BinaryError::BadTypecode)
    );
    assert_eq!(
        binary_get_size(b'<', b'u'),
        Err(BinaryError::BadTypecode)
    );
    assert_eq!(
        binary_get_size(b'@', b'x'),
        Err(BinaryError::BadTypecode)
    );
}

#[test]
fn get_size_native() {
    // '@': native sizes/aligns — l/L are 8 bytes here.
    assert_eq!(binary_get_size(b'@', b'b'), Ok((1, 1)));
    assert_eq!(binary_get_size(b'@', b'B'), Ok((1, 1)));
    assert_eq!(binary_get_size(b'@', BYTEARRAY_TYPECODE), Ok((1, 1)));
    assert_eq!(binary_get_size(b'@', b'h'), Ok((2, 2)));
    assert_eq!(binary_get_size(b'@', b'i'), Ok((4, 4)));
    assert_eq!(binary_get_size(b'@', b'l'), Ok((8, 8)));
    assert_eq!(binary_get_size(b'@', b'L'), Ok((8, 8)));
    assert_eq!(binary_get_size(b'@', b'q'), Ok((8, 8)));
    assert_eq!(binary_get_size(b'@', b'P'), Ok((8, 8)));
    assert_eq!(binary_get_size(b'@', b'e'), Ok((2, 2)));
    assert_eq!(binary_get_size(b'@', b'f'), Ok((4, 4)));
    assert_eq!(binary_get_size(b'@', b'd'), Ok((8, 8)));
}

#[test]
fn get_int_endianness_and_sign() {
    assert_eq!(binary_get_int(0, true, false, &[]), 0);
    // LE unsigned: 0x0201 = 513.
    assert_eq!(binary_get_int(2, false, false, &[0x01, 0x02]), 513);
    // BE unsigned: 0x0102 = 258.
    assert_eq!(binary_get_int(2, false, true, &[0x01, 0x02]), 258);
    // LE signed: 0xff = -1.
    assert_eq!(binary_get_int(1, true, false, &[0xff]), -1);
    // LE signed, 2 bytes: 0xffff = -1.
    assert_eq!(binary_get_int(2, true, false, &[0xff, 0xff]), -1);
    // BE signed: 0x80 0x00 = -32768.
    assert_eq!(binary_get_int(2, true, true, &[0x80, 0x00]), -32768);
    // Unsigned does not sign-extend.
    assert_eq!(binary_get_int(1, false, false, &[0xff]), 255);
    assert_eq!(binary_get_int(2, false, false, &[0xff, 0xff]), 65535);
}

#[test]
fn set_int_round_trip() {
    let mut buf = [0u8; 8];
    binary_set_int(4, false, &mut buf, 0x01020304);
    assert_eq!(&buf[..4], &[0x04, 0x03, 0x02, 0x01]);
    binary_set_int(4, true, &mut buf, 0x01020304);
    assert_eq!(&buf[..4], &[0x01, 0x02, 0x03, 0x04]);
    // Truncation to val_sz.
    binary_set_int(2, false, &mut buf, 0x01020304);
    assert_eq!(&buf[..2], &[0x04, 0x03]);
}

#[test]
fn get_val_array_ints() {
    // Read one element out of `data` with a fresh context (the
    // context borrows the fixture mutably, so it cannot be alive
    // while the assertions below borrow the arena).
    fn get_val(fx: &mut Fixture, tc: u8, data: &[u8], index: usize) -> Value {
        let mut ctx = fx.ctx();
        binary_get_val_array(&mut ctx, tc, data, index)
    }
    let mut fx = Fixture::new();
    // 'b': i8 truncation — probe: array('b', [200])[0] == -56.
    let data = [200u8];
    assert_eq!(get_val(&mut fx, b'b', &data, 0), Value::SmallInt(-56));
    // 'B': u8 — probe: array('B', [300])[0] == 44 (via from-int
    // truncation on store; read back is the stored byte).
    let data = [44u8];
    assert_eq!(get_val(&mut fx, b'B', &data, 0), Value::SmallInt(44));
    // 'h'/'H'.
    let data = [0x10u8, 0x27];
    assert_eq!(get_val(&mut fx, b'h', &data, 0), Value::SmallInt(10000));
    let data = [0xffu8, 0xff];
    assert_eq!(get_val(&mut fx, b'H', &data, 0), Value::SmallInt(65535));
    // 'i': c_int — probe: array('i', [2**31])[0] == -2147483648.
    let data = [0x00u8, 0x00, 0x00, 0x80];
    assert_eq!(
        get_val(&mut fx, b'i', &data, 0),
        Value::SmallInt(-2147483648)
    );
    // 'I': from_uint — probe: array('I', [2**32-1])[0] == 4294967295.
    let data = [0xffu8, 0xff, 0xff, 0xff];
    let v = get_val(&mut fx, b'I', &data, 0);
    assert_eq!(value_as_u64(&fx.arena, v), 4294967295);
    // 'l': c_long.
    let data = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    assert_eq!(get_val(&mut fx, b'l', &data, 0), Value::SmallInt(-1));
    // 'L'/'Q': from_ull — probe: array('Q', [2**64-1])[0] is the
    // full 64-bit value.
    let v = get_val(&mut fx, b'Q', &data, 0);
    assert_eq!(value_as_u64(&fx.arena, v), u64::MAX);
    // 'q': from_ll.
    let data = [0x00u8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40];
    let v = get_val(&mut fx, b'q', &data, 0);
    assert_eq!(value_as_u64(&fx.arena, v), 1u64 << 62);
}

#[test]
fn get_val_array_float_fallthrough() {
    let mut fx = Fixture::new();
    let mut ctx = fx.ctx();
    // e/f/d have NO value branch: read is small int 0 — probe:
    // array('f', [1])[0] == 0, array('e', [5])[0] == 0.
    let data = [0x01u8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    for tc in [b'e', b'f', b'd'] {
        assert_eq!(
            binary_get_val_array(&mut ctx, tc, &data, 0),
            Value::SmallInt(0),
            "typecode {tc}"
        );
    }
}

#[test]
fn get_val_array_o_and_p_round_trip() {
    let mut fx = Fixture::new();
    let s = fx.heap_str(b"ab");
    let t = fx.heap_tuple(vec![Value::SmallInt(1), Value::SmallInt(2)]);
    for v in [
        Value::SmallInt(-12345),
        Value::Bool(true),
        Value::Bool(false),
        Value::None,
        Value::Qstr(5),
        s,
        t,
    ] {
        let mut data = [0u8; 8];
        let mut ctx = fx.ctx();
        binary_set_val_array(&mut ctx, b'O', &mut data, 0, v).unwrap();
        let mut ctx = fx.ctx();
        let back = binary_get_val_array(&mut ctx, b'O', &data, 0);
        assert_eq!(back, v, "O round-trip for {v:?}");
    }
    // 'P': pointer bits as an int.
    let mut data = [0u8; 8];
    binary_set_val_array_from_int(b'P', &mut data, 0, 12345);
    let mut ctx = fx.ctx();
    assert_eq!(
        binary_get_val_array(&mut ctx, b'P', &data, 0),
        Value::SmallInt(12345)
    );
}

#[test]
fn set_val_array_from_int_truncation() {
    // Probe: array('b', [200])[0] == -56, array('h', [70000])[0]
    // == 4464, array('B', [-1])[0] == 255.
    let mut data = [0u8; 8];
    binary_set_val_array_from_int(b'b', &mut data, 0, 200);
    assert_eq!(data[0] as i8, -56);
    binary_set_val_array_from_int(b'h', &mut data, 0, 70000);
    assert_eq!(i16::from_le_bytes([data[0], data[1]]), 4464);
    binary_set_val_array_from_int(b'B', &mut data, 0, -1);
    assert_eq!(data[0], 255);
    binary_set_val_array_from_int(b'H', &mut data, 0, -1);
    assert_eq!(u16::from_le_bytes([data[0], data[1]]), 65535);
    binary_set_val_array_from_int(b'i', &mut data, 0, 1 << 31);
    assert_eq!(i32::from_le_bytes(data[..4].try_into().unwrap()), i32::MIN);
    // 'f'/'d'/'e': no branch — the slot stays zero.
    let mut data = [0xAAu8; 8];
    binary_set_val_array_from_int(b'f', &mut data, 0, 5);
    assert_eq!(data, [0xAA; 8]);
    binary_set_val_array_from_int(b'd', &mut data, 0, 5);
    assert_eq!(data, [0xAA; 8]);
}

#[test]
fn set_val_array_exact_int_path() {
    let mut fx = Fixture::new();
    // Probe: array('i', [2**70 + 5])[0] == 5 — the heap int's
    // low 4 bytes are written (int_to_bytes_impl, return ignored).
    // 2**63 + 5 exercises the same path (a heap int congruent to
    // 5 mod 2**32); 2**70 itself does not fit in a u64.
    let big = {
        let mut ctx = fx.ctx();
        new_int_from_u64(&mut ctx, (1u64 << 63) + 5)
    };
    let mut data = [0u8; 8];
    let mut ctx = fx.ctx();
    binary_set_val_array(&mut ctx, b'i', &mut data, 0, big).unwrap();
    assert_eq!(i32::from_le_bytes(data[..4].try_into().unwrap()), 5);
    // Negative heap int: probe array('i', [-(2**70)-5])[0] == -5
    // (low 32 bits of the two's-complement value); -(2**62)-5
    // has the same low 32 bits and fits in i64.
    let big = {
        let ctx = fx.ctx();
        Value::int(
            ctx.arena,
            Some(ctx.types.int),
            ctx.roots,
            -((1i64 << 62) + 5),
        )
    };
    let mut ctx = fx.ctx();
    binary_set_val_array(&mut ctx, b'i', &mut data, 0, big).unwrap();
    assert_eq!(i32::from_le_bytes(data[..4].try_into().unwrap()), -5);
    // A float is rejected via get_int — probe: array('f', [1.5])
    // raises TypeError("can't convert to int").
    let float_t = fx.types.float;
    let f = Value::Heap({
        let mut ctx = fx.ctx();
        ctx.alloc(Some(float_t), Obj::Float(1.5))
    });
    let mut ctx = fx.ctx();
    assert_eq!(
        binary_set_val_array(&mut ctx, b'i', &mut data, 0, f),
        Err(BinaryError::IntConversion)
    );
}

#[test]
fn get_set_val_round_trip() {
    let mut fx = Fixture::new();
    // '<' int round-trip, big-endian variant.
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    binary_set_val(&mut ctx, b'<', b'i', Value::SmallInt(0x01020304), &mut buf, &mut pos)
        .unwrap();
    assert_eq!(pos, 4);
    assert_eq!(&buf[..4], &[0x04, 0x03, 0x02, 0x01]);
    let mut pos = 0;
    let mut ctx = fx.ctx();
    let v = binary_get_val(&mut ctx, b'<', b'i', &buf, &mut pos).unwrap();
    assert_eq!(v, Value::SmallInt(0x01020304));
    // '>' big-endian.
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    binary_set_val(&mut ctx, b'>', b'i', Value::SmallInt(0x01020304), &mut buf, &mut pos)
        .unwrap();
    assert_eq!(&buf[..4], &[0x01, 0x02, 0x03, 0x04]);
    // '@' aligns: 'b' then 'i' starts at offset 4.
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    binary_set_val(&mut ctx, b'@', b'b', Value::SmallInt(7), &mut buf, &mut pos).unwrap();
    assert_eq!(pos, 1);
    binary_set_val(&mut ctx, b'@', b'i', Value::SmallInt(9), &mut buf, &mut pos).unwrap();
    assert_eq!(pos, 8);
    assert_eq!(&buf[4..8], &[9, 0, 0, 0]);
    // 'O' round-trip through a buffer.
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    binary_set_val(&mut ctx, b'<', b'O', Value::SmallInt(-7), &mut buf, &mut pos).unwrap();
    let mut pos = 0;
    let mut ctx = fx.ctx();
    let v = binary_get_val(&mut ctx, b'<', b'O', &buf, &mut pos).unwrap();
    assert_eq!(v, Value::SmallInt(-7));
    // 'S' reads back as an empty str (safe-Rust limit, pinned).
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    binary_set_val(&mut ctx, b'<', b'S', Value::SmallInt(0), &mut buf, &mut pos).unwrap();
    let mut pos = 0;
    let mut ctx = fx.ctx();
    let v = binary_get_val(&mut ctx, b'<', b'S', &buf, &mut pos).unwrap();
    match v {
        Value::Heap(r) => match fx.arena.get(r) {
            Obj::Str(s) => assert!(s.bytes.is_empty()),
            other => panic!("S read back as {other:?}"),
        },
        other => panic!("S read back as {other:?}"),
    }
    // Bad typecode propagates.
    let mut buf = [0u8; 16];
    let mut pos = 0;
    let mut ctx = fx.ctx();
    assert_eq!(
        binary_set_val(&mut ctx, b'<', b'x', Value::SmallInt(0), &mut buf, &mut pos),
        Err(BinaryError::BadTypecode)
    );
}

#[test]
fn new_int_from_u64_boundaries() {
    let mut fx = Fixture::new();
    let v = {
        let mut ctx = fx.ctx();
        new_int_from_u64(&mut ctx, SMALL_INT_MAX as u64)
    };
    assert_eq!(v, Value::SmallInt(SMALL_INT_MAX));
    let v = {
        let mut ctx = fx.ctx();
        new_int_from_u64(&mut ctx, (1u64 << 62) + 5)
    };
    assert_eq!(value_as_u64(&fx.arena, v), (1u64 << 62) + 5);
    let v = {
        let mut ctx = fx.ctx();
        new_int_from_u64(&mut ctx, u64::MAX)
    };
    assert_eq!(value_as_u64(&fx.arena, v), u64::MAX);
}

#[test]
fn value_word_round_trip() {
    let mut fx = Fixture::new();
    let s = fx.heap_str(b"ab");
    let t = fx.heap_tuple(vec![Value::SmallInt(1)]);
    for v in [
        Value::SmallInt(-12345),
        Value::SmallInt(0),
        Value::Bool(true),
        Value::Bool(false),
        Value::None,
        Value::Qstr(5),
        s,
        t,
    ] {
        assert_eq!(value_from_word(value_word(v)), v, "word round-trip {v:?}");
    }
    // REPR_A words match mp_mirrors.zig exactly.
    assert_eq!(value_word(Value::Bool(true)), 30);
    assert_eq!(value_word(Value::Bool(false)), 14);
    assert_eq!(value_word(Value::None), 6);
    assert_eq!(value_word(Value::Qstr(7)), (7 << 3) | 2);
    assert_eq!(value_word(Value::SmallInt(-1)), ((-1i64 as u64) << 1) | 1);
    // Word 0 (unwritten slot) reads as None.
    assert_eq!(value_from_word(0), Value::None);
}

#[test]
fn array_item_size_table() {
    assert_eq!(array_item_size(b'b'), Ok(1));
    assert_eq!(array_item_size(b'i'), Ok(4));
    assert_eq!(array_item_size(b'l'), Ok(8));
    assert_eq!(array_item_size(b'q'), Ok(8));
    assert_eq!(array_item_size(b'd'), Ok(8));
    assert_eq!(
        array_item_size(b'x'),
        Err(BinaryError::BadTypecode)
    );
}
