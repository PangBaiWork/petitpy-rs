//! Tests for Lane D5: the native `re` / `ure` module
//! (`modre.c` binding over the RE15 engine) and the frozen
//! petitpy-lib bundle.
//!
//! All tests are end-to-end through a real `Vm` (`Vm::new`
//! registers the D5 modules — the `modsocket_tests.rs` /
//! `modd4_tests.rs` `Fx` pattern): run a script, read its
//! globals back, or assert the raised exception. Reference
//! behaviour for every assertion was probed on the reference
//! binary 2026-11-18 (see `scripts/d5_result.txt` §2).

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

/// A fresh VM (D5 modules registered by `Vm::new`) plus
/// the read-back helpers (the `modsocket_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        Self { vm: Vm::new() }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => self.raised_parts(exc),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn raised_parts(&mut self, exc: Value) -> (String, String) {
        let (name, arg) = match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => {
                    let t = d.type_;
                    let name = match self.vm.arena.get(t) {
                        Obj::Type(td) => String::from_utf8_lossy(
                            self.vm.pool.qstr_str(td.name as u16),
                        )
                        .into_owned(),
                        _ => panic!("exception type is not a type"),
                    };
                    (name, d.args.first().copied())
                }
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        };
        let msg = match arg {
            Some(v) => self.val_str(v),
            None => String::new(),
        };
        (name, msg)
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String`.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                Obj::Bytes(b) => String::from_utf8_lossy(&b.bytes).into_owned(),
                Obj::Float(f) => f.to_string(),
                Obj::Tuple(t) => format!("({})", t.len()),
                Obj::List(l) => format!("[{}]", l.items.len()),
                other => panic!("value is not a str: {other:?}"),
            },
            Value::SmallInt(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::None => "None".to_string(),
        }
    }

    fn g_int(&mut self, name: &str) -> i64 {
        match self.g(name) {
            Some(Value::SmallInt(n)) => n,
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Int(i) => i.as_int_checked().expect("test int fits i64"),
                other => panic!("global {name} is not an int: {other:?}"),
            },
            other => panic!("global {name} is not an int: {other:?}"),
        }
    }

    fn g_bool(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    fn g_str(&mut self, name: &str) -> String {
        match self.g(name) {
            Some(v) => self.val_str(v),
            None => panic!("global {name} is absent"),
        }
    }
}

// ── native `ure`: compile / match / search ──────────────────────────

#[test]
fn d5_re_compile_match() {
    let mut fx = Fx::new();
    // `match` is ANCHORED at position 0: pattern "a" on
    // "banana" is None on the reference (the "a" is at index
    // 1). Use pattern "b" so the match lands at (0, 1).
    fx.run_ok(
        "import ure\n\
         m = ure.compile(\"b\").match(\"banana\")\n\
         a = m.group(0)\n\
         b = m.start()\n\
         c = m.end()\n\
         d = str(m)\n",
    );
    assert_eq!(fx.g_str("a"), "b");
    assert_eq!(fx.g_int("b"), 0);
    assert_eq!(fx.g_int("c"), 1);
    assert_eq!(fx.g_str("d"), "<match num=1>");
}

#[test]
fn d5_re_search() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.search(\"na\", \"banana\")\n\
         a = m.group(0)\n\
         b = m.start()\n\
         c = m.end()\n",
    );
    assert_eq!(fx.g_str("a"), "na");
    assert_eq!(fx.g_int("b"), 2);
    assert_eq!(fx.g_int("c"), 4);
}

#[test]
fn d5_re_match_uncompiled_ignores_pos() {
    // The `was_compiled` branch: an uncompiled pattern
    // IGNORES pos/endpos (probed: `ure.match("a", "ba", 1)`
    // is None, while `ure.search("a", "ba", 1)` matches).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.match(\"a\", \"ba\", 1) is None\n\
         b = ure.search(\"a\", \"ba\", 1) is not None\n",
    );
    assert!(fx.g_bool("a"));
    assert!(fx.g_bool("b"));
}

#[test]
fn d5_re_match_pos_endpos() {
    // The C's clamping, probed 2026-11-18: pos is clamped to
    // [0, len], endpos to [pos, len].
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         p = ure.compile(\"a\")\n\
         a = p.match(\"ba\", 1) is not None\n\
         b = p.match(\"ba\", 1, 2) is not None\n\
         c = p.match(\"ba\", 1, 1) is None\n\
         d = p.search(\"ba\", 1) is not None\n\
         e = p.match(\"ba\", -1) is None\n\
         f = p.match(\"ba\", 99) is None\n\
         g = p.search(\"ba\", -5) is not None\n\
         h = p.match(\"ba\", 0) is None\n",
    );
    for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        assert!(fx.g_bool(name), "{name} failed");
    }
}

#[test]
fn d5_re_match_no_match() {
    let mut fx = Fx::new();
    fx.run_ok("import ure\na = ure.compile(\"a\").match(\"b\") is None\n");
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_re_groups() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.compile(\"(a)(b)?\").search(\"a\")\n\
         a = m.group(0)\n\
         b = m.group(1)\n\
         c = m.group(2) is None\n\
         d = len(m.groups())\n\
         e = m.groups()[0]\n",
    );
    assert_eq!(fx.g_str("a"), "a");
    assert_eq!(fx.g_str("b"), "a");
    assert!(fx.g_bool("c"));
    assert_eq!(fx.g_int("d"), 2);
    assert_eq!(fx.g_str("e"), "a");
}

#[test]
fn d5_re_groups_empty() {
    // No capture groups: `groups()` is the empty tuple.
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.compile(\"a\").match(\"a\")\n\
         a = m.groups() == ()\n",
    );
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_re_span_group() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.compile(\"(a)(b)\").search(\"xab\")\n\
         s = m.span(2)\n\
         a = s[0]\n\
         b = s[1]\n\
         t = m.span()\n\
         d = t[0]\n\
         e = t[1]\n\
         f = m.start(1)\n\
         g = m.end(2)\n",
    );
    assert_eq!(fx.g_int("a"), 2);
    assert_eq!(fx.g_int("b"), 3);
    assert_eq!(fx.g_int("d"), 1);
    assert_eq!(fx.g_int("e"), 3);
    assert_eq!(fx.g_int("f"), 1);
    assert_eq!(fx.g_int("g"), 3);
}

#[test]
fn d5_re_span_no_participate() {
    // A non-participating group spans (-1, -1) (probed).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.compile(\"(a)|(b)\").search(\"b\")\n\
         s = m.span(1)\n\
         a = s[0]\n\
         b = s[1]\n\
         c = m.start(1)\n",
    );
    assert_eq!(fx.g_int("a"), -1);
    assert_eq!(fx.g_int("b"), -1);
    assert_eq!(fx.g_int("c"), -1);
}

#[test]
fn d5_re_group_out_of_range() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"a\").match(\"a\").group(5)\n"),
        ("IndexError".to_string(), "5".to_string())
    );
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"a\").match(\"a\").group(-1)\n"),
        ("IndexError".to_string(), "-1".to_string())
    );
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"a\").match(\"a\").group(\"x\")\n"),
        (
            "TypeError".to_string(),
            "can't convert to int".to_string()
        )
    );
}

#[test]
fn d5_re_unicode_span() {
    // Spans are CHAR indices for a str subject (probed:
    // `search("é")` on `"aéb"` spans (1, 2)).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.search(\"é\", \"aéb\")\n\
         a = m.group(0)\n\
         b = m.start()\n\
         c = m.end()\n",
    );
    assert_eq!(fx.g_str("a"), "é");
    assert_eq!(fx.g_int("b"), 1);
    assert_eq!(fx.g_int("c"), 2);
}

#[test]
fn d5_re_bytes() {
    // A bytes subject: group results are bytes, spans are
    // BYTE indices.
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         m = ure.search(b\"a\", b\"banana\")\n\
         a = m.group(0)\n\
         b = m.start()\n\
         c = m.end()\n",
    );
    assert_eq!(fx.g_str("a"), "a");
    assert_eq!(fx.g_int("b"), 1);
    assert_eq!(fx.g_int("c"), 2);
}

#[test]
fn d5_re_compile_bad() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"[\")\n"),
        ("ValueError".to_string(), "error in regex".to_string())
    );
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"a(\")\n"),
        ("ValueError".to_string(), "error in regex".to_string())
    );
}

#[test]
fn d5_re_compile_flags_ignored() {
    // The 2nd arg (flags) is accepted and IGNORED
    // (`MICROPY_PY_RE_DEBUG` is off — probed).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.compile(\"a\", 2).match(\"a\") is not None\n",
    );
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_re_no_split_no_debug() {
    // `split` is a compiled-pattern method only and there is
    // NO `DEBUG` constant on the native module (probed).
    let mut fx = Fx::new();
    let (t, _) = fx.raised_msg("import ure\nure.split\n");
    assert_eq!(t, "AttributeError");
    let (t, _) = fx.raised_msg("import ure\nure.DEBUG\n");
    assert_eq!(t, "AttributeError");
}

#[test]
fn d5_re_names() {
    // `ure` is the same module object as native `re`
    // (`ure.__name__` is `'re'`), but `import re` loads the
    // FROZEN shim — a DIFFERENT module object (probed).
    let mut fx = Fx::new();
    fx.run_ok(
        "import re\n\
         import ure\n\
         a = ure.__name__ == \"re\"\n\
         b = re.__name__ == \"re\"\n\
         c = re is ure\n",
    );
    assert!(fx.g_bool("a"));
    assert!(fx.g_bool("b"));
    assert!(!fx.g_bool("c"));
}

#[test]
fn d5_re_print() {
    // `<match num=N>` is exact; `<re ...>` prints the arena
    // slot (the C prints the ASLR-random raw pointer).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = str(ure.compile(\"(a)\").search(\"a\"))\n\
         b = str(ure.compile(\"a\"))\n",
    );
    assert_eq!(fx.g_str("a"), "<match num=2>");
    assert!(fx.g_str("b").starts_with("<re 0x"));
}

// ── native `ure`: split ─────────────────────────────────────────────

#[test]
fn d5_re_split() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = \"|\".join(ure.compile(\"a\").split(\"banana\"))\n\
         b = \"|\".join(ure.compile(\"a\").split(\"banana\", 1))\n\
         c = \"|\".join(ure.compile(\"x\").split(\"abc\"))\n\
         d = \"|\".join(ure.compile(\"\").split(\"abc\"))\n\
         e = \"|\".join(ure.compile(\"a\").split(\"ba\"))\n",
    );
    assert_eq!(fx.g_str("a"), "b|n|n|");
    assert_eq!(fx.g_str("b"), "b|nana");
    assert_eq!(fx.g_str("c"), "abc");
    assert_eq!(fx.g_str("d"), "abc");
    assert_eq!(fx.g_str("e"), "b|");
}

#[test]
fn d5_re_split_subcaptures() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"(a)\").split(\"banana\")\n"),
        (
            "NotImplementedError".to_string(),
            "splitting with sub-captures".to_string()
        )
    );
}

#[test]
fn d5_re_split_maxsplit_type() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(\"a\").split(\"banana\", \"x\")\n"),
        (
            "TypeError".to_string(),
            "can't convert to int".to_string()
        )
    );
}

// ── native `ure`: sub ───────────────────────────────────────────────

#[test]
fn d5_re_sub_basic() {
    // `count` defaults to 0 (unlimited); `count=1` replaces
    // one; no match returns the subject itself.
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.sub(\"a\", \"X\", \"banana\")\n\
         b = ure.sub(\"a\", \"X\", \"banana\", 1)\n\
         c = ure.sub(\"a\", \"X\", \"banana\", 0)\n\
         d = ure.sub(\"z\", \"X\", \"banana\")\n",
    );
    assert_eq!(fx.g_str("a"), "bXnXnX");
    assert_eq!(fx.g_str("b"), "bXnana");
    assert_eq!(fx.g_str("c"), "bXnXnX");
    assert_eq!(fx.g_str("d"), "banana");
}

#[test]
fn d5_re_sub_no_match_same_object() {
    // No substitution at all: the SUBJECT ITSELF is returned
    // (the C's optimisation — probed).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         s = \"banana\"\n\
         t = ure.sub(\"z\", \"X\", s)\n\
         a = t is s\n",
    );
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_re_sub_groups() {
    // `\g<2>\1` substitutes group 2 then group 1 (probed:
    // `sub("(a)(n)", "\\g<2>\\1", "banana")` is `'bnanaa'`).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.sub(\"(a)(n)\", \"\\\\g<2>\\\\1\", \"banana\")\n",
    );
    assert_eq!(fx.g_str("a"), "bnanaa");
}

#[test]
fn d5_re_sub_callable() {
    // A callable replacement is called with the match object
    // — the intercepted path (`call_sub_ctx`).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.sub(\"a\", lambda m: m.group(0).upper(), \"banana\")\n",
    );
    assert_eq!(fx.g_str("a"), "bAnAnA");
}

#[test]
fn d5_re_sub_missing_group() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.sub(\"(a)\", \"\\\\2\", \"a\")\n"),
        ("IndexError".to_string(), "2".to_string())
    );
}

#[test]
fn d5_re_sub_backslash() {
    // `\\` in the replacement is one literal backslash.
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         r = ure.sub(\"a\", \"\\\\\\\\\", \"banana\")\n\
         a = len(r)\n\
         b = r[1] == \"\\\\\"\n",
    );
    assert_eq!(fx.g_int("a"), 6);
    assert!(fx.g_bool("b"));
}

#[test]
fn d5_re_sub_type_errors() {
    // The reference SEGFAULTS on all three (NULL derefs in
    // `_compilecode` / `re_sub_helper`, rc=134, probed) —
    // Rust raises `TypeError("can't convert to str
    // implicitly")` instead (documented divergence).
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import ure\nure.sub(\"a\", 5, \"banana\")\n"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import ure\nure.compile(123)\n"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import ure\nure.search(\"a\", 123)\n"),
        (
            "TypeError".to_string(),
            "can't convert to str implicitly".to_string()
        )
    );
}

#[test]
fn d5_re_sub_method_form() {
    // The method form goes through the SAME interception
    // (the module-level and method-level `sub` are one
    // `FunBuiltin`).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.compile(\"a\").sub(\"X\", \"banana\")\n\
         b = ure.compile(\"a\").sub(\"X\", \"banana\", 1)\n",
    );
    assert_eq!(fx.g_str("a"), "bXnXnX");
    assert_eq!(fx.g_str("b"), "bXnana");
}

#[test]
fn d5_re_sub_bytes() {
    // A bytes subject: the result is bytes.
    let mut fx = Fx::new();
    fx.run_ok(
        "import ure\n\
         a = ure.sub(b\"a\", b\"X\", b\"banana\")\n",
    );
    assert_eq!(fx.g_str("a"), "bXnXnX");
}
