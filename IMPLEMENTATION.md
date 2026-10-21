# Implementation contract

这不是逐步教程，也不规定语言、模块、IPC、进程模型或具体 Windows API。

实现者应自行选择最简单、最可靠、最可维护的方案。这里仅定义 **最终必须实现的行为、兼容性和安全边界**。如果实机证据表明某个既有设想不是最优，允许改变内部实现，但不得破坏 `DESIGN.md` 的公共体验。

## 1. 最终目标

在一台正常安装并登录官方网易 UU 远程的 Windows 电脑上，用户应能在任意本地项目中直接使用：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

首次配置和诊断最多需要：

```powershell
uu bind
uu list
```

保留词冲突时：

```powershell
uu -- <remote command...>
```

Codex 的日常心智模型只有一个：**本地命令前加 `uu`，就是在当前 workspace 绑定的远端工作目录执行。**

## 2. Workspace binding

一个本地 workspace v1 绑定：

```text
remote device + remote root
```

要求：

- 用户运行 `uu bind` 时，调用时的当前目录精确作为 workspace root，即使位于 Git 仓库内；
- 子目录调用 `uu` 能稳定找到正确 binding；
- binding 放在用户侧，不污染项目仓库；
- v1 一个 workspace 只绑定一台远端设备；
- Codex 不选择、不看到裸 `deviceId`，也不替用户运行 `uu bind`/`uu list`；
- 通常只有一个新出现且可用的 detached Terminal，用户运行 `uu bind` 即授权；候选不唯一则返回歧义诊断，不弹选择器、不猜；
- 同一远端设备可以服务多个本地 workspace，这些 workspace 共享设备级 worker pool，但各自 remote root 必须严格独立。

## 3. Terminal 的产品语义

官方 UU Terminal 由用户自己打开。`uu-cli` 可按需刷新库存，但只有此前由用户运行 `uu bind` 唯一绑定的 Terminal 才是 worker。

Terminal 只是执行资源，不是 Agent 的长期上下文：

- 一个 worker 同时最多执行一个 job；
- 长任务持续占用原 worker，输出实时返回；
- 其他命令可使用其他空闲 worker；
- 已授权 worker 全忙时排队并报告队列位置，不抢占或 kill；
- 绝不跨设备调度；
- session name/id 不应成为日常用户概念；
- 同设备多个 workspace 不需要把 Terminal 再分组，job 自己携带 remote root。

## 4. 手动 binding 与 Terminal 授权

只有用户亲自运行 `uu bind` 才会绑定 workspace 或授权 Terminal。交互直接显示在该命令的调用终端，不创建弹窗/独立 console。Agent 永远不得代跑 `uu bind` 或 `uu list`。

- 用户在官方 UU 中逐个打开专用 Terminal；按需关闭各自的官方标签页以释放连接，然后在目标 workspace 运行 `uu bind`；
- `uu bind` 只接受唯一新出现且可用的 detached Terminal；用户运行命令即明确交给 uu-cli，不额外询问。候选不唯一则诊断并结束，由用户整理后重跑；
- 当前目录精确保存为 local workspace root，不自动改成 Git root；
- 首次 binding 在调用终端确认 remote root，默认值可以来自唯一 Terminal 的 `pwd`；
- 对已绑定 workspace 重复运行 `uu bind`，为同一设备 pool 加入唯一候选 Terminal，并保留已记住的 remote root；只有用户明确选择更改 remote root 时才更新；
- 没有可用 Terminal 时，`uu bind` 返回明确提示，让用户打开专用 Terminal 后再次手动运行；不等待 Terminal 出现；
- 自动发现/库存刷新永不构成授权。普通 `uu <command>`、`put`、`get` 不自动 setup、绑定、授权或弹窗。缺少 binding/已授权 worker 时返回清楚诊断，要求用户运行 `uu bind`；
- 已授权 worker 全忙时 job 排队并报告队列位置，不取消或 kill 其他 job；unknown/quarantined worker 必须继续隔离，`uu bind` 也不能把它当成空闲 worker。

`uu list` 是用户手动执行的只读状态查看。Agent 直接执行任务，不做配置或状态预检；仅实际执行受阻时简短报告具体错误一次，必要时请求最少的人类介入，不反复安排用户检查或重绑。

## 5. Shell context

这是硬契约：**不同 `uu` 调用之间不保证共享 shell-local 状态，而且实现应尽量主动隔离普通 job 的 shell-local 变化。**

Agent/用户不能依赖上一条命令留下的：

- `cd`；
- `export`；
- `source` / virtualenv；
- alias；
- shell options；
- 其他只存在于某个 shell 进程内的状态。

