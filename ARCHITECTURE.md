# Architecture

这份文档定义 v1 的实现模型。目标是让 Codex 只知道 `uu <command>`，把设备、UU terminal、并发调度和 Windows 侧细节全部藏在 CLI 后面。

## 1. 核心模型

v1 只有三个真正的概念：

```text
Local Workspace
      ↓
Remote Binding = device identity + remote root
      ↓
Terminal Pool = 该设备上用户通过官方 UU 手工打开、并交给 uu-cli 使用的 terminal sessions
```

对 Codex 来说只有第一层是可感知的：它在某个本地 workspace 中运行命令。

`deviceId`、session name/id、具体用了哪个 terminal 都是内部信息，不进入日常命令。

## 2. Workspace

### 2.1 根目录

每次 `uu` 从当前工作目录向上寻找最近的已存在 workspace binding。

首次建立 binding 时：

1. 当前目录位于 Git 仓库内：默认使用 Git root；
2. 否则：默认使用当前目录；
3. binding 只写入用户配置目录，不往仓库写 `.uu` 等文件。

允许以后显式重新绑定，但普通 Agent 不需要知道这个过程。

### 2.2 Remote binding

一个 workspace v1 只绑定一个远端设备：

```text
local_root
→ device_identity
→ remote_root
```

`remote_root` 是每次普通 job 的确定起点。首次绑定时优先从用户刚打开并选中的官方 UU Terminal 执行 `pwd` 获取，并让用户以最少交互确认/修改。

v1 暂不设计“一个 workspace 同时绑定两台远端设备”。真实需求出现后再扩展，不提前污染普通语法。

## 3. Terminal pool

Terminal 不是 workspace 上下文，也不是 Agent 身份；它只是某台远端设备上的执行槽位。

```text
device A
├─ terminal 1  idle
├─ terminal 2  busy
└─ terminal 3  idle
```

规则：

- Terminal 必须由用户通过官方 UU GUI 手工创建；uu-cli 不创建第一个官方 session。
- uu-cli 自动发现已有 session，并按官方返回的 device identity 分组。
- 同一 terminal 同一时刻最多执行一个 uu job。
- 长任务持续占用自己的 terminal，并实时把输出返回给调用方；结束后 terminal 重新 idle。
- 有多个 idle terminal 时，调度器任选一个可用 worker；Agent 不感知具体 session。
- 没有空闲 terminal 时排队，而不是抢占正在运行的 job。
- 不跨设备借用 terminal。

### 3.1 哪些 Terminal 可以被 uu-cli 使用

不能假设设备上的所有 Terminal 都归 uu-cli，因为用户可能自己手动使用某个 Terminal。

v1 使用一个很轻的“adopt”流程：

- 新 Terminal 被发现时，如果它尚未属于 uu-cli worker pool，必要时弹出一个简短 PowerShell 提示；
- 默认动作是把它加入当前绑定设备的 worker pool，用户可以跳过；
- 被采用的 session 可通过官方 rename 能力改成 uu-cli 内部名称，或由本地配置记录；最终实现选择更稳健的一种；
- 这些名字只用于内部识别和 `uu list` 诊断，不要求用户记忆。

如果当前只有一个明确的 workspace/device 绑定，并且用户已选择“以后该设备新 Terminal 自动加入”，后续可零交互自动加入。

## 4. 命令语义：不依赖上一条 shell 状态

这是对 Agent 最重要的约束之一。

**不同 `uu` 调用之间不保证复用同一个 Terminal，也不保证继承 shell-local 状态。**

错误依赖：

```powershell
uu cd src
uu source .venv/bin/activate
uu python main.py
```

正确写法：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每个普通 job 都从 workspace 的 `remote_root` 开始，概念上等价于：

```bash
cd <remote_root> && <user command>
```

这样 terminal 可以自由调度，而不会因为某个 shell 上一次 `cd`、`export`、alias、virtualenv 等状态造成随机行为。

真正需要持续 PTY 上下文的交互程序/长任务，在该 job 生命周期内独占一个 terminal；job 结束后不承诺其 shell-local 状态成为下一条命令的输入条件。

后续 Agent Skill 必须明确写出这一点。

## 5. 单次命令边界

官方 UU terminal 是持续 PTY 字节流，没有客户端可达的逐命令 exec/exit-code API。

因此每个 uu job 在内部使用带随机 nonce 的 completion marker：

```text
start job
→ 写入用户命令 + 私有 marker
→ 持续转发 PTY 输出
→ 收到匹配 nonce 的 marker + exit code
→ 从用户输出剥离 marker
→ job 完成
```

