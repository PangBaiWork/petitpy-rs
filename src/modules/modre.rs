//! The native `re` / `ure` module (Lane D5 — `extmod/modre.c`,
//! config `port/mpconfigport.h:113-116`: `MICROPY_PY_RE=1`,
//! `MATCH_GROUPS=1`, `MATCH_SPAN_START_END=1`, `RE_SUB=1`;
//! `MICROPY_PY_RE_DEBUG` is NOT defined, and error reporting
//! defaults to TERSE at `ROM_LEVEL=MINIMUM` — so "regex too
//! complex" is compiled out and every compile failure is
//! `ValueError("error in regex")`, probed 2026-11-18).
//!
//! The module is `MP_REGISTER_EXTENSIBLE_MODULE(MP_QSTR_re, ...)`
//! — it lives in `VmState.extensible_modules` under the name
//! `re` ONLY; `import ure` resolves to the same module object
//! through the import path's existing u-strip (D4), NOT via a
//! second registration (registering both would wrongly list
//! `ure` as a separate module — probed: after `import ure`,
//! `sys.modules` is still `[]`, and `ure.__name__` is `'re'`).
//!
//! Engine: the RE15 lane (`crate::vendor::re15` — the
//! `port/re1.5-patched/` sources: int16 LE jumps, 3-byte jump
//! instructions, `NON_ANCHORED_PREFIX=7`). `sizecode` /
//! `compilecode` / `recursiveloopprog_subject` are the
//! `re1_5_*` calls in `mod_re_compile` / `re_exec_helper` /
//! `re_split` / `re_sub_helper`; `dumpcode` exists in the
//! engine but the reference build does not compile it
//! (`MICROPY_PY_RE_DEBUG` off) and is not used here.
//!
//! Tables (in the C's order):
//! - module globals: `__name__`, `compile` (1–2), `match`
//!   (2–4), `search` (2–4), `sub` (3–5) — NO `split`, NO
//!   `DEBUG` at module level (probed: both `AttributeError`
//!   on the reference; `split` lives only in the compiled
//!   pattern type's locals dict).
//! - `re` locals: `match` (2–4), `search` (2–4), `split`
//!   (2–3), `sub` (3–5). The module-level `match` / `search` /
//!   `sub` are the SAME `FunBuiltin` objects as the type's
//!   (the C registers one `re_match_obj` in both tables).
//! - `match` locals: `group` (2–2), `groups` (1–1), `span`
//!   (1–2), `start` (1–2), `end` (1–2).
//!
//! `sub` is intercepted by GcRef identity (the D1/D4 pattern,
//! `call_sub_ctx`): it is the only entry point that calls user
//! code (a callable replacement). The module/type dicts hold a
//! bare-raise stub `FunBuiltin` for it (the D4
//! `opt_level_stub_fn` precedent); `call_function_ctx` never
//! dispatches the stub.
//!
//! Deliberate divergences from the reference (all probed,
//! all documented in `scripts/d5_result.txt` §3):
//! - A non-str/bytes PATTERN (`compile(123)`, `compile(None)`,
//!   `compile(bytearray(b"a"))`) SEGFAULTS the reference
//!   (NULL deref in `_compilecode`, rc=134); Rust raises
//!   `TypeError("can't convert to str implicitly")`.
//! - An int replacement (`sub("a", 5, "banana")`) SEGFAULTS
//!   the reference (`mp_obj_str_get_str` NULL deref in
//!   `re_sub_helper`, modre.c:357); Rust raises the same
//!   `TypeError`.
//! - `print(re)` is `<re 0x<slot>>` with the arena slot index
//!   (the C prints the raw ASLR-random pointer, which no test
//!   can pin — `print(ure.compile("a"))` on the reference is
//!   `<re 7e0d5e4de4a0>` etc.).

