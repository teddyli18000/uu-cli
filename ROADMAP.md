# ROADMAP

目标：让 `uu <command>` 像本地命令一样自然，同时把 workspace、设备选择、Terminal 调度和 Windows UU 内部机制藏在 CLI 后面。

详细产品契约见 `DESIGN.md`，最终架构见 `ARCHITECTURE.md`，可直接执行的工程步骤见 `IMPLEMENTATION.md`。

## 当前产品模型

```text
Local Workspace
      ↓
Remote Binding = device + remote root
      ↓
Terminal Worker Pool
```

用户手工通过官方 UU 打开 Terminal；uu-cli 自动发现。Codex 不选择 deviceId，不管理 session，也不依赖某个 Terminal 的长期 shell-local 状态。

日常接口：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

配置/诊断：

```powershell
uu bind
uu list
```

## Phase 0 — Terminal research — CLOSED for v1

已确认：

- 已有官方 terminal session 可以查询、附着和复用；
- terminal 是持续 PTY 字节流；
- 官方协议没有逐命令 exec/exit-code API；
- 单个 `uu` job 需要内部 completion marker；
- 第一个 terminal 的 host registration 只存在于官方 GUI launcher 内；
- `windows_terminal -> new_console -> detached_process` 没有可复用的外部 launcher 入口；
- v1 接受“用户手工打开官方 Terminal，uu-cli 自动发现”的 bootstrap；
- 不继续投入 GameViewer 注入、签名伪造或 host-registration 绕过。

研究资料见 `docs/` 和 `proto/terminal.proto`。

## Phase 1 — Repository scaffold + workspace binding

目标：先把所有后续实现依赖的框架搭好。

完成：

- Rust stable 工程；
- 一个 `uu.exe`，内部允许隐藏 broker 模式；
- `%LOCALAPPDATA%\uu-cli` 配置；
- workspace root lookup：最近 binding，首次 Git root 优先；
- CLI ↔ broker user-scoped named pipe skeleton；
- 官方 UU 安装定位；
- `term ls --json` session discovery；
- 按 device identity 分组；
- 首次 bind PowerShell UX；
- `uu list`。

验收：

```text
从一个未绑定的 Git repo 执行 uu ...
→ 找到 workspace root
→ 用户打开 UU Terminal
→ 自动发现
→ 多设备时只提示用户选择一次
→ 保存 workspace 的 device + remote root
```

## Phase 2 — One-worker shell

只证明一个已有 Terminal 可以稳定执行：

```powershell
uu pwd
uu git status
uu "cd src && python3 main.py"
```

要求：

- attach/reuse 已有 session；
- 每次 job 从 remote root 开始；
- completion marker + 随机 nonce；
- marker 不泄漏；
- exit code 返回；
- PTY 输出实时流回；
- 长任务不被短超时截断；
- Ctrl+C 行为可用；
- 连接异常后不自动重放已经发送的命令。

验收：Codex 只加一个 `uu` 前缀即可完成普通远端 shell 工作。

## Phase 3 — Worker pool + automatic discovery

目标：用户开几个官方 Terminal，就得到几个可并发 worker。

要求：

- broker 自动发现新 session；
- 已绑定设备的新 Terminal 可采用/跳过；
- worker busy/idle lease；
- 一个 worker 同时只有一个 job；
- 多个并发 `uu` 调用正确分配；
- worker 全忙时 FIFO 排队；
- 长任务一直占用自己的 worker并实时输出；
- 不跨设备调度；
- broker 重启后安全重建 worker 视图，不重放未知旧 job。

自动发现优先使用进程/事件触发；fallback polling 必须退避，不允许永久高频启动官方 CLI。

## Phase 4 — File transfer research + `put/get`

先研究官方 Windows UU 原生文件入口：

1. 哪个组件实际发起上传/下载；
2. 是否存在可复用本地 CLI / IPC / RPC；
3. 是否依赖 GUI 文件面板；
4. 请求、进度、完成、覆盖、失败语义；
5. 任意二进制文件是否可完全无辅助窗口传输。

然后实现：

```powershell
uu put .\file.bin /remote/file.bin
uu get /remote/file.bin .\roundtrip.bin
```

验收：不同大小/内容文件 round-trip 后逐字节一致，不完整传输绝不返回成功。

## Phase 5 — Broker lifecycle + installer

目标：自动可用，但没事时几乎不占资源。

要求：

- user-level hidden broker/watcher；
- 无 console / tray / GUI；
- UU 不运行时只等待；
- UU 运行但无 Terminal/job 时不保持无意义连接；
- `uu` 在 broker 缺失时可以自动拉起；
- installer user-scope 优先；
- PATH、auto-start、uninstall 完整；
- 卸载不影响官方 UU 或登录状态。

## Agent Skill — 与 Phase 2 同步完成

Skill 只需要：

```text
远端命令：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

必须显式说明：

> 不同 `uu` 调用之间不要依赖 `cd/export/source/alias/virtualenv` 等 shell-local 状态。需要连续状态时放在同一条命令；每个普通 job 从 workspace remote root 开始。

Agent 永远不选择 deviceId/session/worker。

## v1 freeze

在真实 Windows + 官方 UU 环境至少验证：

```powershell
uu git status
uu python3 main.py
uu "source .venv/bin/activate && pytest -q"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
uu list
```

并满足：

- 首次绑定交互短、明确；
- 多设备不会误绑；
- 多 Terminal 可以并发调度；
- 长任务输出实时可见；
- 不依赖 Terminal 历史上下文；
- 命令不会因为内部重试被重复执行；
- 文件字节一致；
- 空闲占用低；
- 安装/升级/卸载干净。

达到 v1 之前不扩：一个 workspace 多设备、MCP、Web、tray、专用 Git/Python API、用户可见 session 管理。
