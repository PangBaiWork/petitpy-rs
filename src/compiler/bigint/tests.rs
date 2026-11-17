//! Ported unit tests for `bigint` — all 35 `test` blocks from
//! `src/compiler/bigint.zig`, in the same order, with the same
//! expectations. Names are translated to `snake_case`; the Zig
//! original's test names are kept in comments.

use super::*;

fn dec(src: &str) -> String {
    parse_literal(src).unwrap().to_decimal_string()
}

fn foldb(op: BinOp, a: &str, b: &str) -> Option<String> {
    fold_binary(op, a, b).unwrap()
}

// parseLiteral decimal
#[test]
fn parse_literal_decimal() {
    assert_eq!("12345", dec("12345"));
}

// parseLiteral negative decimal
#[test]
fn parse_literal_negative_decimal() {
    assert_eq!("-1", dec("-1"));
}

// parseLiteral zero
#[test]
fn parse_literal_zero() {
    assert_eq!("0", dec("0"));
}

// parseLiteral hex lower
#[test]
fn parse_literal_hex_lower() {
    assert_eq!("255", dec("0xff"));
}

// parseLiteral hex upper-prefix
#[test]
fn parse_literal_hex_upper_prefix() {
    assert_eq!("3735928559", dec("0XdeadBEEF"));
}

// parseLiteral octal
#[test]
fn parse_literal_octal() {
    assert_eq!("15", dec("0o17"));
}

// parseLiteral binary
#[test]
fn parse_literal_binary() {
    assert_eq!("10", dec("0b1010"));
}

// parseLiteral underscores
#[test]
fn parse_literal_underscores() {
    assert_eq!("1000000", dec("1_000_000"));
}

// parseLiteral hex with underscores
#[test]
fn parse_literal_hex_with_underscores() {
    assert_eq!("65535", dec("0xff_ff"));
}

// parseLiteral 2**64-1 (hex)
#[test]
fn parse_literal_2_pow_64_minus_1_hex() {
    assert_eq!("18446744073709551615", dec("0xffffffffffffffff"));
}

// parseLiteral 2**128-1 (hex, beyond i64)
#[test]
fn parse_literal_2_pow_128_minus_1_hex() {
    assert_eq!(
        "340282366920938463463374607431768211455",
        dec("0xffffffffffffffffffffffffffffffff")
    );
}

// parseLiteral -(2**63) (signed, hex)
#[test]
fn parse_literal_neg_2_pow_63_signed_hex() {
    assert_eq!("-9223372036854775808", dec("-0x8000000000000000"));
}

// normalizeToDecimal — hex shorthand
#[test]
fn normalize_to_decimal_hex_shorthand() {
    assert_eq!("256", normalize_to_decimal("0x100").unwrap());
}

// normalizeToDecimal — already decimal is idempotent
#[test]
fn normalize_to_decimal_already_decimal_is_idempotent() {
    assert_eq!(
        "1000000000000000000000",
        normalize_to_decimal("1000000000000000000000").unwrap()
    );
}

// isize parity sweep — same as std.fmt for values in range
#[test]
fn isize_parity_sweep_same_as_std_fmt_for_values_in_range() {
    let values: [i64; 11] = [
        0,
        1,
        -1,
        100,
        -100,
        1073741823, // SMALL_INT_MAX
        -1073741824, // SMALL_INT_MIN
        2147483647,
        -2147483648,
        9223372036854775807, // i64 max
        -9223372036854775807 - 1, // i64 min (avoid the -i64 literal overflow trap)
    ];
    for v in values {
        let fmt_s = format!("{v}");
        assert_eq!(fmt_s, dec(&fmt_s));
    }
}

// parseLiteral rejects invalid base char
#[test]
fn parse_literal_rejects_invalid_base_char() {
    assert_eq!(
        Err(BigIntError::InvalidCharacter),
        parse_literal("0xgg")
    );
}

// parseLiteral rejects empty after prefix
#[test]
fn parse_literal_rejects_empty_after_prefix() {
    assert_eq!(Err(BigIntError::InvalidCharacter), parse_literal("0x"));
}

// foldBinary add
#[test]
fn fold_binary_add() {
    assert_eq!(
        Some("18446744073709551616".to_string()),
        foldb(BinOp::Add, "0x8000000000000000", "0x8000000000000000")
    );
}

