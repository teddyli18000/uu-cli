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

首次配置/诊断最多保留：

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

用户通过官方 UU 手工打开 Terminal；`uu-cli` 自动发现这些已有 session，把被允许使用的 Terminal 当成执行槽。开 1 个 Terminal 就有 1 个 worker，开 3 个就可以让最多 3 个前台 job 并行。

Terminal 本身不属于 Codex，也不作为长期 shell 上下文。具体某条命令落到哪个 session，由 `uu-cli` 内部调度。

## 第一次使用

当前分析的官方 Windows UU 4.43.x 没有对外暴露可复用的 headless terminal launcher，因此第一个官方 Terminal 仍由用户手工打开。

第一次在某个项目中执行：

```powershell
uu git status
```

如果还没有 binding，`uu-cli` 会自动确定 workspace、发现用户打开的 Terminal，并在需要时做一次很短的人类选择。

- Git 仓库默认使用 Git root；
- 只有一台明确设备时尽量自动完成；
- 多台设备同时有 Terminal 时，弹一个简单 PowerShell 选择器；
- 用户只选择友好设备名，不接触裸 `deviceId`；
- binding 完成后，原命令继续执行，不要求重跑。

如果后续新打开 Terminal，`uu-cli` 应自动发现。归属明确时尽量自动处理；有歧义或无法判断是否供 uu-cli 使用时，再弹一个几秒内能完成的 PowerShell 选择。

## 多个 Terminal

同一设备上可以打开多个官方 Terminal：

```text
AIC-Car-1
  terminal 1  busy
  terminal 2  idle
  terminal 3  idle
```

`uu-cli` 自动调度空闲 worker。长任务持续占用自己的 terminal，并实时把输出返回给调用它的 Codex；其他命令使用其他空闲 worker。没有空闲 worker 时等待。

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

每条普通 `uu` job 都从当前 workspace 绑定的 `remote root` 开始。后续 Codex Skill 会明确写出这条规则。

## 不干扰官方 UU

`uu-cli` 是旁路辅助工具，不修改或替代官方 UU。

它不能：

- 修改/注入/patch 官方 UU 或 GameViewer；
- 改 UU 登录、账号、设备、更新、启动配置；
- 保存密码、Token、Cookie 等认证材料；
- kill、rename 或偷偷接管用户没有交给 uu-cli 的 Terminal；
- 改变用户正在手工使用的 Terminal；
- 因自身崩溃、升级或卸载影响 UU 的正常使用。

如果某种 attach/reuse 方法会明显干扰官方 UI，就必须换更安全的方案或只使用用户明确提供给 uu-cli 的专用 Terminal。

## 尽可能任何正常电脑都能用

目标不是只在开发机可用，而是：**只要官方 UU Remote 本身能在该 Windows 电脑正常工作，uu-cli 就应尽量做到安装后直接可用。**

因此实现不能硬编码开发机路径、用户名、盘符、deviceId 或精确安装目录；应自动发现官方 UU 和所需组件，尽量使用普通用户权限，不要求用户额外安装 Python/Rust/Node/WSL 等开发环境。

发布前必须在非开发机或干净用户环境验证从安装、首次绑定到实际执行的完整流程。

## 自动发现与占用

产品要求是：官方 UU 在用时，`uu-cli` 随时可用；没有任务或没有 Terminal 时，资源占用尽可能低。

内部可以有轻量后台组件，也可以采用其他实现。无论如何都必须：

- 自动发现新开的官方 Terminal；
- 空闲时 CPU 接近 0，不高频轮询；
- 无 tray、额外 GUI 或常驻可见 console；
- 只有真正需要用户选择时才弹短 PowerShell 交互；
- 用户不需要手工管理第二个服务；
- 不拖累 UU 的启动、退出和升级。

## 官方下载入口

CI 和兼容性测试如需下载安装官方 UU Remote，应从网易官方公开入口获取：

```text
https://uuyc.163.com/download/
```

不要把官方安装包提交或重新分发进本仓库；运行安装包前应验证来源、Authenticode 签名、版本和 hash。

## 当前 Windows UU 限制

研究已确认：

- 外部工具可以查询、附着和复用已有 terminal session；
- 官方 GUI 创建 terminal 前会在 GameViewer 主程序进程内注册 local host/token；
- `windows_terminal -> new_console -> detached_process` 都属于官方 GUI launcher 链；
- 当前没有稳定的 CLI / IPC / deeplink 外部入口可以无点击创建第一个 session。

因此 v1 不注入 GameViewer、不伪造官方客户端、不绕过 host registration。用户只负责用官方 UI 打开 Terminal，之后的发现、绑定和调度由 `uu-cli` 处理。

## 非目标

本项目 v1 不做：

- UU 账号登录或凭据管理；
- 注入/签名绕过；
- 重新实现完整 UU 云协议；
- MCP / Web UI / tray；
- Git/Python/CMake 专用远程 API；
- 面向用户的 session 管理；
- 自动点击 UU GUI 创建 Terminal；
- 一个 workspace 同时驱动多台远端设备。

## 文档

- [DESIGN.md](DESIGN.md)：产品契约与边界
- [ARCHITECTURE.md](ARCHITECTURE.md)：workspace / device / terminal pool 的概念架构
- [IMPLEMENTATION.md](IMPLEMENTATION.md)：实现目标、兼容性和验收契约；不规定具体技术方案
- [TESTING.md](TESTING.md)：CI-first 验证策略和实机边界
- [ROADMAP.md](ROADMAP.md)：阶段目标与验收
- [AGENTS.md](AGENTS.md)：给实现 Agent 的硬规则
- `docs/`、`proto/terminal.proto`：Windows UU terminal 互操作研究
