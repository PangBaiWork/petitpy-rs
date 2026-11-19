# petitpy-rs 已知分歧 Triage 表

> 只读梳理（2026-11-19，主线 master `8f9aafa`，F2 已合并；
> 2026-11-19 更新：G0（本表称 G1）已合并，主线 `f8ccf00`）。
> 来源：`README.md` "Known divergences" 一节、`scripts/*_result.txt` §4、
> `docs/任务书.md` §3.5。探针用原版二进制
> `~/workspace/petitpy-orig/zig-out/bin/petitpy`（REF）和
> `target/release/petitpy-rs`（RUST）实测，探针脚本在
> `~/workspace/divergence_probe/`（`probe_all.py`、`probe_round2.py`、`r3.py`）。
>
> 证据档：**(a) 已探针验证**——双二进制实测 byte-compare；
> **(b) 源码对照**——贴出原版源码位置 + Rust 侧文件行号；
> **(c) 推测**——明确标注，不作定论。
> 规模：S <100 行 / M 100–400 行 / L >400 行或跨多个子系统。
> "gate" 指现有测试覆盖；"无覆盖" 的项修完必须补探针/测试，不许只凭感觉合。
>
> 基线状态（master f8ccf00，G0 已合并）：full 545/552 GREEN、integration
> 202/202（feature `unsafe-ffi` 开）/ 200/202（默认关，仅 select ×2）、
> golden 585/585、capability 67/67、stdlib 8/8、
> nano 454/541 report-only、lib 1708（feature 开）/ 1700（默认关）+ 3 ignored。3 个 ignored 是：
> parser smoke、`makenew_class_attr_on_class`（C.x）、
> `int_from_bytes_e2e_after_classmethod_wrap`（后两者都是 P5）。

## 总表

| 编号 | 分歧 | 可修？ | 规模 | 依赖 | gate |
| --- | --- | --- | --- | --- | --- |
| H1 | `float()` 不走 `__float__` | 可修 | M | hook 缝合 | 无，需新探针 |
| H2 | dict 查找时 FunBc `__eq__` 不跑 | 可修 | M | hook 缝合（map 探针循环） | 无，需新探针 |
| H3 | `dict.get`/`.pop`/`.setdefault` 实例键 | 可修 | M | H2 同机制，方法拦截 | 无，需新探针 |
| H4 | dict 子类实例键 | 可修 | M | H2 同机制，下一层 | 无，需新探针 |
| H5 | set 字面量/存入实例键 | 可修 | M | hook 缝合（set_lookup） | 无，需新探针 |
| H6 | `list.extend(gen)` | 可修 | S–M | hook 缝合（collect） | 无，需新探针 |
| H7 | `str(KeyError(instance))` 打印 fallback | 可修 | M | 异常参数打印 hook | 无，需新探针 |
| H8 | `type(iter(...))` 裸 TypeError | 可修 | M | 迭代器类型头 | 无，需新探针 |
| H9 | `import_from` 的 FunBc 回退 | 可修 | M | hook 缝合（import） | 无，低价值 |
| H10 | API `ppy_str`/`ppy_repr` 遇 raising `__str__` | 可修 | M | 打印 hook | 需扩展 api_test |
| U1 | select ×2 | 已修（feature 开时 202/202；默认关仍 200/202） | — | unsafe-ffi（G0 lane，已合并 f8ccf00） | integration select ×2 |
| U2 | `os.umask` 仍是 stub | 可修 | S | unsafe-ffi | integration（现有 stub 断言需改） |
| U3 | `os.close` 只关 os.open 登记的 fd（含 stdio 复用） | 可修 | S | unsafe-ffi | 无，需新探针 |
| U4 | `socket.fromfd` 的 close 不关 OS fd | 可修 | S | unsafe-ffi | integration（只断言 fileno） |
| U5 | subprocess 超时 kill 用 SIGKILL 而非 SIGTERM | 可修 | S–M | unsafe-ffi | integration（断言 returncode） |
| U6 | socket 构造器是 /dev/null 占位 fd（bind 换 fd 号等） | 可修 | M | unsafe-ffi | integration（socket 相关） |
| U7 | `getsockopt` 一律 ENOPROTOOPT | 可修 | S–M | unsafe-ffi | 无，需新探针 |
| U8 | subprocess fd 流模式不共享 offset/append，socket fd 失败 | 可修 | M | unsafe-ffi（dup2） | 无，低价值 |
| U9 | stdio 上 `os.fsync`/`os.lseek`（重定向到常规文件时） | 可修 | S | unsafe-ffi | 无，需新探针 |
| P1 | `super()` 构造错误丢 message | 可修 | S | 无（one-line） | 无，需新探针 |
| P2 | CLI 路径 `NPY_GC_STRESS` 失效（`Vm::new` 不调 `gc_init`） | 可修 | S | 无（one-line） | 无，需环境变量探针 |
| P3 | 文件写无缓冲（`/dev/full` 写时即报） | 可修 | M | 无，纯 std | 无，需新探针 |
| P4 | `import os.path` 报错名是 `os.path` 而非 `os_path` | 可修 | S–M | import loader | 无，需新探针 |
| P5 | 类型对象属性查找缺口（`C.x`、`int.from_bytes`） | 可修 | M | 类型 attr slot + loader 切换 | 2 个 ignored 测试 |
| P6 | str_data 动态 qstr 残留站点 | 待审计 | M | 先审计再定 | 部分已验证修复 |
| P7a | CLI 非 UTF-8 文件名直接 panic（rc=-6） | 可修 | S | 无，纯 std | 无，需新探针 |
| P7b | API `ppy_exec` 非 UTF-8 文件名 lossy | 可修 | M | 编译器需走字节名 | 需扩展 api_test |
| P8 | finaliser 回调走 std stderr，不走 SinkGuard | 可修 | M | F1 的 sink | 无，需 API 探针 |
| P9 | stress 路径 finaliser 延迟 | 可修 | M | GC/VM 边界 | 无，低价值 |
| P10 | host 函数 kwargs 被拒 | 不是分歧（原版同样拒） | — | 无 | G2 已验证，测试钉住 |
| P11 | API 上用户实例 `__getitem__` 未测试 | 已验证，无需修 | — | 无 | G2 探针通 |
| P12 | DUP_TOP_TWO 潜在 NULL bug | 已修（G2） | — | 无 | 编译器对 `a[i] += x` 发该指令 |
| P13 | 直接调 `str_join` 短调用 panic | 已由 E2c 修掉 | — | 无 | G2 已验证，无需改动 |
| P14 | 管道 stdout 时原版缓冲输出、Rust 直写 | 可修 | M 待定 | 输出缓冲机制待定位 | 无，需时间戳探针 |
| N1 | 原版崩溃（rc=134）系列 | 不修 | — | — | — |
| N2 | subprocess `input=123` 原版挂起 | 不修 | — | — | — |
| N3 | identity-hash / 地址值 | 不修（设计如此） | — | — | — |
| N4 | `re` 打印 ASLR 指针 | 不修（设计如此） | — | — | — |
| N5 | `\|=` 不走 union 分支 | 不是分歧 | — | — | 已探针一致 |
| N6 | `type('T',...)` 不调 `__init_subclass__` | 不是分歧 | — | — | 已探针一致 |
| N7 | petitpy-vm 的 Zig stack trace | 不修（噪声） | — | — | — |
| N8 | signal handler 从不分发 | 不是分歧 | — | — | 已探针一致 |
| N9 | F1 重入 caveat | 不修（设计） | — | — | — |
| N10 | F1 防御性 NULL 处理 | 不修（有意） | — | — | — |
| N11 | `real_load_maybe` 缺 `__class__`/`__next__` | 暂不修 | — | 无调用方 | — |
| N12 | REF 集合顺序本身不确定 | 不是分歧 | — | — | — |
| N13 | ≥1e9 项生成器 list cap | 不修（不可观测） | — | — | — |
| N14 | 未触发 errno / `unexpected_error_tracing` | 不是分歧 | — | — | 源码引用 |
| N15 | REPL accum 16384 / 20000 字符 ENOMEM | 不是分歧 | — | — | 已探针一致 |
| N16 | `os.access` 近似 | 暂不修 | — | — | 已探针一致 |

