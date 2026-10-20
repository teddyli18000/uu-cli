# AGENTS.md

## Project intent

`uu-cli` 的目标是把 Windows 本地已经登录的网易 UU 远程变成一个几乎透明的远端 shell + 文件通道。

先读：

1. `README.md`
2. `DESIGN.md`
3. `ARCHITECTURE.md`
4. `IMPLEMENTATION.md`
5. `ROADMAP.md`

研究资料用于理解已经确认的 UU 行为，但不要把研究过程本身变成产品复杂度。

## Your role

你是主要实现者。请自行完成设计落地、技术选型、代码组织、调试、实机验证、installer 和兼容性处理。

不要等待别人逐步告诉你该用哪个 Windows API、哪个 crate、哪个 IPC 或哪个模块结构。只要不违反产品契约和硬边界，内部实现由你决定。

遇到文档中的建议与真实 Windows + 官方 UU 行为冲突时，以实机证据为准；保留公共体验，选择更简单、更稳健的实现，并把必要的设计变化记录清楚。

## Public contract

日常用户和 Agent 只需要：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

配置/诊断最多：

```powershell
uu bind
uu list
```

不要重新引入 `devices/use/session/new/attach/kill` 等内部概念作为日常公共模型。

## Product model

```text
Local Workspace
      ↓
Remote Binding = one device + remote root
      ↓
Terminal Worker Pool
```

- v1 一个 workspace 只绑定一台远端设备；
- Codex 不选择 deviceId；
- 用户在官方 UU 中手工打开 Terminal；
- uu-cli 自动发现并使用被允许的 Terminal；
- Terminal 是 worker，不是 Agent 的长期 shell 上下文；
- 多 Terminal 用于并发；长任务持续占用自己的 worker并实时输出；
- 不跨设备调度。

## Shell context — HARD RULE

**不同 `uu` 调用之间不得依赖 shell-local 状态。**

不要假设下面成立：

```powershell
uu cd src
uu source .venv/bin/activate
uu python main.py
```

应使用：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

普通 job 从 workspace binding 的 `remote_root` 开始。

最终 Agent Skill 必须明确写出这条规则。

## Workspace and device UX

- Git repo 默认以 Git root 作为 workspace；非 Git 目录采用合理根目录；
- binding 存在用户侧，不污染 repo；
- 从子目录运行时使用最近的 workspace binding；
- 多设备同时有 Terminal 时，只让用户做一次极短的人类选择；
- UI 展示 friendly device name，不要求用户或 Codex处理裸 deviceId；
- 绝不能根据相似名字猜错设备；
- 已绑定后日常 `uu` 不再要求设备选择。

## Terminal discovery and interaction

当前官方 Windows UU 的研究结论：已有 Terminal 可以查询/附着/复用，但第一个官方 Terminal 没有稳定的外部 headless 创建入口。因此用户负责在官方 UU 中打开 Terminal，uu-cli 负责之后的发现和使用。

体验要求：

- 如果 `uu` 正在等待 Terminal，用户打开后应自动发现并继续；
- 新 Terminal 归属明确时尽量自动处理；
- 有歧义或无法确定用户是否愿意交给 uu-cli 使用时，可以弹出一个非常短的 PowerShell 选择窗口；
- 交互要默认项清晰、按数字/Enter 即可完成；
- 不重复骚扰已经确认过的绑定；
- 用户明确跳过的 Terminal 不得被使用。

## Do not interfere with official UU — HARD RULE

uu-cli 必须是旁路工具，不能破坏或劫持官方 UU。

禁止：

- 修改/替换/patch/inject 官方 UU 或 GameViewer 二进制；
- 修改 UU 的登录、账号、设备、更新、启动等设置；
- 提取或保存密码、Token、Cookie、local-host token 等认证材料；
- 关闭、kill、rename 或偷偷接管用户未授权的 Terminal；
- 改变用户正在手工操作的 Terminal 状态或输入；
- 签名伪造、认证绕过、GUI 自动点击、进程注入；
- 让 uu-cli 的崩溃、升级、卸载影响 UU 本身正常使用。

如果某个官方 attach/reuse 路径会明显干扰用户当前 UU 窗口，不要硬用；寻找安全方案，或者只使用用户明确提供给 uu-cli 的专用 Terminal。

## Portability — HARD REQUIREMENT

目标不是只在开发机运行。

在官方 UU Remote 本身能够正常工作的受支持 Windows 电脑上，用户安装 uu-cli 后应尽可能无需手工改路径、源码或开发环境即可使用。

因此：

- 不硬编码开发机路径、用户名、盘符、deviceId、session name、版本号；
- 自动发现官方 UU 安装和可用组件；
- 优先能力探测，而不是只认一个精确客户端版本；
- 不要求用户安装 Python/Rust/Node/WSL 等开发运行时；
- 尽量普通用户权限运行；
- 对不同路径、Windows 用户、设备名、系统语言保持稳健；
- 发布前必须在非开发机或干净用户环境完成完整安装/首次使用验证；
- 不兼容时给出明确诊断，不能静默失败。

## Command semantics

实现必须保证：

- 输出实时返回；
- 长任务可用；
- 单次命令完成边界和 exit code 可靠；
- stdout/stderr 如果 PTY 合流则保持事实，不伪造分离；
- Ctrl+C/取消尽量符合普通终端预期；
- 已经发送的命令在连接异常后绝不自动换 Terminal 重放；
- 结果不确定时明确报告 unknown/may-still-be-running。

具体 marker、PTY、IPC、调度和恢复机制由你自行设计。

## Background behavior

用户不应该管理第二套服务。

最终行为应满足：

- 官方 UU 在用时，uu-cli 随时可用；
- 自动发现用户新开的 Terminal；
- 无任务、无 Terminal 或 UU 未运行时资源占用尽可能低；
- 禁止永久高频 polling/spawn 官方 CLI；
- 无 tray、额外 GUI、常驻可见 console；
- 只有真正需要人类决定时才弹短 PowerShell 交互；
- 后台组件异常后，下一次 `uu` 可以自动恢复或明确报错；
- 不拖累 UU 的启动、退出、升级。

是否采用 broker、watcher、event notification、named pipe、lazy process 等完全由你决定。

## File transfer

公共接口固定：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

必须二进制安全、明确成功/失败、不完整传输不能报告成功。优先利用稳定官方能力，但内部方案由你根据研究和实测决定。

## Scope discipline

v1 不扩：

- 一个 workspace 同时控制多台远端设备；
- MCP / Web UI / tray；
- Git/Python/CMake 专用远程 API；
- 用户可见 session 管理体系；
- 自己的一套持久 shell-context 系统；
- 自动操纵 UU GUI。

项目价值来自低学习成本、可靠、无感和可移植，不是功能数量。

## Definition of done

不要把“代码写完”当完成。v1 必须在真实 Windows + 官方 UU 上端到端验证：

- 干净 workspace 首次绑定；
- 多设备同时开 Terminal 时选择正确且不会误绑；
- 新 Terminal 自动发现，交互短且不骚扰；
- 多 Terminal 并发和长任务；
- shell-local 状态规则；
- 连接异常不会重复执行命令；
- `put/get` 二进制 round-trip；
- UU/Terminal 重启和消失时行为安全；
- 空闲占用低；
- 安装、升级、卸载不影响官方 UU；
- 至少一台非开发机/干净环境从安装到使用通过。

实现者自行规划开发顺序、测试方式和 PR 粒度；不要为了遵循旧的实现建议而牺牲更好的方案。