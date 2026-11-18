//! The `_json` builtin module (`src/modules/modjson.zig`, 455
//! lines — the fast paths of `json.dumps` / `json.loads`).
//!
//! `encode(obj, item_sep, kv_sep, ensure_ascii) -> str`,
//! `string(s, ensure_ascii) -> str`, `decode(s) -> value`.
//! The pure-Python `petitpy-lib/json.py` layer (which falls
//! back to its own encoder/decoder on the native errors) is
//! D5's frozen bundle, not this module.
//!
//! Faithful points (all probed on the reference binary
//! 2026-11-18, see `scripts/d3_result.txt` §2):
//! - Encoder accepts exactly None / bool / int / finite
//!   float / str / list / tuple / dict-of-those (exact
//!   types — a subclass or a non-str key is
//!   `TypeError("not handled by the native JSON encoder")`);
//!   max depth 256 (deeper → the same TypeError, and the
//!   Python layer raises RecursionError).
//! - String escapes: `"` `\` `\b \t \n \f \r`, other
//!   `<0x20` → `\u00xx` (lowercase hex); `ensure_ascii`
//!   escapes `>=0x7F` as `\uXXXX` (astral → surrogate
//!   pair), decoding the UTF-8 sequence the Zig way
//!   (no validation — a truncated sequence decodes short).
//! - `ensure_ascii` is an IDENTITY check against `True`
//!   (`encode(..., 1)` does NOT escape).
//! - Floats print via the repr printer (`1.0`, `1e+30`);
//!   NaN/inf are unsupported (the Python layer writes
//!   `Infinity` / `NaN`).
//! - Dict order is INSERTION order; separators come from
//!   the `item_sep` / `kv_sep` args verbatim.
//! - Decoder is strict: no leading zero (`01`), no bare
//!   `1.` / `1e`, no trailing garbage; `\u` surrogates
//!   (D800–DFFF, even valid pairs) are rejected; max depth
//!   256; anything else is `ValueError("not handled by the
//!   native JSON decoder")`.
//! - Decoder ints: ≤18 digits → small int (all 18-digit
//!   values fit the 62-bit window); >18 digits → BigInt.
//! - Decoder floats go through `parse_num_float(tok,
//!   false)` (libc `strtod`, correctly rounded).
//! - There is NO `JSONEncoder` / `JSONDecodeError` here —
//!   those live in `json.py` (D5).

use crate::memory::gc::CallCtx;
use crate::objects::containers::objdict::dict_store;
use crate::objects::objtype::objmodule::new_module;
use crate::objects::primitives::objfloat::new_float;
use crate::objects::primitives::objint::{fun_n_args, int_to_string};
use crate::objects::primitives::ops::{
    new_int_from_bigint, static_qstr_id, str_bytes_of, QstrResolver,
};
use crate::runtime::opmethods::native_base;
use crate::objects::strings::mpprint::{
    mp_print_float, Print, MP_FLOAT_REPR_PREC, PF_FLAG_ALWAYS_DECIMAL,
};
use crate::objects::value::{DictData, FunBuiltinData, GcRef, ListData, MapData, Obj, StrData, Value};
use crate::pyerr::{PyError, PyResult};
use crate::vendor::parsenum::{parse_num_float, parse_num_integer, DecimalNum};

/// The encoder/decoder's single failure mode; the caller
/// maps it to the module's public error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unsupported;

/// Max nesting depth (`modjson.zig: max_depth = 256`).
const MAX_DEPTH: usize = 256;

/// `TypeError("not handled by the native JSON encoder")`.
fn encode_unsupported(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise(
        "TypeError",
        Some("not handled by the native JSON encoder"),
    );
    PyError::PyRaise
}

/// `ValueError("not handled by the native JSON decoder")`.
fn decode_unsupported(ctx: &mut CallCtx) -> PyError {
    ctx.record_raise(
        "ValueError",
        Some("not handled by the native JSON decoder"),
    );
    PyError::PyRaise
}

