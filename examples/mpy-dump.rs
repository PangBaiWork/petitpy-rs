//! Dump a .mpy file's parsed structure in a canonical, diffable format.
//!
//! Used by `scripts/check_mpy_dump.sh` to compare the Rust reader
//! against the Zig reader (`scripts/mpy_dump.zig` prints the same
//! format byte for byte). Usage: mpy-dump <input.mpy>

use petitpy_rs::compiler::mpy::{self, Module, ObjEntry, QstrEntry, RawCode};
use std::io::Write;

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn dump_obj(o: &ObjEntry, out: &mut String) {
    match o {
        ObjEntry::None => out.push_str("none"),
        ObjEntry::False => out.push_str("false"),
        ObjEntry::True => out.push_str("true"),
        ObjEntry::Ellipsis => out.push_str("ellipsis"),
        ObjEntry::Str(s) => {
            out.push_str("str:");
            out.push_str(&hex(s));
        }
        ObjEntry::Bytes(b) => {
            out.push_str("bytes:");
            out.push_str(&hex(b));
        }
        ObjEntry::Int(s) => {
            out.push_str("int:");
            out.push_str(&hex(s));
        }
        ObjEntry::Float(s) => {
            out.push_str("float:");
            out.push_str(&hex(s));
        }
        ObjEntry::Complex(s) => {
            out.push_str("complex:");
            out.push_str(&hex(s));
        }
        ObjEntry::Tuple(t) => {
            out.push_str("tuple:[");
            for (i, it) in t.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_obj(it, out);
            }
            out.push(']');
        }
    }
}

fn dump_rc(rc: &RawCode, path: &str, out: &mut String) {
    out.push_str(&format!(
        "{path} kind={} fun_data_len={} fun_data={} children={}\n",
        rc.kind,
        rc.fun_data.len(),
        hex(&rc.fun_data),
        rc.children.len()
    ));
    for (i, c) in rc.children.iter().enumerate() {
        dump_rc(c, &format!("{path}/{i}"), out);
    }
}

/// Canonical dump of a parsed module. Shared format with
/// `scripts/mpy_dump.zig` — keep the two in sync.
pub fn dump_module(m: &Module) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "header version={} feature_flags={} small_int_bits={} arch_flags={}\n",
        m.version,
        m.feature_flags,
        m.small_int_bits,
        match m.arch_flags {
            None => "none".to_string(),
            Some(v) => v.to_string(),
        }
    ));
    out.push_str(&format!("n_qstr={} n_obj={}\n", m.qstrs.len(), m.objs.len()));
    for (i, q) in m.qstrs.iter().enumerate() {
        match q {
            QstrEntry::Static(id) => out.push_str(&format!("q[{i}]=S:{id}\n")),
            QstrEntry::New(b) => out.push_str(&format!("q[{i}]=N:{}\n", hex(b))),
        }
    }
    for (i, o) in m.objs.iter().enumerate() {
        out.push_str(&format!("o[{i}]="));
        dump_obj(o, &mut out);
        out.push('\n');
    }
    dump_rc(&m.rc, "rc", &mut out);
    out
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 {
        let _ = writeln!(std::io::stderr(), "usage: mpy-dump <input.mpy>");
        std::process::exit(2);
    }
    let data = match std::fs::read(&argv[1]) {
        Ok(d) => d,
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "fopen failed");
            std::process::exit(2);
        }
    };
    let m = match mpy::read(&data) {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "mpy.read failed: {e}");
            std::process::exit(2);
        }
    };
    print!("{}", dump_module(&m));
}