// foldBinary sub yielding zero
#[test]
fn fold_binary_sub_yielding_zero() {
    assert_eq!(
        Some("0".to_string()),
        foldb(BinOp::Sub, "0xffffffffffffffff", "0xffffffffffffffff")
    );
}

// foldBinary mul beyond i64
#[test]
fn fold_binary_mul_beyond_i64() {
    // (1 << 40) * (1 << 40) = 1 << 80
    assert_eq!(
        Some("1208925819614629174706176".to_string()),
        foldb(BinOp::Mul, "1099511627776", "1099511627776")
    );
}

// foldBinary floordiv python semantics
#[test]
fn fold_binary_floordiv_python_semantics() {
    // -7 // 3 == -3 (floor toward -inf, not -2 from C-style trunc)
    assert_eq!(Some("-3".to_string()), foldb(BinOp::Floordiv, "-7", "3"));
}

// foldBinary mod euclidean
#[test]
fn fold_binary_mod_euclidean() {
    // -7 % 3 == 2 (positive, matches divFloor remainder)
    assert_eq!(Some("2".to_string()), foldb(BinOp::Mod, "-7", "3"));
}

// foldBinary floordiv-by-zero returns null
#[test]
fn fold_binary_floordiv_by_zero_returns_null() {
    assert_eq!(None, foldb(BinOp::Floordiv, "5", "0"));
}

// foldBinary shl
#[test]
fn fold_binary_shl() {
    // 1 << 65
    assert_eq!(Some("36893488147419103232".to_string()), foldb(BinOp::Shl, "1", "65"));
}

// foldBinary shr
#[test]
fn fold_binary_shr() {
    // (1 << 65) >> 1 = 1 << 64
    assert_eq!(
        Some("18446744073709551616".to_string()),
        foldb(BinOp::Shr, "36893488147419103232", "1")
    );
}

// foldBinary shl rejects negative shift
#[test]
fn fold_binary_shl_rejects_negative_shift() {
    assert_eq!(None, foldb(BinOp::Shl, "1", "-1"));
}

// foldBinary shl rejects oversize shift
#[test]
fn fold_binary_shl_rejects_oversize_shift() {
    assert_eq!(None, foldb(BinOp::Shl, "1", "1000000"));
}

// foldBinary bitwise on big-ints
#[test]
fn fold_binary_bitwise_on_big_ints() {
    assert_eq!(
        Some("255".to_string()),
        foldb(BinOp::Band, "0xffffffffffffffff", "0xff")
    );
}

// foldUnary negate
#[test]
fn fold_unary_negate() {
    assert_eq!(
        "-9223372036854775808",
        fold_unary(UnOp::Neg, "0x8000000000000000").unwrap()
    );
}

// foldUnary invert positive
#[test]
fn fold_unary_invert_positive() {
    // ~0x3fffffff == -0x40000000 == -1073741824
    assert_eq!("-1073741824", fold_unary(UnOp::Invert, "0x3fffffff").unwrap());
}

// foldUnary invert negative
#[test]
fn fold_unary_invert_negative() {
    // ~(-5) == 4
    assert_eq!("4", fold_unary(UnOp::Invert, "-5").unwrap());
}

// tryToSmallInt fits
#[test]
fn try_to_small_int_fits() {
    let m = parse_literal("1073741823").unwrap(); // SMALL_INT_MAX
    assert_eq!(Some(1073741823), try_to_small_int(&m));
}

// tryToSmallInt overflow above
#[test]
fn try_to_small_int_overflow_above() {
    // SMALL_INT_MAX + 1 = 2^62 = 4611686018427387904
    let m = parse_literal("4611686018427387904").unwrap();
    assert_eq!(None, try_to_small_int(&m));
}

// tryToSmallInt overflow below
#[test]
fn try_to_small_int_overflow_below() {
    // SMALL_INT_MIN - 1 = -(2^62) - 1 = -4611686018427387905
    let m = parse_literal("-4611686018427387905").unwrap();
    assert_eq!(None, try_to_small_int(&m));
}

// classifyText routes correctly
#[test]
fn classify_text_routes_correctly() {
    assert_eq!(
        FoldResult::Small(0),
        classify_text("0").unwrap()
    );
    assert_eq!(
        FoldResult::BigDecimal("18446744073709551615".to_string()),
        classify_text("0xffffffffffffffff").unwrap()
    );
}
