# Brief — socket 完整实现，FFI，`unsafe-ffi` 特性开启时生效

用户指令（2026-11-20 ~17:20，verbatim）：“完整实现socket使用ffi，当开启标志时实现”。
前置共识（2026-11-20 ~17:09–17:15）：不必逐字节仿 CPython，API 功能相同即可；
Rust std 只有正向 DNS（`ToSocketAddrs`），无反向 DNS；RustPython 用 `socket2` +
`dns-lookup` + `gethostname` + `nix` + `libc`，且 fromfd 用 unsafe，三个 crate 均
为纯 Rust、无需 C 编译器。本 lane 不引入任何外部 crate，沿用本项目已有的
`src/support/ffi.rs` 模式（raw `extern "C"` 声明，0 依赖），把 FFI 直接写出来。

## 0. 例外说明（用户指令覆盖 agent-rules.md 第 3 条）

- agent-rules.md 规定“纯 Rust std、无外部 crate、无新增 unsafe”，但用户已明确
  下令 socket 用 FFI、且仅在 `unsafe-ffi` 特性开启时实现。这是用户指令对该
  条的**定向例外**，与 Phase G 的 `unsafe-ffi` 例外同性质：unsafe 只允许在
  FFI 边界（`src/support/ffi.rs` / `ffi_dns.rs` 的 wrapper 内 + fromfd 的
  `FromRawFd` 一处），每个 unsafe 块必须有 invariant 注释；特性关闭时
  （默认构建）行为与当前纯 std 实现**逐字节一致**，不许有任何回归。
- 验收标准仍是“同一套测试”（agent-rules.md 第 2 条）：原始测试套件（corpus /
  integration / golden / capability / stdlib / nano）+ 新增的 socket 测试。
  对照原项目（第 1 条）对 socket 不适用：Phase H 已定，socket/select/
  subprocess 以 **CPython API 语义**为准，不必与 petitpy 内部原理一致；
  功能相同即可，不必逐字节仿 CPython 的错误文案/边缘行为。

## 1. 设计（冻结，三个 lane 共用）

### 1.1 分层

```
CPython socket API (modsocket.rs, 已有的方法/模块函数，行为入口不变)
        │
        │  特性关闭 (默认): 走现有纯 std SocketState (Unbound 占位符等)，不动
        │  特性开启: 走 FFI 后端
        ▼
SocketState::Ffi(FfiSocket)          ← 新增变体，cfg(feature = "unsafe-ffi")
        │  src/modules/modsocket_ffi.rs (新文件)
        ▼
ffi.rs / ffi_dns.rs 的 safe wrapper  ← 每个返回 Result<_, i32> (errno)
        │  raw extern "C" 声明 (私有 mod ffi)，0 依赖，不引入 libc crate
        ▼
libc / Winsock (系统库，链接时自动找，不编译 C 代码)
```

### 1.2 关键决策

- **真实 fd 从构造起就存在**：特性开启时 `socket()` 直接调 `socket(2)` 拿
  真实 fd，不再用 `UdpSocket::bind("127.0.0.1:0")` 占位符；`fileno()` 在
  整个生命周期内稳定，不再于 bind/connect 时变化。这是 FFI 后端相对纯
  std 的最大行为改进。
- **错误约定**：ffi wrapper 一律返回 `Result<T, i32>`，`Err` 是 errno
  （Unix）或 Winsock 错误码（Windows，见 1.3）。modsocket 层把 errno
  转成 OSError：复用现有 `io_error_name` / `os_err` / `raise_os_error`
  的映射（`src/runtime/modos.rs:78` 的 `win32_to_errno` 已有 Windows
  码→POSIX errno 映射，复用它；不要另造一套）。
- **全局表**：复用现有 `sockets()` 全局表（`Mutex<HashMap<TableKey,
  SocketState>>`），`TableKey = i64` 不变。FFI socket 的 key 就是它的
  原始 fd（Unix i32；Windows SOCKET 为 usize，截成 i64——SOCKET 句柄值
  实际 < 2^63，与现有 Windows `AsRawSocket` 路径一致）。