另：本次 triage 实测闭合 14 项旧遗留（见文末“已验证闭合”）；nano 评估见文末。

---

## H — Hook/缝合族（VM hook 需穿过 ctx-only 路径）

### H1 — `float()` 不走 `__float__`
- 行为：`float(G(1.5))`（`G.__float__` 返回 9.9）→ REF `9.9`，Rust `TypeError: can't convert to float`。
- 证据 **(a)**：`probe_all.py` `float_dunder`，双二进制实测。出处：F3 §4(b)、README。
- 机制：FLOAT_MAYBE slot 路径走 stub seam，FunBc dunder 跑不起来，于是落到 native 值。修法：像 `hash` 那样对 `float` 内置做 GcRef-identity 拦截（`call_float_ctx`），与 F3 Fix 6 同形。
- 规模 M。依赖：无（纯 Rust 缝合）；风险中——拦截点在 `builtin_vm.rs`，别动 `obj.rs` 的 slot 路径。gate：无覆盖，需新探针 + lib 测试。

### H2 — dict 查找时 FunBc `__eq__` 不跑
- 行为：`d[A()] = "x"; d[A()]`（`A.__hash__` 返回 7，`__eq__` 按类型判等）→ REF `"x"`，Rust `KeyError`。
- 证据 **(a)**：`probe_all.py` `dict_eq`。出处：F3 §4(c)。注意 HASH 一半已修（F3 Fix 4），只剩探针循环里的比较。
- 机制：把 hook 穿过 `compact_lookup` / `compact_rehash` 的探针比较（目前是 ctx-only `obj_equal`）。rehash 回放也要同样处理，否则缩容/扩容后仍丢。
- 规模 M。依赖：H3/H4/H5 同机制，建议同 lane。风险中高——`map.rs` 是全树共用的热路径，改完必须全门重跑。gate：无覆盖，需新探针。

