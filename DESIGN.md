# Design contract

这份文档定义 `uu-cli` v1 的产品行为与边界。内部实现可以变化，但公共体验不能因为 Windows UU 的内部细节变复杂。

## 1. 第一性原理

用户真正需要的是：

> 在某个本地项目里，用 `uu <command>` 像本地 shell 一样操作一个远端工作目录。

正常使用只需要：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

如果使用者已经会 shell，就不应该再学习“设备/session/任务 API”。

## 2. Workspace 是唯一稳定的用户上下文

v1 的绑定关系是：

```text
local workspace
→ remote device
→ remote root
```

首次绑定时：

- Git 仓库内默认使用 Git root；
- 非 Git 目录默认使用当前目录；
- binding 存在用户配置目录，不往项目仓库写额外文件；
- 一个 workspace v1 只绑定一个远端设备。

从子目录执行 `uu` 时，向上查找最近的 workspace binding。

## 3. Terminal 是执行资源，不是 Agent 上下文

用户通过官方 UU GUI 手工打开 Terminal；`uu-cli` 自动发现并把允许使用的 session 组成该设备的 worker pool。

```text
device
├─ worker 1  idle
├─ worker 2  busy
└─ worker 3  idle
```

规则：

- 一个 terminal 同一时刻只执行一个 uu job；
- 长任务持续占用自己的 terminal，并实时回传输出；
- 其他并发命令使用其他 idle terminal；
- 没有空闲 worker 时排队；
- 绝不跨设备借 terminal；
- session name/id 不进入日常用户体验。

## 4. 不依赖上一条 shell-local 状态

不同 `uu` 调用之间不保证使用同一个 Terminal，因此不保证继承：

- `cd`；
- `export`；
- `source` / virtualenv；
- alias；
- 其他 shell-local 状态。

不要：

```powershell
uu cd src
uu source .venv/bin/activate
uu python main.py
```

应该：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每个普通 job 都从该 workspace 的 `remote root` 开始。Codex Skill 必须明确写出这条规则。

持续交互程序或长任务在自己的 job 生命周期内保持同一个 terminal lease；job 结束后不承诺 shell-local 状态被下一条命令继承。

## 5. 第一个 Terminal 由用户创建

当前研究的官方 Windows UU 4.43.x 没有对外暴露稳定的 headless terminal launcher。官方 GUI 会在 GameViewer 主进程内注册 local host/token，再启动官方 CLI；这个创建链没有可复用的外部入口。

因此 v1 流程是：

```text
UU 已运行并登录
→ 用户手工打开官方 Terminal
→ uu-cli 自动发现
→ workspace 完成设备/remote-root binding
→ 后续 Codex 只执行 uu ...
```

`uu-cli` 不自动点击 GUI，不注入 GameViewer，不伪造签名，不绕过 host registration。

## 6. 首次绑定交互

第一次普通 `uu <command>` 也可以自动进入 bind 流程，不强迫用户先学 `uu bind`。

### 没有 Terminal

```text
uu-cli · D:\projects\aic
当前没有可用的 UU Terminal。
请在网易 UU 远程中打开目标设备的“终端远控”。
等待中…  Ctrl+C 取消
```

用户打开后，原来的命令继续，不要求重跑。

### 只有一台设备

尽量一键确认 workspace + device，再从选中的 terminal 获取 `pwd` 作为 remote root 默认值。

### 多台设备

只给人类一次简短 PowerShell 选择：

```text
发现多个远端设备：
  [1] AIC-Car-1   3 terminals
  [2] AIC-Car-2   2 terminals
选择 [1]:
```

内部保存稳定 device identity；Codex 不接触裸 `deviceId`。

## 7. 新 Terminal 的发现

新开的官方 Terminal 应被 uu-cli 自动发现。

- 已绑定设备且已允许自动加入：直接加入 worker pool；
- 否则只弹一个短提示确认是否加入；
- 用户自己手动使用、明确跳过的 Terminal 不得被调度。

具体内部可用 rename 或本地 registry 标记 worker，选更稳定、对用户干扰更小的方案。

## 8. 公共 CLI

日常：

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

`uu bind` 用于首次或重新建立当前 workspace binding；`uu list` 只给人类查看当前 workspace、remote、worker busy/idle。

v1 不再把 `devices/use` 作为主要公共模型。设备只是 workspace binding 的内部组成部分。

## 9. 命令边界与输出

官方 terminal 是持续 PTY 字节流，没有客户端可达的逐命令 exec/exit-code API。

每个 job 内部使用带随机 nonce 的 completion marker 来判断本次命令完成和 `$?`。marker 必须从最终输出剥离，不能依赖 prompt 文本猜结束。

PTY stdout/stderr 可能合流，CLI 不伪造不存在的分离语义。

一旦命令已经写入远端 PTY，连接异常时不得自动在另一个 worker 重放。无法确认结果时明确返回“命令可能仍在运行/执行状态未知”。

## 10. 文件传输

公共接口保持：

```powershell
uu put .\file.bin /remote/file.bin
uu get /remote/file.bin .\roundtrip.bin
```

要求：二进制安全、明确完成/失败、传输不完整绝不返回成功。优先研究官方原生文件通道；除非原生入口确认不可复用，不提前把 terminal + base64 固化成正式架构。

## 11. 后台生命周期

为了自动发现 Terminal 和协调多个 Codex 并发调用，v1 允许一个极轻的用户级 broker。

约束：

- 无 console / GUI / tray；
- 单实例；
- UU 未运行、无 terminal、无 job 时尽量只等待，CPU 占用接近 0；
- 优先事件触发刷新，必要 polling 必须退避，不能永久高频启动官方 CLI；
- 只有需要人选择设备/接纳新 Terminal 时才弹一个短 `pwsh` 交互；
- `uu` 调用发现 broker 不在时可以自行拉起恢复。

在不修改官方 UU 的前提下，严格做到“UU 一启动 broker 才启动、UU 一退出 broker 进程绝对消失”未必存在可靠系统触发点。产品优先保证低占用、自动发现和无感，而不是为了进程数量强行引入脆弱 hook。

## 12. 安装与配置

目标交付物：一个 user-scope Windows installer。

安装：

- 安装 `uu.exe`；
- 加入当前用户 PATH；
- 配置轻量 broker/watcher 自动启动；
- 注册标准卸载入口；
- 不修改官方 UU 文件。

用户配置存放 `%LOCALAPPDATA%\uu-cli\` 或等价位置，只保存 workspace binding、设备非敏感 identity、remote root、worker 标记和少量 UX 偏好。

不得保存 UU 密码、Token、Cookie、local-host token 等认证材料。

## 13. 对 Agent 的要求

Agent 只需要知道：

```text
远端执行：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

硬规则：

> 不依赖不同 `uu` 调用之间的 shell-local 状态；需要连续 `cd/source/export` 时放进同一条命令。每个普通 job 从 workspace remote root 开始。

Agent 不选择 deviceId、不选择 session、不管理 worker。

## 14. 非目标

v1 不做：

- 一个 workspace 同时驱动多台远端设备；
- 用户可见 session 管理；
- MCP / Web UI / tray；
- 远程桌面自动化；
- Git/Python/CMake 专用 API；
- 自动点击 UU GUI；
- GameViewer 注入、签名伪造、host-registration 绕过；
- 自己的一套持久 shell-context 系统。

详细内部架构见 `ARCHITECTURE.md`，实施蓝图见 `IMPLEMENTATION.md`。
