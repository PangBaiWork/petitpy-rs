//! Lane D3 tests: the `math` module (`src/modules/modmath.zig`),
//! end-to-end snippets through `Vm` (the `math` module is built
//! with `build_math_module` and registered in
//! `state.builtin_modules`, the way D1 will register it —
//! `import math` then resolves). Expected values are the
//! reference binary's, probed 2026-11-18 (see
//! `scripts/d3_result.txt` §2).

use crate::objects::containers::objdict::dict_get_value;
use crate::objects::obj::obj_get_type;
use crate::objects::primitives::ops::static_qstr_id;
use crate::objects::value::{Obj, Value};
use crate::runtime::vm::{Vm, VmError};

use crate::modules::modmath::build_math_module;

/// Build the `math` module and register it the way D1 will
/// (`state.builtin_modules`), then pin it in `roots.extra`.
fn setup_math(vm: &mut Vm) {
    let fun_var = vm.state.vm_types.fun_builtin_var;
    let module_type = vm.state.vm_types.module;
    let math_module = {
        let (mut ctx, state) = vm.split();
        let m = build_math_module(&mut ctx, fun_var, module_type).expect("math module builds");
        let math_id = static_qstr_id(b"math").expect("math is a static qstr");
        state.builtin_modules.push((math_id, m));
        m
    };
    vm.pin(math_module);
}

/// A fresh VM with `math` registered, plus the read-back helpers
/// (the `vm_tests.rs` `Fx` / `objstringio_tests.rs` pattern).
struct Fx {
    vm: Vm,
}

impl Fx {
    fn new() -> Self {
        let mut vm = Vm::new();
        setup_math(&mut vm);
        Self { vm }
    }

    fn run(&mut self, src: &str) -> Result<Value, VmError> {
        self.vm.load_module_from_source(src, "<test>")
    }

    fn run_ok(&mut self, src: &str) -> Value {
        match self.run(src) {
            Ok(v) => v,
            Err(VmError::Raised(exc)) => {
                let (name, msg) = self.raised_parts(exc);
                panic!("module raised {name}: {msg}")
            }
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    /// Run a module that must raise; return (type name, first
    /// arg as a string).
    fn raised_msg(&mut self, src: &str) -> (String, String) {
        match self.run(src) {
            Err(VmError::Raised(exc)) => self.raised_parts(exc),
            Ok(_) => panic!("expected module to raise, it returned Ok"),
            Err(e) => panic!("module failed to run: {e:?}"),
        }
    }

    fn raised_parts(&mut self, exc: Value) -> (String, String) {
        let t = obj_get_type(&self.vm.arena, &self.vm.types, exc).expect("an exception has a type");
        let name = match self.vm.arena.get(t) {
            Obj::Type(d) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(d.name as u16)).into_owned()
            }
            _ => panic!("exception type is not a type object"),
        };
        let msg = match exc {
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Exception(d) => match d.args.first() {
                    Some(v) => self.val_str(*v),
                    None => String::new(),
                },
                other => panic!("raised value is not an exception: {other:?}"),
            },
            other => panic!("raised value is not an exception: {other:?}"),
        };
        (name, msg)
    }

    /// Read a module global by name (`None` when absent).
    fn g(&mut self, name: &str) -> Option<Value> {
        let q = self.vm.pool.from_strn(name.as_bytes()).ok()? as u32;
        let globals = Value::Heap(self.vm.state.globals);
        let (mut ctx, _state) = self.vm.split();
        dict_get_value(&mut ctx, globals, Value::Qstr(q)).ok()
    }

    fn gf(&mut self, name: &str) -> f64 {
        match self.g(name) {
            Some(Value::Heap(r)) => match self.vm.arena.get(r) {
                Obj::Float(f) => *f,
                other => panic!("global {name} is not a float: {other:?}"),
            },
            other => panic!("global {name} is not a float: {other:?}"),
        }
    }

    fn gb(&mut self, name: &str) -> bool {
        match self.g(name) {
            Some(Value::Bool(b)) => b,
            other => panic!("global {name} is not a bool: {other:?}"),
        }
    }

    /// A string-valued `Value` (`Qstr` or heap `Str`), as a
    /// Rust `String`.
    fn val_str(&mut self, v: Value) -> String {
        match v {
            Value::Qstr(q) => {
                String::from_utf8_lossy(self.vm.pool.qstr_str(q as u16)).into_owned()
            }
            Value::Heap(r) => match self.vm.arena.get(r) {
                Obj::Str(s) => String::from_utf8_lossy(&s.bytes).into_owned(),
                other => panic!("value is not a str: {other:?}"),
            },
            other => panic!("value is not a str: {other:?}"),
        }
    }
}

