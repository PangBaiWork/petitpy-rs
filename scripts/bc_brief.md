# Lane C5 — runtime bc (runtime/bc.zig → Rust)

## Standing rules (read first, non-negotiable)

`~/workspace/petitpy-rs/docs/agent-rules.md` — 对照原文、不许自创、同一套测试、
纯 Rust std（no external crates）、文件 <800 行、无 unsafe（无理由不许）、
原项目只读（`~/workspace/petitpy-orig`，绝不修改）、每块一提交、不许假完成。
主 agent 会独立验证后才合并；不要自己合并不验证。

## Worktree / branch

- Worktree `~/workspace/petitpy-rs-bc`, branch `lane/bc`.
- Base: master `7faf3d5`（C0 + Lane C1 已合：Value/Obj 接口冻结，
  运行时 qstr 已统一，215 测试全绿）。
- 只改本 lane 的文件。提交时只 `git add` 自己的文件路径，**绝不 `git add -A`**。

## Source (read-only original, petitpy devel HEAD df7de3a)

- `src/runtime/bc.zig` — 461 行，**0 test blocks**。
  imports：std、`mp_mirrors.zig`、`pyerr.zig`。
- 内容：
  - varint 编解码：`export fn mp_encode_uint` / `mp_decode_uint` /
    `mp_decode_uint_value` / `mp_decode_uint_skip`
    （`MP_ENCODE_UINT_MAX_BYTES = 10`）。
  - 帧装配：`export fn mp_setup_code_state` /
    `mp_setup_code_state_helper`（由实参装配 code state：位置参数、
    varargs、kwargs、默认值；用 `MpCodeState`、`MpMap`、`MpObjDict`、
    qstr 表查 arg 名）。
  - 常量：`MP_SCOPE_FLAG_VARARGS / VARKEYWORDS / DEFKWARGS`、
    `MP_MAP_LOOKUP / MP_MAP_LOOKUP_ADD_IF_NOT_FOUND`。

## Target

- `src/runtime/bc.rs`（+ `src/runtime/mod.rs` 加 `pub mod bc;`）。
- 依赖（都已在主线）：C0 冻结的 `Value` / `GcRef` / `GcArena` /
  `Obj`（`docs/value-obj-design.md`——读它，`MpCodeState` 的 Rust
  形态按设计文档的 arena + 句柄方式，不要发明自己的表示）；
  Phase B 的 `compiler::mpy::RawCode`（code 结构）；
  C1 的 `crate::qstr_table`（qstr 查表）与 `crate::runtime::qstr`。
- **不要重复造轮子**：`compiler::emit::encoders` 已有
  encodeUint/encodeInt（emit.zig 侧）。先比对两边的编解码是否同一
  算法：相同就复用（runtime 侧的 decode 是新增的；encode 若相同
  则调用 emit 侧的，不要复制一份）；不同则在结果文件里写明差异。
- `MpCodeState`：原版是 C 结构体（含 ip、code_state 指针等）。Rust
  形态：一个 owned struct，字段按 bc.zig 的实际使用列出（不要照搬
  全部 C 字段——只 port bc.zig 用到的）；code 用 `GcRef` 或 RawCode
  引用按设计文档的方式。

## Acceptance（全部要数，不要形容词）

1. varint：encode/decode 往返单测（边界 0、127、128、2^28、2^56、
   10 字节上限；decode_skip 步进正确；与 emit 侧 encoders 的
   交叉比对——同一值同一字节）。
2. `mp_setup_code_state`：用冻结的 Value/GcArena + 一个最小
   RawCode 搭测试（位置参数、varargs、kwargs、默认值、缺省报错
   路径按 bc.zig 的分支覆盖）。
3. `cargo test --release --lib --tests` 全过（基线 215 保持 + 本 lane
   的新测试）；`cargo build --release` 0 warnings；无 unsafe；
   每文件 <800 行；纯 std，无外部 crate。
4. 原项目 `~/workspace/petitpy-orig` 保持干净（`git status` 空）。
5. 结果文件 `scripts/bc_result.txt`：1:1 映射、与 emit encoders 的
   复用关系、`MpCodeState` 的 Rust 形态、测试数字、commit hash。
6. 一个提交，commit message：`Lane C5: port runtime bc to Rust`。
