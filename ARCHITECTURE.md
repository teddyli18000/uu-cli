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

- 用户运行 `uu bind` 时，当前目录就是精确 local root，即使它位于 Git 仓库内；
- 普通 `uu` 从子目录执行时使用最近的已存在 binding；
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

通常只处理一个新出现且可用的 detached Terminal；`uu bind` 用户调用即授权它。若有多个设备/Terminal 候选则报明确歧义并退出，不弹 picker、不猜，用户在官方 UU 中整理后重跑。Codex 不直接选择 deviceId，也不代替用户运行 bind。

## 4. Terminal worker pool

官方 UU Terminal 由用户自己在官方 UU 中逐个打开。库存发现不会授权；只有用户运行 `uu bind` 并选择的 Terminal 才进入 worker pool。

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
- 已授权 worker 全忙时排队并报告队列位置；
- 不抢占、不串流、不跨设备；
- worker/session 的内部名字不要求用户记忆。

## 5. Terminal 发现与人类授权

设备/Terminal 列表可以被刷新，但发现始终只是库存信息，不会授权或接管新 Terminal。授权只能由用户手动运行 `uu bind` 发起，并在该调用终端中完成；不得弹出独立授权窗口，也不得由后台 watcher 自动授权。

用户按自己的节奏在官方 UU 中逐个打开专用 Terminal；按需关闭对应官方标签页以释放连接，再切到目标 workspace 目录运行 `uu bind`。该目录精确成为 local root。唯一新出现且可用的 detached Terminal 即明确授权，无需设备/session picker 或再次问“是否交给 uu-cli”。

### 5.1 首次绑定

正常情况 `uu bind` 只找到一个新出现且可用的 detached Terminal，并直接授权。多个候选则给出歧义诊断并结束，不展示选择器或猜测；用户自行整理到唯一候选后重跑。首次绑定时在调用终端确认 remote root，默认值可来自该 Terminal 的 `pwd`。没有可用 Terminal 时命令清楚提示用户先打开 Terminal 并再次运行 `uu bind`，不让普通命令停在等待授权状态。

### 5.2 同设备增加 worker

已绑定 workspace 再运行 `uu bind` 时，将唯一新出现且可用的 detached Terminal 加入同一设备 pool。重复绑定保留现有 remote root；只有用户明确选择更改时才改 root。一次 bind 只处理一个 Terminal，便于用户逐个添加。

### 5.3 普通命令与 worker 状态

普通 `uu <command>`、`put` 和 `get` 只使用当前 workspace 已绑定且已授权的 worker。它们不自动绑定/授权、不提示或等待用户开新 Terminal。若 workspace 没有 binding 或没有已授权可用 worker，返回明确诊断，要求用户运行 `uu bind`。已授权 worker 全忙时排队并报告队列位置；unknown/quarantined worker 必须继续隔离。

### 5.3 不存在“同设备多个 workspace 的 Terminal 归属问题”

同设备多个 workspace 共享同一 worker pool，job 自己携带 `remote_root`。因此新 Terminal 不需要再问“属于哪个 workspace”。

## 6. Interaction channel

人类配置/授权只属于 `uu bind`；它使用当前调用终端。普通远端命令数据流不承担配置交互。

```text
human ── uu bind in exact workspace directory ── authorize sole new Terminal, confirm root
Agent ── uu stdin/stdout ── remote job on already authorized worker
```

要求：