需要连续状态时放在同一次调用中，例如：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每个普通 job 从 workspace 的 `remote_root` 开始，并应在一个 job-local shell scope 中运行，使 `cd/export/source/exit/exec` 等不会无意成为下一条 job 的隐式输入。

具体使用 subshell、fresh shell 或其他机制，由实现者根据真实 PTY、交互和 Ctrl+C 行为自行决定。

最终 Skill 必须明确写出这一点。

## 6. 命令行为

`uu <command>` 应尽量表现得像直接在远端 shell 中执行：

- 输出实时可见；
- 长任务不因工具自己的短超时被截断；
- 能可靠判断一次调用的完成边界和 exit code；
- PTY stdout/stderr 如果天然合流，不伪造不存在的分离；
- Ctrl+C/取消行为尽量符合普通终端预期；
- 已经发送到远端的命令，连接异常后不得自动在另一个 Terminal 重放；
- 无法确认执行结果时明确返回 unknown/may-still-be-running，而不是猜测成功或再次执行。
- binding 缺失或没有已授权 worker 时指出“请用户运行 `uu bind`”；存在已授权 worker 但都在忙时可以等待空闲 worker。

具体如何实现命令边界、attach、恢复和并发，由实现者根据现有研究和实机结果决定。

## 7. Crash / recovery safety

后台组件、CLI 客户端或本地连接异常不能导致 worker 被错误复用。

必须满足：

- 如果某个 worker 在崩溃前可能仍有已发送 job，恢复后不能直接视为 idle；
- 能证明旧 job 已结束才可安全重新加入 idle pool；
- 无法证明时进入 unknown/quarantine，等待、重新打开 Terminal 或使用其他 worker；
- 绝不能为了“恢复”而向可能仍繁忙的 PTY 注入新命令；
- 绝不能重放已经发送过的旧命令。

恢复机制如何记录/证明由实现者决定。

## 8. 不干扰官方 UU — 硬边界

`uu-cli` 是官方 UU 的旁路辅助工具，不是它的补丁或替代品。

必须满足：

- 不修改、替换、注入、patch 官方 UU/GameViewer 二进制；
- 不修改官方 UU 的账号、登录、设备、更新、启动等配置；
- 不保存或导出 UU 密码、Token、Cookie、local-host token 等认证材料；
- 不杀死、关闭、重命名或偷偷接管用户未授权给 uu-cli 的 Terminal；
- 不改变用户正在手工操作的 Terminal 的输入/状态；
- 不因为 uu-cli 崩溃、升级、卸载而破坏 UU 自身功能；
- 不依赖签名伪造、认证绕过、GUI 自动点击或进程注入；
- 如果某种官方 attach/reuse 行为会明显影响用户当前 UU 窗口，只能用于用户已经明确交给 uu-cli 的专用 Terminal，或换更安全方案。

研究和调用官方已存在的本地互操作接口可以，但必须保持上述边界。

## 9. 尽可能在任何正常电脑上可用

目标不是“只在开发者这台电脑能跑”，而是：

> 在官方 UU Remote 本身能够正常工作的受支持 Windows 电脑上，安装 uu-cli 后无需手工改路径、改源码或配置开发环境即可使用。

因此实现必须尽量做到：

- 自动发现官方 UU 安装位置和相关组件，不硬编码开发机路径；
- 不硬编码用户名、盘符、deviceId、session name、安装版本或本机目录；
- 优先能力探测/兼容性检测，而不是只认一个精确版本号；
- 不要求用户安装 Python、Rust、Node、PowerShell 7、WSL 等开发运行时；
- 安装和日常运行尽量使用普通用户权限，不无故要求管理员；
- 配置只保存在当前用户合理的应用数据目录；
- 升级能够保留有效 binding，卸载只清理 uu-cli 自己的内容；
- 官方 UU 不在预期位置、版本行为变化或缺少能力时，给出明确诊断，而不是静默失效；
- 对不同机器上的路径、设备名、Windows 用户名、显示语言等差异保持稳健。

支持范围应跟随官方 Windows UU 实际支持的平台；不要把某台开发机的偶然环境当成产品前置条件。

## 10. 后台生命周期与资源占用

用户希望：官方 UU 在用时，uu-cli 随时可用；没有任务或没有可用 Terminal 时，资源占用尽可能低。

实现方式自由，但最终必须满足：

- 用户不需要手工启动/停止第二个服务；
- 可按需刷新 Terminal 库存，但发现本身不能授权新 Terminal；
- 空闲时 CPU 接近 0，不做高频轮询或持续 spawn 官方 CLI；
- 没有必要时不维持昂贵连接；
- 无额外 GUI、tray 或常驻可见 console；
- 只有用户运行 `uu bind` 时才在当前调用终端交互；普通命令和后台流程不弹窗、不做授权；
- 后台组件缺失/崩溃后，下一次 `uu` 能自动恢复或给出可理解错误；
- uu-cli 的后台生命周期不能拖累或阻止官方 UU 正常启动、退出、升级。

