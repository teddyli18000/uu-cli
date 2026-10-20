use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;

const OSC_PREFIX: &[u8] = b"\x1b]7770;UUCLI;";
const BEL: u8 = 0x07;

/// Convert CLI argv into the remote shell text promised by the public contract.
///
/// One argument is intentionally treated as an explicit shell expression so
/// `uu "cd src && pytest"` works. With multiple arguments we quote every argv
/// element, preserving argv semantics rather than accidentally interpreting
/// `|`, `>`, `$`, `*`, etc. as remote shell syntax.
pub fn argv_to_shell(argv: &[OsString]) -> Result<String> {
    if argv.is_empty() {
        bail!("remote command is empty");
    }

    let args = argv
        .iter()
        .map(|arg| {
            arg.to_str().map(str::to_owned).ok_or_else(|| {
                anyhow!("remote command contains text that cannot be represented as UTF-8")
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if args.len() == 1 {
        return Ok(args[0].clone());
    }

    Ok(args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" "))
}

/// Build one line injected into an already-open persistent shell.
///
/// A private start marker is emitted before the user command so the client can
/// discard the PTY's input echo and current prompt. The command then executes in
/// a subshell, keeping `cd`, `export`, `source`, `exit`, and `exec` from becoming
/// hidden prerequisites of a later uu job. The end marker carries the exit code.
pub fn wrap_job(remote_root: &str, shell_command: &str, nonce: &str) -> Result<String> {
    validate_nonce(nonce)?;
    if remote_root.is_empty() {
        bail!("remote root is empty");
    }

    let root = shell_quote(remote_root);
    let command = shell_quote(shell_command);

    Ok(format!(
        "printf '\\033]7770;UUCLI;{nonce};BEGIN\\007'; ( cd -- {root} && eval {command} ); __uu_status=$?; printf '\\033]7770;UUCLI;{nonce};END;%d\\007' \"$__uu_status\"\n"
    ))
}

pub fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// Streaming decoder for one framed remote job.
///
/// Bytes before the start marker are shell UI/input echo and are discarded.
/// Bytes after the start marker are user output and can be streamed immediately.
/// Bytes after the end marker are the next prompt and are discarded. Markers may
/// be split across arbitrary PTY frames.
#[derive(Debug)]
pub struct CompletionDecoder {
    start_marker: Vec<u8>,
    end_prefix: Vec<u8>,
    pending: Vec<u8>,
    started: bool,
    completed: Option<i32>,
}

impl CompletionDecoder {
    pub fn new(nonce: &str) -> Result<Self> {
        validate_nonce(nonce)?;

        let mut start_marker = OSC_PREFIX.to_vec();
        start_marker.extend_from_slice(nonce.as_bytes());
        start_marker.extend_from_slice(b";BEGIN");
        start_marker.push(BEL);

        let mut end_prefix = OSC_PREFIX.to_vec();
        end_prefix.extend_from_slice(nonce.as_bytes());
        end_prefix.extend_from_slice(b";END;");

        Ok(Self {
            start_marker,
            end_prefix,
            pending: Vec::new(),
            started: false,
            completed: None,
        })
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn completed(&self) -> Option<i32> {
        self.completed
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>> {
        if self.completed.is_some() {
            return Ok(Vec::new());
        }

        self.pending.extend_from_slice(chunk);
        if !self.started {
            self.seek_start_marker();
            if !self.started {
                return Ok(Vec::new());
            }
        }

        self.decode_started_output()
    }

    /// Flush buffered user output only when the transport ended after the start
    /// marker but before completion. The caller must report the job as unknown
    /// and must never replay it automatically.
    pub fn finish_without_marker(&mut self) -> Vec<u8> {
        if self.started {
            std::mem::take(&mut self.pending)
        } else {
            self.pending.clear();
            Vec::new()
        }
    }

    fn seek_start_marker(&mut self) {
        if let Some(start) = find_subslice(&self.pending, &self.start_marker) {
            let after = start + self.start_marker.len();
            self.pending.drain(..after);
            self.started = true;
            return;
        }

        let keep = self.start_marker.len().saturating_sub(1);
        if self.pending.len() > keep {
            let discard = self.pending.len() - keep;
            self.pending.drain(..discard);
        }
    }

    fn decode_started_output(&mut self) -> Result<Vec<u8>> {
        if let Some(start) = find_subslice(&self.pending, &self.end_prefix) {
            let status_start = start + self.end_prefix.len();
            if let Some(relative_end) = self.pending[status_start..]
                .iter()
                .position(|byte| *byte == BEL)
            {
                let status_end = status_start + relative_end;
                let status_text = std::str::from_utf8(&self.pending[status_start..status_end])
                    .context("completion marker contains a non-UTF-8 exit status")?;
                let status = status_text.parse::<i32>().map_err(|_| {
                    anyhow!("invalid exit status in completion marker: {status_text}")
                })?;
                let output = self.pending[..start].to_vec();
                self.pending.clear();
                self.completed = Some(status);
                return Ok(output);
            }

            let output = self.pending[..start].to_vec();
            self.pending.drain(..start);
            return Ok(output);
        }

        let keep = self.end_prefix.len().saturating_sub(1);
        if self.pending.len() <= keep {
            return Ok(Vec::new());
        }
        let emit_len = self.pending.len() - keep;
        Ok(self.pending.drain(..emit_len).collect())
    }
}

fn validate_nonce(nonce: &str) -> Result<()> {
    if nonce.is_empty() || !nonce.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        bail!("job nonce must be non-empty ASCII alphanumeric text");
    }
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(xs: &[&str]) -> Vec<OsString> {
        xs.iter().map(OsString::from).collect()
    }

    #[test]
    fn one_argument_is_explicit_shell_text() {
        assert_eq!(
            argv_to_shell(&os(&["cd src && pytest -q"])).unwrap(),
            "cd src && pytest -q"
        );
    }

    #[test]
    fn multiple_arguments_preserve_argv_semantics() {
        assert_eq!(
            argv_to_shell(&os(&["printf", "%s\\n", "a b", "$HOME", "*.txt"])).unwrap(),
            "'printf' '%s\\n' 'a b' '$HOME' '*.txt'"
        );
    }

    #[test]
    fn quote_handles_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn wrapper_isolates_state_and_has_two_markers() {
        let wrapped = wrap_job(
            "/home/user/my project",
            "cd src && export X=1 && exit 7",
            "AbC123",
        )
        .unwrap();
        assert!(wrapped.contains("UUCLI;AbC123;BEGIN"));
        assert!(wrapped.contains("( cd -- '/home/user/my project' && eval "));
        assert!(wrapped.contains("'cd src && export X=1 && exit 7'"));
        assert!(wrapped.contains("UUCLI;AbC123;END;%d"));
    }

    #[test]
    fn decoder_drops_echo_and_handles_markers_split_across_frames() {
        let mut decoder = CompletionDecoder::new("abc123").unwrap();
        let start = b"\x1b]7770;UUCLI;abc123;BEGIN\x07";
        let end = b"\x1b]7770;UUCLI;abc123;END;17\x07prompt$ ";
        let mut received = Vec::new();

        received.extend(decoder.push(b"prompt$ printf '\\033]7770;UUCLI;").unwrap());
        received.extend(decoder.push(&start[..8]).unwrap());
        received.extend(decoder.push(&start[8..]).unwrap());
        received.extend(decoder.push(b"hello ").unwrap());
        received.extend(decoder.push(&end[..9]).unwrap());
        received.extend(decoder.push(&end[9..]).unwrap());

        assert_eq!(received, b"hello ");
        assert!(decoder.started());
        assert_eq!(decoder.completed(), Some(17));
    }

    #[test]
    fn decoder_ignores_echoed_backslash_markers_before_start() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        let echo = b"printf '\\033]7770;UUCLI;nonce;BEGIN\\007'\r\n";
        assert!(decoder.push(echo).unwrap().is_empty());
        assert!(!decoder.started());

        let mut output = decoder
            .push(b"\x1b]7770;UUCLI;nonce;BEGIN\x07real output\r\n")
            .unwrap();
        output.extend(
            decoder
                .push(b"\x1b]7770;UUCLI;nonce;END;0\x07prompt$ ")
                .unwrap(),
        );
        assert_eq!(output, b"real output\r\n");
        assert_eq!(decoder.completed(), Some(0));
    }

    #[test]
    fn eof_before_start_does_not_leak_shell_ui() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        assert!(decoder.push(b"prompt$ echoed wrapper").unwrap().is_empty());
        assert!(decoder.finish_without_marker().is_empty());
    }

    #[test]
    fn eof_after_start_flushes_only_user_output() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        decoder
            .push(b"noise\x1b]7770;UUCLI;nonce;BEGIN\x07")
            .unwrap();
        assert!(decoder.push(b"partial output").unwrap().is_empty());
        assert_eq!(decoder.finish_without_marker(), b"partial output");
    }

    #[test]
    fn invalid_status_is_protocol_error() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        decoder.push(b"\x1b]7770;UUCLI;nonce;BEGIN\x07").unwrap();
        let err = decoder
            .push(b"out\x1b]7770;UUCLI;nonce;END;nope\x07")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid exit status"));
    }
}
