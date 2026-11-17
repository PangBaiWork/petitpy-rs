//! re1.5 regex engine, ported to Rust.
//!
//! Source (petitpy devel HEAD `df7de3a` — the files actually compiled
//! by `make build`): `port/re1.5-patched/` — `re1.5.h` (156 lines),
//! `compilecode.c` (239), `charclass.c` (41), `recursiveloop.c` (87),
//! `dumpcode.c` (65). `port/Makefile:103-110` copies the vanilla
//! vendor `lib/re1.5/*` into the embed package and then overlays it
//! with `port/re1.5-patched/*` in full; `build.zig:747` compiles the
//! embed copy. The patch (2026-07-20) widens jump offsets from int8
//! to int16 little-endian (every `Jmp`/`Split`/`RSplit` is now 3
//! bytes) and grows `NON_ANCHORED_PREFIX` from 5 to 7. This port
//! follows the patched files, not the vanilla vendor ones — see
//! `scripts/re15_result.txt` for the vanilla-vs-patched delta.
//!
//! The caller of this engine is `vendor/micropython/extmod/modre.c`
//! (the `ure` module; Phase D ports it). It uses exactly four entry
//! points, mirrored here: [`sizecode`], [`compilecode`],
//! [`recursiveloopprog`], [`dumpcode`]. The compiled bytecode is an
//! in-memory structure, not a persistent format — but the instruction
//! encoding is ported 1:1 so the Phase D modre port can follow
//! `modre.c` mechanically.
//!
//! Deliberate deviations from the C (all documented in the result
//! file too):
//! - `re1_5_fatal` (C: `assert(!x)` in modre.c) is Rust `panic!`.
//! - `re1_5_stack_chk` (C: `mp_cstack_check()`) has no Rust
//!   equivalent; the matcher recurses without a stack check.
//! - `ByteProg::bytelen` is kept equal to `insts.len()` by
//!   construction (the C updates it incrementally; same invariant).
//! - Errors: C returns NULL / -1 / 1; Rust returns
//!   [`CompileError`] (kinds name the exact C failure site) and
//!   `None` from [`sizecode`].

#[path = "re15_compile.rs"]
mod re15_compile;
#[path = "re15_match.rs"]
mod re15_match;

use self::re15_compile::compile_into;
use self::re15_match::recursiveloop;

// ---------------------------------------------------------------------------
// Opcodes (re1.5.h: `enum /* Inst.opcode */`). Values are part of the
// bytecode encoding — do not renumber.
// ---------------------------------------------------------------------------

/// Instructions which consume input bytes (and thus fail if none left).
pub const CONSUMERS: u8 = 1;
/// Match a literal byte (operand: the byte).
pub const CHAR: u8 = 1;
/// Match any single byte.
pub const ANY: u8 = 2;
/// Match a character class (operands: count byte, then count pairs).
pub const CLASS: u8 = 3;
/// Match a negated character class.
pub const CLASS_NOT: u8 = 4;
/// Match a named class (operand: the class letter, e.g. `d`/`D`).
pub const NAMED_CLASS: u8 = 5;

/// Instructions below this value consume input.
pub const ASSERTS: u8 = 0x50;
/// Assert at the beginning of a line.
pub const BOL: u8 = 0x50;
/// Assert at the end of the subject.
pub const EOL: u8 = 0x51;

/// Instructions which take a relative offset as argument.
pub const JUMPS: u8 = 0x60;
/// Unconditional jump (operand: int16 little-endian offset).
pub const JMP: u8 = 0x60;
/// Split: try the next instruction first, else jump.
pub const SPLIT: u8 = 0x61;
/// Split: try the jump target first, else fall through.
pub const RSPLIT: u8 = 0x62;

/// Save the current position into a capture slot (operand: slot).
pub const SAVE: u8 = 0x7e;
/// Successful match.
pub const MATCH: u8 = 0x7f;

/// Bytes of the non-anchored ("search") prefix (patched value; the
/// vanilla engine used 5). `recursiveloopprog` skips this many bytes
/// when `anchored` is true.
pub const NON_ANCHORED_PREFIX: usize = 7;

/// Byte stored in a class pair to mark a named-class entry
/// (re1.5.h: `RE15_CLASS_NAMED_CLASS_INDICATOR`).
pub const CLASS_NAMED_CLASS_INDICATOR: u8 = 0;