/// A str/bytes argument as bytes (`mp_obj_str_get_data`):
/// anything else is `TypeError("can't convert to str
/// implicitly")` (probed: `_json.string(123, True)`).
fn str_arg_bytes(ctx: &mut CallCtx, v: Value) -> Result<Vec<u8>, PyError> {
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

/// The decode input as bytes: a non-str/bytes argument reads
/// as EMPTY (`mp_obj_str_get_data` returns NULL + len 0, and
/// the Zig's `py_decode` then fails the empty parse, so
/// `_json.decode(123)` is the decoder ValueError, probed —
/// NOT the TypeError). No error is recorded here.
fn decode_input_bytes(ctx: &CallCtx, v: Value) -> Vec<u8> {
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
    str_bytes_of(ctx.arena, ctx.types, resolver, v).unwrap_or_default()
}

/// The `Encoder` (`modjson.zig: Encoder`): output buffer
/// plus the separator / ascii settings.
struct Encoder {
    out: Vec<u8>,
    item_sep: Vec<u8>,
    kv_sep: Vec<u8>,
    ascii: bool,
}

impl Encoder {
    /// `\uXXXX` (lowercase hex, `modjson.zig: hex`).
    fn hex_escape(&mut self, cp: u32) {
        self.out
            .extend_from_slice(format!("\\u{cp:04x}").as_bytes());
    }

    /// `Encoder.str` (`modjson.zig`): a byte slice as a JSON
    /// string literal, with the Zig's run-chunked escaping.
    fn encode_str(&mut self, data: &[u8]) {
        self.out.push(b'"');
        let mut run = 0;
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            let esc: Option<&[u8]> = match c {
                b'"' => Some(b"\\\""),
                b'\\' => Some(b"\\\\"),
                0x08 => Some(b"\\b"),
                b'\t' => Some(b"\\t"),
                b'\n' => Some(b"\\n"),
                0x0C => Some(b"\\f"),
                b'\r' => Some(b"\\r"),
                _ => None,
            };
            if let Some(esc) = esc {
                self.out.extend_from_slice(&data[run..i]);
                self.out.extend_from_slice(esc);
                i += 1;
                run = i;
                continue;
            }
            if c < 0x20 || (self.ascii && c >= 0x7F) {
                self.out.extend_from_slice(&data[run..i]);
                if c < 0x80 {
                    self.hex_escape(c as u32);
                    i += 1;
                } else {
                    // ensure_ascii: decode the UTF-8 sequence
                    // the Zig way (no validation).
                    let n = if c >= 0xF0 {
                        4
                    } else if c >= 0xE0 {
                        3
                    } else {
                        2
                    };
                    let mut cp: u32 = (c as u32) & (0x7F >> n);
                    for &b in data.get(i + 1..).unwrap_or(&[]).iter().take(n - 1) {
                        cp = (cp << 6) | ((b & 0x3F) as u32);
                    }
                    if cp >= 0x10000 {
                        self.hex_escape(0xD800 + ((cp - 0x10000) >> 10));
                        self.hex_escape(0xDC00 + ((cp - 0x10000) & 0x3FF));
                    } else {
                        self.hex_escape(cp);
                    }
                    i = (i + n).min(data.len());
                }
                run = i;
                continue;
            }
            i += 1;
        }
        self.out.extend_from_slice(&data[run..]);
        self.out.push(b'"');
    }

    /// `Encoder.value` (`modjson.zig`): the value cases, in
    /// the Zig's order. Container children are encoded at
    /// `depth + 1`; the depth check fires on entry.
    fn value(&mut self, ctx: &mut CallCtx, o: Value, depth: usize) -> Result<(), Unsupported> {
        if depth > MAX_DEPTH {
            return Err(Unsupported);
        }
        match o {
            Value::None => {
                self.out.extend_from_slice(b"null");
                Ok(())
            }
            Value::Bool(true) => {
                self.out.extend_from_slice(b"true");
                Ok(())
            }
            Value::Bool(false) => {
                self.out.extend_from_slice(b"false");
                Ok(())
            }
            Value::SmallInt(n) => {
                self.out.extend_from_slice(format!("{n}").as_bytes());
                Ok(())
            }
            Value::Qstr(id) => {
                let bytes = ctx.pool.qstr_str(id as u16).to_vec();
                self.encode_str(&bytes);
                Ok(())
            }
            Value::Heap(r) => match ctx.arena.get(r) {
                Obj::Str(s) => {
                    let bytes = s.bytes.clone();
                    self.encode_str(&bytes);
                    Ok(())
                }
                Obj::Float(f) => {
                    let f = *f;
                    if !f.is_finite() {
                        return Err(Unsupported);
                    }
                    let mut sink: Vec<u8> = Vec::new();
                    let mut print = Print::new(&mut sink);
                    let _ = mp_print_float(
                        &mut print,
                        f,
                        b'g',
                        PF_FLAG_ALWAYS_DECIMAL,
                        0,
                        -1,
                        MP_FLOAT_REPR_PREC,
                    );
                    self.out.extend_from_slice(&sink);
                    Ok(())
                }
                Obj::Int(_) => {
                    let Some(s) = int_to_string(o, ctx.arena) else {
                        return Err(Unsupported);
                    };
                    self.out.extend_from_slice(s.as_bytes());
                    Ok(())
                }
                Obj::List(l) => {
                    let items = l.items.clone();
                    self.items(ctx, &items, depth)
                }
                Obj::Tuple(items) => {
                    let items = items.clone();
                    self.items(ctx, &items, depth)
                }
                Obj::Dict(d) => {
                    let entries: Vec<(Value, Value)> = d
                        .map
                        .entries
                        .iter()
                        .filter_map(|e| e.key.map(|k| (k, e.value)))
                        .collect();
                    self.out.push(b'{');
                    let mut first = true;
                    for (k, v) in entries {
                        // Keys must be qstr or exact str.
                        let key_bytes = match k {
                            Value::Qstr(id) => ctx.pool.qstr_str(id as u16).to_vec(),
                            Value::Heap(kr) => match ctx.arena.get(kr) {
                                Obj::Str(s)
                                    if ctx.arena.header(kr).type_ == Some(ctx.types.str_) =>
                                {
                                    s.bytes.clone()
                                }
                                _ => return Err(Unsupported),
                            },
                            _ => return Err(Unsupported),
                        };
                        if !first {
                            self.out.extend_from_slice(&self.item_sep.clone());
                        }
                        first = false;
                        self.encode_str(&key_bytes);
                        self.out.extend_from_slice(&self.kv_sep.clone());
                        self.value(ctx, v, depth + 1)?;
                    }
                    self.out.push(b'}');
                    Ok(())
                }
                _ => Err(Unsupported),
            },
        }
    }

    /// `Encoder.items` (`modjson.zig`): a list/tuple.
    fn items(
        &mut self,
        ctx: &mut CallCtx,
        elems: &[Value],
        depth: usize,
    ) -> Result<(), Unsupported> {
        self.out.push(b'[');
        for (i, e) in elems.iter().enumerate() {
            if i > 0 {
                let sep = self.item_sep.clone();
                self.out.extend_from_slice(&sep);
            }
            self.value(ctx, *e, depth + 1)?;
        }
        self.out.push(b']');
        Ok(())
    }
}

