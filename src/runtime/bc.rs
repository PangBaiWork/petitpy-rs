//! Bytecode helpers — port of `src/runtime/bc.zig` (461 lines, 0 test
//! blocks).
//!
//! Two halves, exactly as in the original:
//!
//! 1. **varint codec** — `mp_encode_uint` / `mp_decode_uint` /
//!    `mp_decode_uint_value` / `mp_decode_uint_skip`
//!    (`MP_ENCODE_UINT_MAX_BYTES = 10`). 7-bit chunks, little-endian
//!    encoded / big-endian stored, MSB continuation flag (bc.zig
//!    header comment; vendor `py/bc.c`).
//! 2. **frame setup** — `mp_setup_code_state` /
//!    `mp_setup_code_state_helper` (vendor `py/bc.c:127-337`, Zig
//!    port verbatim in structure): binds positional args, `*args`,
//!    `**kwargs`, defaults and keyword-only args into a fresh code
//!    state, then advances `ip` past the prelude / source info /
//!    cells so the VM starts at the first real opcode.
//!
//! Encode reuse (brief): the emit side already has this exact
//! algorithm as `compiler::emit::encoders::encode_uint` (Phase B,
//! verified 552/552 byte-identical against the original). The two
//! encoders are the same loop — the Zig `mp_encode_uint` builds the
//! bytes from the end with plain `0x7f` chunks and sets `| 0x80` on
//! all but the last on copy-out; the emit port sets the `0x80` bit
//! while filling — identical output for every input, including 0
//! (one `0x00` byte) and `usize::MAX` (10 bytes). So this module
//! **re-exports** the emit encoder instead of copying it; only the
//! decode half is new here.
//!
//! Differences from the C/Zig shape, all deliberate and documented:
//! - `MpCodeState` is an owned Rust struct (`CodeState`) with only
//!   the fields `bc.zig` actually uses (`fun_bc`, `ip`, `n_state`,
//!   `exc_sp_idx`; the state slots themselves). The C `sp` field
//!   (`&state[0] - 1`) is the VM's value-stack pointer — it is owned
//!   by the VM (C13), not stored here; `old_globals` is set by the
//!   C caller (`INIT_CODESTATE` in `objfun.c:217-220`) *after*
//!   `mp_setup_code_state` returns, so it is not part of this
//!   struct either.
//! - `ip` is a byte offset into `RawCode.fun_data`, not a raw
//!   pointer (the Zig `[*]const Byte`).
//! - The C `Obj` slots are `Option<Value>` (`None` == `MP_OBJ_NULL`).
//! - The C `mp_obj_fun_bc_t` pieces the helper reads — `bytecode`,
//!   the module qstr table (`context->constants.qstr_table`), and
//!   the `extra_args[]` flex array — are passed explicitly
//!   (`&RawCode`, `&[u16]`, `&[Value]`): the frozen `FunBcData`
//!   (`docs/value-obj-design.md` D4) holds only a code-store index,
//!   so the VM (C13) wires those pieces in when it calls setup.
//! - `n_state` is a caller parameter, as in C: the caller decodes
//!   it from the sig prelude first (`DECODE_CODESTATE_SIZE`,
//!   `objfun.c:205-213`) and the helper uses the cached value,
//!   decoding S again only to discard it (`(void)n_state_unused`).
//! - Raises do not go through a `pending_exc` global (there is no
//!   VM state yet — D7). `setup_code_state` returns
//!   `Result<CodeState, SetupError>`; the error carries exactly the
//!   data the original formats into its `TypeError` message (arg
//!   index / qstr id), and `SetupError::message` renders the TERSE
//!   strings, all verified against the reference binary (see tests).
//! - The empty-`*args` tuple is passed in (`empty_tuple`): the
//!   original stores the `mp_const_empty_tuple` singleton
//!   (`objtuple.c:241-243`: `mp_obj_new_tuple(0, …)` returns it),
//!   and `f(*a): return a is ()` is observably `True` on the
//!   reference binary — a fresh empty tuple per call would break
//!   that identity. The VM owns the singleton and passes it here.
//! - Truncated bytecode panics (via slice indexing), mirroring the
//!   Zig port's index-out-of-bounds panic on the same malformed
//!   input; the `.mpy` loader validates structure first.
//! - `decode_uint` uses `wrapping_shl`, matching C unsigned
//!   wrap for over-long varints (the Zig `<<` on `usize` wraps
//!   the same way).
//! - Rooting: allocations inside setup use the caller's `RootSet`
//!   (as `CallCtx::alloc` does). With the C0 skeleton's threshold
//!   of `-1` no collection can trigger mid-setup; once C4 lands
//!   the trigger rule the VM must root the in-progress code state,
//!   as it roots its value stack.
//!
//! Tests live in `src/runtime/bc_tests.rs` (split out so both files
//! stay under the 800-line cap — the Lane RE15 precedent).

