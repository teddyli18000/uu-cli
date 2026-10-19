# uu-cli 实现阻塞项：实测与协议结论

本文补充终端通道调查之外的四项实现阻塞。结论分为：

- **Confirmed**：代码/消息定义可直接对应。
- **Observed**：在本机回环环境实际运行观察到的行为（有限次，已停止）。
- **Inferred**：与已确认部分一致，但缺一个直接测量。
- **Not tested**：未测；宿主安全要求已停止动态执行。

> 宿主安全说明：操作者要求后续分析尽量在容器/副本上进行。本文记录的是停止动态测试前已经获得的有限观察；没有为了补结论再次执行官方程序或操作实时会话。测试创建的 `probe*` 会话已清理，未触碰用户 GUI 的 `bash-2` / `bash-3`。

---

## 1. 无 GUI 时能否由本地程序建立 terminal

### 1.1 结论

**能建立“管理/查询”连接，但不能在没有交互终端或 GUI 本地宿主的情况下直接 `term new`。**

- **Observed**：主程序未运行时，`uuyc-cli` 的 `version` / `status` / `echo` / `device list` 全部返回 `Main application is not running`。
- **Observed**：主程序运行后，安装目录下的 `uuyc-cli term -d <local-device> ls --json` 在**没有 GUI 终端会话**时成功，返回 CLI 本地包装：
  ```json
  {"success":true,"data":{"items":[],"total":0},"error":null,"timestamp":"..."}
  ```
  这说明查询/管理路径不依赖 GUI 终端窗口。
- **Observed**：`term new/attach` 在非交互 stdin/stdout 下直接报 `Error: term new/attach requires an interactive terminal`。
- **Observed**：用伪终端（ConPTY）驱动 `term new` 时，CLI 进程退出码出现过 7/259，但没有捕获到终端输出；无法据此证明会话建立成功。
- **Observed（关键）**：用户 GUI 的“终端远控”窗口实际启动了一个 CLI 进程，命令行形如：
  ```text
  uuyc-cli.exe term -d <local-device> new
    --local-host-token <redacted>
    --local-host-kind windows_terminal
    --local-session-name 终端远控
    --local-window-name uuyc-terminal-<hash>
  ```
  这说明 GUI 额外做的是**本地终端宿主绑定**：生成本地 host token、声明 host kind 为 `windows_terminal`、绑定窗口/会话名，然后在 Windows Terminal/控制台里启动 CLI。

### 1.2 本地调用/IPC 路径

- **Confirmed（字符串/静态）**：CLI 与主程序使用全局命名管道：
  - `\\.\pipe\Global\GAMEVIEWER-IPC-SERVER-8C83A13E-D2C4-450C-B69E-6427B73FBD7B`
  - `\\.\pipe\Global\GAMEVIEWER-IPC-SERVER-INIT-KEY-E59D03D6-1522-430B-8EEA-35904D724BC7`
- **Confirmed（字符串/静态）**：CLI 的本地命令语法是 `cmd:...` / `resp:...`，日志标签 `cli_ipc`、`cli_connect_ipc`；命令如 `cmd:echo:%1`、`cmd:device_connect:%1`、`cmd:connect:%1:%2`。
- **Observed**：复制到其它目录的 `uuyc-cli.exe` 会报 `Main application is not running`；安装目录下的同一版本可连接。说明 CLI 按自身安装根目录/注册表发现主程序，不能简单复制单文件使用。
- **Inferred**：`INIT-KEY` 管道用于主程序/启动器初始化本地 IPC；本次观察时该管道未出现，但安装版 CLI 仍能工作，说明它可能是首次启动/特定启动方式的初始化，而不是每次查询的硬前提。

### 1.3 对 uu-cli 的含义

- 只做 `ls` / `has` / `kill` 等管理操作：可以走官方 CLI，不需要 GUI 终端窗口。
- 要做 `term new/attach`：需要**交互终端**，并且要提供 GUI 同款的本地宿主参数（`--local-host-token`、`--local-host-kind windows_terminal`、`--local-window-name`、`--local-session-name`）。没有这些参数时，CLI 不能完成本地宿主绑定。
- **Not tested**：本地程序自行生成合法 `local-host-token` 并由无 GUI 进程完成绑定；宿主安全要求下未继续尝试。

---

## 2. Session 生命周期、并发与稳定标识

### 2.1 CLI 暴露的标识

**Observed**：`term ls --json` 的本地包装 item 字段为：

```json
{
  "name": "bash-3",
  "deviceId": "<redacted>",
  "deviceName": "WSL临时",
  "shell": "/bin/bash",
  "state": "running",
  "createdAt": "2026-10-19T17:27:45Z"
}
```

