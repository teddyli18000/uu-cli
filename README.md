# uu-cli

让远程 UU 设备像本地 shell 一样可用。

`uu-cli` 复用 **Windows 本地已经运行、已经登录的网易 UU 远程**。它不重新实现登录，不保存账号凭据，也不要求 Codex 学一套新的远程工具。

## 日常使用

对人和 Agent 来说，远程执行只多一个 `uu` 前缀：

```powershell
uu pwd
uu git status
uu python3 main.py --debug
uu "source .venv/bin/activate && pytest -q"
```

文件传输：

```powershell
uu put .\main.py ~/aic/main.py
uu get ~/aic/result.csv .\result.csv
```

诊断/首次配置只保留：

```powershell
uu bind
uu list
```

正常情况下 Codex 不需要使用 `bind/list`。

## 核心模型

```text
本地 workspace
      ↓
远端绑定 = 设备 + remote root
      ↓
该设备上的 UU Terminal worker pool
```

用户通过官方 UU 手工打开 Terminal；`uu-cli` 自动发现这些已有 session，把可用 Terminal 当成执行槽。开 1 个 Terminal 就有 1 个 worker，开 3 个就可以让最多 3 个前台 job 并行。

Terminal 本身不属于 Codex，也不作为长期 shell 上下文。具体某条命令落到哪个 session，由 `uu-cli` 内部调度。

## 第一次使用

当前分析的官方 Windows UU 4.43.x 没有对外暴露可复用的 headless terminal launcher，因此第一个官方 Terminal 仍由用户手工打开。

第一次在某个项目中执行：

```powershell
uu git status
```

如果还没有 binding，`uu-cli` 会：

1. 自动确定 workspace 根目录：Git root 优先，否则当前目录；
2. 等待/发现你在官方 UU 中打开的 Terminal；
3. 如果只涉及一台远端设备，尽量一键确认；
4. 如果多台设备都有 Terminal，弹出一个简短 PowerShell 选择器让你选一次；
5. 记录该 workspace 的设备和 remote root；
6. 继续执行原来的命令。

Codex 不接触裸 `deviceId`，也不需要管理 session name/id。

## 多个 Terminal

同一设备上可以打开多个官方 Terminal：

```text
AIC-Car-1
  terminal 1  busy
  terminal 2  idle
  terminal 3  idle
```

`uu-cli` 自动调度空闲 worker。长任务持续占用自己的 terminal，并实时把输出返回给调用它的 Codex；其他命令使用其他空闲 worker。没有空闲 worker 时排队。

新开的 Terminal 会被自动发现；必要时只弹一次简短确认，决定是否把它加入 uu-cli 的 worker pool。

## 很重要：不同 `uu` 调用不共享 shell-local 状态

不要依赖上一条命令留下的 `cd`、`export`、virtualenv、alias 等状态，因为下一条命令可能被调度到另一个 Terminal。

不要这样：

```powershell
uu cd src
uu source .venv/bin/activate
uu python main.py
```

应该这样：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每条普通 `uu` job 都从当前 workspace 绑定的 `remote root` 开始。后续 Codex Skill 会把这条规则写死。

## 自动发现与占用

v1 允许一个极轻的用户级 broker，用来：

- 发现官方 UU 是否运行；
- 发现新打开的 Terminal；
- 协调多个 Codex/多个并发 `uu` 调用；
- 维护 worker busy/idle；
- 必要时弹出短 PowerShell 选择器。

它没有 GUI、tray 或可见 console。UU 没运行、没有 Terminal、没有任务时尽可能只等待，不保持无意义连接，也不高频轮询。

`uu` 被调用但 broker 不在时，CLI 可以自己拉起它，因此后台组件不是用户需要管理的第二套服务。

## 当前 Windows UU 限制

研究已确认：

- 外部工具可以查询、附着和复用已有 terminal session；
- 官方 GUI 创建 terminal 前会在 GameViewer 主程序进程内注册 local host/token；
- `windows_terminal -> new_console -> detached_process` 都属于官方 GUI launcher 链；
- 当前没有稳定的 CLI / IPC / deeplink 外部入口可以无点击创建第一个 session。

因此 v1 不注入 GameViewer、不伪造官方客户端、不绕过 host registration。用户只负责用官方 UI 打开 Terminal，之后的发现、绑定和调度由 `uu-cli` 处理。

## 非目标

本项目不做：

- UU 账号登录或凭据管理；
- 注入/签名绕过；
- 重新实现完整 UU 云协议；
- MCP / Web UI / tray；
- Git/Python/CMake 专用远程 API；
- 面向用户的 session 管理；
- 自动点击 UU GUI 创建 Terminal；
- v1 中一个 workspace 同时驱动多台远端设备。

## 文档

- [DESIGN.md](DESIGN.md)：产品契约与边界
- [ARCHITECTURE.md](ARCHITECTURE.md)：workspace / device / terminal pool 架构
- [IMPLEMENTATION.md](IMPLEMENTATION.md)：可以直接执行的工程蓝图
- [ROADMAP.md](ROADMAP.md)：阶段与验收
- [AGENTS.md](AGENTS.md)：给实现 Agent 的硬规则
- `docs/`、`proto/terminal.proto`：Windows UU terminal 互操作研究