- **文件行数**：modsocket.rs 已 2463 行，接近 2500 上限。FFI 后端必须
  放在新文件 `src/modules/modsocket_ffi.rs`；modsocket.rs 只加
  `SocketState::Ffi` 变体 + 各方法的 FFI 分支（每方法一个 arm，委托
  给 FfiSocket）。ffi.rs 的 socket 部分与 DNS 部分分两个文件，避免
  并行 lane 改同一文件冲突（见 §3）。
- **超时/非阻塞**：用 `SO_RCVTIMEO` / `SO_SNDTIMEO`（setsockopt）
  + `fcntl(F_GETFL/F_SETFL, O_NONBLOCK)`（Unix）实现；Windows 用
  `ioctlsocket(FIONBIO)`。不要用 std 的 `set_read_timeout`（FFI
  后端不持有 std 对象）。

### 1.3 平台（Windows / macOS / Android 必须能编译）

- **errno 读取必须按平台 cfg**，不许只写 Linux 的 `__errno_location`：
  Linux `__errno_location()`，macOS `__error()`，Android `__errno()`，
  Windows `WSAGetLastError()`。在 ffi.rs 里写一个 cfg 分支的 `errno()`。
- **Windows 必须先 WSAStartup**：Winsock 函数调用前必须
  `WSAStartup(MAKEWORD(2,2))`，用 `OnceLock` 保证只调一次，失败时
  返回对应错误。链接用 `#[link(name = "ws2_32")]`（extern 块上）。
  Windows 的 SOCKET 是 `usize`（u64），`INVALID_SOCKET = usize::MAX`；
  关闭用 `closesocket` 不是 `close`；`SO_*` 等常量值与 Unix 不同，
  必须用 Winsock 的值（对照 CPython `socketmodule.c` / Winsock 头文件，
  不要抄 Linux 值）。
- **macOS**：`socketpair`、`getaddrinfo`、`getnameinfo`、`gethostname`
  都有；`accept4` 没有（用 `accept` + 事后 fcntl 设 CLOEXEC/NONBLOCK）；
  `SO_REUSEPORT` 存在但语义与 Linux 不同，按 macOS 语义。
- **Android（bionic）**：socket 函数都有；errno 是 `__errno()`；
  只要求**编译通过**（CI 只跑测试，Android 行跑 `cargo test` 时
  socket 测试的网络部分可能受沙箱限制，见 §5）。

### 1.4 不做的事（明确边界）

- 不引入 `socket2` / `dns-lookup` / `gethostname` / `libc` / `nix`
  任何外部 crate（0 依赖不变）。
- 不动 select/subprocess（select on socket 的 readiness 提升见 S3，
  只做 socket 侧的配合，不重写 modselect.rs 的整体结构）。
- 不动默认（特性关闭）构建的任何行为；不碰 `src/support/readiness.rs`
  的核心逻辑（S3 的 socket monitor 除外，见 §3 S3）。

## 2. 冻结接口（S1 实现，S2/S3 按此写代码；S1 合并后如有改动，主 agent
##    会 rebase 并通知）

### 2.1 `src/support/ffi.rs`（S1 负责，socket 系统调用部分）

在现有 `mod ffi` 内增加（Unix/Windows cfg 分支，Windows 块加
`#[link(name = "ws2_32")]`）。每个 wrapper 都是 safe fn，返回
`Result<_, i32>`，失败时立即读 errno/WSA 错误码；每个 unsafe 块有
invariant 注释。函数清单（签名用 Rust 侧类型，内部转 C 类型）：