fn inst_is_consumer(op: u8) -> bool {
    op < ASSERTS
}

// ---------------------------------------------------------------------------
// Types (re1.5.h: `struct ByteProg`, `struct Subject`)
// ---------------------------------------------------------------------------

/// A compiled regex program (C: `struct ByteProg` — `bytelen`, `len`,
/// `sub`, `insts[]`).
///
/// `bytelen` is the byte length of `insts` (kept in sync by
/// construction), `len` is the instruction count (as printed by
/// `dumpcode`), `sub` is the number of capture groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ByteProg {
    pub bytelen: usize,
    pub len: usize,
    pub sub: usize,
    pub insts: Vec<u8>,
}

impl ByteProg {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One capture-group span (the C `modre.c` caller keeps a flat
/// `const char *caps[(sub + 1) * 2]` array: slot `2*i` is group `i`'s
/// start, slot `2*i + 1` its end, `NULL` when the group did not
/// participate). `caps` is therefore a slice of `sub + 1` [`Sub`]s;
/// a group is unmatched when `start` is `None` (modre.c only tests
/// the start pointer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sub {
    pub start: Option<usize>,
    pub end: Option<usize>,
}

impl Sub {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the group participated in the match.
    pub fn is_matched(&self) -> bool {
        self.start.is_some()
    }
}

/// The subject of a match (C: `struct Subject` — `begin_line`,
/// `begin`, `end` are byte offsets into `bytes`).
///
/// `begin_line` is the start of the string (what `^` anchors to),
/// `begin` is where the search starts (modre.c `pos`), `end` is
/// where it ends (modre.c `endpos`). They differ exactly when the
/// caller passes start/end positions, as in `re_start_end_pos.py`.
#[derive(Debug, Clone, Copy)]
pub struct Subject<'a> {
    pub bytes: &'a [u8],
    pub begin_line: usize,
    pub begin: usize,
    pub end: usize,
}

impl<'a> Subject<'a> {
    /// The whole byte string, search from its start to its end.
    pub fn whole(bytes: &'a [u8]) -> Self {
        Subject {
            bytes,
            begin_line: 0,
            begin: 0,
            end: bytes.len(),
        }
    }

    pub fn new(bytes: &'a [u8], begin_line: usize, begin: usize, end: usize) -> Self {
        Subject {
            bytes,
            begin_line,
            begin,
            end,
        }
    }
}

// ---------------------------------------------------------------------------
// Compile errors (C: `_compilecode` returns NULL; `re1_5_sizecode`
// returns -1; `re1_5_compilecode` returns 1. The C does not
// distinguish causes — the kinds below name the exact return-NULL
// site in `compilecode.c`, for diagnostics only.)
// ---------------------------------------------------------------------------

/// Why compilation failed. Each variant is one `return NULL` site in
/// `_compilecode` (patched `compilecode.c`), or the deferred
/// `EMIT_CHECKED` / `EMIT_JMP_CHECKED` overflow flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileErrorKind {
    /// Pattern ends with a backslash (`compilecode.c`: `case '\\'`).
    TrailingBackslash,
    /// `[` class runs into the end of the pattern (`case '['`).
    UnterminatedClass,
    /// `(` without a matching `)` (`case '('`).
    UnmatchedParen,
    /// Stray `)` at the top level (`re1_5_compilecode`: `*re != 0`).
    UnexpectedCloseParen,
    /// `?`, `*` or `+` with no preceding atom (`PC == term`).
    NothingToRepeat,
    /// A jump offset does not fit in int16 (`EMIT_JMP_CHECKED`).
    OffsetOverflow,
    /// A Save slot (`2 * sub`) or class pair count does not fit in
    /// int8 (`EMIT_CHECKED`).
    SlotOverflow,
}

/// A compile failure. `Display` gives the C-site name; the reference
/// binary reports every compile failure as `ValueError: error in
/// regex` (modre.c raises "regex too complex" only when
/// `MICROPY_ERROR_REPORTING >= NORMAL`, which this build does not
/// set — verified on the reference binary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileError {
    pub kind: CompileErrorKind,
}

