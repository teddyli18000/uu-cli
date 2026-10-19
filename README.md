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

除此之外，不扩展一套新的 shell、进程管理或任务 API。

## 核心约定

- `uu <command...>`：除保留的 `put` / `get` 外，后面的内容就是远端 shell 命令。尽可能原样传递，不解释 `git`、`python`、`cmake` 等业务命令。
- `uu put <local> <remote>`：把本地文件传到远端。
- `uu get <remote> <local>`：把远端文件取回本地。
- 默认目标设备在安装/配置阶段确定。日常执行不要求再写设备名或设备 ID。
- 长时间运行、交互输入、stdout/stderr、退出码应尽可能表现得像直接在远端 shell 中运行。
- 运行时不弹控制台、不弹 UU 工具窗口、不提供托盘 UI。

## 生命周期

用户不管理 `uu-cli` 的启动和停止。

理想行为：

```text
Windows UU Remote 启动
        ↓
uu-cli 后台能力静默可用
        ↓
Codex / 用户随时执行 `uu ...`
        ↓
Windows UU Remote 完全退出
        ↓
uu-cli 后台能力随之退出
```

不设计 `uu start` / `uu stop`。具体如何跟随 UU 生命周期、如何连接 UU、如何复用会话，由实现根据实际 Windows UU 行为决定，不在这里预设。

## 安装体验

Release 最终提供一个 Windows 安装程序，例如：

```text
uu-cli-setup.exe
```

运行一次完成安装：

- 安装 `uu` 命令并加入当前用户 PATH；
- 安装必要的后台组件与生命周期集成；
- 完成最少量的目标设备配置；
- 不修改或替换官方 UU Remote；
- 默认不要求管理员权限，除非某项能力确实无法在用户权限下完成。

卸载必须同样简单：支持 Windows「已安装的应用」正常卸载，并允许安装程序执行卸载模式。卸载只清理 `uu-cli` 自己安装的文件、配置和集成，不删除 UU Remote、不清除 UU 登录状态。

## 非目标

本项目不做：

- UU 账号登录或扫码登录；
- 密码、Token、Cookie 等凭据管理；
- 绕过 UU 的认证或权限控制；
- 重新实现完整 UU 云协议；
- 为 Codex 设计一套新的任务/进程/开发工具 API；
- 运行时 GUI、托盘、常驻窗口；
- 把内部协议细节暴露给使用者。

## 对 Agent 的定位

Skill 只负责告诉 Agent：

```text
本地命令：python3 main.py
远端命令：uu python3 main.py

上传：uu put <local> <remote>
下载：uu get <remote> <local>
```

能力必须来自 CLI 本身，而不是依赖 Skill 里的复杂提示词或脚本。

更具体的产品边界和实现约束见 [DESIGN.md](DESIGN.md) 与 [AGENTS.md](AGENTS.md)。