- `sys_socket(domain: i32, ty: i32, proto: i32) -> Result<RawFd, i32>`
- `sys_bind(fd, addr: *const sockaddr, len: u32) -> Result<(), i32>`
- `sys_listen(fd, backlog: i32) -> Result<(), i32>`
- `sys_accept(fd, addr: *mut sockaddr, len: *mut u32) -> Result<RawFd, i32>`
- `sys_connect(fd, addr: *const sockaddr, len: u32) -> Result<(), i32>`
- `sys_send(fd, buf: *const u8, len: usize, flags: i32) -> Result<usize, i32>`
- `sys_recv(fd, buf: *mut u8, len: usize, flags: i32) -> Result<usize, i32>`
- `sys_sendto(fd, buf, len, flags, addr: *const sockaddr, addrlen: u32) -> Result<usize, i32>`
- `sys_recvfrom(fd, buf, len, flags, addr: *mut sockaddr, addrlen: *mut u32) -> Result<usize, i32>`
- `sys_sendmsg(fd, msg: *const msghdr, flags: i32) -> Result<usize, i32>`（S3 用，S1 先声明）
- `sys_recvmsg(fd, msg: *mut msghdr, flags: i32) -> Result<usize, i32>`（S3 用，S1 先声明）
- `sys_shutdown(fd, how: i32) -> Result<(), i32>`
- `sys_getsockname(fd, addr: *mut sockaddr, len: *mut u32) -> Result<(), i32>`
- `sys_getpeername(fd, addr: *mut sockaddr, len: *mut u32) -> Result<(), i32>`
- `sys_setsockopt(fd, level: i32, opt: i32, val: *const u8, len: u32) -> Result<(), i32>`
- `sys_getsockopt(fd, level: i32, opt: i32, val: *mut u8, len: *mut u32) -> Result<(), i32>`
- `sys_close(fd) / sys_dup(fd)`（close 已有，复用；dup 新增）
- `sys_fcntl(fd, cmd: i32, arg: i32) -> Result<i32, i32>`（Unix；Windows 用 ioctlsocket，见 S3）
- `sys_socketpair(domain: i32, ty: i32, proto: i32, fds: *mut [RawFd; 2]) -> Result<(), i32>`（Unix；Windows 见 S3）
- `sys_poll` 或 `sys_select`（S1 为 connect_timeout 选其一，写清选择；S3 的 select-on-socket 复用）

结构体（`#[repr(C)]`，放 ffi.rs 或 modsocket_ffi.rs，S1 定）：
`sockaddr`（通用，含 sa_family + 14 字节 data）、`sockaddr_in`、
`sockaddr_in6`、`sockaddr_un`（Unix；Windows 无 AF_UNIX 路径时
`#[cfg(unix)]`）、`sockaddr_storage`（足够大，对齐正确）、`addrinfo`
（S2 用，S1 先定义结构）、`msghdr` + `iovec`（S3 用，S1 先定义结构）。
常量（cfg 分支，Unix 值与 Windows Winsock 值分别写，S1 定）：
`AF_UNSPEC/UNIX/INET/INET6`、`SOCK_STREAM/DGRAM/RAW`、`IPPROTO_IP/TCP/UDP`、
`SOL_SOCKET`、`SO_REUSEADDR/BROADCAST/SNDBUF/RCVBUF/RCVTIMEO/SNDTIMEO/
REUSEPORT/KEEPALIVE`、`TCP_NODELAY`、`IP_TTL`、`SHUT_RD/WR/RDWR`、
`MSG_*`、`O_NONBLOCK`、`F_GETFL/F_SETFL`、`EINTR/EAGAIN/EWOULDBLOCK`。
（现有 modsocket.rs 的常量是 Linux 硬编码值；FFI 路径必须用 cfg 正确的值，
不要复用 Linux 值到 Windows/macOS。）

### 2.2 `src/support/ffi_dns.rs`（S2 负责，新文件，DNS/服务部分）

与 ffi.rs 同模式（safe wrapper + `Result<_, i32>`，invariant 注释）：

