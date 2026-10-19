# 终端通道补充说明

本文补充上一版《终端通道互操作说明》，修正三处格式细节，并回答两个仍未闭环的问题：会话变更通知走哪条 DataChannel，以及桌面与终端切换时在哪一步断开。

标注：**确认** = 代码/消息定义可直接对应；**推断** = 与已确认部分一致但缺一个直接证据；**待确认** = 需要实际连接观察。

---

## 1. 三处格式修正

### 1.1 帧头字节序：小端

`TERM` 帧的 16 字节头按小端解释：

| 偏移 | 长度 | 字段 |
|---:|---:|---|
| 0x00 | 4 | magic `TERM` |
| 0x04 | 1 | version = 1 |
| 0x05 | 1 | type |
| 0x06 | 2 | channel / 管理通道编号（u16 LE） |
| 0x08 | 4 | session_id / route_id（u32 LE） |
| 0x0c | 4 | payload_len（u32 LE，最大 0x100000） |
| 0x10 | n | payload |

**确认**：客户端写入和服务端读取都是原始 x86 读取，没有字节交换；`payload_len` 与总长度校验为 `16 + len`。
**注意**：不要按大端解析 0x06/0x08/0x0c。

### 1.2 偏移 0x08：会话/路由编号，不是保留字段

- 控制请求（kill/list 等）该字段为 `0`。
- 会话相关帧（open/stdin/stdout/exit 等）该字段为非零，服务端用它把帧关联到具体会话。
- 终止/列表处理器会把该值原样传给响应发送器；会话帧的响应使用同一个值。

**确认**：控制请求为 0；会话帧非零；服务端按该值查会话。
**推断**：具体是 `session_id` 还是 `route_id` 取决于连接层；实现时按“会话/路由编号”原样回显即可。

### 1.3 `list_sessions_resp`：裸数组，不是 `{"sessions":[...]}`

远端 `list_sessions_resp`（type 10）的载荷是 JSON 数组：

```json
[
  {
    "session_id": 42,
    "name": "work",
    "state": "running",
    "shell": "powershell",
    "last_active_ms": 123456,
    "created_at_ms": 123000
  }
]
```

**确认**：服务端列表处理器直接构造数组并作为 type 10 载荷发送；没有 `sessions` 外层键。
**确认**：客户端校验要求顶层是数组；`session_id` 非零、`name` 非空、`state` 为 `running`/`detached`/`exited`。
**注意**：`uuyc-cli` 的**本地管理命令**响应是另一层，形如 `{"data":{"items":[...]},"message":...,"success":...}`。不要把本地命令响应格式套到远端 `list_sessions_resp` 上。

---

## 2. 会话变更通知走哪条 DataChannel

远端有两条按名字区分的 DataChannel：

| 通道 | 承载内容 | 证据 |
|---|---|---|
| `CONTROL_DATA_CHANNEL`（id = 2） | `gv_pb.Message`：RPC 请求/响应、`terminal_session_changed` 等通知 | 名字比较后立即 `mov ecx,2`；`control_data_send`；`terminal_message_receive`/`invalid_envelope` |
| `BINARY_DATA_CHANNEL` | `TERM` 帧、原始终端字节（stdin/stdout） | `binary_data_send`；`terminal_binary_channel`；`terminal_frame_send`/`terminal_frame_receive` |

**确认**：`CONTROL_DATA_CHANNEL` 的 id 为 2，承载 `gv_pb.Message`。
**待确认**：`BINARY_DATA_CHANNEL` 的数值 id；但它的用途明确，是 `TERM` 帧/原始字节通道。

`TerminalSessionChanged` 是 `gv_pb.Message` 的 oneof 字段 28，消息体为空。它必须作为 `gv_pb.Message` 发在 **`CONTROL_DATA_CHANNEL`（id 2）** 上，不能发在 `BINARY_DATA_CHANNEL` / `TERM` 帧通道上。

```text
正确：CONTROL_DATA_CHANNEL(id=2) -> gv_pb.Message{ terminal_session_changed = {} }
错误：BINARY_DATA_CHANNEL      -> TERM 帧 type=... 或裸 JSON
```

**确认**：字段号 28、空消息、oneof 名称 `terminal_session_changed`。
**推断**：客户端在 `onReceiveControlData` 路径解析 `gv_pb.Message`；`onReceiveBinaryData` 只处理原始字节。

---

## 3. 列表残留：通知与刷新

### 3.1 服务端行为

终止请求（type 11）处理完成后：

