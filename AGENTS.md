# AGENTS.md

## Project intent

`uu-cli` 把 Windows 本地已登录的网易 UU 远程变成一个几乎透明的远端 shell + 文件通道。

实现前先读：

1. `README.md`
2. `DESIGN.md`
3. `ARCHITECTURE.md`
4. `IMPLEMENTATION.md`
5. `ROADMAP.md`

研究资料只作为证据，不覆盖产品契约。

## Public contract

日常用户/Agent 只需要：

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

不要重新引入 `devices/use/session/new/attach/kill` 作为日常公共模型。

## Final v1 model

```text
Local Workspace
      ↓
Remote Binding = device identity + remote root
      ↓
Terminal Worker Pool
```

关键规则：

- 一个 workspace v1 只绑定一个远端设备；
- Codex 不选择 deviceId；
- 用户通过官方 UU GUI 手工打开 Terminal；
- uu-cli 自动发现已有 session，不自动点击 GUI 创建第一个 session；
- Terminal 是 worker，不是 Agent 的长期上下文；
- 一个 terminal 同时只能执行一个 job；
- 长任务一直 lease 该 terminal并实时输出；
- 其他命令使用其他 idle worker；
- worker 全忙则排队；
- 不跨设备调度。

## Shell context — HARD RULE

**不同 `uu` 调用之间不得依赖 shell-local 状态。**

不要让实现或测试假设下面这种代码成立：

```powershell
uu cd src
uu source .venv/bin/activate
uu python main.py
```

正确：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每个普通 job 都从 workspace binding 的 `remote_root` 开始。

后续 Codex Skill 必须明确写出该规则。

## Workspace binding

- 已有 binding：从 cwd 向上使用最近的 binding；
- 新 binding：Git root 优先，否则 cwd；
- 不往 repo 写 `.uu` 等配置；
- 配置放 `%LOCALAPPDATA%\uu-cli`；
- 多设备同时有 session 时，只给用户弹一次短 PowerShell 选择；
- 内部保存稳定 device identity，UI 展示 friendly device name；
- 绝不根据相似名称猜设备。

## Terminal bootstrap and discovery

当前官方 Windows UU 4.43.x 的结论已经收口：

- 已有 terminal 可以查询/附着/复用；
- 第一个 terminal 的 host registration 只在官方 GUI launcher 内；
- `windows_terminal -> new_console -> detached_process` 没有稳定外部 launcher 入口；
- v1 不继续研究注入、伪造签名、host-registration 绕过。

没有 session 时，第一次 `uu` 可以等待用户打开官方 Terminal；session 出现后原命令继续。

新 session 自动发现优先使用 Windows 进程/事件触发；fallback polling 必须退避，禁止永久高频 spawn 官方 CLI。

不要假设设备上的所有 terminal 都可被 uu-cli 使用；必须有 adopt/allow 语义，确保用户手工保留的 terminal 不被抢。

## Job semantics

官方 terminal 是 PTY 字节流，没有客户端逐命令 exec/exit-code API。

实现必须：

- 每 job 生成随机 nonce；
- 使用 completion marker 获取命令边界 + `$?`；
- 流式解析 marker，允许跨 frame 边界；
- marker 不得泄漏到用户输出；
- 不靠 prompt 文本猜结束；
- stdout/stderr 可能合流，不伪造分离；
- 命令一旦已经写入 PTY，连接异常时绝不自动在另一个 worker 重放；
- 无法确认结果时返回 unknown/may-still-be-running，而不是猜成功或自动重跑。

## Broker

v1 允许轻量用户级 broker，因为它解决：

- 自动发现 Terminal；
- 多 Codex 并发；
- worker busy/idle；
- 队列；
- PowerShell 选择提示。

建议一个 `uu.exe`，内部隐藏 `--internal-broker` 模式。

要求：

- 单实例；
- user-scoped named pipe；
- 无可见 console / tray / GUI；
- UU 未运行、无 terminal/job 时尽量只等待；
- 不保持无意义连接；
- `uu` 发现 broker 不在时可以自行拉起；
- broker 崩溃不能破坏官方 UU。

不要为了追求“进程绝对为 0”去 hook/修改官方 UU。

## Preferred implementation

v1 建议 Rust stable。保持依赖少：Win32/ConPTY/named pipe/process monitor 通过 `windows` crate；配置用 serde；没有真实需要不要引入重量级异步框架。

推荐模块和 milestone 见 `IMPLEMENTATION.md`，不要另起一套架构。

## Windows UU integration

第一版 session 管理优先复用已验证的官方 `uuyc-cli term ... ls --json` 等管理能力。

注意：

- 官方 CLI 需要从真实安装环境运行；复制出来的单文件可能无法连接主程序；
- 不硬编码用户自己的路径/device ID/token；
- 不把真实 local-host token、账号信息或抓包凭据写入仓库；
- 只有官方 wrapper 明确成为瓶颈时才考虑更底层协议。

## Authentication / integrity boundary

禁止：

- 提取/保存 UU 密码、Token、Cookie；
- 认证绕过；
- GameViewer 注入；
- 官方客户端签名伪造；
- 修改官方二进制；
- 提交受版权保护的大段反编译代码。

允许记录最小互操作行为、消息结构和兼容性结论。

## File transfer

优先研究官方原生文件通道和本地入口。不要未经验证就把 terminal + base64 固化成架构。

公共接口始终：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

必须二进制安全、明确失败、可验证完整性。

## Development ownership

### Local Codex = primary implementer

本地 Codex 负责所有需要真实 Windows + 官方 UU 的工作：

- Rust scaffold 落地；
- Win32 / ConPTY / named pipe / process watch；
- uuyc-cli interop；
- session attach；
- 多 worker；
- 长任务/Ctrl+C/重连；
- installer；
- 实机验收。

按 `IMPLEMENTATION.md` milestone 分 PR，每个 PR 写真实测试结果。

### Cloud ChatGPT = architect/reviewer

- 维护 DESIGN/ARCHITECTURE/IMPLEMENTATION/ROADMAP；
- review PR；
- 发现实测与 spec 冲突时更新设计；
- 不假装拥有用户本地 UU 环境；
- 不与本地 Codex 同时大改同一实现模块造成冲突。

## Scope discipline

v1 不扩：

- 一个 workspace 多设备；
- MCP / Web UI / tray；
- SSH 作为产品依赖；
- Git/Python/CMake 专用命令；
- 用户可见 session 管理；
- 远程桌面 GUI 自动化；
- 自己的一套持久 shell-context 系统。

项目价值来自低学习成本和透明性，不是命令数量。
