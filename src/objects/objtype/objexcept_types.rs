//! The 28 ROM exception types (`objexcept.zig:513` onward).
//!
//! The Zig builds them as static `MpObjTypeFull` ROM structs via
//! two helpers — `rootExcType` (BaseException: make_new + print +
//! attr slots, no parent) and `excType` (derived: the same slots
//! plus a `parent` slot pointing at the immediate base) — all with
//! flags `0x0000` and slot order make_new=1, print=2, attr=3,
//! parent=4.
//!
//! In Rust the types are heap `TypeData` literals (the frozen
//! shape every lane uses): `make_new` is the type-call path, not
//! a `TypeData` slot (D5 — C12a/C13 wire `exception_make_new`);
//! the `attr` slot is C12a's (the field does not exist yet —
//! `exception_attr` is a standalone fn until then); the `parent`
//! slot is the `base: Option<GcRef>` field. `print` IS set
//! (`exception_print`).
//!
//! MRO: the reference binary's `ValueError.__mro__` is
//! `(ValueError, Exception, BaseException, object)` — every
//! exception MRO ends with `object` (probed 2026-11-17). The
//! builder takes the `object` type and hand-populates the linear
//! MROs (single inheritance: `[self] + parent.mro`).
//!
//! `StopAsyncIteration` (async-await on) and `UnicodeError`
//! (str-unicode on) exist in full config — both are included
//! unconditionally here, since this port targets full only.
//! `ViperTypeError` is absent (`MICROPY_EMIT_NATIVE = 0`).

use crate::memory::gc::CallCtx;
use crate::objects::objtype::objexcept::{exception_make_new_slot, exception_print};
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{DictData, GcRef, MapData, Obj, TypeData, Value};

/// (name, parent name) in Zig ROM order (`objexcept.zig:513-577`).
/// `None` parent = `BaseException` itself.
pub const EXCEPTION_HIERARCHY: &[(&str, Option<&str>)] = &[
    ("BaseException", None),
    ("SystemExit", Some("BaseException")),
    ("KeyboardInterrupt", Some("BaseException")),
    ("GeneratorExit", Some("BaseException")),
    ("Exception", Some("BaseException")),
    ("StopAsyncIteration", Some("Exception")),
    ("StopIteration", Some("Exception")),
    ("ArithmeticError", Some("Exception")),
    ("OverflowError", Some("ArithmeticError")),
    ("ZeroDivisionError", Some("ArithmeticError")),
    ("AssertionError", Some("Exception")),
    ("AttributeError", Some("Exception")),
    ("EOFError", Some("Exception")),
    ("ImportError", Some("Exception")),
    ("ModuleNotFoundError", Some("ImportError")),
    ("LookupError", Some("Exception")),
    ("IndexError", Some("LookupError")),
    ("KeyError", Some("LookupError")),
    ("MemoryError", Some("Exception")),
    ("NameError", Some("Exception")),
    ("OSError", Some("Exception")),
    ("RuntimeError", Some("Exception")),
    ("NotImplementedError", Some("RuntimeError")),
    ("SyntaxError", Some("Exception")),
    ("IndentationError", Some("SyntaxError")),
    ("TypeError", Some("Exception")),
    ("ValueError", Some("Exception")),
    ("UnicodeError", Some("ValueError")),
];

/// `rootExcType` / `excType` (`objexcept.zig:474/501`): one
/// exception type. `mro` is the caller-owned linear chain
/// (self first, ending with `object`); `dict` is the locals
/// dict (empty for the ROM types — exception types carry no
/// methods). `base` is the parent type (`None` only for
/// `BaseException` — whose parent is `object`, passed via `mro`).
pub fn exc_type_data(name: u32, base: Option<GcRef>, mro: Vec<GcRef>, dict: GcRef) -> TypeData {
    TypeData {
        name,
        flags: 0,
        base,
        mro,
        dict,
        attr: None,
        // The exception types' `make_new` slot IS
        // `mp_obj_exception_make_new` (objexcept.zig:316) — the
        // identity `is_exception_type_obj` compares against.
        make_new: Some(exception_make_new_slot),
        bases: None,
        namedtuple_fields: None,
        protocol: None,
        unary_op: None,
        binary_op: None,
        call: None,
        getiter: None,
        subscr: None,
        buffer: None,
        print: Some(exception_print),
    }
}

