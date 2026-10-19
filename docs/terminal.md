# 终端通道互操作说明

本文描述 UU Remote（NetEase GameViewer）终端子系统的通道格式、消息字段和会话生命周期，供 `uu-cli` 及其它本地工具复用。内容基于官方 Windows 客户端 4.43.0.3607 与 4.43.1 的协议行为对照；两个版本的 protobuf 描述一致。

标注约定：

- **确认**：字段、类型、数值或顺序在协议行为中可直接对应。
- **推断**：与已确认部分一致，但还缺少一个直接证据链。
- **待确认**：当前无法从静态行为中确定，需要实际连接观察。

---

## 1. 传输与帧格式

终端控制通道使用带长度前缀的 `TERM` 帧。每个帧由 16 字节头和可变长载荷组成：

| 偏移 | 长度 | 字段 | 说明 |
|---:|---:|---|---|
| 0x00 | 4 | magic | ASCII `TERM`（`54 45 52 4D`） |
| 0x04 | 1 | version | 固定 `0x01` |
| 0x05 | 1 | type | 帧类型，见下表 |
| 0x06 | 2 | channel | 管理通道编号，小端 u16；控制请求使用 `0` |
| 0x08 | 4 | reserved/seq | 小端 u32；控制请求中观察到为 `0` |
| 0x0c | 4 | payload_len | 小端 u32，最大 `0x100000` |
| 0x10 | n | payload | JSON 文本或原始字节，取决于 type |

在流式传输上，每个帧前还有一个 **u32 小端长度前缀**。接收端先读长度，再读完整帧。长度前缀的精确覆盖范围（是否包含 16 字节头）属于实现细节，当前按“前缀 + 完整帧”处理。

**确认**：magic、version、type、channel、payload_len 的位置与含义；控制请求 channel=0。
**推断**：偏移 0x08 的 u32 为序号/保留字段；当前所有控制请求均为 0。

---

## 2. 帧类型

| 值 | 名称 | 方向 | 载荷 |
|---:|---|---|---|
| 1 | `open` | 主控 → 受控 | JSON |
| 2 | `stdin` | 主控 → 受控 | 原始字节 |
| 3 | `resize` | 主控 → 受控 | JSON |
| 4 | `close` | 主控 → 受控 | 无 |
| 5 | `stdout` | 受控 → 主控 | 原始字节 |
| 6 | `exit` | 受控 → 主控 | JSON |
| 7 | `signal` | 主控 → 受控 | JSON |
| 8 | `error` | 受控 → 主控 | JSON |
| 9 | `list_sessions_req` | 主控 → 受控 | 无 |
| 10 | `list_sessions_resp` | 受控 → 主控 | JSON |
| 11 | `kill_session_req` | 主控 → 受控 | JSON |
| 12 | `kill_session_resp` | 受控 → 主控 | JSON |
| 13 | `open_resp` | 受控 → 主控 | JSON |
| 14 | `unlock_req` | 主控 → 受控 | JSON |
| 15 | `unlock_resp` | 受控 → 主控 | JSON |
| 16 | `detach` | 主控 → 受控 | 无 |

**确认**：1–16 的数值与名称对应关系；`kill_session_req=11`、`kill_session_resp=12`。
**注意**：没有 `TERM`/`TRES`、`RENAME`、`LIST` 这类独立帧类型；重命名走 protobuf RPC（见第 5 节）。`OPEN`/`RESIZE`/`SIGNAL` 只出现在日志文本中，不是线上的 type 值。

---

## 3. 会话管理

### 3.1 列表

`list_sessions_req`（type 9）无载荷；受控端返回 `list_sessions_resp`（type 10），载荷为 JSON：

```json
{
  "sessions": [
    {
      "session_id": 42,
      "name": "work",
      "state": "running",
      "last_active_ms": 123456,
      "created_at_ms": 123000,
      "attachable": true
    }
  ]
}
```

**确认**：`session_id` 非零、`name` 非空、`state` 为 `running`/`detached`/`exited`。
**推断**：外层 `sessions` 键；`last_active_ms`/`created_at_ms`/`attachable` 的字段名来自客户端校验字符串。部分路径也接受 `createdAt` 拼写，实际实现建议同时兼容两种。

### 3.2 终止

`kill_session_req`（type 11）：

```json
{"session_id": 42}
```

`kill_session_resp`（type 12）：

```json
{"code": 0, "message": "ok"}
```

**确认**：请求 JSON 只有 `session_id`（整数）；响应 JSON 读取 `code`（整数）和 `message`（字符串）；成功时 `code=0`；`code=404` 且 `message="session_not_found"` 表示会话不存在。
**确认**：终止请求的 channel=0；响应必须为 type 12。客户端等待响应的超时为 15 秒。