- `sys_getaddrinfo(node: *const c_char, service: *const c_char, hints: *const addrinfo, res: *mut *mut addrinfo) -> i32`（返回 gai 错误码，非 errno；成功 0；调用方用 `sys_freeaddrinfo` 释放；`getaddrinfo` 不是线程安全的说法是错的——它是线程安全的，但返回链表必须 free）
- `sys_freeaddrinfo(res: *mut addrinfo)`
- `sys_getnameinfo(sa: *const sockaddr, salen: u32, host: *mut c_char, hostlen: u32, serv: *mut c_char, servlen: u32, flags: i32) -> i32`
- `sys_gethostname(name: *mut c_char, len: usize) -> Result<(), i32>`
- `sys_getservbyname(name: *const c_char, proto: *const c_char) -> *mut servent`（返回指针可空；读 `s_port`（网络字节序）+ `s_name`；不许长期持有指针）
- `sys_getservbyport(port: i32, proto: *const c_char) -> *mut servent`
- `sys_inet_pton(af: i32, src: *const c_char, dst: *mut u8) -> Result<(), i32>`
- `sys_inet_ntop(af: i32, src: *const u8, dst: *mut c_char, size: u32) -> Result<(), i32>`
- `sys_inet_aton(cp: *const c_char, inp: *mut u32) -> Result<(), i32>`（Unix；Windows 无 inet_aton，用 inet_pton 拼）
- `sys_inet_ntoa(addr: u32) -> *const c_char`（返回静态缓冲区指针，立即复制，不许持有）
- `htonl/htons/ntohl/ntohs`：**不要调 libc**，它们是宏/内联；用纯 Rust
  `u32::to_be` / `u16::to_be` 等实现（S2 做）。

### 2.3 `src/modules/modsocket_ffi.rs`（S1 建文件并实现核心方法）

```rust
// 全文件 #[cfg(feature = "unsafe-ffi")]
pub struct FfiSocket {
    pub fd: RawFd,        // Unix i32；Windows usize (SOCKET)
    pub family: i32,
    pub kind: i32,
    pub proto: i32,
    pub blocking: bool,   // shadow：std 没有 getter，沿用现有 SockSettings 模式
    pub read_timeout: Option<Duration>,
    pub write_timeout: Option<Duration>,
}
impl FfiSocket {
    pub fn new(family: i32, kind: i32, proto: i32) -> Result<Self, i32>;
    pub fn from_raw(fd: RawFd, family: i32, kind: i32, proto: i32) -> Self; // fromfd 用 (S3)
    pub fn fileno(&self) -> i64;
    pub fn bind(&mut self, addr: &SockAddr) -> Result<(), i32>;
    pub fn listen(&mut self, backlog: i32) -> Result<(), i32>;
    pub fn accept(&mut self) -> Result<(Self, SockAddr), i32>;
    pub fn connect(&mut self, addr: &SockAddr) -> Result<(), i32>;
    pub fn connect_timeout(&mut self, addr: &SockAddr, timeout: Duration) -> Result<(), i32>;
    pub fn send(&self, buf: &[u8], flags: i32) -> Result<usize, i32>;
    pub fn recv(&self, buf: &mut [u8], flags: i32) -> Result<usize, i32>;
    pub fn sendto(&self, buf: &[u8], flags: i32, addr: &SockAddr) -> Result<usize, i32>;
    pub fn recvfrom(&self, buf: &mut [u8], flags: i32) -> Result<(usize, SockAddr), i32>;
    pub fn shutdown(&self, how: i32) -> Result<(), i32>;
    pub fn getsockname(&self) -> Result<SockAddr, i32>;
    pub fn getpeername(&self) -> Result<SockAddr, i32>;
    pub fn setsockopt(&self, level: i32, opt: i32, val: &[u8]) -> Result<(), i32>;
    pub fn getsockopt(&self, level: i32, opt: i32, buf: &mut [u8]) -> Result<usize, i32>;
    pub fn setblocking(&mut self, blocking: bool) -> Result<(), i32>;
    pub fn settimeout(&mut self, r: Option<Duration>, w: Option<Duration>) -> Result<(), i32>;
    pub fn close(self) -> Result<(), i32>;
    pub fn dup(&self) -> Result<Self, i32>;
    // S3 增加：sendmsg / recvmsg / detach / makefile 配合
}
```