不能靠 shell prompt 文本猜命令是否结束。

一旦命令已经写入 PTY，连接异常时不得自动在另一个 terminal 重放；如果不能确认结果，返回“执行状态未知/命令可能仍在运行”，避免副作用命令重复执行。

## 6. 多设备

多个设备通过官方返回的稳定 device identity 隔离。

首次为 workspace 建立 binding 时：

- 只发现一个有可用 Terminal 的设备：默认选它，尽量只需一次确认；
- 同时发现多个设备：弹出简短 PowerShell 选择器，以友好 device name + terminal 数量展示；用户选一次；
- 内部保存稳定 identity，Codex 不接触裸 `deviceId`。

示例：

```text
uu-cli · D:\projects\aic

发现多个远端设备：
  [1] AIC-Car-1   3 terminals
  [2] AIC-Car-2   2 terminals

选择设备 [1]:
```

绑定后普通调用始终使用该 workspace 的设备，不自动 failover 到其他设备。

## 7. 最小交互面

正常使用：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

诊断/绑定只保留：

```powershell
uu bind
uu list
```

其中：

- `uu bind`：建立或重新建立当前 workspace 的 device + remote root binding，并接纳可用 Terminal；
- `uu list`：只用于人类诊断，显示当前 workspace、绑定设备、remote root、worker 数量和 busy/idle；
- 不要求 Codex 使用 `bind/list`，除非首次配置或用户明确要求。

第一次普通 `uu <command>` 也可以自动进入 bind 流程，不强迫用户先学 `uu bind`。

## 8. 自动发现与后台 broker

为了做到“用户手动打开官方 UU Terminal 后，uu-cli 自己发现并接上”，v1 允许一个轻量后台 broker。

目标不是常驻服务平台，而是一个用户级协调器：

- 单实例；
- 无窗口、无 tray；
- CPU 空闲时接近 0；
- UU 未运行时只等待，不频繁启动官方 CLI；
- 优先使用 Windows 进程/事件通知触发刷新；实在需要 polling 时使用退避，不做高频轮询；
- 新的官方 Terminal 出现时刷新 session list；
- 只有在需要用户选择设备/接纳 Terminal 时才主动弹出简短 `pwsh` 交互；
- `uu` 命令本身也能在 broker 缺失时拉起/恢复 broker，不能因为后台进程异常让 CLI 永久不可用。

严格“官方 UU 一启动 broker 才启动、UU 一退出 broker 就完全消失”在不修改官方 UU 的前提下未必有可靠系统触发点。实现优先保证低占用和无感：可以使用一个极轻的用户级 watcher 在登录会话中等待 UU，UU 不运行时不做实际工作。

## 9. Broker 与 CLI 的职责

建议采用一个可执行文件 `uu.exe`，公开只有 CLI；内部隐藏 broker 模式。

```text
uu.exe CLI
   ↓ local IPC
uu.exe broker
   ↓
workspace/config
session discovery
worker scheduler
terminal attachments
output streaming
```

好处：

- 安装物少；
- broker 统一协调多个 Codex/多个并发 `uu` 调用；
- busy/idle 状态不会因为多个独立 CLI 进程彼此不知道而冲突；
- 后续 installer/lifecycle 更简单。

内部 IPC 建议使用当前用户范围的 Windows named pipe；不得开放到其他用户会话。

## 10. 配置

配置放在当前用户的应用数据目录，例如：

```text
%LOCALAPPDATA%\uu-cli\
```

只保存：

- workspace local root；
- remote root；
- 已验证的设备 identity + friendly name；
- worker/session 的非敏感识别信息；
- 少量 UX 偏好（如该设备新 Terminal 是否自动加入）。

不保存 UU 密码、Token、Cookie、local-host token 或其他认证材料。

## 11. `uu list` 示例

```text
Workspace  D:\projects\aic
Remote     AIC-Car-1 · /home/pi/aic
Workers    3

  worker-1   busy   python3 train.py
  worker-2   idle
  worker-3   idle
```

这里的 worker 名只用于诊断。Agent 不需要知道它们。

## 12. v1 非目标

- 一个 workspace 同时驱动多台设备；
- 用户手工管理 session id；
- 依赖某个 Terminal 的长期 shell history；
- 自动点击 UU GUI 创建 Terminal；
- 注入/伪造官方客户端；
- MCP/Web/tray/远程桌面自动化；
- 为 Git/Python/CMake 设计专用命令。
