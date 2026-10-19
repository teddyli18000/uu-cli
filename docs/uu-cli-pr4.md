# uu-cli PR4：无 GUI 终端与命令边界

本文回答两个剩余核心问题：**只开已登录 UU、不弹窗、不点 Terminal，能否执行 `uu pwd`**，以及**持久 shell 里如何可靠划分一条命令并拿到退出码**。

结论标注：**Confirmed** = 代码/字面量可直接对应；**Observed** = 停止宿主机动态测试前已获得的有限观察；**Inferred** = 与已确认部分一致但缺一个直接测量；**Not tested** = 未测。

> 宿主安全：按操作者要求，后续只在容器/副本上做静态分析，没有再次执行官方程序或操作实时会话。本文的“最终证明”部分给出可执行的最小验证步骤，但**尚未运行**。

---

## 1. 无 GUI new/attach 到底怎么完成

### 1.1 绑定链（Confirmed）

GUI 的 `device_terminal_launcher.cpp` 在启动 CLI **之前**完成本地宿主注册，再回填 PID，最后启动：

```text
terminal_launch stage=start
terminal_launch result=failed reason=host_registration_failed   # 先注册
terminal_launch result=success launcher=windows_terminal pid=   # 再启动
terminal_launch result=failed reason=pending_host_process_update_failed  # 回填 PID
terminal_launch result=fallback ... fallback=new_console
terminal_launch result=success launcher=new_console
terminal_launch result=success launcher=detached_process pid=
```

实际命令行：

```text
wt.exe -w <window> new-tab --title <title>
  uuyc-cli.exe term -d <device> new|attach
    --local-host-token <token>
    --local-host-kind <kind>
    --local-session-name <name>
    --local-window-name <window>
    --local-session-id <id>
```

- **Confirmed**：token 由 GUI 生成/注册，CLI 只通过 argv 转发。
- **Confirmed**：CLI 只声明 5 个 `--local-host-*` 参数；**没有** `--local-process-id` 和 `--local-window-handle`，这两个字段由 CLI 自己填充。
- **Confirmed**：主程序（GameViewer.exe）的终端连接解析器按顺序校验：`Invalid terminal host token` → kind → per-kind 字段 → `Invalid terminal host process id` → `Invalid terminal host window handle` → `Invalid local terminal session id`；另有 `Terminal session is already open`。
- **Confirmed**：受控端 `GameViewerServer.exe` 中 **没有** `local_host` / `terminal_host` 字面量；绑定只在主程序/CLI 侧产生和校验，受控端不解析它。

### 1.2 Host kind（Confirmed）

只存在两种终端 host kind：

| kind | 说明 |
|---|---|
| `windows_terminal` | 需要 Windows Terminal 窗口/标签；registry 通过 UI Automation 操作 `CASCADIA_HOSTING_WINDOW_CLASS` 的标签 |
| `console` | 控制台宿主；GUI 自己会在 Windows Terminal 不可用时回退到 `new_console` |

另有 `detached_process` 启动方式（`terminal_launch result=success launcher=detached_process pid=`），说明**无窗口启动在启动器层面是被支持的**。没有 `headless` 这个 kind。

- **Confirmed**：`console` 条目可以在没有 Windows Terminal 标签的情况下注册为 `pending` 并被回收（`terminal_host_stale ... kind=console state=pending`）。
- **Inferred**：`console` 是 headless/ConPTY 的目标 kind；只要提供 `local_process_id`（以及有窗口时的 `local_window_handle`）和有效 token，就可能完成绑定。
- **Not tested**：`console` 的完整必填字段集、伪终端能否提供被接受的 window handle、远端是否拒绝伪造的 PID/window handle。

### 1.3 local-host-token 是谁生成的、怎么验证

- **Confirmed**：token 由 GUI 生成/注册；CLI 二进制中没有 UUID/GUID/RtlGenRandom 之类的生成原语，只有转发。
- **Confirmed**：token 以 argv 传递，`terminal_host_*` 日志不记录它。
- **Unverified**：token 的具体格式（GUID？HMAC？注册表键？）、熵、生命周期；GUI 里有通用 GUID 格式化代码，但不能证明 token 就是 GUID。
- **Confirmed（架构）**：受控端不校验 `local_host_token`；校验发生在主程序侧。token 还会出现在命令行（Task Manager / Sysmon / EDR 可见），因此它更像**本地宿主关联 id**，而不是需要保密的凭据；但主程序确实会用 `Invalid terminal host token` 拒绝不匹配的值。
- **Not tested**：token 是否一次性、是否绑定 PID/window handle、是否可重放。

