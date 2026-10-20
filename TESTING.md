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
- 使用 fake/fixture session provider 做多设备、多 worker、busy/idle、断线、未知状态、并发和 no-replay 测试。

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
- 新 Terminal 打开后的人类 PowerShell 交互体验。

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
human decision: accept / skip / auto-accept-device
job: not-sent / running / finished / connection-unknown
```

关键行为必须固定：

- 没有歧义就不要问；
- 真有歧义才弹短 PowerShell；
- 弹窗不能污染 `uu` 的 stdout，也不能偷走命令 stdin；
- 一个等待中的 `uu` 请求明确需要某设备 Terminal 时，用户刚打开的合适 Terminal 应直接满足该请求，避免连续弹两次确认；
- 多个新 Terminal 短时间出现时尽量合并处理，不连续弹多个窗口；
- 用户 skip 的 Terminal 不得随后被自动调度；
- 已绑定 workspace 不因为其他设备上线而静默切换；
- 已经发送的命令绝不因为恢复/重连自动重复执行。

## 7. Shell-state isolation tests

除了验证“不同 `uu` 调用不应依赖上次状态”，还应专门验证状态不会意外泄漏成为下一条命令的隐式前提。

例如：

```text
job A: cd /tmp && export UU_TEST=1
job B: pwd && test -z "$UU_TEST"
```

job B 应从 workspace `remote_root` 开始，并且实现应尽可能隔离 job A 的 shell-local 变更。具体使用 subshell、fresh shell 或其他机制由实现者决定，但产品语义必须稳定。

`exit`、`exec`、失败命令、管道、重定向、带引号路径、Unicode 路径也应纳入命令包装测试。

## 8. Crash / recovery safety

Broker/后台组件崩溃或 CLI 客户端异常退出后，不允许把“可能仍在运行旧 job”的 Terminal 立即当 idle 使用。

实现者可以设计恢复协议，但必须满足：

- 不知道旧命令是否结束时，worker 进入 unknown/quarantine，而不是自动复用；
- 能可靠证明旧 job 已结束时才恢复 worker；
- 无法证明时宁可要求用户开新的 Terminal 或显式处理，也不能向可能繁忙的 PTY 注入新命令；
- 任何恢复流程都不得重放已经发送过的命令。

## 9. Release gate

一个 release candidate 至少应具备：

1. CI build/tests 全通过；
2. installer 在全新 hosted Windows job 中完成安装/基础诊断/卸载；
3. 如果 CI 环境允许，当前官方 UU 客户端下载、签名与组件发现 smoke test 通过；
4. 真实 Windows + 已登录官方 UU + 真实远端设备完成 shell、多 worker、长任务、异常恢复、`put/get` 验收；
5. 非开发机或干净 Windows 环境完成至少一次从安装到首次 binding 的完整体验验证。

只有 mock 通过不等于 v1 完成。
