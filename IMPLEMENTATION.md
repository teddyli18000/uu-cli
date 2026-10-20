# Implementation blueprint

这份文档把 `ARCHITECTURE.md` 变成可以直接执行的工程任务。原则：先做最小闭环，再补完整性；任何内部复杂度都不能泄漏到用户体验。

## 1. 技术选择

v1 建议使用 **Rust stable**。

原因：

- 单文件原生 Windows 可执行程序；
- Win32 / ConPTY / named pipe / process monitoring 都可直接调用；
- 低空闲占用，适合轻量 broker；
- 类型系统适合处理 session/job/IPC 状态，减少并发误用；
- installer 可继续独立选择 Inno Setup / WiX，不影响核心代码。

依赖尽量克制。优先标准库 + `windows` crate + `serde/serde_json` + 小型错误处理/CLI 参数库；除非真实需要，不引入重量级异步框架。

## 2. 一个二进制，两种内部角色

对用户只安装：

```text
uu.exe
```

公开调用：

```powershell
uu <command...>
uu put ...
uu get ...
uu bind
uu list
```

内部允许隐藏模式：

```text
uu.exe --internal-broker
```

这个参数不属于公共 API，不写进用户文档。

CLI 负责：

- 解析当前 cwd；
- 找 workspace binding；
- 把 job 请求发给 broker；
- 转发 stdin/stdout；
- 返回远端 exit code 或本地错误。

Broker 负责：

- 单实例；
- UU 进程状态；
- session discovery；
- workspace/config；
- terminal worker pool；
- busy/idle/queue；
- attach/PTY；
- completion marker；
- 输出流回传；
- 必要的 PowerShell 交互提示。

## 3. 推荐代码结构

```text
src/
  main.rs
  cli.rs
  broker.rs
  ipc.rs
  config.rs
  workspace.rs
  uu/
    mod.rs
    install.rs
    devices.rs
    sessions.rs
    terminal.rs
  scheduler.rs
  job.rs
  marker.rs
  prompt.rs
  process_watch.rs
  file_transfer.rs   # Phase 2 前可以只有接口/placeholder
```

不要建立大量抽象层。每个模块都应该能回答一个清晰问题。

## 4. 数据模型

只保存最小状态。

```text
WorkspaceBinding
  local_root
  device_identity
  device_name
  remote_root

Worker
  device_identity
  session_name / validated internal session reference
  state = idle | leased | unavailable

Job
  id
  workspace_root
  device_identity
  command
  worker
  state = queued | sent | running | finished | unknown
```

不要持久化 busy 状态。Broker 重启后重新发现 session，并把不确定的旧 job 视为不可自动重放。

## 5. Workspace lookup

实现顺序：

1. 从 cwd 向上查本地配置中的最近 binding；
2. 找到即使用；
3. 没找到时计算建议 root：Git root 优先，否则 cwd；
4. 进入自动 bind 流程。

绑定信息保存在 `%LOCALAPPDATA%\uu-cli\config.json` 或等价用户配置文件。

配置更新必须原子写入：写临时文件后 rename/replace，避免崩溃留下半个 JSON。

## 6. Session discovery

第一版优先复用已经验证的官方 `uuyc-cli term ... ls --json` 管理面，不提前重写底层协议。

要求：

- 自动定位官方安装目录中的 CLI；
- 不假设把 `uuyc-cli.exe` 复制到项目目录还能工作；
- 解析本地 JSON wrapper；
- 至少取得 device identity、device name、session name/state；
- 发现/解析失败要区分“UU 未运行”“未登录/设备不可用”“没有 session”“官方版本不兼容”。

后续只有当官方 CLI 包装成为明确瓶颈时才考虑更底层路径。

## 7. 第一次 bind UX

### 情况 A：当前没有可用官方 Terminal

普通命令例如：

```powershell
uu git status
```

应进入等待/提示：

```text
uu-cli · D:\projects\aic
当前没有可用的 UU Terminal。
请在网易 UU 远程中打开目标设备的“终端远控”。
等待中…  Ctrl+C 取消
```

Broker 在等待期间低成本监听/刷新。一旦 session 出现继续流程，不要求用户重新执行命令。

### 情况 B：只发现一台设备

显示最短确认：

```text
发现 AIC-Car-1 · 3 terminals
绑定到 D:\projects\aic ? [Enter]
```

然后从一个 terminal 获取 `pwd`：

```text
远端目录 /home/pi/aic  [Enter 确认 / 输入修改]
```

完成 binding 后继续原来的 job。

### 情况 C：发现多台设备

```text
发现多个远端设备：
  [1] AIC-Car-1   3 terminals
  [2] AIC-Car-2   2 terminals
选择 [1]:
```

只让用户看 friendly name 和数量；内部保存稳定 identity。

## 8. 新 Terminal 的自动发现

Broker 需要尽量无感地发现用户后来手动打开的新官方 Terminal。

优先策略：

1. 监听官方 `uuyc-cli.exe` / 相关 terminal host 进程启动事件；
2. 事件出现后刷新 session list；
3. 如果 Windows 事件监听在某版本不稳定，再使用低频 + 指数退避 polling；
4. 不允许每秒永久 spawn 官方 CLI。

发现新 session：

- 已绑定设备、且用户允许该设备自动加入 → 自动加入 worker pool；
- 否则弹一次简短 PowerShell 提示：

```text
AIC-Car-1 新终端 bash-4
[Enter] 加入 uu workers    [S] 跳过
```

