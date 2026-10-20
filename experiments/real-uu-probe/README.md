# real-uu-probe

Independent observation tool, not the product CLI. Uses the candidate transport's
portable-pty 0.9 Windows ConPTY, 24x120 console and official `term -d ... attach ...`.
No binding, pool, transfer, installer, token access or official configuration changes.

Build: `cargo build --manifest-path experiments/real-uu-probe/Cargo.toml`.

`real-uu-probe list <official-cli>` lists device/session metadata. `attach
<official-cli> <device> <session> <createdAt> <log-directory>` rechecks that exact
session instance before attaching. Selection and authorization belong to the human
operating this experiment. Never select an unrelated terminal.

The local input commands are `pwd`, `hello`, `exit7`, `second`, `unicode`, `sleep`,
`stream`, `interrupt`, `interrupt-key`, `after`, `status`, `quit`. They send fixed benign shell commands.
Each command is individually wrapped with the current candidate ASCII completion
markers and runs in a subshell. No automated sequence, retry, reconnect or replay.
The observer reports ordered visible nonce tokens, not a verdict about stream
integrity or a production decoder fix. It separately records whether literal CRLF
separators required by the original candidate decoder survived. It refuses another
shell command until the previous complete nonce end token/status is observed. `interrupt`
sends exactly one ETX byte (03); it does not synthesize success or free a busy job.
`interrupt-key` instead sends the Win32 input protocol's left-Ctrl down, C down/up
(Unicode ETX, Ctrl modifier), and Ctrl up events into this probe's own ConPTY input.
It requires the startup `ESC[?9001h` request and a pending command. No console mode
or official process configuration is changed. Neither form asserts cancellation.
Protocol reference: [Microsoft Terminal input encoder](https://github.com/microsoft/terminal/blob/main/src/terminal/input/terminalInput.cpp).

`guard-<command>` selects a separate job-guard experiment with the same fixed
command whitelist. It runs `set +m` in the explicitly surrendered dedicated shell
as a worker baseline, then uses an isolated guard subshell with INT/EXIT traps and
an inner command subshell. END and BEGIN have trailing newlines. This intentionally
changes job-control mode only in that dedicated shell; it is not a default product
repair or a change to remote system/UU settings. Cancellation still needs a matching
END and observed prompt/reuse; no pending state is cleared by the interrupt itself.

`human.log` contains milestones; `raw.jsonl` contains timestamped TX/RX hex and
process-state events before any interpretation. Cursor inheritance DSR is logged
and answered only before a shell command is sent. Raw RX is never stripped or
normalized. Startup output/process liveness does not alone establish attach success.

`quit` drops only this probe's local ConPTY attachment. It never invokes terminal
kill/rename or sends remote `exit`. The effect of local disconnect on the selected
official terminal is an observation, not assumed safe. The tool never calls
`child.kill()`. Log directories contain real host information and stay gitignored;
do not publish them. Put only sanitized conclusions in REAL_UU_FINDINGS.md.