use crate::memory::gc::CallCtx;
use super::modre_match::{
    match_end_builtin, match_group_builtin, match_groups_builtin, match_span_builtin,
    match_start_builtin,
};
use super::modre_types::{match_type_data, re_type_data};
use crate::modules::{fix_dict, mk_fun, new_d4_module, store};
use crate::objects::containers::objlist::new_list;
use crate::objects::obj::{obj_get_int, obj_get_type, obj_is_callable};
use crate::objects::primitives::ops::{static_qstr_id, str_bytes_of, QstrResolver};
use crate::objects::strings::objstr_a::{new_bytes, new_str};
use crate::objects::value::{DictData, GcRef, MapData, MatchData, Obj, ReData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::runtime::raise::raise_type_arg;
use crate::runtime::vm::VmState;
use crate::runtime::vm_ops::{exc_from_pyerror, type_error};
use crate::vendor::re15::{self, ByteProg, Subject, Sub};

// ── local helpers ───────────────────────────────────────────────────
// `check_arity` / `exc` are local per module (the D4 precedent:
// modmicropython.rs:72 / modsignal.rs:69) — `call_function_ctx`
// bypasses the normal dispatch for the intercepted `sub`, so
// the sig is checked here, with the same message the
// dispatcher raises.

fn check_arity(
    ctx: &mut CallCtx,
    state: &VmState,
    n_args: usize,
    n_kw: usize,
    min: usize,
    max: usize,
) -> Result<(), Value> {
    if n_kw != 0 || n_args < min || n_args > max {
        return Err(type_error(ctx, state, "argument num/types mismatch"));
    }
    Ok(())
}

fn exc(ctx: &mut CallCtx, state: &VmState, e: PyError) -> Value {
    exc_from_pyerror(ctx, state, e)
}

/// `mp_obj_str_get_data` as a `Vec<u8>` (`str_bytes_of`: qstr /
/// `str` / `bytes` only). `None` is `TypeError("can't convert
/// to str implicitly")` — the `sendall` shape (modsocket.rs).
/// This is the shared shape for patterns, subjects and string
/// replacements; the reference SEGFAULTS on a non-str pattern
/// or an int replacement instead (see the module docs).
fn str_arg(ctx: &mut CallCtx, v: Value) -> PyResult<Vec<u8>> {
    let pool = ctx.pool;
    let resolver: QstrResolver<'_> = &|id| {
        let Ok(id16) = u16::try_from(id) else {
            return None;
        };
        if (id16 as usize) < pool.total() {
            Some(pool.qstr_str(id16).to_vec())
        } else {
            None
        }
    };
    match str_bytes_of(ctx.arena, ctx.types, resolver, v) {
        Some(b) => Ok(b),
        None => {
            ctx.record_raise("TypeError", Some("can't convert to str implicitly"));
            Err(PyError::PyRaise)
        }
    }
}

/// The result-piece constructor (`mp_obj_new_str_of_type`):
/// `str` when the subject is a str, `bytes` when it is bytes.
/// A qstr subject is a str.
pub(crate) fn subject_is_str(ctx: &CallCtx, subject: Value) -> bool {
    match subject {
        Value::Qstr(_) => true,
        Value::Heap(r) => matches!(ctx.arena.get(r), Obj::Str(_)),
        _ => false,
    }
}

pub(crate) fn piece(ctx: &mut CallCtx, is_str: bool, bytes: &[u8]) -> Value {
    if is_str {
        new_str(ctx, ctx.types, bytes)
    } else {
        new_bytes(ctx, ctx.types, bytes)
    }
}

/// The subject's bytes for a match object. The subject passed
/// `str_arg` when the match was created, so this cannot fail
/// (the `expect` documents the invariant, it is not a
/// user-input path).
pub(crate) fn subject_bytes(ctx: &mut CallCtx, subject: Value) -> Vec<u8> {
    str_arg(ctx, subject).expect("match subject is always str/bytes")
}