- 正常 `uu` stdout 不混入选择菜单；
- 工具诊断走 stderr；
- 只有用户明确运行 `uu bind` 时才读取当前控制台的人类输入；
- 普通命令在缺少 binding/worker 时给出“请用户运行 `uu bind`”的诊断，不尝试弹窗或读取 stdin；
- 不依赖 PowerShell 7，也不创建独立 PowerShell/console 窗口。

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
→ choose one idle worker on bound device, or report/queue safely
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
→ 用户手动运行 `uu bind` 后发现并授权唯一候选 Terminal
→ 没任务时尽可能不占资源
```

实现者可以选择 broker、watcher、事件通知、lazy process 或其他结构。概念架构只要求：

- 用户不手工管理第二个服务；
- Terminal 库存可刷新，但发现不授权；`uu bind` 的用户选择才会授权；
- 空闲 CPU 接近 0；
- 不永久高频 polling/spawn 官方组件；
- 不需要时不保持昂贵连接；
- 无 tray、额外 GUI 或常驻可见 console；
- 不弹独立窗口；交互只在用户运行 `uu bind` 的调用终端中进行；
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

由用户手动执行的首次配置/诊断：

```powershell
uu bind
uu list
```

保留词冲突时使用：

```powershell
uu -- <remote command...>
```

`uu list` 是只读、人类手动执行的诊断命令，可以展示：

```text
Workspace  D:\projects\aic
Remote     AIC-Car-1 · /home/pi/aic
Workers    3

  worker 1   busy   python3 train.py
  worker 2   idle
  worker 3   idle
```

这里 worker 名称只用于诊断，不构成 Agent API。Agent 不得运行 `uu bind` 或 `uu list`；缺少 binding/worker 时请用户在 workspace 调用终端运行 `uu bind`，需要诊断时请用户运行 `uu list`。

## 16. v1 非目标

- 一个 workspace 同时驱动多台设备；
- 用户手工管理 session id；
- 持久 shell-context 系统；
- 自动点击 UU GUI 创建 Terminal；
- 注入/签名伪造/认证绕过；
- MCP / Web / tray；
- Git/Python/CMake 专用远程 API。

任何内部设计如果需要新增大量用户概念才能成立，默认应该重新考虑。

## 当前实现映射

- `windows_app` 连接公共 CLI、调用终端内的显式绑定交互、worker 调度及 put/get；`uu list` 是人类手动执行的只读诊断。
- `model/config/authorization/workspace` 保存用户侧绑定及具体 Terminal 实例授权；普通命令和后台进程不能添加授权。
- `worker_lease` 通过 OS 文件锁让同设备多 workspace 共享实例 worker；发送前同步写入 inflight 记录，只有可靠完成才能删除。崩溃释放文件锁但保留记录。
- `uuyc/discovery/inventory` 发现官方组件并查询设备、Terminal，不创建或关闭官方 Terminal。
- `transport/runner/job` 附着已授权 Terminal，经一个本地 ConPTY reader 收发、识别完成边界，并流式返回合流输出；每条普通命令使用 subshell。
- `worker_host` 为每个实际使用的 Terminal 保留一个隐藏附着进程。公共 CLI 通过仅当前用户可访问的本机 named pipe 路由 job；活连接不重新查询/附着，空闲五分钟或官方 UU 退出时退出。transport 的空闲检查与任务 liveness 同时检查官方 UU 的进程句柄；已经发送但未确认完成的 job 不重试。缓存不是授权，调度仍验证用户授权和实例 lease。
- 专用 Bash worker 在父 shell 关闭作业控制并启用 `ignoreeof`：前台命令照常收到 Ctrl+C/EOF，命令完成后迟到的调用方 EOF 不得关闭 worker。业务命令仍在嵌套 subshell 中隔离自身状态。
- `transfer` 复用同一 worker 的隔离 job，base64 分块上传 / 流式下载，size 与 SHA-256 校验后同目录发布。
- `uu-background` 只承载专用 worker attachment；不设置登录启动项，也不运行发现 watcher。绑定/授权由用户直接运行 `uu bind`，选择在调用终端内完成。普通命令可定位并复用已授权 worker；缺少 binding/worker 时给出运行 `uu bind` 的明确诊断，不自动授权或弹窗。
- 用户安装包只含两个原生可执行文件、PowerShell 5.1 安装/卸载脚本及 Agent Skill，不含官方 UU 二进制或开发运行时。

实例身份依赖官方列表提供 device identity、session name 和 createdAt。字段不足时拒绝授权；不能把名字当作旧实例证明。官方 attach 只接受设备与名字，列表复查与 attach 之间能否获得更强的原子实例保证尚无官方能力证据，需纳入真实互操作检查。
