# uu-cli PR5：headless host registration

本文只回答一个问题：**官方 UU 已登录、没有打开 Terminal/远程桌面时，普通后台进程能否零窗口、零点击完成 `pwd`。**

结论标注：**Confirmed** = 代码/字面量可直接对应；**Observed** = 本次有限宿主验证实测；**Inferred** = 与已确认部分一致但缺一个直接测量；**Not tested** = 未测。

> 宿主安全：PR5 只做了操作者授权的有限验证（连接 SERVER pipe、随机 token、只读命令行取 token），没有破坏性操作、没有杀 GUI 会话、没有写 Program Files。测试没有留下 `probe_*` 会话。

---

## 1. 非官方本地进程能否连 SERVER pipe 并完成 status/echo

### 1.1 实测

- **Observed**：普通 PowerShell 进程（非官方 CLI）可以 `Connect` 到
  `\\.\pipe\Global\GAMEVIEWER-IPC-SERVER-8C83A13E-D2C4-450C-B69E-6427B73FBD7B`，连接成功。
- **Observed**：依次发送以下四种无副作用帧，均**没有收到任何响应**，连接在 800 ms 后仍保持：
  - 裸文本 `cmd:get_version\n`
  - 4 字节小端长度 + `cmd:get_version`
  - 4 字节小端长度 + JSON `{"cmd":"get_version"}`
  - 裸 JSON + 换行
- **Observed**：没有崩溃、没有关闭连接；服务端只是不回应。

### 1.2 静态证据：不是“文本行”协议

`GameViewer.exe` 的 `ipc_server_channel.cpp` / `ipc_local_server.cpp` 字符串显示：

```text
ipc_server_receive result=dropped reason=header_read_failed
ipc_server_receive result=dropped reason=invalid_magic
ipc_server_receive result=dropped reason=invalid_length
ipc_server_receive result=dropped reason=incomplete_message
ipc_server_receive result=dropped reason=payload_read_failed
ipc_message_parse result=failed reason=invalid_json
```

⇒ **Confirmed**：SERVER pipe 是“magic + header + length + JSON payload”的帧，不是裸 `cmd:` 文本行；直接写文本不会被解析。
⇒ **Inferred**：未响应的直接原因是帧头 magic/长度不对；需要精确 header 才能进入 `ipc_message_parse`。

### 1.3 静态证据：客户端身份/签名校验

同一区域还有：

```text
ipc_client_verify result=failed stage=get_process_id code=
ipc_client_verify result=failed stage=open_process code=
ipc_client_verify result=failed stage=query_image code=
ipc_client_verify result=success peer_pid=
ipc_client_signature result=skipped reason=test_environment
ipc_client_signature result=failed code=
ipc_client_signature result=success
response_security_check result=failed reason=signature_missing
response_security_check result=failed reason=signature_invalid
```

⇒ **Confirmed**：主程序会取客户端 PID、打开进程、查询 image，并做客户端签名/安全校验；存在 `test_environment` 跳过分支。
⇒ **Inferred**：非官方进程（PowerShell/python）大概率在 `ipc_client_verify` / `ipc_client_signature` 阶段被拒；即使帧头正确，也未必能完成 `cmd:status`/`cmd:echo`。
⇒ **Not tested**：精确 header/magic + 官方签名客户端；没有继续解析帧结构，因为这不是 PR5 的验收点。

**本项结论**：非官方进程**能连上** pipe，但**不能完成 status/echo**；失败点在帧协议/客户端身份校验，不是网络或权限。

---

## 2. device_terminal_launcher 的 host registration 函数

### 2.1 调用链（Confirmed）

`device_terminal_launcher.cpp` 的顺序：

```text
terminal_launch stage=start
terminal_launch result=failed reason=host_registration_failed   # 先注册
terminal_launch result=success launcher=windows_terminal pid=
terminal_launch result=failed reason=pending_host_process_update_failed  # 回填 PID
terminal_launch result=fallback reason=windows_terminal_config_unavailable fallback=new_console
terminal_launch result=success launcher=new_console
terminal_launch result=success launcher=detached_process pid=
```

- 输入：设备 ID、`local_host_kind`、`local_session_name`、`local_window_name`、`local_session_id`，以及 GUI 生成的 `local_host_token`。
- 输出：一个已注册的 host entry（`state=pending` → 回填 PID 后 `state=bound`），然后启动 `wt.exe -w <win> new-tab ... uuyc-cli.exe term ... --local-host-*`。
- registry 保存位置：**主程序进程内的 `terminal_host_registry` 表**（`terminal_host_register/retain/close/stale` 日志），不是 Windows 注册表、也不是持久文件；进程退出即消失。
- token 生命周期：GUI 注册 host 时生成，随命令行传给 CLI；close/stale 时随 host entry 回收。**token 格式/熵/是否一次性/是否绑定 PID 未验证。**

### 2.2 校验顺序（Confirmed）

主程序的终端连接解析器按顺序检查：

```text
Invalid terminal host token
  -> Invalid terminal host kind
  -> per-kind 必填字段（windows_terminal 需要 window name）
  -> Invalid terminal host process id
  -> Invalid terminal host window handle
  -> Invalid local terminal session id
  -> Terminal session is already open
```

受控端 `GameViewerServer.exe` 中**没有** `local_host` / `terminal_host` 字面量：绑定只在主程序/CLI 侧产生和校验，受控端不解析它。

### 2.3 host kind

