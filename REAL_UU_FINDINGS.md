# Real UU command-channel experiment

## Environment

- Date: 2026-10-20 (Asia/Singapore).
- Official Windows UU / CLI: 4.43.0.3607, app V4.43.0.
- Remote: user-provided Linux test host, `/bin/bash`; manual baseline returned
  `probe-ready` and `/home/abot`.
- Dedicated session: `bash-3`; created 2026-10-20T12:18:31Z.
- Probe: standalone `experiments/real-uu-probe`, initial source `8cb5584`,
  visible-token observer `9cdf603`;
  portable-pty 0.9, local ConPTY 24x120, official CLI `term ... attach`.
- Build/format/clippy: [Actions 39421096788](https://github.com/teddyli18000/uu-cli/actions/runs/39421096788).
- No product CLI, worker pool, file transfer or installer involved. No host tokens,
  official process command lines, credentials, binary/configuration modifications,
  terminal new/kill/rename operations, reconnect or command replay.
- Raw logs/screenshots remain local under the probe's gitignored `logs` directory.
  RX means bytes read from the local ConPTY master, not proven remote-original bytes.

## Observed — round 1, original official window still attached

- Preflight: selected instance existed and was `running`.
- Startup RX: inherited-cursor query `ESC[6n`; probe replied `ESC[1;1R` once.
- Other RX: VT private modes, SGR reset, OSC window title, cursor visibility and
  official Chinese connection progress; visible diagnostic lines used CRLF.
- Official CLI reported `Terminal session is already attached (code 2001)`.
  It exited with local process status 102. Its preceding claim about a detached
  session does not override this explicit failure.
- The user observed the original official window remaining normal at its prompt.
- Zero remote command wrappers and zero ETX interrupts were sent. Only the local
  cursor-query reply was written. No fallback, retry or alternate session used.
- Completion/exit-code framing, command echo, continuous execution, Unicode
  command output, real-time command output, Ctrl+C and post-interrupt reuse: untested.
- Operator ended the probe after observing process exit; no session kill/rename
  or remote `exit` command was sent.

## Observed — round 2, user detached the dedicated official tab

- User closed only the selected official tab, without `exit`. Query returned the
  same session name and createdAt, now `detached`.
- One attach succeeded without host metadata/token access. RX included official
  connection success and the remote bash prompt. User reported no abnormal change
  to other UU windows.
- Sent exactly one candidate-wrapped `pwd`, once. At about 76 ms after the local
  write, RX contained the ordered nonce BEGIN, `/home/abot`, END status `0`, and
  a new bash prompt. Raw payload between the visible tokens ended with
  `ESC[19;1H`, not an ordinary newline.
- Exact observed fragment (ESC written symbolically):
  `ESC[17;1HUUC<nonce>B/home/abotESC[19;1HUUC<nonce>E0Z`.
  Neither nonce token was immediately preceded by the expected literal CRLF.
- The original candidate completion matcher therefore did not report completion.
  No second command, interrupt, retry or product transport repair was attempted.
- Probe ended normally; its own attach child was no longer running. Query then
  showed the same session instance `detached`, not killed or recreated.
- The baseline screenshot was partly covered by another window; it cannot prove
  the entire official window state. Window-impact statements above are user reports.

## Observed — round 3, sequential commands and cancellation

- Same detached instance, one successful attachment. Observer changed only to
  locate complete ordered nonce tokens; TX wrapper and ConPTY settings unchanged.
- Startup replayed earlier command text/output. Fresh per-command nonces prevented
  that history from completing a new command. The old END was not visible in the
  replay; its disappearance is observed, its cause is not established.
- [Observer build 39422344118](https://github.com/teddyli18000/uu-cli/actions/runs/39422344118)
  passed Windows format, clippy and release build.

| Command | Observed result | Time to visible END |
| --- | --- | --- |
| `pwd` | `/home/abot`, exit 0 | 72 ms |
| `printf 'hello-uu\n'` | `hello-uu`, exit 0 | 168 ms |
| `sh -c 'exit 7'` | exit 7 | 75 ms |
| `printf 'second-command\n'` | `second-command`, exit 0 | 90 ms |
| `printf '你好-UU\n'` | UTF-8 characters preserved, exit 0 | 154 ms |
| `printf 'tick-1\n'; sleep 2; printf 'tick-2\n'` | Two separate RX bursts, exit 0 | 2147 ms |

- Streaming: first tick arrived about 155 ms after dispatch; second tick about
  1.96 seconds later, before END. Real-time delivery is demonstrated for this case.
- Original literal-CRLF framing failed compatibility on every completed command.
  Normal lines in this round used CRLF; separator rendering also introduced extra
  LF. These presentation bytes are not proof of original stdout byte fidelity.
- `sleep 30` began; one ETX byte (`03`) was written locally 7.59 seconds later.
  No immediate RX or completion followed. A bash prompt returned about 30.07 seconds
  after dispatch, without this command's END/status. Actual cancellation is not
  demonstrated; command outcome remained unknown to the probe.
- No post-interrupt command was sent into that uncertain state. Post-interrupt
  reuse is untested. Probe disconnected normally without remote kill/exit/rename;
  its local attach process was confirmed gone. This instance is not reused for
  the next experiment.
- Read-only `GetConsoleMode` on this probe's own attach console returned input
  `0x1a8`, output `0xf`. No mode was changed. Startup requested Win32 input mode
  (`ESC[?9001h`). Whether explicit key-event encoding resolves cancellation needs
  a separate experiment; the mode readback alone does not establish a cause.

## Manual cancellation baseline before round 4

- User opened a fresh official Terminal and ran `sleep 30`, then physically pressed
  Ctrl+C after about two seconds. User reported no response and no prompt return.
- User subsequently confirmed `sleep 30` finished naturally and the prompt returned,
  then closed that test tab. Query identified a new detached `bash-4` instance.
- Thus the failure is also reported through the official UI; it cannot currently
  be attributed solely to the probe's ConPTY ETX representation.

## Observed — round 4, Win32 Ctrl+C key events

- Fresh `bash-4` after the confirmed manual baseline; one successful attach.
  [Build 39423730079](https://github.com/teddyli18000/uu-cli/actions/runs/39423730079)
  (`ef50264`) passed Windows format, clippy and release build.
- Candidate wrapper and ConPTY settings unchanged. `sleep 30` dispatched at
  12.562 s; BEGIN observed at 12.662 s.
- At 17.681 s, wrote the requested Win32 input protocol's four-event sequence:
  Ctrl down; C down/up with ETX and Ctrl modifier; Ctrl up. Every byte is in local
  raw logs. No GUI key injection or console-mode change was used.
- No immediate RX. At 42.654 s CRLF returned, then prompt at 42.664 s. No END or
  exit code was observed. Thus the sleep again lasted about 30 seconds; neither
  input representation has demonstrated cancellation.
- No subsequent command was sent. Probe disconnected normally, retaining pending
  state as unknown. Its local attach child was confirmed gone.
- Next diagnostic is read-only remote shell flags, `monitor`, `stty` and shell
  PID/process-group/foreground-group/session metadata. No remote settings changed.

## Remote shell diagnostic (user-run, official UI)

- `shell-flags=himBHs`, `monitor on`.
- `stty`: `intr = ^C`, `isig icanon iexten echo echoctl`; no disabled interrupt
  character or disabled signal generation observed. No `stty` changes made.
- Shell PID = PGID = SID, state `Ss`; foreground PGID differed while the `ps`
  diagnostic itself was the foreground command. This snapshot alone does not show
  which process group received the earlier interrupts.
- Signal delivery to the actual foreground job remains unverified. The remote
  host implementation/version has not yet been identified in this experiment.

## Observed — round 5, dedicated worker baseline and per-job guard

- User surrendered the diagnostic session `bash-5`; exact instance checked before
  one successful attach. [Build 39424881018](https://github.com/teddyli18000/uu-cli/actions/runs/39424881018)
  (`86900bd`) passed Windows format, clippy and release build.
- Explicit experiment: `set +m` in the dedicated worker, a guard subshell with
  INT/EXIT traps, and a further inner command subshell. Framing adds trailing
  newlines to BEGIN/END. No `stty`, system or UU configuration changes.
- Same attachment, ordered fresh nonces:

| Step | Result |
| --- | --- |
| `pwd` | expected path, END status 0 after 126 ms |
| `sh -c 'exit 7'` | END status 7 after 80 ms |
| `sleep 30` + ETX | ETX sent 5.97 s after dispatch; END status 130 arrived 87 ms later; prompt followed |
| `printf 'after-interrupt\n'` | expected output, END status 0 after 129 ms |

- Cancellation and subsequent reuse are demonstrated for this ordinary foreground
  sleep. The result does not promise termination of commands that ignore SIGINT,
  deliberately leave their process group or daemonize.
- This supports the shared-process-group worker baseline. It does not prove the
  remote host's exact signal implementation or isolate each guard change's effect.
- All four results still failed the old literal-CRLF matcher. The observer was
  reading complete visible tokens; product decoder/output integration remains open.
- Probe disconnected normally with no pending command, without killing the remote
  Terminal. Its dedicated shell retains the deliberately changed monitor baseline.

## Observed — round 6, pinned product wrapper and decoder

- [Build 39427353688](https://github.com/teddyli18000/uu-cli/actions/runs/39427353688)
  (`64e856d`) compiled the actual `src/job.rs` from production commit
  `79fa6fbc0c8d3dbb4ebf50c2589e684bd5d85d37` inside Actions. The probe used
  `wrap_job` and `CompletionDecoder::new_console`; its visible-token observer was
  disabled for these jobs. Same authorized `bash-5`, exact-instance preflight.
- All jobs used the explicit test root `/`, without product binding or scheduling:

| Step | Product decoder result |
| --- | --- |
| `pwd` | `/`, exit 0, 143 ms |
| `sh -c 'exit 7'` | exit 7, 112 ms |
| Unicode printf | `你好-UU`, exit 0, 129 ms |
| two ticks separated by sleep 2 | first payload streamed in 127 ms; second about 2 s later; exit 0 |
| `sleep 30` + ETX | interrupted after about 5 s; exit 130 decoded 95 ms after ETX |
| post-interrupt printf | `after-interrupt`, exit 0, 128 ms |

- Returned payload remained ConPTY presentation bytes, including CRLF. No arbitrary
  VT stripping or original-byte fidelity claim. This round did not exercise the
  complete product runner, binding, pool or installer.
- Probe disconnected normally with no pending command. Its own local attach
  child was confirmed gone; no remote exit/kill/rename was sent.

## Conclusion

- Attach handoff is demonstrated in this environment: a live official attachment
  rejects a second client; user closing that dedicated tab leaves the instance
  detached and attachable. It is not simultaneous sharing of an open tab.
- Real ConPTY replaces protocol CRLF separators with cursor-addressing VT, so
  the original literal-CRLF framing is invalid here. Round 6 demonstrates the
  product's nonce-token decoder on the observed command cases.
- Sequential commands, status 7, Unicode characters and delayed output have been
  observed in one attachment. The visible-token observer is instrumentation, not
  a product output-normalization fix or proof of arbitrary binary/text fidelity.
- Original manual/ETX/Win32 cancellation paths failed with interactive job control
  enabled. With the dedicated worker baseline and per-job guard, ETX cancellation,
  status 130 and next-command reuse passed for `sleep 30` in round 5.
- Command-channel approach is viable for the observed cases. The production
  wrapper and console decoder passed this isolated probe; full runner integration,
  disconnect handling and safe remote-root capture still need acceptance. This
  does not constitute full product acceptance or arbitrary output byte fidelity.
- Official attach accepts a session name, not an atomic instance identifier;
  name + createdAt preflight detects replacements before attach but cannot prove
  an atomic guarantee across the query/attach interval.
- Full product acceptance remains open.
