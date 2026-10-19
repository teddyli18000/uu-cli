# AGENTS.md

## Project intent

`uu-cli` 的目标不是“再做一个远程管理平台”，而是把 **Windows 本地已登录的 UU Remote** 变成一个几乎透明的远端 shell + 文件通道。

先读 `README.md` 和 `DESIGN.md`。公共体验优先于内部实现。

## Public contract

正常用户只需要记住：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

不要在没有明确真实需求时增加新的顶层概念、子命令或管理界面。

关键约束：

- `uu <command...>` 后面的业务命令尽量原样交给远端 shell。
- 不要求用户每次指定设备名 / ID；v1 使用一个默认目标。
- 运行时不弹 console、GUI、托盘、UU 工具窗口。
- 不设计 `start/stop`；后台能力跟随官方 UU 生命周期，或在无需后台组件时保持无常驻。
- session、重连、并发、协议细节全部属于内部实现。
- stdout 尽量只包含远端命令输出，工具自身诊断走 stderr。

## Do not assume UU internals

Windows UU 内部实现仍在实机分析中。不要提前锁死：

- 必须使用 `uuyc-cli.exe`；
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

**当前阶段先立目标、做研究，不要因为看到空仓库就直接堆代码。**

只有具体实现任务明确后才开始写运行代码。实现时遵循：

1. 先验证 Windows UU 当前版本的实际接口；
2. 用最小原型证明命令执行 / 文件传输链路；
3. 保持 `uu / put / get` 公共契约不变；
4. 再处理生命周期、安装器和兼容性；
5. v1 验收通过前不扩功能。

## Background lifecycle

如果实现需要后台 helper：

- 它必须静默；
- 不创建用户可见窗口；
- 不作为与 UU 无关的永久后台服务存在；
- 官方 UU 完全退出后，它应退出；
- 生命周期联动如何实现由实机证据决定。

不要为了方便实现而要求用户手工启动第二个服务。

## Installer / release target

最终 Release 目标是一个 Windows 安装程序 EXE，例如：

```text
uu-cli-setup.exe
```

它负责安装 `uu`、PATH、必要后台集成和卸载入口。默认优先 user-scope；卸载不能破坏官方 UU Remote 或其登录状态。

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
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

并确认：无额外窗口、输出可被 Agent 直接消费、文件字节一致、长任务可用、UU 完全退出后无独立后台残留、卸载不影响官方 UU。