`SockAddr` 是本文件的内部地址类型（INET/INET6/UNIX 三种，含与
`sockaddr_*` 的互转；S2 的 getaddrinfo 返回的地址也转成它）。
`FfiSocket` 的方法一律返回 `Result<_, i32>`（errno），不直接抛
Python 异常；modsocket.rs 层负责转 OSError。

### 2.4 modsocket.rs 的接入（S1 负责核心接入，S2/S3 各接自己的部分）

- 加 `#[cfg(feature = "unsafe-ffi")] SocketState::Ffi(FfiSocket)` 变体。
- `socket()` 构造：特性开启时调 `FfiSocket::new`（真实 fd，无占位符）；
  关闭时走现有占位符路径（不动）。
- 每个现有方法（bind/listen/accept/connect/recv/send/sendall/sendto/
  recvfrom/setblocking/settimeout/gettimeout/getsockopt/setsockopt/
  fileno/close/dup/makefile/getpeername/getsockname/shutdown）在
  `match` 里加 `SocketState::Ffi(f) => ...` arm，委托给 FfiSocket。
  **不许改现有纯 std arm 的行为**。
- `SockSettings`（shadow state）复用现有结构；FFI socket 的 key
  不再迁移（fd 稳定），但 `dup()` 产生新 key 时要迁移 settings（沿用
  现有迁移逻辑）。

## 3. Lane 分工（3 并行，各自独立 worktree + 分支）

### S1 — FFI 核心 + socket 对象（最重，先合）

- ffi.rs：§2.1 全部（socket 系统调用 + sockaddr 结构 + 常量 +
  可移植 errno；`sys_sendmsg`/`sys_recvmsg`/`msghdr`/`iovec`/
  `addrinfo` 结构先声明，供 S3/S2 用）。
- 新建 `src/modules/modsocket_ffi.rs`：`SockAddr` + `FfiSocket` +
  §2.3 除 sendmsg/recvmsg 外的全部方法 + `connect_timeout`。
- modsocket.rs：`SocketState::Ffi` 变体 + `socket()` 构造分流 +
  核心方法（bind/listen/accept/connect/recv/send/sendall/sendto/
  recvfrom/setblocking/settimeout/gettimeout/getsockopt/setsockopt/
  fileno/close/dup/getpeername/getsockname/shutdown）的 FFI arm。
- 测试：新增 `tests/sockffi_core.rs`（`#![cfg(feature =
  "unsafe-ffi")]`），覆盖：创建即真实 fd（fileno 在 bind 前后
  不变）、TCP bind/listen/accept/connect 往返、send/recv、
  UDP sendto/recvfrom、setsockopt/getsockopt（SO_REUSEADDR、
  TCP_NODELAY、SO_RCVTIMEO）、非阻塞 connect 超时、shutdown、
  getpeername/getsockname、dup。每个测试都要有对应的纯 std
  行为对照（默认构建下同一测试文件不编译；用 feature 门控）。
- 验收：特性构建 `cargo test --release --features unsafe-ffi
  --test sockffi_core` 全绿；默认构建全门不变（§5）。

### S2 — DNS / 服务 / 地址工具 + 高层构造

- 新建 `src/support/ffi_dns.rs`：§2.2 全部（getaddrinfo/
  getnameinfo/gethostname/getservbyname/getservbyport/inet_pton/
  inet_ntop/inet_aton/inet_ntoa；hton* 用纯 Rust）。
- modsocket.rs（模块函数，特性开启时走 FFI，关闭时保持现有行为）：
  `getaddrinfo`（调 getaddrinfo，返回 (family, type, proto, canonname,
  sockaddr) 五元组）、`gethostbyname`（getaddrinfo 取首个）、
  `gethostbyname_ex`（gethostbyname + getnameinfo 反查）、
  `gethostbyaddr` / `getnameinfo`（getnameinfo 反向）、
  `gethostname`（gethostname(2)）、`getfqdn`（gethostname +
  gethostbyaddr 组合；解析失败时回退到 hostname 本身，按 CPython
  语义）、`getservbyname` / `getservbyport`（libc；查不到时
  OSError，与现有 9 条硬编码表合并——**FFI 优先，查不到再查内置表，
  最后才报错**，不要删内置表）、`create_connection`（getaddrinfo +
  逐个 connect，带 timeout）、`create_server`（getaddrinfo + bind +
  listen + setblocking(False) 按 CPython 默认）、
  `getdefaulttimeout` / `setdefaulttimeout`（模块级状态，复用现有
  模式）、`socketpair`（Unix `socketpair(2)`；Windows 用 loopback
  TCP pair，与 CPython Windows 行为一致）、`htonl/htons/ntohl/ntohs`、
  `inet_aton/inet_ntoa/inet_pton/inet_ntop`。
