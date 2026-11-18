//! str / bytes methods, part 1 (Lane C8a).
//!
//! Port of `src/objects/strings/objstr.zig` lines 685–1860 (seams
//! 4/7/8/11): `get_substring_data`, the find family, startswith/
//! endswith, replace, join, split/rsplit, strip/lstrip/rstrip.
//!
//! Calling convention: every function takes `args` WITH `self` at
//! `args[0]`, exactly as the `fun_builtin_var` wrappers receive
//! them after `argcheck` (e.g. `str_find` is `var(2, 4)` —
//! `args = [self, needle, start?, end?]`). The signatures take
//! `&BuiltinTypes` explicitly (the C6 precedent — `obj.rs` helpers
//! do the same); how the VM threads `types` into a `BuiltinFn`
//! (`fn(&mut CallCtx, &[Value])`, no types parameter) is a C10/D1
//! interface question, recorded in the result file — this lane does
//! not change the frozen slot signatures to answer it.

use crate::memory::gc::CallCtx;
use crate::objects::obj::{obj_get_int, obj_get_type, BuiltinTypes};
use crate::objects::strings::objstr_a::*;
use crate::objects::strings::unicode::{
    unichar_isspace, utf8_ptr_to_index,
};
use crate::objects::value::{ListData, Obj, Value};

// ── get_substring_data ────────────────────────────────────────────────

/// `get_substring_data` (`objstr.zig:685-708`): parse optional
/// `[start, end]` args (from `args`, WITHOUT self) and compute the
/// `(byte_offset, len)` sub-range of `obj`'s data. `end < start`
/// clamps the length to 0.
pub fn get_substring_data(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    obj: Value,
    args: &[Value],
) -> Result<(usize, usize), StrError> {
    // Copy first: `str_data` borrows the arena, but
    // `str_index_to_ptr` below takes `&mut CallCtx`. Pool-aware:
    // the subject is often a dynamic-qstr literal (D5).
    let data = str_data_pool(ctx.arena, ctx.pool, obj).ok_or(StrError::BadImplicitConversion)?;
    let is_bytes = is_bytes_type(ctx.arena, types, obj);
    let mut start = 0usize;
    let mut len = data.len();
    if !args.is_empty() {
        let mut end = data.len();
        if args.len() > 1 && args[1] != Value::None {
            end = str_index_to_ptr(ctx, types, &data, is_bytes, args[1], true)?;
        }
        if args[0] != Value::None {
            start = str_index_to_ptr(ctx, types, &data, is_bytes, args[0], true)?;
        }
        len = end.saturating_sub(start);
    }
    Ok((start, len))
}

// ── find family ───────────────────────────────────────────────────────

/// `strFinderImpl` (`objstr.zig:734-781`): the shared core of
/// find/rfind/index/rindex. `direction > 0` = forward (first
/// occurrence), `< 0` = backward (last). Not found: `is_index`
/// raises `ValueError: substring not found`, else returns -1.
/// Found: a unicode str reports the code-point index, bytes the
/// byte offset.
pub fn str_finder(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
    direction: i32,
    is_index: bool,
) -> Result<Value, StrError> {
    let self_value = args[0];
    let self_type = obj_get_type(ctx.arena, types, self_value);
    str_check_arg_type(ctx.arena, types, self_type, args[1])?;
    // Copy first: `str_data` borrows the arena, but
    // `str_index_to_ptr` below takes `&mut CallCtx`. Pool-aware
    // (D5): the haystack/needle are often dynamic-qstr literals.
    let haystack = str_data_pool(ctx.arena, ctx.pool, self_value).ok_or(StrError::BadImplicitConversion)?;
    let needle = str_data_pool(ctx.arena, ctx.pool, args[1]).ok_or(StrError::BadImplicitConversion)?;
    let is_bytes = is_bytes_type(ctx.arena, types, self_value);
    let is_str = is_str_type(ctx.arena, types, self_value);

    let mut start = 0usize;
    let mut end = haystack.len();
    if args.len() >= 3 && args[2] != Value::None {
        start = str_index_to_ptr(ctx, types, &haystack, is_bytes, args[2], true)?;
    }
    if args.len() >= 4 && args[3] != Value::None {
        end = str_index_to_ptr(ctx, types, &haystack, is_bytes, args[3], true)?;
    }

    let mut p = None;
    if end >= start {
        p = find_subbytes(&haystack[start..end], &needle, direction).map(|o| start + o);
    }
    match p {
        None => {
            if is_index {
                Err(StrError::SubstrNotFound)
            } else {
                Ok(Value::SmallInt(-1))
            }
        }
        Some(off) => {
            if is_str {
                Ok(Value::SmallInt(utf8_ptr_to_index(&haystack, off) as i64))
            } else {
                Ok(Value::SmallInt(off as i64))
            }
        }
    }
}

