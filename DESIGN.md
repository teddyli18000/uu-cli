# Design contract

这份文档只定义 **产品行为、公共接口与边界**。Windows UU 内部具体调用哪个进程、IPC、协议或组件，由实机分析决定；研究结论可以约束实现，但不能反过来污染公共体验。

## 1. 第一性原理

用户真正需要的不是“管理远程工具”，而是：

> 把 Windows 本机已经登录的 UU Remote 变成一个尽量透明的远端 shell + 文件通道。

正常使用时只需要：

```powershell
uu <command...>
uu put <local> <remote>
uu get <remote> <local>
```

如果使用者已经会 shell，就不应该再学习新的命令体系。

## 2. 公共 CLI 契约

### 2.1 远端 shell

```powershell
uu pwd
uu git status
uu python3 main.py --epochs 20
uu "cd ~/project && cmake --build build && ./build/app"
```

规则：

- 除保留的 `put` / `get` 及少量配置命令外，`uu` 后面的内容就是远端命令。
- CLI 不理解、不分类、不重写 `git`、`python`、`cmake` 等业务命令；它只负责可靠地交给远端 shell。
- 普通参数形式尽量保持 argv 语义；需要 `&&`、管道、重定向、变量展开等 shell 语法时，可把整段命令作为一个字符串交给 `uu`。
- stdout 尽量只包含远端程序输出；CLI 自身诊断写 stderr。
- 远端退出码可获得时，`uu` 返回该退出码；连接/协议失败返回非零本地错误。
- 长任务不能因为 CLI 自己的短超时被随意截断。
- stdin、交互输入、Ctrl-C 等行为以“尽量像直接坐在远端 shell 前”为目标。
- 官方 terminal 是 PTY 字节流，stdout/stderr 可能合流，CLI 不伪造不存在的分离语义。

`put` / `get` 是保留词。极少数确实需要执行名为 `put` 或 `get` 的远端命令时，可使用原始字符串形式，例如：

```powershell
uu "put arg1 arg2"
```

### 2.2 v1 的 Terminal bootstrap

对当前分析的官方 Windows UU 4.43.x，研究已确认：

- 外部工具可以查询、附着和复用已有 terminal session；
- 官方 GUI 创建 terminal 时，会先在 GameViewer 主程序进程内完成本地 host registration/token 注册；
- 该 launcher/registration 没有可复用的 CLI、IPC、deeplink 或其他稳定外部入口；
- `detached_process` 也只是 GUI launcher 的最后 fallback，不是对外 headless API。

因此 v1 **不再要求 `uu-cli` 自己创建第一个官方 terminal session**。正常流程是：

```text
Windows UU 已运行并登录
        ↓
用户在 UU GUI 中为目标设备打开一次官方 Terminal
        ↓
`uu` 发现已有可用 session
        ↓
附着/复用 session
        ↓
执行远端命令并返回结果
```

这个人工步骤属于当前官方客户端的 bootstrap 前置条件，不是新的日常命令体系。

要求：

- 用户只负责通过官方 UU 创建可用 terminal；之后不要求理解 session id/name、attach/detach 等概念。
- Agent 不应自己操作 UU GUI。
- `uu-cli` 不通过注入、伪造官方签名、绕过 host registration 等方式强行创建 session。
- 如果当前没有可用 session，`uu` 必须明确报错并提示“在 UU 中为当前目标打开一次 Terminal”，不能静默失败，也不能偷偷弹 GUI。
- 已有可用 session 时，顺序命令应优先复用；真实并行任务是否使用额外 session 由实现和安全性决定。
- 多个候选 session 无法安全区分时，宁可报错，也不能猜错并执行到不确定 shell。
- UU 重启、session 被关闭/kill 或失效后，可能需要用户再次打开官方 Terminal。

如果未来官方 UU 暴露稳定的 headless host/register 入口，可以去掉人工 bootstrap，而 **不改变任何公共 CLI 语法**。

### 2.3 命令边界

官方 terminal 协议没有逐命令 `exec` 或逐命令 exit-code 消息；它提供的是持续 PTY 字节流和会话级退出状态。

因此持久 shell 中，`uu-cli` 可以在内部为每次调用注入不可见的命令完成 marker，并携带本次调用的随机 nonce 与 `$?`，用于识别：

- 本次命令何时结束；
- 本次命令的 exit code；
- 哪段 marker 属于本次调用。

marker 必须在返回给用户/Agent 前剥离，且不能要求用户修改 shell 配置。不要靠 prompt 文本猜命令完成状态。

### 2.4 文件

```powershell
uu put .\main.py ~/project/main.py
uu get ~/project/result.csv .\result.csv
```

要求：

- 二进制安全，不擅自改编码、换行或内容。
- 成功后两端文件内容必须一致；实现可在内部做长度/hash 校验。
- 传输不完整必须明确失败，不能返回成功。
- 传输协议、分块、重试、临时文件等细节不暴露给用户。
- v1 先把“单文件可靠传输”做好；目录支持以后仍复用同一 `put/get` 语法即可。
- 文件传输是独立的一等能力，不要求用户理解它内部是否与 terminal 共用通道。

## 3. 目标设备

日常命令不能被设备选择拖累。正常路径始终是：

```powershell
uu <command...>
uu put ...
uu get ...
```

CLI 维护一个 **当前默认目标设备**。

首次安装/配置时：

