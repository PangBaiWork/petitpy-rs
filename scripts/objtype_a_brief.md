# Lane C12a: port objtype (type-system core) to Rust

Branch: `lane/objtype-a` (base master `994bba3`, "Merge lane/callables-a").
Worktree: `~/workspace/petitpy-rs-objtype-a`.
Original: `src/objects/objtype/objtype.zig` — 2,214 lines, 0 test blocks.
Target: `src/objects/objtype/` (new directory; register in
`src/objects/mod.rs`).

## Standing rules (read first)

- `~/workspace/petitpy-rs/docs/agent-rules.md` — 对照原文、不许自创、同一套测试、
  纯 Rust std、文件<800行、无 unsafe、原项目只读、每块一提交、不许假完成。
- Work in SMALL STEPS: one file at a time, run `cargo test` after each file,
  keep the final report SHORT.
- NEVER `git add -A`. Commit only your own files, by path.
- The original repo `~/workspace/petitpy-orig` is READ-ONLY. Verify every
  behavioural claim on the reference binary
  `~/workspace/petitpy-orig/zig-out/bin/petitpy` before asserting it in a test.
- Baseline in this tree: `cargo test --release --lib --tests` = 688 passed,
  1 ignored; `cargo test --release --bins` = 9 passed. (cargo needs
  `export PATH="$HOME/.cargo/bin:$PATH"`.)
- The sibling lane C12b (objexcept + objmodule + objobject, 1,020 lines)
  is NOT dispatched yet — do not port exception/module/object types here.

## Scope (objtype.zig, line refs verified — read the file first)

- Special accessors: `npy_ty_attr_value_is_special_accessor` (:196),
  `npy_ty_scan_special_accessors_flag` (:200).
- Slot-offset table (:277-288) — the Zig reads slot indices by offset
  from the type header; in Rust this is the `TypeData` field layout.
  Port it as the documentation of which slot index maps to which
  `TypeData` field (do NOT reproduce the offset arithmetic).
- `mp_obj_is_subclass_fast` (:409) — MRO/base walk, the fast predicate
  the Zig uses everywhere (incl. tuple Add, see below).
- `mp_obj_cast_to_native_base` (:439) — the slow path for comparisons
  against a native base (C9c's tuple_cmp note: "else the slow path
  (cast-to-native-base, C12's)").
- `mp_obj_instance_is_callable` (:454), `mp_load_super_method` (:463),
  `instanceCallImpl` / `mp_obj_instance_call` (:478/:491).
- `isSubclassImpl` / `mp_obj_is_subclass` (:524/:551),
  `builtinIssubclassImpl` / `mp_builtin_issubclass` (:555/:562),
  `builtinIsinstanceImpl` / `mp_builtin_isinstance` (:566/:569),
  plus their FunBuiltin objects (:577/:581).
- ROM types: `mp_type_type` (:597), `mp_type_super` (:631),
  `mp_type_staticmethod` (:666), `mp_type_classmethod` (:698).
- `native_base_init_wrapper_obj` (:734), unary/binary op method-name
  tables (:747/:772).
- `npy_ty_class_lookup` (:840) — the class attribute lookup (MRO walk
  + descriptor protocol entry point; READ it fully before porting).
- Instance machinery: `mp_obj_instance_getiter` (:1050),
  `instance_unary_op` (:1148), `instance_binary_op` (:1152),
  `instance_subscr` (:1190), `mp_obj_instance_get_call` (:1248),
  `instance_get_buffer` (:1261), `mp_obj_instance_load_attr` (:1288),
  `mp_obj_instance_store_attr` (:1379), `mp_obj_instance_attr` (:1459).
- `type_make_new` / `type_call` (:1503/:1516) — calling a type
  constructs an instance (the make_new dispatch C10a deferred).
- `super_print` / `super_make_new` / `super_attr` (:1529/:1558/:1562).
- `instance_print` / `type_print` (:1638/:1688),
  `type_attr` (:1701).
- `mp_obj_new_type` (:2015) — runtime type creation (name, bases
  tuple, locals dict → TypeData + MRO). **This is the core of the lane.**
- `instance_count_native_bases` (:2034),
  `native_base_init_wrapper` (:2070), `mp_obj_new_instance` (:2084),
  `static_class_method_make_new` (:2112),
  `mp_obj_instance_make_new` (:2212).

## The `attr` slot (C10a's deferred item — do it here, carefully)

- `TypeData` gains `attr: Option<AttrFn>` (`AttrFn` is already in
  `src/objects/slots.rs`, load-only, added by C10a). There are ~56
  `TypeData {` literals in the tree (grep to confirm) — update ALL of
  them; `None` where the Zig's type has no attr slot. The C10a report
  said "30 处" — that count was wrong; count it yourself.
- Wire the three AttrFn functions C10a already ported and tested:
  `fun_bc_attr` (objfun.zig:465), `bound_meth_attr`
  (objboundmeth.zig:57), `closure_attr` (objclosure.zig:54), into
  their type datas. `FUN_BC_ATTR` (C10a) is the marker constant.
- CORRECTION to the C10a report: `mp_load_method` itself is NOT this
  lane's — it lives in `runtime.zig:1188` (C14's file). Do NOT port it
  here. This lane only adds the `attr` slot the function reads.

## ROM singletons (C9b/C9c TODOs — this lane owns them)

- The ROM empty-tuple singleton: the Zig has a single `()` object;
  `TUPLE_HASH_SEED` in `src/objects/containers/objtuple.rs` is
  currently 0 with a C12 TODO — when the singleton lands, the seed
  becomes the singleton's identity word so `hash(()) == id(())`.
  Decide where the singleton lives (a `Rom` registry? an arena
  field? `BuiltinTypes`?) and document it; C13 consumes it.
- The ROM empty-bytes singleton (C8a note: `new_empty_bytes` is
  currently a fresh heap object, not the singleton) — same treatment.
- The built-in type objects themselves: in the Zig they are static
  `MpObjTypeFull` ROM structs; in Rust they are heap `TypeData`
  built once. Decide the registry shape and document it — every
  lane's `*_type_data` constructor is the seed; this lane unifies
  them into one place C13 can initialise.

## The tuple ADD subclass-fast fix (C9c's recorded divergence — close it)

- The Zig's tuple ADD uses `mp_obj_is_subclass_fast` (namedtuple IS
  a tuple subclass, `a + (3,)` works); Rust's `tuple_binary_op`
  (C9b, `src/objects/containers/objtuple.rs`) still requires an
  exact tuple. Implement `is_subclass_fast` in this lane, then use
  it in the tuple Add arm (replace the exact-tuple check). Verify on
  the reference binary first (main-agent probes during C9c:
  `P(1,2)+(3,)` → `(1,2,3)`, `(1,2)+P(1,2)` → `(1,2,1,2)`,
  both plain tuples). This is a small, in-scope edit to objtuple.rs —
  the same class of rebase-time fix as C9c's `tuple_cmp`.