/// The compiled-pattern type object (`BuiltinTypes.re_type`);
/// `None` means no re module was built and registered —
/// unreachable through the module itself; a defensive
/// `TypeError` rather than a panic (the `socket_type`
/// precedent in modsocket.rs).
fn re_type(ctx: &mut CallCtx) -> PyResult<GcRef> {
    match ctx.types.re_type {
        Some(t) => Ok(t),
        None => {
            ctx.record_raise("TypeError", Some("re type not registered"));
            Err(PyError::PyRaise)
        }
    }
}

/// The match-object type object (`BuiltinTypes.match_type`);
/// same shape as `re_type`.
fn match_type(ctx: &mut CallCtx) -> PyResult<GcRef> {
    match ctx.types.match_type {
        Some(t) => Ok(t),
        None => {
            ctx.record_raise("TypeError", Some("match type not registered"));
            Err(PyError::PyRaise)
        }
    }
}

/// Exact-type test (`mp_obj_is_type(args[0], &re_type)` in
/// `re_exec_helper` / `re_sub_helper`).
fn is_re(ctx: &CallCtx, v: Value) -> bool {
    matches!(
        obj_get_type(ctx.arena, ctx.types, v),
        Some(t) if ctx.types.re_type == Some(t)
    )
}

/// The compiled program of an `Obj::Re` value (`None` for
/// anything else — the caller checks `is_re` first).
fn re_prog(ctx: &CallCtx, v: Value) -> Option<ByteProg> {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Re(d) => Some(d.prog.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The `(caps, subject)` of an `Obj::Match` value (`None` for
/// anything else). Cloned: the caller needs `&mut CallCtx`
/// afterwards, so a borrow into the arena cannot be held.
pub(crate) fn match_parts(ctx: &CallCtx, v: Value) -> Option<(Vec<Sub>, Value)> {
    match v {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Match(m) => Some((m.caps.clone(), m.subject)),
            _ => None,
        },
        _ => None,
    }
}

/// Allocate a match object (`re_exec_helper`'s tail:
/// `num_matches = sub + 1`, `str = args[1]`).
fn alloc_match(ctx: &mut CallCtx, caps: Vec<Sub>, subject: Value) -> PyResult<Value> {
    let t = match_type(ctx)?;
    let num_matches = caps.len();
    Ok(Value::Heap(ctx.alloc(
        Some(t),
        Obj::Match(MatchData {
            caps,
            num_matches,
            subject,
        }),
    )))
}

// ── module builder ──────────────────────────────────────────────────

/// Build the native `re` module object (`extmod/modre.c` table):
/// globals `{__name__, compile, match, search, sub}` — NO
/// `split`, NO `DEBUG` (both probed absent on the reference).
/// Returns `(module, re_type, match_type, sub_builtin)` (the D4
/// `build_collections_module` shape — the registrar stores
/// the two types in `BuiltinTypes` and the `sub` builtin in
/// `VmState.re_sub_builtin` for the GcRef-identity
/// interception). Registration into
/// `VmState.extensible_modules` is the registrar's, not this
/// builder's. The module and its types are pushed to
/// `ctx.roots` by `new_module` / the allocations below.
pub fn build_re_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<(Value, GcRef, GcRef, Value)> {
    let (module, dict) = new_d4_module(ctx, "re", module_type);
    // The shared FunBuiltins: the C registers ONE
    // `re_match_obj` / `re_search_obj` / `re_sub_obj` in both
    // the module table and the `re` locals table, so
    // `re.match is ure.compile("a").match` on the reference.
    // They are built once here with `mk_fun` and stored in
    // both dicts (the type dicts use `MapData::from_pairs` —
    // the same constructor `locals_dict_from_entries` uses).
    let q = |name: &[u8]| {
        Value::Qstr(static_qstr_id(name).expect("re member name is a static qstr"))
    };
    let compile_fn = mk_fun(ctx, fun_builtin_var, "compile", 1, 2, re_compile_builtin, None);
    let match_fn = mk_fun(ctx, fun_builtin_var, "match", 2, 4, re_match_builtin, None);
    let search_fn = mk_fun(ctx, fun_builtin_var, "search", 2, 4, re_search_builtin, None);
    let split_fn = mk_fun(ctx, fun_builtin_var, "split", 2, 3, re_split_builtin, None);
    let sub_fn = mk_fun(ctx, fun_builtin_var, "sub", 3, 5, re_sub_stub_fn, None);
    // `re` locals dict (modre.c `re_locals_dict_table` order:
    // match, search, split, sub).
    let re_dict = ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![
                (q(b"match"), match_fn),
                (q(b"search"), search_fn),
                (q(b"split"), split_fn),
                (q(b"sub"), sub_fn),
            ]),
        }),
    );
    // `match` locals dict (modre.c `match_locals_dict_table`
    // order: group, groups, span, start, end).
    let group_fn = mk_fun(ctx, fun_builtin_var, "group", 2, 2, match_group_builtin, None);
    let groups_fn = mk_fun(
        ctx,
        fun_builtin_var,
        "groups",
        1,
        1,
        match_groups_builtin,
        None,
    );
    let span_fn = mk_fun(ctx, fun_builtin_var, "span", 1, 2, match_span_builtin, None);
    let start_fn = mk_fun(ctx, fun_builtin_var, "start", 1, 2, match_start_builtin, None);
    let end_fn = mk_fun(ctx, fun_builtin_var, "end", 1, 2, match_end_builtin, None);
    let match_dict = ctx.alloc(
        None,
        Obj::Dict(DictData {
            map: MapData::from_pairs(vec![
                (q(b"group"), group_fn),
                (q(b"groups"), groups_fn),
                (q(b"span"), span_fn),
                (q(b"start"), start_fn),
                (q(b"end"), end_fn),
            ]),
        }),
    );
    let re_t = ctx.alloc(Some(ctx.types.type_), Obj::Type(re_type_data(re_dict)));
    let match_t = ctx.alloc(
        Some(ctx.types.type_),
        Obj::Type(match_type_data(match_dict)),
    );
    let object = ctx.types.object;
    if let Obj::Type(d) = ctx.arena.get_mut(re_t) {
        d.mro = vec![re_t, object];
    }
    if let Obj::Type(d) = ctx.arena.get_mut(match_t) {
        d.mro = vec![match_t, object];
    }
    ctx.roots.push(Value::Heap(re_t));
    ctx.roots.push(Value::Heap(match_t));
    store(ctx, dict, "compile", compile_fn);
    store(ctx, dict, "match", match_fn);
    store(ctx, dict, "search", search_fn);
    store(ctx, dict, "sub", sub_fn);
    fix_dict(ctx, dict);
    Ok((module, re_t, match_t, sub_fn))
}