### H3 — `dict.get` / `.pop` / `.setdefault` 实例键
- 行为：同 H2 的键，`d.get(A())` → REF `"x"`，Rust `TypeError: unsupported type for operator`；`d.pop(A(), 'miss')` 同理。
- 证据 **(a)**：`probe_all.py` `dict_get`。出处：F3 §4(e)。
- 机制：`dict_get_helper` 走 FunBuiltin 方法槽，无 hook；需方法拦截（与 H2 的 hash 路径汇合，或给这三个方法加 hook 版 helper）。
- 规模 M。依赖 H2。gate：无覆盖，需新探针。

### H4 — dict 子类实例键
- 行为：`class D(dict)` 的实例用实例键下标时，`try_instance_subscr` SENTINEL → stub subscr → ctx-only dict slot，H2 的缺口再低一层。
- 证据 **(b)**：F3 §4(f) 的源码对照；本次未单独探针（H2 的探针已证同族机制失效，dict 子类路径按代码走同一 ctx-only slot）。
- 机制：随 H2 一起修（stub subscr 也要能拿到 hook）。规模 M（并入 H2，不单独立项计）。gate：无覆盖，需新探针。

### H5 — set 字面量/存入实例键
- 行为：`{A()}` 后 `A() in s` → REF `True`，Rust `TypeError: unsupported type for operator`。
- 证据 **(a)**：`probe_all.py` `set_inst`。出处：E2h §4（`set_lookup` → ctx-only `hash_of`）。
- 机制：与 H2 同形，把 hook 穿过 `set_lookup` / set 存入路径。规模 M。依赖 H2（同 lane）。gate：无覆盖，需新探针。

### H6 — `list.extend(gen)`
- 行为：`l.extend(x for x in [1,2,3])` → REF `[1, 2, 3]`，Rust `TypeError: object not iterable`。
- 证据 **(a)**：`probe_all.py` `list_extend_gen`。出处：E2i §4(c)（与已修的 `array.extend(gen)` 同一 hook 缺口）。
- 机制：list 路径补 `collect_iterable` hook（E2b 修 generator collection 时留下的口子）。规模 S–M。依赖：无。gate：无 corpus 覆盖，需新探针。

### H7 — `str(KeyError(instance))` 打印 fallback
- 行为：`str(KeyError(Color.RED))` → REF `Color.RED`，Rust `<Color object at 30d>`。
- 证据 **(a)**：`probe_all.py` `str_keyerr_inst`。出处：F3 §4(d)。
- 机制：异常参数打印走 ctx-only fallback；需给 exception print slot 加 hook（与 F3 Fix 6 同族但独立机制）。`str(KeyError('k'))` / `(42)` 已一致，只修实例参数一路。规模 M。gate：无覆盖，需新探针。

### H8 — `type(iter(...))` 裸 TypeError
- 行为：`type(iter('ab'))` → REF `<class 'iterator'>`，Rust 裸 `TypeError`（空 message）。
- 证据 **(a)**：`probe_all.py` `type_iter`。出处：E2b §4、E2f §4(b)。
- 机制：GETITEM 迭代器等内部迭代器对象没有类型头；需给迭代器一个真正的类型对象（其余迭代器类型已有头）。规模 M。依赖：无；风险中——`Obj::Iter` 一族的类型头改动面广，先探针确认哪些迭代器类型受影响（list/tuple/gen/iter0 族）。gate：无覆盖（corpus iter0/1/2 不含该行），需新探针。

### H9 — `import_from` 的 FunBc 回退
- 行为：from-import 属性缺失且模块是包时，带 FunBc `__import__` override 仍走 ctx-only `call_function_n_kw`；corpus 从未命中。
- 证据 **(b)**：E2i §4(e) 源码对照（`runtime_import.rs`，`import_from_ctx` 传了真 override dict 但回退分支没用上）。
- 机制：把 hook 穿过 `import_from`。规模 M。价值低（触发条件窄、无 gate），排最后。gate：无。

### H10 — API `ppy_str` / `ppy_repr` 遇 raising `__str__`
- 行为：实例的 `__str__` / `__repr__` 会抛错时，`ppy_str` / `ppy_repr` 走 ctx-only 打印路径；E2d 的 hook 只活在 VM dispatch 里，不在 `obj_print_helper` 里。
- 证据 **(b)**：F1 §4(i) 源码对照。未探针（需 C 侧用例）。
- 机制：把 hook 线程进 `obj_print_helper`，或在 api 层拦截。规模 M。gate：需扩展 `tests/embed/api_test.c`（目前只打印 tuple/list/dict）。

---

## U — unsafe-ffi 族（需 cargo feature `unsafe-ffi`；G1 在立项）

> 前提：`unsafe-ffi` feature 默认关闭时，以下全部保持现状（纯 std、0 unsafe）；
> 开启后才走 FFI 路径。U1（select）已由 G1 lane 在修，本表只记状态，不再拆。

### U2 — `os.umask` 仍是 stub
- 行为：`os.umask(0o022)` → REF 返回旧 mask（实测 `7`），Rust `NotImplementedError: os.umask is not implemented (no std umask API)`。
- 证据 **(a)**：`probe_all.py` `umask`。出处：D2 §4、F5b §4(d)。
- 机制：`umask(2)` extern "C"，返回旧值。规模 S。gate：现有测试断言 stub 抛错，修完需改测试。

