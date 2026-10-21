---
name: uu-cli
description: Run commands and transfer files in the current workspace's UU Remote binding from Windows using uu.
---

Use `uu <command...>` for remote execution, `uu put <local> <remote>` to upload, and `uu get <remote> <local>` to download.

Different `uu` calls do not share shell-local state such as `cd/export/source/alias/virtualenv`. Put dependent operations in one call, for example `uu "cd src && source .venv/bin/activate && python main.py"`. Each ordinary job starts at the workspace's remote root.

`uu bind` and `uu list` are human-only commands. **Never run either command on the user's behalf.** If this workspace is unbound or has no authorized Terminal, stop and tell the human to run `uu bind` in the workspace. If status needs inspection, tell the human to run `uu list`.

The human opens a dedicated Terminal in official UU, closes that Terminal's official tab when needed to release its connection, changes to the exact directory that should be this workspace root, and runs `uu bind`. Usually there is exactly one newly available live detached Terminal, so the explicit `uu bind` call hands it to uu-cli without a device/session picker, popup, or second confirmation. If more than one candidate exists, bind reports ambiguity and exits; the human resolves it in official UU and runs bind again. Repeating `uu bind` adds one Terminal to the same device pool and preserves the existing remote root unless the human intentionally changes it. Discovery alone never authorizes a Terminal, and ordinary commands never auto-bind, authorize, or wait for a new Terminal. Long-running commands stream output to their original caller while holding one worker. If all authorized workers are busy, `uu` remains pending and reports the number of waiting commands ahead on stderr. Wait or poll the same local process; do not kill it or submit the command again merely because it is queued, and never cancel another job.

Never choose device IDs, sessions, workers, or silently operate an unapproved Terminal. When uu reports unknown/may-still-be-running, do not replay a command with side effects; report the uncertainty and inspect before any retry.

Use `uu -- <command...>` when the remote command starts with a reserved word (`put/get/bind/list`).
