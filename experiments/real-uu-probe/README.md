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
`interrupt`, `after`, `status`, `quit`. They send fixed benign shell commands.
Each command is individually wrapped with the current candidate ASCII completion
markers and runs in a subshell. No automated sequence, retry, reconnect or replay.
The tool reports candidate marker detection, not a verdict about stream integrity.
It refuses another shell command until the previous marker is observed. `interrupt`
sends exactly one ETX byte (03); it does not synthesize success or free a busy job.

`human.log` contains milestones; `raw.jsonl` contains timestamped TX/RX hex and
process-state events before any interpretation. Cursor inheritance DSR is logged
and answered only before a shell command is sent. Raw RX is never stripped or
normalized. Startup output/process liveness does not alone establish attach success.

`quit` drops only this probe's local ConPTY attachment. It never invokes terminal
kill/rename or sends remote `exit`. The effect of local disconnect on the selected
official terminal is an observation, not assumed safe. The tool never calls
`child.kill()`. Log directories contain real host information and stay gitignored;
do not publish them. Put only sanitized conclusions in REAL_UU_FINDINGS.md.
