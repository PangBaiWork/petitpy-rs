# Value / Obj 接口设计（冻结）

> 状态：冻结（2026-11-17）。Phase C/D 的 lane 按本文编码；改动本文
> 须主 agent 修订并同步所有在途 lane 的 brief。
>
> 依据（只读原项目）：`src/objects/obj.zig`（835）、
> `src/mp_mirrors.zig`（1,082）、`src/memory/gc.zig`（569）、
> `src/runtime/mpstate.zig`（319）、`src/runtime/vm.zig`（1,233 头部）。

## 1. 原项目的值模型（事实）

- `Obj = usize`，REPR_A 标签编码（`mp_mirrors.zig:480-560`，对照
  vendor `py/obj.h:84-102`）：
  - `(o & 1) == 1` → 小整数，值 = `(isize)o >> 1`；
  - `(o & 7) == 2` → qstr 立即数，id = `o >> 3`；
  - `(o & 7) == 6` → 立即数（none=6，false=14，true=30）；
  - `(o & 3) == 0` 且 `o != 0` → 堆对象指针。
  - `OBJ_NULL = 0`，`OBJ_STOP_ITERATION = 0`，`OBJ_SENTINEL = 4`。
  - 小整数窗口 62 位：`SMALL_INT_MAX = 2^62-1`，`SMALL_INT_MIN = -2^62`。
- 每个堆对象以 `MpObjBase { type: *const MpObjType }` 开头
  （`mp_mirrors.zig:27`）；类型也是堆对象（`mp_obj_type_t`）。
- `obj.zig`：`@cImport("py/obj.h","py/runtime.h")` + `extern fn`
  调 vendor C（`mp_unary_op`、`mp_obj_new_int`、`mp_obj_float_get`…）
  + `extern const mp_type_*` 类型 ROM；raise 路径用 `PyError` +
  `pending_exc` 轮询（无 NLR/longjmp，Zig 里已删）。
- GC（`gc.zig`）：vendor `py/gc.c` 的块堆设计，**保守根扫描**
  （扫数据段与影子栈）；公开 API 很小：`allocMaybe` / `allocWith` /
  `reallocMaybe` / `gc_collect` / `gc_collect_root` /
  `gc_collect_start/end`（空操作）；`enterPython` / `leavePython`
  （`python_depth`）；`collect_wanted`、`alloc_count`、
  `allocated_blocks`、`lock_depth`、`finalising`。
- 全局状态 `mp_state_ctx`（`mpstate.zig`）：`mp_state_vm_t` 含
  ROOT POINTER SECTION（GC 根的起点）。

## 2. 核心决策

### D1 — 不移植 `mp_mirrors.zig`

Rust 里没有 vendor C 可供镜像（全部 C 都用 Rust 重写，无 C FFI），
因此 `extern struct` 镜像层没有移植对象。**按其语义定义原生 Rust
值模型**：行为（`isinstance`、hash、相等、`repr`、异常语义、
`gc.collect()` 的计数语义）必须与原版一致；内部表示（指针标签、
结构体布局）不要求与 C 相同——`.mpy` 格式不编码对象布局，
`id()` 之外没有可观测的地址。

### D2 — `Value`（`src/objects/value.rs`）

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Value {
    SmallInt(i64),   // 构造时强制 62 位窗口，超窗走堆上 BigInt
    Bool(bool),
    None,
    Qstr(u32),       // qstr id，与 Zig 立即数同义
    Heap(GcRef),     // 堆对象句柄
}
```

- `Value` 是 `Copy`：立即数按值传；堆对象只传句柄，对象本身活在 arena。
- `Qstr(u32)` 的 id 与 A1 的 qstr 表同号；`Value::Qstr` 即 Zig 的
  `objFromQstr(q)` 立即数。
- 哨兵：`STOP_ITERATION` 与 `NULL` 在原版同为 0；Rust 用
  `Option<Value>`（`None`）表达空，不另设哨兵变体。
  `OBJ_SENTINEL`（内部哨兵）按需由各 lane 在本地定义，不进 `Value`。

### D3 — `GcRef` 与 `GcArena`（`src/memory/gc.rs`）

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GcRef(pub u32);          // arena 槽位索引

pub struct GcArena {
    slots: Vec<Slot>,               // Slot { header: ObjHeader, payload: Obj }
    free: Vec<u32>,                 // 空闲槽位栈
    pub allocated_bytes: usize,     // gc.mem_alloc() 的口径，见 D6
    pub alloc_count: usize,         // 自上次 collect 的分配次数
    pub threshold: i64,             // 触发阈值（默认 -1 = 按原版规则）
    pub lock_depth: usize,          // 分配锁（原版 gc_lock）
}
```

- **不移动对象**（mark-sweep，与原版一致）：`GcRef` 在对象生
  存期内稳定；`id()` 返回槽位索引——原版 C 里释放后的地址也会被
  重用，语义一致。
- 无 `Rc`/`RefCell`（与 A3 同规则）：arena + 句柄；VM 以受控的
  可变借用访问 arena。
