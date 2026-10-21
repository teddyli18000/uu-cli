# Real UU command-channel findings

## Environment

- 2026-10-20; interactive Windows, official UU 4.43.0.3607.
- User-authorized dedicated Linux /bin/bash Terminal; remote host build unknown.
- Raw logs remain local and ignored; no account credentials collected.
- Isolated probe built in Actions; production job/runner/transport revision 3c2e44725b8f80ac09c593482ebcc964190c27c6.

## Observed

- Attach: an already-open official attachment rejects another client. Closing only the dedicated tab leaves the same session instance detached and attachable.
- Official-window impact: user reported no abnormal switching, closure or disconnection during the detached attach experiment.
- RX before first command: official banners, prompt and retained terminal history; fresh per-command nonces exclude stale results.
- Echo: wrapper input is echoed; full BEGIN/END tokens are absent from the transmitted wrapper text.
- Line ending / VT: ConPTY returns presentation bytes. CRLF can become cursor addressing; literal-CRLF framing failed.
- Exit framing: product console decoder recognizes complete nonce tokens independently of rendered line separators, preserving remaining presentation bytes.
- Continuous commands: product wrapper/decoder observed pwd exit 0, sh -c 'exit 7' exit 7, Unicode text and subsequent commands in one attach.
- Full product transport/runner: immediately after attach, validated PWD1 records decoded the expected absolute directory about 400 ms after probe start; no artificial readiness delay. Six fixed jobs then completed in the same attach.
- Real-time output: actual runner delivered the first tick about 95 ms after dispatch, second about two seconds later.
- Ctrl+C: original interactive job-control baseline failed, including the user's manual official-UI attempt. With set +m in the dedicated worker and per-job INT/EXIT guard, ETX interrupted ordinary foreground sleep and yielded exit 130 about 95 ms later.
- Product cancellation: an Interrupt queued after two seconds made sleep 30 return status 130 at 2.064 seconds after dispatch. The next command returned expected text and exit 0 in about 107 ms.
- Other Terminal: a separate running manual session was never attached; its queried identity/state remained unchanged. This is management-query evidence, not another visual UI observation.
- Cleanup: probe disconnected normally with no pending job; its local child exited. No remote kill, exit or rename was sent.

## Conclusion

