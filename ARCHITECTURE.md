# Architecture

这份文档只定义 `uu-cli` v1 的概念架构。它不规定具体语言、进程模型、IPC、Windows API 或代码目录；实现者应根据真实 Windows + 官方 UU 环境自行选择最简单可靠的方案。

## 1. 核心模型

```text
Local Workspace
      ↓
Remote Binding = device identity + remote root
      ↓
Terminal Worker Pool
```

对 Codex 来说，唯一稳定概念是当前本地 workspace。设备身份、session name/id、worker 调度都隐藏在 CLI 后面。

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

## 3. Remote device

设备是 Terminal pool 的隔离边界。

```text
device A → workers A1 A2 A3
device B → workers B1 B2
```

不同设备的 worker 绝不能混用。

如果首次绑定时同时发现多个设备已经打开 Terminal，应由用户通过简短 PowerShell 选择器选择一次。UI 使用友好设备名和必要的区分信息，内部再保存经过验证的稳定 identity。

Codex 不直接选择 deviceId。

## 4. Terminal worker pool

官方 UU Terminal 由用户自己在官方 UU 中打开。`uu-cli` 自动发现已经存在的 Terminal，并把被用户允许使用的 Terminal 视为 worker。

```text
device A
├─ worker 1  idle
├─ worker 2  busy
└─ worker 3  idle
```

规则：

- 一个 worker 同时只属于一个正在运行的 job；
- 长任务持续占用原 worker，并持续把输出返回给调用方；
- 其他命令可调度到其他 idle worker；
- worker 全忙时等待；
- 不抢占、不串流、不跨设备；
- worker/session 的内部名字不要求用户记忆。

## 5. 新 Terminal 的发现和归属

用户手动打开新官方 Terminal 后，`uu-cli` 应尽快自动发现。

处理原则：

- 如果一个 `uu` 请求正在等待 Terminal，而且新 Terminal 显然属于该请求的目标设备，可以直接继续；
- 如果归属完全明确，可以无提示加入对应 pool；
- 如果存在多个设备、多个可能 workspace，或者无法判断用户是否愿意把这个 Terminal 交给 uu-cli，弹一个极短的 PowerShell 选择窗口；
- 选择应支持默认项、数字键和 Enter，避免要求用户输入内部 ID；
- 已确认过的同一 Terminal/binding 不重复提示；
- 用户明确跳过的 Terminal 不得被使用。

不依赖修改官方 session 名称来完成归属。实现者应优先使用对 UU 干扰最小的本地识别方式。

## 6. Shell context

Terminal 不是 workspace 的长期 shell 状态容器。

不同 `uu` 调用之间不保证复用同一个 worker，因此不保证继承：

- `cd`；
- `export`；
- `source` / virtualenv；
- alias；
- 其他 shell-local 状态。

普通 job 从 workspace 的 `remote_root` 开始。

因此：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

是正确模型，而不是依赖前一条 `uu cd src`。

持续交互/长任务在自己的生命周期内保持同一个 worker；job 结束后不承诺留下的 shell-local 状态会被下一条命令使用。

## 7. Job

一次 `uu <command>` 对应一次 job。

概念流程：

```text
resolve workspace binding
→ choose one idle worker on the bound device
→ run from remote_root
→ stream output in real time
→ determine completion + exit status
→ release worker
```

关键安全语义：

- 命令一旦已经发送到远端，不因为连接异常自动换 worker 重跑；
- 如果无法确认结果，明确报告 unknown/may still be running；
- 不使用 shell prompt 文本作为唯一的命令完成判断；
- PTY 本身 stdout/stderr 合流时保持事实，不虚构分离。

具体 completion marker、PTY attachment、恢复协议等属于实现细节。

## 8. 文件传输

`put/get` 绑定到当前 workspace 的远端设备和路径上下文：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

文件通道不要求与 Terminal 共用同一种内部机制，但必须：

- 二进制安全；
- 完成/失败明确；
- 不完整不能报告成功；
- 不暴露底层协议给用户或 Agent。

## 9. 自动可用与低占用

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
- 需要用户决定时可以弹短 PowerShell 交互；
- 后台能力失效后，下一次 `uu` 可以恢复或给出明确错误。

## 10. Official UU isolation

`uu-cli` 必须把官方 UU 当作外部事实来源，而不是需要改造的宿主。

边界：

- 不修改、替换、patch、inject 官方二进制；
- 不改登录、设备、更新、启动配置；
- 不保存认证秘密；
- 不 kill/rename/close 用户未授权的 session；
- 不改变用户正在手工操作的 Terminal；
- 不让自己的崩溃/卸载影响官方 UU；
- 如果某种 attach/reuse 会影响官方 UI，应换安全路径或只使用专门提供给 uu-cli 的 Terminal。

## 11. Portability

架构不能依赖开发机偶然状态。

应当允许：

```text
另一台正常 Windows 电脑
+ 官方 UU Remote 已安装、可登录、可开 Terminal
+ 安装 uu-cli
→ 直接完成首次 binding 并使用
```

因此设备身份、安装路径、用户目录、盘符、session 名、UU 版本差异都必须通过运行时发现/兼容性逻辑处理，而不是写死在代码里。

安装后不应要求 Python/Rust/Node/WSL 等开发运行时。

## 12. User-visible surface

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

## 13. v1 非目标

- 一个 workspace 同时驱动多台设备；
- 用户手工管理 session id；
- 持久 shell-context 系统；
- 自动点击 UU GUI 创建 Terminal；
- 注入/签名伪造/认证绕过；
- MCP / Web / tray；
- Git/Python/CMake 专用远程 API。

任何内部设计如果需要新增大量用户概念才能成立，默认应该重新考虑。