impl CompileError {
    fn new(kind: CompileErrorKind) -> Self {
        CompileError { kind }
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.kind {
            CompileErrorKind::TrailingBackslash => "trailing backslash in pattern",
            CompileErrorKind::UnterminatedClass => "unterminated character class",
            CompileErrorKind::UnmatchedParen => "unmatched '(' in pattern",
            CompileErrorKind::UnexpectedCloseParen => "unexpected ')' in pattern",
            CompileErrorKind::NothingToRepeat => "nothing to repeat",
            CompileErrorKind::OffsetOverflow => "regex too complex (jump offset overflow)",
            CompileErrorKind::SlotOverflow => "regex too complex (too many groups or class pairs)",
        };
        write!(f, "{}", s)
    }
}

impl std::error::Error for CompileError {}

// ---------------------------------------------------------------------------
// Compile helpers (patched compilecode.c: EMIT / EMIT_CHECKED /
// EMIT_JMP_CHECKED / INSERT_CODE / REL)
// ---------------------------------------------------------------------------

/// Read pattern byte `i`, or 0 past the end — the C pattern is a
/// NUL-terminated string, so `re[1]` / `re[2]` peeks at the end read
/// the terminator (0). (The C reads one byte past the terminator for
/// `re[2]` at the very end; in practice that byte is 0.)
fn byte_at(pat: &[u8], i: usize) -> u8 {
    if i < pat.len() {
        pat[i]
    } else {
        0
    }
}

/// `REL(at, to)` (patched): jump offsets are relative to the byte
/// *after* the 3-byte instruction (opcode + int16).
fn rel(at: usize, to: usize) -> i32 {
    to as i32 - at as i32 - 3
}

/// `_emit_checked`: 1-byte checked emit, for Save slots and class
/// counts. Records the first overflow in `err` (the C keeps a single
/// `bool err` flag, checked at the end of `_compilecode`).
fn emit_checked(prog: &mut ByteProg, at: usize, val: i32, err: &mut Option<CompileErrorKind>) {
    if err.is_none() && val != val as i8 as i32 {
        *err = Some(CompileErrorKind::SlotOverflow);
    }
    prog.insts[at] = val as u8;
}

/// `_emit_jmp_checked`: 2-byte checked emit, little-endian int16, for
/// jump offsets.
fn emit_jmp_checked(prog: &mut ByteProg, at: usize, val: i32, err: &mut Option<CompileErrorKind>) {
    if err.is_none() && val != val as i16 as i32 {
        *err = Some(CompileErrorKind::OffsetOverflow);
    }
    prog.insts[at] = (val & 0xff) as u8;
    prog.insts[at + 1] = ((val >> 8) & 0xff) as u8;
}

/// `INSERT_CODE(at, num, pc)`: insert `num` zero bytes at `at`,
/// shifting the code after it right (the C memmoves when a buffer
/// exists; in sizecode mode it only advances `pc` — the resulting
/// byte length is identical either way).
fn insert_code(prog: &mut ByteProg, at: usize, num: usize) {
    prog.insts.splice(at..at, std::iter::repeat_n(0u8, num));
}

/// `MATCH_NAMED_CLASS_CHAR(c)`: `d`/`D`, `s`/`S`, `w`/`W`.
fn is_named_class_char(c: u8) -> bool {
    (c | 0x20) == b'd' || (c | 0x24) == b'w'
}

/// `re1_5_sizecode`: the number of bytes a compiled program for
/// `pattern` needs, or `None` when the pattern is invalid. (The C
/// returns -1 on failure; `None` is its Rust form.)
pub fn sizecode(pattern: &[u8]) -> Option<usize> {
    let mut prog = ByteProg::new();
    match compile_into(&mut prog, pattern, false) {
        Ok(()) => Some(prog.bytelen),
        Err(_) => None,
    }
}

/// `re1_5_compilecode`: compile `pattern` into `prog` (reset first,
/// as the C does). On failure `prog` is left partially filled, as
/// the C leaves it — the caller (modre.c) discards it.
pub fn compilecode(prog: &mut ByteProg, pattern: &[u8]) -> Result<(), CompileError> {
    compile_into(prog, pattern, true)
}