The observed command-channel approach is viable. The actual production wrapper, console decoder, runner, transport and validated remote-directory capture passed the isolated real-UU probe ([Actions build](https://github.com/teddyli18000/uu-cli/actions/runs/39431629252)). Earlier raw observations are retained separately; this final mode records actual API output and milestones because the transport does not expose private RX/TX.

This is not full product acceptance. Remaining checks include product CLI/binding/authorization interaction, real disconnect/recovery, multiple workers, real file transfer and all other TESTING.md release gates. Presentation output is not guaranteed to equal the original remote byte stream. Cancellation of SIGINT-ignoring or detached jobs is not demonstrated.

The official attach API accepts a name rather than an atomic instance identifier. Preflight name + createdAt detects replacement before attach, but does not prove atomicity across query and attach.
## 2026-10-21 public CLI / disposable WSL follow-up

- Environment: the same official Windows UU 4.43.0.3607, a newly imported disposable Ubuntu 18.04.6 WSL2 host (glibc 2.27, Bash 4.4.20), and released uu-remote-linux v0.2.12-aic.3. This is a real official-client integration test, not vehicle or clean-Windows acceptance.
- Candidate: Actions package from revision 92e7e114, followed by revision 4ffbb8fd with explicitly enabled local RX diagnostics. No product installation, PATH change, or account-state export was used.
- First binding: the public `uu pwd` path opened its genuine separate authorization and remote-root consoles; accepting the dedicated Terminal and its captured home directory saved one workspace binding. No configuration was seeded to bypass these choices.
- Observed successes through public `uu`: expected pwd and status 0; ordinary and Unicode text; a no-output command with status 7; and a subsequent command with status 0 on the same remote Terminal instance.
- Observed failures: some reattachments delivered official connection banners but no remote terminal RX, while the same PTY view showed the new command's BEGIN, END/status and returned prompt. Those calls remained in-flight locally; owned local clients were stopped and their worker markers retained as unknown, without replay or forced idle.
- Official connection behavior: rapid public invocations produced official error 105 (too many operations). An open official Terminal-list window also coincided with environment-check timeouts; closing only that test list window allowed the next query to succeed. These observations do not yet isolate the cause.
- Public cancellation remains unaccepted: in the captured failed-reattach case, sleep completed remotely before cancellation could be attempted; local Ctrl+C while BEGIN was missing did not finish the caller within five seconds. The earlier single-attach probe's successful cancellation is separate evidence.
- The Windows synthetic binding test independently traced LF caller input through the public stdin reader and runner, but the fake console received the text without LF. Production ConPTY newline adaptation is being corrected; changing the test input to CRLF would hide this issue.
- Next gate: rebuild the bounded input/output fixes in Actions, then rerun the public sequence and live cancellation. The intermittent output stall and connection rate limit must be resolved or explicitly bounded before claiming normal public CLI use or v1 completion.

### Public cancellation and repeated-attach observation

- Actions candidate `da6b331` continuously drains ConPTY output independently of the event consumer, with an 8 MiB bounded backlog. Its Windows/Linux tests and formatting passed; its only Clippy error was fixed by `6e4d641` without changing runtime behavior.
- A fresh dedicated Terminal completed public `uu pwd` with status 0. Its next public invocation ended with `unknown`: the official attach process exited before BEGIN/completion. The remote view retained only the first command, and the host recorded management LIST requests without an OPEN for the failed call. This differs from the earlier remote-completed/local-RX-stalled observation.
- Another dedicated Terminal completed a real public cancellation: after `cancel-ready` became visible, the harness waited two seconds, then generated Ctrl+C on only its owned public CLI console. The CLI returned status 130 in 88.3 ms; the remote view showed END130 and the shell prompt. This supersedes the earlier statement that public cancellation was unaccepted for ordinary foreground sleep.
- Its next public `after-cancel` invocation again ended with unknown before remote OPEN. Both unknown worker markers remain quarantined; neither command was replayed or reassigned. Post-interrupt reuse through a separate public invocation therefore remains unaccepted.
- The host recorded DETACH immediately after each successful public command, including the cancelled sleep. Missing DETACH is not supported as the explanation for these two failures.
- Temporary diagnostics are needed on the actual failed public path to retain the official pre-BEGIN error. A separate read-only reattach probe succeeded later and recovered the existing history, without sending a command; this does not prove the failed public invocation's cause.

### Captured reconnect failure and scoped correction

- The explicitly enabled failure diagnostic in Actions candidate `e561394` captured official connection banners followed by `Streamer error: 9013`; the child exited with code 105. The remote view had no BEGIN or execution of that public command. A separate invocation had failed during discovery with official error 105 and the explicit message “操作过于频繁，请稍后再试”. These are distinct observed errors; the numeric streamer code alone has not been mapped to a cause.
- The public scheduler was querying remote inventory twice and creating a new official attachment for every call, although the single-attachment probe had already demonstrated continuous jobs. ConPTY's local cursor handshake was incorrectly sufficient to consider the transport prepared, even while official remote connection establishment could still fail. The resulting unknown status was conservative but the reconnect pattern made normal routing unreliable.
- The correction keeps one official attachment per authorized worker in a hidden `uu-background` process, routes public calls through a current-user-only local pipe, and expires it after five idle minutes. Live worker routing avoids another remote inventory/attach cycle. Each job still carries its own remote root and uses the existing framing, isolation, lease and quarantine rules. Unknown jobs are never replayed.
- Implementation and Actions/real-UU acceptance of this correction are recorded below when observed; this design entry alone is not a passing result.

### Persistent route: EOF regression found in real use

- Candidate `878ddce` reused one helper/official attach for public `pwd` and `printf` (ordinary/Unicode output, second call 170 ms). The next call failed unknown and the host recorded that the dedicated Bash exited with status 0. The failed job was not replayed; the worker remains quarantined.
- Caller stdin closure was forwarded as terminal VEOF (`Ctrl+D`). When a short command has already returned, VEOF can reach the parent prompt and exit the worker. A Git Bash pipe fixture exposed the corresponding modeling error: it treated VEOF as a literal prefix of the next shell command.
- The Bash worker now enables `ignoreeof` before each job, alongside its existing job-control bootstrap. Foreground commands still receive terminal EOF. A standalone Linux PTY experiment passed 25 command/late-EOF cycles, foreground `cat` EOF, and another command; the production-wrapper regression runs in Actions. The Windows pipe fixture does not prove foreground EOF semantics.
- This entry records the failure and focused correction, not successful acceptance of the rebuilt public route.

### Public persistent-route acceptance

- Candidate: Actions package `edea7ca` ([package/install run](https://github.com/teddyli18000/uu-cli/actions/runs/39524610449)), used directly from the disposable test directory without installing it. One newly created, explicitly authorized official `bash-5` Terminal was used; prior unknown worker markers were retained.
- The first attempt timed out in official discovery after 20 seconds and explicitly reported that no remote job had been sent. A separate attempt then attached successfully. This first-connection timeout remains an observed official-channel limitation; there was no retry of a dispatched job.
- Six separate public processes completed `pwd`, ordinary/Unicode `printf`, `sh -c 'exit 7'`, `second-command`, cancelled `sleep 30`, and `after-cancel`. Expected statuses were 0, 0, 7, 0, 130, 0. The hidden helper PID and its official attach child PID remained identical before/after the ordinary calls and after cancellation.
- For cancellation the harness waited for actual `cancel-ready` output, waited two seconds, then generated Ctrl+C only on its owned public process console. Status 130 arrived 108.2 ms after the signal. The next public command returned `after-cancel` through the same attachment.
- Actual piped stdin also passed: text piped into public `uu cat` returned status 0 on EOF, followed by a successful `after-stdin-eof` command. PTY echo made the text visible twice (echo plus cat output); this is presentation output, not separated stdout/stderr or an exact byte-stream promise.
- The remote same-PTY view showed completion records, the post-interrupt/post-EOF commands, and a reusable Bash prompt. Idle EOF displayed Bash's `Use "logout" to leave the shell` notice instead of exiting. No other Terminal was attached, renamed, or killed.
- The command-channel minimum loop now passes through the public CLI using one persistent official attachment. This does not complete vehicle, multi-worker, disconnect/crash, clean external Windows, or the remaining v1 acceptance gates in TESTING.md.

## 2026-10-21 public discovery, pool, and transfer acceptance

- Environment: official Windows UU `4.43.0.3607`; user-authorized disposable Ubuntu 18.04 WSL host running uu-remote-linux `v0.2.12-aic.3`. This is real UU/remote integration on the development PC, not external clean-Windows or multi-device acceptance.
- With no foreground `uu` request, the real background watcher found two newly opened detached Terminals. One real authorization console handled the two Enter decisions in sequence.
- Unsolicited manual Terminal: the official connection was retained while an unrelated public command completed. After releasing that test connection, the watcher presented one authorization console; answering N persisted an exact-instance decline. It was absent from both authorization records and worker metadata; another public command succeeded through the existing pool.
- Public pool concurrency: a long `sleep 30` and a short command both returned exit 0. `FastEndedWhileLongRunning=true`; remote shell PIDs were `36426` and `36353`. First output arrived after 12.810 s; the short command completed after 19.284 s including connection setup.
- Cross-workspace isolation: workspaces A and B shared the device pool while using roots `/home/uutest` and `/tmp/uu-cli-root-b`. A changed directory and exported `UU_WORKSPACE_LEAK=present`; B still started in its own root with the variable absent, and later commands in A continued to start at A's root after work in B. Five measured calls took 397, 262, 256, 222 and 241 ms, all exit 0.
- Waiting binding: while setup was waiting, a newly opened official `bash-3` appeared attached. After releasing that Terminal's local official connection as prompted, setup proceeded directly to remote-root confirmation without a second authorization; the real input `/tmp/uu-cli-root-b` was saved. This covers the waiting/new-Terminal flow, but not the separate old-candidate bug where a new workspace bind waits when existing helpers already hold all Terminal connections; that path needs a rebuilt candidate.
- File transfer: public `put/get` round-tripped 16,385 random binary bytes with matching SHA-256. Tested Chinese and space-containing local/remote paths, relative paths, absolute paths, `~`, zero-byte files, and a subsequent `pwd`. The non-empty upload took 22.115 s and download 6.763 s; zero-byte upload/download took 546/372 ms.
- A prior one-shot large-base64 `get` completed remotely but its local caller received no completion record. The current path uses 768-byte chunks with per-chunk acknowledgment and publishes only after full SHA-256 validation; the 16,385-byte round-trip above passed.
- Safety behavior in the tested candidate: a `running` session with unknown/non-true attachability is not attached; the waiting prompt directs the user to close the dedicated Terminal tab to release the official connection and not type `exit`; missing BEGIN for 30 seconds yields unknown without replay; stale helper metadata is cleared before startup.
- Caller crash/no-replay: a long public job first appended one record to a dedicated remote counter file, printed `crash-started`, then slept 20 seconds. After observing the start marker, the harness killed only owned caller PID `285212`. Its exact in-flight marker remained; a following public command ran on another worker and returned `recovery-next` with exit 0. After 21 seconds the remote counter was still exactly 1, so the dispatched command was not replayed. This does not test network failure or worker-helper crash/recovery.
- Watcher idle sample: with no foreground `uu` request, watcher PID `293128` ran for 30.017 seconds, consuming 78.125 ms CPU with 6.25 MiB working set. This measures the watcher process only; spawned official subprocesses were excluded, so it does not establish whole-process-tree idle resource usage.
- Remaining acceptance: external clean Windows from install through login and use, real multi-device selection, UU/Terminal/network interruption and restart/recovery, worker-helper crash recovery, all-attached new-workspace binding on its fixed candidate, full watcher process-tree idle resource use, and large-file throughput. Do not infer v1 completion from this run.
