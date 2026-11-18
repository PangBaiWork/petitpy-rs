//! Frozen-bundle + D5 regression tests (split from `modre_tests.rs`
//! at the 800-line cap): frozen module imports (`import json`,
//! `import collections`, the `re` shim, enum/pathlib/shutil/uuid,
//! ipaddress), the `#[ignore]`d FunBc-`__init__` cases, and the
//! regression tests for the two master bugs D5 fixed (`*args` /
//! `**kw` header types in `setup_code_state`, the missing `reversed`
//! builtin). The `Fx` harness is a copy of `modre_tests.rs`'s
//! (one harness per test file, the `modd4_tests.rs` precedent).

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

// ── frozen bundle ───────────────────────────────────────────────────

#[test]
fn d5_frozen_json() {
    // The frozen `json.py` layer over the registered `_json`
    // native module.
    let mut fx = Fx::new();
    fx.run_ok(
        "import json\n\
         a = json.loads('{\"k\": 1}')[\"k\"]\n\
         b = json.dumps({\"k\": 1})\n\
         c = json.dumps([1, \"a\"])\n",
    );
    assert_eq!(fx.g_int("a"), 1);
    assert_eq!(fx.g_str("b"), "{\"k\": 1}");
    assert_eq!(fx.g_str("c"), "[1, \"a\"]");
}

#[test]
fn d5_frozen_heapq_bisect() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import heapq\n\
         h = [3, 1, 2]\n\
         heapq.heapify(h)\n\
         a = heapq.heappop(h)\n\
         import bisect\n\
         b = bisect.bisect([1, 3, 5], 4)\n",
    );
    assert_eq!(fx.g_int("a"), 1);
    assert_eq!(fx.g_int("b"), 2);
}

#[test]
fn d5_frozen_functools_string() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import functools\n\
         a = functools.reduce(lambda x, y: x + y, [1, 2, 3])\n\
         import string\n\
         b = string.ascii_lowercase[:3]\n",
    );
    assert_eq!(fx.g_int("a"), 6);
    assert_eq!(fx.g_str("b"), "abc");
}

#[test]
fn d5_frozen_collections_abc() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections.abc import Mapping\n\
         a = isinstance({}, Mapping)\n\
         b = isinstance([], Mapping)\n\
         import collections.abc\n\
         c = hasattr(collections.abc, \"Mapping\")\n",
    );
    // The frozen abc's `Mapping.__subclasshook__` is loose:
    // `isinstance([], Mapping)` is True on the reference
    // (probed 2026-11-18), as is `isinstance({}, Mapping)`.
    assert!(fx.g_bool("a"));
    assert!(fx.g_bool("b"));
    assert!(fx.g_bool("c"));
}

#[test]
fn d5_frozen_importlib_metadata() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import importlib.metadata\n\
         a = hasattr(importlib.metadata, \"version\")\n",
    );
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_frozen_ipaddress() {
    // Import + attribute only: `ip_network(...)` instantiates the
    // Python `IPv4Network` class, whose `__init__` is a FunBc —
    // the pre-existing stub-seam gap (see the ignored test below).
    let mut fx = Fx::new();
    fx.run_ok(
        "import ipaddress\n\
         a = hasattr(ipaddress, \"ip_network\")\n",
    );
    assert!(fx.g_bool("a"));
}

#[test]
fn d5_frozen_ipaddress_ignored() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import ipaddress\n\
         a = str(ipaddress.ip_network(\"10.0.0.0/8\", strict=False))\n",
    );
    assert_eq!(fx.g_str("a"), "10.0.0.0/8");
}

#[test]
fn d5_frozen_enum_pathlib_shutil_uuid() {
    // `typing` and `datetime` are NOT here: both instantiate a
    // Python class at import time (`_Subscriptable("Any")`,
    // `timezone.utc = timezone(...)`), which hits the FunBc
    // `__init__` stub-seam gap — see the ignored test below.
    let mut fx = Fx::new();
    fx.run_ok(
        "import enum\n\
         a = hasattr(enum, \"Enum\")\n\
         import pathlib\n\
         b = hasattr(pathlib, \"Path\")\n\
         import shutil\n\
         c = hasattr(shutil, \"copy\")\n\
         import uuid\n\
         d = hasattr(uuid, \"uuid4\")\n",
    );
    for name in ["a", "b", "c", "d"] {
        assert!(fx.g_bool(name), "{name} failed");
    }
}

#[test]
fn d5_frozen_typing_datetime_ignored() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import typing\n\
         a = hasattr(typing, \"Optional\")\n\
         import datetime\n\
         b = hasattr(datetime, \"datetime\")\n",
    );
    assert!(fx.g_bool("a"));
    assert!(fx.g_bool("b"));
}

#[test]
fn d5_frozen_re_constants() {
    // The frozen `re` shim loads (`import ure as _ure` +
    // constants) — only CALLING it needs `_CompiledPattern`
    // (see the ignored test below).
    let mut fx = Fx::new();
    fx.run_ok(
        "import re\n\
         a = re.IGNORECASE\n\
         b = re.DEBUG\n\
         c = re.MULTILINE\n",
    );
    assert_eq!(fx.g_int("a"), 2);
    assert_eq!(fx.g_int("b"), 0);
    assert_eq!(fx.g_int("c"), 0);
}

