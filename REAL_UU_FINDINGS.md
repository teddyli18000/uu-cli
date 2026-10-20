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