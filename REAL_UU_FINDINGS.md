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