//! Round-trip a .mpy file: read it, write it back, compare bytes.
//!
//! Port of `src/compiler/mpy_roundtrip.zig` (96 lines). The Zig file is a
//! CLI program (`zigpy-mpy-roundtrip <input.mpy>`, exit 0 on match, 1 on
//! mismatch, 2 on read error); the check logic lives here as a library
//! function and the CLI itself is `examples/mpy-roundtrip.rs`, which
//! reproduces the Zig CLI's messages and exit codes exactly.
//!
//! Known format edge (inherent to the format, identical in Zig): a raw
//! code header with the `has_children` bit set but `n_children == 0`
//! parses to an empty `children` list, and the writer derives
//! `has_children` from `children.len() > 0`, so the re-serialized bytes
//! differ (the `has_children` bit is dropped). The compiler never emits
//! such a header; see the `mismatch_on_empty_children_header` test,
//! which also cross-checks the Zig round-trip tool on the same bytes.

use super::mpy::{self, MpyError};

/// Outcome of a round-trip check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Roundtrip {
    /// Read -> write reproduced the input bytes exactly.
    Match {
        bytes: usize,
    },
    /// Read -> write differs from the input.
    Mismatch {
        in_len: usize,
        out_len: usize,
        /// Byte offset of the first differing byte, if the two share a
        /// common prefix shorter than both.
        first_diff: Option<usize>,
    },
}

/// Read `data` as a .mpy module, write it back, compare with the input.
/// Returns the check outcome; a read failure is returned as `Err`
/// (the CLI maps it to exit code 2, mirroring the Zig CLI).
pub fn check(data: &[u8]) -> Result<Roundtrip, MpyError> {
    let m = mpy::read(data)?;
    let out = mpy::write(&m);
    if out == data {
        Ok(Roundtrip::Match { bytes: data.len() })
    } else {
        Ok(Roundtrip::Mismatch {
            in_len: data.len(),
            out_len: out.len(),
            first_diff: first_diff(data, &out),
        })
    }
}

/// Offset of the first byte where `a` and `b` differ, or `None` if one
/// is a prefix of the other.
pub fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    let min = a.len().min(b.len());
    for i in 0..min {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::mpy::tests::frozen_files;
    use super::*;
    use mpy::{Module, ObjEntry, QstrEntry, RawCode, MPY_MAGIC, MPY_VERSION};

    #[test]
    fn first_diff_vectors() {
        assert_eq!(first_diff(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(first_diff(&[1, 2, 3], &[1, 2, 4]), Some(2));
        assert_eq!(first_diff(&[1, 2], &[1, 2, 3]), None);
        assert_eq!(first_diff(&[], &[1]), None);
        assert_eq!(first_diff(&[9], &[1]), Some(0));
    }

    #[test]
    fn check_matches_all_frozen_files() {
        // Same coverage as mpy::frozen_files_round_trip_byte_identical,
        // exercised through the check() API (the Zig CLI's code path).
        for f in frozen_files() {
            let data = std::fs::read(&f).unwrap();
            assert_eq!(
                check(&data),
                Ok(Roundtrip::Match { bytes: data.len() }),
                "check() mismatch on {}",
                f.display()
            );
        }
    }

    #[test]
    fn check_returns_read_error() {
        assert_eq!(check(&[]), Err(MpyError::UnexpectedEof));
        assert_eq!(
            check(&[b'X', MPY_VERSION, 0, 31]),
            Err(MpyError::InvalidMagic)
        );
    }

    #[test]
    fn mismatch_on_empty_children_header() {
        // Raw-code header 0x04 = has_children bit set, kind offset 0,
        // fun_data_len 0; followed by n_children = 0. The reader yields
        // an empty children list; the writer drops the has_children bit,
        // so the output is one byte shorter. This is a property of the
        // format as the Zig writer implements it too (it derives
        // has_children from children.len() > 0) — see
        // scripts/check_mpy_write.sh, which runs the Zig round-trip
        // tool on these same bytes and expects MISMATCH as well.
        let data = [MPY_MAGIC, MPY_VERSION, 0, 31, 0, 0, 0x04, 0x00];
        assert_eq!(
            check(&data),
            Ok(Roundtrip::Mismatch {
                in_len: 8,
                out_len: 7,
                first_diff: Some(6),
            })
        );
        // Sanity: the parsed module is exactly what the bytes say.
        let m = mpy::read(&data).unwrap();
        assert_eq!(m.rc.kind, mpy::MP_CODE_BYTECODE);
        assert!(m.rc.children.is_empty());
    }

    #[test]
    fn check_matches_hand_built_module() {
        // A Module built in Rust round-trips through check() (exercises
        // the writer side of the check path with all obj kinds).
        let m = Module {
            version: MPY_VERSION,
            feature_flags: 0,
            small_int_bits: 31,
            arch_flags: None,
            qstrs: vec![QstrEntry::Static(1), QstrEntry::New(b"x".to_vec())],
            objs: vec![ObjEntry::Tuple(vec![ObjEntry::None])],
            rc: RawCode {
                kind: mpy::MP_CODE_BYTECODE,
                fun_data: vec![0xaa],
                children: vec![],
            },
        };
        let bytes = mpy::write(&m);
        assert_eq!(check(&bytes), Ok(Roundtrip::Match { bytes: bytes.len() }));
    }
}
