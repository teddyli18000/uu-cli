---
name: uu-cli
description: Run commands and transfer files in the current workspace's UU Remote binding from Windows using uu.
---

Use `uu <command...>` for remote execution, `uu put <local> <remote>` to upload, and `uu get <remote> <local>` to download.

Different `uu` calls do not share shell-local state such as `cd/export/source/alias/virtualenv`. Put dependent operations in one call, for example `uu "cd src && source .venv/bin/activate && python main.py"`. Each ordinary job starts at the workspace's remote root.

Use `uu -- <command...>` when the remote command starts with a reserved word (`put/get/bind/list`). `uu list` diagnoses the current binding; `uu bind` asks the human to establish or change it.

The human opens Terminal in official UU and makes any device/root/authorization decision. Never choose device IDs, sessions, workers, or silently operate an unapproved Terminal. When uu reports unknown/may-still-be-running, do not replay a command with side effects; report the uncertainty and inspect before any retry.
