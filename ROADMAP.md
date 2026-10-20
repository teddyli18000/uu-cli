# ROADMAP

目标：让 `uu <command>` 像本地命令一样自然，同时把 workspace、设备选择、Terminal 调度和 Windows UU 内部机制藏在 CLI 后面。

详细产品契约见 `DESIGN.md`，概念架构见 `ARCHITECTURE.md`，实现目标与验收见 `IMPLEMENTATION.md`。

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

已经确认到足够支持实现：

- 已有官方 terminal session 可以查询、附着和复用；
- terminal 是持续 PTY 字节流；
- 官方协议没有逐命令 exec/exit-code API；
- 第一个 terminal 的 host registration 没有稳定外部 headless launcher；
- v1 接受“用户手工打开官方 Terminal，uu-cli 自动发现”的 bootstrap；
- 不继续投入 GameViewer 注入、签名伪造或 host-registration 绕过。

研究资料见 `docs/` 和 `proto/terminal.proto`。

## Phase 1 — Workspace binding + one working remote shell

目标不是搭某种指定框架，而是证明最小产品闭环：

```text
一个全新本地 workspace
→ 用户在官方 UU 打开 Terminal
→ uu-cli 自动发现
→ 必要时短交互选择设备
→ 绑定 remote root
→ uu <command> 可以真实执行并实时返回输出
```

验收：

- workspace root 识别正确；
- 多设备不会误绑；
- Codex 不需要 deviceId/session；
- 普通命令只需增加 `uu` 前缀；
- exit status 和错误语义可信；
- 长任务不会被工具自己的短超时截断；
- 已发送命令不会因为恢复逻辑重复执行。

## Phase 2 — Multiple Terminal workers + automatic discovery

目标：用户开几个官方 Terminal，就能自然获得对应并发能力。

验收：

- 新 Terminal 自动发现；
- 无歧义尽量自动，有歧义才弹短 PowerShell 选择；
- 一个 worker 同时只执行一个 job；
- 多个 `uu` 调用能够并发；
- worker 全忙时安全等待；
- 长任务一直占用自己的 worker并实时输出；
- 不跨设备调度；
- 用户未授权/正在手工使用的 Terminal 不被偷偷接管。

## Phase 3 — Reliable file transfer

实现公共接口：

```powershell
uu put .\file.bin /remote/file.bin
uu get /remote/file.bin .\roundtrip.bin
```

实现者自行决定复用哪条稳定官方路径或可靠 fallback。

验收：

- 任意二进制内容；
- 明确成功/失败；
- 不完整传输不返回成功；
- 多种大小和内容 round-trip 后逐字节一致；
- 文件传输不会干扰官方 UU 正常使用。

## Phase 4 — Low-idle lifecycle + recovery

目标：UU 在用时 `uu` 随时可用；没任务时尽可能没有成本。

验收：

- 用户不需要管理第二个服务；
- UU/Terminal 启停后能够恢复发现；
- 无任务时 CPU 接近 0；
- 无永久高频 polling/spawn；
- 无 tray、额外 GUI、常驻可见 console；
- 后台组件异常后 `uu` 可自动恢复或给出清楚错误；
- 不影响官方 UU 的启动、退出、升级。

内部是否使用 broker/watcher/event/lazy process 由实现者决定。

## Phase 5 — Portable installer + clean-machine validation

目标不是“开发机能跑”，而是换一台正常 Windows 电脑也能安装后使用。

验收：

- 自动发现官方 UU 安装和所需组件；
- 不硬编码开发机路径、盘符、用户名、deviceId、session 名；
- 不要求 Python/Rust/Node/WSL 等开发环境；
- 尽量 user-scope，不无故要求管理员；
- PATH、升级、卸载行为正确；
- 卸载不影响官方 UU 或登录状态；
- 版本/环境不兼容时有明确诊断；
- 至少一台非开发机或干净用户环境，从安装到首次绑定再到真实执行完整通过。

## Agent Skill

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
- 多 Terminal 可安全并发；
- 长任务输出实时可见；
- 不依赖 Terminal 历史上下文；
- 命令不会因为内部重试被重复执行；
- 文件字节一致；
- 空闲占用低；
- 安装/升级/卸载干净；
- 不修改、不劫持、不破坏官方 UU；
- 非开发机/干净环境可用。

达到 v1 之前不扩：一个 workspace 多设备、MCP、Web、tray、专用 Git/Python API、用户可见 session 管理。

实现者自行规划技术路线、开发顺序和 PR 粒度。路线图只约束结果，不约束内部手段。