use crate::compiler::mpy::RawCode;
use crate::memory::gc::{GcArena, RootSet};
use crate::objects::value::{CellData, DictData, GcRef, MapData, Obj, Value};
use crate::runtime::qstr::RuntimeQstr;

/// Re-exported emit-side encoder — the same algorithm as Zig
/// `mp_encode_uint`; see the module docs. Writes into `buf` (must
/// hold at least `MP_ENCODE_UINT_MAX_BYTES` bytes) starting from the
/// end; returns the slice actually written.
pub use crate::compiler::emit::encoders::encode_uint;

/// Maximum bytes of one varint (`MP_ENCODE_UINT_MAX_BYTES`, 64-bit:
/// `(8 * 8 + 6) / 7 = 10`).
pub const MP_ENCODE_UINT_MAX_BYTES: usize = 10;

/// Scope flags (`py/runtime0.h:34-44`); only the three `bc.zig` uses
/// are defined here.
pub const MP_SCOPE_FLAG_VARARGS: usize = 0x04;
pub const MP_SCOPE_FLAG_VARKEYWORDS: usize = 0x02;
pub const MP_SCOPE_FLAG_DEFKWARGS: usize = 0x08;

/// `mp_map_lookup` modes (`py/obj.h`): look up only, or insert a
/// `(key, NULL)` entry when missing.
pub const MP_MAP_LOOKUP: i32 = 0;
pub const MP_MAP_LOOKUP_ADD_IF_NOT_FOUND: i32 = 1;

// ── varint decode ──────────────────────────────────────────────────

/// `mp_decode_uint`: decode one varint, advancing `*p` past it.
/// Panics on truncated input (mirrors the Zig port's OOB panic).
pub fn decode_uint(p: &mut &[u8]) -> usize {
    let mut unum: usize = 0;
    loop {
        let v = take_byte(p);
        unum = unum.wrapping_shl(7) | ((v & 0x7f) as usize);
        if v & 0x80 == 0 {
            break;
        }
    }
    unum
}

/// `mp_decode_uint_value`: decode one varint without advancing the
/// caller's cursor.
pub fn decode_uint_value(p: &[u8]) -> usize {
    let mut q = p;
    decode_uint(&mut q)
}

/// `mp_decode_uint_skip`: return the slice just past one varint.
pub fn decode_uint_skip(p: &[u8]) -> &[u8] {
    let mut q = p;
    while q[0] & 0x80 != 0 {
        q = &q[1..];
    }
    &q[1..]
}

/// Consume one byte, panicking on truncated input.
fn take_byte(p: &mut &[u8]) -> u8 {
    let (b, rest) = p.split_first().expect("truncated bytecode prelude");
    *p = rest;
    *b
}

// ── prelude decode (vendor `py/bc.h` macros, via `bc.zig`) ────────

