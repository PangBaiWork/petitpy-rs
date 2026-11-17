# Phase B — compile + emit + bridge (Zig → Rust)

## Standing rules (read first, non-negotiable)

`~/workspace/petitpy-rs/docs/agent-rules.md` — 对照原文、不许自创、同一套测试、
纯 Rust std（no external crates）、文件 <800 行、无 unsafe（无理由不许）、
原项目只读（`~/workspace/petitpy-orig`，绝不修改）、每块一提交、不许假完成。
主 agent 会独立验证后才合并；不要自己合并不验证。

## Worktree / branch

- MAIN worktree `~/workspace/petitpy-rs`, branch master (like A3).
- Base: master `942d8d0`（已含 A1/A2/A3 + Lane M/C/FMT/RE15，主线 170 测试全绿）。
- 只改本 phase 的文件。提交时只 `git add` 自己的文件路径，**绝不 `git add -A`**。
- 主线目前没有别的 lane 在跑；你的所有工作都在主线。

## Source (read-only original, petitpy devel HEAD df7de3a)

- `src/compiler/compile.zig` — 5,593 行，**12 test blocks**
- `src/compiler/emit.zig` — 1,682 行，**12 test blocks**
- `src/compiler/bridge.zig` — 123 行，**0 test blocks**
- 合计 7,398 行，24 个 test blocks。
- compile.zig / emit.zig **没有 @cImport**（只有注释里提到）；bridge.zig 的
  @cImport 只有 malloc/free（见下）。
- 入口（compile.zig:5175）：`compile(allocator, qctx, tree: ParseTree,
  source_file: Qstr, is_repl: bool) CompileError!mpy.Module`——输入是 A3 的
  ParseTree，输出是 Lane M 的 mpy.Module。三遍结构 SCOPE / CODE_SIZE / EMIT，
  必须原样保留（有 "3-pass consistency" 测试盯着字节码偏移量）。
- emit.zig：MP_BC_* 操作码常量（从 C 手工移植的）、Emit / EmitCommon /
  ConstObj / Pass；import 只有 qstr、scope、mpy。
- bridge.zig：C 可调入口 `npy_zig_compile_to_mpy`——源串 → .mpy 字节，
  malloc 出 buf、错误码 ERR_OK/OOM/SYNTAX/INDENT、输入种类
  SINGLE/FILE/EVAL（对 MicroPython `mp_parse_input_kind_t`）。

## Target

- `src/compiler/emit/`——emit.zig 1,682 行，必须像 lexer/parser 一样拆子模块，
  每文件 <800 行。
- `src/compiler/compile/`——compile.zig 5,593 行，必须拆子模块（按 Compiler
  的结构 / pass / 各 compile_* 处理函数分；dispatch 表单列一处）。
- `src/compiler/bridge.rs`——123 行，单文件。
- 在 `src/compiler/mod.rs` 里加模块声明。
- 依赖都已在主线：A3 的 parser/scope/bigint、Lane M 的 mpy reader/writer。
- 所有权设计：沿用 A3 的 arena 模式（ScopeArena + id handle），不要用
  Rc<RefCell>（会在编译期借用检查里 double-borrow）；偏离 Zig 的地方写进
  结果文件，说明理由。

## Bridge 的 Rust 形态（重要）

- 移植为安全 Rust API：`compile_to_mpy(source: &str, kind: InputKind)
  -> Result<Vec<u8>, CompileError>`（kind = Single/File/Eval）。
  C 的 malloc/free 出 buf 契约改为 Vec 所有权；ERR_* 错误码映射到
  CompileError/SyntaxError 枚举。
- **不要**加 `extern "C"` 导出——C-ABI 导出是 Phase F（嵌入 API）的事，
  不在本 phase。
- 内部走完整管线：lexer → parser → compile → mpy.write（与 bridge.zig
  的注释一致）。

## Acceptance（全部要数，不要形容词）

1. 24 个 ported test blocks 全过（compile 12 + emit 12，名字见原文件；
   含 dispatch 表、3-pass 一致性、scopeComputeThings 顺序、
   compile 端到端 `x = 1`、SyntaxError 上浮、wk cache、EmitCommon
   round-trip、encodeUint/encodeInt、label/jump）。
2. **强差分检查一：.mpy 字节一致**——`scripts/check_compile_mpy.py`：
   用 Rust 编译器把 552 个 corpus `.py` 全编成 .mpy，与原版
   `petitpy --emit-mpy` 的输出逐字节比对，**552/552 一致**。
   （`petitpy --emit-mpy F.py` 出 base64 .mpy；写盘时解码。）
3. **强差分检查二：跑起来一致**——`scripts/check_compile_run.py`：
   每个 Rust 编出的 .mpy 用 `petitpy-vm` 跑，与原版直接跑 `.py`
   的 stdout/stderr/rc 比对；7 个已知 corpus 失败 + 运行时失败
   （deque_micropython.py 等）按 A2/A3 的办法区分（parse 错 vs
   runtime 错），逐项列明，不得含糊。
4. `cargo test --release --lib --tests` 全过（基线 170 保持 + 本 phase
   的新测试）；`cargo build --release` 0 warnings；无 unsafe；每文件
   <800 行；纯 std，无外部 crate。
5. 原项目 `~/workspace/petitpy-orig` 保持干净（`git status` 空）。
6. 结果文件 `scripts/b_result.txt`：1:1 函数/类型映射、pass 结构说明、
   与 Zig 不同的所有权设计、divergences 清单、测试与差分数字、
   commit hash。
7. 一个提交，branch master，commit message：
   `Phase B: port compile + emit + bridge from Zig`。

## Notes

- 这是前端链最后一块：做完它，`petitpy-rs` 就能把任意 `.py` 编成与原版
  字节一致的 `.mpy`。不要碰 VM / 对象 / 模块（那是 Phase C/D）。
- compile.zig 里有大量 `// Vendor ... (compile.c:...)` 注释——那是它对照
  的 vendor C 源码位置，读不懂某段时顺着注释去 vendor 里找，不要猜。
- 完成后把结果文件写好就停下，等主 agent 验证；不要自己合并不验证。
