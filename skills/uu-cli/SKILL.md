---
name: uu-cli
description: Run commands and transfer files in the current workspace's UU Remote binding from Windows using uu.
---

Run the requested work directly with `uu <command...>`, `uu put <local> <remote>`, or `uu get <remote> <local>`. Treat `uu` like a local command. Assume the workspace is prepared; do not perform routine connection, binding, or status checks before work.

The user handles binding and Terminal preparation. Do not run `uu bind` or `uu list`, manage sessions, inspect internal configuration, or turn normal work into a setup/troubleshooting workflow. Only when an actual command is blocked, briefly report the concrete error once and request the minimum human intervention needed. Continue independent work where possible. Do not repeatedly ask the user to check status, rebind, or open Terminals. Do not describe this workflow as a requirement imposed by official UU.

Different calls do not share shell-local state (`cd/export/source/alias/virtualenv`). Each job starts at the workspace's remote root. Keep dependent steps in one call, for example `uu "cd src && source .venv/bin/activate && python main.py"`.

Output streams to the caller. A long task holds one worker; `uu` handles pool routing and queuing internally. If queued, wait or poll the same local process. Do not kill it, resubmit it, or cancel another task to obtain capacity.

If the result is unknown/may-still-be-running, report the uncertainty and do not replay side-effecting work. Use `uu -- <command...>` for a remote command starting with a reserved word (`put/get/bind/list`).