/// The six fields of the sig prelude (`MP_BC_PRELUDE_SIG_DECODE`).
///
/// `pub(crate)`: the VM lane (C13) decodes the sig prelude itself
/// to size the code state (`DECODE_CODESTATE_SIZE`, `objfun.c`), and
/// the tests pin the decode vectors.
pub(crate) struct PreludeSig {
    pub(crate) n_state: usize,
    pub(crate) n_exc_stack: usize,
    pub(crate) scope_flags: usize,
    pub(crate) n_pos_args: usize,
    pub(crate) n_kwonly_args: usize,
    pub(crate) n_def_pos_args: usize,
}

/// `MP_BC_PRELUDE_SIG_DECODE_INTO` — advances `*ip` past the sig
/// bytes. First byte `xSSSSEAA`, continuation bytes `xFSSKAED`.
pub(crate) fn prelude_sig_decode(ip: &mut &[u8]) -> PreludeSig {
    let mut z = take_byte(ip);
    // xSSSSEAA
    let mut s = ((z >> 3) & 0xf) as usize;
    let mut e = ((z >> 2) & 0x1) as usize;
    let mut f = 0usize;
    let mut a = (z & 0x3) as usize;
    let mut k = 0usize;
    let mut d = 0usize;
    let mut n: u32 = 0;
    while z & 0x80 != 0 {
        z = take_byte(ip);
        // xFSSKAED
        s |= ((z & 0x30) as usize).wrapping_shl(2 * n);
        e |= ((z & 0x02) as usize).wrapping_shl(n);
        f |= (((z & 0x40) >> 6) as usize).wrapping_shl(n);
        a |= ((z & 0x4) as usize).wrapping_shl(n);
        k |= (((z & 0x08) >> 3) as usize).wrapping_shl(n);
        d |= ((z & 0x1) as usize).wrapping_shl(n);
        n += 1;
    }
    s += 1;
    PreludeSig {
        n_state: s,
        n_exc_stack: e,
        scope_flags: f,
        n_pos_args: a,
        n_kwonly_args: k,
        n_def_pos_args: d,
    }
}

/// The two fields of the size prelude (`MP_BC_PRELUDE_SIZE_DECODE`).
pub(crate) struct PreludeSize {
    pub(crate) n_info: usize,
    pub(crate) n_cell: usize,
}

/// `MP_BC_PRELUDE_SIZE_DECODE_INTO` — advances `*ip` past the size
/// bytes. Bytes are `xIIIIIIC` (I = source-info length, 6 bits per
/// byte; C = cell count, 1 bit per byte).
pub(crate) fn prelude_size_decode(ip: &mut &[u8]) -> PreludeSize {
    let mut c = 0usize;
    let mut i = 0usize;
    let mut n: u32 = 0;
    loop {
        let z = take_byte(ip);
        // xIIIIIIC
        c |= ((z & 1) as usize).wrapping_shl(n);
        i |= (((z & 0x7e) >> 1) as usize).wrapping_shl(6 * n);
        if z & 0x80 == 0 {
            break;
        }
        n += 1;
    }
    PreludeSize { n_info: i, n_cell: c }
}

// ── map lookup (vendor `mp_map_lookup`, linear scan) ──────────────

/// Look `key` up in a `MapData` entry list. Returns `(index,
/// is_new)`: `is_new` is true only when `ADD_IF_NOT_FOUND` created
/// the entry (its value is the `NULL` placeholder until the caller
/// sets it — the C code's `elem->value == MP_OBJ_NULL` test on a
/// fresh insert is exactly `is_new`, since a stored value is never
/// `NULL`). `None` only for a `LOOKUP` miss.
///
/// Note: the C map is a hash table; this linear scan has the same
/// lookup semantics (C9 owns the `DictData` payload and may replace
/// it with a hash table later).
fn map_lookup(
    entries: &mut Vec<(Value, Value)>,
    key: Value,
    mode: i32,
) -> Option<(usize, bool)> {
    if let Some(pos) = entries.iter().position(|(k, _)| *k == key) {
        return Some((pos, false));
    }
    if mode == MP_MAP_LOOKUP_ADD_IF_NOT_FOUND {
        entries.push((key, Value::None));
        Some((entries.len() - 1, true))
    } else {
        None
    }
}

