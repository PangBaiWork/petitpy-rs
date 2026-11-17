//! Tests for `callables` (Lane C10a).
//!
//! Reference-binary probe values (2026-11-17, `/tmp/c10a_probe*.py`):
//! - `f.__name__ == "f"`, `repr(f) == "<function f at 0x...>"`
//!   (address is a per-run artifact — Rust prints the arena slot
//!   index instead, same format); `str(f)` is identical (kind
//!   ignored); `f.__qualname__` / `f.__doc__` → `AttributeError`;
//!   `f.__globals__ is globals()`.
//! - Builtin functions have NO `__name__` (`len.__name__` →
//!   `AttributeError` — the builtin-fun types have no attr slot);
//!   `repr(len) == "<function>"`; `[].append.__name__` →
//!   `AttributeError`; `repr([].append) == "<bound_method>"`;
//!   `C().m.__name__ == "m"`; `repr(C().m) ==
//!   "<bound_method>"`; `c.m == c.m` (same `c`), `C().m !=
//!   C().m`; `repr(closure) == "<closure>"`;
//!   `f.__name__ = "x"` → `AttributeError` (store path).
//! - Arity errors are all terse `TypeError: argument num/types
//!   mismatch` (`len(1, 2)`, `len()`, `f()`, `f(1, 2, 3)`); the
//!   setup errors carry their own messages (`f(1, c=5)` →
//!   "unexpected keyword argument"; `f(a=1, a=2)` → "function got
//!   multiple values for argument 'a'"; `f(b=3)` → "function
//!   missing required positional argument #0"; `g(a=1)` on
//!   `def g(a, b, c=3)` → missing `#1`).
//!
//! Discipline: build every fixture value first, then create the
//! `CallCtx` — a live `ctx` borrows the fixture mutably, so no
//! `fx` helper may follow it in the same scope (drop the `ctx`
//! first when the arena is needed again).

use super::objboundmeth::*;
use super::objclosure::*;
use super::objfun::*;
use super::super::containers::tests::{empty_dict, Fixture};
use crate::compiler::bridge::compile_to_mpy;
use crate::compiler::mpy::{self, Module, QstrEntry, RawCode};
use crate::compiler::parser::InputKind;
use crate::memory::gc::{CallCtx, GcArena};
use crate::objects::primitives::objint::fun_n_args;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::slots::PrintKind;
use crate::objects::value::{
    CellData, FunBuiltinData, GcRef, ModuleData, Obj, TypeData, Value,
};
use crate::pyerr::PyError;
use crate::runtime::bc::{prelude_sig_decode, prelude_size_decode};
use crate::runtime::qstr::RuntimeQstr;

// ── compile helper (mirrors `bc_tests.rs`) ──────────────────────

/// Compile `source` (a module defining exactly one function) and
/// return that function's `RawCode` plus the resolved module qstr
/// table (prelude index -> qstr id).
pub(crate) fn compile_fun(source: &str, pool: &mut RuntimeQstr) -> (RawCode, Vec<u16>) {
    let bytes = compile_to_mpy(source, InputKind::FileInput).expect("test source must compile");
    let Module { qstrs, rc, .. } = mpy::read(&bytes).expect("test .mpy must read");
    assert_eq!(rc.children.len(), 1, "test module must define exactly one function");
    let fun = rc.children.into_iter().next().unwrap();
    let mut table = Vec::with_capacity(qstrs.len());
    for e in &qstrs {
        match e {
            QstrEntry::Static(id) => table.push(*id),
            QstrEntry::New(bytes) => table.push(pool.from_strn(bytes).expect("qstr intern")),
        }
    }
    (fun, table)
}

/// `n_state` for a `RawCode`, decoded from its sig prelude.
pub(crate) fn n_state_of(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    prelude_sig_decode(&mut cursor).n_state
}

