# Architecture

这份文档只定义 `uu-cli` v1 的概念架构。它不规定具体语言、进程模型、IPC、Windows API 或代码目录；实现者应根据真实 Windows + 官方 UU 环境自行选择最简单可靠的方案。

## 1. 核心模型

```text
Local Workspace
      ↓
Remote Binding = device identity + remote root
      ↓
Device-level Terminal Worker Pool
```

对 Codex 来说，唯一稳定概念是当前本地 workspace。设备 identity、session name/id、worker 调度都隐藏在 CLI 后面。

## 2. Workspace

每个 workspace v1 只绑定一个远端设备和一个远端工作根目录：

```text
local_root
→ remote_device
→ remote_root
```

默认行为：

- Git 仓库内以 Git root 作为建议 local root；
- 非 Git 目录采用当前目录或合理的最近根目录；
- 从子目录执行时使用最近的已存在 binding；
- binding 存在用户侧，不向 repo 写 `.uu` 等项目文件；
- v1 暂不处理一个 workspace 同时操作两台远端设备。

同一 remote device 可以绑定多个 local workspace。它们共享设备级 worker pool，但每个 job 都使用自己的 `remote_root`，因此不会因为共享 Terminal 而混淆项目目录。

## 3. Remote device

设备是 Terminal pool 的硬隔离边界：

```text
device A → workers A1 A2 A3
device B → workers B1 B2
```

不同设备的 worker 绝不能混用。

首次绑定时如果多台设备都有可用 Terminal，只让用户通过简短人类选择器选择一次 friendly name；内部保存经过验证的稳定 identity。Codex 不直接选择 deviceId。

## 4. Terminal worker pool

官方 UU Terminal 由用户自己在官方 UU 中打开。`uu-cli` 自动发现已经存在的 Terminal，并把被用户允许使用的 Terminal 视为 worker。

```text
device A
├─ worker 1  idle
├─ worker 2  busy
└─ worker 3  idle
```

规则：

- pool 属于 device，不属于某个 workspace；
- 一个 worker 同时只属于一个正在运行的 job；
- 长任务持续占用原 worker，并持续把输出返回给调用方；
- 其他命令可调度到其他 idle worker；
- worker 全忙时等待；
- 不抢占、不串流、不跨设备；
- worker/session 的内部名字不要求用户记忆。

## 5. Terminal 发现与人类授权

用户打开新官方 Terminal 后，`uu-cli` 应尽快自动发现，但**发现不等于可以静默接管**。

### 5.1 有请求正在等待

如果 `uu` 已经明确告诉用户“请打开目标设备 Terminal；新打开的 Terminal 将用于本次请求”，那么用户随后打开匹配 Terminal 的动作本身就是本次授权。

如果 workspace 尚未绑定，而用户被提示“打开你想使用设备的 Terminal”，等待期间唯一新出现的 Terminal 可以同时完成：

```text
identify device
→ authorize worker for this pending request
→ collect/confirm remote root
→ continue original command
```

不要让用户重新运行原命令，也不要再弹第二个“是否交给 uu-cli”确认。

若同时出现多个候选设备/Terminal，则只弹一次短选择，不能猜。

### 5.2 没有请求正在等待

新 Terminal 必须经过一次极短授权：

```text
AIC-Car-1 新终端已打开
[Enter] 交给 uu-cli    [N] 保留手动使用
```

- Enter 后加入 device pool；
- `N`、Esc、关闭或取消都视为不授权；
- v1 不提供“以后这个设备所有新 Terminal 都自动接管”的永久策略；
- 多个 Terminal 短时间出现时应在同一个交互窗口里顺序/批量处理，避免多个窗口抢焦点。

### 5.3 不存在“同设备多个 workspace 的 Terminal 归属问题”

同设备多个 workspace 共享同一 worker pool，job 自己携带 `remote_root`。因此新 Terminal 不需要再问“属于哪个 workspace”。

## 6. Interaction channel

人类配置/授权和远端命令数据流必须分离。

```text
Codex / shell ── uu stdin/stdout ── remote job
                       │
                       └── human decision → short separate interaction when required
```

要求：

- 正常 `uu` stdout 不混入选择菜单；
- 工具诊断走 stderr；
- 选择器不能消费原命令 stdin；
- 被 pipe/Agent 调用时，也不能因为需要人选择而把菜单文本伪装成远端输出；
- 当前环境无法安全展示交互时，不猜选择结果；
- 不依赖 PowerShell 7 必须存在：可以优先使用 `pwsh`，但必须能回退系统自带 Windows PowerShell 或项目自己的最小原生交互。

## 7. Shell-state isolation

Terminal 是复用的 worker，但不是持久 workspace context。

不同 `uu` 调用之间不能依赖：

- `cd`；
- `export`；
- `source` / virtualenv；
- alias；
- shell option；
- 其他 shell-local 状态。

普通 job 概念上应当：

```text
enter isolated shell scope
→ cd remote_root
→ execute user command
→ collect exit status
→ leave scope
```

这样上一条命令的 `cd/export/source/exit/exec` 不应成为下一条 job 的隐式输入。