#[test]
fn math_constants() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
ok_name = math.__name__ == 'math'
v_e = math.e
v_pi = math.pi
v_tau = math.tau
v_inf = math.inf
v_nan = math.nan
"#,
    );
    assert!(fx.gb("ok_name"));
    assert_eq!(fx.gf("v_e"), std::f64::consts::E);
    assert_eq!(fx.gf("v_pi"), std::f64::consts::PI);
    assert_eq!(fx.gf("v_tau"), std::f64::consts::TAU);
    assert!(fx.gf("v_inf").is_infinite() && fx.gf("v_inf") > 0.0);
    assert!(fx.gf("v_nan").is_nan());
}

#[test]
fn math_trig_exp_log() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.sin(0) == 0.0
assert math.cos(0) == 1.0
assert math.tan(0) == 0.0
assert math.exp(0) == 1.0
assert math.exp(1) == math.e
assert math.log(math.e) == 1.0
assert math.sqrt(4) == 2.0
assert math.sqrt(2) == 1.4142135623730951
assert math.log(2) == 0.6931471805599453
assert math.log(8, 2) == 3.0
assert math.log(100, 10) == 2.0
assert math.asin(0) == 0.0
assert math.acos(1) == 0.0
assert math.atan(0) == 0.0
assert math.atan2(1, 1) == math.pi / 4
assert math.hypot(3, 4) == 5.0
assert math.pow(2, 10) == 1024.0
assert math.copysign(1, -2) == -1.0
assert math.copysign(1, 2) == 1.0
assert math.fmod(5.5, 2) == 1.5
assert math.fmod(-5.5, 2) == -1.5
assert math.fabs(-3) == 3.0
"#,
    );
}

#[test]
fn math_log2_log10_portable_form() {
    // The vendor's `log(x) * (1/ln 2)` form: `log2(8)` is
    // 2.9999999999999996, NOT 3.0 (probed on the reference).
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.log2(8) == 2.9999999999999996
assert math.log2(2) == 1.0
v_log10 = math.log10(1000)
"#,
    );
    // log10(1000): assert against the Rust computation of the
    // same portable form (the exact value is libm-dependent).
    let expected = 1000f64.ln() * 0.4342944819032518276511289189;
    assert_eq!(fx.gf("v_log10"), expected);
}

#[test]
fn math_domain_errors() {
    let mut fx = Fx::new();
    for src in [
        "import math\nmath.sqrt(-1)",
        "import math\nmath.log(0)",
        "import math\nmath.log(-1)",
        "import math\nmath.exp(1000)",
        "import math\nmath.pow(0, -1)",
        "import math\nmath.pow(-2, 0.5)",
        "import math\nmath.asin(2)",
        "import math\nmath.acos(2)",
        "import math\nmath.log2(-1)",
        "import math\nmath.log10(0)",
        "import math\nmath.log(8, 0)",
        "import math\nmath.log(8, -2)",
        "import math\nmath.fmod(1, 0)",
    ] {
        assert_eq!(
            fx.raised_msg(src),
            ("ValueError".to_string(), "math domain error".to_string()),
            "{src}"
        );
    }
}

#[test]
fn math_log_base_one_is_div_by_zero() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.raised_msg("import math\nmath.log(8, 1)"),
        ("ZeroDivisionError".to_string(), "divide by zero".to_string())
    );
}

#[test]
fn math_ceil_floor_trunc() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.ceil(1.5) == 2
assert math.ceil(-1.5) == -1
assert math.ceil(3) == 3
assert math.floor(1.5) == 1
assert math.floor(-1.5) == -2
assert math.trunc(1.9) == 1
assert math.trunc(-1.9) == -1
assert math.trunc(2**70 + 0.5) == 2**70
"#,
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.ceil(float('inf'))"),
        (
            "OverflowError".to_string(),
            "can't convert inf to int".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.ceil(float('nan'))"),
        (
            "ValueError".to_string(),
            "can't convert NaN to int".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.floor(float('-inf'))"),
        (
            "OverflowError".to_string(),
            "can't convert inf to int".to_string()
        )
    );
}