- 注意：`getaddrinfo` 返回的 canonname 可能为空；`getnameinfo` 的
  flags 按 CPython 透传（NI_NUMERICHOST 等）；`getservbyname` 的
  `servent` 指针立即复制，不许持有；`getaddrinfo` 的链表必须
  `freeaddrinfo`（用 Drop guard，不要漏）。
- 测试：新增 `tests/sockffi_dns.rs`（`#![cfg(feature =
  "unsafe-ffi")]`）：localhost 正向/反向、gethostname 非空、
  getservbyname("http")=80、inet_pton/ntop 往返、htonl 往返、
  socketpair 往返、create_connection 连本地 listener、
  create_server 可 accept。**不依赖外网**（沙箱无外网；只用
  localhost / 127.0.0.1 / ::1）。
- 验收：特性构建该测试文件全绿；默认构建全门不变（§5）。

### S3 — 高级 + 平台 + select 配合 + 总验证

- FFI 高级：`sendmsg` / `recvmsg`（msghdr + iovec + ancillary
  数据先只支持普通 buffer，控制消息（SCM_RIGHTS 等）明确报
  NotImplementedError，不许静默丢）、`fromfd`（`FromRawFd` /
  `FromRawSocket`，unsafe 一处，SAFETY 注释；fd 必须有效，无效时
  EBADF→OSError）、`detach`（`IntoRawFd::into_raw_fd`，safe，
  从全局表移除但不关闭）、`makefile` 的 FFI 配合（用
  `File::from_raw_fd` 包 fd；注意不要 double-close：从全局表
  移除后 fd 归属转给 file 对象）。
- AF_UNIX（FFI）：Unix 上 `sockaddr_un` 的 bind/connect/listen/
  accept/send/recv 全套；Windows 上 AF_UNIX：先探 Winsock 是否
  支持（Win10 1803+ 有 `AF_UNIX`），支持则走 FFI，不支持则保持
  现有 NotImplementedError，并在结果文件写明。
- Windows 平台收尾：WSAStartup（OnceLock）、`closesocket`、
  `ioctlsocket(FIONBIO)`、Windows `socketpair`（loopback TCP，
  与 S2 的 socketpair 配合——S2 做通用版，S3 做 Windows 的
  Winsock 细节修正）、SOCKET/usize 与 TableKey i64 的转换、
  WSA 错误码→errno 映射复用 `win32_to_errno`。
- select 配合（H-select §4(1) 的 socket 部分）：给
  `modsocket::socket_probe` 提供真实的 readiness 判断——FFI
  socket 用 `poll`/`select` 探 readable（替代现在的“永不 ready”），
  或在 `readiness.rs` 的 socket monitor 里用 `recv(MSG_PEEK)`
  排空判断；**只做 socket 侧**，不重写 modselect.rs；做完后
  `h_select` 的 socket 相关探针要能通过（之前永不 ready 的用例）。
  如时间不够，select 配合可降级为“探针仍永不 ready，但写明原因”，
  不许假装通过。
- 总验证：S1+S2 合并后，跑完整验收（§5），并写
  `scripts/sockffi_result.txt` 汇总三个 lane 的结果。

## 4. 工作方式（沿用 lane 规则）

- 每 lane 独立 worktree + 分支；每块一提交；不许 `git add -A`
 （只提交自己的文件）；原项目只读；小步提交（一个文件一测）。