### U3 — `os.close` 只关 os.open 登记的 fd
- 行为：`open('/dev/null')` 后 `os.close(f.fileno())` → REF `os.close done`，Rust `OSError: [Errno 9] EBADF`；且 `os.close(1)` 后 REF 的 `os.open` 复用 fd 1（print 写入 /dev/null，无输出），Rust 返回 fd 3。
- 证据 **(a)**：`probe_all.py` `os_close_openfd`、`os_close_stdio`。出处：F6 §4(a)、D2 §4。
- 机制：`close(2)` extern "C"，对未登记 fd 直接调；`os_fds` 表仍保留（已登记 fd 的行为不变）。规模 S。gate：无覆盖，需新探针。

### U4 — `socket.fromfd` 的 close 不关 OS fd
- 行为：`fileno() == fd` 成立，但 `close()` 后原 fd 仍可用；REF 关闭底层 fd。
- 证据 **(b)**：F5b §4(b)（已探针：REF close 后 fd 真关；Rust 不关）。机制：`from_raw_fd`（unsafe）接管所有权。
- 规模 S。gate：integration 只断言 fileno，需补 close 语义探针。

### U5 — subprocess 超时 kill 用 SIGKILL 而非 SIGTERM
- 行为：超时 kill → Rust returncode -9，REF -15；且 REF 有 1s grace 期。
- 证据 **(b)**：D3 §4 源码对照（std `Command::kill` 只发 SIGKILL）。机制：`kill(2)` 发 SIGTERM，等 1s，仍活着再 SIGKILL。规模 S–M。gate：integration 有超时 case（断言 returncode，需核对）。

### U6 — socket 构造器是 /dev/null 占位 fd
- 行为：构造时调不了 `socket()`，先占 /dev/null fd，`bind()` 时才换真 listener（fd 号会变；bind 到 listen 之间来的连接会被接受；`listen(backlog)` 是验证 no-op；`bind("")` 返回 ENOENT 而 REF 绑抽象 socket；fromfd listener 上 listen 得 EINVAL）。
- 证据 **(b)**：D3 §4 源码对照。机制：`socket(2)` + 真 bind/listen，一次性修掉上述 5 个子行为。规模 M。gate：integration socket 相关 case。
- 注意：U6 与 U4/U7 同文件 `modsocket.rs`，同 lane。

### U7 — `getsockopt` 一律 ENOPROTOOPT
- 行为：`s.getsockopt(SOL_SOCKET, SO_PEERCRED, 12)` → REF 返回 12 字节 ucred（未连接时 pid 0、uid/gid -1），Rust `OSError 92`。
- 证据 **(a)**：`r3.py` `getsockopt3`。出处：D3 §4（REF 返回真字节；`peer_cred` 在 stable std 里不稳定）。
- 机制：`getsockopt(2)` extern "C"，按 buflen 返回 bytes。规模 S–M。gate：无覆盖，需新探针。

### U8 — subprocess fd 流模式不共享 offset/append，socket fd 失败
- 行为：fd 流模式是重开 `/proc/self/fd/<fd>`，不共享原 offset/append；socket fd 直接 ENXIO。
- 证据 **(b)**：D3 §4 源码对照。机制：`pre_exec` + `dup2`（unsafe FFI），子进程继承真 fd。规模 M。价值低（冷门路径），排最后。gate：无。

### U9 — stdio 上 `os.fsync` / `os.lseek`（重定向到常规文件时）
- 行为：stdout/stdin 重定向到常规文件时，`os.fsync(1)` → REF ok，Rust `OSError EINVAL`；`os.lseek(0, 0, 0)` → REF `0`，Rust `OSError 29`（ESPIPE）。
- 证据 **(a)**：`r8.py`（stdio 重定向实测；管道 stdout 时双边一致 EINVAL，故只在常规文件时分歧）。出处：D2 §4。
- 机制：`fsync(2)` / `lseek(2)` extern "C" 直通 fd 0/1/2。规模 S。gate：无覆盖，需新探针（重定向场景）。

---

## P — 纯 std 可修族（不需 unsafe）

### P1 — `super()` 构造错误丢 message
- 行为：`super(1, 2)` → REF `TypeError: issubclass() arg 2 must be a class or a tuple of classes`，Rust 裸 `TypeError`（空 message）。
- 证据 **(a)**：`probe_all.py` `super_err1`。出处：F4 §4（`super_make_new` 仍返回裸 `to_pyerror()` 信号，与已修的 MRO bug 同形）。
- 机制：F4 §4 已写明——`record_typed` 一行，`TypedRaise` 已存在。规模 S。gate：无覆盖，需新探针。

### P2 — CLI 路径 `NPY_GC_STRESS` 失效
- 行为：`Vm::new` 从不调 `arena.gc_init()`，`NPY_GC_STRESS` 在 CLI 路径全死；F1 只在 `ppy_new` 里补了。
- 证据 **(b)**：F1 §4(h) 源码对照（`src/memory/gc.rs:414-418` 读 env var，但无调用方）。
- 机制：在 `Vm::new` 或 CLI 入口调一次 `gc_init()`。规模 S。注意：`Vm::new` 也被测试用——env 变量控制，默认行为不变；但要确认 `gc_init` 在测试进程里被反复调无副作用。gate：无，需 `NPY_GC_STRESS=10` 跑 CLI 探针。

