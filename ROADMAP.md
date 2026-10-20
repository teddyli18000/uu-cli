# ROADMAP

`uu-cli` 的路线只有一个判断标准：**让 `uu <command>` 像本地命令一样自然，并且不要求用户理解 UU 的内部机制。**

当前 Windows UU 4.43.x 的研究已经把 terminal 的主要边界查清：已有 session 可以管理/附着/复用，但第一个 terminal session 的 host registration 只存在于官方 GUI launcher 内，没有可复用的外部 headless 入口。因此 v1 接受一个人工 bootstrap：用户在 UU 中为目标设备打开一次官方 Terminal，之后 `uu` 负责复用。

## 当前产品契约

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>

uu devices
uu use <device>
```

正常工作流：

```text
UU 已运行并登录
  ↓
用户为目标设备打开一次官方 Terminal
  ↓
uu-cli 发现并复用 session
  ↓
Codex / 用户只执行 uu ...
```

没有可用 session 时，`uu` 明确提示用户打开一次官方 Terminal；不自动点 GUI，不尝试绕过官方 host registration。

---

## Phase 0 — Research / interoperability

### Terminal — CLOSED for v1

已完成的主要结论：

- TERM / RPC / session 管理的关键协议行为已记录；
- 官方 CLI 能查询、附着、终止已有 session；
- terminal 是持续 PTY 字节流，不存在客户端可达的逐命令 exec/exit-code API；
- 单次 `uu` 调用需要客户端内部 marker/sentinel 来识别命令完成和 `$?`；
- 官方 GUI 创建 terminal 前会在主程序进程内注册 local host/token；
- `windows_terminal`、`new_console`、`detached_process` 都属于同一个 GUI launcher 链；
- launcher 没有 CLI / IPC / deeplink 外部调用入口；
- v1 不继续投入时间逆向 headless host registration，也不采用注入、伪造签名等脆弱路线。

研究资料见 `docs/` 和 `proto/terminal.proto`。

### File transfer — NEXT RESEARCH PRIORITY

只回答 `uu put/get` 真正需要的问题：

1. Windows 官方 UU 哪个组件发起原生文件上传/下载；
2. 是否存在可复用的本地 CLI / IPC / RPC 入口；
3. 是否要求 GUI 文件面板已经打开；
4. 请求、进度、完成、覆盖和失败语义；
5. 能否无辅助窗口传输任意二进制文件；
6. 如果原生入口不可复用，最小可靠 fallback 是什么。

**通过门槛：** 最小原型可以把任意二进制文件传到远端再取回，并逐字节一致。

> 不预设 terminal + base64 为正式方案。只有确认原生文件通道不可复用后才评估 fallback，公共接口始终是 `uu put/get`。

### Device identity — PARALLEL RESEARCH

产品 UX 已定，只需确认实现字段：

- `device list` 实际返回哪些身份字段；
- 哪个字段在 UU 重启、设备改名、重新登录后稳定；
- 在线/离线状态来源；
- 同名设备如何可靠区分；
- 默认目标持久化保存什么最安全。

**通过门槛：** 多设备时不会因为名字相似或 ID 不稳定而把命令发错设备。

---

## Phase 1 — Minimal shell prototype

前置条件：用户已在官方 UU 中为目标设备打开一个可复用 terminal session。

只证明：

```powershell
uu pwd
uu "cd ~/project && python3 main.py"
```

必须做到：

- 自动发现当前默认设备的已有 terminal；
- 自动 attach / reuse，不暴露 session name/id；
- 没有可用 session 时明确提示人工 bootstrap；
- 不自动弹 UU GUI；
- 命令尽量原样交给远端 shell；
- 使用带随机 nonce 的内部 completion marker 获取单次命令边界与 exit code；
- marker 不泄漏到用户输出；
- PTY 合流输出可供 Agent 直接消费；
- Ctrl-C 不把 session 状态搞坏；
- 长任务不被短超时截断。

**通过门槛：** 在 terminal bootstrap 完成后，Codex 原本会运行的普通 shell 命令只需前面增加 `uu` 即可远程执行。

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

要求：

- 首次只有一台设备时尽量零操作；
- 多台设备时用户明确选择；
- 默认目标持久保存；
- 日常命令仍不带设备参数；
- 默认目标离线时明确失败；
- 不自动 failover；
- 不因为同名/近似名猜设备；
- 当前目标没有可复用 terminal 时，只提示为该目标打开一次官方 Terminal。

**通过门槛：** 多台在线设备时，命令和 terminal session 都不会匹配到错误机器。

---

## Phase 4 — Runtime lifecycle

实现前先判断是否真的需要后台 helper。优先无常驻设计。

要求：

- 无 `uu start/stop`；
- 无额外 GUI / tray；
- UU 关闭后不留下独立后台服务；
- session 失效能被检测并清楚报错；
- UU 重启后允许用户重新完成一次 Terminal bootstrap，然后继续使用；
- helper 崩溃不能破坏官方 UU。

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
- 首次需要时选择默认设备；
- 注册 Windows 标准卸载项；
- 如果当前没有 terminal session，在首次执行时给出简短 bootstrap 提示，而不是安装器自动操纵 GUI。

升级：保留合理配置，不重复污染 PATH，不留下旧 helper。

卸载：只移除项目自身文件、配置、PATH 和集成；不影响官方 UU 或登录状态。

---

## v1 freeze

前置条件：用户已为默认目标打开一个官方 Terminal。

然后：

```powershell
uu devices
uu use <target>
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

并满足：

- 用户不需要管理 session，只需要完成一次官方 Terminal bootstrap；
- `uu-cli` 不自动操作 GUI、不弹辅助窗口；
- 命令/文件都到正确默认设备；
- 单次命令结束和 exit code 可可靠识别；
- 输出适合人和 Agent 直接消费；
- 长任务可用；
- 文件字节一致；
- UU 退出后无独立残留；
- 安装/升级/卸载干净。

在这之前不扩：MCP、Web UI、tray、专用 Git/Python API、用户可见 session 管理、GUI 自动化等。

## 设计纪律

遇到新的内部发现时，按这个顺序判断：

1. 它是否改变了用户真正需要的行为？
2. 如果没有，能不能完全藏在 `uu / put / get` 后面？
3. 是否有现成官方能力可以复用？
4. 是否有更小、更稳定的路径？
5. 是否正在为了内部实现方便而增加用户学习成本？

第 5 条如果答案是“是”，默认说明设计方向错了。