### 1.4 能不能直接连 UU 的 SERVER pipe，绕过 GUI host

- **Confirmed**：SERVER pipe 的命令面可恢复：`cmd:terminal:`、`cmd:device_*`、`cmd:cloudpc_*`、`cmd:assist_*`、`cmd:input_password:`、`cmd:echo:` 等；响应语法 `resp:OK` / `resp:error:<code>` / `resp:waiting_confirmation` / `resp:need_password` / `progress:<stage>`。
- **Confirmed**：没有 `cmd:terminal_host_register` 这个 verb；宿主 registry 是 GUI **进程内**填充的，不是通过 pipe 暴露的。
- **Inferred**：`cli_foreground_handoff ... stage=peer_identity` 更像“代表 CLI 申请前台激活”的授权，而不是连接时的 peer 拒绝；没有找到 `GetNamedPipeClientProcessId` 一类字符串，校验更可能落在 token 上。
- **Not tested**：SERVER pipe 是否接受非官方客户端连接；`cmd:terminal:` 是否能用 `kind=console` + 自造 token 完成绑定。

### 1.5 最终证明：`uu pwd`，只开已登录 UU，不弹窗、不点 Terminal

**当前结论：尚未证明。** 已恢复的链路如下：

| # | 步骤 | 状态 |
|---|---|---|
| 0 | 主程序运行且已登录；SERVER pipe 在监听 | **Observed**（pipe 存在，`term ls` 可无 GUI 工作） |
| 1 | 非官方客户端连接 SERVER pipe，`cmd:status` 得到 `resp:OK` | **Not tested** |
| 2 | `cmd:terminal:` 带 `local_host_kind=console` + 自造 token | **Not tested**；预期会得到 `Invalid terminal host token` |
| 3 | 取得有效 `local_host_token` | **未证明的关键一步** |
| 4 | 完成 host 绑定后 attach，走 TERM 帧执行 `pwd` | **Not tested**（协议本身已恢复） |

**为什么第 3 步是卡点（Confirmed）**：token 由 GUI 的 host registration 生成，而 SERVER pipe 没有 `terminal_host_register` verb；native client 无法通过公开 IPC 自行签发 token。

**两个可判定的验证（均需宿主动态执行，当前已停止）**：

1. **宽松校验**：用随机 GUID 作为 `local_host_token`，`kind=console` 发 `cmd:terminal:`。返回 `Invalid terminal host token` → 不能自造；返回 `state=bound`/会话信息 → 可以自造。
2. **重放 GUI token**：从 `device_terminal_launcher` 的命令行抓一个 GUI 生成的 token（只读进程命令行），用自己的 `local_process_id` 重放。成功说明 token 不是一次性、也不绑定原 PID；失败说明 token 绑定宿主进程/窗口。

**可行但未证明的路径**：
- 用 `console`/`detached_process` 启动一个隐藏控制台宿主，复用 GUI 生成的 token；这仍需要 GUI 至少启动过一次，不是“完全无点击”。
- 直接实现 native SERVER pipe 客户端；协议可讲，但 token 签发/校验是唯一未闭合的链路。
- 让官方增加 headless host kind 或 `terminal_host_register` IPC；这是 vendor 改动，不属于客户端设计。

**结论**：在现有证据下，**不能声称**“只开已登录 UU + `uu pwd` + 完全不弹窗”已经成立。它是可验证的，但需要上面两个测试之一；在宿主安全限制解除前，PR4 只能把链路和卡点钉死，不能把未测写成已成立。

---

## 2. 持久 shell 下如何可靠划分一条 uu 命令

### 2.1 官方没有命令边界（Confirmed）

- 帧类型 1..16 中没有 `exec`/`command`/`run`；`stdin`(2) 与 `stdout`(5) 是原始字节流。
- `exit`(6) 是**会话级**退出，载荷只有 `{"exit_code":N}`，在 shell 进程结束时发一次；CLI 自己提示 `[Tips] Type 'exit' to end the remote session.`
- `signal`(7) 是取消/中断，不携带命令结果。
- CLI 只有 `term new/attach/ls/kill/rename`；`term exec`、`term send`、`term run` 出现次数为 0。
- `uuyc-mux.exe` 确实实现了 `send-keys`、`capture-pane`、`wait-for`、`if-shell`、`run-shell`，但它是**受控端本地 mux**，由服务端通过内部 `Terminal.MuxControl` 命名管道驱动；客户端只有 PTY 字节流，没有转发 mux 命令的协议 op。