/// Mutable entry list of a heap `Dict` (panics if the slot does not
/// hold a dict — a caller-contract violation).
fn dict_entries_mut(arena: &mut GcArena, dict: GcRef) -> &mut Vec<(Value, Value)> {
    match arena.get_mut(dict) {
        Obj::Dict(d) => &mut d.map.entries,
        _ => panic!("kwargs dict slot does not hold a Dict"),
    }
}

// ── errors ─────────────────────────────────────────────────────────

/// What `mp_setup_code_state_helper` raises, as data. The original
/// raises `TypeError` with the TERSE messages below
/// (`MICROPY_ERROR_REPORTING_TERSE`); `message()` renders them.
/// The VM lane turns this into the exception object + `PyError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    /// `mp_arg_error_terse_mismatch` (`py/argcheck.c:140-142`).
    ArgCountMismatch,
    /// `"function got multiple values for argument '%q'"`.
    MultipleValues { qstr: u32 },
    /// TERSE `"unexpected keyword argument"`.
    UnexpectedKeyword,
    /// `"function missing required positional argument #%d"`
    /// (`index` is the 0-based arg index — see the helper).
    MissingPositional { index: usize },
    /// `"function missing required keyword argument '%q'"`.
    MissingKeyword { qstr: u32 },
    /// `"function missing keyword-only argument"`.
    MissingKeywordOnly,
}

impl SetupError {
    /// Render the exact TERSE message the original raises.
    pub fn message(&self, qstr: &RuntimeQstr) -> String {
        match self {
            SetupError::ArgCountMismatch => "argument num/types mismatch".to_string(),
            SetupError::MultipleValues { qstr: q } => format!(
                "function got multiple values for argument '{}'",
                String::from_utf8_lossy(qstr.qstr_str(*q as u16))
            ),
            SetupError::UnexpectedKeyword => "unexpected keyword argument".to_string(),
            SetupError::MissingPositional { index } => {
                format!("function missing required positional argument #{}", index)
            }
            SetupError::MissingKeyword { qstr: q } => format!(
                "function missing required keyword argument '{}'",
                String::from_utf8_lossy(qstr.qstr_str(*q as u16))
            ),
            SetupError::MissingKeywordOnly => "function missing keyword-only argument".to_string(),
        }
    }
}

// ── code state ─────────────────────────────────────────────────────

/// Rust form of C `mp_code_state_t` (`mp_mirrors.zig:167`), with
/// only the fields `bc.zig` uses. `state` is the `n_state` local
/// slots (`None` == `MP_OBJ_NULL`); `ip` is a byte offset into the
/// function's `fun_data`, pointing at the first real opcode once
/// setup returns. `sp` (the VM value-stack pointer) and
/// `old_globals` live outside this struct — see the module docs.
#[derive(Debug, Clone)]
pub struct CodeState {
    /// `fun_bc` — the function object (`Obj::FunBc` slot).
    pub fun: GcRef,
    /// `ip` — byte offset into `RawCode.fun_data`.
    pub ip: usize,
    /// The `n_state` local slots.
    pub state: Vec<Option<Value>>,
    /// `n_state` — caller-decoded, cached (see `setup_code_state`).
    pub n_state: usize,
    /// `exc_sp_idx` — 0 after setup.
    pub exc_sp_idx: usize,
}

