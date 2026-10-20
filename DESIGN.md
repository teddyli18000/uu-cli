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
- 非 Git 目录默认使用当前目录或合理的最近根目录；
- binding 存在用户配置目录，不往项目仓库写额外文件；
- 一个 workspace v1 只绑定一个远端设备。

从子目录执行 `uu` 时，使用最近的 workspace binding。

同一台远端设备可以被多个本地 workspace 绑定；这些 workspace 共享该设备的 Terminal worker pool。每个 job 使用自己 workspace 的 `remote_root`，因此不需要把 Terminal 再按 workspace 切成多套。

## 3. Terminal 是设备级执行资源，不是 Agent 上下文

用户通过官方 UU GUI 手工打开 Terminal；`uu-cli` 自动发现并把用户允许使用的 session 组成 **该设备的 worker pool**。

```text
device A
├─ worker 1  idle
├─ worker 2  busy
└─ worker 3  idle
```

规则：

- 一个 terminal 同一时刻只执行一个 uu job；
- 长任务持续占用自己的 terminal，并实时回传输出；
- 其他并发命令使用其他 idle terminal；
- 没有空闲 worker 时等待；
- 绝不跨设备借 terminal；
- 同设备不同 workspace 可以复用同一 pool，但每个 job 的 remote root 独立；
- session name/id 不进入日常用户体验。

## 4. 不依赖、也尽量不泄漏上一条 shell-local 状态

不同 `uu` 调用之间不保证使用同一个 Terminal，因此不能依赖 `cd`、`export`、`source`/virtualenv、alias 或其他 shell-local 状态。

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

每个普通 job 都从该 workspace 的 `remote_root` 开始，并应在独立的 shell scope 中执行，使本次 job 的 `cd/export/source/exit/exec` 等变化尽可能不会污染后续 job。具体使用 subshell、fresh shell 或其他机制由实现者决定。

持续交互程序或长任务在自己的 job 生命周期内保持同一个 terminal；job 结束后不承诺它留下的 shell-local 状态成为下一条命令的输入。

Codex Skill 必须明确写出这一点。

## 5. 第一个 Terminal 由用户创建

当前研究的官方 Windows UU 4.43.x 没有对外暴露稳定的 headless terminal launcher。官方 GUI 会在 GameViewer 主进程内注册 local host/token，再启动官方 CLI；这个创建链没有可复用的外部入口。

因此 v1 流程是：

```text
UU 已运行并登录
→ 用户手工打开官方 Terminal
→ uu-cli 自动发现
→ 必要时做一次极短的人类确认
→ workspace 完成 device + remote-root binding
→ 后续 Codex 只执行 uu ...
```

`uu-cli` 不自动点击 GUI，不注入 GameViewer，不伪造签名，不绕过 host registration。

## 6. 交互逻辑 — 没有歧义就不问，同一个决定只问一次

### 6.1 Workspace 尚未绑定，用户执行 `uu ...`

- 已经只有一台明确设备有已授权 worker：选择该设备，不要求用户输入 deviceId；
- 多台设备都有已授权 worker：弹一次设备选择，以 friendly name + terminal 数量展示；
- 当前没有 worker：`uu` 等待，并明确提示“请在官方 UU 中打开想使用设备的 Terminal；新打开的 Terminal 将用于本次请求”；用户随后打开的唯一匹配 Terminal 视为对本次请求的明确授权，原命令继续，不要求重跑，也不再弹第二个“是否交给 uu-cli”确认；
- 如果等待期间同时出现多个设备/多个新 Terminal，才弹一次短选择，不能猜。

第一次 binding 还需要 `remote_root`。默认值可以来自所选远端 Terminal 当前 `pwd`；用户只需 Enter 接受或输入正确目录。之后不重复询问。

### 6.2 没有请求正在等待，新 Terminal 出现

发现不等于可以静默接管。

弹一个很短的人类确认：

```text
AIC-Car-1 新终端已打开
[Enter] 交给 uu-cli    [N] 保留手动使用
```

规则：