/// Expected final `ip`: prelude bytes + source-info bytes + cell
/// bytes, measured by the decode cursors.
pub(crate) fn expected_ip(fun: &RawCode) -> usize {
    let mut cursor: &[u8] = &fun.fun_data;
    let _sig = prelude_sig_decode(&mut cursor);
    let size = prelude_size_decode(&mut cursor);
    (fun.fun_data.len() - cursor.len()) + size.n_info + size.n_cell
}

// ── fixture ─────────────────────────────────────────────────────

/// The containers `Fixture` plus the eight callable type objects
/// (built here, not in the shared fixture — the callers take the
/// header type explicitly, C9b precedent) and the VM-owned
/// empty-tuple singleton.
pub(crate) struct CFx {
    pub(crate) fx: Fixture,
    pub(crate) fun_bc: GcRef,
    pub(crate) fun_builtin_0: GcRef,
    pub(crate) fun_builtin_1: GcRef,
    pub(crate) fun_builtin_2: GcRef,
    pub(crate) fun_builtin_3: GcRef,
    pub(crate) fun_builtin_var: GcRef,
    pub(crate) bound_meth: GcRef,
    pub(crate) closure: GcRef,
    pub(crate) empty_tuple: Value,
}

impl CFx {
    pub(crate) fn new() -> CFx {
        let mut fx = Fixture::new();
        let mk = |fx: &mut Fixture, data: TypeData| -> GcRef {
            let t = fx.arena.alloc(None, Obj::Type(data), &fx.roots);
            fx.roots.push(Value::Heap(t));
            t
        };
        // The dict for each type data is built BEFORE the `mk`
        // call: `mk(&mut fx, ...empty_dict(&mut fx.arena...))`
        // would borrow `fx` twice.
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_bc = mk(&mut fx, fun_bc_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_builtin_0 = mk(&mut fx, fun_builtin_0_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_builtin_1 = mk(&mut fx, fun_builtin_1_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_builtin_2 = mk(&mut fx, fun_builtin_2_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_builtin_3 = mk(&mut fx, fun_builtin_3_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let fun_builtin_var = mk(&mut fx, fun_builtin_var_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let bound_meth = mk(&mut fx, bound_meth_type_data(d));
        let d = empty_dict(&mut fx.arena, &fx.roots);
        let closure = mk(&mut fx, closure_type_data(d));
        let empty_tuple =
            Value::Heap(fx.arena.alloc(None, Obj::Tuple(Vec::new()), &fx.roots));
        CFx {
            fx,
            fun_bc,
            fun_builtin_0,
            fun_builtin_1,
            fun_builtin_2,
            fun_builtin_3,
            fun_builtin_var,
            bound_meth,
            closure,
            empty_tuple,
        }
    }

    pub(crate) fn ctx(&mut self) -> CallCtx<'_> {
        self.fx.ctx()
    }

    /// qstr id of `name` in the fixture's pool.
    pub(crate) fn q(&mut self, name: &str) -> u32 {
        u32::from(self.fx.pool.from_strn(name.as_bytes()).unwrap())
    }

    /// A heap `FunBuiltin` with this sig range + body (header type
    /// is the var type; the adapters read only the payload).
    pub(crate) fn builtin(
        &mut self,
        type_: GcRef,
        name: &str,
        min: u16,
        max: u16,
        func: crate::objects::slots::BuiltinFn,
    ) -> Value {
        let name_id = static_qstr_id(name.as_bytes()).unwrap_or_else(|| self.q(name));
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(
            Some(type_),
            Obj::FunBuiltin(FunBuiltinData {
                name: name_id,
                n_args: fun_n_args(min, max),
                func,
                kw_func: None,
            }),
        ))
    }

    /// A heap `FunBuiltin` with a keyword-taking body.
    pub(crate) fn kw_builtin(
        &mut self,
        name: &str,
        min: u16,
        max: u16,
        kw_func: crate::objects::slots::BuiltinKwFn,
    ) -> Value {
        let type_ = self.fun_builtin_var;
        let name_id = static_qstr_id(name.as_bytes()).unwrap_or_else(|| self.q(name));
        let mut ctx = self.ctx();
        Value::Heap(ctx.alloc(
            Some(type_),
            Obj::FunBuiltin(FunBuiltinData {
                name: name_id,
                n_args: fun_n_args(min, max),
                // Never called for a kw builtin (the adapter calls
                // `kw_func`); returns `PyRaise` if it ever is.
                func: |_, _| Err(PyError::PyRaise),
                kw_func: Some(kw_func),
            }),
        ))
    }

    /// A heap `Cell` holding `value` (header type `None` —
    /// bootstrap pattern, as in `bc_tests.rs`).
    pub(crate) fn cell(&mut self, value: Option<Value>) -> GcRef {
        self.fx
            .arena
            .alloc(None, Obj::Cell(CellData { value }), &self.fx.roots)
    }

    /// A fresh module object whose globals dict is `dict`
    /// (returned as the `Value` of the dict).
    pub(crate) fn module_with_globals(&mut self) -> (GcRef, Value) {
        let dict = empty_dict(&mut self.fx.arena, &self.fx.roots);
        let dict_value = Value::Heap(dict);
        let name = self.q("m");
        let module = self.fx.arena.alloc(
            None,
            Obj::Module(ModuleData { name, dict }),
            &self.fx.roots,
        );
        (module, dict_value)
    }
}

// ── test builtin bodies ─────────────────────────────────────────

pub(crate) fn ret_arg0(_ctx: &mut CallCtx, args: &[Value]) -> Result<Value, PyError> {
    Ok(args[0])
}

pub(crate) fn ret_arg1(_ctx: &mut CallCtx, args: &[Value]) -> Result<Value, PyError> {
    Ok(args[1])
}

pub(crate) fn ret_arg2(_ctx: &mut CallCtx, args: &[Value]) -> Result<Value, PyError> {
    Ok(args[2])
}

pub(crate) fn ret_const(_ctx: &mut CallCtx, _args: &[Value]) -> Result<Value, PyError> {
    Ok(Value::SmallInt(42))
}

/// Keyword probe: returns a tuple `(n_pos, n_kw, first_kw_name,
/// first_kw_value)` — or `(n_pos, 0, 0, None)` when no kwargs.
/// Pins exactly what the adapter delivered.
pub(crate) fn kw_probe(ctx: &mut CallCtx, pos: &[Value], kw: &[(u32, Value)]) -> Result<Value, PyError> {
    let (name, value) = kw
        .first()
        .map(|(n, v)| (Value::SmallInt(*n as i64), *v))
        .unwrap_or((Value::SmallInt(0), Value::None));
    let tuple = Obj::Tuple(vec![
        Value::SmallInt(pos.len() as i64),
        Value::SmallInt(kw.len() as i64),
        name,
        value,
    ]);
    // Copy `types` out first: `ctx.alloc` borrows `ctx` mutably.
    let types = ctx.types;
    Ok(Value::Heap(ctx.alloc(Some(types.tuple), tuple)))
}

pub(crate) fn tuple_items(arena: &GcArena, v: Value) -> Vec<Value> {
    match v {
        Value::Heap(r) => match arena.get(r) {
            Obj::Tuple(items) => items.clone(),
            _ => panic!("not a tuple"),
        },
        _ => panic!("not a heap tuple"),
    }
}

// ── `arg_check_num` ─────────────────────────────────────────────

#[test]
fn arg_check_fixed_arities() {
    assert!(arg_check_num(0, 0, 0, 0, false));
    assert!(!arg_check_num(1, 0, 0, 0, false));
    assert!(!arg_check_num(0, 1, 0, 0, false));
    assert!(arg_check_num(1, 0, 1, 1, false));
    assert!(!arg_check_num(0, 0, 1, 1, false));
    assert!(!arg_check_num(2, 0, 1, 1, false));
    assert!(!arg_check_num(1, 1, 1, 1, false));
    assert!(arg_check_num(2, 0, 2, 2, false));
    assert!(!arg_check_num(3, 0, 2, 2, false));
}

#[test]
fn arg_check_var_range_and_kw() {
    assert!(arg_check_num(0, 0, 0, 5, false));
    assert!(arg_check_num(5, 0, 0, 5, false));
    assert!(!arg_check_num(6, 0, 0, 5, false));
    assert!(!arg_check_num(0, 1, 0, 5, false));
    assert!(arg_check_num(0, 1, 0, 5, true));
    assert!(arg_check_num(3, 2, 1, 4, true));
    assert!(!arg_check_num(0, 0, 1, 4, true));
    assert!(!arg_check_num(5, 0, 1, 4, true));
    // `fun_sig_range` decodes C7's `(min << 8) | max` packing.
    assert_eq!(fun_sig_range(fun_n_args(2, 4)), (2, 4));
}

// ── `fun_builtin_*_call` adapters ───────────────────────────────

#[test]
fn builtin_0_call() {
    let mut cfx = CFx::new();
    let t0 = cfx.fun_builtin_0;
    let f = cfx.builtin(t0, "zero", 0, 0, ret_const);
    let mut ctx = cfx.ctx();
    assert_eq!(fun_builtin_0_call(&mut ctx, f, 0, 0, &[]), Ok(Value::SmallInt(42)));
    assert!(fun_builtin_0_call(&mut ctx, f, 1, 0, &[Value::SmallInt(1)]).is_err());
    assert!(fun_builtin_0_call(&mut ctx, f, 0, 1, &[Value::Qstr(0), Value::None]).is_err());
    assert!(fun_builtin_0_call(&mut ctx, Value::SmallInt(1), 0, 0, &[]).is_err());
}

#[test]
fn builtin_1_call() {
    let mut cfx = CFx::new();
    let t1 = cfx.fun_builtin_1;
    let f = cfx.builtin(t1, "one", 1, 1, ret_arg0);
    let mut ctx = cfx.ctx();
    assert_eq!(
        fun_builtin_1_call(&mut ctx, f, 1, 0, &[Value::SmallInt(7)]),
        Ok(Value::SmallInt(7))
    );
    assert!(fun_builtin_1_call(&mut ctx, f, 0, 0, &[]).is_err());
    assert!(fun_builtin_1_call(&mut ctx, f, 2, 0, &[Value::SmallInt(1), Value::SmallInt(2)])
        .is_err());
    assert!(fun_builtin_1_call(
        &mut ctx,
        f,
        1,
        1,
        &[Value::SmallInt(1), Value::Qstr(0), Value::None]
    )
    .is_err());
}

#[test]
fn builtin_2_and_3_call() {
    let mut cfx = CFx::new();
    let t2 = cfx.fun_builtin_2;
    let t3 = cfx.fun_builtin_3;
    let f2 = cfx.builtin(t2, "two", 2, 2, ret_arg1);
    let f3 = cfx.builtin(t3, "three", 3, 3, ret_arg2);
    let mut ctx = cfx.ctx();
    let args2 = [Value::SmallInt(1), Value::SmallInt(2)];
    assert_eq!(fun_builtin_2_call(&mut ctx, f2, 2, 0, &args2), Ok(Value::SmallInt(2)));
    assert!(fun_builtin_2_call(&mut ctx, f2, 1, 0, &args2[..1]).is_err());
    let args3 = [Value::SmallInt(1), Value::SmallInt(2), Value::SmallInt(3)];
    assert_eq!(fun_builtin_3_call(&mut ctx, f3, 3, 0, &args3), Ok(Value::SmallInt(3)));
    assert!(fun_builtin_3_call(&mut ctx, f3, 2, 0, &args3[..2]).is_err());
}

#[test]
fn builtin_var_call_no_kw() {
    let mut cfx = CFx::new();
    let tv = cfx.fun_builtin_var;
    let f = cfx.builtin(tv, "v", 1, 3, ret_arg0);
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(9), Value::SmallInt(8)];
    assert_eq!(fun_builtin_var_call(&mut ctx, f, 1, 0, &args[..1]), Ok(Value::SmallInt(9)));
    assert_eq!(fun_builtin_var_call(&mut ctx, f, 2, 0, &args), Ok(Value::SmallInt(9)));
    assert!(fun_builtin_var_call(&mut ctx, f, 0, 0, &[]).is_err());
    assert!(fun_builtin_var_call(
        &mut ctx,
        f,
        4,
        0,
        &[Value::SmallInt(1); 4]
    )
    .is_err());
    // Any kwarg fails a no-kw var builtin (terse mismatch).
    assert!(fun_builtin_var_call(
        &mut ctx,
        f,
        1,
        1,
        &[Value::SmallInt(1), Value::Qstr(0), Value::None]
    )
    .is_err());
}

#[test]
fn builtin_var_call_kw() {
    let mut cfx = CFx::new();
    let k_id = cfx.q("k");
    let f = cfx.kw_builtin("kw", 0, 2, kw_probe);
    let mut ctx = cfx.ctx();
    // One positional + one kwarg: the probe echoes both.
    let args = [Value::SmallInt(5), Value::Qstr(k_id), Value::SmallInt(6)];
    let r = fun_builtin_var_call(&mut ctx, f, 1, 1, &args).unwrap();
    assert_eq!(
        tuple_items(&ctx.arena, r),
        vec![
            Value::SmallInt(1),
            Value::SmallInt(1),
            Value::SmallInt(k_id as i64),
            Value::SmallInt(6),
        ]
    );
    // No kwargs at all: the kw fn is still called (empty pairs).
    let r = fun_builtin_var_call(&mut ctx, f, 0, 0, &[]).unwrap();
    assert_eq!(
        tuple_items(&ctx.arena, r),
        vec![
            Value::SmallInt(0),
            Value::SmallInt(0),
            Value::SmallInt(0),
            Value::None,
        ]
    );
    // Over the positional max: error, even with kwargs.
    assert!(fun_builtin_var_call(
        &mut ctx,
        f,
        3,
        0,
        &[Value::SmallInt(1); 3]
    )
    .is_err());
}

#[test]
fn call_fun_builtin_dispatches_by_sig() {
    // A var builtin with sig (2, 2) behaves exactly like the
    // fixed-2 adapter (same predicate, same `args[..n_args]`).
    let mut cfx = CFx::new();
    let tv = cfx.fun_builtin_var;
    let f = cfx.builtin(tv, "two", 2, 2, ret_arg1);
    let mut ctx = cfx.ctx();
    let args = [Value::SmallInt(10), Value::SmallInt(20)];
    assert_eq!(call_fun_builtin(&mut ctx, f, 2, 0, &args), Ok(Value::SmallInt(20)));
    assert!(call_fun_builtin(&mut ctx, f, 1, 0, &args[..1]).is_err());
    assert!(call_fun_builtin(&mut ctx, Value::None, 2, 0, &args).is_err());
}

// ── `new_fun_bc` / `fun_bc_get_name` ────────────────────────────

#[test]
fn new_fun_bc_unpacks_defaults_and_traces() {
    let mut cfx = CFx::new();
    let def_tuple = cfx.fx.heap_tuple(vec![Value::SmallInt(5), Value::SmallInt(6)]);
    let def_dict = cfx.fx.dict();
    let (module, globals) = cfx.module_with_globals();
    let name_id = cfx.q("f");
    let type_ = cfx.fun_bc;
    let mut ctx = cfx.ctx();
    let fun = new_fun_bc(
        &mut ctx,
        type_,
        name_id,
        2,
        0,
        Some(def_tuple),
        Some(def_dict),
        Some(module),
    );
    let data = match ctx.arena.get(fun) {
        Obj::FunBc(d) => d.clone(),
        _ => panic!("not a FunBc"),
    };
    assert_eq!(data.name, name_id);
    assert_eq!(data.n_args, 2);
    assert_eq!(data.def_pos, vec![Value::SmallInt(5), Value::SmallInt(6)]);
    assert_eq!(data.def_kw, Some(def_dict));
    assert_eq!(data.context, Some(module));
    // `children()` traces the defaults, the dict and the module.
    let children = Obj::FunBc(data).children();
    assert_eq!(
        children,
        vec![Value::SmallInt(5), Value::SmallInt(6), def_dict, Value::Heap(module)]
    );
    // `fun_bc_get_name` reads the stored name; `__globals__`
    // resolves through the context module.
    let fun_value = Value::Heap(fun);
    assert_eq!(fun_bc_get_name(&ctx.arena, fun_value), Some(name_id));
    assert_eq!(fun_bc_get_name(&ctx.arena, Value::SmallInt(1)), None);
    let globals_id = static_qstr_id(b"__globals__").unwrap();
    assert_eq!(fun_bc_attr(&mut ctx, fun_value, globals_id), Ok(Some(globals)));
}

#[test]
fn new_fun_bc_no_defaults() {
    let mut cfx = CFx::new();
    let name_id = cfx.q("g");
    let type_ = cfx.fun_bc;
    let mut ctx = cfx.ctx();
    let fun = new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None);
    let data = match ctx.arena.get(fun) {
        Obj::FunBc(d) => d.clone(),
        _ => panic!("not a FunBc"),
    };
    assert!(data.def_pos.is_empty());
    assert_eq!(data.def_kw, None);
    assert_eq!(data.context, None);
    assert_eq!(Obj::FunBc(data).children(), Vec::<Value>::new());
    // A non-tuple `def_pos` is treated as "no defaults".
    let fun2 = new_fun_bc(
        &mut ctx,
        type_,
        name_id,
        0,
        0,
        Some(Value::SmallInt(1)),
        None,
        None,
    );
    match ctx.arena.get(fun2) {
        Obj::FunBc(d) => assert!(d.def_pos.is_empty()),
        _ => panic!("not a FunBc"),
    }
}

// ── attr / print / type data ────────────────────────────────────

#[test]
fn fun_bc_attr_name_and_globals() {
    let mut cfx = CFx::new();
    let (module, globals) = cfx.module_with_globals();
    let name_id = cfx.q("f");
    // All four attr names are resolved through the pool BEFORE
    // the `ctx` is created — `__doc__` is NOT a static qstr
    // (`static_qstr_id` returns `None` for it), so it must be
    // interned dynamically like the compiler would.
    let name_attr = cfx.q("__name__");
    let globals_attr = cfx.q("__globals__");
    let qualname_attr = cfx.q("__qualname__");
    let doc_attr = cfx.q("__doc__");
    let type_ = cfx.fun_bc;
    let mut ctx = cfx.ctx();
    let fun = new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, Some(module));
    let fun_value = Value::Heap(fun);
    assert_eq!(fun_bc_attr(&mut ctx, fun_value, name_attr), Ok(Some(Value::Qstr(name_id))));
    assert_eq!(fun_bc_attr(&mut ctx, fun_value, globals_attr), Ok(Some(globals)));
    // The reference binary raises AttributeError for these two
    // (probed); the load path reports "not found".
    assert_eq!(fun_bc_attr(&mut ctx, fun_value, qualname_attr), Ok(None));
    assert_eq!(fun_bc_attr(&mut ctx, fun_value, doc_attr), Ok(None));
    // No context module: `__globals__` is not found.
    let fun2 = new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None);
    assert_eq!(fun_bc_attr(&mut ctx, Value::Heap(fun2), globals_attr), Ok(None));
    // A non-FunBc value: not found.
    assert_eq!(fun_bc_attr(&mut ctx, Value::SmallInt(1), name_attr), Ok(None));
}

