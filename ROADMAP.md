# ROADMAP

目标：让 `uu <command>` 像本地命令一样自然，同时把 workspace、设备选择、Terminal 调度和 Windows UU 内部机制藏在 CLI 后面。

详细产品契约见 `DESIGN.md`，概念架构见 `ARCHITECTURE.md`，实现目标与验收见 `IMPLEMENTATION.md`，验证策略见 `TESTING.md`。

## 当前进度（2026-10-21）

PR [#7](https://github.com/teddyli18000/uu-cli/pull/7) 已实现原生 CLI、用户侧 binding、按设备共享的 worker pool、实例授权、shell 隔离、流式 I/O、no-replay/quarantine、校验文件传输、隐藏 watcher、用户级安装包及 Agent Skill。真实 Windows UU + disposable Ubuntu 18.04 WSL host 已验证 watcher 发现和授权、等待中的新 Terminal 无二次授权、两个 worker 并发、跨 workspace root/shell 隔离、16,385 字节二进制 `put/get`。详细证据见 [TESTING.md](TESTING.md#当前实现与证据2026-10-21) 和 [REAL_UU_FINDINGS.md](REAL_UU_FINDINGS.md)。

当前仍为验证候选，不是 v1 完成。另已验证真实 public caller 在已发送长任务期间崩溃后不重放任务；一次 watcher 自身空闲资源采样也已完成，但不含它启动的官方子进程。已有 helper 保持连接时的新 workspace 绑定已在修复候选实测通过，无需新 Terminal。网络/UU/Terminal/worker-helper 故障恢复、真实多设备、完整 watcher 进程树空闲资源，以及外部干净 Windows 从安装到实际使用仍待 `requires-real-uu` 验收。

## 当前产品模型

```text
Local Workspace
      ↓
Remote Binding = device + remote root
      ↓
Device-level Terminal Worker Pool
```

用户手工通过官方 UU 打开 Terminal；uu-cli 自动发现。Codex 不选择 deviceId，不管理 session，也不依赖某个 Terminal 的长期 shell-local 状态。

同一 remote device 可以服务多个 local workspace；worker pool 归 device，共享 worker，但每个 job 使用自己 workspace 的 remote root。

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

保留词冲突：

```powershell
uu -- <remote command...>
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

目标：证明最小产品闭环，而不是先堆框架。

```text
全新 local workspace
→ 用户在官方 UU 打开 Terminal
→ uu-cli 自动发现
→ 必要时短交互选择设备
→ 确认 remote root
→ uu <command> 真实执行并实时返回输出
```

验收：

- workspace root 识别正确；
- 多设备不会误绑；
- Codex 不需要 deviceId/session；
- 等待中的 `uu` 明确告诉用户新开 Terminal 会用于本次请求；用户打开后继续原请求，不要求重跑，也不重复确认；
- 人类选择不污染 job stdin/stdout；
- 普通命令只需增加 `uu` 前缀；
- 每个 job 从 remote root 开始，并尽量隔离 shell-local 变化；
- exit status 和错误语义可信；
- 长任务不会被工具自己的短超时截断；
- 已发送命令不会因为恢复逻辑重复执行。

从这一阶段开始就建立 Windows CI，不等最后再补。

## Phase 2 — Multiple Terminal workers + automatic discovery

目标：用户开几个官方 Terminal，就自然得到对应并发能力。

验收：

- 新 Terminal 自动发现；
- 有等待请求时按 Phase 1 语义直接满足，不重复确认；
- 没有等待请求时弹一次短授权：Enter=交给 uu-cli，N/Esc/关闭=保留手动使用；
- v1 不提供永久自动接管某设备后续新 Terminal 的开关；
- 多个新 Terminal 尽量在同一交互窗口处理；
- 一个 worker 同时只执行一个 job；
- 多个 `uu` 调用能够并发；
- worker 全忙时安全等待；
- 长任务一直占用自己的 worker并实时输出；
- 不跨设备调度；
- 同设备多个 workspace 共享 pool 时不会串 remote root；
- 用户未授权/正在手工使用的 Terminal 不被偷偷接管；
- 后台组件异常后，可能仍忙的 worker 不会被错误当 idle。

## Phase 3 — Reliable file transfer

实现公共接口：

```powershell
uu put .\file.bin remote/file.bin
uu get remote/file.bin .\roundtrip.bin
```

实现者自行决定复用哪条稳定官方路径或可靠 fallback。

验收：

- 任意二进制内容；
- 明确成功/失败；
- 不完整传输不返回成功；
- 远端相对路径相对于 workspace remote root；
- 绝对路径和 `~` 的远端语义正确；
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
- unknown/quarantined worker 不会被误复用；
- 不影响官方 UU 的启动、退出、升级；
- 不要求用户安装 PowerShell 7 或其他额外运行时。

内部是否使用 broker/watcher/event/lazy process 由实现者决定。

## Phase 5 — Portable installer + CI compatibility

目标不是“开发机能跑”，而是换一台正常 Windows 电脑也能安装后使用。

CI-first：凡是能在 GitHub-hosted Windows 上做的构建、故障注入、clean install、升级/卸载和兼容性检查都放进 Actions，尽量少折腾开发者主力 Windows。

验收：

- 自动发现官方 UU 安装和所需组件；
- 不硬编码开发机路径、盘符、用户名、deviceId、session 名；
- 不要求 Python/Rust/Node/PowerShell 7/WSL 等开发环境；
- 尽量 user-scope，不无故要求管理员；
- PATH、升级、卸载行为正确；
- 卸载不影响官方 UU 或登录状态；
- 版本/环境不兼容时有明确诊断；
- installer 在全新 hosted Windows job 中完成安装/基础诊断/卸载；
- 需要官方 UU installer 的 smoke test 只从官方公开来源临时下载，并验证 Authenticode/版本/hash；
- 可以通过 scheduled/manual workflow 定期检测最新版官方 UU 的安装/组件发现兼容性；
- CI 不保存真实 UU 登录凭据；
- 如果 hosted runner 的 OS/SKU 不受官方 UU 支持，UU 安装 smoke 可以 non-gating/manual，但其他 clean-state CI 仍必须 gating；
- 至少一个真实非开发/干净环境，从安装到首次 binding 再到真实执行完整通过。

GitHub-hosted runner 不能替代真实登录后的 UU 端到端测试；只把真实 Terminal/远端设备必须验证的最后一小段留给实机。

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

CI + 真实 Windows/官方 UU 分层验证至少覆盖：

```powershell
uu git status
uu python3 main.py
uu "source .venv/bin/activate && pytest -q"
uu put .\test.bin remote/test.bin
uu get remote/test.bin .\roundtrip.bin
uu list
```

并满足：

- 首次绑定交互短、明确；
- 多设备不会误绑；
- 等待中的 `uu` + 新 Terminal 无重复确认；
- 没有等待请求时未授权 Terminal 不被接管；
- 同设备多 workspace 不串目录；
- 多 Terminal 可安全并发；
- 长任务输出实时可见；
- 普通 job 不依赖/尽量不泄漏 Terminal 历史上下文；
- 命令不会因为内部重试被重复执行；
- crash/recovery 不会错误复用 busy worker；
- 文件字节一致；
- 空闲占用低；
- 安装/升级/卸载干净；
- 不修改、不劫持、不破坏官方 UU；
- CI clean-machine 路径通过；
- 真实非开发/干净环境可用。

达到 v1 之前不扩：一个 workspace 多设备、MCP、Web、tray、专用 Git/Python API、用户可见 session 管理。

实现者自行规划技术路线、开发顺序和 PR 粒度。路线图只约束结果，不约束内部手段。