/// All 28 ROM exception types, by name — the C13 bootstrap
/// registry (C12a's ROM registry consumes this shape).
pub struct ExceptionTypes {
    pub base_exception: GcRef,
    pub system_exit: GcRef,
    pub keyboard_interrupt: GcRef,
    pub generator_exit: GcRef,
    pub exception: GcRef,
    pub stop_async_iteration: GcRef,
    pub stop_iteration: GcRef,
    pub arithmetic_error: GcRef,
    pub overflow_error: GcRef,
    pub zero_division_error: GcRef,
    pub assertion_error: GcRef,
    pub attribute_error: GcRef,
    pub eof_error: GcRef,
    pub import_error: GcRef,
    pub module_not_found_error: GcRef,
    pub lookup_error: GcRef,
    pub index_error: GcRef,
    pub key_error: GcRef,
    pub memory_error: GcRef,
    pub name_error: GcRef,
    pub os_error: GcRef,
    pub runtime_error: GcRef,
    pub not_implemented_error: GcRef,
    pub syntax_error: GcRef,
    pub indentation_error: GcRef,
    pub type_error: GcRef,
    pub value_error: GcRef,
    pub unicode_error: GcRef,
}

impl ExceptionTypes {
    /// Look a type up by its (static) name — the ROM-registry
    /// accessor C13's builtins table uses.
    pub fn by_name(&self, name: &str) -> Option<GcRef> {
        Some(match name {
            "BaseException" => self.base_exception,
            "SystemExit" => self.system_exit,
            "KeyboardInterrupt" => self.keyboard_interrupt,
            "GeneratorExit" => self.generator_exit,
            "Exception" => self.exception,
            "StopAsyncIteration" => self.stop_async_iteration,
            "StopIteration" => self.stop_iteration,
            "ArithmeticError" => self.arithmetic_error,
            "OverflowError" => self.overflow_error,
            "ZeroDivisionError" => self.zero_division_error,
            "AssertionError" => self.assertion_error,
            "AttributeError" => self.attribute_error,
            "EOFError" => self.eof_error,
            "ImportError" => self.import_error,
            "ModuleNotFoundError" => self.module_not_found_error,
            "LookupError" => self.lookup_error,
            "IndexError" => self.index_error,
            "KeyError" => self.key_error,
            "MemoryError" => self.memory_error,
            "NameError" => self.name_error,
            "OSError" => self.os_error,
            "RuntimeError" => self.runtime_error,
            "NotImplementedError" => self.not_implemented_error,
            "SyntaxError" => self.syntax_error,
            "IndentationError" => self.indentation_error,
            "TypeError" => self.type_error,
            "ValueError" => self.value_error,
            "UnicodeError" => self.unicode_error,
            _ => return None,
        })
    }

    /// Every type, in hierarchy order — for rooting.
    pub fn all(&self) -> [GcRef; 28] {
        [
            self.base_exception,
            self.system_exit,
            self.keyboard_interrupt,
            self.generator_exit,
            self.exception,
            self.stop_async_iteration,
            self.stop_iteration,
            self.arithmetic_error,
            self.overflow_error,
            self.zero_division_error,
            self.assertion_error,
            self.attribute_error,
            self.eof_error,
            self.import_error,
            self.module_not_found_error,
            self.lookup_error,
            self.index_error,
            self.key_error,
            self.memory_error,
            self.name_error,
            self.os_error,
            self.runtime_error,
            self.not_implemented_error,
            self.syntax_error,
            self.indentation_error,
            self.type_error,
            self.value_error,
            self.unicode_error,
        ]
    }
}