- Enter 后该 session 成为该 device pool 的 worker；
- `N`、Esc、关闭窗口或取消都视为 **不使用**；
- v1 不提供“以后该设备所有新 Terminal 都自动接管”的永久开关，避免用户未来手动开的 Terminal 被无感抢走；
- 短时间连续出现多个新 Terminal 时应在同一个交互窗口里顺序/批量处理，避免连续弹多个窗口；
- 同一设备存在多个 workspace **不构成歧义**，因为 pool 属于 device。

### 6.3 交互与命令 I/O 必须分离

人类选择窗口不能污染正常 `uu` 的 stdout，也不能偷走远端命令 stdin。

要求：

- `uu <command>` 的 stdout 尽量只保留远端输出；
- 工具错误/状态提示走 stderr；
- 需要人类选择时使用独立的短 PowerShell/console 交互，而不是把菜单插进一个可能正在被 Codex 或 pipe 使用的命令流；
- 不能把 PowerShell 7 (`pwsh`) 当作用户必须预装的运行时：若存在可优先使用，否则可回退系统自带 Windows PowerShell 或项目自身的最小原生交互窗口；
- 如果环境当前不能安全展示交互，宁可等待/给明确错误，也不要猜。

Codex 不接触裸 `deviceId`。

## 7. 公共 CLI

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

`put/get/bind/list` 是保留入口。为了仍然满足“几乎任意远端命令都只是加 `uu` 前缀”，提供标准 escape：

```powershell
uu -- <remote command...>
```

例如远端确实有名为 `list`/`bind`/`put`/`get` 的命令时，用 `uu -- ...` 强制按远端命令执行。

`uu bind` 用于首次或重新建立当前 workspace binding；`uu list` 只给人类查看当前 workspace、remote 和 worker 状态。

v1 不把 `devices/use` 作为主要公共模型。设备只是 workspace binding 的内部组成部分。

## 8. Job、命令边界与恢复

一次 `uu <command>` 是一个 job。

必须满足：

- 从 workspace `remote_root` 开始；
- 输出实时返回；
- 长任务不被工具自己的短超时截断；
- 能可靠判断单次命令完成和 exit code；
- 不靠 shell prompt 文本猜结束；
- PTY stdout/stderr 合流时保持事实，不伪造分离；
- 一个 worker 被 job 占用期间不能再接收第二个 job。

一旦命令已经写入远端 PTY，连接异常时不得自动在另一个 worker 重放。无法确认结果时明确返回“命令可能仍在运行 / 执行状态未知”。

如果后台组件崩溃或重启，任何可能仍运行旧 job 的 worker 都不能直接当成 idle。实现者必须可靠恢复，或者把它 quarantine/unknown，直到能够证明安全再复用。宁可让用户开一个新 Terminal，也不能把新命令注入可能还忙着的 PTY。

## 9. 文件传输

公共接口保持：

```powershell
uu put .\file.bin remote/path.bin
uu get remote/path.bin .\roundtrip.bin
```

路径语义：

- 远端相对路径相对于当前 workspace 的 `remote_root`；
- 远端绝对路径按绝对路径处理；
- `~` 等 shell 风格路径如果公共接口接受，就必须稳定解析为远端语义，不能意外按 Windows 本地路径解释。

要求：二进制安全、明确完成/失败、传输不完整绝不返回成功。内部优先复用稳定官方能力，但最终实现方式不属于产品契约。

## 10. 不干扰官方 UU — 硬边界

`uu-cli` 是官方 UU 的旁路工具，不是 patch、插件注入器或替代客户端。

必须满足：

- 不修改、替换、patch、inject 官方 UU/GameViewer 二进制；
- 不改变 UU 的账号、登录、设备、更新、启动配置；
- 不保存或导出 UU 密码、Token、Cookie、local-host token 等认证材料；
- 不 kill、close、rename 或偷偷接管用户没有授权给 uu-cli 的 Terminal；
- 不改变用户正在手工操作的 Terminal 的输入或 shell 状态；
- 不让 uu-cli 崩溃、升级、卸载影响官方 UU 本身；
- 不依赖签名伪造、认证绕过、GUI 自动点击、进程注入。

