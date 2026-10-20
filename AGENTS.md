# AGENTS.md

## Project intent

`uu-cli` 的目标不是“再做一个远程管理平台”，而是把 **Windows 本地已登录的 UU Remote** 变成一个几乎透明的远端 shell + 文件通道。

先读 `README.md`、`DESIGN.md` 和 `ROADMAP.md`。公共体验优先于内部实现。

## Public contract

正常用户只需要记住：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

多设备只保留最小配置入口：

```powershell
uu devices
uu use <device>
```

不要在没有明确真实需求时增加新的顶层概念、子命令或管理界面。

关键约束：

- `uu <command...>` 后面的业务命令尽量原样交给远端 shell。
- v1 假定用户已经通过官方 UU GUI 为目标设备打开一个可复用 terminal session。
- `uu-cli` 负责发现、附着、复用已有 session；不要求用户/Agent 管理 session id/name。
- 当前没有可用 session 时，明确提示用户在 UU 中打开一次 Terminal；不要自动点击 GUI。
- 不要求用户每次指定设备名 / ID；日常使用针对一个已配置的默认目标。
- 默认目标不可用时明确失败，绝不自动切换到另一台设备。
- 主控 Windows 上 `uu-cli` 自己不弹 console、GUI、托盘或辅助窗口。
- 不设计 `start/stop`；如果需要后台能力，它不得独立于官方 UU 长期常驻。
- stdout 尽量只包含远端命令输出，工具自身诊断走 stderr。

## Research status

Terminal 研究对 v1 已基本收口。主要证据位于：

- `docs/terminal.md`
- `docs/uu-cli-blockers.md`
- `docs/uu-cli-pr4.md`
- `docs/uu-cli-pr5.md`
- `docs/uu-cli-pr6.md`
- `proto/terminal.proto`

已确定：

- 官方 terminal 是持续 PTY 字节流；
- 外部工具可以管理/附着已有 session；
- 官方协议没有逐命令 exec/逐命令 exit-code API；
- GUI 创建 terminal 前会在 GameViewer 主程序内注册 local host/token；
- `windows_terminal -> new_console -> detached_process` 是同一个 GUI launcher 链；
- 当前没有可复用的 CLI / IPC / deeplink headless launcher 入口。

不要再为了“自动创建第一个 terminal”继续投入注入、伪造签名、绕过 host registration 等路线。除非官方客户端未来出现新的稳定外部接口，否则 v1 接受人工 Terminal bootstrap。

研究结论仍需区分 **Confirmed / Observed / Inferred / Not tested**；不要把推断伪装成产品事实。

## Implementation rule

当前可以开始最小 shell 原型，但严格限制范围：

1. 发现当前默认设备的已有 terminal session；
2. 可靠 attach / reuse；
3. 用内部随机 nonce completion marker 划分一次 `uu` 调用并取得 `$?`；
4. marker 不得泄漏到用户输出；
5. 验证长任务、Ctrl-C、session 失效和错误语义；
6. 同时单独研究 `put/get` 的原生文件传输路径；
7. 设备稳定身份字段单独验证；
8. shell + file + device 基础闭环之后再处理安装器和生命周期。

任何原型都只是内部实现，不应该迫使用户学习原型参数、session 或协议概念。

## Terminal behavior

v1 正常流程：

```text
用户在官方 UU 中打开一次目标设备 Terminal
  → uu 发现当前默认设备
  → 发现并附着/复用已有 terminal
  → 把命令交给远端 shell
  → 用内部 marker 判断本次命令完成和 exit code
  → 返回可消费输出
```

没有可用 session 时：

```text
uu ...
  → stderr: 当前目标没有可用的 UU Terminal；请在官方 UU 中打开一次 Terminal
  → 非零退出
```

不要：

- 自动操纵 UU GUI；
- 暴露 `new/attach/ls/kill/session_id` 等内部概念给日常用户；
- 用 prompt 文本猜命令结束；
- 假设 stdout/stderr 可以从 PTY 字节流中可靠分离；
- 多个候选 session 不确定时随便猜一个。

## Authentication / integrity boundary

必须保持：

- 依赖官方 UU 正常安装、运行和登录；
- 不提取、导出或持久化 UU 密码、Token、Cookie、账号密钥；
- 不实现认证绕过；
- 不注入 GameViewer；
- 不伪造官方客户端签名；
- 不修改官方 UU 二进制；
- 不把私有 device ID、账号信息、真实 local-host token、抓包凭据写进仓库或日志；
- 不提交受版权保护的反编译大段内容。

允许记录互操作所需的最小接口行为、消息结构和兼容性结论。

## Device selection

v1 允许账号下存在多台设备，但日常命令始终面向一个默认目标。

- `uu devices`：查看设备和当前目标；
- `uu use <device>`：显式切换默认目标；
- 首次配置多设备时必须让用户明确选择；
- 目标离线/失效时不能自动选择其他在线设备；
- 设备身份匹配策略必须避免“名字相似就猜”；
- 稳定 ID/字段等实现细节必须由实机证据决定。

## File transfer

这是当前优先研究项之一。

优先研究官方 Windows UU 原生文件通道和本地入口。不要未经验证就把 terminal + base64 写成正式架构；如果最终需要 fallback，也必须保持：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

并保证二进制安全、明确失败、可验证完整性。

## Background lifecycle

优先无常驻设计。如果实现需要 helper：

- 必须静默；
- 不创建用户可见窗口；
- 不作为与 UU 无关的永久后台服务存在；
- 官方 UU 完全退出后应退出；
- helper 崩溃不能破坏官方 UU。

不要为了方便实现而要求用户手工启动第二个服务。

## Installer / release target

最终 Release 目标是一个 Windows 安装程序 EXE，例如：

```text
uu-cli-setup.exe
```

它负责安装 `uu`、PATH、必要后台集成、首次目标配置和卸载入口。默认优先 user-scope；卸载不能破坏官方 UU Remote 或其登录状态。

Terminal bootstrap 不由安装器自动点击 GUI；首次执行发现没有 session 时再给出简短提示即可。

## Scope discipline

除非任务明确要求，不要加入：

- MCP；
- Web UI；
- tray app；
- SSH 作为产品依赖；
- Git/Python/构建系统专用命令；
- 面向用户的 session 管理；
- GUI 自动化；
- GameViewer 注入/签名绕过；
- “为了以后可能有用”的抽象层。

项目价值来自 **低学习成本和透明性**，不是命令数量。

## Definition of done for v1

前置条件：用户已在官方 UU 中为默认目标打开一个可复用 Terminal。

至少验证：

```powershell
uu devices
uu use <target>
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

并确认：

- 不要求用户管理 session；
- 没有 session 时错误清楚，不自动弹 GUI；
- `uu-cli` 自身无额外窗口；
- 命令边界与 exit code 可靠；
- 输出可被 Agent 直接消费；
- 文件字节一致；
- 长任务可用；
- 默认目标离线时不会误执行到别的设备；
- UU 完全退出后无独立后台残留；
- 卸载不影响官方 UU。