- **Observed**：CLI 本地列表**不暴露 `session_id`**；`term attach/has/kill/rename` 都以 **session name** 作为参数。
- **Confirmed（远端协议）**：`list_sessions_resp`（type 10）的裸数组 item 含 `session_id`、`name`、`state`、`shell`、`last_active_ms`、`created_at_ms`；`kill_session_req` 用 `session_id`。
- **Inferred**：对 `uu-cli` 的用户面标识应使用 **name**（稳定、可读、CLI 已按 name 操作）；`session_id` 只作为远端协议内部路由/终止字段，不暴露给 agent 用户。

### 2.2 已观察到的生命周期

- **Observed**：一次被中止的 `term new` 之后，`term ls` / `term has` 返回：
  ```json
  {"success":false,"error":{"code":105,"message":"Timeout waiting for terminal environment check"}}
  ```
  约 10 秒后同一命令恢复正常。说明中止 attach 会留下一个短暂的 stale 连接/检查窗口。
- **Observed**：列表中出现过 `bash-2`（`state=detached`）与 `bash-3`（`state=running`）并存；这是 GUI/WSL 临时设备的会话，说明**多会话可以并存**，至少 2 个。
- **Observed**：测试创建的 `probe*` 会话在 CLI 进程退出后不再出现在列表中（`No changes: session not found`）；没有留下长期运行的 probe 会话。
- **Not tested**：受控端主动 detach 后会话是否保持 running；controller 进程退出后会话保留多久；网络断线/重连；多会话上限。
- **Confirmed（静态）**：服务端有 `terminal detach`、`terminal close`、`terminal session termination`、`replaced_connections`、`session_limit` 等路径；close/detach 由客户端发起、服务端按 connection handle 校验。
- **Inferred**：code 105 的根因是 `check_env` RPC 在已有连接上等待 20 秒超时；中止 attach 后旧连接未及时清理，导致后续命令复用/等待同一连接。这与“车端没有看到新的 check_env”一致：请求不是新连接，而是既有连接上的 RPC。

### 2.3 并发边界

- **Observed**：GUI 同时存在 detached/running 两个会话。
- **Confirmed（静态）**：存在 `session_limit`、`session_limit_reached`、`replaced_connections`（连接被重新绑定到不同 session id 的计数）。
- **Not tested**：具体上限数值、达到上限时的错误码/提示、并发创建时的竞争行为。

---

## 3. Terminal 数据语义

### 3.1 stdout / stderr

- **Confirmed（结构）**：帧类型枚举 1..16 中没有 `stderr`；终端输出只有 type 5 `stdout`。
- **Inferred（行为）**：stdout/stderr 在受控端 ConPTY/PTY 中合流，作为单一有序、无标签的字节流发送；接收端无法区分来源。
- **Observed（有限）**：在 WSL 临时设备的 bash 中执行 `pwd > file`、`printf 'OUT1\n' > file` 成功，文件内容为 `/root`、`OUT1`；说明命令确实在远端 shell 执行。
- **Not tested**：终端流中 stdout/stderr 的实际交错顺序与标签（CLI 的终端输出未被伪终端捕获；宿主安全要求下未继续）。

### 3.2 输入回显

- **Inferred**：CLI 将本地控制台切到 raw VT 模式（`Warning: Failed to switch terminal to raw VT mode`），本地不回显；回显由远端 PTY/readline 产生，再以 type 5 返回。
- **Not tested**：实际回显时序与 `stty -echo` 行为。

### 3.3 Type 6 exit 与退出码

- **Confirmed（静态）**：type 6 载荷键为 `exit_code`，即 `{"exit_code": <int>}`；服务端从真实子进程退出状态取得该值。
- **Observed**：在 WSL 临时设备的 bash 中发送 `exit 7`，CLI 进程退出码为 **7**（伪终端助手记录 `[CLI_EXIT]=7`）。这说明**远端 shell 的退出码会传播到 CLI 进程退出码**。
- **Inferred**：`uu-cli` 可以直接把 CLI 退出码作为远端命令退出码返回；不需要额外解析 type 6 载荷。
- **Not tested**：`exit 5` 的重复验证、非零错误码的边界、CLI 是否在正常 shell 退出时打印提示。

### 3.4 命令级退出码

- **Inferred**：普通命令的退出码没有单独的协议消息；它是 shell/PTY 子进程的退出状态，最终进入 type 6 `exit_code`。如果要在同一条终端里执行多条命令并逐条取码，需要 shell 侧显式输出（例如 `; echo $?`），不能依赖协议逐命令返回。

---

