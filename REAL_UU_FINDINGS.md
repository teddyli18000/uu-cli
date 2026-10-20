# Real UU command-channel experiment

## Environment

- Date: 2026-10-20 (Asia/Singapore).
- Official Windows UU / CLI: 4.43.0.3607, app V4.43.0.
- Remote: user-provided Linux test host, `/bin/bash`; manual baseline returned
  `probe-ready` and `/home/abot`.
- Dedicated session: `bash-3`; created 2026-10-20T12:18:31Z.
- Probe: standalone `experiments/real-uu-probe`, source commit `8cb5584`;
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

## Conclusion

- Attach handoff is demonstrated in this environment: a live official attachment
  rejects a second client; user closing that dedicated tab leaves the instance
  detached and attachable. It is not simultaneous sharing of an open tab.
- Current transport as a reliable command channel: not yet established. Real
  ConPTY replaces protocol CRLF separators with cursor-addressing VT, so the
  candidate's literal-CRLF framing is invalid here. This is a real official-CLI
  observation, independent of the earlier cooked-console fake failures.
- Next bounded observation: retain the same TX wrapper and ConPTY settings; teach
  only the experimental observer to locate ordered complete visible nonce tokens
  while continuing to report the original decoder failure and retain all bytes.
  Then measure sequential commands, exit 7, Unicode, delayed output and Ctrl+C in
  one attachment. This is instrumentation, not a product output-normalization fix.
- Official attach accepts a session name, not an atomic instance identifier;
  name + createdAt preflight detects replacements before attach but cannot prove
  an atomic guarantee across the query/attach interval.
- Full product acceptance remains open.
