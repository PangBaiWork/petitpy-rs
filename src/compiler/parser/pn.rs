//! Parse-node bit-packing and const-object kinds.
//!
//! Pure-Rust port of the `mp_parse_node_t` macros in vendor parse.h
//! plus the `ConstObjectKind` discriminator from `parser.zig`.
//! The `Pn` tag encoding is identical to the Zig original — leaves,
//! small ints and nulls are bit-packed exactly as in vendor, and all
//! the `pn_*` predicates work on the raw `Pn` value.

/// A parse node, bit-packed exactly like vendor `mp_parse_node_t`.
/// See parse.h:36-48 for the layout. usize because uintptr_t.
pub type Pn = usize;

pub const PN_NULL: Pn = 0;
pub const PN_SMALL_INT: Pn = 0x01;
pub const PN_ID: Pn = 0x02;
pub const PN_STRING: Pn = 0x06;
pub const PN_TOKEN: Pn = 0x0a;

// ── Parse-node bit-packing (pure-Rust port of parse.h macros) ──────────────

/// Build a SMALL_INT leaf: bit 0 set, value left-shifted by 1.
pub fn pn_new_small_int(val: isize) -> Pn {
    PN_SMALL_INT | ((val as usize) << 1)
}

/// Build a leaf of `kind` (one of PN_ID/PN_STRING/PN_TOKEN) carrying `arg`
/// (qstr id or token-kind ordinal) shifted into bits 4+.
pub fn pn_new_leaf(kind: Pn, arg: usize) -> Pn {
    kind | (arg << 4)
}

pub fn pn_is_null(pn: Pn) -> bool {
    pn == PN_NULL
}

pub fn pn_is_leaf(pn: Pn) -> bool {
    (pn & 3) != 0
}

pub fn pn_is_struct(pn: Pn) -> bool {
    pn != PN_NULL && (pn & 3) == 0
}

pub fn pn_is_small_int(pn: Pn) -> bool {
    (pn & 0x1) == PN_SMALL_INT
}

pub fn pn_is_id(pn: Pn) -> bool {
    (pn & 0x0f) == PN_ID
}

pub fn pn_is_token_kind(pn: Pn, k: u16) -> bool {
    pn == (PN_TOKEN | ((k as Pn) << 4))
}

pub fn pn_leaf_kind(pn: Pn) -> usize {
    pn & 0x0f
}

pub fn pn_leaf_arg(pn: Pn) -> usize {
    pn >> 4
}

pub fn pn_leaf_small_int(pn: Pn) -> isize {
    // Arithmetic right shift — preserve sign bit. Bit 0 is the tag.
    (pn as isize) >> 1
}

// ── Const-object kind discriminator ───────────────────────────────────────
//
// Vendor's RULE_const_object node payload is one or two mp_parse_node_t
// slots holding an mp_obj_t. Under our nano config we don't have an
// obj system at parse time, so we steal the same layout to carry the
// raw literal bytes plus a one-byte kind discriminator.
//
// Layout of nodes[]:
//   [0] = ConstObjectKind discriminator (stored as usize, low 32 bits used)
//   [1] = byte_len (or element count for .tuple)
//   [2] = pointer (cast usize) to the byte buffer (allocated from chunk)
//
// In the Rust port nodes[2] is always 0 and the payload lives in the
// arena entry (see the parser module docs).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstObjectKind {
    Integer = 1,
    FloatOrImag = 2,
    LongString = 3,
    BytesLiteral = 4,
    /// Folded const tuple. Vendor MICROPY_COMP_CONST_TUPLE produces this
    /// when a tuple literal `(a, b, ...)` (or its testlist_star_expr /
    /// subscriptlist variants) has all const-foldable elements. The
    /// payload is NOT a byte buffer — instead we treat nodes[2] as a
    /// pointer to an array of N child Pn values (each itself a const
    /// node), and nodes[1] carries N. compile.zig's compileConstObject
    /// builds the corresponding mpy ConstObj at codegen time.
    Tuple = 5,
}

impl ConstObjectKind {
    pub fn from_u32(v: u32) -> Option<ConstObjectKind> {
        match v {
            1 => Some(ConstObjectKind::Integer),
            2 => Some(ConstObjectKind::FloatOrImag),
            3 => Some(ConstObjectKind::LongString),
            4 => Some(ConstObjectKind::BytesLiteral),
            5 => Some(ConstObjectKind::Tuple),
            _ => None,
        }
    }
}

/// Payload of a `RULE_const_object` node.
#[derive(Debug, Clone)]
pub(crate) enum ConstPayload {
    /// Raw literal bytes (integer / float / long-string / bytes).
    Bytes(Vec<u8>),
    /// Folded const tuple: the child `Pn` values, source order.
    Tuple(Vec<Pn>),
}