- **Confirmed**：只有 `windows_terminal` 和 `console` 两种终端 host kind；另有 `new_console` / `detached_process` 启动方式。
- `windows_terminal` 需要真实 Windows Terminal 标签（registry 通过 UI Automation 操作 `CASCADIA_HOSTING_WINDOW_CLASS`）。
- `console` 可以在没有 WT 标签时注册为 `pending` 并被回收；GUI 自己会回退到 `new_console`，且存在 `detached_process` 启动路径。
- **Inferred**：`console` 是 headless/ConPTY 的目标 kind；只要提供 `local_process_id`（及有窗口时的 `local_window_handle`）和有效 token，就可能绑定。
- **Not tested**：`console` 的完整必填字段集、伪终端能否提供被接受的 window handle。

---

## 3. 随机 token + kind=console 是否被接受

**实测命令**（官方 CLI，交互终端由 ConPTY 提供）：

```text
uuyc-cli.exe term -d <local> new probe_rand
  --local-host-kind console
  --local-host-token <random-guid>
  --local-session-name probe_rand
  --local-window-name probe_rand
  --local-session-id 1
```

**Observed**：

- 进程约 **1.1 秒**结束，退出码 0；
- `term ls --json` 中没有 `probe_rand`，只有用户原有的 `bash-2`；
- 没有创建会话，没有残留。

**Inferred**：这是**快速拒绝**，不是 `check_env` 的 20 秒超时；按主程序校验顺序（token 最先），拒绝阶段最可能是 **`Invalid terminal host token`**。CLI 的终端输出未被 ConPTY 捕获，所以没有拿到错误原文；准确阶段只能按顺序推断。

**Not tested**：有效 token + `kind=console`、空 token、错误 kind 的对照；精确错误码/错误文本。

---

## 4. GUI 已注册 token 能否被另一 PID/headless console 重放

- **Not tested**：测试时没有存活的 GUI terminal CLI 进程，也没有 GUI 已注册的 token；要获得 token 必须先打开一次 Terminal（产生窗口/点击），与“零窗口零点击”的验收目标冲突。
- **Confirmed（静态）**：token 通过命令行传给 CLI；CLI 只转发，不生成；主程序会校验 token、kind、PID、window handle、local session id。
- **Inferred**：如果 token 绑定 PID/window，另一 PID 重放会在 `Invalid terminal host process id` / `Invalid terminal host window handle` 阶段失败；如果只做格式/存在性校验，则可能成功。需要一次“打开 Terminal 取 token，再用另一 PID 重放”的实验才能判定；本轮未做。

---

## 5. 如果 token 不能自造/重放，能否调用已有本地 IPC 注册

- **Confirmed**：SERVER pipe 命令表中**没有** `cmd:terminal_host_register`；host registry 是 GUI 进程内填充的，不是通过 IPC 暴露的。
- **Confirmed**：CLI 只声明 5 个 `--local-host-*` 参数，没有 `--local-process-id`/`--local-window-handle`；这两个字段由 CLI 自己填充。
- **Inferred**：普通后台进程无法通过现有本地 IPC 让主程序“注册一个新 host/token”；除非注入 GUI 进程、伪造官方签名客户端，或由官方增加 headless kind / register verb——这些都不是 uu-cli 客户端层能做的。
- **Not tested**：是否存在未文档化的 IPC op 可触发 host registration；当前命令表/字符串中没有发现。

---

## 6. 最终验收问题

> 官方 UU 已登录、没有打开 Terminal/远程桌面，从普通后台进程能否完成 `pwd`，全程零窗口零点击？

**答案：No（当前官方客户端）。**

依据：

1. **Confirmed**：token 由 GUI 的 host registration 生成；SERVER pipe 没有注册 host/token 的 IPC verb；普通后台进程不能自造。
2. **Observed**：随机 token + `kind=console` 被快速拒绝，没有创建会话。
3. **Observed/Confirmed**：非官方进程能连 SERVER pipe，但拿不到 status/echo 响应；服务端有客户端 image/签名校验和二进制帧协议。
4. **Confirmed**：官方 CLI 需要交互终端 + 本地宿主绑定；`windows_terminal` 需要 WT 标签，`console` 需要主程序注册的 host entry。
5. **Not tested**：GUI token 重放；如果 token 不绑定 PID/window，理论上可以“打开一次 Terminal 取 token，之后后台复用”，但这仍不是零点击，且未验证。

**能够改变答案的路径**（都不是当前客户端层能完成的）：

- 官方增加 `headless` host kind 或 `terminal_host_register` IPC；
- 官方 token 校验宽松到允许自造（本次随机 token 实测已否定）；
- 使用官方签名 CLI + 一次 GUI 注册的 token（仍需一次 Terminal 打开）。

**因此 PR5 的验收结论是 No，而不是“未验证”或“理论上可以”。** 只有在上述 vendor 改动或一次 GUI 注册后，才可能做到后台 `pwd`。

---

## 7. 置信度汇总

| 结论 | 状态 |
|---|---|
| 非官方进程能连 SERVER pipe | Observed |
| 裸文本/长度前缀/JSON 猜测帧无响应 | Observed |
| SERVER pipe 是 magic+header+length+JSON 帧 | Confirmed（字符串/日志） |
| 主程序做客户端 PID/image/签名校验 | Confirmed（字符串/日志） |
| 非官方客户端被身份校验拒绝 | Inferred |
| token 由 GUI 生成/注册，CLI 仅转发 | Confirmed |
| registry 在主程序进程内，非持久 | Confirmed（日志/代码结构） |
| 主程序按 token→kind→pid→window→session 校验 | Confirmed |
| 受控端不解析 local_host_* | Confirmed |
| host kind 只有 windows_terminal / console | Confirmed |
| 随机 token + console 被拒绝 | Observed |
| 拒绝阶段是 token | Inferred（顺序 + 快速失败） |
| GUI token 可被另一 PID 重放 | Not tested |
| 现有 IPC 可注册 host/token | No — Confirmed（无 verb） |
| 零窗口零点击完成 `pwd` | **No（当前客户端）** |