// ── compile / exec ──────────────────────────────────────────────────

/// `mod_re_compile` (modre.c:443-460): `sizecode` then
/// `compilecode` on the pattern bytes; any failure is
/// `ValueError("error in regex")` (TERSE reporting — see the
/// module docs). The 2nd arg (flags) is accepted and IGNORED
/// (the C only reads it under `MICROPY_PY_RE_DEBUG`, which is
/// off — probed: `compile("a", 2)` works). The Rust engine's
/// `compilecode` runs the same two passes internally
/// (`sizecode` is `compile_into(..., false)`, `compilecode`
/// is `compile_into(..., true)`), so one call covers both.
fn compile_pattern(ctx: &mut CallCtx, pattern: Value) -> PyResult<ByteProg> {
    let bytes = str_arg(ctx, pattern)?;
    let mut prog = ByteProg::new();
    match re15::compilecode(&mut prog, &bytes) {
        Ok(()) => Ok(prog),
        Err(_) => {
            ctx.record_raise("ValueError", Some("error in regex"));
            Err(PyError::PyRaise)
        }
    }
}

/// `mod_re_compile` as a builtin: `compile(pattern[, flags])`.
fn re_compile_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let prog = compile_pattern(ctx, args[0])?;
    let t = re_type(ctx)?;
    Ok(Value::Heap(ctx.alloc(Some(t), Obj::Re(ReData { prog }))))
}