#[test]
fn d5_frozen_js_error() {
    // `import js` re-raises the ADR-006 ImportError (the
    // frozen js.py catches the failed `_petitpy_js` import
    // — `ModuleNotFoundError` IS an `ImportError` subclass).
    let mut fx = Fx::new();
    let (t, m) = fx.raised_msg("import js\n");
    assert_eq!(t, "ImportError");
    assert!(
        m.contains("no host bridge available"),
        "unexpected js ImportError message: {m}"
    );
}

#[test]
fn d5_frozen_subprocess_thread() {
    // The frozen `subprocess.py` layer over the registered
    // `_subprocess` native module; `_thread` loads standalone.
    let mut fx = Fx::new();
    fx.run_ok(
        "import subprocess\n\
         a = hasattr(subprocess, \"run\")\n\
         import _thread\n\
         b = _thread.__name__ == \"_thread\"\n",
    );
    assert!(fx.g_bool("a"));
    assert!(fx.g_bool("b"));
}

#[test]
fn d5_frozen_sys_modules() {
    // Frozen modules register in `sys.modules` (C15b's
    // `load_frozen`); builtin/extensible modules do NOT
    // (D2's finding — `import ure` leaves it empty).
    let mut fx = Fx::new();
    fx.run_ok(
        "import sys\n\
         import json\n\
         a = sys.modules[\"json\"] is json\n",
    );
    assert!(fx.g_bool("a"));
}

// ── ignored: blocked by the pre-existing FunBc `__init__` gap ──────
// A Python class's `__init__` goes through stub seams in
// `instance_make_new` (`run_init` → `seams.call_pair` raises
// bare `PyRaise` for a `FunBc` callee — the C13b-class gap,
// unscheduled; owner: Phase E VM-seam runner work). The
// frozen bundle exposes it for every Python class in
// petitpy-lib (`_CompiledPattern`, `deque`, `Counter`,
// `defaultdict`, `itertools.chain`, ...). These tests assert
// the DESIRED (reference) behaviour and are ignored until
// the gap is fixed — they must NOT be deleted.

#[test]
fn d5_frozen_re_compile_ignored() {
    let mut fx = Fx::new();
    fx.run_ok(
        "import re\n\
         m = re.compile(\"a\", re.IGNORECASE).match(\"A\")\n\
         a = m.group(0)\n",
    );
    assert_eq!(fx.g_str("a"), "A");
}

#[test]
#[ignore]
fn d5_frozen_deque_ignored() {
    let mut fx = Fx::new();
    fx.run_ok(
        "from collections import deque\n\
         d = deque([1, 2, 3])\n\
         a = len(d)\n\
         d.append(4)\n\
         b = len(d)\n\
         c = d.popleft()\n",
    );
    assert_eq!(fx.g_int("a"), 3);
    assert_eq!(fx.g_int("b"), 4);
    assert_eq!(fx.g_int("c"), 1);
}














/// Regression: `setup_code_state` allocated the `*args` tuple and
/// the `**kw` dict with header type `None`, so `len(args)`,
/// `args[0]`, `len(kw)`, `not kw` and `**kw` unpacking all failed
/// (`obj_get_type` returned `None`). Fixed in D5 by allocating
/// them with the tuple/dict type headers (`src/runtime/bc.rs`).
/// The frozen `json.loads`/`json.dumps` were the first callers to
/// hit it (`if not kw:` + `_Decoder(s, **kw)`).
#[test]
fn d5_varargs_tuple_len() { let mut fx = Fx::new(); fx.run_ok("def f(*a):\n    return len(a)\nn = f(1, 2)\n"); assert_eq!(fx.g_int("n"), 2); }
#[test]
fn d5_varargs_tuple_subscr() { let mut fx = Fx::new(); fx.run_ok("def f(*a):\n    return a[0]\nx = f(7, 8)\n"); assert_eq!(fx.g_int("x"), 7); }
#[test]
fn d5_varkw_not_empty() { let mut fx = Fx::new(); fx.run_ok("def f(**kw):\n    if not kw:\n        return \"empty\"\n    return \"nonempty\"\na = f()\n"); assert_eq!(fx.g_str("a"), "empty"); }
#[test]
fn d5_varkw_len() { let mut fx = Fx::new(); fx.run_ok("def f(**kw):\n    return len(kw)\nn = f()\n"); assert_eq!(fx.g_int("n"), 0); }
#[test]
fn d5_varkw_unpack() { let mut fx = Fx::new(); fx.run_ok("def inner(**kw):\n    return len(kw)\ndef outer(**kw):\n    return inner(**kw)\nn = outer()\n"); assert_eq!(fx.g_int("n"), 0); }

/// `reversed` as a builtin (D5): the original registers it in
/// `modbuiltins.zig:1082`; D1 missed it, so frozen `heapq`
/// (`heapify` uses `reversed`) raised `NameError`. The type,
/// make_new slot and binding mirror the C13b `map` pattern.
#[test]
fn d5_reversed_list() { let mut fx = Fx::new(); fx.run_ok("a = list(reversed([1, 2, 3]))\nassert a == [3, 2, 1]\n"); }
#[test]
fn d5_reversed_str() { let mut fx = Fx::new(); fx.run_ok("a = list(reversed(\"abc\"))\nassert a == [\"c\", \"b\", \"a\"]\n"); }
#[test]
fn d5_reversed_print() { let mut fx = Fx::new(); fx.run_ok("a = str(reversed([1, 2]))\nassert a == \"<reversed>\"\n"); }