1. 发送 `kill_session_resp`（type 12，JSON `{"code":0,"message":"ok"}`）。
2. 触发 `TerminalSessionChanged`（`gv_pb.Message` 字段 28，空消息）。
3. 不主动推送列表；`list_sessions_resp`（type 10）只回答 `list_sessions_req`（type 9）。

**确认**：应答与通知是两条独立发送路径；通知是异步的，到达顺序不保证。
**确认**：type 12 不携带会话清单，不能清除列表。

### 3.2 客户端行为

- `uuyc-cli`：没有 `TerminalSessionChanged` 处理，也不缓存列表；`ls` 每次都发 `list_sessions_req`。因此对 CLI 来说，丢不丢字段 28 不影响列表。
- `GameViewer` UI：`gv_pb.Message` 分发器与列表发送点已定位，但终端函数尚未被定义为函数，无法确认它收到字段 28 后是“重新拉列表”还是“本地删除行”。**待确认**。

**结论**：服务端“只回 type 12 不够”是确认的；客户端 UI 的具体刷新动作仍是推断。实际实现应先发 type 12，再在 `CONTROL_DATA_CHANNEL` 发字段 28；如果 UI 仍不刷新，问题在 UI 处理器，而不是应答格式。

---

## 4. 桌面与终端切换：断开发生在哪一步

### 4.1 正常关闭

- 客户端发送 `close`（type 4）或 `detach`（type 16），携带 `session_id`。
- 服务端校验该会话的 connection handle 是否等于当前连接：
  - 匹配：移除/分离会话，并触发 `TerminalSessionChanged`。
  - 不匹配：返回错误或延迟处理。
- **确认**：正常关闭由客户端发起；服务端按 connection handle 校验。

### 4.2 桌面/终端切换

客户端在进入终端前会请求 `terminal_room_takeover`；成功后才进入 `terminal_peer_ready result=waiting_binary_channel`，再等待 `terminal_binary_channel result=ready`。

顺序：

```text
terminal_attach / connect
  -> terminal_room_takeover (requested -> success)
  -> 旧桌面 peer 被关闭（peer_closed / Room kick out / Room logout）
  -> terminal_peer_ready waiting_binary_channel
  -> terminal_binary_channel ready
  -> open / check_env / 数据帧
```

- **推断（强）**：断开发生在 `terminal_room_takeover` 成功之后、终端 binary channel 就绪之前；旧桌面连接收到 `peer_closed`/`Room kick out`。
- **待确认**：具体是房间服务、服务端还是客户端发起关闭；服务端的 `terminal_connection_generation` 处理器只注册新 handle，没有定位到直接关闭旧 peer 的调用。
- **确认**：客户端日志把 `terminal_connection result=closed` 与 `state_code`、`peer_closed`、`Room kick out`、`Room logout` 放在同一状态机里。

### 4.3 复用桌面连接

如果走 `terminal_attach result=reuse_session` / `reuse_existing_channel`，终端附着在已有连接上，不触发 room takeover，也不应产生上述断开。**确认**：复用路径存在；**推断**：复用时不发生断开。

---

## 5. 给车端的落地顺序

1. 收到 `kill_session_req`（type 11，`session_id`）后，先回 type 12：
   ```json
   {"code":0,"message":"ok"}
   ```
   错误时用 `session_not_found`（404）或 `invalid_kill_session_request`。
2. 在 `CONTROL_DATA_CHANNEL`（id 2）发送：
   ```text
   gv_pb.Message { terminal_session_changed: {} }   // field 28
   ```
3. 客户端随后会发 `list_sessions_req`（type 9）；回 `list_sessions_resp`（type 10），载荷是裸数组。
4. 如果客户端是 `uuyc-cli` 而不是 UI，它不会等字段 28；`ls` 直接查询即可。

---

## 6. 置信度与未确认项

| 结论 | 置信度 |
|---|---|
| 帧头小端；0x08 为会话/路由编号 | 确认 |
| 远端 `list_sessions_resp` 为裸数组 | 确认 |
| `CONTROL_DATA_CHANNEL` id=2 承载 `gv_pb.Message` | 确认 |
| `TerminalSessionChanged` 走 control channel、字段 28、空消息 | 确认 |
| type 12 应答不刷新列表；服务端另发通知 | 确认 |
| GameViewer UI 收到字段 28 后重新拉列表 | 待确认（处理器未定义） |
| 桌面/终端切换在 room takeover 后、binary channel 前断开 | 推断 |
| 断开由哪一侧发起 | 待确认 |
| `BINARY_DATA_CHANNEL` 数值 id | 待确认 |
| `RpcResponse` 字段 25、JSON 字段可选性 | 待确认 |