/// `re_exec_helper` (modre.c:192-241): the shared `match` /
/// `search` body. `args[0]` is a compiled pattern (the
/// `was_compiled` branch — `pos`/`endpos` honoured, with the
/// C's clamping) or a pattern string (compiled fresh, and
/// `pos`/`endpos` IGNORED — probed: `ure.match("a", "ba",
/// 1)` is None). `args[1]` is the subject (str or bytes).
/// No match → `None`; else a match object (`num_matches =
/// sub + 1`, `str = args[1]`).
fn re_exec(ctx: &mut CallCtx, anchored: bool, args: &[Value]) -> PyResult<Value> {
    let (prog, was_compiled) = if is_re(ctx, args[0]) {
        (
            re_prog(ctx, args[0]).expect("is_re checked the payload"),
            true,
        )
    } else {
        (compile_pattern(ctx, args[0])?, false)
    };
    let subject_bytes = str_arg(ctx, args[1])?;
    let len = subject_bytes.len();
    let mut begin = 0usize;
    let mut end = len;
    if was_compiled && args.len() > 2 {
        // Arg #2 is starting-pos (clamped to [0, len]).
        let startpos = obj_get_int(ctx, ctx.types, args[2])
            .map_err(|e| ctx.record_typed(e))?;
        let startpos = startpos.clamp(0, len as i64) as usize;
        begin = startpos;
        if args.len() > 3 {
            // Arg #3 is ending-pos (clamped to [startpos, len]).
            let endpos = obj_get_int(ctx, ctx.types, args[3])
                .map_err(|e| ctx.record_typed(e))?;
            end = endpos.clamp(startpos as i64, len as i64) as usize;
        }
    }
    let subject = Subject::new(&subject_bytes, 0, begin, end);
    let mut caps = vec![Sub::default(); prog.sub + 1];
    if !re15::recursiveloopprog_subject(&prog, &subject, &mut caps, anchored) {
        return Ok(Value::None);
    }
    alloc_match(ctx, caps, args[1])
}

/// `re_match` (modre.c): `match(pattern, string[, pos[,
/// endpos]])` / `compiled.match(string[, pos[, endpos]])`.
/// Anchored (`is_anchored = true`).
fn re_match_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    re_exec(ctx, true, args)
}

/// `re_search` (modre.c): same shape, unanchored.
fn re_search_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    re_exec(ctx, false, args)
}

// ── split ───────────────────────────────────────────────────────────

