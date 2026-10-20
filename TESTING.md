# Testing and validation

这份文档定义 `uu-cli` 的验证策略。原则是：**能在干净 CI 虚拟机里验证的事情，不消耗开发者自己的 Windows；只有必须依赖真实登录态、真实远端设备和交互式 UU Terminal 的部分才落到本地实机。**

## 1. CI-first

仓库是公开仓库，应充分利用 GitHub-hosted Windows runners 做构建、测试、打包和干净环境验证。

每个 PR 至少应尽可能覆盖：

- build / lint / unit tests；
- 配置、workspace lookup、调度、命令包装、错误语义等纯逻辑测试；
- installer 构建；
- 在新的 Windows runner 上安装构建出来的 installer；
- 验证 PATH、文件位置、启动项/后台组件、重复安装/升级、卸载和残留；
- 验证程序在没有 UU、UU 未运行、找不到兼容组件时能给出正确诊断，而不是崩溃；
- 使用 fake/fixture session provider 做多设备、多 workspace、多 worker、busy/idle、断线、未知状态、并发和 no-replay 测试。

不要因为开发机上“已经能跑”就跳过 CI 中的 clean-state 测试。

## 2. 官方 UU 客户端在 CI 中的使用

如果某项兼容性测试需要官方 Windows UU 安装包，实现者应在 workflow 运行时从 **网易/UU 的官方公开来源** 获取当前安装包，而不是把安装包提交到仓库或使用第三方镜像。

要求：

- 不在仓库中重新分发官方安装包；
- 不从搜索结果、网盘、镜像站或未知 CDN 直接信任二进制；
- 下载后验证 Windows Authenticode 签名/发布者等可以验证的来源信息；
- 记录测试时实际使用的文件版本和 hash，方便兼容性追踪；
- workflow 结束后由临时 runner 自然清理，不把官方二进制打进 uu-cli release artifact；
- 官方下载入口发生变化时，优先更新发现逻辑/测试，不硬编码开发机上的安装包路径。

可以设置 scheduled/manual compatibility workflow，定期使用当前官方客户端做“下载 → 签名检查 → 安装 → uu-cli 发现/兼容性诊断 → 卸载”烟雾测试，从而尽早发现 UU 更新造成的变化。

如果 GitHub-hosted Windows 的 OS/SKU 本身不受官方 UU 支持，这类官方客户端安装 smoke 可以 non-gating/manual，并把原因写清楚；不能为了 CI 绿色伪造“真实 UU 已验证”。

## 3. GitHub-hosted runner 能证明什么

Hosted Windows runner 很适合验证：

- 新 VM / 新用户状态下构建和安装；
- installer 可重复性；
- PATH / startup / uninstall；
- 安装路径自动发现逻辑；
- 对不同 Windows runner image 的兼容性；
- 无 UU / UU 未运行 / 不兼容版本等错误路径；
- 如果官方 UU 可以在该 runner 环境安装，则继续验证安装发现、组件定位、版本/能力探测。

这些测试应尽量替代开发者电脑上的反复安装、卸载和破坏性试验。

## 4. Hosted CI 不能假装证明什么

GitHub-hosted runner **不能被当作真实 UU 端到端环境的替代品**。以下内容通常仍需要真实交互式 Windows + 正常登录的官方 UU + 真实远端设备：

- UU 账号登录后的设备列表和真实权限；
- 用户从官方 GUI 手工打开 Terminal；
- 真实 session attach/reuse 是否干扰官方窗口；
- 长任务、Ctrl+C、交互 stdin、真实 PTY 数据流；
- 多个真实 UU Terminal 的并发；
- UU 重启、远端网络异常、真实 session 消失；
- 原生文件通道的真实 round-trip；
- 新 Terminal 打开后的人类交互体验。

CI 做不到时要明确标记 `requires-real-uu`，不能通过 mock 以后声称端到端已经验证。

## 5. 本地实机测试最小化

本地 Windows 只承担 CI 无法覆盖的最终互操作测试。

原则：

- 先让 PR 在 CI 完全通过，再碰开发者电脑；
- 尽量使用 uu-cli 自己的 installer/release candidate 测，而不是开发目录中的临时脚本；
- 不修改官方 UU 二进制或配置来“方便测试”；
- 不把真实账号凭据、token、device id 等上传到 Actions artifact/log；
- 需要破坏性测试时优先用临时 Windows VM/额外用户配置，而不是主力系统；
- 测完应证明 uu-cli 可以完整卸载，官方 UU 仍正常。

