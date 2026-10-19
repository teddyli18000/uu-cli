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
- `uu` 必须自己建立或复用远端终端；不要求用户/Agent 先打开 UU Terminal。
- 不要求用户每次指定设备名 / ID；日常使用针对一个已配置的默认目标。
- 默认目标不可用时明确失败，绝不自动切换到另一台设备。
- 主控 Windows 上运行时不弹 console、GUI、托盘、UU 工具窗口。
- 不设计 `start/stop`；后台能力跟随官方 UU 生命周期，或在无需后台组件时保持无常驻。
- session、重连、并发、协议细节全部属于内部实现。
- stdout 尽量只包含远端命令输出，工具自身诊断走 stderr。

## Research is evidence, not architecture

仓库已经包含终端互操作研究：

- `docs/terminal.md`
- `proto/terminal.proto`

其中记录的官方 `uuyc-cli term` 能力、TERM 帧、RPC、session 行为等可以作为实现证据，但 **不等于最终实现必须采用某一路线**。

必须继续区分：

- **confirmed**：有直接证据；
- **inferred**：合理推断但未完成证据链；
- **unknown**：需要实机验证。

研究文档可以继续扩展，但不要把推断伪装成产品事实。

## Do not assume UU internals

Windows UU 内部实现仍在实机分析中。不要提前锁死：

- 必须使用 `uuyc-cli.exe`；
- 必须直接实现 TERM 帧；
- 必须使用某个命名管道 / IPC；
- 必须自己实现某个网络协议；
- 必须常驻 daemon；
- 文件传输必须走 terminal/base64；
- session 必须采用某种特定复用方式。

先观察当前安装版本的真实行为，再选择最小、最稳定的实现。

优先复用官方 UU 已有的本地状态和能力；不要为了“架构完整”重复实现已经存在的东西。

## Authentication boundary

必须保持：

- 依赖官方 UU 正常安装、运行和登录；
- 不提取、导出或持久化 UU 密码、Token、Cookie、账号密钥；
- 不实现认证绕过；
- 不把私有 device ID、账号信息、抓包凭据写进仓库或日志；
- 不把官方二进制、受版权保护的反编译大段内容提交到仓库。

允许记录为互操作所需的最小接口行为、消息结构结论和兼容性发现，但要保持可审计、可复现，并避免包含用户秘密。

## Implementation rule

**当前阶段先立目标、做研究，不要因为仓库已经有协议文档就直接开始堆运行代码。**

只有具体实现任务明确后才开始写运行代码。实现时遵循：

1. 先验证 Windows UU 当前版本的实际接口；
2. 用最小原型证明 `uu <command>` 可以在无 GUI 操作下自己建立/复用终端；
3. 用独立最小原型证明 `put/get` 的可靠文件传输路径；
4. 保持 `uu / put / get` 公共契约不变；
5. 再处理多设备默认目标、生命周期、安装器和兼容性；
6. v1 验收通过前不扩功能。

任何原型都只是用于验证内部路线，不应该迫使用户学习原型的参数、session 或协议概念。

## Terminal behavior

最终必须满足：

```text
uu <command>
  → 自动选择默认设备
  → 自动建立或复用远端终端
  → 把命令交给远端 shell
  → 返回可消费的输入/输出/退出状态
```

顺序任务优先复用已有合适会话；真实并行任务允许多个会话。具体策略由证据和可靠性决定。

不要把 `new/attach/ls/kill/session_id` 等内部终端概念暴露成日常公共接口。

## Background lifecycle

如果实现需要后台 helper：

- 它必须静默；
- 不创建用户可见窗口；
- 不作为与 UU 无关的永久后台服务存在；
- 官方 UU 启动后它应自动可用；
- 官方 UU 完全退出后它应退出；
- 生命周期联动如何实现由实机证据决定。

不要为了方便实现而要求用户手工启动第二个服务。

## Device selection

v1 允许账号下存在多台设备，但日常命令始终面向一个默认目标。

- `uu devices`：查看设备和当前目标；
- `uu use <device>`：显式切换默认目标；
- 首次配置多设备时必须让用户明确选择；
- 目标离线/失效时不能自动选择其他在线设备；
- 设备身份匹配策略必须避免“名字相似就猜”的危险行为。

## Installer / release target

最终 Release 目标是一个 Windows 安装程序 EXE，例如：

```text
uu-cli-setup.exe
```

它负责安装 `uu`、PATH、必要后台集成、首次目标配置和卸载入口。默认优先 user-scope；卸载不能破坏官方 UU Remote 或其登录状态。

安装/升级/卸载必须可重复、可恢复，不留下孤儿进程、PATH 垃圾或官方 UU 文件修改残留。

## Scope discipline

除非任务明确要求，不要加入：

- MCP；
- Web UI；
- tray app；
- SSH 作为产品依赖；
- Git/Python/构建系统专用命令；
- 面向用户的 session 管理；
- 远程桌面 GUI 自动化；
- “为了以后可能有用”的抽象层。

项目价值来自 **低学习成本和透明性**，不是命令数量。

## Definition of done for v1

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

- 不要求手工打开 UU Terminal；
- 主控端无额外窗口；
- 输出可被 Agent 直接消费；
- 文件字节一致；
- 长任务可用；
- 默认目标离线时不会误执行到别的设备；
- UU 完全退出后无独立后台残留；
- 卸载不影响官方 UU。
