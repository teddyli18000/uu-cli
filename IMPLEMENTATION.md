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

Codex 的日常心智模型只有一个：**本地命令前加 `uu`，就是在当前 workspace 绑定的远端工作目录执行。**

## 2. Workspace binding

一个本地 workspace v1 绑定：

```text
remote device + remote root
```

要求：

- Git 仓库中默认以 Git root 作为 workspace；非 Git 目录使用合理的最近根目录/当前目录；
- 子目录调用 `uu` 能稳定找到正确 binding；
- binding 放在用户侧，不污染项目仓库；
- v1 一个 workspace 只绑定一台远端设备；
- Codex 不选择、不看到裸 `deviceId`；
- 多设备同时存在时，由用户通过极短的人类交互选择一次，之后记住。

## 3. Terminal 的产品语义

官方 UU Terminal 由用户自己打开。`uu-cli` 负责发现已经存在的 Terminal，并把被允许使用的 Terminal 当成 worker。

Terminal 只是执行资源，不是 Agent 的长期上下文：

- 一个 worker 同时最多执行一个 job；
- 长任务持续占用原 worker，输出实时返回；
- 其他命令可使用其他空闲 worker；
- worker 全忙时等待，不抢占；
- 绝不跨设备调度；
- 具体 session name/id 不应成为日常用户概念。

### 新 Terminal 的交互

目标是尽量无感：

- 当某个 `uu` 调用正在等待可用 Terminal 时，用户刚打开的目标设备 Terminal 应被自动发现并继续原请求；
- 如果新 Terminal 的归属完全明确，可以直接加入对应 worker pool；
- 如果存在歧义（多个设备、多个 workspace、或无法判断是否供 uu-cli 使用），允许自动弹出一个非常短的 PowerShell 选择窗口；
- 选择器优先提供默认项和数字键/Enter 操作，不展示协议字段；
- 不反复询问已经确认过的同一绑定/Terminal；
- 用户明确不交给 uu-cli 的 Terminal 不得被使用。

交互设计以“几秒内完成、不需要理解 UU 内部机制”为标准。

## 4. Shell context

这是硬契约：**不同 `uu` 调用之间不保证共享 shell-local 状态。**

因此 Agent/用户不能依赖上一条命令留下的：

- `cd`；
- `export`；
- `source` / virtualenv；
- alias；
- 其他只存在于某个 shell 进程内的状态。

需要连续状态时放在同一次调用中，例如：

```powershell
uu "cd src && source .venv/bin/activate && python main.py"
```

每个普通 job 从 workspace 的 `remote root` 开始。后续 Skill 必须明确写出这一点。

## 5. 命令行为

`uu <command>` 应尽量表现得像直接在远端 shell 中执行：

- 输出实时可见；
- 长任务不因工具自己的短超时被截断；
- 能可靠判断一次调用的完成边界和 exit code；
- PTY stdout/stderr 如果天然合流，不伪造不存在的分离；
- Ctrl+C/取消行为尽量符合普通终端预期；
- 已经发送到远端的命令，连接异常后不得自动在另一个 Terminal 重放；
- 无法确认执行结果时明确返回未知状态，而不是猜测成功或再次执行。

具体如何实现命令边界、attach、恢复和并发，由实现者根据现有研究和实机结果决定。

## 6. 不干扰官方 UU — 硬边界

`uu-cli` 是官方 UU 的旁路辅助工具，不是它的补丁或替代品。

必须满足：

- 不修改、替换、注入、patch 官方 UU/GameViewer 二进制；
- 不修改官方 UU 的账号、登录、设备、更新、启动等配置；
- 不保存或导出 UU 密码、Token、Cookie、local-host token 等认证材料；
- 不杀死、关闭、重命名或偷偷接管用户未授权给 uu-cli 的 Terminal；
- 不改变用户正在手工操作的 Terminal 的输入/状态；
- 不因为 uu-cli 崩溃、升级、卸载而破坏 UU 自身功能；
- 不依赖签名伪造、认证绕过、GUI 自动点击或进程注入；
- 如果某种官方 attach/reuse 行为会明显影响用户当前 UU 窗口，必须换更安全的实现或要求使用专门提供给 uu-cli 的 Terminal，不能静默干扰。

