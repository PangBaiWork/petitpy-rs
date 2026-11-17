//! Compiler half of the re1.5 port: `_compilecode` +
//! `re1_5_compilecode` (patched `compilecode.c`), split out of
//! `re15.rs` so no file exceeds 800 lines. See `re15.rs` for the
//! source note and the public API.

use super::{
    byte_at, emit_checked, emit_jmp_checked, insert_code, is_named_class_char, rel, ByteProg,
    CompileError, CompileErrorKind, ANY, BOL, CHAR, CLASS, CLASS_NAMED_CLASS_INDICATOR,
    CLASS_NOT, EOL, JMP, MATCH, NAMED_CLASS, RSPLIT, SAVE, SPLIT,
};

// ---------------------------------------------------------------------------
// _compilecode (patched compilecode.c)
// ---------------------------------------------------------------------------

/// Compile one alternation level starting at pattern offset `i`.
/// Returns the offset of the closing `)` (or the end of the pattern)
/// on success. `err` collects the first checked-emit overflow; it is
/// reported at the end, exactly like the C (`return err ? NULL : re`).
#[allow(clippy::too_many_lines)]
fn compile_code(
    pat: &[u8],
    mut i: usize,
    prog: &mut ByteProg,
    err: &mut Option<CompileErrorKind>,
) -> Result<usize, CompileError> {
    let start = prog.insts.len();
    let mut term = prog.insts.len();
    let mut alt_label: Option<usize> = None;

    // C: `for (; *re && *re != ')'; re++)` — the trailing `i += 1`
    // at the bottom of this loop is the for-loop's `re++`. A NUL
    // byte ends the pattern too: the C pattern is a NUL-terminated
    // string, so everything from the first NUL on is invisible to
    // the C engine (modre.c passes `const char *`).
    while i < pat.len() && pat[i] != 0 && pat[i] != b')' {
        match pat[i] {
            b'\\' => {
                i += 1;
                if i >= pat.len() {
                    return Err(CompileError::new(CompileErrorKind::TrailingBackslash));
                }
                if is_named_class_char(pat[i]) {
                    term = prog.insts.len();
                    prog.insts.push(NAMED_CLASS);
                    prog.insts.push(pat[i]);
                    prog.len += 1;
                } else {
                    // MP_FALLTHROUGH to default: literal byte.
                    term = prog.insts.len();
                    prog.insts.push(CHAR);
                    prog.insts.push(pat[i]);
                    prog.len += 1;
                }
            }
            b'.' => {
                term = prog.insts.len();
                prog.insts.push(ANY);
                prog.len += 1;
            }
            b'[' => {
                let mut cnt: i32 = 0;
                term = prog.insts.len();
                i += 1;
                if byte_at(pat, i) == b'^' {
                    prog.insts.push(CLASS_NOT);
                    i += 1;
                } else {
                    prog.insts.push(CLASS);
                }
                prog.insts.push(0); // Skip # of pair byte.
                prog.len += 1;
                // C: `for (cnt = 0; *re != ']'; re++, cnt++)`.
                while byte_at(pat, i) != b']' {
                    let mut c = byte_at(pat, i);
                    if c == b'\\' {
                        i += 1;
                        c = byte_at(pat, i);
                        if is_named_class_char(c) {
                            // Named class inside a set: emit the
                            // indicator byte + the class letter.
                            prog.insts.push(CLASS_NAMED_CLASS_INDICATOR);
                            prog.insts.push(byte_at(pat, i));
                            cnt += 1;
                            i += 1;
                            continue;
                        }
                    }
                    if c == 0 {
                        return Err(CompileError::new(CompileErrorKind::UnterminatedClass));
                    }
                    if byte_at(pat, i + 1) == b'-' && byte_at(pat, i + 2) != b']' {
                        i += 2;
                    }
                    prog.insts.push(c);
                    prog.insts.push(byte_at(pat, i));
                    cnt += 1;
                    i += 1;
                }
                emit_checked(prog, term + 1, cnt, err);
            }
            b'(' => {
                term = prog.insts.len();
                let mut sub = 0usize;
                let capture = byte_at(pat, i + 1) != b'?' || byte_at(pat, i + 2) != b':';

                if capture {
                    sub = prog.sub + 1;
                    prog.sub = sub;
                    prog.insts.push(SAVE);
                    prog.insts.push(0); // Slot patched below.
                    emit_checked(prog, prog.insts.len() - 1, 2 * sub as i32, err);
                    prog.len += 1;
                } else {
                    i += 2;
                }

                i = compile_code(pat, i + 1, prog, err)?;
                if byte_at(pat, i) != b')' {
                    return Err(CompileError::new(CompileErrorKind::UnmatchedParen));
                }

                if capture {
                    prog.insts.push(SAVE);
                    prog.insts.push(0); // Slot patched below.
                    emit_checked(prog, prog.insts.len() - 1, 2 * sub as i32 + 1, err);
                    prog.len += 1;
                }
            }
            b'?' => {
                if prog.insts.len() == term {
                    return Err(CompileError::new(CompileErrorKind::NothingToRepeat));
                }
                // Insert a jump (3 bytes) at `term`, pushing the
                // existing body forward.
                insert_code(prog, term, 3);
                if byte_at(pat, i + 1) == b'?' {
                    prog.insts[term] = RSPLIT;
                    i += 1;
                } else {
                    prog.insts[term] = SPLIT;
                }
                emit_jmp_checked(prog, term + 1, rel(term, prog.insts.len()), err);
                prog.len += 1;
                term = prog.insts.len();
            }
            b'*' => {
                if prog.insts.len() == term {
                    return Err(CompileError::new(CompileErrorKind::NothingToRepeat));
                }
                insert_code(prog, term, 3);
                let jmp_at = prog.insts.len();
                prog.insts.push(JMP);
                prog.insts.push(0);
                prog.insts.push(0);
                emit_jmp_checked(prog, jmp_at + 1, rel(jmp_at, term), err);
                if byte_at(pat, i + 1) == b'?' {
                    prog.insts[term] = RSPLIT;
                    i += 1;
                } else {
                    prog.insts[term] = SPLIT;
                }
                emit_jmp_checked(prog, term + 1, rel(term, prog.insts.len()), err);
                prog.len += 2;
                term = prog.insts.len();
            }
            b'+' => {
                if prog.insts.len() == term {
                    return Err(CompileError::new(CompileErrorKind::NothingToRepeat));
                }
                let at = prog.insts.len();
                if byte_at(pat, i + 1) == b'?' {
                    prog.insts.push(SPLIT);
                    i += 1;
                } else {
                    prog.insts.push(RSPLIT);
                }
                prog.insts.push(0);
                prog.insts.push(0);
                emit_jmp_checked(prog, at + 1, rel(at, term), err);
                prog.len += 1;
                term = prog.insts.len();
            }
            b'|' => {
                if let Some(label) = alt_label {
                    emit_jmp_checked(prog, label, rel(label, prog.insts.len()) + 1, err);
                }
                // Insert 3 bytes at `start` (Split + 2-byte offset),
                // push existing body forward. Emit trailing Jmp
                // (3 bytes) and reserve alt_label for the offset to
                // be patched later.
                insert_code(prog, start, 3);
                let jmp_at = prog.insts.len();
                prog.insts.push(JMP);
                prog.insts.push(0);
                prog.insts.push(0);
                alt_label = Some(jmp_at + 1);
                prog.insts[start] = SPLIT;
                emit_jmp_checked(prog, start + 1, rel(start, prog.insts.len()), err);
                prog.len += 2;
                term = prog.insts.len();
            }
            b'^' => {
                prog.insts.push(BOL);
                prog.len += 1;
                term = prog.insts.len();
            }
            b'$' => {
                prog.insts.push(EOL);
                prog.len += 1;
                term = prog.insts.len();
            }
            _ => {
                term = prog.insts.len();
                prog.insts.push(CHAR);
                prog.insts.push(pat[i]);
                prog.len += 1;
            }
        }
        i += 1;
    }

    if let Some(label) = alt_label {
        emit_jmp_checked(prog, label, rel(label, prog.insts.len()) + 1, err);
    }
    match *err {
        Some(kind) => Err(CompileError::new(kind)),
        None => Ok(i),
    }
}