/// `re_split` (modre.c:251-299): `compiled.split(string[,
/// maxsplit])` — method form ONLY (there is no module-level
/// `split`). `maxsplit` defaults to 0, which means UNLIMITED
/// (the loop breaks only on `maxsplit > 0 && --maxsplit ==
/// 0` — probed: `split("banana", 1)` is `['b', 'nana']`).
/// An empty match (or no match) ends the loop; the tail piece
/// is always appended. A program WITH capture groups raises
/// `NotImplementedError("splitting with sub-captures")` after
/// the first piece is appended (probed). Pieces take the
/// subject's type (`str` or `bytes`).
fn re_split_builtin(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let prog = match re_prog(ctx, args[0]) {
        Some(p) => p,
        None => {
            // Unreachable through normal attribute access (the
            // type is not exposed as a module attribute, so
            // `args[0]` is always a compiled pattern) — a
            // defensive `TypeError`, not a crash (the C would
            // deref a garbage pointer here).
            ctx.record_raise("TypeError", Some("split() requires a compiled pattern"));
            return Err(PyError::PyRaise);
        }
    };
    let subject_bytes = str_arg(ctx, args[1])?;
    let is_str = subject_is_str(ctx, args[1]);
    let mut maxsplit: i64 = 0;
    if args.len() > 2 {
        maxsplit = obj_get_int(ctx, ctx.types, args[2])
            .map_err(|e| ctx.record_typed(e))?;
    }
    let end = subject_bytes.len();
    let mut pieces: Vec<Value> = Vec::new();
    let mut begin = 0usize;
    loop {
        let mut caps = vec![Sub::default(); prog.sub + 1];
        let subject = Subject::new(&subject_bytes, 0, begin, end);
        let matched = re15::recursiveloopprog_subject(&prog, &subject, &mut caps, false);
        if !matched || caps[0].start == caps[0].end {
            break;
        }
        let (s0, e0) = (
            caps[0].start.expect("group 0 participates on a match"),
            caps[0].end.expect("group 0 participates on a match"),
        );
        pieces.push(piece(ctx, is_str, &subject_bytes[begin..s0]));
        if prog.sub > 0 {
            ctx.record_raise(
                "NotImplementedError",
                Some("splitting with sub-captures"),
            );
            return Err(PyError::PyRaise);
        }
        begin = e0;
        if maxsplit > 0 {
            maxsplit -= 1;
            if maxsplit == 0 {
                break;
            }
        }
    }
    pieces.push(piece(ctx, is_str, &subject_bytes[begin..end]));
    let list = new_list(ctx, ctx.types.list, &pieces);
    Ok(Value::Heap(list))
}

// ── sub ─────────────────────────────────────────────────────────────

/// The stub `func` for the intercepted `sub` builtin — a bare
/// raise, like D4's `opt_level_stub_fn` (the interception in
/// `call_function_ctx` handles the real calls).
fn re_sub_stub_fn(ctx: &mut CallCtx, _args: &[Value]) -> PyResult<Value> {
    let _ = ctx;
    Err(PyError::PyRaise)
}

/// The `\g<N>` / `\N` / `\\` expansion of one replacement
/// string (`re_sub_helper`'s inner loop, modre.c:340-372):
/// `\g<12>` and `\12` both substitute group 12 (multi-digit,
/// `unsigned int` arithmetic — wrapping, as the C's); a group
/// number `>= num_matches` raises `IndexError(arg)` with the
/// number as the arg (probed: missing group 2 → `IndexError
/// 2`); a non-participating group substitutes nothing; `\\`
/// appends one backslash; any other `\x` drops the backslash
/// and appends `x` literally on the next iteration.
fn expand_repl(
    ctx: &mut CallCtx,
    caps: &[Sub],
    subject_bytes: &[u8],
    repl: &[u8],
    out: &mut Vec<u8>,
) -> PyResult<()> {
    let num_matches = caps.len();
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'\\' {
            i += 1;
            let mut is_g_format = false;
            if i + 1 < repl.len() && repl[i] == b'g' && repl[i + 1] == b'<' {
                i += 2;
                is_g_format = true;
            }
            if i < repl.len() && repl[i].is_ascii_digit() {
                let mut match_no: u32 = 0;
                while i < repl.len() && repl[i].is_ascii_digit() {
                    match_no = match_no
                        .wrapping_mul(10)
                        .wrapping_add((repl[i] - b'0') as u32);
                    i += 1;
                }
                if is_g_format && i < repl.len() && repl[i] == b'>' {
                    i += 1;
                }
                if match_no as usize >= num_matches {
                    return Err(raise_type_arg(
                        ctx,
                        "IndexError",
                        Value::small_int(match_no as i64)
                            .expect("a u32 fits the small-int window"),
                    ));
                }
                if let Some(start) = caps[match_no as usize].start {
                    let end = caps[match_no as usize]
                        .end
                        .expect("a participating group has an end");
                    out.extend_from_slice(&subject_bytes[start..end]);
                }
            } else if i < repl.len() && repl[i] == b'\\' {
                out.push(b'\\');
                i += 1;
            }
            // Else: the backslash is dropped; the next byte is
            // appended literally by the next loop iteration.
        } else {
            out.push(repl[i]);
            i += 1;
        }
    }
    Ok(())
}

