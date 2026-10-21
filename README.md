# uu-cli

让远程 UU 设备像本地 shell 一样可用。

`uu-cli` 复用 **Windows 本地已经运行、已经登录的网易 UU 远程**。它不重新实现登录，不保存账号凭据，也不要求 Codex 学一套新的远程工具。

## 当前状态与安装

`v0.1.2` 修复 Windows 打包应用导致的配置隔离：人类终端与 Codex 统一使用用户目录下的状态，已有绑定和授权自动导入。手动绑定、无弹窗和按设备排队的用法不变。2026-10-21 的 WSL 和小车实测证明了 shell、文件通道、取消和并发等能力，但其中的弹窗/等待请求授权流程属于旧交互设计，不能作为新 UX 的验收证据。当前契约要求用户在调用终端手动运行 `uu bind` / `uu list`；新交互有独立的 Windows fixture 验收。实测证据见 `TESTING.md` 和 `REAL_UU_FINDINGS.md`。这些实测和 hosted CI 都不代表全部 v1 验收已完成；外部干净 Windows、真实多设备及网络/worker 恢复仍待验收。

从 [GitHub Latest Release](https://github.com/teddyli18000/uu-cli/releases/latest) 下载 `uu-cli-windows-package.zip`，解压后，在 Windows 自带 PowerShell 中运行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\install.ps1
```

安装到当前用户的 `%LOCALAPPDATA%\Programs\uu-cli`，添加用户 PATH，不设置开机启动，也不启动发现 watcher。需要选择或授权 Terminal 时，由用户在 workspace 调用终端手动运行 `uu bind`；普通 `uu` 只定位/使用已授权 worker，不自动做 binding 或授权。无需 Rust、Python、Node、WSL 或 PowerShell 7；新终端会收到更新后的 PATH。卸载运行同包中的 `installer\uninstall.ps1`。安装、升级和卸载不修改官方 UU；卸载保留用户绑定及 unknown worker 安全记录，避免重装后误复用可能仍忙的 Terminal。

Agent Skill 已写好，随包放在 `skills/uu-cli/SKILL.md`，Release 也提供独立的 `uu-cli-skill.zip`。将其中的 `uu-cli` 文件夹放入 Codex 的 `$CODEX_HOME/skills`（未设置时为 `%USERPROFILE%\.codex\skills`），在新会话中使用。CLI installer 不修改 Codex 配置或自动安装 Skill。

旧版实测覆盖 shell、自动发现/授权、worker pool 与文件通道；自动授权和弹窗属于已废弃的旧交互证据，不代表当前 binding 契约已经通过实机验收。断网等异常恢复按当前范围暂不作为本次交付阻塞项；其他验收状态见 `TESTING.md`。

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

首次配置和诊断命令由用户本人执行：

```powershell
uu bind
uu list
```

Agent 直接执行任务命令，不例行检查连接、绑定或状态。绑定和状态查看由用户自行处理；Agent 不代跑 `uu bind` / `uu list`，也不把正常任务变成配置流程。只有实际执行受阻时，简短报告具体错误一次，必要时请求最少的人类介入，避免反复要求检查或重绑。

## 核心模型

```text
本地 workspace
      ↓
远端绑定 = 设备 + remote root
      ↓
该设备上的 UU Terminal worker pool
```

用户通过官方 UU 手工逐个打开专用 Terminal，并通过自己运行的 `uu bind` 选择并授权。发现 Terminal 不等于授权；只有已授权的 Terminal 才进入该设备 worker pool。开 1 个就有 1 个 worker，开 3 个就可以让最多 3 个前台 job 并行。

Terminal 本身不属于 Codex，也不作为长期 shell 上下文。具体某条命令落到哪个 session，由 `uu-cli` 内部调度。

## 首次绑定与增加 Terminal

官方 Windows UU 没有稳定的外部入口创建第一个 Terminal，因此用户先在官方 UU 中手工打开一个专用 Terminal。按需关闭它自己的官方标签页以释放连接，然后在目标 workspace 的 PowerShell 中由用户本人运行：

```powershell
uu bind
```

`uu bind` 将调用时的当前目录精确保存为 workspace root，即使它位于 Git 仓库内。通常只有一个新出现且可用的 detached Terminal，运行 `uu bind` 就把它明确交给 uu-cli，不弹窗也不再问一次。若候选不唯一，命令直接报告歧义，用户在官方 UU 中整理候选后再运行。首次绑定时，用户也在当前 PowerShell 确认远端工作目录，默认值来自该 Terminal 的 `pwd`。没有可用 Terminal 时，命令说明下一步；用户准备好后再次运行 `uu bind`。

要逐个增加并发 worker：在官方 UU 中再开一个专用 Terminal，按需关闭它自己的标签页，再由用户在同一 workspace 再运行一次 `uu bind`。新 Terminal 唯一可用时会加入同一设备的 pool，并保留 workspace 已记住的 remote root。多个候选时不会猜选；先在官方 UU 中整理为唯一候选。只有用户明确选择更改设备或 remote root 时才更改绑定。

Agent 的正常 `uu <command>` 不会自动绑定、授权或等待新 Terminal。若 workspace 未绑定或没有已授权 worker，命令会提示用户手动运行 `uu bind`。已授权 worker 全忙时，命令排队并报告队列位置，不会取消或 kill 其他 job；状态未知的 worker 会保持隔离。

## 多个 Terminal

同一设备上可以打开多个官方 Terminal：

```text
AIC-Car-1
  terminal 1  busy
  terminal 2  idle
  terminal 3  idle
```

`uu-cli` 自动调度已授权的空闲 worker。长任务持续占用自己的 terminal，并实时把输出返回给调用它的 Codex；其他命令使用其他空闲 worker。已授权 worker 全忙时命令排队并报告位置；仅被发现但未由 `uu bind` 授权的 Terminal 不会自动加入 pool。

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

- 可以刷新新开的官方 Terminal 库存，但发现本身不能授权或接管；
- 空闲时 CPU 接近 0，不高频轮询；
- 无 tray、额外 GUI 或常驻可见 console；
- 不自动弹窗；只有用户手动运行 `uu bind` 时才在当前调用终端进行选择；
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

因此 v1 不注入 GameViewer、不伪造官方客户端、不绕过 host registration。用户用官方 UI 打开 Terminal，并手动运行 `uu bind` 完成选择和授权；之后已授权 Terminal 的调度由 `uu-cli` 处理。

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

Windows 默认状态放在 `%USERPROFILE%\.uu-cli`，不写入 workspace，也不受 Codex 等打包应用的 AppData 虚拟化影响。旧版 `%LOCALAPPDATA%\uu-cli` 中当前上下文可见的绑定、授权及 unknown 记录会自动导入一次，原文件保留；已有共享绑定优先，拒绝记录优先于授权。升级前先等旧任务结束。显式 `UU_CLI_CONFIG` 仍使用指定路径，选择该路径时需保证各调用方能访问同一份文件。用户运行 `uu list` 可查看实际配置路径。

没有常驻发现 watcher 或开机启动项。用户运行 `uu bind` 时前台刷新设备/Terminal 列表并在当前调用终端完成选择和授权；普通命令只使用已授权 worker，必要时按需建立附件连接。worker helper 空闲五分钟后释放；退出官方 UU 后，尚未确认完成的命令仍按 unknown 处理，不自动重放。旧版 watcher、授权弹窗实测和资源采样见 [TESTING.md](TESTING.md)。