#[test]
fn math_isnan_isinf_isfinite() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.isnan(math.nan)
assert not math.isnan(1.0)
assert math.isinf(math.inf)
assert math.isinf(-math.inf)
assert not math.isinf(1.0)
assert math.isfinite(1.5)
assert not math.isfinite(math.inf)
assert not math.isfinite(math.nan)
"#,
    );
}

#[test]
fn math_frexp() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.frexp(2.5) == (0.625, 2)
assert math.frexp(0.0) == (0.0, 0)
assert math.frexp(-8.0) == (-0.5, 4)
assert math.frexp(math.inf) == (math.inf, 0)
assert math.frexp(5e-324) == (0.5, -1073)
"#,
    );
    // frexp(nan) == (nan, 0): NaN never equals itself, so check
    // the parts separately.
    fx.run_ok(
        r#"
import math
m = math.frexp(math.nan)
assert math.isnan(m[0])
assert m[1] == 0
"#,
    );
}

#[test]
fn math_modf() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.modf(2.5) == (0.5, 2.0)
assert math.modf(-2.5) == (-0.5, -2.0)
assert math.modf(0.0) == (0.0, 0.0)
assert math.modf(math.inf) == (0.0, math.inf)
m = math.modf(math.nan)
assert math.isnan(m[0]) and math.isnan(m[1])
"#,
    );
}

#[test]
fn math_ldexp() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.ldexp(1.5, 2) == 6.0
assert math.ldexp(1.0, -1074) == 5e-324
assert math.ldexp(1.0, -1075) == 0.0
assert math.ldexp(0.0, 100) == 0.0
assert math.ldexp(1.5, 10000) == math.inf
assert math.ldexp(math.inf, 2) == math.inf
assert math.isnan(math.ldexp(math.nan, 2))
"#,
    );
}

#[test]
fn math_radians_degrees() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.degrees(1) == 57.29577951308232
assert math.radians(90) == 1.5707963267948966
assert math.radians(180) == math.pi
"#,
    );
}

#[test]
fn math_gcd() {
    let mut fx = Fx::new();
    fx.run_ok(
        r#"
import math
assert math.gcd() == 0
assert math.gcd(0) == 0
assert math.gcd(12, 8) == 4
assert math.gcd(-12, 8) == 4
assert math.gcd(12, 8, 20) == 4
assert math.gcd(2**100, 2**50) == 2**50
assert math.gcd(True, False) == 1
"#,
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.gcd(1.5)"),
        (
            "TypeError".to_string(),
            "gcd() requires integers".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.gcd('x')"),
        (
            "TypeError".to_string(),
            "gcd() requires integers".to_string()
        )
    );
}

#[test]
fn math_type_errors_and_missing_names() {
    let mut fx = Fx::new();
    for (src, want) in [
        (
            "import math\nmath.sqrt('4')",
            ("TypeError", "can't convert to float"),
        ),
        (
            "import math\nmath.ceil('x')",
            ("TypeError", "can't convert to float"),
        ),
        (
            "import math\nmath.isnan('x')",
            ("TypeError", "can't convert to float"),
        ),
        (
            "import math\nmath.ldexp(1.5, 'x')",
            ("TypeError", "can't convert to int"),
        ),
    ] {
        assert_eq!(
            fx.raised_msg(src),
            (want.0.to_string(), want.1.to_string()),
            "{src}"
        );
    }
    // No factorial / isclose / gamma in this module (probed).
    // `hasattr` is a D1 builtin, not bound yet — use try/except.
    fx.run_ok(
        r#"
import math
try:
    math.factorial
    assert False
except AttributeError:
    assert True
try:
    math.isclose
    assert False
except AttributeError:
    pass
try:
    math.gamma
    assert False
except AttributeError:
    pass
"#,
    );
    // Wrong arg counts are the polled sig error.
    assert_eq!(
        fx.raised_msg("import math\nmath.log(2, 3, 4)"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
    assert_eq!(
        fx.raised_msg("import math\nmath.sqrt()"),
        (
            "TypeError".to_string(),
            "argument num/types mismatch".to_string()
        )
    );
}
