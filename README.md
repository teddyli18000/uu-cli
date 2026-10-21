# uu-cli

让远程 UU 设备像本地 shell 一样可用。

`uu-cli` 复用 **Windows 本地已经运行、已经登录的网易 UU 远程**。它不重新实现登录，不保存账号凭据，也不要求 Codex 学一套新的远程工具。

## 当前状态与安装

当前分支提供 Windows 原生候选实现。2026-10-21 已用登录中的官方 UU 4.43.0.3607、用户授权的 disposable Ubuntu 18.04 WSL host 和 `uu-remote-linux` v0.2.12-aic.3 验证后台发现与授权、两个 worker 并发、跨 workspace 隔离、真实 `put/get`，以及 caller 崩溃后不重放已发送任务。这些开发机实测和 hosted CI 都不代表 v1 已完成；外部干净 Windows、真实多设备及网络/worker 恢复仍待验收。

从 [GitHub Release v0.1.0-preview.2](https://github.com/teddyli18000/uu-cli/releases/tag/v0.1.0-preview.2) 下载 `uu-cli-windows-package.zip`，解压后，在 Windows 自带 PowerShell 中运行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\install.ps1
```

安装到当前用户的 `%LOCALAPPDATA%\Programs\uu-cli`，添加用户 PATH，不设置开机启动，也不在安装时启动后台。首次执行 `uu` 时按需启动发现组件；官方 UU 退出时发现组件和本工具的附着连接随之退出。无需 Rust、Python、Node、WSL 或 PowerShell 7；新终端会收到更新后的 PATH。卸载运行同包中的 `installer\uninstall.ps1`。安装、升级和卸载不修改官方 UU；卸载保留用户绑定及 unknown worker 安全记录，避免重装后误复用可能仍忙的 Terminal。

Agent Skill 已写好，随包放在 `skills/uu-cli/SKILL.md`，Release 也提供独立的 `uu-cli-skill.zip`。将其中的 `uu-cli` 文件夹放入 Codex 的 `$CODEX_HOME/skills`（未设置时为 `%USERPROFILE%\.codex\skills`），在新会话中使用。CLI installer 不修改 Codex 配置或自动安装 Skill。

本次预发布交付已实测的 shell、自动发现/授权、worker pool 与文件通道；按当前使用范围，断网等异常恢复不作为这次交付的阻塞项。后续完整验收仍按 `TESTING.md` 记录。

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
- 多台设备同时有 Terminal 时，弹一个简单 独立 console 选择器；
- 用户只选择友好设备名，不接触裸 `deviceId`；
- binding 完成后，原命令继续执行，不要求重跑。

等待中的请求会明确提示新开的匹配 Terminal 将用于本次请求，随后不会重复授权。若官方连接仍被该 Terminal 标签页占用，按提示关闭这个专用标签页以释放连接；不要在里面输入 `exit`。没有等待请求时，新 Terminal 需要一次短授权：Enter 交给 uu-cli，N/Esc/关闭保留手动使用。授权绑定到具体实例，重建的同名 Terminal 不继承旧授权。

## 多个 Terminal

同一设备上可以打开多个官方 Terminal：

```text
AIC-Car-1
  terminal 1  busy
  terminal 2  idle
  terminal 3  idle
```

`uu-cli` 自动调度空闲 worker。长任务持续占用自己的 terminal，并实时把输出返回给调用它的 Codex；其他命令使用其他空闲 worker。没有空闲 worker 时等待。

真实验证中，`sleep 30` 与短任务同时运行且使用不同远端 Bash PID；短任务在长任务结束前完成。两个 workspace 也通过同一 pool 运行，并各自从绑定的 remote root 开始，`cd`/`export` 不泄漏到另一个 workspace 或后续 job。

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

每条普通 `uu` job 都从当前 workspace 绑定的 `remote root` 开始。随包的 Codex Skill 明确写出这条规则。

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
- 只有真正需要用户选择时才弹短 console 交互；
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


## 当前实现约束

远端使用兼容 POSIX 的交互 shell；普通 job 在 subshell 中从 remote root 开始。PTY 将 stdout/stderr 合流，uu 直接转发合流输出，返回远端 exit code。命令一旦发送，断连只会报告 unknown/may-still-be-running 并隔离 worker，不会换 Terminal 重放。需要继续使用时，在官方 UU 中打开新 Terminal。

文件通道通过同一已授权 worker 传输 base64，远端需要 `base64`、`dd`、`wc`、`tr`、`cut`、`sha256sum` 和 `mv`。上传按 768 字节分块；下载逐块确认并解码。两端核对字节数和 SHA-256 后才替换目标。真实 `put/get` 已对 16,385 个随机二进制字节完成哈希一致验证，也覆盖零字节、相对/绝对/`~` 路径及中英文和空格路径；该样本之外的大文件吞吐仍未实测。失败上传可能留下相邻的 `.uu-cli-*.part` 文件，不会作为成功目标发布。

默认状态放在 `%LOCALAPPDATA%\uu-cli`，不写入 workspace。隐藏 watcher 由 `uu` 按需启动，官方 UU 开启期间每 20 秒检查新 Terminal；前台等待 Terminal 时每 2 秒检查。watcher 用进程句柄等待官方 UU 退出，无 UU 时不常驻轮询。专用 worker helper 保持连接供后续命令复用，空闲五分钟或官方 UU 退出时释放。退出官方 UU 后，尚未确认完成的命令仍按 unknown 处理，不自动重放。发现和授权的实测结果及资源采样见 [TESTING.md](TESTING.md)。