当用户明确把某个 Terminal 交给 uu-cli 后，该 Terminal 才可以作为专用 worker 使用。

如果某种 attach/reuse 方法会明显干扰官方 UU 或用户未授权的 Terminal，必须换更安全的实现。

## 11. 可移植性 — 目标是“换台电脑也能装了就用”

目标平台不是开发者自己的 Windows，而是：**官方 UU Remote 本身能够正常工作的受支持 Windows 电脑。**

实现必须尽可能：

- 自动发现官方 UU 安装位置和可用组件；
- 不硬编码开发机路径、用户名、盘符、deviceId、session 名、精确安装目录；
- 优先做能力探测和兼容性判断，而不是只认一个固定版本；
- 不要求用户额外安装 Python、Rust、Node、PowerShell 7、WSL 等开发运行时；
- 尽量以普通用户权限完成安装和运行；
- 对不同 Windows 用户、路径、设备名、系统语言保持稳健；
- 在不兼容时给出明确诊断，而不是静默失败。

支持范围应跟随官方 Windows UU 实际支持的平台；不要为了开发方便把开发机偶然环境写成前提。

## 12. 后台生命周期与资源占用

用户目标是：UU 在使用时，`uu-cli` 随时可用；没有任务或没有 Terminal 时，资源占用尽可能低。

实现方式不固定，可以是 broker、watcher、事件驱动、lazy process 或其他方案。但结果必须满足：

- 用户不需要手工 start/stop 第二个服务；
- 新开的官方 Terminal 能被自动发现；
- 空闲时 CPU 接近 0，不永久高频轮询或反复 spawn 官方 CLI；
- 没必要时不保持昂贵连接；
- 无 tray、额外 GUI 或常驻可见 console；
- 只有真正需要用户选择时才弹短人类交互；
- 后台组件异常后，下一次 `uu` 可以恢复或给出明确错误；
- 不拖累或阻止官方 UU 的启动、退出、升级。

产品要求是“跟着 UU 使用时自然可用”，不要求为了字面上的“同进程启动/退出”去 hook 或修改官方 UU。

## 13. 安装、CI 与验证

目标交付物：一个普通用户可以直接安装的 Windows installer。

安装完成后用户只应该感知 `uu` 命令和必要时的极短绑定交互。

安装/升级/卸载必须：

- 不要求开发环境；
- 尽量 user-scope；
- 正确配置 PATH 和必要的低占用后台集成；
- 保留有效 workspace binding；
- 卸载只清理 uu-cli 自己的文件、配置和集成；
- 不修改或删除官方 UU 及其登录状态。

开发验证采用 CI-first：能在 GitHub-hosted Windows 干净 VM 中做的 build/test/package/install/uninstall/兼容性测试全部优先放到 CI，减少对开发者主力 Windows 的反复安装和风险。真实 UU 登录、真实 Terminal、远端设备等 CI 无法证明的部分，再做最小必要实机验收。

详细策略见 `TESTING.md`。

## 14. 对 Agent 的要求

Agent 只需要知道：

```text
远端执行：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

硬规则：

> 不依赖不同 `uu` 调用之间的 shell-local 状态；需要连续 `cd/source/export` 时放进同一条命令。每个普通 job 从 workspace remote root 开始。

Agent 不选择 deviceId、不选择 session、不管理 worker，也不管理后台组件。

## 15. 非目标

v1 不做：

- 一个 workspace 同时驱动多台远端设备；
- 用户可见 session 管理；
- MCP / Web UI / tray；
- 远程桌面自动化；
- Git/Python/CMake 专用 API；
- 自动点击 UU GUI；
- GameViewer 注入、签名伪造、host-registration 绕过；
- 自己的一套持久 shell-context 系统。

详细概念架构见 `ARCHITECTURE.md`；实现目标和验收契约见 `IMPLEMENTATION.md`；测试策略见 `TESTING.md`。