## 6. 交互逻辑测试矩阵

至少覆盖以下状态组合：

```text
workspace: bound / unbound
UU: absent / installed-stopped / running
worker: none / idle / all-busy / disappears
remote devices: one / multiple
new terminal: waiting-request / unsolicited
human decision: accept / reject / cancel-close
job: not-sent / running / finished / connection-unknown
```

关键行为必须固定：

- 没有歧义就不要问；
- 真有歧义才弹短人类选择；
- 弹窗不能污染 `uu` 的 stdout，也不能偷走命令 stdin；
- 一个等待中的 `uu` 请求在提示中明确说明“新打开的匹配 Terminal 将用于本次请求”，随后用户打开它时不得再弹第二个重复授权；
- 没有等待请求时，新 Terminal 必须经过一次授权；N/Esc/关闭/取消都表示不使用；
- v1 不存在“设备以后所有新 Terminal 自动接管”的持久设置；
- 多个新 Terminal 短时间出现时尽量在一个交互窗口处理，不连续抢焦点；
- 同一设备多个 workspace 不构成 Terminal 归属歧义；
- 用户未授权的 Terminal 不得随后被自动调度；
- Terminal 授权必须绑定到可验证的 session 实例；如果 UU 重启/重建后只能看到相同名字、却无法证明还是同一实例，不得仅凭 session name 继承旧授权；
- 已绑定 workspace 不因为其他设备上线而静默切换；
- 已经发送的命令绝不因为恢复/重连自动重复执行；
- 交互实现不能要求 PowerShell 7 预装，fallback 也必须测试。

## 7. Shell-state isolation tests

除了验证“不同 `uu` 调用不应依赖上次状态”，还应专门验证状态不会意外泄漏成为下一条命令的隐式前提。

例如：

```text
job A: cd /tmp && export UU_TEST=1
job B: pwd && test -z "$UU_TEST"
```

job B 应从 workspace `remote_root` 开始，并且实现应尽可能隔离 job A 的 shell-local 变更。具体使用 subshell、fresh shell 或其他机制由实现者决定，但产品语义必须稳定。

`exit`、`exec`、失败命令、管道、重定向、带引号路径、Unicode 路径也应纳入命令包装测试。

还要覆盖 CLI 保留词 escape：远端命令名为 `put/get/bind/list` 时，`uu -- ...` 必须强制按远端命令处理。

## 8. Crash / recovery safety

后台组件崩溃或 CLI 客户端异常退出后，不允许把“可能仍在运行旧 job”的 Terminal 立即当 idle 使用。

实现者可以设计恢复协议，但必须满足：

- 不知道旧命令是否结束时，worker 进入 unknown/quarantine，而不是自动复用；
- 能可靠证明旧 job 已结束时才恢复 worker；
- 无法证明时宁可要求用户开新的 Terminal 或显式处理，也不能向可能繁忙的 PTY 注入新命令；
- 任何恢复流程都不得重放已经发送过的命令。

## 9. File-path semantics tests

`put/get` 至少验证：

- remote relative path 相对于 workspace `remote_root`；
- remote absolute path 不被改写成本地语义；
- 若支持 `~`，按远端用户 home 解析；
- Windows 本地 Unicode/空格路径；
- 远端 Unicode/空格路径；
- 0-byte、小文件、大文件、随机二进制；
- 失败/中断时不报告成功。

## 10. Workspace path tests

Windows workspace key 至少验证：

- drive-letter/case 差异不会把同一目录误当两个 binding；
- trailing separator 不产生重复 binding；
- 从子目录能选择最近的已存在 binding；
- nested Git repo/submodule 不会错误跳到更远的 parent binding；
- 空格和 Unicode 本地路径可用。

具体 canonicalization 算法由实现者决定，但用户不能因为 Windows 路径表示方式变化而随机切错 workspace。

## 11. Release gate

一个 release candidate 至少应具备：

1. CI build/tests 全通过；
2. installer 在全新 hosted Windows job 中完成安装/基础诊断/卸载；
3. 如果 CI 环境允许，当前官方 UU 客户端下载、签名与组件发现 smoke test 通过；如果环境不支持，清楚标记而不是伪造通过；
4. 真实 Windows + 已登录官方 UU + 真实远端设备完成 shell、多 worker、长任务、异常恢复、`put/get` 验收；
5. 非开发机或干净 Windows 环境完成至少一次从安装到首次 binding 的完整体验验证。

只有 mock 通过不等于 v1 完成。