- 写代码前先读 `src/modules/modsocket.rs`（2463 行）、
  `src/support/ffi.rs`、`src/support/readiness.rs`、
  `src/runtime/modos.rs:78`（win32_to_errno）、
  `src/entry/mod.rs:44`（io_error_name），以及 CPython
  `socketmodule.c` 中对应函数的行为（功能相同即可，不必逐行仿）。
- **不要等主 agent**：brief 已冻结接口；S2/S3 按 §2.3 的 FfiSocket
  签名写代码（S1 的实现未合入时，先按签名写，本地用 stub 顶替
  编译验证逻辑；合并时主 agent 会 rebase）。
- 结果文件：每 lane 写 `scripts/<lane>_result.txt`，含 §4 格式
  （改了哪些文件、测试数、与基线的 diff、遗留问题）。

## 5. 验收（每 lane + 最终，主 agent 独立重跑）

- **默认构建（特性关闭）必须零回归**：`cargo test --release
  --lib` 1771/0/2；`cargo test --release --tests` entry_cli 等；
  full 545/552、integration 201/202、golden 585/585、capability
  67/67、stdlib 8/8、nano 454/541（report-only）。即 FFI 代码
  不得影响默认构建。
- **特性构建（`--features unsafe-ffi`）**：lib 1761/10/2 基线
  不得新增失败（debug 的 10 个是 debug_assert 基线）；新增的
  `sockffi_core` / `sockffi_dns` 测试文件全绿；full 545/552、
  integration **202/202**（S3 的 fromfd 实现后，之前因 fromfd
  丢掉的 `socket_fromfd_wraps_fd` 应通过；若未通过，写明原因，
  不许改测试来凑数）、golden 585/585、capability 67/67、
  stdlib 8/8、nano 454/541。
- **0 warnings**（`cargo check --all-targets`，默认 + 特性两构建）；
  **0 个新增 unsafe**（除 ffi.rs/ffi_dns.rs wrapper 内与 fromfd
  一处外，grep 验证）；文件 <2500 行（modsocket_ffi.rs、
  ffi_dns.rs 各自 <2500；modsocket.rs 不得超 2500）。
- **CI 可编译**：特性构建在 Linux / macOS / Windows / Android
  上至少能编译（CI 的 windows-gnu 交叉检查可作为参考；本机
  不许交叉编译——CI 任务的约束仍有效，lane 只做本地 Linux 验证，
  跨平台编译由 CI 验证，lane 须在结果文件写明哪些平台分支是
  未在本地验证的）。

## 6. 已知坑（先写下来，免得重复踩）

- `socket()` 返回的 fd 在 Unix 是 i32，在 Windows 是 SOCKET
  （usize）；不要假设 fd 是小整数；`fileno()` 返回 i64。
- `getaddrinfo` 返回的链表必须 `freeaddrinfo`，否则泄漏；
  用 RAII guard。
- `getnameinfo` / `gethostname` 的 buffer 大小：NI_MAXHOST 1025、
  NI_MAXSERV 32；hostname 用 256。
- `accept()` 在非阻塞 listener 上返回 EAGAIN，不是错误——按
  CPython 语义转成 BlockingIOError（errno EAGAIN）。
- `connect()` 在非阻塞 socket 上返回 EINPROGRESS，随后用
  select/poll 等可写；`connect_timeout` 按此实现。
- `send()` 返回 0 不代表对方关闭（UDP 上可能）；`recv()` 返回
  0 才表示对方关闭（TCP）。
- `SO_RCVTIMEO` / `SO_SNDTIMEO` 的值是 `timeval` 结构（秒+微秒），
  不是毫秒整数；Windows 上是 DWORD 毫秒——**平台不同**，cfg 分开。
- `inet_aton` 在 Windows 不存在；`getservbyname` 在 Windows
  要链接 ws2_32（已有）。
- Rust 的 `extern "C"` 声明不产生 C 代码，不需要 C 编译器；
  链接的是系统已有的 libc / ws2_32。
- `htonl` 等是字节序转换，不是系统调用；不要为它们写 FFI。
