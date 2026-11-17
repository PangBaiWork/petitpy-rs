//! Matcher half of the re1.5 port: `_re1_5_classmatch` /
//! `_re1_5_namedclassmatch` (patched `charclass.c`) and
//! `_recursiveloop` (patched `recursiveloop.c`), split out of
//! `re15.rs` so no file exceeds 800 lines. See `re15.rs` for the
//! source note and the public API.

use super::{
    inst_is_consumer, ByteProg, Sub, Subject, ANY, BOL, CHAR, CLASS, CLASS_NAMED_CLASS_INDICATOR,
    CLASS_NOT, EOL, JMP, MATCH, NAMED_CLASS, RSPLIT, SAVE, SPLIT,
};

// ---------------------------------------------------------------------------
// _re1_5_classmatch / _re1_5_namedclassmatch (patched charclass.c —
// identical to the vanilla file)
// ---------------------------------------------------------------------------

/// `_re1_5_classmatch`: `pc` is the index of the count byte (just
/// past the Class/ClassNot opcode) in `insts`; `sp` is the subject
/// byte. Ranges compare as *signed* chars, exactly like the C
/// (`char` is signed on the reference platform) — this matters for
/// bytes >= 0x80.
fn class_match(insts: &[u8], pc: usize, sp: u8) -> bool {
    // pc points to "cnt" byte after opcode.
    let is_positive = insts[pc - 1] == CLASS;
    // C: `int cnt = *pc++` — the count byte read as a *signed* char.
    // compilecode never emits a count above 127 (EMIT_CHECKED), so
    // the loop below runs `cnt` times; a hand-built program with a
    // count byte > 127 would spin in the C (`while (cnt--)` on a
    // negative count never reaches 0) — here it is treated as 0.
    let mut cnt = insts[pc] as i8;
    let mut p = pc + 1;
    while cnt > 0 {
        cnt -= 1;
        if insts[p] == CLASS_NAMED_CLASS_INDICATOR {
            if named_class_match(insts[p + 1], sp) {
                return is_positive;
            }
        } else {
            let lo = insts[p] as i8;
            let hi = insts[p + 1] as i8;
            let c = sp as i8;
            if c >= lo && c <= hi {
                return is_positive;
            }
        }
        p += 2;
    }
    !is_positive
}

/// `_re1_5_namedclassmatch`: `class_byte` is the class letter
/// (`d`/`D`/`s`/`S`/`w`/`W`); `sp` is the subject byte. Returns
/// whether the byte is in the class. Uppercase letters are the
/// negated class (`off` starts at 0 for them, 1 for lowercase).
fn named_class_match(class_byte: u8, sp: u8) -> bool {
    // C: `int off = (*pc >> 5) & 1;` — `*pc` is a signed char.
    let mut off = ((class_byte as i8 as i32) >> 5) & 1;
    // The class tests only involve ASCII ranges, so signed vs
    // unsigned comparison of `sp` makes no difference here.
    if (class_byte | 0x20) == b'd' {
        if !(sp >= b'0' && sp <= b'9') {
            off ^= 1;
        }
    } else if (class_byte | 0x20) == b's' {
        if !(sp == b' ' || (sp >= b'\t' && sp <= b'\r')) {
            off ^= 1;
        }
    } else {
        // w
        if !((sp >= b'A' && sp <= b'Z')
            || (sp >= b'a' && sp <= b'z')
            || (sp >= b'0' && sp <= b'9')
            || sp == b'_')
        {
            off ^= 1;
        }
    }
    off != 0
}

// ---------------------------------------------------------------------------
// recursiveloop (patched recursiveloop.c)
// ---------------------------------------------------------------------------

/// Read a signed int16 little-endian jump offset at `pc`, returning
/// the offset and the index just past it (C: `_read_jmp_off`).
fn read_jmp_off(insts: &[u8], pc: usize) -> (i32, usize) {
    let lo = insts[pc] as i32;
    let hi = insts[pc + 1] as i8 as i32; // Sign-extend.
    (lo | (hi << 8), pc + 2)
}

/// `recursiveloop` (patched recursiveloop.c). `pc` is a byte index
/// into `prog.insts`, `sp` a byte index into `subject.bytes`.
/// `caps` has one [`Sub`] per capture group plus group 0; `nsubp` is
/// the number of C pointer slots (`caps.len() * 2`).
///
/// Differences from the C, by necessity:
/// - `re1_5_stack_chk()` (`mp_cstack_check()`) has no Rust
///   equivalent: this recurses without a stack check. The C aborts
///   the process (rc=134 in this Debug build) on deep recursion such
///   as `(a*)*` vs "aaa"; the Rust version runs on the (much larger)
///   thread stack instead.
/// - An unknown opcode is `panic!` (C: `re1_5_fatal`, i.e.
///   `assert(!x)` in modre.c).
/// - Positions past the end of `insts` would read out of bounds in
///   the C; here indexing panics. Unreachable for programs produced
///   by [`compilecode`].
pub(crate) fn recursiveloop(
    prog: &ByteProg,
    mut pc: usize,
    mut sp: usize,
    subj: &Subject,
    caps: &mut [Sub],
    nsubp: usize,
) -> bool {
    let insts = &prog.insts;
    loop {
        if inst_is_consumer(insts[pc]) {
            // If we need to match a character, but there's none left,
            // it's fail.
            if sp >= subj.end {
                return false;
            }
        }
        let op = insts[pc];
        pc += 1;
        match op {
            CHAR => {
                if subj.bytes[sp] != insts[pc] {
                    return false;
                }
                pc += 1;
                // MP_FALLTHROUGH to Any.
                sp += 1;
            }
            ANY => {
                sp += 1;
            }
            CLASS | CLASS_NOT => {
                if !class_match(insts, pc, subj.bytes[sp]) {
                    return false;
                }
                pc += insts[pc] as usize * 2 + 1;
                sp += 1;
            }
            NAMED_CLASS => {
                if !named_class_match(insts[pc], subj.bytes[sp]) {
                    return false;
                }
                pc += 1;
                sp += 1;
            }
            MATCH => {
                return true;
            }
            JMP => {
                let (off, next) = read_jmp_off(insts, pc);
                pc = (next as isize + off as isize) as usize;
            }
            SPLIT => {
                let (off, next) = read_jmp_off(insts, pc);
                if recursiveloop(prog, next, sp, subj, caps, nsubp) {
                    return true;
                }
                pc = (next as isize + off as isize) as usize;
            }
            RSPLIT => {
                let (off, next) = read_jmp_off(insts, pc);
                if recursiveloop(prog, (next as isize + off as isize) as usize, sp, subj, caps, nsubp)
                {
                    return true;
                }
                pc = next;
            }
            SAVE => {
                let off = insts[pc] as usize;
                pc += 1;
                if off >= nsubp {
                    continue;
                }
                // Slot 2*i is group i's start, 2*i+1 its end.
                let idx = off / 2;
                let is_start = off % 2 == 0;
                let old = if is_start {
                    std::mem::replace(&mut caps[idx].start, Some(sp))
                } else {
                    std::mem::replace(&mut caps[idx].end, Some(sp))
                };
                if recursiveloop(prog, pc, sp, subj, caps, nsubp) {
                    return true;
                }
                if is_start {
                    caps[idx].start = old;
                } else {
                    caps[idx].end = old;
                }
                return false;
            }
            BOL => {
                if sp != subj.begin_line {
                    return false;
                }
            }
            EOL => {
                if sp != subj.end {
                    return false;
                }
            }
            _ => {
                panic!("recursiveloop: invalid opcode {:#04x} at {}", op, pc - 1);
            }
        }
    }
}