#[test]
fn fun_bc_print_format() {
    let mut cfx = CFx::new();
    let name_id = cfx.q("f");
    let type_ = cfx.fun_bc;
    let mut ctx = cfx.ctx();
    let fun = new_fun_bc(&mut ctx, type_, name_id, 0, 0, None, None, None);
    let mut out = String::new();
    fun_bc_print(&mut ctx, Value::Heap(fun), PrintKind::Str, &mut out);
    assert_eq!(out, format!("<function f at 0x{:x}>", fun.0));
    // The kind is ignored: repr prints identically (probed).
    let mut out2 = String::new();
    fun_bc_print(&mut ctx, Value::Heap(fun), PrintKind::Repr, &mut out2);
    assert_eq!(out2, out);
    // A non-FunBc value prints nothing.
    let mut out3 = String::new();
    fun_bc_print(&mut ctx, Value::SmallInt(1), PrintKind::Str, &mut out3);
    assert_eq!(out3, "");
}

#[test]
fn callable_type_data_shapes() {
    let mut cfx = CFx::new();
    let d = empty_dict(&mut cfx.fx.arena, &cfx.fx.roots);
    // `qstr_str` returns the raw bytes (`&[u8]`); the id is a
    // runtime `Qstr` (`u16`) — cast the stored `u32` down.
    let name = |td: &TypeData| cfx.fx.pool.qstr_str(td.name as u16);
    let bc = fun_bc_type_data(d);
    assert_eq!(name(&bc), b"function" as &[u8]);
    assert_eq!(bc.flags, crate::objects::obj::FLAG_BINDS_SELF);
    assert!(bc.call.is_some());
    assert!(bc.print.is_some());
    assert!(bc.unary_op.is_none());
    let b0 = fun_builtin_0_type_data(d);
    assert_eq!(name(&b0), b"function" as &[u8]);
    assert_eq!(b0.flags, crate::objects::obj::FLAG_BINDS_SELF | crate::objects::obj::FLAG_BUILTIN_FUN);
    // `fn_addr_eq`: the registered slot must be exactly the
    // matching adapter (plain `==` on fn pointers is the
    // `unpredictable_function_pointer_comparisons` lint).
    assert!(std::ptr::fn_addr_eq(
        b0.call.unwrap(),
        fun_builtin_0_call as crate::objects::slots::CallFn
    ));
    // NO print slot: the Zig's `builtinFunType` fills ONLY the
    // call slot (`objfun.zig:405-425`, `slot_index_print = 0`) —
    // `repr(len) == "<function>"` is the VM's default print for
    // objects without a print slot (C13's), not a slot here.
    let b1 = fun_builtin_1_type_data(d);
    assert!(std::ptr::fn_addr_eq(
        b1.call.unwrap(),
        fun_builtin_1_call as crate::objects::slots::CallFn
    ));
    let b2 = fun_builtin_2_type_data(d);
    assert!(std::ptr::fn_addr_eq(
        b2.call.unwrap(),
        fun_builtin_2_call as crate::objects::slots::CallFn
    ));
    let b3 = fun_builtin_3_type_data(d);
    assert!(std::ptr::fn_addr_eq(
        b3.call.unwrap(),
        fun_builtin_3_call as crate::objects::slots::CallFn
    ));
    let bv = fun_builtin_var_type_data(d);
    assert!(std::ptr::fn_addr_eq(
        bv.call.unwrap(),
        fun_builtin_var_call as crate::objects::slots::CallFn
    ));
    let bm = bound_meth_type_data(d);
    assert_eq!(name(&bm), b"bound_method" as &[u8]);
    assert_eq!(bm.flags, 0);
    assert!(bm.call.is_some());
    assert!(bm.unary_op.is_some());
    assert!(bm.binary_op.is_some());
    assert!(bm.print.is_none());
    let cl = closure_type_data(d);
    assert_eq!(name(&cl), b"closure" as &[u8]);
    assert_eq!(cl.flags, crate::objects::obj::FLAG_BINDS_SELF);
    assert!(cl.call.is_some());
    assert!(cl.print.is_none());
}