### P3 — 文件写无缓冲
- 行为：写 `/dev/full` → REF 写时成功、close 时吞掉 flush 错（`write ok / close ok`），Rust 写时即 `OSError 28`。
- 证据 **(a)**：`probe_all.py` `full_write`。出处：F6 §4(b)。
- 机制：给文件对象加写缓冲，flush 在 close 时（close 吞错与 REF 一致）。规模 M。风险中——缓冲改动影响所有文件写路径，需全门重跑；`os.write` 直通路径不受影响。gate：无覆盖，需新探针。

### P4 — `import os.path` 报错名是 `os.path` 而非 `os_path`
- 行为：`import os.path` → REF `ModuleNotFoundError: No module named 'os_path'`，Rust `No module named 'os.path'; 'os' is not a package`。
- 证据 **(a)**：`probe_all.py` `os_path`。出处：D2 §4（C14 `load_attr_maybe` 的 parent 分支不触发 `__getattr__`，异常类型一致，只差名字）。
- 机制：在 import loader 的 parent 分支把名字按 Zig 的拼接规则处理（`os` + `path` → `os_path`）。规模 S–M。gate：无覆盖，需新探针。

### P5 — 类型对象属性查找缺口（`C.x`、`int.from_bytes`）
- 行为：`C.x`（类属性查到类上）与 `int.from_bytes(...)` 端到端调用失败；是仅剩的 2 个 ignored lib 测试（`makenew_class_attr_on_class`、`int_from_bytes_e2e_after_classmethod_wrap`）。
- 证据 **(b)**：D5 §4 + 两个 ignored 测试的注释（`load_method_maybe_ctx` 只查 `type` 的 dict，不查类型自己的 dict，也不走通用 `TypeData.attr` slot；原版走 `mp_obj_type_attr`）。
- 机制：port `type_attr`，并把 VM dispatch 从本地近似切到 C14 的 loader（`runtime_method.rs` 已有真 `mp_load_method`）。规模 M。风险中高——LOAD_METHOD 是热路径，改完全门重跑。gate：2 个 ignored 测试（修完去 ignore）。

### P6 — str_data 动态 qstr 残留站点（待审计）
- 现状：D5 §4 列了 `objstr_methods_b.rs` 6 处（lower/upper/title/capitalize/zfill）、`methods_c.rs` 1 处、另约 30 处树内站点（str slot、`%`/format 路径）。
- 本次实测：动态串上的 `upper/title/zfill/center` 全部一致（`probe_round2.py` `str_dynamic`），说明 headline case 已被后续 lane 修掉；D5 的清单已过期。
- 任务：lane 先按 D5 清单逐项 grep + 探针，确认哪些残留站点仍会抛 `can't convert to str implicitly`，再修；不要按旧清单直接开工。规模 M（审计后定）。证据 **(a)** 部分 + **(b)**。

### P7a — CLI 非 UTF-8 文件名直接 panic（本次新发现）
- 行为：脚本文件名含 `0xff` 字节时，REF 正常跑（traceback 印 `<stdin>`），Rust `thread 'main' panicked at std/src/env.rs:878`（rc=-6），`std::env::args()` 在非 Unicode 参数上 unwrap。
- 证据 **(a)**：`r3.py` `nonutf8`。这是比 F1(f) 更严重的一级：API 只是 lossy，CLI 直接崩。
- 机制：CLI 改 `args_os()` + lossy 转换。规模 S。gate：无覆盖，需新探针（非 UTF-8 文件名脚本）。

### P7b — API `ppy_exec` 非 UTF-8 文件名 lossy
- 行为：`ppy_exec` 的 filename 经 `from_utf8_lossy`，traceback 里丢字节；Zig 传原始字节当 source 名。
- 证据 **(b)**：F1 §4(f)。机制：编译器 source 名需走字节（`&[u8]`），改动面到编译器入口。规模 M。gate：需扩展 api_test。

### P8 — finaliser 回调走 std stderr，不走 SinkGuard
- 行为：drain 打印 traceback 直接走 std stderr；host 装了 write sink 也收不到。
- 证据 **(b)**：F6 §4（`finaliser.rs` drain 直写 stderr）。机制：把 sink 透传给 drain。规模 M。gate：无，需 API 探针。

### P9 — stress 路径 finaliser 延迟
- 行为：`alloc()` 内的 stress 收集只攒 `pending_finalisers`，无 CallCtx 可跑 `__del__`，拖到下次显式 `gc.collect()`（对象 seed-marked 保活）。
- 证据 **(b)**：F6 §4 源码对照。机制：在 VM 顶层调用返回处等安全点补 drain。规模 M。价值低（只在 `NPY_GC_STRESS` 下可观测，且 CLI 从不 stress 收集——见 P2）。gate：无。

### P10 — host 函数 kwargs 被拒（不是分歧，G2 已验证关闭）
- 行为：Python 侧带 kwargs 调 host 函数 → Rust `argument num/types mismatch`；**原版同样拒**：`mp_arg_check_num_sig`（`argcheck.zig:43`，`n_kw != 0 and !takes_kw` → terse）在 hostCall 之前就拒掉，host sig 为 `(1, maxInt(u16), false)`（`api.zig:713`）。F1 §4(a) “Zig 忽略 kwargs”的说法是错的。
- 证据 **(a)**：G2 探针 + 新增 Rust 测试 `host_call_with_kwargs_is_mismatch` 钉住该一致；未编造 C 入口，`api_test.c` 未动。