/// `mp_setup_code_state` (`py/bc.c:322-332`): `code_state->ip` is
/// set to the function bytecode and the helper does the binding.
///
/// - `fun`: the `Obj::FunBc` handle, recorded in the returned state.
/// - `code`: the function's `RawCode` (`fun_data` = prelude +
///   source info + cells + bytecode).
/// - `qstr_table`: the module's resolved qstr table (prelude index
///   -> qstr id), as the C loader builds it from the `.mpy` qstr
///   section.
/// - `extra_args`: the function's default args, in creation order:
///   `n_def_pos_args` positional defaults, then — iff the
///   `DEFKWARGS` scope flag is set — the defkwargs dict
///   (`mp_obj_new_fun_bc`, `py/objfun.c:414-440`).
/// - `empty_tuple`: the VM-owned `mp_const_empty_tuple` singleton
///   (see the module docs).
/// - `n_state`: caller-decoded from the sig prelude (`S + 1`,
///   `objfun.c` `DECODE_CODESTATE_SIZE`).
/// - `args`: `n_args` positionals followed by `2 * n_kw` keyword
///   `(name, value)` pairs; names are `Value::Qstr` immediates.
pub fn setup_code_state(
    fun: GcRef,
    code: &RawCode,
    qstr_table: &[u16],
    extra_args: &[Value],
    empty_tuple: Value,
    n_state: usize,
    n_args: usize,
    n_kw: usize,
    args: &[Value],
    arena: &mut GcArena,
    roots: &RootSet,
) -> Result<CodeState, SetupError> {
    debug_assert_eq!(
        args.len(),
        n_args + 2 * n_kw,
        "args must hold n_args positionals + 2*n_kw (name, value) pairs"
    );
    let mut cs = CodeState {
        fun,
        ip: 0,
        state: vec![None; n_state],
        n_state,
        exc_sp_idx: 0,
    };
    setup_code_state_helper(
        &mut cs,
        code,
        qstr_table,
        extra_args,
        empty_tuple,
        n_args,
        n_kw,
        args,
        arena,
        roots,
    )?;
    Ok(cs)
}