**结论**：官方协议只给“PTY 字节流 + 会话退出码”；**没有**客户端可达的逐命令边界或逐命令 exit code。

### 2.2 推荐：客户端注入 sentinel，不改用户语法

在持久 shell 中，`uu` 内部把用户命令包成一行（用户仍然只输入 `uu python3 main.py`）：

```bash
{ python3 main.py; }; printf '\033]7770;%d\007' "$?"
```

- `{ …; }` 在当前 shell 执行，`$?` 是用户命令的退出码，在 `printf` 时立即捕获。
- `printf` 写到 tty，marker 内联出现在 `stdout`(5) 流里，不需要额外通道。
- `OSC 7770` 是私有转义序列；未知 OSC 会被终端忽略；客户端在渲染前剥离 `ESC ] 7770 ; <n> BEL`，同时去掉回显行里的副本。
- 用户语法不变；CLI 只负责 framing/解析。
- 如果没有收到 marker：报 **unknown**，让用户用 `signal`(7) 取消；**不要**从 prompt 推断 exit code。
- `exit`/`exec` 使 shell 退出：只收到 `exit`(6) 的会话级 `exit_code`；`signal`(7) 不能当命令结果。
- 需要处理的边界：命令吞掉后续 stdin、后台任务、多行命令、语法错误、命令挂起、escape 过滤。

### 2.3 可选：OSC 133 shell integration

在 shell 的 `PROMPT_COMMAND`/`precmd` 里发 `ESC ] 133 ; D ; <status> BEL`，每个 prompt 都带上一命令状态。优点：不逐条包装，手输命令也有状态；缺点：**会修改用户 shell 环境**，必须显式、可逆、告知用户。建议作为 `--shell-integration` opt-in，默认关闭。

### 2.4 不推荐

- **Prompt 解析**：prompt 可配置（PS1、starship、oh-my-posh、多行），误判成本高。
- **Mux 边界**：`send-keys`/`wait-for`/`capture-pane` 在受控端内部，客户端协议不暴露；使用它需要 vendor 协议改动。

---

## 3. 对 uu-cli 的实现建议

1. **管理面**：继续用官方 CLI 的 `term ls/has/kill/rename`；它们不需要 GUI 终端。
2. **创建/附着**：当前不能证明完全无 GUI。优先验证 §1.5 的两个 token 测试；在验证前，不要把“无窗口 `uu pwd`”当作已实现能力。
3. **命令执行**：持久 shell + 客户端 sentinel（§2.2）；默认不修改用户 shell，`--shell-integration` 作为 opt-in。
4. **退出码**：单条命令用 sentinel 的 `$?`；会话结束用 `exit`(6) 的 `exit_code`；`signal`(7) 只表示取消。
5. **安全**：`local_host_token` 会出现在命令行，按“本地关联 id”处理，不要把它当长期凭据；不要把真实 token 写入仓库/日志。

---

## 4. 置信度汇总

| 结论 | 状态 |
|---|---|
| token 由 GUI 生成/注册，CLI 仅 argv 转发 | Confirmed |
| 主程序按 token→kind→pid→window→session 顺序校验 | Confirmed |
| 受控端不解析 local_host_* | Confirmed |
| host kind 只有 windows_terminal / console（+ detached_process 启动方式） | Confirmed |
| 无 headless kind；console 是 headless 目标 | Confirmed / Inferred |
| SERVER pipe 协议可讲；无 terminal_host_register verb | Confirmed |
| token 格式/一次性/PID 绑定 | Unverified |
| 非官方客户端能否连 SERVER pipe | Not tested |
| 自造 token 能否绑定 | Not tested |
| 重放 GUI token 能否绑定 | Not tested |
| `uu pwd` 无窗口、无点击已成立 | **Not tested / 未证明** |
| 官方有逐命令边界/exit code | **No — Confirmed** |
| sentinel/OSC 133 方案 | Inferred（推荐） |
| escape 序列是否被服务端过滤 | Not tested |