- `ObjHeader { type_: GcRef, flags: u8 }`：`type_` 指向 `Obj::Type`
  槽位；`flags` 含 mark 位与 finaliser 位（原版块头的 mark/finaliser
  位的 Rust 形态）。

### D4 — `Obj` 枚举（变体集冻结，payload 归各 lane 所有）

变体（与原版对象种类一一对应，名字即原版类型名）：

```
Float(f64) | Int(BigInt) | Complex { re: f64, im: f64 }
Str(StrData) | Bytes(BytesData)
Tuple(Vec<Value>) | List(ListData) | Dict(DictData) | Set(SetData)
| Array(ArrayData) | Range(RangeData) | Slice(SliceData)
| FunBc(FunBcData) | FunBuiltin(FunBuiltinData) | BoundMeth(BoundMethData)
| Closure(ClosureData) | Generator(GeneratorData) | Cell(CellData)
| Type(TypeData) | Module(ModuleData) | Exception(ExceptionData)
| Property(PropertyData) | Super(SuperData) | Iterator(IteratorData)
| Deque(DequeData) | NamedTuple(NamedTupleData) | AttrTuple(AttrTupleData)
| Map(MapData) | StrIter(...) // 其余迭代器态按 lane 需要增补
```

- **变体名与语义冻结**；每个变体的 payload 结构体归属其 lane
  （如 `DictData` 归 C9），lane 可演进 payload 内部字段，但不得
  改名、删变体、改 `Value`/`GcArena` 上的公开访问签名。
- `Obj` 的 trace（标记子对象）由 payload 所在 lane 实现；
  骨架只实现已有简单变体的 trace，未填 payload 的变体暂按叶子处理，
  填 payload 的 lane 必须同步补 trace（否则 GC 会误回收——
  验收里有针对性的用例）。

### D5 — `Type`（类型即对象）

```rust
pub struct TypeData {
    pub name: u32,                  // qstr id
    pub flags: u16,                 // 原版 mp_obj_type_t.flags
    pub base: Option<GcRef>,        // 基类（Obj::Type）
    pub mro: Vec<GcRef>,            // C3 线性化（Phase C 不展开 MRO 计算细节，用原版顺序）
    pub dict: GcRef,                // 类型字典（Obj::Dict）
    pub unary_op: Option<UnaryOpFn>,
    pub binary_op: Option<BinaryOpFn>,
    pub call: Option<CallFn>,
    pub getiter: Option<GetIterFn>,
    // C6 增补：subscr、buffer、print（其余槽位按 lane 需要增补）
    pub subscr: Option<SubscrFn>,
    pub buffer: Option<BufferFn>,
    pub print: Option<PrintFn>,
}
```

- 方法槽位是 Rust 函数指针（`fn(&mut Vm, Value, ...) -> PyResult<Value>`），
  不是 trait object——与原版 `mp_obj_type_t` 的槽位表一一对应。
- **C6 修正（2026-11-17）：槽位函数带 op 参数。** C0 的
  `UnaryOpFn`/`BinaryOpFn` 没有 op，但原版槽是
  `mp_unary_op(op, arg)` / `mp_binary_op(op, lhs, rhs)`——没有 op
  就无法表达 LEN/BOOL/EQUAL。现为
  `UnaryOpFn = fn(&mut CallCtx, UnaryOp, Value)`,
  `BinaryOpFn = fn(&mut CallCtx, BinaryOp, Value, Value)`，
  `UnaryOp`（12 变体，vendor 判别值 0–11）与 `BinaryOp`
  （52 变体，vendor 判别值 0–51，NOT_IN=50、IS_NOT=51）定义在
  `src/objects/slots.rs`。修正时 grep 确认此前没有任何 lane
  实现过槽，故不破坏任何已有代码。
- 内置类型（int/str/list/…）的 `TypeData` 由各对象 lane 在
  `GcArena` 里构造并登记；`mp_type_roms.zig` 的静态 ROM 表在 Rust
  里改为启动时构造（无 `extern const`）。

### D6 — GC 语义（与原版可观测行为一致）

- mark-sweep；`collect(roots) -> usize` 返回释放的对象数（原版
  `gc_collect()` 的返回值语义；`gc` 模块的 `collect()` 透传它）。
- 触发：**C4 修正（2026-11-17）：不是 `alloc_count >= threshold`
  收集一次，而是压力取模规则**——`stress_every != 0` 且
  `autoCollect()`（lock_depth == 0 且 auto_collect_enabled）
  时，`alloc_count % stress_every == 0` 触发 collect
  （gc.zig:489-493）；`threshold` 字段即 stress 控制
  （`NPY_GC_STRESS` 环境变量设定），`threshold == -1` 时不触发。
  另用单调的 `stress_alloc_count` 保相位（显式 collect 不重置它）。
  vendor py/gc.c 的 `gc_alloc_amount >= gc_alloc_threshold`
  规则没有进 gc.zig，不要按它实现。
