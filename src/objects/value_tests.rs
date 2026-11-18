//! Tests for `value.rs` (C0): small-int window, immediates
//! identity. Split out of `value.rs` by D5 (the file was over
//! the 800-line cap after the `Obj::Re` / `Obj::Match` payloads
//! landed); the tests themselves are unchanged.

use crate::objects::value::*;
    use crate::memory::gc::{GcArena, RootSet};

    #[test]
    fn small_int_window_boundaries() {
        // Mirrors mp_mirrors.zig:548-551.
        assert_eq!(SMALL_INT_MAX, (1i64 << 62) - 1);
        assert_eq!(SMALL_INT_MIN, -(1i64 << 62));
        assert!(small_int_fits(0));
        assert!(small_int_fits(SMALL_INT_MAX));
        assert!(small_int_fits(SMALL_INT_MIN));
        assert!(!small_int_fits(SMALL_INT_MAX + 1));
        assert!(!small_int_fits(SMALL_INT_MIN - 1));
        assert_eq!(Value::small_int(5), Some(Value::SmallInt(5)));
        assert_eq!(Value::small_int(1i64 << 62), None);
    }

    #[test]
    fn immediates_identity() {
        // Mirrors the OBJ_* constants (mp_mirrors.zig:492-497).
        assert_eq!(Value::none(), Value::None);
        assert_eq!(Value::new_bool(true), Value::Bool(true));
        assert_ne!(Value::new_bool(true), Value::new_bool(false));
        assert_ne!(Value::none(), Value::new_bool(false));
        assert_eq!(Value::qstr(42), Value::Qstr(42));
        assert!(Value::Qstr(7).is_qstr());
        assert!(Value::SmallInt(-3).is_small_int());
        assert!(Value::None.is_none());
        assert!(!Value::SmallInt(0).is_none());
    }

    #[test]
    fn int_out_of_window_goes_to_heap() {
        let mut arena = GcArena::new();
        let roots = RootSet::new();
        let big = 1i64 << 62;
        let v = Value::int(&mut arena, None, &roots, big);
        assert!(v.is_heap());
        match arena.get(v.as_heap().unwrap()) {
            // 2^62 fits in i64 (just outside the 62-bit small-int window).
            Obj::Int(z) => assert_eq!(z.as_int_checked(), Some(1i64 << 62)),
            other => panic!("expected Int, got {:?}", other),
        }
        let small = Value::int(&mut arena, None, &roots, 7);
        assert_eq!(small, Value::SmallInt(7));
    }

    #[test]
    fn truth_table_primitives() {
        let arena = GcArena::new();
        assert!(!Value::None.is_true(&arena).unwrap());
        assert!(!Value::Bool(false).is_true(&arena).unwrap());
        assert!(Value::Bool(true).is_true(&arena).unwrap());
        assert!(!Value::SmallInt(0).is_true(&arena).unwrap());
        assert!(Value::SmallInt(-1).is_true(&arena).unwrap());
        // Qstr(0) is MP_QSTRnull and Qstr(1) is MP_QSTR_ — the
        // generated table holds "" at both ids (LENGTHS[0] ==
        // LENGTHS[1] == 0), so both are falsy, like bool("") on the
        // reference binary; a non-empty static qstr is truthy.
        // (C6 corrects the old assertion, which was written against
        // the pre-fix arm that returned true for every qstr.)
        assert!(!Value::Qstr(0).is_true(&arena).unwrap());
        assert!(!Value::Qstr(1).is_true(&arena).unwrap());
        let int_id = crate::qstr_table::QSTR_STRINGS
            .iter()
            .position(|s| *s == b"int")
            .unwrap() as u32;
        assert!(Value::Qstr(int_id).is_true(&arena).unwrap());
    }