/// `py_encode` (`modjson.zig`): `encode(obj, item_sep,
/// kv_sep, ensure_ascii)`, sig (4, 4). `ensure_ascii` is
/// an identity check against `True`.
fn json_encode(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let item_sep = str_arg_bytes(ctx, args[1])?;
    let kv_sep = str_arg_bytes(ctx, args[2])?;
    let ascii = args[3] == Value::Bool(true);
    let mut enc = Encoder {
        out: Vec::new(),
        item_sep,
        kv_sep,
        ascii,
    };
    match enc.value(ctx, args[0], 0) {
        Ok(()) => Ok(Value::Heap(ctx.alloc(
            Some(ctx.types.str_),
            Obj::Str(StrData { bytes: enc.out }),
        ))),
        Err(Unsupported) => Err(encode_unsupported(ctx)),
    }
}

/// `py_string` (`modjson.zig`): `string(s, ensure_ascii)`,
/// sig (2, 2). A str, or a str subclass by its value
/// (`nativeBase`).
fn json_string(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let base = native_base(ctx, args[0]);
    let bytes = str_arg_bytes(ctx, base)?;
    let ascii = args[1] == Value::Bool(true);
    let mut enc = Encoder {
        out: Vec::new(),
        item_sep: Vec::new(),
        kv_sep: Vec::new(),
        ascii,
    };
    enc.encode_str(&bytes);
    Ok(Value::Heap(ctx.alloc(
        Some(ctx.types.str_),
        Obj::Str(StrData { bytes: enc.out }),
    )))
}