是否采用 broker、watcher、事件监听、lazy process 或其他结构，由实现者自行决定。

## 11. 文件传输

公共接口固定：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

必须：

- 任意二进制安全；
- 成功/失败语义明确；
- 不完整传输不能报告成功；
- 远端相对路径以 workspace `remote_root` 为基准；
- 绝对路径保持远端绝对语义；
- 若支持 `~`，必须按远端环境解析；
- 尽可能复用官方稳定能力；
- 内部传输机制不暴露给 Agent。

如果原生路径不可复用，实现者可自行选择可靠 fallback，但不要为了实现方便改变公共接口或破坏文件完整性。

## 12. CI-first 验证

能离开开发者主力电脑完成的事情，优先交给 GitHub Actions。

实现者应主动建立 Windows CI 来覆盖：

- build / lint / unit / integration-with-fakes；
- 多 workspace / 多 device / 多 worker 的逻辑状态矩阵；
- shell-state isolation、命令包装、no-replay、unknown/quarantine；
- installer 构建；
- 在新的 hosted Windows job 中安装、PATH、后台集成、升级/重复安装、卸载和残留检查；
- 无 UU、UU 未运行、组件缺失、不兼容版本等错误路径；
- 如果 hosted runner 环境允许，从官方公开来源下载当前 Windows UU installer，验证 Authenticode/版本/hash，并做安装发现和兼容性 smoke test。

官方 UU 安装包不得提交或重新分发到仓库；CI 只从官方来源临时获取。不得把真实 UU 账号凭据放入 GitHub Secrets 去模拟用户登录。

可以使用 scheduled/manual workflow 定期检查最新官方 UU 的安装/组件发现是否发生变化。

如果 GitHub-hosted Windows 的 OS/SKU 本身不受官方 UU 支持，官方 UU 安装 smoke 可以作为 non-gating/manual compatibility signal；不能为了让 CI 绿而伪装成“真实 UU 已验证”。核心 build/test/installer clean-state CI 仍应正常 gating。

真实登录态、真实 Terminal attach/reuse、真实远端设备、多 Terminal、长任务、Ctrl+C 和真实文件通道仍需要最小必要的实机验收；mock 或 hosted CI 不能冒充这部分已经通过。

详细测试策略见 `TESTING.md`。

## 13. Agent Skill

Skill 要极短，只教三件事：

```text
远端执行：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

再加一条硬规则：

> 不要依赖不同 `uu` 调用之间的 shell-local 状态；需要连续 `cd/source/export` 时放进同一条命令。普通 job 从 workspace 的 remote root 开始。

Agent 不管理 deviceId、session、worker 或 uu-cli 后台生命周期，不运行 `uu bind` / `uu list`，不把正常任务变成配置和诊断流程；真正受阻才简报一次，排队则等待原进程。

## 14. 完成标准

实现者自行决定开发顺序和代码结构，但 v1 完成前必须证明：

- 全新 workspace 由用户手动运行 `uu bind` 完成首次 binding；
- 唯一新 Terminal 可被 `uu bind` 明确授权；候选不唯一时诊断退出，不弹窗、不猜、不绑定；
- `uu bind` 当前目录精确成为 workspace root，包括 Git 仓库内目录；
- Terminal 授权只由用户运行 `uu bind` 完成，且在调用终端内无弹窗或二次确认；
- 重复 `uu bind` 可为同设备 pool 增加 Terminal，保留 remote root；
- 普通命令/文件传输不自动绑定或授权；缺少 binding/worker 时给出运行 `uu bind` 的提示；
- 已授权 worker 全忙可排队并报告位置，不取消/kill 其他 job；unknown/quarantined worker 不复用；
- 用户选择不会污染普通 job stdin/stdout；
- 同一设备多个 workspace 共享 pool 时不会串 remote root；
- 多个 Terminal 能安全并发调度；
- 长任务实时输出且不阻塞其他 worker；
- 普通 job 不依赖且尽可能不泄漏 Terminal 历史 `cd/source/export` 状态；
- 已发送命令不会因内部恢复逻辑重复执行；
- crash/recovery 不会把可能繁忙的 worker 错当 idle；
- `put/get` 二进制 round-trip 一致；
- UU 重启、Terminal 消失、网络异常、后台组件重启时行为可理解且不串设备；
- 空闲占用低；
- 安装、升级、卸载不影响官方 UU；
- CI clean-machine 路径通过；
- 至少一个真实非开发/干净环境完成从安装到当前目录首次 `uu bind` 的完整体验验证。

只要满足这些目标和 `DESIGN.md` 的公共契约，实现者有充分自由选择技术方案。