/// `re1_5_recursiveloopprog`, C-faithful form: takes the full
/// [`Subject`] (modre.c sets `begin_line`/`begin`/`end` from the
/// string and the `pos`/`endpos` arguments). `caps` must have
/// `prog.sub + 1` entries (group 0 plus one per capture group).
pub fn recursiveloopprog_subject(
    prog: &ByteProg,
    subject: &Subject,
    caps: &mut [Sub],
    anchored: bool,
) -> bool {
    let start = if anchored { NON_ANCHORED_PREFIX } else { 0 };
    let nsubp = caps.len() * 2;
    recursiveloop(prog, start, subject.begin, subject, caps, nsubp)
}

/// `re1_5_recursiveloopprog` on a whole byte string (the brief's
/// signature): `begin_line == begin == 0`, `end == subject.len()`.
/// For start/end positions use [`recursiveloopprog_subject`].
pub fn recursiveloopprog(
    prog: &ByteProg,
    subject: &[u8],
    caps: &mut [Sub],
    anchored: bool,
) -> bool {
    recursiveloopprog_subject(prog, &Subject::whole(subject), caps, anchored)
}

// ---------------------------------------------------------------------------
// re1_5_dumpcode (patched dumpcode.c)
// ---------------------------------------------------------------------------

/// `re1_5_dumpcode`, as a `String` (the C prints to stdout; the
/// reference build does not compile it — `MICROPY_PY_RE_DEBUG` is
/// off — so this is a debug aid, verified against a standalone build
/// of the patched C sources, see the result file).
///
/// Byte-fidelity notes vs the C `printf`:
/// - `char %c` prints the raw byte; Rust maps it through Latin-1
///   (`byte as char`), which agrees for ASCII.
/// - `class ... 0x%02x` prints a *signed* char promoted to int, so a
///   pair byte >= 0x80 prints as e.g. `ffffff80`; the casts below
///   reproduce that.
pub fn dumpcode(prog: &ByteProg) -> String {
    let code = &prog.insts;
    let mut out = String::new();
    let mut pc = 0usize;
    let dump_off = |code: &[u8], pc: usize| -> i32 {
        let lo = code[pc] as i32;
        let hi = code[pc + 1] as i8 as i32;
        lo | (hi << 8)
    };
    while pc < prog.bytelen {
        out.push_str(&format!("{:2}: ", pc));
        let op = code[pc];
        pc += 1;
        match op {
            SPLIT => {
                let off = dump_off(code, pc);
                out.push_str(&format!("split {} ({})\n", pc as i32 + off + 2, off));
                pc += 2;
            }
            RSPLIT => {
                let off = dump_off(code, pc);
                out.push_str(&format!("rsplit {} ({})\n", pc as i32 + off + 2, off));
                pc += 2;
            }
            JMP => {
                let off = dump_off(code, pc);
                out.push_str(&format!("jmp {} ({})\n", pc as i32 + off + 2, off));
                pc += 2;
            }
            CHAR => {
                out.push_str(&format!("char {}\n", code[pc] as char));
                pc += 1;
            }
            ANY => {
                out.push_str("any\n");
            }
            CLASS | CLASS_NOT => {
                let mut num = code[pc] as i32;
                out.push_str(&format!(
                    "class{} {}",
                    if code[pc - 1] == CLASS_NOT { "not" } else { "" },
                    num
                ));
                pc += 1;
                while num > 0 {
                    num -= 1;
                    // C passes a signed char to %02x: sign-extended.
                    out.push_str(&format!(
                        " 0x{:02x}-0x{:02x}",
                        code[pc] as i8 as i32 as u32,
                        code[pc + 1] as i8 as i32 as u32
                    ));
                    pc += 2;
                }
                out.push('\n');
            }
            NAMED_CLASS => {
                out.push_str(&format!("namedclass {}\n", code[pc] as char));
                pc += 1;
            }
            MATCH => {
                out.push_str("match\n");
            }
            SAVE => {
                out.push_str(&format!("save {}\n", code[pc]));
                pc += 1;
            }
            BOL => {
                out.push_str("assert bol\n");
            }
            EOL => {
                out.push_str("assert eol\n");
            }
            _ => {
                panic!("dumpcode: invalid opcode {:#04x} at {}", op, pc - 1);
            }
        }
    }
    out.push_str(&format!("Bytes: {}, insts: {}\n", prog.bytelen, prog.len));
    out
}
