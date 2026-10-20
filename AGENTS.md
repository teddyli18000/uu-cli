# AGENTS.md

## Project intent

`uu-cli` 的目标是把 Windows 本地已经登录的网易 UU 远程变成一个几乎透明的远端 shell + 文件通道。

开始实现前读完：

1. `README.md`
2. `DESIGN.md`
3. `ARCHITECTURE.md`
4. `IMPLEMENTATION.md`
5. `TESTING.md`
6. `ROADMAP.md`

研究资料用于理解已经确认的 UU 行为，但不要把研究过程本身变成产品复杂度。

## Your role

你是主要实现者。请自行完成技术选型、代码组织、开发顺序、CI、installer、实机调试和兼容性处理。

不要等待别人逐步告诉你该用什么语言、Windows API、IPC、进程模型或 crate/library。只要不违反产品契约和硬边界，内部实现由你决定。

遇到文档中的实现设想与真实 Windows + 官方 UU 行为冲突时，以实机证据为准；优先保住公共体验、安全边界和可移植性，然后选择更简单稳健的方案，并同步更新必要文档。

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

## Final product model

```text
Local Workspace
      ↓
Remote Binding = one device + remote root
      ↓
Device-level Terminal Worker Pool
```

关键规则：

- v1 一个 workspace 只绑定一台远端设备；
- Codex 不选择 deviceId；
- 同一 remote device 可以服务多个 workspace；
- worker pool 属于 device，不属于 workspace；
- 每个 job 携带自己的 workspace remote root；
- 用户在官方 UU 中手工打开 Terminal；
- uu-cli 自动发现并只使用被允许的 Terminal；
- Terminal 是 worker，不是 Agent 的长期 shell 上下文；
- 多 Terminal 用于并发；长任务持续占用自己的 worker 并实时输出；
- 不跨设备调度。

## Shell context — HARD RULE

**不同 `uu` 调用之间不得依赖 shell-local 状态，而且普通 job 应尽量隔离自己的 shell-local 变化。**

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

普通 job 从 workspace binding 的 `remote_root` 开始。实现应尽量让本次 `cd/export/source/exit/exec` 不泄漏成为下一条 job 的隐式上下文。

最终 Agent Skill 必须明确写出这条规则。

## Interaction UX — HARD RULE

目标是：**没有歧义就自动，有歧义才问，同一个决定只问一次。**

必须遵守：

- `uu` 正在等待某个明确设备的 worker 时，用户刚打开的该设备 Terminal 直接满足请求，不要再弹一次多余确认；
- workspace 未绑定且正在提示用户“打开想使用设备的 Terminal”时，唯一新出现的 Terminal 可以同时完成设备识别和本次 worker 授权；
- 没有等待请求时，新 Terminal 若未启用自动加入，只弹一个短 PowerShell：Enter=使用，N=跳过，A=该设备以后自动；
- 同一设备多个 workspace **不是** Terminal 归属歧义，因为它们共享 device-level pool；
- 多个新 Terminal 短时间出现时尽量合并提示；
- 用户 skip 的 session 之后不得偷偷使用；
- 已确认过的 binding/session/device preference 不重复骚扰。

人类选择和 job I/O 必须分离：选择器不能污染 `uu` stdout，也不能消费远端命令 stdin。需要人决定时用独立、短、可取消的 PowerShell 交互或等价方式。

## Workspace and device UX

- Git repo 默认以 Git root 作为 workspace；非 Git 目录采用合理根目录；
- binding 存在用户侧，不污染 repo；
- 从子目录运行时使用最近的 workspace binding；
- 多设备同时可用时，只让用户按 friendly name 做一次短选择；
- 不要求用户或 Codex 看到裸 deviceId；
- 绝不能根据相似名字猜错设备；
- 已绑定后日常 `uu` 不再要求设备选择；
- remote root 首次可用所选 Terminal 的 `pwd` 作默认值，用户 Enter 接受或输入修改，之后记住。

## Terminal bootstrap and discovery

当前官方 Windows UU 的研究结论已经收口：已有 Terminal 可以查询/附着/复用，但第一个官方 Terminal 没有稳定外部 headless 创建入口。因此用户负责在官方 UU 中打开 Terminal，uu-cli 负责之后的发现和使用。