## Probes to run on the reference binary (before asserting)

- `isinstance(True, int)` → False, `issubclass(bool, int)` → False
  (C7 verified — do NOT re-derive from CPython).
- `isinstance(P(1,2), tuple)` → True (C9c verified).
- `class A: pass; class B(A): pass; isinstance(B(), A)`,
  `issubclass(B, A)`, `issubclass(B, (A, int))` (tuple classinfo),
  `issubclass(B, B)`, `isinstance(1, (int, str))`.
- `type(1)`, `type(1) is int`, `type(type(1)) is type`,
  `type('x').__name__`, `int.__mro__`, `bool.__bases__`.
- `class C: def __init__(self, x): self.x = x; c = C(5); c.x`,
  `c.y = 3; c.y`, `hasattr(c, 'x')`, `getattr(c, 'z', 9)`.
- `class D: pass; d = D(); d() ` → TypeError "not callable";
  `class E: def __call__(self): return 7; E()()`.
- `super()` inside a method (probe `class F(G): def m(self):
  return super().m()`), `staticmethod` / `classmethod` basics.
- `P(1,2) + (3,)` and `(1,2) + P(1,2)` (see the ADD fix above).
- `type('X', (object,), {'a': 1})` — runtime type creation via
  the 3-arg `type()` call; `X().a`, `X.__name__`, `X.__bases__`.

## Acceptance

- `cargo test --release --lib --tests`: 688 baseline + ≥30 new.
- `cargo test --release --bins`: 9 passed.
- `cargo build --release`: 0 warnings. No `unsafe` anywhere.
- Every file < 800 lines (objtype.zig is 2,214 lines — split the Rust
  side: type creation + MRO / instance machinery / super+staticmethod+
  classmethod / ROM registry, like C9a's map/objdict split).
- Original repo `~/workspace/petitpy-orig`: clean.
- Result file: `scripts/objtype_a_result.txt` — verdict counts,
  1:1 Zig→Rust mapping, every probed behaviour with the reference
  value, every intentional divergence, the `attr`-slot + ROM-registry
  decisions with Zig line refs, the ADD fix, and a merge note (what
  C13/C12b must consume: `mp_obj_new_type`, `is_subclass_fast`,
  the ROM registry, the `attr` slot).
- One commit on this branch: "Lane C12a: port objtype (type system core) to Rust".
  Do NOT commit the brief (`scripts/objtype_a_brief.md` stays untracked).

## When done

Write the result file, commit, and STOP — report back and wait for
verification. Do not merge, do not start another lane.