必要时用官方 rename 为 worker 加内部唯一标识；如果 rename 在真实测试中会影响用户体验，则只保存在本地 registry/config，并重新评估稳定识别方式。

## 9. Scheduler

Scheduler 只做三件事：

1. 按 workspace binding 找到正确 device pool；
2. 找 idle worker；
3. lease 到 job 完成。

没有 idle worker：FIFO 排队。

不能：

- 抢占正在运行的命令；
- 把同一个 worker 同时给两个 job；
- 自动把 job 切到别的 device；
- 已发送后因为网络错误自动重放。

## 10. Command wrapping

普通命令必须每次从 `remote_root` 开始。

概念模板：

```bash
cd -- '<remote_root>' && { <user-command>; }; __uu_emit_marker <nonce> "$?"
```

实际实现必须正确处理 shell quoting；不要直接字符串拼接未经转义的路径/nonce。

completion marker：

- 每 job 随机 nonce；
- 不能与普通输出轻易碰撞；
- 流式解析，marker 可以跨 frame 边界；
- 返回给用户前剥离；
- marker 内的 status 映射为本地进程 exit code。

用户命令不能因为 wrapper 而被执行两次。

## 11. 输出与长任务

PTY 输出实时流回调用 `uu` 的进程。

- 不等整个命令结束后一次性返回；
- 不尝试伪造 stdout/stderr 分离；
- 长任务 worker 持续 leased；
- 另一个 job 使用其他 idle worker；
- Ctrl+C 第一目标是中断当前远端前台 job，不是杀 broker；
- 如果 attach/连接断掉且不知道命令结果，job 标记 `unknown`，不自动重跑。

## 12. 本地 IPC

CLI ↔ broker 使用当前用户作用域 Windows named pipe。

协议保持小：

```text
hello/version
run_job
stdin
cancel
stdout_chunk
job_exit
error
bind/list control
```

要求：

- 带 protocol version；
- 限制同一登录用户；
- frame 有明确长度，不用 newline 猜边界；
- 输出流允许 backpressure；
- broker 挂掉时 CLI 给短错误并可重启一次 broker。

## 13. Broker lifecycle / 占用

用户希望“UU 开着时自动可用，没事时尽量没有占用”。在不修改官方 UU 的约束下，推荐：

- 安装一个 user-level auto-start broker/watcher；
- broker 没有 console/tray；
- UU 不运行：只等待进程事件/低频检查；
- UU 运行但没有 terminal/job：不 attach，不保持无意义网络连接；
- 有 terminal/job：才建立需要的管理/terminal 连接；
- `uu` 被调用但 broker 不在：CLI 自己拉起 broker；
- 后续如果找到可靠、不修改 UU 的启动事件机制，可以再把常驻 watcher 去掉。

不要为了追求“进程绝对为 0”而牺牲自动发现；也不要用高频轮询换自动感。

## 14. `uu list`

只做诊断：

```text
Workspace  D:\projects\aic
Remote     AIC-Car-1 · /home/pi/aic
Workers    3

  1   busy   python3 train.py
  2   idle
  3   idle
```

可额外显示未接纳 session，但不要暴露协议字段或要求用户操作 session id。

## 15. Agent Skill contract

Skill 必须极短，只告诉 Agent：

```text
远端执行：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

并明确一条硬规则：

> 不要依赖不同 `uu` 调用之间的 shell-local 状态。需要 `cd` / `source` / `export` 连续生效时，把它们放在同一条 shell 命令中。每次普通 `uu` 调用都从该 workspace 绑定的 remote root 开始。

Agent 不选择 deviceId、不选择 session、不管理 worker。

## 16. 实施顺序

### M0 — repository scaffold

- Rust workspace / build；
- CLI 参数；
- config/workspace 模块；
- named pipe skeleton；
- unit tests。

### M1 — discovery + bind

- 找官方 UU/uuyc-cli；
- `term ls --json`；
- workspace root；
- device grouping；
- first-bind PowerShell UX；
- `uu list`。

### M2 — one worker shell

- attach 一个已有 session；
- run `pwd`；
- marker + exit code；
- streaming output；
- Ctrl+C；
- no-replay error semantics。

### M3 — worker pool

- 多 terminal discovery；
- adopt/ignore；
- idle/busy lease；
- 多并发 `uu`；
- queue；
- long-running jobs。

### M4 — file transfer

先完成原生文件入口研究，再实现 `put/get`。不要提前把 base64 固化成架构。

### M5 — lifecycle + installer

- broker auto-start / low-idle watcher；
- recovery；
- user-scope installer；
- PATH；
- uninstall；
- version compatibility diagnostics。

## 17. 开发职责

建议明确分工，避免两个 Agent 同时改同一块：

### Cloud / ChatGPT

- 维护产品与架构文档；
- 维护任务拆分和验收标准；
- review 每个 PR；
- 做不依赖真实 Windows UU 的核心设计、测试策略和代码审查；
- 根据本地实测结果及时修改 spec。

### Local Codex

作为主要实现者：

- 在真实 Windows + 官方 UU 环境开发；
- 实现 Win32 / ConPTY / process watcher / named pipe / uuyc-cli interop；
- 跑真实 session/并发/长任务/重连测试；
- 构建 installer 并在本机安装/卸载验证；
- 每个 milestone 单独 PR，附真实测试结果。

这样云端不假装拥有本地 UU 环境，本地 Codex 也不临时改产品设计。实现严格以 `DESIGN.md`、`ARCHITECTURE.md`、本文件和 `AGENTS.md` 为准。
