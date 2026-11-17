//! Const-object handler — §26 of `compile.zig`.
//!
//! `compile_const_object` + `pn_to_const_obj`: turn a const-object
//! parse node into an `EmitCommon` const (int/float/complex/
//! str/bytes/tuple). Split out of `expr.rs` (file-size rule);
//! re-exported from there.

use super::{Compiler};
use crate::compiler::bigint::normalize_to_decimal;
use crate::compiler::emit::ConstObj;
use crate::compiler::grammar_data as grammar;
use crate::compiler::lexer::TokenKind;
use crate::compiler::parser::{
    pn_is_leaf, pn_is_small_int, pn_leaf_arg, pn_leaf_kind, pn_leaf_small_int, ConstObjectKind, ConstPayload, Pn, PN_STRING,
    PN_TOKEN,
};
use crate::compiler::qstr::Qstr;

/// SMALL_INT range tracks MP_SMALL_INT_BITS in the configured object
/// representation: 63 bits on 64-bit hosts (MICROPY_OBJ_REPR_A, our
/// nano + full build), 31 bits on 32-bit hosts (wasm32) where
/// mp_obj_t is u32.
const SMALL_INT_BITS: usize = 63;
const SMALL_INT_MAX: isize = (1isize << (SMALL_INT_BITS - 1)) - 1;
const SMALL_INT_MIN: isize = -(1isize << (SMALL_INT_BITS - 1));

// ─────────────────────────────────────────────────────────────────────────
// §26 — Const-object handler.
// ─────────────────────────────────────────────────────────────────────────

pub fn compile_const_object(comp: &mut Compiler, pn: Pn) {
    // ConstObjectKind discriminator + raw bytes payload — see parser
    // makeNodeConstObject. We surface integers as small-ints when they
    // fit; everything else routes through ConstObj.
    let nodes: Vec<Pn> = comp.tree.struct_nodes(pn).to_vec();
    // Const-object payload is split across three nodes:
    //   [0] = ConstObjectKind discriminator (u32 stored in usize)
    //   [1] = byte length (or element count for .tuple)
    //   [2] = pointer to the byte buffer (or element array for .tuple)
    // In Rust the payload lives in the arena entry (nodes[1]/[2] are 0).
    let kind_u32: u32 = nodes[0] as u32;
    let kind = ConstObjectKind::from_u32(kind_u32);

    match kind {
        Some(ConstObjectKind::Integer) => {
            let bytes: Vec<u8> = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Bytes(b)) => b.to_vec(),
                _ => Vec::new(),
            };
            // Vendor's threshold: fit in MP_SMALL_INT (63-bit signed
            // for MICROPY_OBJ_REPR_A on a 64-bit host — our nano +
            // full build). Without this gate, values like
            // `0o12345670123456701234` (=1.88e17, fits i64 but
            // overflows 62-bit small_int magnitude) would silently go
            // out as LOAD_CONST_SMALL_INT with a VLI payload, where
            // vendor emits an int_big const obj with the decimal text.
            //
            // For the int_big path we may need a decimal re-encoding —
            // vendor stores int_big as the DECIMAL text representation,
            // so a literal like 0o123 becomes "83" inside the .mpy. We
            // do that conversion here when the source text wasn't
            // already decimal.
            if let Ok(v) = parse_int_auto(&bytes) {
                if v >= SMALL_INT_MIN && v <= SMALL_INT_MAX {
                    comp.emit.load_const_small_int(v, &mut comp.scopes);
                    return;
                }
                // Fits in isize but not in small_int: emit as int_big
                // with the decimal text (matching vendor's mpz print).
                comp.emit.load_const_obj(
                    ConstObj::IntBig(v.to_string().into_bytes()),
                    &mut comp.scopes,
                );
                return;
            }
            // Overflowed isize — beyond what parseInt can hold.
            // The source text may also be hex/octal/binary; vendor mpy-cross
            // always serialises int_big as CANONICAL DECIMAL TEXT (see
            // py/persistentcode.c + py/parsenum.c with base=10). Route
            // through the bigint module.
            match std::str::from_utf8(&bytes)
                .ok()
                .and_then(|s| normalize_to_decimal(s).ok())
            {
                Some(dec) => comp.emit.load_const_obj(
                    ConstObj::IntBig(dec.into_bytes()),
                    &mut comp.scopes,
                ),
                None => comp.emit.overflow = true,
            }
        }
        Some(ConstObjectKind::FloatOrImag) => {
            let bytes: Vec<u8> = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Bytes(b)) => b.to_vec(),
                _ => Vec::new(),
            };
            comp.emit
                .load_const_obj(ConstObj::Float(bytes), &mut comp.scopes);
        }
        Some(ConstObjectKind::LongString) => {
            let bytes: Vec<u8> = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Bytes(b)) => b.to_vec(),
                _ => Vec::new(),
            };
            comp.emit
                .load_const_obj(ConstObj::Str(bytes), &mut comp.scopes);
        }
        Some(ConstObjectKind::BytesLiteral) => {
            let bytes: Vec<u8> = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Bytes(b)) => b.to_vec(),
                _ => Vec::new(),
            };
            comp.emit
                .load_const_obj(ConstObj::Bytes(bytes), &mut comp.scopes);
        }
        Some(ConstObjectKind::Tuple) => {
            // Parser-folded const tuple. The payload is a list of `Pn`
            // values, each a const-foldable parse node. Convert each
            // recursively to a ConstObj, build the .tuple variant, and
            // emit LOAD_CONST_OBJ.
            let items: Vec<Pn> = match comp.tree.const_payload(pn) {
                Some(ConstPayload::Tuple(items)) => items.clone(),
                _ => Vec::new(),
            };
            let inner: Vec<ConstObj> = items
                .iter()
                .map(|it| pn_to_const_obj(comp, *it))
                .collect();
            comp.emit
                .load_const_obj(ConstObj::Tuple(inner), &mut comp.scopes);
        }
        None => {
            comp.emit.overflow = true;
        }
    }
}