/// The `Decoder` (`modjson.zig: Decoder`): a byte cursor
/// over the input.
struct Decoder<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Decoder<'a> {
    /// Skip JSON whitespace (`' ' \t \n \r`).
    fn ws(&mut self) {
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => return,
            }
        }
    }

    fn peek(&self) -> Result<u8, Unsupported> {
        self.s.get(self.i).copied().ok_or(Unsupported)
    }

    fn literal(&mut self, word: &[u8], o: Value) -> Result<Value, Unsupported> {
        if !self.s[self.i..].starts_with(word) {
            return Err(Unsupported);
        }
        self.i += word.len();
        Ok(o)
    }

    /// The digit run at the cursor; returns its length.
    fn digits(&mut self) -> usize {
        let start = self.i;
        while let Some(&c) = self.s.get(self.i) {
            if !c.is_ascii_digit() {
                break;
            }
            self.i += 1;
        }
        self.i - start
    }

    /// `Decoder.number` (`modjson.zig`): only numbers whose
    /// meaning is not in doubt. `≤18` digits → small int;
    /// more → BigInt; any fraction/exponent → float via
    /// `parse_num_float(tok, false)`.
    fn number(&mut self, ctx: &mut CallCtx) -> Result<Value, Unsupported> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let int_start = self.i;
        let n_int = self.digits();
        if n_int == 0 || (n_int > 1 && self.s[int_start] == b'0') {
            return Err(Unsupported);
        }
        let mut is_float = false;
        if self.s.get(self.i) == Some(&b'.') {
            is_float = true;
            self.i += 1;
            if self.digits() == 0 {
                return Err(Unsupported);
            }
        }
        if matches!(self.s.get(self.i), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(Unsupported);
            }
        }
        let tok = &self.s[start..self.i];
        if is_float {
            return match parse_num_float(tok, false) {
                Ok(DecimalNum::Float(f)) => Ok(new_float(ctx, ctx.types, f)),
                _ => Err(Unsupported),
            };
        }
        if n_int <= 18 {
            let text = std::str::from_utf8(tok).map_err(|_| Unsupported)?;
            let n: i64 = text.parse().map_err(|_| Unsupported)?;
            return Ok(Value::SmallInt(n));
        }
        match parse_num_integer(tok, 10) {
            Ok(z) => Ok(new_int_from_bigint(ctx.arena, ctx.types, &ctx.roots, &z)),
            Err(_) => Err(Unsupported),
        }
    }

    /// `Decoder.string` (`modjson.zig`): the fast path
    /// returns the raw slice when nothing was escaped.
    fn string(&mut self, ctx: &mut CallCtx) -> Result<Value, Unsupported> {
        self.i += 1;
        let mut run = self.i;
        let mut out: Vec<u8> = Vec::new();
        let mut escaped = false;
        while let Some(&c) = self.s.get(self.i) {
            if c == b'"' {
                let tail = &self.s[run..self.i];
                self.i += 1;
                if !escaped {
                    return Ok(Value::Heap(ctx.alloc(
                        Some(ctx.types.str_),
                        Obj::Str(StrData {
                            bytes: tail.to_vec(),
                        }),
                    )));
                }
                out.extend_from_slice(tail);
                return Ok(Value::Heap(ctx.alloc(
                    Some(ctx.types.str_),
                    Obj::Str(StrData { bytes: out }),
                )));
            }
            if c != b'\\' {
                if c < 0x20 {
                    return Err(Unsupported);
                }
                self.i += 1;
                continue;
            }
            if !escaped {
                escaped = true;
            }
            out.extend_from_slice(&self.s[run..self.i]);
            self.i += 1;
            let e = self.peek()?;
            self.i += 1;
            match e {
                b'"' | b'\\' | b'/' => out.push(e),
                b'b' => out.push(0x08),
                b'f' => out.push(0x0C),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'u' => {
                    if self.i + 4 > self.s.len() {
                        return Err(Unsupported);
                    }
                    let hex = &self.s[self.i..self.i + 4];
                    let text = std::str::from_utf8(hex).map_err(|_| Unsupported)?;
                    let code = u16::from_str_radix(text, 16).map_err(|_| Unsupported)?;
                    if (0xD800..=0xDFFF).contains(&code) {
                        return Err(Unsupported);
                    }
                    // `code >= 0x80 and !str_unicode` is dead
                    // under full (str_unicode is on).
                    let ch = char::from_u32(code as u32).ok_or(Unsupported)?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    self.i += 4;
                }
                _ => return Err(Unsupported),
            }
            run = self.i;
        }
        Err(Unsupported)
    }

    /// `Decoder.value` (`modjson.zig`): the value cases.
    /// Containers are allocated up front and rooted in
    /// `ctx.roots` while their children are parsed (the
    /// struct-unpack pattern: values allocated during a
    /// nested parse must not be collected).
    fn value(&mut self, ctx: &mut CallCtx, depth: usize) -> Result<Value, Unsupported> {
        if depth > MAX_DEPTH {
            return Err(Unsupported);
        }
        self.ws();
        match self.peek()? {
            b'"' => self.string(ctx),
            b'{' => {
                self.i += 1;
                let dict = Value::Heap(ctx.alloc(
                    Some(ctx.types.dict),
                    Obj::Dict(DictData {
                        map: MapData::new(),
                    }),
                ));
                ctx.roots.push(dict);
                self.ws();
                if self.peek()? == b'}' {
                    self.i += 1;
                    return Ok(dict);
                }
                loop {
                    self.ws();
                    if self.peek()? != b'"' {
                        return Err(Unsupported);
                    }
                    let k = self.string(ctx)?;
                    ctx.roots.push(k);
                    self.ws();
                    if self.peek()? != b':' {
                        return Err(Unsupported);
                    }
                    self.i += 1;
                    let v = self.value(ctx, depth + 1)?;
                    ctx.roots.push(v);
                    dict_store(ctx, dict, k, v).map_err(|_| Unsupported)?;
                    self.ws();
                    let c = self.peek()?;
                    self.i += 1;
                    if c == b'}' {
                        return Ok(dict);
                    }
                    if c != b',' {
                        return Err(Unsupported);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let list = Value::Heap(ctx.alloc(
                    Some(ctx.types.list),
                    Obj::List(ListData { items: Vec::new() }),
                ));
                ctx.roots.push(list);
                self.ws();
                if self.peek()? == b']' {
                    self.i += 1;
                    return Ok(list);
                }
                loop {
                    let item = self.value(ctx, depth + 1)?;
                    ctx.roots.push(item);
                    if let Value::Heap(lr) = list {
                        if let Obj::List(l) = ctx.arena.get_mut(lr) {
                            l.items.push(item);
                        }
                    }
                    self.ws();
                    let c = self.peek()?;
                    self.i += 1;
                    if c == b']' {
                        return Ok(list);
                    }
                    if c != b',' {
                        return Err(Unsupported);
                    }
                }
            }
            b't' => self.literal(b"true", Value::Bool(true)),
            b'f' => self.literal(b"false", Value::Bool(false)),
            b'n' => self.literal(b"null", Value::None),
            b'-' | b'0'..=b'9' => self.number(ctx),
            _ => Err(Unsupported),
        }
    }
}

/// `py_decode` (`modjson.zig`): `decode(s)`, sig (1, 1).
/// A trailing check (`ws`, then `i == len`) rejects
/// trailing garbage.
fn json_decode(ctx: &mut CallCtx, args: &[Value]) -> PyResult<Value> {
    let data = decode_input_bytes(ctx, args[0]);
    let mut dec = Decoder { s: &data, i: 0 };
    let v = match dec.value(ctx, 0) {
        Ok(v) => v,
        Err(Unsupported) => return Err(decode_unsupported(ctx)),
    };
    dec.ws();
    if dec.i != data.len() {
        return Err(decode_unsupported(ctx));
    }
    Ok(v)
}

/// Build the `_json` module object (`modjson.zig:431-443`):
/// globals `{__name__: "_json", encode, decode, string}`.
/// Registration into `VmState.builtin_modules` is D1's
/// (the `build_io_module` pattern).
pub fn build_json_module(
    ctx: &mut CallCtx,
    fun_builtin_var: GcRef,
    module_type: GcRef,
) -> PyResult<Value> {
    let json_id = static_qstr_id(b"_json").expect("_json is a static qstr");
    let module = new_module(ctx, json_id, module_type);
    let dict = match module {
        Value::Heap(r) => match ctx.arena.get(r) {
            Obj::Module(m) => m.dict,
            _ => return Err(PyError::PyRaise),
        },
        _ => return Err(PyError::PyRaise),
    };
    let dict_v = Value::Heap(dict);
    let bind = |ctx: &mut CallCtx,
                dict_v: Value,
                name: &str,
                min: u16,
                max: u16,
                func: crate::objects::slots::BuiltinFn|
     -> PyResult<()> {
        let id = static_qstr_id(name.as_bytes()).expect("json fn name is a static qstr");
        let fun = ctx.alloc(
            Some(fun_builtin_var),
            Obj::FunBuiltin(FunBuiltinData {
                name: id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: None,
            }),
        );
        let v = Value::Heap(fun);
        ctx.roots.push(v);
        dict_store(ctx, dict_v, Value::Qstr(id), v).map_err(|e| ctx.record_typed(e))?;
        Ok(())
    };
    bind(ctx, dict_v, "encode", 4, 4, json_encode)?;
    bind(ctx, dict_v, "decode", 1, 1, json_decode)?;
    bind(ctx, dict_v, "string", 2, 2, json_string)?;
    Ok(module)
}