---

## 4. 终止应答与会话变更通知（问题一）

### 4.1 结论

**只回 type 12 的 `{"code":0,"message":"ok"}` 不足以让主控端的会话列表消失。** 主控端还需要一条会话变更通知，然后重新拉取会话列表。

推荐顺序：

```
主控                                          受控
  |-- kill_session_req (type 11, channel=0, {"session_id":N}) -->|
  |<-- kill_session_resp (type 12, 同一 channel, {"code":0,"message":"ok"}) --|
  |<-- TerminalSessionChanged（gv_pb Message 字段 28，空消息） --|
  |-- list_sessions_req (type 9) -->|
  |<-- list_sessions_resp (type 10, sessions 数组) --|
```

### 4.2 字段与消息类型

- **确认**：终止请求 = `TERM` 帧 type 11，channel=0，JSON `{"session_id":<int>}`。
- **确认**：终止应答 = `TERM` 帧 type 12，JSON `{"code":<int>,"message":<string>}`；成功为 `{"code":0,"message":"ok"}`；错误时 `message` 为 `session_not_found`（`code=404`）或 `invalid_kill_session_request`。客户端只按 type 12 + `code` 匹配，不校验 channel。
- **确认**：`TerminalSessionChanged` 是 `gv_pb.Message` 的 oneof 字段 28，消息体为空，没有 `session_id`，因此它只能表示“有变化，请刷新”。
- **确认**：受控端的处理顺序是先发送 type 12，再触发 `TerminalSessionChanged` 推送；推送是异步的，到达顺序不保证。
- **确认**：type 12 应答本身不携带会话清单，不能清除列表；只回 type 12 而不推送 `TerminalSessionChanged`，列表不会消失。
- **推断**：客户端收到空通知后发 `list_sessions_req`，用 `list_sessions_resp` 更新列表。
- **待确认**：如果只发 `TerminalSessionChanged` 而不发 type 12，主控端是否会完成终止请求；建议两者都发，顺序如上。

### 4.3 路由条件

- 应答必须使用与请求相同的管理通道编号（channel）。客户端侧的 kill 等待器不比较 channel，但服务端按连接句柄路由；不同通道的应答可能被丢弃或归错会话。
- `session_id` 必须与请求一致；受控端以 `session_id` 查找会话，找不到时返回 404/`session_not_found`。
- `kill_session_resp` 只表示请求完成，不表示列表已刷新；不要把它当作会话变更通知。

---

## 5. 环境检测（问题二）

### 5.1 消息

`CheckEnvReq` 是空消息；`CheckEnvResp` 只有两个字段：

```proto
message CheckEnvReq {}
message CheckEnvResp {
  int32  code    = 1;
  string message = 2;
}
```

两者通过 `gv_pb` RPC 信封传输：

- `Message.rpc_request` = 21，`RpcRequest.terminal_check_env_req` = 28。
- `Message.rpc_response` = 22，`RpcResponse.terminal_check_env_resp` = 24。
- `RequestHeader.request_id` / `ResponseHeader.request_id` 都是 int64；请求与响应必须使用同一个 `request_id`。

**确认**：消息字段、RPC 字段号、`request_id` 关联、20 秒等待超时。
**确认**：`CheckEnvResp.code=0` 表示成功；非 0 表示失败。

### 5.2 发送条件

`check_env` 只出现在“新建/附着终端”的驱动路径中，不是独立命令：

1. 主控端建立终端控制通道（新建连接，或复用已有桌面连接）。
2. 在绑定会话之前，发送 `CheckEnvReq`（空体 + `request_id`）。
3. 等待同一 `request_id` 的 `CheckEnvResp`；超时 20 秒。
4. 成功后记录 `terminal_access source=environment_check`，再进入 `open`。
5. 如果走的是解锁路径，则记录 `source=unlock_response`，不再重复环境检测。

**确认**：`check_env` 是 RPC，不是新连接；它发生在会话绑定之前。
**确认**：受控端只有在 `CheckEnvResp.code == 0` 且 `message == "ok"` 时才授予访问；授权按连接保存，最后一次成功会覆盖前一次。
**推断**：复用桌面连接时，如果该连接已经完成过环境检测/授权，主控端会跳过 `check_env`；只有新建终端通道才重新发送。这是“车端没有看到新的 check_env”的最可能原因。
**推断**：桌面与终端共享连接时，授权槽可能已由桌面连接填充；终端复用同一连接时不需要新的 `check_env`，因此受控端看不到新请求。
**推断**：如果受控端只在会话 id 存在后才记录请求，也会看不到 `check_env`，因为它发生在会话绑定之前。

### 5.3 失败路径

“终端环境检测失败”对应以下情况：

