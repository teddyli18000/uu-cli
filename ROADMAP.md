# ROADMAP

`uu-cli` 的路线只有一个判断标准：**让 `uu <command>` 像本地命令一样自然，并且不要求用户理解 UU 的内部机制。**

这不是功能清单，而是实现顺序和决策门槛。前一阶段没有被证据证明之前，不为了“完整”提前做后一阶段。

## 当前状态

已经确定的产品契约：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>

uu devices
uu use <device>
```

已经完成的研究基础：

- Windows 官方 UU 4.43.x 的终端通道互操作笔记：`docs/terminal.md`
- 终端相关 `gv_pb` 子集：`proto/terminal.proto`
- 已确认官方客户端存在终端新建、附着、列表、终止等能力

仍未决定：

- 最终终端实现是否直接包装官方 CLI、调用更底层本地接口，或自行实现已确认的通道子集；
- 文件传输最稳定的本地入口；
- 后台 helper 是否必要；
- UU 生命周期联动的最佳实现方式；
- 安装器技术选型。

这些问题必须由实机证据决定，而不是先画架构再找理由。

---

## Phase 0 — Research / interoperability

目标：把“官方 Windows UU 到底已经提供了什么”弄清楚。

### Terminal

当前已有较完整基础，但实现前还要验证最小闭环：

1. 不打开 UU GUI Terminal 的情况下，能否从本地程序直接建立远端终端；
2. 新建、attach、detach、退出的真实生命周期；
3. stdin/stdout/stderr/exit code 能拿到什么程度；
4. 长任务与 Ctrl-C 的行为；
5. 并行多个终端的边界；
6. UU 完全退出后通道如何失效。

**通过门槛：** 能证明后台程序可以稳定完成“一条命令 → 远端 shell → 输出返回”，且主控端无窗口。

### File transfer

单独分析 UU GUI 的文件上传/下载路径，回答：

1. 本地哪个组件真正执行文件传输；
2. 是否存在可复用的本地 IPC / RPC / 内部接口；
3. 上传和下载的请求、进度、完成、错误语义；
4. 是否可以完全无 GUI；
5. 大文件、覆盖、取消、失败恢复如何表现。

**通过门槛：** 能以最小原型把任意二进制文件从主控端传到远端再取回，并验证字节一致。

> 不把 terminal + base64 当作正式文件传输方案，除非研究证明原生路径不可用；即使需要 fallback，也不能改变 `uu put/get` 公共接口。

---

## Phase 1 — Minimal shell prototype

只证明：

```powershell
uu pwd
uu "cd ~/project && python3 main.py"
```

必须做到：

- 无手工打开 UU Terminal；
- 主控端无弹窗；
- 自动建立或复用远端 terminal；
- 命令尽量原样交给远端 shell；
- stdout/stderr 可供 Agent 直接消费；
- 远端退出码尽可能映射到本地；
- Ctrl-C 不把内部状态搞坏；
- 长任务不被短超时截断。

这一阶段不做安装器、不做漂亮配置、不做额外命令。

**通过门槛：** Codex 原本会运行的普通 shell 命令，只需前面增加 `uu` 即可远程执行。

---

## Phase 2 — Reliable `put/get`

实现：

```powershell
uu put .\file.bin /remote/file.bin
uu get /remote/file.bin .\roundtrip.bin
```

要求：

- 任意二进制内容；
- 明确成功/失败；
- 不完整传输绝不返回成功；
- 必要时内部校验 size/hash；
- 错误不要求用户理解传输协议；
- 大文件行为明确，不靠“碰运气”。

**通过门槛：** 多种大小与内容的文件 round-trip 后逐字节一致。

---

## Phase 3 — Default device selection

实现最小管理面：

```powershell
uu devices
uu use <device>
```

目标不是“设备管理”，而是解决多设备账号下 `uu` 到底发给谁。

要求：

- 首次只有一台设备时尽量零操作；
- 多台设备时用户明确选择；
- 默认目标持久保存；
- 日常命令仍不带设备参数；
- 默认目标离线时明确失败；
- 不自动 failover 到别的设备；
- 不因为同名/近似名猜设备。

**通过门槛：** 多台在线设备时，命令不会误执行到错误机器。

---

## Phase 4 — Background lifecycle

只在前面功能链路明确后决定是否真的需要后台 helper。

外部行为固定：

```text
官方 UU 启动 → uu-cli 自动可用
官方 UU 完全退出 → uu-cli 不再独立常驻
```

要求：

- 无 `uu start/stop`；
- 无 console / GUI / tray；
- UU 重启后自动恢复；
- helper 崩溃不破坏官方 UU；
- UU 崩溃/强退后不留孤儿后台；
- 不为了实现方便安装一个永远常驻的独立服务。

**通过门槛：** 用户正常开关 UU 即可，不需要意识到第二个进程体系。

---

## Phase 5 — Installer / Release

最终用户拿到一个 EXE：

```text
uu-cli-setup.exe
```

安装：

- user-scope 优先；
- 安装 `uu`；
- 写入当前用户 PATH；
- 安装必要后台集成；
- 首次需要时选择默认设备；
- 注册 Windows 标准卸载项。

升级：

- 原地升级；
- 保留合理配置；
- 不重复污染 PATH；
- 不留下旧 helper。

卸载：

- 移除项目自身文件；
- 移除 PATH 和生命周期集成；
- 移除项目自身配置；
- 不退出、不卸载、不清空官方 UU；
- 不破坏 UU 登录状态。

**通过门槛：** 干净 Windows 用户环境中“安装一次 → 直接用 → 标准卸载后恢复原状”。

---

## v1 freeze

达到以下条件后才考虑 v1：

```powershell
uu devices
uu use <target>
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

并同时满足：

- 无手工 UU Terminal 操作；
- 主控端无额外窗口；
- 命令/文件都到正确默认设备；
- 输出适合人和 Agent 直接消费；
- 长任务可用；
- 文件字节一致；
- UU 退出后无独立残留；
- 安装/升级/卸载干净。

在这之前不扩：MCP、Web UI、tray、专用 Git/Python API、用户可见 session 管理、远程桌面自动化等。

## 设计纪律

遇到新的内部发现时，按这个顺序判断：

1. 它是否改变了用户真正需要的行为？
2. 如果没有，能不能完全藏在 `uu / put / get` 后面？
3. 是否有现成官方能力可以复用？
4. 是否有更小、更稳定的路径？
5. 是否正在为了内部实现方便而增加用户学习成本？

第 5 条如果答案是“是”，默认说明设计方向错了。