/// `str_find` (`objstr.zig:769-771`).
pub fn str_find(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_finder(ctx, types, args, 1, false)
}

/// `str_rfind` (`objstr.zig:772-774`).
pub fn str_rfind(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_finder(ctx, types, args, -1, false)
}

/// `str_index` (`objstr.zig:775-777`).
pub fn str_index(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_finder(ctx, types, args, 1, true)
}

/// `str_rindex` (`objstr.zig:778-780`).
pub fn str_rindex(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_finder(ctx, types, args, -1, true)
}

// ── startswith / endswith ─────────────────────────────────────────────

/// `strStartendswithImpl` (`objstr.zig:783-821`): `args[0]` is self,
/// `args[1]` is one prefix or a tuple of them, `args[2..]` are the
/// optional `[start, end]`. The prefixes are NOT type-checked
/// against self — only their data is read (`mp_obj_str_get_data`),
/// so `"abc".startswith(b"a")` is `True` on the reference binary;
/// a non-str/bytes prefix raises "can't convert to str implicitly".
fn str_startendswith(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
    ends_with: bool,
) -> Result<Value, StrError> {
    let (start, str_len) = get_substring_data(ctx, types, args[0], &args[2..])?;
    // Pool-aware (D5): subject and prefix are often dynamic-qstr
    // literals — the arena-only `str_data` returns `None` for them.
    let data = str_data_pool(ctx.arena, ctx.pool, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let mut prefixes: Vec<Value> = vec![args[1]];
    if let Value::Heap(r) = args[1] {
        if let Obj::Tuple(items) = ctx.arena.get(r) {
            prefixes = items.clone();
        }
    }
    for prefix in prefixes {
        let prefix_data = str_data_pool(ctx.arena, ctx.pool, prefix).ok_or(StrError::BadImplicitConversion)?;
        let prefix_len = prefix_data.len();
        let s_offset = if ends_with {
            if str_len >= prefix_len {
                str_len - prefix_len
            } else {
                0
            }
        } else {
            0
        };
        if prefix_len <= str_len
            && data[start + s_offset..start + s_offset + prefix_len] == prefix_data[..]
        {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// `str_startswith` (`objstr.zig:823-825`).
pub fn str_startswith(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_startendswith(ctx, types, args, false)
}

/// `str_endswith` (`objstr.zig:826-828`).
pub fn str_endswith(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_startendswith(ctx, types, args, true)
}

// ── replace ───────────────────────────────────────────────────────────

/// One pass of the `str_replace` two-pass algorithm
/// (`objstr.zig:1313-1364`): scan `s` for `old`, writing the
/// replacement into `out` when given. Returns `(output_len,
/// num_replacements_done)`. Pass 1 (`out == None`) only measures;
/// pass 2 fills the buffer. `max_rep < 0` means unlimited.
fn replace_pass(
    s: &[u8],
    old: &[u8],
    new: &[u8],
    max_rep: i64,
    mut out: Option<&mut Vec<u8>>,
) -> (usize, usize) {
    let mut replaced_str_index: usize = 0;
    let mut num_replacements_done: usize = 0;
    let mut offset: usize = 0;
    let mut str_len_remain: usize = s.len();
    if old.is_empty() {
        // Empty needle: the replacement starts at index 0.
        if let Some(o) = out.as_mut() {
            o.extend_from_slice(new);
        }
        replaced_str_index += new.len();
        num_replacements_done += 1;
    }
    while (max_rep < 0 || num_replacements_done < max_rep as usize) && str_len_remain > 0 {
        let Some(occ_rel) = find_subbytes(&s[offset..offset + str_len_remain], old, 1) else {
            break;
        };
        let mut occ = offset + occ_rel;
        if old.is_empty() {
            occ += 1;
        }
        let pre_len = occ - offset;
        if let Some(o) = out.as_mut() {
            o.extend_from_slice(&s[offset..offset + pre_len]);
            o.extend_from_slice(new);
        }
        replaced_str_index += pre_len + new.len();
        offset = occ + old.len();
        str_len_remain = s.len() - offset;
        num_replacements_done += 1;
    }
    // Copy the tail.
    if let Some(o) = out.as_mut() {
        o.extend_from_slice(&s[offset..offset + str_len_remain]);
    }
    replaced_str_index += str_len_remain;
    (replaced_str_index, num_replacements_done)
}

/// `str_replace` (`objstr.zig:1306-1372`): `args = [self, old, new,
/// count?]` (count defaults to -1, unlimited; 0 returns self
/// unchanged; negative means unlimited). `old` longer than self
/// returns self unchanged; no occurrence returns self unchanged
/// (the SAME object, not a copy).
pub fn str_replace(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let mut max_rep: i64 = -1;
    if args.len() == 4 {
        max_rep = obj_get_int(ctx, types, args[3])?;
        if max_rep == 0 {
            return Ok(args[0]);
        } else if max_rep < 0 {
            max_rep = -1;
        }
    }
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    str_check_arg_type(ctx.arena, types, self_type, args[1])?;
    str_check_arg_type(ctx.arena, types, self_type, args[2])?;
    let s = str_data_pool(ctx.arena, ctx.pool, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let old = str_data_pool(ctx.arena, ctx.pool, args[1]).ok_or(StrError::BadImplicitConversion)?;
    let new = str_data_pool(ctx.arena, ctx.pool, args[2]).ok_or(StrError::BadImplicitConversion)?;
    if old.len() > s.len() {
        return Ok(args[0]);
    }
    let (out_len, n_rep) = replace_pass(&s, &old, &new, max_rep, None);
    if n_rep == 0 {
        return Ok(args[0]);
    }
    let mut out = Vec::with_capacity(out_len);
    let _ = replace_pass(&s, &old, &new, max_rep, Some(&mut out));
    debug_assert_eq!(out.len(), out_len);
    let is_str = is_str_value(ctx.arena, args[0]);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

// ── join ──────────────────────────────────────────────────────────────

/// `str_join` (`objstr.zig:1469-1534`): `args = [self, iterable]`.
/// Every item must have exactly self's type (bytes ↔ bytearray are
/// normalised in the Zig; no bytearray exists yet), else `TypeError:
/// join expects a list of str/bytes objects consistent with self
/// object`. The result has self's type.
pub fn str_join(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_value = args[0];
    let self_type = obj_get_type(ctx.arena, types, self_value);
    // Pool-aware: the separator is usually a string literal,
    // which loads as a DYNAMIC qstr — the arena-only `str_data`
    // returns `None` for it (the C13-class dynamic-qstr gap;
    // same fix as D1's `len('abc')` and C13b's `str_equal`).
    let sep = str_data_pool(ctx.arena, ctx.pool, self_value).ok_or(StrError::BadImplicitConversion)?;
    let items = iter_items(ctx, types, args[1], None)?;
    let mut out: Vec<u8> = Vec::new();
    let mut first = true;
    // E2e (item 4): bytes ↔ bytearray normalise on both arms
    // (vendor `strJoinImpl`, `objstr.zig:1432-1446`) — a
    // bytearray item joins into a bytes result
    // (`b'-'.join([bytearray(b'a'), b'b'])`), and a bytearray
    // separator works. The result type stays `self_type`
    // (a bytearray separator yields a bytearray — the
    // `new_str_of_type` bytearray arm).
    let norm_self_t = if self_type == Some(types.bytearray) {
        Some(types.bytes)
    } else {
        self_type
    };
    for item in &items {
        // `joinItem` is `nativeBase` on full
        // (`objstr.zig:1411`): a subclass instance joins as its
        // native value (F3 — `''.join([S('x'), 'y'])` raised
        // `JoinType` while the instance's type differed from
        // `str`).
        let item = crate::objects::objtype::subclass::native_base(ctx.arena, *item);
        let mut item_t = obj_get_type(ctx.arena, types, item);
        if item_t == Some(types.bytearray) {
            item_t = Some(types.bytes);
        }
        if item_t != norm_self_t {
            return Err(StrError::JoinType);
        }
        if !first {
            out.extend_from_slice(&sep);
        }
        first = false;
        let d = str_data_pool(ctx.arena, ctx.pool, item).ok_or(StrError::BadImplicitConversion)?;
        out.extend_from_slice(&d);
    }
    let is_str = is_str_value(ctx.arena, self_value);
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &out,
    ))
}

// ── split / rsplit ────────────────────────────────────────────────────

/// `strSplitImpl` (`objstr.zig:1536-1592`): `args = [self, sep?,
/// maxsplit?]` (sep defaults to `None` = whitespace, maxsplit to
/// -1 = unlimited). An empty `sep` raises `ValueError: empty
/// separator`.
pub fn str_split(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let mut splits: i64 = -1;
    let mut sep: Value = Value::None;
    if args.len() > 1 {
        sep = args[1];
        if args.len() > 2 {
            splits = obj_get_int(ctx, types, args[2])?;
        }
    }
    let s = str_data_pool(ctx.arena, ctx.pool, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let is_str = is_str_value(ctx.arena, args[0]);
    let result_type = result_type(types, self_type, is_str);
    let mut items: Vec<Value> = Vec::new();
    if sep == Value::None {
        // Separate on whitespace (`unichar_isspace`, byte-wise).
        let top = s.len();
        let mut p = 0usize;
        while p < top && unichar_isspace(s[p] as u32) {
            p += 1;
        }
        while p < top && splits != 0 {
            let start = p;
            while p < top && !unichar_isspace(s[p] as u32) {
                p += 1;
            }
            items.push(new_str_of_type(ctx, types, result_type, &s[start..p]));
            if p >= top {
                break;
            }
            while p < top && unichar_isspace(s[p] as u32) {
                p += 1;
            }
            if splits > 0 {
                splits -= 1;
            }
        }
        if p < top {
            items.push(new_str_of_type(ctx, types, result_type, &s[p..top]));
        }
    } else {
        str_check_arg_type(ctx.arena, types, self_type, sep)?;
        // Pool-aware (D5): the separator is often a dynamic-qstr literal.
        let sep_data = str_data_pool(ctx.arena, ctx.pool, sep).ok_or(StrError::BadImplicitConversion)?;
        if sep_data.is_empty() {
            return Err(StrError::EmptySeparator);
        }
        let top = s.len();
        let mut p = 0usize;
        loop {
            let start = p;
            loop {
                if splits == 0 || p + sep_data.len() > top {
                    p = top;
                    break;
                } else if s[p..p + sep_data.len()] == sep_data[..] {
                    break;
                }
                p += 1;
            }
            items.push(new_str_of_type(ctx, types, result_type, &s[start..p]));
            if p >= top {
                break;
            }
            p += sep_data.len();
            if splits > 0 {
                splits -= 1;
            }
        }
    }
    Ok(Value::Heap(ctx.alloc(
        Some(types.list),
        Obj::List(ListData { items }),
    )))
}

/// `strRsplitImpl` (`objstr.zig:1595-1663`): `args = [self, sep?,
/// maxsplit?]`. With fewer than 3 args, or a negative maxsplit, it
/// delegates to `str_split`. `sep == None` with 3 args raises
/// `NotImplementedError: rsplit(None,n)`; an empty sep raises
/// `ValueError: empty separator`. Note the Zig does NOT type-check
/// `sep` here (a non-str sep reads garbage in the original — a
/// crash; Rust raises "can't convert to str implicitly" instead).
pub fn str_rsplit(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    if args.len() < 3 {
        return str_split(ctx, types, args);
    }
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let sep = args[1];
    // Pool-aware (D5): subject and separator are often dynamic-qstr literals.
    let s = str_data_pool(ctx.arena, ctx.pool, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let mut splits = obj_get_int(ctx, types, args[2])?;
    if splits < 0 {
        return str_split(ctx, types, args);
    }
    if sep == Value::None {
        return Err(StrError::RsplitNone);
    }
    let sep_data = str_data_pool(ctx.arena, ctx.pool, sep).ok_or(StrError::BadImplicitConversion)?;
    if sep_data.is_empty() {
        return Err(StrError::EmptySeparator);
    }
    let is_str = is_str_value(ctx.arena, args[0]);
    let result_type = result_type(types, self_type, is_str);
    // Pre-sized result, filled right-to-left, then compacted
    // (`npyStrListFinalizeRsplit` shifts the used tail down).
    let mut items: Vec<Option<Value>> = vec![None; (splits + 1) as usize];
    let mut idx = splits;
    let mut last = s.len();
    loop {
        let mut p: i64 = last as i64 - sep_data.len() as i64;
        loop {
            if splits == 0 || p < 0 {
                break;
            }
            if s[p as usize..p as usize + sep_data.len()] == sep_data[..] {
                break;
            }
            p -= 1;
        }
        if p < 0 || splits == 0 {
            items[idx as usize] = Some(new_str_of_type(ctx, types, result_type, &s[..last]));
            break;
        }
        let right_off = p as usize + sep_data.len();
        items[idx as usize] = Some(new_str_of_type(
            ctx,
            types,
            result_type,
            &s[right_off..last],
        ));
        idx -= 1;
        last = p as usize;
        splits -= 1;
    }
    let items: Vec<Value> = items.into_iter().flatten().collect();
    Ok(Value::Heap(ctx.alloc(
        Some(types.list),
        Obj::List(ListData { items }),
    )))
}

// ── strip ─────────────────────────────────────────────────────────────

const STRIP_L: i32 = 0;
const STRIP_R: i32 = 1;
const STRIP_S: i32 = 2;

/// `strUniStrip` (`objstr.zig:1665-1761`): `args = [self, chars?]`
/// (chars defaults to `" \t\n\r\x0b\x0c"`). The chars arg IS
/// type-checked (`str_check_arg_type` — a mismatch raises "can't
/// convert to str implicitly"; the original forgets to poll the
/// raise and crashes on a non-str chars arg, so Rust raises where
/// the original segfaults). All-stripped yields the empty str/bytes;
/// nothing-stripped returns self itself.
fn str_uni_strip(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    stype: i32,
    args: &[Value],
) -> Result<Value, StrError> {
    let self_type = obj_get_type(ctx.arena, types, args[0]);
    let ws: &[u8] = b" \t\n\r\x0b\x0c";
    // Pool-aware (D5): the strip-chars arg is often a dynamic-qstr literal.
    let chars: Vec<u8> = if args.len() == 1 {
        ws.to_vec()
    } else {
        str_check_arg_type(ctx.arena, types, self_type, args[1])?;
        str_data_pool(ctx.arena, ctx.pool, args[1]).ok_or(StrError::BadImplicitConversion)?
    };
    let s = str_data_pool(ctx.arena, ctx.pool, args[0]).ok_or(StrError::BadImplicitConversion)?;
    let is_str = is_str_value(ctx.arena, args[0]);
    let orig_len = s.len();
    let mut first_good: Option<usize> = None;
    let mut last_good: usize = 0;
    let mut i: i64 = if stype == STRIP_R && orig_len > 0 {
        orig_len as i64 - 1
    } else {
        0
    };
    let delta: i64 = if stype == STRIP_R { -1 } else { 1 };
    let mut len_remaining = orig_len;
    while len_remaining > 0 {
        let idx = i as usize;
        if find_subbytes(&chars, &s[idx..idx + 1], 1).is_none() {
            if first_good.is_none() {
                first_good = Some(idx);
                if stype == STRIP_L {
                    last_good = orig_len - 1;
                    break;
                } else if stype == STRIP_R {
                    first_good = Some(0);
                    last_good = idx;
                    break;
                }
            }
            last_good = idx;
        }
        i += delta;
        len_remaining -= 1;
    }
    let Some(first) = first_good else {
        return Ok(if is_str {
            new_empty_str()
        } else {
            new_empty_bytes(ctx, types)
        });
    };
    let stripped_len = last_good - first + 1;
    if stripped_len == orig_len {
        return Ok(args[0]);
    }
    Ok(new_str_of_type(
        ctx,
        types,
        result_type(types, self_type, is_str),
        &s[first..first + stripped_len],
    ))
}

/// `str_strip` (`objstr.zig:1763-1765`).
pub fn str_strip(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_uni_strip(ctx, types, STRIP_S, args)
}

/// `str_lstrip` (`objstr.zig:1766-1768`).
pub fn str_lstrip(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_uni_strip(ctx, types, STRIP_L, args)
}

/// `str_rstrip` (`objstr.zig:1769-1771`).
pub fn str_rstrip(
    ctx: &mut CallCtx,
    types: &BuiltinTypes,
    args: &[Value],
) -> Result<Value, StrError> {
    str_uni_strip(ctx, types, STRIP_R, args)
}