/// Recursive helper: convert a const-foldable Pn into the emit-side
/// ConstObj. Bytes for big ints / floats / strings borrow from the
/// parser's chunk (lifetime = ParseTree); the tuple-element list is
/// freshly owned. Vendor mp_parse_node_convert_to_obj
/// (parse.c:396) is the analogue.
///
/// Note the integer case keeps the RAW token bytes (e.g. `"0x123"`),
/// exactly as Zig's `pnToConstObj` does — unlike `compile_const_object`,
/// which re-encodes through `parseInt` first. This only fires for
/// parser-folded const tuples.
pub fn pn_to_const_obj(comp: &mut Compiler, pn: Pn) -> ConstObj {
    if pn_is_small_int(pn) {
        // Vendor encodes ints in const-obj tables as text.
        let v = pn_leaf_small_int(pn);
        return ConstObj::IntBig(v.to_string().into_bytes());
    }
    if pn_is_leaf(pn) {
        let lk = pn_leaf_kind(pn);
        let a = pn_leaf_arg(pn);
        if lk == PN_STRING {
            // String literal interned as a qstr — resolve bytes from
            // the context.
            let s = comp.qctx.qstr_str(a as Qstr);
            return ConstObj::Str(s.to_vec());
        }
        if lk == PN_TOKEN {
            let tk = a as u8;
            return match tk {
                x if x == TokenKind::KW_NONE as u8 => ConstObj::None,
                x if x == TokenKind::KW_TRUE as u8 => ConstObj::BoolTrue,
                x if x == TokenKind::KW_FALSE as u8 => ConstObj::BoolFalse,
                x if x == TokenKind::ELLIPSIS as u8 => ConstObj::Ellipsis,
                _ => ConstObj::None, // unreachable in const fold
            };
        }
        return ConstObj::None;
    }
    // Struct: either nested const_object (recurse) or empty atom_paren
    // (the empty tuple `()`).
    if comp
        .tree
        .is_struct_kind(pn, grammar::RULE_const_object)
    {
        let kind_u32: u32 = comp.tree.struct_nodes(pn)[0] as u32;
        let kind = ConstObjectKind::from_u32(kind_u32);
        match comp.tree.const_payload(pn) {
            Some(ConstPayload::Bytes(b)) => {
                let bytes = b.to_vec();
                match kind {
                    Some(ConstObjectKind::Integer) => ConstObj::IntBig(bytes),
                    Some(ConstObjectKind::FloatOrImag) => ConstObj::Float(bytes),
                    Some(ConstObjectKind::LongString) => ConstObj::Str(bytes),
                    Some(ConstObjectKind::BytesLiteral) => ConstObj::Bytes(bytes),
                    _ => ConstObj::None,
                }
            }
            Some(ConstPayload::Tuple(items)) => {
                let items = items.clone();
                let inner: Vec<ConstObj> = items
                    .iter()
                    .map(|it| pn_to_const_obj(comp, *it))
                    .collect();
                ConstObj::Tuple(inner)
            }
            None => ConstObj::None,
        }
    } else if comp.tree.is_struct_kind(pn, grammar::RULE_atom_paren) {
        // Empty `()`.
        ConstObj::Tuple(vec![])
    } else {
        ConstObj::None
    }
}

/// Mirror of Zig `std.fmt.parseInt(isize, bytes, 0)`: optional sign,
/// `0x`/`0o`/`0b` prefix (either case — verified against the reference
/// binary: `0X123` and `0x123` produce identical .mpy), no underscores
/// (those fall through to `normalize_to_decimal`), overflow → Err.
fn parse_int_auto(text: &[u8]) -> Result<isize, ()> {
    let mut rest = text;
    let mut neg = false;
    if let Some((&c, r)) = rest.split_first() {
        if c == b'-' {
            neg = true;
            rest = r;
        } else if c == b'+' {
            rest = r;
        }
    }
    let (radix, digits): (u32, &[u8]) = if rest.len() >= 2 && rest[0] == b'0' {
        match rest[1] {
            b'x' | b'X' => (16, &rest[2..]),
            b'o' | b'O' => (8, &rest[2..]),
            b'b' | b'B' => (2, &rest[2..]),
            _ => (10, rest),
        }
    } else {
        (10, rest)
    };
    if digits.is_empty() {
        return Err(());
    }
    let mut acc: u128 = 0;
    for &c in digits {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u128,
            b'a'..=b'f' => (c - b'a' + 10) as u128,
            b'A'..=b'F' => (c - b'A' + 10) as u128,
            _ => return Err(()),
        };
        if d >= radix as u128 {
            return Err(());
        }
        acc = acc
            .checked_mul(radix as u128)
            .and_then(|a| a.checked_add(d))
            .ok_or(())?;
    }
    if neg {
        if acc > (isize::MAX as u128) + 1 {
            return Err(());
        }
        Ok((-(acc as i128)) as isize)
    } else {
        if acc > isize::MAX as u128 {
            return Err(());
        }
        Ok(acc as isize)
    }
}