- 只有一个可用设备：可直接选择；
- 有多个可用设备：让用户明确选择一次；
- 选择结果持久保存，但不保存 UU 账号凭据。

设备管理只保留最小入口：

```powershell
uu devices
uu use <device>
```

要求：

- `uu devices` 用于查看可用设备及当前默认目标；
- `uu use <device>` 修改默认目标；
- 日常命令不要求重复设备名或 device id；
- 默认目标离线或不可用时明确失败，**绝不自动切换到另一台在线设备**；
- 设备重命名、ID 变化等情况由实现尽量稳健处理，但不能悄悄执行到错误设备。

设备的稳定身份字段仍由后续实机研究决定，产品层不提前假定某个 ID 永久稳定。

## 4. Windows UU 是事实来源

`uu-cli` 复用本机官方 UU Remote 的现有状态：

- UU Remote 已安装；
- UU Remote 已运行；
- 用户已经正常登录；
- 目标设备在该账号下可访问；
- v1 执行 shell 命令时，目标设备已有用户通过官方 GUI 建立的可用 terminal session。

`uu-cli` 不拥有账号体系，不保存或导出 UU 凭据，也不绕过认证。

实现层可以调用官方 CLI、本地 IPC、已有本地组件或其他经过验证的本机接口，但公共 CLI 不依赖这些细节。

终端研究资料位于 `docs/` 与 `proto/terminal.proto`。研究文档记录 confirmed / observed / inferred / not tested 事实，不等于要求最终实现照搬某条内部路线。

## 5. 生命周期与可见性

v1 不要求用户管理 `uu-cli` 自己的第二个服务。

目标行为：

```text
官方 UU Remote 启动并登录
        ↓
用户打开一次目标设备官方 Terminal
        ↓
`uu-cli` 可发现/复用该 session
        ↓
任意终端 / Codex 可调用 uu
        ↓
UU 或 session 失效时 uu 明确失败
```

约束：

- 无 `uu start` / `uu stop`。
- 如果需要后台组件，它不得独立于官方 UU 长期常驻。
- `uu-cli` 自己不得创建可见 console、GUI、托盘或任务栏入口。
- 调用 `uu ...` 不得为了建立连接而自动弹出 UU Terminal 或其他辅助窗口。
- 没有可用官方 terminal session 时，给出明确、短的人工操作提示。
- 如果最终实现不需要常驻后台组件更好。

## 6. 安装、升级、卸载

Release 的理想交付物是 **一个 Windows 安装程序 EXE**：

```text
uu-cli-setup.exe
```

安装后用户只感知 `uu` 命令。

安装程序负责：

- 安装 CLI 和必要运行组件；
- 把 `uu` 放进当前用户 PATH；
- 配置后台生命周期集成（若实现需要）；
- 在需要时完成一次默认设备选择；
- 注册 Windows 标准卸载入口。

原则：

- 默认 user-scope 安装，尽量不要求管理员权限。
- 升级可直接覆盖旧版本，并保留合理的用户配置。
- 卸载只删除 `uu-cli` 自己的程序、配置、PATH 和生命周期集成。
- 不删除、不重装、不退出官方 UU Remote，不清除其账号状态。
- 不修改官方 UU 文件；除非未来有非常明确、可恢复且必要的兼容性理由，否则不接受这种路线。

## 7. 对 Agent 的要求

Codex 不应该知道内部协议，也不应该被迫学习“远程任务 API”。

它只需要把：

```text
python3 main.py
```

变成：

```text
uu python3 main.py
```

需要文件时使用：

```text
uu put ...
uu get ...
```

设备选择通常由用户预先配置，Agent 不应在每条命令中重复指定设备。

如果 `uu` 报告当前没有可复用 terminal session，Agent 应停止并把这个明确前置条件交给用户；不自行点击 GUI、不尝试绕过 host registration。

## 8. 明确不做

v1 不因为“可能以后有用”而加入：

- 进程管理 API；
- Git/Python/CMake 专用子命令；
- Web UI；
- MCP server；
- 托盘管理器；
- 远程桌面自动化 API；
- 账号/Token 管理；
- 自己的一套脚本语言；
- 用户可见的 session 管理体系；
- 自动化点击 UU GUI 来创建 Terminal；
- 注入 GameViewer、伪造官方签名客户端、绕过 terminal host registration。

发现真实需求后再扩展，且优先保持现有核心语法不变。

## 9. v1 验收标准

前置条件：Windows 主控机已安装、运行并登录官方 UU；用户已为默认目标设备打开一个可复用的官方 Terminal session。

然后：

```powershell
uu devices
uu use <target>
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

应满足：

1. `uu` 能发现并复用已有官方 terminal，不要求用户管理 session；
2. 没有 session 时给出明确提示，不自动弹 GUI；
3. 主控端 `uu-cli` 自己不弹额外窗口；
4. 命令确实在当前默认设备执行；
5. 目标离线时不会误切到另一台设备；
6. 命令输出可直接被人或 Codex 使用；
7. 命令边界和 exit code 能稳定识别，长任务/常见交互不会被 CLI 自己破坏；
8. round-trip 文件字节一致；
9. UU Remote 完全退出后，`uu-cli` 不留下独立后台进程；
10. 卸载后 PATH、后台集成和项目自身文件被清理，官方 UU 保持原样。

达到这些条件之前，不扩展功能面。