- `mem_alloc()` / `mem_free()`：按原版的块记账口径换算为字节
  （原版以块为单位，`MICROPY_MEM_STATS` 语义；C4 lane 落地时与
  原版二进制逐项对数）。
- **根必须是显式的**（D1 的推论）：Rust 没有办法保守扫描原生栈，
  原版 gc.zig 的保守扫描在 Rust 里不存在。根的集合：
  `RootSet { vm_stack: Vec<Value>, globals: GcRef, builtins: GcRef,
  pending_exc: Option<Value>, ... }`——VM 的值栈、全局字典、
  `mpstate` 的根指针区、异常状态。任何持有 `Value` 的 Rust 结构
  若不在 `RootSet` 的遍历范围内，就是漏根（C4 验收用分配压力测试捕捉）。
- `gc_collect_root(ptrs, len)`（原版 API）对应 `RootSet::add_extra`；
  `gc_collect_start/end` 是空操作，照抄。

### D7 — `PyError`（`src/pyerr.rs`，已存在骨架）

- 保持信号枚举（`PyRaise` / `OutOfMemory`）；异常对象本身不进
  `PyError`——它活在 `VmState.pending_exc: Option<Value>`，
  与 Zig 的 `pyerr.pending_exc` 全局量同构，避免 `pyerr` ↔
  `objects::value` 循环依赖。
- 无 NLR/longjmp：错误经 `Result<T, PyError>` 传播（Zig 侧亦然）。

### D8 — 其余冻结项

- 小整数：构造时强制窗口（`Value::new_int`：窗内 → `SmallInt`，
  窗外 → 堆上 `Int(BigInt)`，接 Lane C 的 `vendor::mpz::BigInt`；
  与 `objint.c` 的 small/mpz 分流一致）。
- 字符串：标识符/驻留串用 `Value::Qstr` 立即数；动态串用堆上
  `StrData { bytes: Vec<u8>, ... }`；全配置 `STR_UNICODE=1`
  （A2 的 build option）。
- 浮点：堆对象（REPR_A 下浮点只有堆上一种表示，`obj.zig` 注释）；
  格式化接 Lane FMT 的 `format_float`。
- **`bool` 不是 `int` 的子类（C7 修正，2026-11-17，参考二进制已核实）**：
  `isinstance(True, int)` 为 False，`issubclass(bool, int)` 为
  False，`type(True).__bases__` 是 `(<class 'object'>,)`。int/bool/cell
  的 `TypeData.base` 均为 None，mro 为 [self]。bool→int 只存在于
  算术委托（槽内把 bool 转成小整数 0/1），不在类型层级里。C7 的
  `bool_type_data` 已按此落地（base=None）。不要按 CPython 的
  bool⊂int 假设写代码。

## 3. 冻结的公开 API（lane 直接用的签名）

```rust
// src/objects/value.rs
Value::new_int(i64) -> Value
Value::new_bool(bool) -> Value
Value::none() -> Value
Value::qstr(u32) -> Value
Value::heap(GcRef) -> Value
Value::is_small_int(&self) -> bool
Value::is_qstr(&self) -> bool
Value::is_none(&self) -> bool
Value::is_bool(&self) -> bool
Value::is_heap(&self) -> bool
Value::as_small_int(&self) -> Option<i64>
Value::as_qstr(&self) -> Option<u32>
Value::as_heap(&self) -> Option<GcRef>
Value::is_true(&self, arena: &GcArena) -> PyResult<bool>  // 真值测试（C6 lane 补全）
pub const SMALL_INT_MAX: i64;  // 2^62-1
pub const SMALL_INT_MIN: i64;  // -2^62

// src/memory/gc.rs
GcArena::new() -> GcArena
GcArena::alloc(&mut self, obj: Obj) -> GcRef
GcArena::get(&self, r: GcRef) -> &Obj
GcArena::get_mut(&mut self, r: GcRef) -> &mut Obj
GcArena::collect(&mut self, roots: &RootSet) -> usize
GcArena::mem_alloc(&self) -> usize
GcArena::mem_free(&self) -> usize
pub struct RootSet { pub stack: Vec<Value>, pub extra: Vec<Value> }
impl RootSet { pub fn new() -> Self; pub fn push(&mut self, v: Value); }
```

## 4. 骨架范围（C0 交付物）

- `src/objects/value.rs`：`Value`、`GcRef`、`Obj` 枚举（全部变体，
  payload 先给简单形态/占位，注明归属 lane）、`TypeData` 骨架、
  小整数窗口、表示层测试（标签不变量对照 `mp_mirrors.zig` 的编码表）。
- `src/memory/gc.rs`：`GcArena` + `RootSet` + mark-sweep +
  记账 + GC 测试（分配→丢根→collect 释放数正确；循环引用可回收）。
- `src/lib.rs` 增加 `pub mod memory; pub mod objects;`。
- 本文档 + 提交（master，直接提交，C0 在主线做）。