/// `mp_setup_code_state_helper` (`py/bc.c:127-320`, Zig port
/// verbatim in structure). Binds the arguments into `cs.state`
/// and leaves `cs.ip` at the first bytecode instruction.
fn setup_code_state_helper(
    cs: &mut CodeState,
    code: &RawCode,
    qstr_table: &[u16],
    extra_args: &[Value],
    empty_tuple: Value,
    n_args_in: usize,
    n_kw: usize,
    args: &[Value],
    arena: &mut GcArena,
    roots: &RootSet,
) -> Result<(), SetupError> {
    let mut n_args = n_args_in;
    // `kwargs = args + n_args` is computed BEFORE the positional
    // check below reassigns `n_args` (bc.c/bc.zig order); splitting
    // up front keeps that order explicit.
    let (pos_args, kw_pairs) = args.split_at(n_args);

    let fun_data = &code.fun_data;
    // Decode prelude. The C macros advance `code_state->ip` — mirror
    // that by tracking the cursor, so the post-state matches
    // (`ip` then points at the source-info section).
    let mut cursor: &[u8] = fun_data;
    let sig = prelude_sig_decode(&mut cursor);
    let size = prelude_size_decode(&mut cursor);
    // The helper uses the caller-cached `n_state`, not the decoded
    // S — `(void)n_state_unused; (void)n_exc_stack_unused;` (bc.c).
    let _ = (sig.n_state, sig.n_exc_stack);
    let n_state = cs.n_state;
    cs.ip = fun_data.len() - cursor.len();
    let ip_after_prelude = cursor;

    // `code_state_state = code_state->sp + 1` (sp was `&state[0] - 1`).
    let state = &mut cs.state;
    cs.exc_sp_idx = 0;
    // The wrapper already zeroed the slots (`@memset(state, 0)`).

    // `var_pos_kw_args` — the slot where the var-args tuple and the
    // var-kw dict go. Signed index so the `*var_pos_kw_args--`
    // pattern works.
    let mut vpk_idx: isize =
        n_state as isize - 1 - sig.n_pos_args as isize - sig.n_kwonly_args as isize;

    // check positional arguments
    if n_args > sig.n_pos_args {
        if sig.scope_flags & MP_SCOPE_FLAG_VARARGS == 0 {
            return Err(SetupError::ArgCountMismatch);
        }
        let tuple = arena.alloc(None, Obj::Tuple(pos_args[sig.n_pos_args..].to_vec()), roots);
        state[vpk_slot(vpk_idx)] = Some(Value::Heap(tuple));
        vpk_idx -= 1;
        n_args = sig.n_pos_args;
    } else {
        if sig.scope_flags & MP_SCOPE_FLAG_VARARGS != 0 {
            state[vpk_slot(vpk_idx)] = Some(empty_tuple);
            vpk_idx -= 1;
        }
        // Apply processing and check below only if we don't have
        // kwargs, otherwise, kw handling code below has own extensive
        // checks.
        if n_kw == 0 && sig.scope_flags & MP_SCOPE_FLAG_DEFKWARGS == 0 {
            // `n_pos_args - n_def_pos_args` is always >= 0 by
            // construction (def args are a suffix of positional args).
            if n_args >= sig.n_pos_args - sig.n_def_pos_args {
                let mut i = n_args;
                while i < sig.n_pos_args {
                    state[n_state - 1 - i] =
                        Some(extra_args[i - (sig.n_pos_args - sig.n_def_pos_args)]);
                    i += 1;
                }
            } else {
                return Err(SetupError::ArgCountMismatch);
            }
        }
    }

    // copy positional args into state
    for i in 0..n_args {
        state[n_state - 1 - i] = Some(pos_args[i]);
    }

    // check keyword arguments
    if n_kw != 0 || sig.scope_flags & MP_SCOPE_FLAG_DEFKWARGS != 0 {
        let mut kwargs_dict: Option<GcRef> = None;
        if sig.scope_flags & MP_SCOPE_FLAG_VARKEYWORDS != 0 {
            let d = arena.alloc(
                None,
                Obj::Dict(DictData {
                    map: MapData {
                        entries: Vec::new(),
                    },
                }),
                roots,
            );
            state[vpk_slot(vpk_idx)] = Some(Value::Heap(d));
            kwargs_dict = Some(d);
        }

        let mut i = 0;
        'kw_outer: while i < n_kw {
            let wanted_arg_name = kw_pairs[2 * i];

            // `arg_names = mp_decode_uint_skip(code_state->ip)` — skip
            // simple_name qstr at start of the source-info section.
            let mut arg_names = decode_uint_skip(ip_after_prelude);

            let mut j = 0;
            let n_named = sig.n_pos_args + sig.n_kwonly_args;
            while j < n_named {
                let idx = decode_uint(&mut arg_names);
                let arg_qstr = qstr_table[idx];
                if wanted_arg_name == Value::Qstr(u32::from(arg_qstr)) {
                    if state[n_state - 1 - j].is_some() {
                        // error_multiple
                        return Err(SetupError::MultipleValues {
                            qstr: u32::from(arg_qstr),
                        });
                    }
                    state[n_state - 1 - j] = Some(kw_pairs[2 * i + 1]);
                    i += 1;
                    continue 'kw_outer;
                }
                j += 1;
            }
            // Didn't find name match with positional args
            if sig.scope_flags & MP_SCOPE_FLAG_VARKEYWORDS == 0 {
                // TERSE form (`MICROPY_ERROR_REPORTING_TERSE`)
                return Err(SetupError::UnexpectedKeyword);
            }
            let dict = kwargs_dict.expect("VARKEYWORDS set but no kwargs dict allocated");
            let value = kw_pairs[2 * i + 1];
            let entries = dict_entries_mut(arena, dict);
            match map_lookup(entries, wanted_arg_name, MP_MAP_LOOKUP_ADD_IF_NOT_FOUND) {
                Some((pos, is_new)) => {
                    if is_new {
                        entries[pos].1 = value;
                    } else {
                        // error_multiple (jumped to from goto in C)
                        return Err(SetupError::MultipleValues {
                            qstr: wanted_arg_name.as_qstr().unwrap_or(0),
                        });
                    }
                }
                None => unreachable!("ADD_IF_NOT_FOUND always returns an entry"),
            }
            i += 1;
        }

        // fill in defaults for positional args
        // C: `d = &state[n_state - n_pos_args]`,
        //    `s = &extra_args[n_def_pos_args - 1]`,
        //    `for (i = n_def_pos_args; i > 0; i--, d++, s--) ...`
        let mut d_idx = n_state - sig.n_pos_args;
        let mut s_idx = sig.n_def_pos_args as isize - 1;
        let mut k = sig.n_def_pos_args;
        while k > 0 {
            if state[d_idx].is_none() {
                state[d_idx] = Some(extra_args[vpk_slot(s_idx)]);
            }
            d_idx += 1;
            s_idx -= 1;
            k -= 1;
        }

        // Check that all mandatory positional args are specified.
        // C: `while (d < &state[n_state]) { if (*d++ == NULL) ... }`
        // The `*d++ == NULL` post-increments d, then checks the OLD
        // slot. The argument-number formula: `&state[n_state] - d`
        // (post-increment) — i.e. `n_state - d_idx`.
        while d_idx < n_state {
            let old_d = d_idx;
            d_idx += 1;
            if state[old_d].is_none() {
                return Err(SetupError::MissingPositional {
                    index: n_state - d_idx,
                });
            }
        }

        // Check that all mandatory keyword args are specified.
        // Fill in default kw args if we have them.
        // C: `arg_names = mp_decode_uint_skip(code_state->ip);`
        let mut arg_names2 = decode_uint_skip(ip_after_prelude);
        for _ in 0..sig.n_pos_args {
            arg_names2 = decode_uint_skip(arg_names2);
        }
        for k in 0..sig.n_kwonly_args {
            let idx = decode_uint(&mut arg_names2);
            let arg_qstr = qstr_table[idx];
            if state[n_state - 1 - sig.n_pos_args - k].is_none() {
                let mut found: Option<Value> = None;
                if sig.scope_flags & MP_SCOPE_FLAG_DEFKWARGS != 0 {
                    let def_kw_dict_obj = extra_args[sig.n_def_pos_args];
                    let def_dict = def_kw_dict_obj
                        .as_heap()
                        .expect("defkwargs extra arg is not a heap object");
                    let entries = dict_entries_mut(arena, def_dict);
                    if let Some((pos, _)) = map_lookup(
                        entries,
                        Value::Qstr(u32::from(arg_qstr)),
                        MP_MAP_LOOKUP,
                    ) {
                        found = Some(entries[pos].1);
                    }
                }
                match found {
                    Some(v) => state[n_state - 1 - sig.n_pos_args - k] = Some(v),
                    None => {
                        return Err(SetupError::MissingKeyword {
                            qstr: u32::from(arg_qstr),
                        })
                    }
                }
            }
        }
    } else {
        // no keyword arguments given
        if sig.n_kwonly_args != 0 {
            return Err(SetupError::MissingKeywordOnly);
        }
        if sig.scope_flags & MP_SCOPE_FLAG_VARKEYWORDS != 0 {
            let d = arena.alloc(
                None,
                Obj::Dict(DictData {
                    map: MapData {
                        entries: Vec::new(),
                    },
                }),
                roots,
            );
            state[vpk_slot(vpk_idx)] = Some(Value::Heap(d));
        }
    }

    // jump over code info (source file, argument names and line-number
    // mapping)
    let mut ip2 = &ip_after_prelude[size.n_info..];

    // bytecode prelude: initialise closed over variables
    let mut c = size.n_cell;
    while c != 0 {
        c -= 1;
        let local_num = ip2[0] as usize;
        ip2 = &ip2[1..];
        let cell_in = state[n_state - 1 - local_num];
        let cell = arena.alloc(None, Obj::Cell(CellData { value: cell_in }), roots);
        state[n_state - 1 - local_num] = Some(Value::Heap(cell));
    }

    // now that we skipped over the prelude, set the ip for the VM
    cs.ip = fun_data.len() - ip2.len();
    Ok(())
}

/// `vpk_idx` is a signed slot index (the Zig `@intCast`, which
/// panics on negative in safe builds); convert the same way.
fn vpk_slot(idx: isize) -> usize {
    usize::try_from(idx).expect("varargs slot index went negative — malformed prelude")
}
