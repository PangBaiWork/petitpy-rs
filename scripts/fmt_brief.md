# Lane FMT — vendor C: formatfloat.c → Rust

## Standing rules (read first, non-negotiable)

`~/workspace/petitpy-rs/docs/agent-rules.md` — 对照原文、不许自创、同一套测试、
纯 Rust std（no external crates）、文件 <800 行、无 unsafe（无理由不许）、
原项目只读（`~/workspace/petitpy-orig`，绝不修改）、每块一提交、不许假完成。
主 agent 会独立验证后才合并；不要自己合并到 master。

## Worktree / branch

- Worktree: `~/workspace/petitpy-rs-fmt`, branch `lane/fmt`
- Base: master `e68c7e1`（已含 A2 lexer + Lane C mpz，树可直接编译）
- 只改本 lane 的文件。提交时只 `git add` 自己的文件路径，**绝不 `git add -A`**
  （主 agent 曾在 A3 进行中误扫过别人的文件）。
- A3 正在主线工作，不要动主线；你的所有工作都在本 worktree。

## Source (read-only original, petitpy devel HEAD df7de3a)

- `vendor/micropython/py/formatfloat.c` — 569 行，唯一公开函数：
  `int mp_format_float(mp_float_t f, char *buf, size_t buf_size, char fmt, int prec, char sign)`
- `vendor/micropython/py/formatfloat.h` — 36 行（声明 + 返回值约定，先读它）
- 调用者（定 API 边界用，读它们，不要移植它们）：
  - Zig: `src/objects/strings/mpprint.zig:36`（extern 声明）与 `:458`（调用点，
    传 `buf.len - 3`）；`:382` 注释写了 vendor 实现与 CPython 的已知差异，必读。
  - C: `vendor/micropython/py/mpprint.c`、`objcomplex.c`、`objfloat.c`

## Target

`src/vendor/formatfloat.rs`（超 800 行则拆分子模块；`src/lib.rs` 加 `pub mod` 行
已由 Lane C 建好 `src/vendor/mod.rs`——在里面加 re-export）。

API（忠实镜像 C 签名，C 惯用法转不过来的地方才改，并写清理由）：
- `format_float_into_buf(f: f64, fmt: u8, prec: i32, sign: u8, buf: &mut [u8]) -> i32`
  —— 忠实移植，含 C 的 buffer 截断语义和返回值约定（返回值含义从 formatfloat.h
  和 C 代码确认，写进结果文件）。
- `format_float(f: f64, fmt: u8, prec: i32, sign: u8) -> String`
  —— 仅作便利包装，内部调 into_buf（无界版本不得改变格式化算法）。
- `fmt`/`prec`/`sign` 的取值与含义以 C 代码为准（`%f`/`%e`/`%g` 等、
  精度缺省、符号位处理）；不要按 CPython 的 format spec 重新设计。

## Acceptance（全部要数，不要形容词）

1. `cargo test --release --lib --tests` 全过：A1 16 + A2 17 + Lane C 36 保持，
   加本 lane 的单测（覆盖每个分支：fmt 种类、prec 边界、sign、inf/nan/±0、
   大/小量级、buffer 刚好装下与装不下）。
2. **差分探针电池 ≥200 例，0 失败**（`scripts/gen_fmt_vectors.py` + `scripts/fmt_vectors.txt`，
   确定性种子，写死在脚本里；重跑生成器文件必须一字不变）。
   Oracle 优先级：**原版参照二进制第一**（`~/workspace/petitpy-orig/zig-out/bin/petitpy`，
   用 `'%f' % x`、`'%e'`、`'%g'`、`'%5.2f'`、`format(x, '.3f')`、`str(x)` 等脚本跑），
   CPython 的 `format()` 只在与原版一致的地方当 oracle；原版与 CPython 不一致的
   地方（mpprint.zig:382 注释起步）要逐项写进结果文件的 divergences，
   探针按原版语义建模，不要为了迎合 CPython 改算法。
3. `cargo build --release` 0 warnings；`src/vendor/formatfloat.rs` 无 unsafe；
   纯 std，无外部 crate；每个文件 <800 行。
4. 原项目 `~/workspace/petitpy-orig` 保持干净（`git status` 空）。
5. 结果文件 `scripts/fmt_result.txt`：1:1 函数映射、返回值/截断约定、divergences
   清单、测试与探针数字、commit hash。
6. 一个提交，branch `lane/fmt`，commit message：`Lane FMT: port formatfloat.c to Rust`。

## Notes

- 这是纯算法移植：不要碰 Value/Obj、不要碰 mpprint/objstr（那是 Phase C/D 的事）。
- C 代码里对 `double` 的位操作/舍入要逐行对照，不要用 Rust 的 `format!("{:e}")`
  之类“差不多”的东西代替——原版的输出就是标准，哪怕它和 CPython 不同。
- 完成后把结果文件写好就停下，等主 agent 验证；不要自己合并不验证。
