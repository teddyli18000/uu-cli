# Real UU command-channel findings

## Environment

- 2026-10-20; interactive Windows, official UU 4.43.0.3607.
- User-authorized dedicated Linux /bin/bash Terminal; remote host build unknown.
- Raw logs remain local and ignored; no account credentials collected.
- Isolated probe built in Actions; production module revision 79fa6fbc0c8d3dbb4ebf50c2589e684bd5d85d37.

## Observed

- Attach: an already-open official attachment rejects another client. Closing only the dedicated tab leaves the same session instance detached and attachable.
- Official-window impact: user reported no abnormal switching, closure or disconnection during the detached attach experiment.
- RX before first command: official banners, prompt and retained terminal history; fresh per-command nonces exclude stale results.
- Echo: wrapper input is echoed; full BEGIN/END tokens are absent from the transmitted wrapper text.
- Line ending / VT: ConPTY returns presentation bytes. CRLF can become cursor addressing; literal-CRLF framing failed.
- Exit framing: product console decoder recognizes complete nonce tokens independently of rendered line separators, preserving remaining presentation bytes.
- Continuous commands: product wrapper/decoder observed pwd exit 0, sh -c 'exit 7' exit 7, Unicode text and subsequent commands in one attach.
- Real-time output: first tick arrived about 127 ms after dispatch, second about two seconds later.
- Ctrl+C: original interactive job-control baseline failed, including the user's manual official-UI attempt. With set +m in the dedicated worker and per-job INT/EXIT guard, ETX interrupted ordinary foreground sleep and yielded exit 130 about 95 ms later.
- Reuse: next command after interruption returned expected text and exit 0.
- Cleanup: probe disconnected normally with no pending job; its local child exited. No remote kill, exit or rename was sent.

## Conclusion

The observed command-channel approach is viable. The actual production wrapper and console decoder passed the isolated real-UU probe ([Actions build](https://github.com/teddyli18000/uu-cli/actions/runs/39427353688)).

This is not full product acceptance. Remaining checks include the complete runner, safe remote-root capture, disconnect handling, authorization/binding interaction, multiple workers and all other TESTING.md release gates. Presentation output is not guaranteed to equal the original remote byte stream. Cancellation of SIGINT-ignoring or detached jobs is not demonstrated.

The official attach API accepts a name rather than an atomic instance identifier. Preflight name + createdAt detects replacement before attach, but does not prove atomicity across query and attach.