### P11 — API 上用户实例 `__getitem__` 未测试（G2 已验证，无需修）
- 现状：`ppy_getitem` / `ppy_setitem` 走 `try_instance_subscr` 的路径存在；G2 新增 Rust 测试 `ppy_getitem_setitem_user_dunders`（用户 `__getitem__` 返回 k*10、`__setitem__` 写 dict）**通过**，路径已存在，无需修。关闭。
- 证据 **(a)**：G2 探针 + 测试。

### P12 — DUP_TOP_TWO 潜在 NULL bug（G2 已修）
- 行为：`vm_dispatch.rs:611`——任一 top slot 为 NULL 时推两个 NULL，而不是像 ROT_TWO 那样原样复制两个 slot。G2 已修（新 `spush_opt`，原样复制两 slot；dispatch 层手造 .mpy 测试，反证通过）。
- 证据 **(b)**：D2 §4 + 代码对照（ROT_TWO 在 626 行已正确处理 NULL）。**纠正**：编译器确实发该指令——`src/compiler/compile/assign.rs:211` 对 `a[i] += x` 发 `dup_top_two`（另 `handlers_control.rs:417`）；NULL 情形仍只对手造 .mpy 可达。原 triage “编译器无调用方”的说法是错的。

### P13 — 直接调 `str_join` 短调用 panic（已由 E2c 修掉，G2 已验证）
- 行为：不经 VM 拦截直接调 `str_join` 会 SIGABRT；用户代码到不了（VM 调用都过 `call_join_ctx` 的 arity 检查），只有 Rust 侧直接调用方能触发。
- 证据 **(b)**：E2f §4(e)。G2 已验证：`str_join_builtin` 走 `builtin_method!` 宏（`args.len() < $min` 即 Err），现存测试 `e2c_short_direct_calls_error_not_panic` 通过，无需改动。

### P14 — 管道 stdout 时原版缓冲输出、Rust 直写（G0 验收新发现）
- 行为：`print('hello'); time.sleep(2); print('bye')`，stdout 接管道时 REF 的 `hello` 在 2.02s 后才到达（与 `bye` 同时），Rust 的 `hello` 在 0.00s 即到。两者最终输出字节相同，只差到达时间。
- 证据 **(a)**：主 agent 2026-11-19 双二进制时间戳探针（G0 验收 §8，`~/workspace/g0_verify_main.log`）。与 C8a “`mp_plat_print` 是 plain `write(1, ...)`”表面冲突——缓冲应在 Zig std io 层或 embed 层，需源码对照定位。
- 机制：待定。先读 `main.zig` / `embed_exec.zig` 的打印路径 + Zig std io writer 是否带缓冲；再决定是给 Rust 输出加缓冲（flush 点：换行？退出？）还是只在 print 路径。规模 M 待定。gate：无覆盖，需新探针（时间戳读管道）。建议并入 G5（P3 文件缓冲同属输出缓冲族）或单独小 lane。

---

## N — 不修 / 不是分歧

### N1 — 原版崩溃（rc=134）系列：复现崩溃是倒退
REF 在这些输入上 abort（Zig panic / 空指针 / memcpy 越界），Rust 合理抛错或返回。逐项核对过源码或探针：
- `struct.pack('<s', 123)`：REF SIGABRT（`modstruct.zig:251` memcpy 越界，实测 rc=-6，证据 **(a)** `r3.py`）；Rust 返回 `b'\x00'`。不修。
- `str.center` 负宽/超大宽、`bytearray(-1)`、`str(1,2,3,4)`、`b'ab'.hex(1)`：REF abort，Rust 抛错/截断（C8b）。不修。
- `(a*)*` in `ure`：REF abort（RE15）。不修。
- `sort(key=len)` 在 int 上：REF abort（NULL key 进比较，`mp_mirrors.zig:745`）；Rust 传播错误（D1 §4）。不修。
- `math.ldexp(1.5, 2**40)`：REF abort（`@intCast` panic）；Rust 饱和（D3 §4）。不修。
- `os.read(<-1)`：REF abort；Rust MemoryError（D2 §4）。不修。
- `print(file=<non-stream>)`：REF abort；Rust OSError（D2 §4）。不修。
- `re.compile(123/None/bytearray)` 等非 str：REF SEGFAULT；Rust TypeError（D5 §4）。不修。
- `struct` `'S'` get 空串 vs REF 在 null 上 abort（D3 §4）。不修。
- `str`/`tuple`/`bytes` 在恰好 16 MiB 处 repeat：REF abort（`mp_seq_multiply` 空指针，`sequence.zig:48`）；Rust MemoryError 或成功（E2i §4(f)）。不修。
- `opt_level(-1)` wrap、`sleep(inf)` 睡 Duration::MAX：REF abort（D4 §4）。不修。
- `type(compile(...))`：REF abort（code 类型指针为空）；Rust 返回真 code 类型（E1 §3.3）。不修。
- `getpwuid` 负/超大 id：REF abort；Rust KeyError（D4 §4）。不修。