// ---------------------------------------------------------------------------
// re1_5_sizecode / re1_5_compilecode (patched compilecode.c)
// ---------------------------------------------------------------------------

/// Compile `pattern` into `prog`, writing the 7-byte non-anchored
/// prefix and the `Save 0` / `Save 1` / `Match` framing exactly like
/// `re1_5_compilecode`. `check_trailing` is true for `compilecode`
/// (a stray `)` is an error) and false for `sizecode` (the C
/// `sizecode` only runs `_compilecode` and never checks the tail —
/// so for `a)` it returns a size while `compilecode` fails; modre.c
/// raises "error in regex" either way in this build).
pub(crate) fn compile_into(
    prog: &mut ByteProg,
    pattern: &[u8],
    check_trailing: bool,
) -> Result<(), CompileError> {
    prog.len = 0;
    prog.sub = 0;
    prog.insts.clear();

    // Non-anchored ("search") prefix — 7 bytes with the widened
    // jumps: RSplit +4 (skip Any+Jmp on split fail), Any, Jmp -7
    // (loop back to RSplit). Anchored ("match") skips this via
    // NON_ANCHORED_PREFIX.
    prog.insts.extend_from_slice(&[RSPLIT, 4, 0, ANY, JMP, 0xf9, 0xff]);
    prog.len += 3;

    prog.insts.push(SAVE);
    prog.insts.push(0);
    prog.len += 1;

    let mut err: Option<CompileErrorKind> = None;
    let pos = compile_code(pattern, 0, prog, &mut err)?;
    if check_trailing && byte_at(pattern, pos) != 0 {
        return Err(CompileError::new(CompileErrorKind::UnexpectedCloseParen));
    }

    prog.insts.push(SAVE);
    prog.insts.push(1);
    prog.len += 1;

    prog.insts.push(MATCH);
    prog.len += 1;

    prog.bytelen = prog.insts.len();
    Ok(())
}