具体用 subshell、fresh shell 或其他机制由实现者根据真实 PTY/交互行为决定。

持续交互/长任务在自己的生命周期内保持同一个 worker；job 结束后不把其 shell-local 状态作为下一条命令的契约。

## 8. Job

一次 `uu <command>` 对应一次 job。

概念流程：

```text
resolve workspace binding
→ choose one idle worker on bound device
→ enter job-local shell scope at remote_root
→ stream output in real time
→ determine completion + exit status
→ release worker
```

关键安全语义：

- 命令一旦已经发送到远端，不因为连接异常自动换 worker 重跑；
- 如果无法确认结果，明确报告 unknown / may still be running；
- 不使用 shell prompt 文本作为唯一命令完成判断；
- PTY stdout/stderr 合流时保持事实，不虚构分离。

## 9. Crash / recovery

“后台进程重启后把所有 session 当 idle”是不安全的。

如果 worker 在后台组件崩溃前可能有 job 正在运行：

```text
worker → unknown / quarantined
```

只有当实现能够可靠证明旧 job 已经结束时，才重新放回 idle pool。

不能证明时，允许：

- 等待可证明的完成信号；
- 让用户关闭/重新打开该 Terminal；
- 使用其他新 worker。

但不能向一个可能仍在运行旧命令的 PTY 注入新命令，也不能重新发送旧命令。

## 10. File transfer

`put/get` 绑定到当前 workspace 的 remote device 和 remote root：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

远端相对路径相对于 `remote_root`；绝对路径保持绝对语义。若支持 `~`，必须按远端用户环境稳定解析，不能落到本地 Windows 语义。

文件通道不要求与 Terminal 共用同一种内部机制，但必须：

- 二进制安全；
- 完成/失败明确；
- 不完整不能报告成功；
- 不暴露底层协议给用户或 Agent。

## 11. 自动可用与低占用

产品要求：

```text
官方 UU 正常使用
→ uu-cli 随时可以响应
→ 用户打开 Terminal 后自动发现
→ 没任务时尽可能不占资源
```

实现者可以选择 broker、watcher、事件通知、lazy process 或其他结构。概念架构只要求：

- 用户不手工管理第二个服务；
- 新 Terminal 能自动被发现；
- 空闲 CPU 接近 0；
- 不永久高频 polling/spawn 官方组件；
- 不需要时不保持昂贵连接；
- 无 tray、额外 GUI 或常驻可见 console；
- 需要用户决定时可以弹短人类交互；
- 后台能力失效后，下一次 `uu` 可以恢复或给出明确错误。

“随 UU 一起可用”是产品语义，不要求通过 hook/patch 强行让两个进程严格同生共死。

## 12. Official UU isolation

`uu-cli` 必须把官方 UU 当作外部事实来源，而不是需要改造的宿主。

边界：

- 不修改、替换、patch、inject 官方二进制；
- 不改登录、设备、更新、启动配置；
- 不保存认证秘密；
- 不 kill/rename/close 用户未授权的 session；
- 不改变用户正在手工操作的 Terminal；
- 不让自己的崩溃/卸载影响官方 UU；
- 如果某种 attach/reuse 会影响官方 UI，只能用于用户已经明确交给 uu-cli 的专用 Terminal，或者换更安全路径。

## 13. Portability

架构不能依赖开发机偶然状态。

应当允许：

```text
另一台官方 UU 正常支持的 Windows 电脑
+ 官方 UU Remote 已安装
+ 安装 uu-cli
→ 直接完成首次 binding 并使用
```

因此设备 identity、安装路径、用户目录、盘符、session 名、UU 版本差异都必须通过运行时发现/兼容性逻辑处理，而不是写死在代码里。

安装后不应要求 Python/Rust/Node/PowerShell 7/WSL 等开发运行时。

## 14. Validation architecture

验证也遵循分层：

```text
pure logic / packaging / clean install
        ↓ GitHub-hosted Windows CI
official UU install/discovery smoke
        ↓ CI when environment supports it
real login / real Terminal / real remote device
        ↓ minimal real-machine integration test
```

CI 不应伪装成真实登录后的 UU 环境；真实环境也不应承担本可以交给 CI 的反复构建、安装、卸载和故障注入。

详见 `TESTING.md`。

## 15. User-visible surface

日常：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

首次配置/诊断：

```powershell
uu bind
uu list
```

保留词冲突时使用：

```powershell
uu -- <remote command...>
```

`uu list` 可以展示：

```text
Workspace  D:\projects\aic
Remote     AIC-Car-1 · /home/pi/aic
Workers    3

  worker 1   busy   python3 train.py
  worker 2   idle
  worker 3   idle
```

这里 worker 名称只用于诊断，不构成 Agent API。

## 16. v1 非目标

- 一个 workspace 同时驱动多台设备；
- 用户手工管理 session id；
- 持久 shell-context 系统；
- 自动点击 UU GUI 创建 Terminal；
- 注入/签名伪造/认证绕过；
- MCP / Web / tray；
- Git/Python/CMake 专用远程 API。

任何内部设计如果需要新增大量用户概念才能成立，默认应该重新考虑。