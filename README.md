# uu-cli

让远程 UU 设备像本地 shell 一样可用。

`uu-cli` 复用 **Windows 本地已经运行、已经登录的网易 UU 远程**。它不重新实现登录，不保存账号凭据，也不要求 Codex 学一套新的远程工具。

## 目标

对人和 Agent 来说，远程执行只多一个 `uu` 前缀：

```powershell
uu pwd
uu git status
uu python3 main.py --debug
uu "cd ~/aic && pytest -q"
```

文件传输只保留两个直观命令：

```powershell
uu put .\main.py ~/aic/main.py
uu get ~/aic/result.csv .\result.csv
```

如果账号下有多台设备，只额外保留最小配置入口：

```powershell
uu devices
uu use <device>
```

选定默认设备后，日常执行仍然只是 `uu <command...>`。目标设备离线时明确失败，不自动切换到其他设备。

## 当前 Windows UU 限制

对当前分析的官方 Windows UU 4.43.x，外部程序可以管理/附着已有 terminal session，但**没有可复用的零窗口、零点击 terminal launcher 入口**。官方 GUI 在创建终端时会先在主程序进程内注册本地 host/token，然后再启动官方 CLI；这个注册入口没有暴露给普通外部客户端。

因此 v1 采用一个很小的人工 bootstrap：

```text
启动并登录 Windows UU
        ↓
用户在 UU 中为目标设备打开一次官方 Terminal
        ↓
之后 Codex / 用户直接使用 `uu ...`
```

`uu-cli` 不要求用户管理 session，也不会自己弹出 UU Terminal。只要存在可复用的官方 terminal session，`uu` 就负责发现、附着/复用并执行命令；如果当前没有可用 session，明确提示用户在 UU 中打开一次终端，而不是偷偷操作 GUI。

如果未来官方 UU 暴露稳定的 headless host/register 接口，可以移除这一步，而不改变 `uu / put / get` 公共语法。

## 核心约定

- `uu <command...>`：除保留命令外，后面的内容就是远端 shell 命令。尽可能原样传递，不解释 `git`、`python`、`cmake` 等业务命令。
- `uu put <local> <remote>`：把本地文件传到远端。
- `uu get <remote> <local>`：把远端文件取回本地。
- `uu devices`：查看可用设备和当前默认目标。
- `uu use <device>`：修改默认目标设备。
- 长时间运行、交互输入和退出码应尽可能表现得像直接在远端 shell 中运行。
- 官方 terminal 是单一 PTY 字节流；stdout/stderr 不保证可区分。
- 主控 Windows 上 `uu-cli` 自己不弹控制台、不弹辅助 GUI、不提供托盘 UI。
- terminal attach、复用、命令边界、并发等内部细节不暴露给使用者。

## 生命周期

用户不管理 `uu-cli` 的启动和停止。

v1 的正常状态是：

```text
Windows UU Remote 启动并登录
        ↓
用户打开一次目标设备的官方 Terminal
        ↓
`uu-cli` 可复用该 session
        ↓
Codex / 用户随时执行 `uu ...`
        ↓
UU / session 失效时 `uu` 明确报错
```

不设计 `uu start` / `uu stop`。如果实现需要后台 helper，它不得独立于官方 UU 长期常驻。

## 安装体验

Release 最终提供一个 Windows 安装程序，例如：

```text
uu-cli-setup.exe
```

运行一次完成安装：

- 安装 `uu` 命令并加入当前用户 PATH；
- 安装必要的后台组件与生命周期集成；
- 首次需要时完成一次默认设备选择；
- 注册 Windows 标准卸载入口；
- 不修改或替换官方 UU Remote；
- 默认不要求管理员权限，除非某项能力确实无法在用户权限下完成。

卸载只清理 `uu-cli` 自己安装的文件、配置和集成，不删除 UU Remote、不清除 UU 登录状态。

## 非目标

本项目不做：

- UU 账号登录或扫码登录；
- 密码、Token、Cookie 等凭据管理；
- 绕过 UU 的认证或权限控制；
- 注入/伪造官方客户端来绕过 terminal host registration；
- 重新实现完整 UU 云协议；
- 为 Codex 设计一套新的任务/进程/开发工具 API；
- 运行时 GUI、托盘、常驻窗口；
- 面向用户的 session 管理；
- 把内部协议细节暴露给使用者。

## 对 Agent 的定位

Skill 只负责告诉 Agent：

```text
本地命令：python3 main.py
远端命令：uu python3 main.py

上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

如果 `uu` 报告“没有可用的官方 terminal session”，这是一个需要用户在 UU GUI 中打开一次 Terminal 的明确前置条件，而不是 Agent 自己去点 GUI。

更具体的产品边界见 [DESIGN.md](DESIGN.md)，实现阶段规则见 [AGENTS.md](AGENTS.md)，阶段规划见 [ROADMAP.md](ROADMAP.md)。终端研究见 `docs/` 下的互操作笔记。
