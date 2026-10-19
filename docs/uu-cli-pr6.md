# uu-cli PR6：`detached_process` 路径完整追穿

本文只追 `device_terminal_launcher.cpp` 的第三条 launcher：`detached_process`。不扩展 TERM 协议，不研究其他功能。

标注：**Confirmed** = 代码/字面量可直接对应；**Observed** = 之前有限实测；**Inferred** = 与已确认部分一致但缺直接测量；**Not tested** = 未测。

> 本轮只做静态/副本分析，没有执行官方程序、没有创建/操作实时会话、没有写 Program Files。

---

## 1. 它在哪里、什么条件下触发

**Confirmed**：`device_terminal_launcher.cpp` 里有三条 launcher，按顺序回退：

```text
windows_terminal
  -> new_console
    -> detached_process
      -> all_launchers_failed
```

日志顺序（同一 TU）：

```text
terminal_launch stage=start platform=  executable_path_chars=
terminal_launch result=failed reason=executable_missing
terminal_launch result=failed reason=device_unavailable
terminal_launch result=failed reason=host_registration_failed
terminal_launch result=success launcher=windows_terminal pid=
terminal_launch result=fallback reason=windows_terminal_failed
terminal_launch result=fallback reason=windows_terminal_config_unavailable fallback=new_console
terminal_launch result=fallback reason=windows_terminal_elevated_profile
terminal_launch result=success launcher=new_console
terminal_launch result=failed launcher=new_console code=
terminal_launch result=success launcher=detached_process pid=
terminal_launch result=failed reason=all_launchers_failed
terminal_launch result=failed reason=pending_host_process_update_failed
```

⇒ **触发条件**：Windows Terminal 路径失败/不可用，且 `new_console` 也失败后，才尝试 `detached_process`；它是**最后兜底**，不是默认路径，也不是用户可选的“无窗口模式”。

**调用者（Confirmed）**：只有 GUI 的 `home_terminal_window_manager.cpp`（`terminal_window_open` / `terminal_connection_prompt`）。`terminal_connection_prompt` 的 source 只有四个，全部是 GUI 事件：

```text
source=home_frame
source=disconnect_device
source=my_device_removed
source=cloud_device_removed
```

**没有非 GUI 入口（Confirmed，三面穷举）**：

- SERVER pipe 的 30+ 个 `cmd:` verb 中没有 terminal-window/terminal-launch verb；
- 39 个 `ipc_request operation=` 名称中没有 terminal 操作；
- deeplink 只有 `uuremote://external/{assistance,cloudDeviceAdd,cloudDeviceMarket,purchase/center}`，没有 terminal 路由；
- `cmd:terminal:` 是 **attach/connect only**：需要 `device_id` + 完整 host metadata，拒绝信息是 `Invalid terminal command` / `Invalid terminal connect JSON` / `device_id is required` / `Incomplete terminal host metadata`，没有 create/launch 分支。

---

## 2. 启动前是否仍调用 `terminal_host_register`

**Confirmed：是。**

`terminal_launch result=failed reason=host_registration_failed` 是 launcher 阶段的失败，出现在任何 `result=success launcher=...` 之前；最后一步 `pending_host_process_update_failed` 是启动后回填 PID 的失败。顺序是：

```text
register -> launch -> back-fill PID
```

**注册的 host kind（Inferred）**：`windows_terminal` 只对应 `wt.exe` 路径；`new_console` 和 `detached_process` 是非 WT 路径，注册 **`console`**。主程序 validator 只接受 `windows_terminal` 和 `console` 两种 kind。

---

## 3. Token 从哪里来、由谁注册

**Confirmed**：

- token 在 GUI 的 `terminal_host_register` 阶段生成/注册；
- 通过共享命令模板以 argv 传给子进程：

```text
uuyc-cli.exe uuyc-terminal-%1 -d <device> new|attach
  --local-host-token <t>
  --local-host-kind <k>
  --local-session-name <n>
  --local-window-name <w>
  --local-session-id <id>
```

- CLI 只把它转发进 `cmd:terminal:`（字段 `local_host_token/local_host_kind/local_window_name/local_session_name/local_session_id/local_process_id/local_window_handle`）；
- token 不通过 IPC 反向传递。

**Unverified**：token 格式/熵/生命周期/是否一次性/是否绑定 PID。

---

## 4. PID / window handle / session id 怎么填

**Confirmed**（由失败阶段名推断出顺序）：