不要为了“自动创建第一个 Terminal”重新走注入、签名伪造、host-registration 绕过或 GUI 自动点击。

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

用户通过 uu-cli 的明确 PowerShell 交互把某个 Terminal 交给 uu-cli 后，可以把它作为专用 worker；在此之前不能静默接管。

如果某个官方 attach/reuse 路径会干扰用户未授权的 UU 窗口，不要硬用；换安全方案。

## Job safety

实现必须保证：

- 输出实时返回；
- 长任务可用；
- 单次命令完成边界和 exit code 可靠；
- stdout/stderr 如果 PTY 合流则保持事实，不伪造分离；
- Ctrl+C/取消尽量符合普通终端预期；
- 一个 worker 同时最多一个 job；
- 已经发送的命令在连接异常后绝不自动换 Terminal 重放；
- 结果不确定时明确报告 unknown/may-still-be-running。

后台组件/CLI 崩溃后，任何可能还有旧 job 的 worker 不能直接当 idle。必须可靠证明安全，或者标为 unknown/quarantine。宁可开新 Terminal，也不能往可能繁忙的 PTY 注入新命令。

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
- 不兼容时给出明确诊断，不能静默失败。

## Background behavior

用户不应该管理第二套服务。

最终行为应满足：

- 官方 UU 在用时，uu-cli 随时可用；
- 自动发现用户新开的 Terminal；
- 无任务、无 Terminal 或 UU 未运行时资源占用尽可能低；
- 禁止永久高频 polling/spawn 官方 CLI；
- 无 tray、额外 GUI、常驻可见 console；
- 只有真正需要人类决定时才弹短 PowerShell；
- 后台组件异常后，下一次 `uu` 可以自动恢复或明确报错；
- 不拖累 UU 的启动、退出、升级。

是否采用 broker、watcher、event notification、named pipe、lazy process 等完全由你决定。

## File transfer

公共接口固定：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

必须二进制安全、明确成功/失败、不完整传输不能报告成功。

远端相对路径相对于 workspace `remote_root`；绝对路径保持远端绝对语义；若支持 `~`，必须按远端环境解析。

优先利用稳定官方能力，但内部方案由你根据研究和实测决定。

## CI-first — HARD REQUIREMENT

尽可能把构建、故障注入、安装和兼容性测试放到 GitHub Actions，减少开发者主力电脑的消耗和风险。

- 建立 Windows CI；
- pure logic / fake provider / concurrency / no-replay / quarantine 全部自动测；
- installer 在新的 hosted Windows job 里安装、检查 PATH/后台集成、再卸载并检查残留；
- 测无 UU、UU 未启动、组件缺失、不兼容等路径；
- 需要官方 UU installer 时，只在 workflow 运行时从网易/UU 官方公开来源下载；不要提交或重新分发；
- 对下载的官方二进制验证 Authenticode/版本/hash；
- 可以做 scheduled/manual compatibility smoke，主动发现官方 UU 更新造成的兼容性变化；
- 不要把真实 UU 账号凭据放进 CI。

GitHub-hosted CI 无法证明真实登录、真实 Terminal attach、多 Terminal、远端设备、Ctrl+C 和文件通道；这些只做最小必要实机验收。不要拿 mock 冒充端到端通过。

详见 `TESTING.md`。

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

不要把“代码写完”当完成。v1 必须按 `TESTING.md` 做 CI + 真实环境分层验证，并至少证明：

- 干净 workspace 首次绑定；
- 多设备同时开 Terminal 时选择正确且不会误绑；
- 等待中的 `uu` + 新 Terminal 不重复询问；
- 新 Terminal 自动发现，交互短且不骚扰；
- 同设备多 workspace 共享 worker pool 不串 remote root；
- 多 Terminal 并发和长任务；
- shell-local 状态不成为跨 job 隐式上下文；
- 连接异常不会重复执行命令；
- crash/recovery 不会错误复用 busy worker；
- `put/get` 二进制 round-trip；
- UU/Terminal 重启和消失时行为安全；
- 空闲占用低；
- CI clean-machine installer/卸载通过；
- 安装、升级、卸载不影响官方 UU；
- 至少一个真实非开发/干净环境从安装到使用通过。

实现者自行规划开发顺序、测试方式和 PR 粒度；不要为了遵循旧实现建议而牺牲更好的方案。