### N2 — subprocess `input=123` 原版挂起
REF hang，Rust TypeError（D3 §4）。复现挂起是倒退。不修。

### N3 — identity-hash / 地址值：本身不确定
REF 印 ASLR 原始地址，Rust 印 arena 下标（F3 §4(g)、E2g）。机制正确（每对象稳定、dict 可用）；REF 自己每次运行都变，无确定性测试能依赖。证据 **(a)**：`super_err3` 探针只差地址。不修。

### N4 — `re` 打印 ASLR 指针
同 N3：`<re 0xPTR>` 的指针是 ASLR 随机值，Rust 用 slot 下标（D5 §4）。不修。

### N5 — `|=` 不走 union 分支：不是分歧
vendor 只检查 plain `OR`（`runtime.zig:575-578`）；双边探针一致（`x |= str` 都报 `unsupported types for __or__: 'type', 'type'`）。证据 **(b)** F5a §4。关闭。

### N6 — `type('T', (B,), {})` 不调 `__init_subclass__`：不是分歧
vendor 行为，已探针，测试已 pin（F4 §4）。关闭。

### N7 — petitpy-vm 的 Zig stack trace：噪声
missing-file 报错首行一致（`error: FileNotFound`），其后是 Zig runtime 的地址噪声，无法复现、也不应复现（E1 §3.4、F2 §4）。关闭。

### N8 — signal handler 从不分发：不是分歧
双边都只记录不分发：装 handler 后发 SIGUSR1，双边都被默认动作杀死（rc=-10，证据 **(a)** `probe_round2.py` `signal_dispatch`）。原版自己的 MVP 限制（D4 §4）。关闭。

### N9 — F1 重入 caveat：设计，不修
嵌套 `ppy_*` 调用经原始 interp 指针重入（锁串行化，与 Zig 依赖的保证相同）；改成 UnsafeCell 内部可变性是被 brief 禁止的重构（F1 §4(b)）。关闭。

### N10 — F1 防御性 NULL 处理：有意为之
NULL `py`/out 指针/argv 时 Rust 返回错误或 no-op，Zig 会 fault（F1 §4(g)）。更安全，不修。

### N11 — `real_load_maybe` 缺 `__class__` / `__next__`：暂不修
`mp_load_method_maybe`（`runtime.zig:1199-1210`）的特殊 case，当前无调用方需要（F5a §4）。留待有调用方时再补。

### N12 — REF 集合顺序本身不确定：不是分歧
多元素实例 set 的顺序在 REF 各次运行间交替（ASLR 依赖的实例 hash）；Rust 确定性（E2d、E2i §4(i)）。关闭。

### N13 — ≥1e9 项生成器 list cap：不可观测
需先产出 1e9 项才到 cap 检查（E2i §4(g)）。无实际输入能走到。不修。

### N14 — 未触发 errno / `unexpected_error_tracing`：不是分歧
F2 的表按 Zig 开关逐项源码引用；`unexpected_error_tracing` 在 `std.options` 默认关闭，未观测到（F2 §4）。关闭。

### N15 — REPL accum 16384 / 20000 字符 ENOMEM：不是分歧
accum 经探针证实不可观测（continuation 恒 false）；20000 字符行的 ENOMEM 双边一致（F2 §4）。关闭。

### N16 — `os.access` 近似：暂不修
D2 的实现是 open/mode 位检查；本次探针 `/dev/null` 可读、不存在文件不可读，双边一致（`probe_round2.py` `os_access`）。完整 `access(2)` 语义超出范围，暂不修。

---

## 已验证闭合（本次 triage 实测，原先记在 §4 里的遗留）

以下在旧 result 文件里记为遗留/缺口，本次双二进制实测已一致，不再列入修复表：

| 项 | 出处 | 本次验证 |
| --- | --- | --- |
| getitem-iterator（`for`/`list()` over 仅 `__getitem__` 的类） | E2f §4(a) | `probe_all.py` `getitem_iter` IDENTICAL |
| `builtins.<newname> = v` | E2i §4(d) | `probe_all.py` `builtins_newname`，双边同报 `AttributeError: no attribute 'foo'` |
| `sys.path` 重绑不重指 import 搜索 | D2 §4 | `probe_all.py` `syspath_rebind` IDENTICAL |
| `int(x, base)` 在实例上 | E2h §4 | `probe_all.py` `int_base_inst` IDENTICAL |
| `**kwargs` 动态键 intern | E2 §4(o) | `probe_round2.py` `kwargs_dynamic` IDENTICAL |
| `__main__` 在任何模块加载前 | D4 §4 | `probe_round2.py` `main_import` IDENTICAL |
| `divmod`/`pow`/`sum` 在 int 子类操作数上 | D1 §4 | `probe_round2.py` `plain_divmod` IDENTICAL |
| `__build_class__` 带 base 的 3 参数形 | E2g §4 | `r3.py` `build_class_base` IDENTICAL |
| 数组子类报错类型名 `'X'` vs `'array'` | E2i §4(j) | `probe_all.py` `array_sub_err` IDENTICAL |
| subprocess env 无 `=` 条目 | D3 §4 | 双边都在 frozen `subprocess.py` 的 `run` 里同报 `no attribute 'items'`（list env 到不了子进程） |
| `os.fsync(1)` 管道 stdout | D2 §4 | `probe_all.py` `fsync_stdout` IDENTICAL（仅常规文件重定向时分歧，见 U9） |
| 动态串的 `upper/title/zfill/center` | D5 §4 | `probe_round2.py` `str_dynamic` IDENTICAL（残留站点见 P6） |

