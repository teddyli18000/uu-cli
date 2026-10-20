use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;

const END_MARKER: u8 = b'Z';

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
/// The dedicated worker permanently disables job control so Ctrl+C reaches the
/// shell and its foreground job together. A guard subshell owns completion and
/// interrupt traps; the job runs in a nested subshell so its shell state, traps,
/// exit, and exec cannot replace the guard.
pub fn wrap_job(remote_root: &str, shell_command: &str, nonce: &str) -> Result<String> {
    validate_nonce(nonce)?;
    if remote_root.is_empty() {
        bail!("remote root is empty");
    }

    let root = shell_quote(remote_root);
    let command = shell_quote(shell_command);

    Ok(format!(
        "set +m; ( trap '__uu_status=$?; printf \"\\r\\n%s%s%s%d%s\\r\\n\" UUC {nonce} E \"$__uu_status\" Z' EXIT; trap 'exit 130' INT; printf '\\r\\n%s%s%s' 'UUC' '{nonce}' 'B'; ( cd -- {root} && eval {command} ) )\n"
    ))
}

/// Build the read-only current-directory probe used when binding a newly
/// authorized existing terminal. This intentionally has no remote-root cd.
pub fn wrap_pwd_probe(nonce: &str) -> Result<String> {
    validate_nonce(nonce)?;
    Ok(format!(
        "printf '\\r\\n%s%s%s' 'UUC' '{nonce}' 'B'; pwd; __uu_status=$?; printf '\\r\\n%s%s%s%d%s\\r\\n' 'UUC' '{nonce}' 'E' \"$__uu_status\" 'Z'\n"
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
    console: bool,
}

impl CompletionDecoder {
    pub fn new(nonce: &str) -> Result<Self> {
        Self::with_console_mode(nonce, false)
    }

    /// ConPTY returns terminal presentation bytes: protocol newlines may become
    /// cursor-addressing VT. Match complete nonce tokens independently of those
    /// separators while preserving the remaining presentation bytes.
    pub fn new_console(nonce: &str) -> Result<Self> {
        Self::with_console_mode(nonce, true)
    }

    fn with_console_mode(nonce: &str, console: bool) -> Result<Self> {
        validate_nonce(nonce)?;

        let separator = if console { "" } else { "\r\n" };
        let start_marker = format!("{separator}UUC{nonce}B").into_bytes();
        let end_prefix = format!("{separator}UUC{nonce}E").into_bytes();

        Ok(Self {
            start_marker,
            end_prefix,
            pending: Vec::new(),
            started: false,
            completed: None,
            console,
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
                .position(|byte| *byte == END_MARKER)
            {
                let status_end = status_start + relative_end;
                let status_text = std::str::from_utf8(&self.pending[status_start..status_end])
                    .context("completion marker contains a non-UTF-8 exit status")?;
                let status = status_text.parse::<i32>().map_err(|_| {
                    anyhow!("invalid exit status in completion marker: {status_text}")
                })?;
                if self.console && !(0..=255).contains(&status) {
                    bail!("completion exit status is outside 0..255");
                }
                let output = self.pending[..self.output_end(start)].to_vec();
                self.pending.clear();
                self.completed = Some(status);
                return Ok(output);
            }

            if self.console && self.pending.len() - status_start > 3 {
                bail!("completion exit status is too long");
            }
            let output = self.pending[..self.output_end(start)].to_vec();
            self.pending.drain(..start);
            return Ok(output);
        }

        let keep = longest_suffix_prefix(&self.pending, &self.end_prefix);
        // Retain a possible CRLF separator until the following nonce prefix is
        // known. This delays at most two presentation bytes, never a whole line.
        let separator_tail = if self.console { 2 } else { 0 };
        let emit_len = self.pending.len().saturating_sub(keep + separator_tail);
        Ok(self.pending.drain(..emit_len).collect())
    }

    fn output_end(&self, marker_start: usize) -> usize {
        if !self.console {
            return marker_start;
        }
        let before = &self.pending[..marker_start];
        if before.ends_with(b"\r\n") {
            marker_start - 2
        } else if before.ends_with(b"\n") {
            marker_start - 1
        } else {
            marker_start
        }
    }
}

fn longest_suffix_prefix(bytes: &[u8], prefix: &[u8]) -> usize {
    let max = bytes.len().min(prefix.len().saturating_sub(1));
    for length in (1..=max).rev() {
        if bytes[bytes.len() - length..] == prefix[..length] {
            return length;
        }
    }
    0
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
    fn wrapper_guards_interrupts_and_isolates_user_shell_control() {
        let wrapped = wrap_job(
            "/home/user/my project",
            "cd src && export X=1 && exit 7",
            "AbC123",
        )
        .unwrap();
        assert!(wrapped.starts_with("set +m; ( trap '__uu_status=$?; printf"));
        assert!(wrapped.contains("trap 'exit 130' INT"));
        assert!(wrapped.contains("printf '\\r\\n%s%s%s' 'UUC' 'AbC123' 'B';"));
        assert!(wrapped.contains("( cd -- '/home/user/my project' && eval "));
        assert!(wrapped.contains("eval 'cd src && export X=1 && exit 7' ) )\n"));
        assert!(wrapped
            .contains("printf \"\\r\\n%s%s%s%d%s\\r\\n\" UUC AbC123 E \"$__uu_status\" Z' EXIT"));
        assert!(!wrapped.contains("UUCAbC123B"));
        assert!(!wrapped.contains("UUCAbC123E"));
    }

    #[test]
    fn pwd_probe_keeps_its_start_marker_and_terminates_end_marker_with_newline() {
        let script = wrap_pwd_probe("abc123").unwrap();
        assert!(script.contains("printf '\\r\\n%s%s%s' 'UUC' 'abc123' 'B'; pwd;"));
        assert!(!script.contains("set +m"));
        assert!(!script.contains("cd --"));
        assert!(script.contains("printf '\\r\\n%s%s%s%d%s\\r\\n' 'UUC' 'abc123' 'E'"));
        assert!(!script.contains("UUCabc123B"));
        assert!(!script.contains("UUCabc123E"));
    }

    #[test]
    fn console_decoder_accepts_cursor_addressed_tokens_without_rewriting_payload() {
        let wire = b"\x1b[17;1HUUCnonceB/home/test\x1b[19;1HUUCnonceE0Z\r\nprompt$ ";
        for split in 0..=wire.len() {
            let mut decoder = CompletionDecoder::new_console("nonce").unwrap();
            let mut output = decoder.push(&wire[..split]).unwrap();
            output.extend(decoder.push(&wire[split..]).unwrap());
            assert_eq!(output, b"/home/test\x1b[19;1H");
            assert_eq!(decoder.completed(), Some(0));
        }
    }

    #[test]
    fn console_decoder_preserves_unicode_and_payload_newline_at_every_split() {
        let wire = "echoed wrapper\r\nUUCnonceB你好-UU\r\n\r\nUUCnonceE7Z\r\nprompt$ ".as_bytes();
        for split in 0..=wire.len() {
            let mut decoder = CompletionDecoder::new_console("nonce").unwrap();
            let mut output = decoder.push(&wire[..split]).unwrap();
            output.extend(decoder.push(&wire[split..]).unwrap());
            assert_eq!(output, "你好-UU\r\n".as_bytes());
            assert_eq!(decoder.completed(), Some(7));
        }
    }

    #[test]
    fn console_decoder_rejects_out_of_range_status_and_keeps_unknown_output() {
        let mut decoder = CompletionDecoder::new_console("nonce").unwrap();
        assert_eq!(decoder.push(b"UUCnonceBpartial").unwrap(), b"parti");
        assert_eq!(decoder.finish_without_marker(), b"al");
        let mut decoder = CompletionDecoder::new_console("nonce").unwrap();
        assert!(decoder.push(b"UUCnonceBUUCnonceE256Z").is_err());
        assert_eq!(decoder.completed(), None);
    }

    #[test]
    fn decoder_streams_short_output_without_waiting_for_marker_window() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        decoder.push(b"\r\nUUCnonceB").unwrap();
        assert_eq!(decoder.push(b"short").unwrap(), b"short");
        assert_eq!(decoder.completed(), None);
    }

    #[test]
    fn decoder_drops_echo_and_handles_markers_split_across_frames() {
        let mut decoder = CompletionDecoder::new("abc123").unwrap();
        let start = b"\r\nUUCabc123B";
        let end = b"\r\nUUCabc123E17Zprompt$ ";
        let mut received = Vec::new();

        received.extend(
            decoder
                .push(b"prompt$ printf '\\r\\n%s%s%s' 'UUC' 'abc123' 'B'\r\n")
                .unwrap(),
        );
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
    fn decoder_ignores_echoed_split_marker_before_start() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        let echo = b"printf '\\r\\n%s%s%s' 'UUC' 'nonce' 'B'\r\n";
        assert!(decoder.push(echo).unwrap().is_empty());
        assert!(!decoder.started());

        let mut output = decoder.push(b"\r\nUUCnonceBreal output\r\n").unwrap();
        output.extend(decoder.push(b"\r\nUUCnonceE0Zprompt$ ").unwrap());
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
    fn eof_after_start_does_not_delay_user_output() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        decoder.push(b"\r\nUUCnonceB").unwrap();
        assert_eq!(decoder.push(b"partial output").unwrap(), b"partial output");
        assert!(decoder.finish_without_marker().is_empty());
    }

    #[test]
    fn invalid_status_is_protocol_error() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        decoder.push(b"\r\nUUCnonceB").unwrap();
        let err = decoder.push(b"\r\nUUCnonceExZ").unwrap_err().to_string();
        assert!(err.contains("invalid exit status"));
    }

    #[test]
    fn end_marker_preserves_output_bytes_before_its_crlf_separator() {
        for (output, expected) in [
            (&b"plain\r\n\r\nUUCnonceE0Z"[..], &b"plain\r\n"[..]),
            (&b"plain\r\r\nUUCnonceE0Z"[..], &b"plain\r"[..]),
            (&b"plain\r\nUUCnonceE0Z"[..], &b"plain"[..]),
        ] {
            let mut decoder = CompletionDecoder::new("nonce").unwrap();
            decoder.push(b"\r\nUUCnonceB").unwrap();
            assert_eq!(decoder.push(output).unwrap(), expected);
            assert_eq!(decoder.completed(), Some(0));
        }
    }
}
