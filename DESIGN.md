# Design contract

这份文档只定义 **产品行为、公共接口与边界**。Windows UU 内部具体调用哪个进程、IPC、协议或组件，由实机分析决定；研究结论可以约束实现，但不能反过来污染公共体验。

## 1. 第一性原理

用户真正需要的不是“管理远程工具”，而是：

> 把 Windows 本机已经登录的 UU Remote 变成一个透明的远端 shell + 文件通道。

因此正常使用时只需要：

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

`put` / `get` 是保留词。极少数确实需要执行名为 `put` 或 `get` 的远端命令时，可使用原始字符串形式，例如：

```powershell
uu "put arg1 arg2"
```

### 2.2 自动建立远端终端

`uu <command...>` 必须自己完成远端终端通道的建立或复用。

使用者和 Agent **不能被要求**：

- 先打开 UU GUI 的“终端”窗口；
- 手工新建 session；
- 手工 attach / detach；
- 记住 session id / name；
- 在命令前做额外的连接准备。

理想行为：

```text
uu <command>
   ↓
选择当前目标设备
   ↓
已有合适终端 → 复用
没有合适终端 → 静默建立
   ↓
原样交给远端 shell
   ↓
返回 stdin/stdout/stderr/退出状态
```

顺序命令应优先复用已有合适会话；真实并行任务可以使用多个会话。具体创建、选择、复用、并发和回收算法属于实现细节。

### 2.3 文件

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

临时指定其他设备属于以后可选的高级能力，不进入 v1 的核心使用路径，除非真实需求证明必须存在。

## 4. Windows UU 是事实来源

`uu-cli` 复用本机官方 UU Remote 的现有状态：

- UU Remote 已安装；
- UU Remote 已运行；
- 用户已经正常登录；
- 目标设备在该账号下可访问。

`uu-cli` 不拥有账号体系，不保存或导出 UU 凭据，也不绕过认证。

实现层可以调用官方 CLI、本地 IPC、已有本地组件或其他经过验证的本机接口，但公共 CLI 不依赖这些细节。

已有终端互操作研究见 `docs/terminal.md` 与 `proto/terminal.proto`。这些材料说明能力与协议事实，不等于提前锁定最终实现路线。

## 5. 生命周期与可见性

用户不应该管理第二个远程服务。

目标行为：

```text
官方 UU Remote 启动
        ↓
uu-cli 所需后台能力静默可用
        ↓
任意终端 / Codex 可直接调用 uu
        ↓
官方 UU Remote 完全退出
        ↓
uu-cli 后台能力退出
```

约束：

- 无 `uu start` / `uu stop`。
- 后台组件不得独立于官方 UU 长期常驻。
- 主控 Windows 上不得创建可见 console、GUI、托盘或任务栏入口。
- 调用 `uu ...` 本身也不得为了建立连接而弹出 UU Terminal 或其他辅助窗口。
- 如果最终实现根本不需要常驻后台组件，也可以；只要外部行为符合上述目标。
- 如何监听 UU 启停、是否需要 helper、如何处理异常退出，都由实机证据决定。

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
- 如果实现阶段确实需要修改官方 UU 安装内容，该修改必须最小、版本受控、可恢复，并在卸载时完整还原；优先设计成完全不修改官方文件。

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

因此 CLI 的稳定性、透明性和错误语义比功能数量更重要。

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
- 要求用户手工打开 UU Terminal 的工作流。

发现真实需求后再扩展，且优先保持现有核心语法不变。

## 9. v1 验收标准

在一台已登录 UU Remote 的 Windows 主控机上，安装一次 Release 后：

```powershell
uu devices
uu use <target>
uu pwd
uu "cd ~/project && python3 main.py"
uu put .\test.bin ~/test.bin
uu get ~/test.bin .\roundtrip.bin
```

应满足：

1. `uu` 能自己建立或复用远端终端，不要求用户先开 UU Terminal；
2. 主控端不弹额外窗口；
3. 命令确实在当前默认设备执行；
4. 目标离线时不会误切到另一台设备；
5. 命令输出可直接被人或 Codex 使用；
6. 长任务和常见交互不会被 CLI 自己破坏；
7. round-trip 文件字节一致；
8. UU Remote 完全退出后，`uu-cli` 不留下独立后台进程；
9. 卸载后 PATH、后台集成和项目自身文件被清理，官方 UU 保持原样。

达到这些条件之前，不扩展功能面。