/// Build all 28 ROM exception types in hierarchy order (parents
/// first — `EXCEPTION_HIERARCHY` is topologically ordered).
/// `object` is the `object` type: every MRO ends with it.
/// Each type gets a fresh empty locals dict; every type and dict
/// is pushed into `ctx.roots` as it is allocated (the fixture
/// pattern — a GC during construction must not collect them).
/// The MRO is patched after allocation (`GcArena::get_mut` —
/// the self ref is only known once allocated).
pub fn build_exception_types(ctx: &mut CallCtx, object: GcRef) -> ExceptionTypes {
    let mut by_name: Vec<(String, GcRef, Vec<GcRef>)> = Vec::with_capacity(28);
    for (name, parent) in EXCEPTION_HIERARCHY {
        let id = static_qstr_id(name.as_bytes())
            .unwrap_or_else(|| panic!("{} is a static qstr", name));
        let dict = ctx.alloc(None, Obj::Dict(DictData { map: MapData::new() }));
        ctx.roots.push(Value::Heap(dict));
        let (base, parent_mro): (Option<GcRef>, Vec<GcRef>) = match parent {
            None => (Some(object), vec![object]),
            Some(p) => {
                let (_, pr, pm) = by_name
                    .iter()
                    .find(|(n, _, _)| n == p)
                    .unwrap_or_else(|| panic!("parent {} built before {}", p, name));
                (Some(*pr), pm.clone())
            }
        };
        let data = exc_type_data(id, base, parent_mro, dict);
        let t = ctx.alloc(None, Obj::Type(data));
        ctx.roots.push(Value::Heap(t));
        // Patch the MRO: [self] + parent chain.
        let mut mro = vec![t];
        if let Obj::Type(td) = ctx.arena.get_mut(t) {
            mro.append(&mut td.mro);
            td.mro = mro;
        }
        let mro = match ctx.arena.get(t) {
            Obj::Type(td) => td.mro.clone(),
            _ => unreachable!("just-allocated type"),
        };
        by_name.push((name.to_string(), t, mro));
    }
    let get = |n: &str| -> GcRef {
        by_name
            .iter()
            .find(|(name, _, _)| name == n)
            .map(|(_, t, _)| *t)
            .unwrap_or_else(|| panic!("exception type {} built", n))
    };
    ExceptionTypes {
        base_exception: get("BaseException"),
        system_exit: get("SystemExit"),
        keyboard_interrupt: get("KeyboardInterrupt"),
        generator_exit: get("GeneratorExit"),
        exception: get("Exception"),
        stop_async_iteration: get("StopAsyncIteration"),
        stop_iteration: get("StopIteration"),
        arithmetic_error: get("ArithmeticError"),
        overflow_error: get("OverflowError"),
        zero_division_error: get("ZeroDivisionError"),
        assertion_error: get("AssertionError"),
        attribute_error: get("AttributeError"),
        eof_error: get("EOFError"),
        import_error: get("ImportError"),
        module_not_found_error: get("ModuleNotFoundError"),
        lookup_error: get("LookupError"),
        index_error: get("IndexError"),
        key_error: get("KeyError"),
        memory_error: get("MemoryError"),
        name_error: get("NameError"),
        os_error: get("OSError"),
        runtime_error: get("RuntimeError"),
        not_implemented_error: get("NotImplementedError"),
        syntax_error: get("SyntaxError"),
        indentation_error: get("IndentationError"),
        type_error: get("TypeError"),
        value_error: get("ValueError"),
        unicode_error: get("UnicodeError"),
    }
}

/// `mp_const_GeneratorExit_obj` — the `GeneratorExit` singleton
/// instance (C10b item 4): a no-arg `GeneratorExit` exception.
/// C13 passes it to `gen_instance_close`; it is a plain instance
/// (fresh per call — identity with the Zig's ROM singleton is
/// C12a's ROM-singleton work, same class as the empty tuple).
pub fn generator_exit_instance(ctx: &mut CallCtx, generator_exit: GcRef) -> Value {
    crate::objects::objtype::objexcept::new_exception(ctx, generator_exit)
}