## 当前实现与证据（2026-10-20）

当前为 PR [#7](https://github.com/teddyli18000/uu-cli/pull/7) 中的原生 Windows 验证候选，尚未满足下面的真实 UU release gate。产品代码、构建、测试和安装检查在远端完成。之后经用户授权，仅在开发者电脑运行独立观察探针，连接用户交出的专用 Terminal；没有本地安装产品或修改官方 UU。

| 层级 | 已验证范围 | 证据 |
| --- | --- | --- |
| 纯逻辑与真实 Bash | 授权实例、设备选择、workspace、shell 隔离、并发锁、崩溃 quarantine、no-replay；真实持久 Bash 的 shell 隔离 | [Actions prepare](https://github.com/teddyli18000/uu-cli/actions/runs/39433019505) |
| 文件传输 | 真实 Bash 中随机二进制、空文件、Unicode/空格、绝对路径、tilde；中断、unknown、完整性错误时保留目标且不报告成功 | [Actions transfer](https://github.com/teddyli18000/uu-cli/actions/runs/39433019285) |
| 干净 Windows 安装 | 原生包在另一台全新 hosted VM 安装、真实后台进程唯一性、空闲 CPU、无 UU 诊断、升级、锁定文件失败、PATH、卸载、保留 binding/quarantine；Windows PowerShell 5.1 | [Actions installer](https://github.com/teddyli18000/uu-cli/actions/runs/39433019285) |
| 官方安装发现 | 运行时从官方来源下载，验证 Authenticode/版本/hash，安装后由 uu-cli 发现组件并保留官方“未运行”诊断，卸载完成 | [Official UU smoke](https://github.com/teddyli18000/uu-cli/actions/runs/39412368759) |

官方安装 smoke 使用的 Windows UU 为 `4.43.0.3607`，NetEase 签名有效；安装包 SHA256 为 `F26F84F01551538B9F6996D5CF0CCBF884FB12731903A4F3EFA73BE712C44DA4`。这只证明该次下载、安装、发现与未运行诊断，不证明登录或真实 Terminal attach。

以下仍为 **requires-real-uu**，不得标记通过：首次 binding 的完整人类交互、多设备/新 Terminal 的真实授权体验、完整产品 attach/reuse、真实多 Terminal 并发与长任务、产品 stdin/Ctrl+C、UU/Terminal/网络重启、真实文件 round-trip，以及至少一台非开发环境从安装到首次绑定和执行。正式发布应逐项记录真实验收结果，再决定 release；当前没有发布正式版本。


### 独立真机命令通道观察

用户提供已登录的官方 UU 和明确交出的专用 Linux/bash Terminal。观察结果见 [REAL_UU_FINDINGS.md](REAL_UU_FINDINGS.md)；早期原始收发字节及真实设备标识只保存在本地忽略目录。

[探针 Actions 39431629252](https://github.com/teddyli18000/uu-cli/actions/runs/39431629252) 编译生产提交 3c2e44725b8f80ac09c593482ebcc964190c27c6 未修改的 job/runner/transport 模块。通过真实官方 CLI/ConPTY，实际 transport attach 后立即调用目录捕获，约 400 ms 验证并读回预期绝对路径；随后同一附着内完成退出码 0/7、Unicode、两秒间隔实时输出。sleep 30 在两秒时排入 Interrupt，实际 runner 于发送命令后 2.064 秒返回 130，中断后下一条命令成功。

探针释放自己的本地 attach 子进程，无未完成任务。查询确认原远端实例仍在且 detached，另一未授权运行中 Terminal 的身份/状态未变，探针从未连接它。本轮记录实际 API 输出及时间点；transport 私有 RX/TX 未导出，不把 API 日志称作原始字节日志。

这证明当前模块组成的最小命令闭环。完整 CLI、绑定/授权交互、真实断线恢复、多 worker、真实文件传输及其他 release gate 仍需逐项验收；不据此宣称 v1 完成。ConPTY 输出保持 presentation bytes，不承诺等于远端原始字节流。

命令通道收口提交 8e1d3624d33586608d644ed9aced1f342e4a2afd 的 Windows/Linux fmt、Clippy、tests、release build 全部通过；Windows 四个 CLI integration 包括真实 Git Bash 二进制 put/get 往返均通过。独立 installer/transfer workflow 的三个 job 全通过。此提交相对真机探针引用的 runtime 仅新增测试夹具与断言调整（及文档），不把 fake-provider/Git Bash 结果当作真实 UU 文件通道验收。