| 情况 | 客户端行为 |
|---|---|
| 请求无法发送/序列化失败 | 立即失败，提示发送失败 |
| 等待超过 20 秒 | 超时失败 |
| 收到 `request_id` 不匹配的响应 | 丢弃并继续等待 |
| 收到无法解析的响应 | `invalid terminal environment response` |
| 通道在等待期间断开 | `terminal disconnected while checking environment` |

**确认**：超时、request_id 不匹配、响应解析失败、通道断开这四条路径。
**推断**：复用桌面连接时跳过检测，因此不会产生新的 `check_env`；如果此时仍提示检测失败，应检查复用连接的授权状态，而不是等待新的 `check_env`。

---

## 6. 通道与生命周期

- **管理通道**：`TERM` 帧头中的 u16 `channel`；控制请求使用 0。服务端还维护一个每连接的内部句柄，用于回复路由和会话归属校验；该句柄是否在管道上显式传输，当前未确认。
- **终端数据通道**：`stdin`/`stdout` 走同一帧通道，载荷为原始字节；不要把它们当 JSON 解析。
- **RPC 通道**：`gv_pb.Message` 承载 `rpc_request`/`rpc_response`/`terminal_session_changed`，与 `TERM` 帧是同一终端连接上的不同封装。
- **复用桌面连接**：终端可以复用已有的桌面连接；此时环境检测可能被跳过，授权来源变为 `unlock_response` 或已有的 `environment_check`。
- **独立终端**：新建连接时执行完整的环境检测，再 `open`。

**确认**：帧类型、RPC 字段号、`check_env` 在绑定前发送。
**推断**：复用路径跳过 `check_env`；具体门槛（`reuse_session`、`reuse_existing_channel`、`pb_connected`）来自客户端状态字符串。

---

## 7. 对 `uu-cli` 的实现建议

### 7.1 最短路径

官方 `uuyc-cli.exe` 已经实现同一套终端协议，并提供：

```
uuyc-cli term -d DEVICE new [NAME] [--shell cmd]
uuyc-cli term -d DEVICE attach [NAME]
uuyc-cli term -d DEVICE ls [--json]
uuyc-cli term -d DEVICE has NAME
uuyc-cli term -d DEVICE rename OLD NEW
uuyc-cli term -d DEVICE kill NAME
```

`term new/attach` 会接管当前终端，`--json` 只适用于管理命令。`uu-cli` v1 可以先包装这些命令：用 PTY 运行 `term new/attach`，把命令写入 stdin，从 stdout 读结果；管理命令用 `--json` 解析。

限制：

- `term` 是交互式终端，不是一次性 exec；退出码和输出边界需要从终端流中解析。
- 设备 ID 需要从官方客户端配置/命令中获取；`uu-cli` 不应保存账号凭据。
- 文件传输是独立通道，不在本文范围。

### 7.2 无头实现

如果需要不依赖交互式终端，可以直接实现本文的 `TERM` 帧 + JSON 载荷，以及 `gv_pb` RPC 信封：

1. 建立/复用终端控制通道。
2. 发送 `CheckEnvReq`，等待同 `request_id` 的 `CheckEnvResp`。
3. 发送 `open`（type 1），等待 `open_resp`（type 13）。
4. 发送 `stdin`（type 2），读取 `stdout`（type 5）和 `exit`（type 6）。
5. 终止时发送 `kill_session_req`（type 11），等待 type 12，再处理 `TerminalSessionChanged` 并刷新列表。

**确认**：帧格式、type 值、kill/check_env 字段。
**待确认**：长度前缀的精确布局、连接句柄是否随帧传输、JSON 字段可选性。实现前先用一条最小连接验证这三点。

---

## 8. 置信度与未确认项

| 结论 | 置信度 |
|---|---|
| 16 字节 `TERM` 帧头与字段偏移 | 确认 |
| 帧类型 1–16 与名称 | 确认 |
| kill 请求 type 11 / 响应 type 12 / JSON 字段 | 确认 |
| kill 响应不刷新列表 | 确认 |
| `TerminalSessionChanged` 为空信号，需重新拉列表 | 确认（消息定义与发送）；客户端重拉为推断 |
| 终止处理先发 type 12 再触发通知 | 确认（发送顺序）；到达顺序不保证 |
| `CheckEnvReq` 空 / `CheckEnvResp{code,message}` / request_id 关联 | 确认 |
| 复用桌面连接时跳过 `check_env` | 推断 |
| 长度前缀精确布局、连接句柄传输方式 | 待确认 |
| JSON 字段可选性、`createdAt` 别名 | 待确认 |

未确认项不影响最小实现，但会影响兼容性和错误处理。建议在实现前用一条最小连接验证：长度前缀、channel 回显、`TerminalSessionChanged` 后的列表刷新。