## 4. Type 7 signal 与 Ctrl-C

### 4.1 Payload

- **Confirmed（静态，修正）**：type 7 的 JSON 键是 **`sig`**，不是 `signal`；即 `{"sig":"SIGINT"}`。`sig` 与 `SIGINT` 两个字面量在 CLI 中相邻，错误路径为 `Error: failed to send signal frame`。
- **Confirmed（静态）**：CLI 存在 type 2 `stdin` 转发和 type 7 `signal` 两条路径。
- **Inferred**：Ctrl-C 按键走 **raw `0x03` 的 stdin 帧**，由远端 PTY 的 line discipline 转成 SIGINT；type 7 `{"sig":"SIGINT"}` 是显式中断/终止 API 的独立路径。CLI/GVS/mux/bridge 中没有 `GenerateConsoleCtrlEvent`/`CTRL_C_EVENT`/`0x03` 特判。

### 4.2 实测状态

- **Not tested（结论性）**：一次延迟发送 `0x03` 的 `sleep 60` 测试中，CLI 退出码为 259（仍在运行），没有生成预期的 AFTER 文件；无法判断 Ctrl-C 是否生效。宿主安全要求下未继续。
- **Observed（旁证）**：中止 attach 会导致 code 105 的 stale 窗口，说明会话/连接清理对信号和断开很敏感。

### 4.3 Linux 端映射建议

- **Confirmed（静态）**：Windows 侧没有任何 `tcgetpgrp`/`pgid`/`kill(-pgid)` 参考实现；Linux 侧必须自行选择。
- **Inferred（建议）**：
  - **按键 Ctrl-C**：把 `0x03` 写入 PTY master。由 slave 的 line discipline 负责 `VINTR → SIGINT`，并自动投递给**当前前台进程组**；这也是 vim/less 等自行进入 raw mode 的程序能正确收到字节的方式。
  - **显式中断 API**：`kill(-tcgetpgrp(fd), SIGINT)`；每次使用前重新读取 `tcgetpgrp`，不要缓存 pgid。
  - **不要** `kill(shell_pid, SIGINT)`：那会打断 shell 而不是前台作业，是典型的“Ctrl-C 没反应”根因。

---

## 5. 对 uu-cli 的实现建议

1. **管理面**：v1 直接用官方 `uuyc-cli` 的 `term ls/has/kill/rename`；这些命令不需要 GUI 终端，解析其本地包装 `{success,data:{items,total},error,timestamp}`。
2. **会话标识**：用户面用 `name`，内部保留远端 `session_id`；不要把 CLI 本地列表当成远端裸数组。
3. **创建/附着**：`term new/attach` 需要交互终端；要无 GUI 运行，必须解决本地宿主绑定（`--local-host-token`/`--local-host-kind`/窗口名）或直接实现远端 `open` 帧。当前证据不足以证明可以完全绕过 GUI 宿主。
4. **退出码**：优先使用 CLI 进程退出码；它已观察到与远端 `exit 7` 一致。
5. **Ctrl-C**：优先按 raw `0x03` 设计；显式中断再用 type 7 `{"sig":"SIGINT"}`。Linux 端按 §4.3 映射。
6. **数据流**：按单一合流、无标签的字节流处理；不要假设能区分 stdout/stderr。

---

## 6. 置信度汇总

| 结论 | 状态 |
|---|---|
| 主程序未运行时 CLI 全部管理命令失败 | Observed |
| 安装版 CLI 可无 GUI 终端执行 `term ls --json` | Observed |
| 复制版 CLI 因安装根目录发现失败 | Observed |
| GUI 通过 `--local-host-token/--local-host-kind windows_terminal/--local-window-name/--local-session-name` 启动 CLI | Observed |
| `term new/attach` 需要交互终端 | Observed |
| `term ls --json` 本地包装与 item 字段（无 session_id） | Observed |
| 中止 attach 后 code 105 stale 窗口约 10s | Observed |
| 远端 `exit 7` 传播为 CLI 退出码 7 | Observed |
| 远端 stdout/stderr 合流、无标签 | Confirmed（结构）/ Inferred（行为） |
| 输入回显来自远端 PTY | Inferred |
| type 6 载荷 `{"exit_code":N}` | Confirmed |
| type 7 载荷 `{"sig":"SIGINT"}`（键为 sig） | Confirmed |
| Ctrl-C 走 raw 0x03 stdin | Inferred |
| Linux PTY/前台进程组映射 | Inferred（无 Windows 参考实现） |
| 无 GUI 完全绕过本地宿主创建会话 | Not tested |
| 多会话上限/网络重连/完整 detach 生命周期 | Not tested |