- 注册时创建 registry entry，状态 `pending`；
- 启动子进程后，launcher 把子进程 PID 回填到 entry；失败即 `pending_host_process_update_failed`；
- `terminal_host_stale ... kind=console ... state=pending` 与 `kind=windows_terminal ... reason=tab_missing state=bound` 对应 `pending`→`bound`。

**Inferred**：

- `local_process_id` = 启动的 `uuyc-cli.exe` PID；
- `local_session_name`/`local_session_id` = 生成的 `uuyc-terminal-<n>` 对；
- `local_window_handle` 只对 `windows_terminal` 有意义；`detached_process` 没有窗口，handle 可能是 0/缺省。validator 有 `Invalid terminal host window handle`，但 `console` kind 是否强制要求它，**Not tested**。

---

## 5. 它是否真的全程无窗口

**Not tested（flags 是立即数，没有字符串）**：没有 `CREATE_NO_WINDOW` / `DETACHED_PROCESS` / `SW_HIDE` / `conhost` / `OpenConsole` 字符串可直接确认。

**但有一条决定性约束（Confirmed）**：

- `uuyc-cli.exe` 导入 `GetConsoleMode`、`SetConsoleMode`、`GetConsoleWindow`、`GetStdHandle`、`ReadConsole`；
- **没有**导入 `AllocConsole`、`FreeConsole`、`AttachConsole`；
- CLI 自己报 `term new/attach requires an interactive terminal`。

⇒ **Inferred**：CLI 不能自己创建控制台；真正“无控制台”的 detached 子进程会被 CLI 自己的 guard 拒绝。`detached_process` 必须留下一个**可用但无窗口的控制台**（`CREATE_NO_WINDOW` 风格），而不是 `DETACHED_PROCESS` 的“无控制台”。launcher 名称很可能是误称。

**旁证（Confirmed）**：GameViewer 只导入 `CreateProcessW` 和 `CreateProcessAsUserW`，正好对应两个非 WT launcher：`new_console`（可见控制台）和 `detached_process`（活动用户 token + 无窗口控制台）。

---

## 6. 能否不点 GUI Terminal，让已登录主程序走这条路径

**No — Confirmed（负结论，按 §1 穷举）**：

- 没有 IPC verb；
- 没有 `ipc_request operation=`；
- 没有 deeplink 路由；
- 没有 CLI 子命令；
- `cmd:terminal:` 只做 attach，不做 create/launch。

进入 launcher 的每一条边都是 `home_terminal_window_manager.cpp` 的 GUI 事件。

---

## 7. 真正阻塞点

**Inferred：是“缺少调用入口”，不是“必须在 GUI 进程内注册 host”。**

- `terminal_host_register` 就在 `device_terminal_launcher.cpp` 里，是 launcher 的一个阶段；任何能调用 launcher 的入口都会自动获得注册 + token + 启动。
- 真正的两个阻塞点按顺序：
  1. **没有调用入口**：GUI 之外没有任何 surface 能调用 `terminal_window_open` / `device_terminal_launcher`；
  2. **进程边界**：即使有入口，launcher 必须在主程序进程内运行（它操作进程内 registry 和用户 token），独立客户端只靠 pipe 无法复现。

---

## 8. 最终结论

> 当前官方客户端内部是否已经存在可复用的 zero-window / zero-click terminal launch path？

**No。**

官方客户端**拥有除触发入口之外的所有部件**：

- `detached_process` 是最后兜底 launcher；
- 注册、token、argv、PID 回填都在 launcher 内；
- 但它只由 GUI 的四个窗口事件触发；
- 没有 IPC/deeplink/CLI 入口；
- 而且它大概率是“无窗口控制台”，不是真正的无控制台 detach。

对 `uu-cli` 的可复用资产是**协议和 registry 契约**，不是 launcher 本身。零点击 `uu pwd` 仍然回到 PR4 的未证明步骤：**token 没有 IPC 注册入口**。

---

## 9. 置信度

| 结论 | 状态 |
|---|---|
| `detached_process` 是第三条/最后兜底 launcher | Confirmed |
| 触发条件是 WT 与 new_console 均失败 | Confirmed |
| 只有 GUI window manager 调用 launcher | Confirmed |
| 无 IPC/deeplink/CLI 入口 | Confirmed（穷举） |
| 启动前仍调用 `terminal_host_register` | Confirmed |
| 非 WT 路径注册 kind=console | Inferred |
| token 由 GUI 注册、argv 传给 CLI | Confirmed |
| PID 回填、pending→bound | Confirmed |
| `detached_process` 是无窗口控制台而非真 detach | Inferred |
| 具体 creation flags / console handle 要求 | Not tested |
| 当前客户端有可复用的 zero-window/zero-click 路径 | **No** |
