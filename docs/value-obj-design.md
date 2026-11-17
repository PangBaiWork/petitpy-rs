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
| Instance(InstanceData) | StaticClassMethod(StaticClassMethodData)
```

- **C12a 增补（2026-11-17）：`Instance` 与 `StaticClassMethod`。**
  `Instance { members: MapData, subobj: Vec<Value> }`（用户类实例，
  `objtype.zig` 的 `mp_obj_instance_t`；`members` 是原始 map 而非
  `DictData`，`subobj` 为 0 或 1 个 native-base 对象）。
  `object()` 的结果也是 `Instance`（空 members、无 subobj）——
  原版为它分配裸 `MpObjBase`，但所有 members 访问都经过类型的
  `attr` 槽，而 `mp_type_object` 没有该槽，故空 map 不可达，两者
  观测等价（已核实：`object().__dict__` 与 `o.x = 1` 均报
  AttributeError）。`StaticClassMethod { fun: Value }` 是
  staticmethod / classmethod 共用的 payload，靠 header type 区分。
  另：namedtuple 实例是**普通 Tuple payload**（header type 为元组
  子类，C11），不再用 `NamedTuple` 变体承载实例。
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
    pub bases: Option<GcRef>,       // C12a：多继承 bases 元组（见下）
    pub mro: Vec<GcRef>,            // C3 线性化（Phase C 不展开 MRO 计算细节，用原版顺序）
    pub dict: GcRef,                // 类型字典（Obj::Dict）
    pub attr: Option<AttrFn>,       // C12a：属性槽（load-only）
    pub make_new: Option<MakeNewFn>, // C12a：构造槽
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
- **C10 修正（2026-11-17）：`CallFn` 带 `n_args`/`n_kw`。** C0 的
  `CallFn = fn(&mut CallCtx, Value, &[Value])` 无法表达原版调用
  协议——vendor `mp_call_fun_t`（`py/obj.h:600`）是
  `call(self, n_args, n_kw, args)`，其中 `args` 为 `n_args` 个
  位置参数后跟 `2 * n_kw` 个 `(name, value)` 关键字对。没有
  `n_kw`，槽无法拆分切片（关键字参数会被误读为位置参数），
  `fun_bc_call`（第一个绑定关键字参数的槽）根本无法移植。现为
  `CallFn = fn(&mut CallCtx, Value, usize, usize, &[Value])`。
  修正时 grep 确认此前没有任何 lane 填过 `call` 槽（只有
  `call: None` 字面量），故不破坏任何已有代码。`BuiltinFn`
  （`fn(&mut CallCtx, &[Value])`）保持不变——位置参数专用，
  C8b/C9b 的方法适配器已按此约定编写；关键字内置函数另有
  `BuiltinKwFn = fn(&mut CallCtx, &[Value], &[(u32, Value)])`
  （位置参数 + 关键字对），由 `FunBuiltinData.kw_func: Option`
  携带（`Some` ⟺ 接受关键字参数），D1 填充。
- **C12a（2026-11-17）：`attr` / `make_new` / `bases` 进 `TypeData`，
  三个 AttrFn 已接线。**
  `attr: Option<AttrFn>`（load-only，
  `AttrFn = fn(&mut CallCtx, Value, u32) -> PyResult<Option<Value>>`）
  已接 `fun_bc_attr` / `bound_meth_attr` / `closure_attr`
  （`objfun.zig:482` / `objboundmeth.zig:57` / `objclosure.zig:61`）。
  注意 `AttrFn` 只管 load——store 路径是独立的自由函数
  （`instance_store_attr` / `type_store_attr`，返回
  `PyResult<bool>`：`Ok(true)`=已处理，`Ok(false)`=未处理），
  因为原版 `attr` 槽的 store 分支用 `dest[0] == NULL` 表示成功，
  单值模型表达不了。`mp_load_method`（C14，`runtime.zig:1188`）
  是读 `attr` 槽的那个函数，不在本 lane。
  `make_new: Option<MakeNewFn>`，
  `MakeNewFn = fn(&mut CallCtx, GcRef, &[Value], usize) ->
  PyResult<Value>`（`ctx, type, args, n_kw`；`args` 为
  `n_args` 个位置参数后跟 `2 * n_kw` 个 `(name, value)` 对，
  与 `CallFn` 同布局）。`bases: Option<GcRef>`：多继承时存
  bases 元组本身（此时 `base` 为 None），单继承时 `base` 存父
  类、`bases` 为 None——`instance_count_native_bases` 要对元组
  递归求和，`mro` 展平后会把共享父类计一次而算错（`class
  C(A,B)` 且 A、B 均继承 int 时 layout conflict 应报 2 次）。
  新增 flags：`FLAG_HAS_SPECIAL_ACCESSORS = 0x0002`、
  `FLAG_ITER_IS_ITERNEXT = 0x0080`、`FLAG_ITER_IS_CUSTOM =
  0x0100`、`FLAG_ITER_IS_STREAM = 0x0180`（`mp_mirrors.zig:676-686`）；
  `TYPE_BASE_FLAGS = 0x021C`。
  **注意**：`is_exact_type(arena, types.type_, v)`（header 检查）
  对 fixture 构造的类型对象恒为 False——fixture 用
  `arena.alloc(None, Obj::Type(..))` 建类型（header 为 None）。
  “这是不是一个类型对象”要用 payload 检查
  `is_type_value(arena, v)`（`matches!(arena.get(r),
  Obj::Type(_))`，`is_subclass` 的 classinfo 检查早已用此形状）；
  两者在 header 齐全时（ROM）一致。`obj_get_type` 对类型值
  同样返回 None（fixture）——`type_make_new` 的 1 参数路径对
  类型值特判为 `types.type_`。
  **`cached_mro` 的纠正**：Zig 的 `cachedMro`
  （`objtype.zig:961-966`）只查 locals dict 里的 `__mro__` 且要求
  它恰为元组，**不**要求父类为元组；Rust 初版多加了
  `parent_is_tuple` 门，断了“单继承后代继承 MI 祖先的缓存 MRO”
  这条链，已去掉。
- **C12a（2026-11-17）：ROM 注册表（`src/objects/objtype/rom.rs`）。**
  `build_rom(ctx) -> Rom { types, issubclass, isinstance }`：以
  ctx 的 types 为底，只替换 objtype 所有的字段（`type_`、
  `object`、`super_`、`staticmethod`、`classmethod`、
  `boundmeth`、`property`、`empty_tuple`、`native_init_wrapper`、
  `empty_bytes`），其余内置类型由各自 lane 的 `*_type_data` 构造
  并传入（C13 负责用 proper header 组装它们）。单例住在
  `BuiltinTypes`（`empty_tuple`、`empty_bytes`）：tuple 哈希种子
  是 `empty_tuple` 的 arena index（`hash(()) == id(())`），
  frozenset 哈希种子是 `frozenset` 类型的 arena index；`new_tuple(0)`
  与 `new_empty_bytes` 返回单例（`bytes()` 与字面量 `b'' is
  b''` 成立；切片仍分配新对象，`b'ab'[:0] is b''` 为 False，
  与原版一致）。`type` 类型自指（header 即自身）；ROM 类型
  `base` 为 None、`mro = [self, object]`（`__bases__` 为
  `(object,)` 是无父类规则的产物，已核实）。`issubclass` /
  `isinstance` 是 `fun_builtin_2` 对象，住在 `Rom` 上（供
  modbuiltins 安装，Phase D）。
  **C12b 重叠（已合流，2026-11-18）**：`objobject.zig`（object
  类型 + 四个 locals）与 `objexcept.zig` 由 C12b 并行移植；合流
  时 `rom.rs` 的占位 `object_type_data` 已删，改用 C12b 的
  `object_type_data` + `object_locals_dict`（四个 locals），
  `make_new` 槽为 `object_make_new_slot`（Zig 的
  `object_make_new`：`(0,0,false)` 校验 + 空白分配）——**不是**
  `instance_make_new`：装了 locals 后 `__new__` 查找会命中
  `object.__new__` 并报 "arg must be user-type"（Zig 的
  `object()` 从不查 `__new__`）。`is_exception_type_obj` 已从
  print 槽身份改回 make_new 槽身份（`exception_make_new_slot`
  适配器，`ExcError → PyError`）；`instance_print` 的异常分支
  已接 `mp_obj_is_native_exception_instance`，`PRINT_EXC_SUBCLASS`
  由 `exception_print_inner` 的 `is_subclass` 参数承载。
  **原版 quirks（已在原版二进制上核实，Rust 如实复现）**：
  `npy_ty_class_lookup` 的父类步进在父类是 `object` 时**停止
  而不查 `object` 的 dict**（`objtype.zig:886-897`，单继承分支
  与 C3 分支皆然）——所以子类实例**看不到** object 的 locals：
  `hasattr(X(), '__init__')` 为 False，`X().__init__` 报
  AttributeError；只有 `object()` 自身的 `__init__`/`__new__`
  可达（其类型 dict 被直接查）。不要“修”它。
  `convert_member_lookup` 的函数判定是 **payload 检查**：
  `FunBuiltin`/`FunBc` 值在全树以 header `None` 分配
  （`fun_builtin_N` 类型对象尚未存在），header 读不到
  `BINDS_SELF` 标志——payload 即身份（`is_type_value` 的先例）：
  内置函数在非实例类型中绑定 self、在实例类型中为
  staticmethod（不绑定），字节码函数恒绑定 self；这使
  `object().__init__` 为 bound method（已核实）。
- **C12a（2026-11-17）：`VmSeams`（局部缝合，C13 替换）。**
  objtype 的逻辑函数以 `&mut VmSeams` 为参数，缝合 VM 尚未提供
  的五处：`call`（值调用，C10 runner 形状）、`load_maybe`
  （`mp_load_method_maybe` 的可观测契约）、`unary` / `binary`
  （`mp_unary_op` / `mp_binary_op`，`Ok(None)` = 未实现）、
  `subscr`（`mp_obj_subscr`，`SubscrError::{KeyError,
  Raise(PyError)}`——`__missing__` 的判定需要错误种类，
  `PyResult` 表达不了）。C13 用真正的 VM 状态机统一 C10a/C10b
  的两个 runner 与此处缝合，届时删掉它。stub 的已知失真已在
  `seams.rs` 注明（unary/binary 不能透传真正的槽 raise；
  stub subscr 永不产生 `KeyError`）。
- **C12a（2026-11-17）：元组 ADD 用 subclass-fast。**
  `objtuple.zig:185-189` 的 Add 臂查
  `is_subclass_fast(rhs_type, tuple)` 而非 exact tuple——
  namedtuple / 元组子类可拼接（`P(1,2)+(3,)`、`(1,2)+P(1,2)`
  均为普通 `(1,2,3)`，已核实）；同时要求 rhs 确有 Tuple
  payload（用户类实例的 items 在 native-base subobj 里，走不到
  这个槽；Zig 在此处 `fromObj` 是 UB，Rust 返回 PyRaise）。
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