/// `re_sub_helper` (modre.c:306-390), intercepted by GcRef
/// identity (the D1/D4 pattern): `sub(pattern, repl, string[,
/// count[, flags]])` / `compiled.sub(repl, string[, count[,
/// flags]])`. `count` defaults to 0 (UNLIMITED — probed:
/// `sub("a", "X", "banana")` replaces all three; `count=1`
/// replaces one); `flags` is accepted and ignored. The loop
/// searches unanchored and stops on no-match OR an empty
/// match; with no substitution at all the SUBJECT ITSELF is
/// returned (the C's optimisation — probed: `==` True and the
/// same object). A callable replacement is called with the
/// match object (`mp_call_function_1`); a string replacement
/// goes through `expand_repl`. The result takes the
/// subject's type (`str` → str, `bytes` → bytes).
pub fn call_sub_ctx(
    ctx: &mut CallCtx,
    state: &mut VmState,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
) -> Result<Value, Value> {
    check_arity(ctx, state, n_args, n_kw, 3, 5)?;
    let prog = if is_re(ctx, args[0]) {
        re_prog(ctx, args[0]).expect("is_re checked the payload")
    } else {
        compile_pattern(ctx, args[0]).map_err(|e| exc(ctx, state, e))?
    };
    let replace = args[1];
    let where_v = args[2];
    let mut count: i64 = 0;
    if n_args > 3 {
        count = obj_get_int(ctx, ctx.types, args[3]).map_err(|e| {
            let pe = ctx.record_typed(e);
            exc(ctx, state, pe)
        })?;
    }
    // `args[4]` (flags) is ignored — the C notes "flags are
    // currently ignored".
    let where_bytes = str_arg(ctx, where_v).map_err(|e| exc(ctx, state, e))?;
    let is_str = subject_is_str(ctx, where_v);
    let is_callable = obj_is_callable(ctx.arena, ctx.types, replace);
    let end = where_bytes.len();
    let mut result: Option<Vec<u8>> = None;
    let mut begin = 0usize;
    loop {
        let mut caps = vec![Sub::default(); prog.sub + 1];
        let subject = Subject::new(&where_bytes, 0, begin, end);
        let matched = re15::recursiveloopprog_subject(&prog, &subject, &mut caps, false);
        if !matched || caps[0].start == caps[0].end {
            break;
        }
        let (m0, m1) = (
            caps[0].start.expect("group 0 participates on a match"),
            caps[0].end.expect("group 0 participates on a match"),
        );
        if result.is_none() {
            result = Some(Vec::with_capacity(m0 - begin));
        }
        let out = result.as_mut().expect("result initialised above");
        out.extend_from_slice(&where_bytes[begin..m0]);
        // Get the replacement string (callable → called with
        // the match object; else the value itself).
        let repl_value = if is_callable {
            let match_obj = alloc_match(ctx, caps.clone(), where_v)
                .map_err(|e| exc(ctx, state, e))?;
            let extra_base = ctx.roots.extra.len();
            ctx.roots.extra.push(match_obj);
            let r = crate::runtime::vm_call::call_function_ctx(
                ctx,
                state,
                replace,
                1,
                0,
                &[match_obj],
            );
            ctx.roots.extra.truncate(extra_base);
            r?
        } else {
            replace
        };
        let repl_bytes = str_arg(ctx, repl_value).map_err(|e| exc(ctx, state, e))?;
        expand_repl(ctx, &caps, &where_bytes, &repl_bytes, out)
            .map_err(|e| exc(ctx, state, e))?;
        begin = m1;
        if count > 0 {
            count -= 1;
            if count == 0 {
                break;
            }
        }
    }
    match result {
        None => Ok(where_v),
        Some(mut out) => {
            out.extend_from_slice(&where_bytes[begin..end]);
            Ok(piece(ctx, is_str, &out))
        }
    }
}

