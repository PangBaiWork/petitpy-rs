//! `ConstObj` — discriminated-union value type for the const-obj table.
//!
//! Port of `ConstObj` in `src/compiler/emit.zig`. The `.mpy` writer
//! (`mpy.rs`) consumes these via `to_obj_entry` to produce its
//! `ObjEntry` tags. Deduplicated in `EmitCommon::use_const_obj` by
//! `strictly_equal`, which preserves vendor's signbit-aware ±0.0
//! distinction and recursive tuple equality (vendor
//! `emitcommon.c:strictly_equal`).
//!
//! ## Divergence: owned payloads
//!
//! Zig's `ConstObj` borrows byte slices (`[]const u8`) — either from
//! the parse tree's chunk (lifetime = `ParseTree`) or from the
//! compiler's allocator. A Rust `Compiler` that owns both the tree and
//! the `EmitCommon` cannot hold such borrows without self-reference,
//! so this port stores owned `Vec<u8>` / `Vec<ConstObj>` payloads.
//! The copies are small (constant literals) and the observable output
//! is unchanged: `to_obj_entry` produces the identical `ObjEntry`.

use crate::compiler::mpy::ObjEntry;

/// A constant-table object. Payloads are owned (see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstObj {
    None,
    BoolTrue,
    BoolFalse,
    Ellipsis,
    /// Decimal-text big int (already past the small-int range).
    IntBig(Vec<u8>),
    Str(Vec<u8>),
    Bytes(Vec<u8>),
    /// Text repr of a float; preserves sign for ±0 distinction.
    Float(Vec<u8>),
    Complex(Vec<u8>),
    Tuple(Vec<ConstObj>),
}

impl ConstObj {
    /// Vendor `strictly_equal`: matches type, value, AND ±0.0 sign for
    /// floats. Tuples recurse. Note: True != 1.0 (different types).
    pub fn strictly_equal(a: &ConstObj, b: &ConstObj) -> bool {
        match (a, b) {
            (ConstObj::None, ConstObj::None)
            | (ConstObj::BoolTrue, ConstObj::BoolTrue)
            | (ConstObj::BoolFalse, ConstObj::BoolFalse)
            | (ConstObj::Ellipsis, ConstObj::Ellipsis) => true,
            (ConstObj::IntBig(x), ConstObj::IntBig(y))
            | (ConstObj::Str(x), ConstObj::Str(y))
            | (ConstObj::Bytes(x), ConstObj::Bytes(y))
            | (ConstObj::Float(x), ConstObj::Float(y))
            | (ConstObj::Complex(x), ConstObj::Complex(y)) => x == y,
            (ConstObj::Tuple(xs), ConstObj::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs.iter().zip(ys.iter()).all(|(x, y)| Self::strictly_equal(x, y))
            }
            _ => false,
        }
    }

    /// Convert to the `mpy::ObjEntry` shape the writer expects.
    /// (Zig: `toObjEntry`, which borrows; here the payloads are cloned
    /// into the owned `ObjEntry`.)
    pub fn to_obj_entry(&self) -> ObjEntry {
        match self {
            ConstObj::None => ObjEntry::None,
            ConstObj::BoolTrue => ObjEntry::True,
            ConstObj::BoolFalse => ObjEntry::False,
            ConstObj::Ellipsis => ObjEntry::Ellipsis,
            ConstObj::IntBig(b) => ObjEntry::Int(b.clone()),
            ConstObj::Str(b) => ObjEntry::Str(b.clone()),
            ConstObj::Bytes(b) => ObjEntry::Bytes(b.clone()),
            ConstObj::Float(b) => ObjEntry::Float(b.clone()),
            ConstObj::Complex(b) => ObjEntry::Complex(b.clone()),
            ConstObj::Tuple(items) => {
                ObjEntry::Tuple(items.iter().map(|it| it.to_obj_entry()).collect())
            }
        }
    }
}