研究和调用官方已存在的本地互操作接口可以，但必须保持上述边界。

## 7. 尽可能在任何正常电脑上可用

目标不是“只在开发者这台电脑能跑”，而是：

> 在官方 UU Remote 本身能够正常工作的受支持 Windows 电脑上，安装 uu-cli 后无需手工改路径、改源码或配置开发环境即可使用。

因此实现必须尽量做到：

- 自动发现官方 UU 安装位置和相关组件，不硬编码开发机路径；
- 不硬编码用户名、盘符、deviceId、session name、安装版本或本机目录；
- 优先能力探测/兼容性检测，而不是只认一个精确版本号；
- 不要求用户安装 Python、Rust、Node、WSL 等开发运行时；
- 安装和日常运行尽量使用普通用户权限，不无故要求管理员；
- 配置只保存在当前用户合理的应用数据目录；
- 升级能够保留有效 binding，卸载只清理 uu-cli 自己的内容；
- 官方 UU 不在预期位置、版本行为变化或缺少能力时，给出明确诊断，而不是静默失效；
- 对不同机器上的路径、设备名、Windows 用户名、显示语言等差异保持稳健。

发布前至少应在“干净机器/干净用户配置”场景验证安装和首次使用，而不能依赖开发机遗留状态。

## 8. 后台生命周期与资源占用

用户希望：官方 UU 在用时，uu-cli 随时可用；没有任务或没有可用 Terminal 时，资源占用尽可能低。

实现方式自由，但最终必须满足：

- 用户不需要手工启动/停止第二个服务；
- 自动发现新打开的官方 Terminal；
- 空闲时 CPU 接近 0，不做高频轮询或持续 spawn 官方 CLI；
- 没有必要时不维持昂贵连接；
- 无多余 GUI、tray、常驻可见 console；
- 只有真正需要用户选择时才弹短 PowerShell 交互；
- 后台组件缺失/崩溃后，下一次 `uu` 能自动恢复或给出可理解错误；
- uu-cli 的后台生命周期不能拖累或阻止官方 UU 正常启动、退出、升级。

是否采用 broker、watcher、事件监听、lazy process 或其他结构，由实现者自行决定。

## 9. 文件传输

公共接口固定：

```powershell
uu put <local> <remote>
uu get <remote> <local>
```

必须：

- 任意二进制安全；
- 成功/失败语义明确；
- 不完整传输不能报告成功；
- 尽可能复用官方稳定能力；
- 内部传输机制不暴露给 Agent。

如果原生路径不可复用，实现者可自行选择可靠 fallback，但不要为了实现方便改变公共接口或破坏文件完整性。

## 10. Agent Skill

Skill 要极短，只教三件事：

```text
远端执行：uu <command>
上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

再加一条硬规则：

> 不要依赖不同 `uu` 调用之间的 shell-local 状态；需要连续 `cd/source/export` 时放进同一条命令。普通 job 从 workspace 的 remote root 开始。

Agent 不管理 deviceId、session、worker 或 uu-cli 后台生命周期。

## 11. 完成标准

实现者自行决定开发顺序和代码结构，但 v1 完成前必须在真实 Windows + 官方 UU 环境证明：

- 全新 workspace 可以完成首次 binding；
- 多设备同时开 Terminal 时不会误绑；
- 新开 Terminal 能自动发现，必要时交互简单；
- 多个 Terminal 能安全并发调度；
- 长任务实时输出且不阻塞其他 worker；
- 不依赖 Terminal 的历史 `cd/source/export` 状态；
- 已发送命令不会因内部恢复逻辑重复执行；
- `put/get` 二进制 round-trip 一致；
- UU 重启、Terminal 消失、网络异常、后台组件重启时行为可理解且不串设备；
- 空闲占用低；
- 安装、升级、卸载不影响官方 UU；
- 在至少一台非开发机/干净环境上完成从安装到使用的验证。

只要满足这些目标和 `DESIGN.md` 的公共契约，实现者有充分自由选择技术方案。