---

## Nano 评估（只做评估，不开工）

**结论：给 Rust 加一个 nano 配置是 XL（超大）工作，不建议无明确需求就启动。**

原版 nano 与 full 的差异来源（`build.zig` preset，`port/mpconfigport.h`）：

| 维度 | nano | full | 对 Rust 的含义 |
| --- | --- | --- | --- |
| 模块 | 无 `os`、`time`、`socket`、`select`、`signal`、`pwd/grp`、`subprocess`；`math`/`warning` off（`warning` 全关） | 全开 | 需一个 `nano` cargo feature，条件注册模块（`build_builtins` / `build_d2_modules` / `build_d4_modules` / `build_d5_modules` 分支） |
| 长整型 | native nano 是 `longint_none`：**只有 62-bit small int，没有 BigInt**（`build.zig:283-287`） | MPZ 大整数 | 最深的分叉。`Obj::Int(BigInt)`、`objint.rs`、解析器大整数、格式化、hash 全部要按配置走两条路；C0 冻结的 Value 模型要开洞。回归风险最高。 |
| frozen | frozen-tiny bundle | 完整 frozen（23 模块） | 另建一个 tiny bundle + `build.rs` 分支 |
| 其它门控 | 若干 `is_full` 门控行为（如 reverse op 的门控，`build.zig:332`） | 全开 | 零散的条件分支，需逐项对表 |
| ROM_LEVEL | MINIMUM（两边相同，D4 已注） | MINIMUM | 不是差异 |

工作量估计：光模块门控 + tiny bundle 约 M；small-int-only 整型模式是 L 且碰 C0 冻结设计；另需一套 nano 验收（`run_nano.py` 已有，但要把 Rust nano 二进制接进去，验收数是 541/541 byte-exact）；合计 XL，数个 lane、高回归风险。

建议：除非有“出一个极小体积二进制”的明确需求，否则不做。nano 454/541 report-only 的含义是“full 构建跑 nano 期望”——它量的是配置差异，不是 Rust 的缺陷，不应解读为 87 个待修 bug。

---

## 建议 lane 切分

分组原则：同组内的项尽量不碰同一文件（可并行）；同文件族的项收进同一 lane（串行做，避免 rebase 冲突）。G0（本表称 G1；unsafe-ffi feature + select）已合并（f8ccf00）。

- **G2 — 纯 std 快胜（S 为主）**：P1（`typecreate.rs`）、P2（`vm.rs` 或 CLI 入口）、P7a（`main.rs`）、P10（`api.rs`）、P12（`vm_dispatch.rs`）、P13（`objstr` join 路径）。文件基本不重叠；P11（先写 api_test 探针）可顺带放在本组。验收：各探针 + 全门。
- **G3 — 容器 hook 族（H2/H3/H4/H5/H6）**：`map.rs`、`objdict.rs`、`objlist.rs`、set 路径。同一文件族，**必须一个 lane 串行做**，不可拆并行。验收：H2–H6 探针 + 全门（`map.rs` 是热路径，全门必重跑）。
- **G4 — VM/dispatch hook 族（H1/H7/H8/H10）**：`builtin_vm.rs`（float 拦截）、异常打印（H7）、迭代器类型头（H8）、`obj_print_helper`/api（H10）。文件分散但同属“hook 缝合”机制，可一个 lane。P5（类型属性查找，`objtype` + LOAD_METHOD）若与 H8 的迭代器类型头抢 `objtype` 文件，排后一组。
- **G5 — 纯 std 中型（P3/P4/P5/P6/P8/P9）**：P3 文件缓冲（`objfileio.rs`）、P4 import 命名（`builtinimport.rs`）、P5 类型属性（`objtype` + dispatch）、P6 str_data 审计（`objstr_methods_*`）、P8/P9 finaliser（`finaliser.rs`/`gc.rs`）。文件不重叠，一个 lane 可做；P6 先审计后动手。
- **G6a — unsafe-ffi 之 os（U2/U3/U9）**：`modos.rs`。依赖 G1 合并。
- **G6b — unsafe-ffi 之 socket/subprocess（U4/U5/U6/U7/U8）**：`modsocket.rs` + `modsubprocess.rs`。依赖 G1 合并；与 G6a 文件不重叠，可与 G6a 并行。
- **H9（import_from）与 P7b（API 文件名）**：价值低/改动面大，排最后；H9 可并入 G4（同 import 区），P7b 并入 G2 的 api 侧或单独留待。

顺序建议：G2 → G3 → G4 → G5（纯 std，无 feature 依赖，可立即排）；G6a/G6b 可排（G0 已合）。H9、P7b、U8、P9、P12、P13 是填空项，哪个 lane 顺手就带，不单独开 lane。P14（stdout 缓冲）建议并入 G5 或单独小 lane。
