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
/// The user command executes in a subshell. `cd`, `export`, `source`, `exit`,
/// and `exec` therefore cannot become hidden prerequisites of a later uu job.
/// `eval` moves parsing of the user expression inside that subshell, so a
/// syntax error still returns control to the parent shell and emits a marker.
pub fn wrap_job(remote_root: &str, shell_command: &str, nonce: &str) -> Result<String> {
    if remote_root.is_empty() {
        bail!("remote root is empty");
    }
    if nonce.is_empty() || !nonce.bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("job nonce must be non-empty ASCII alphanumeric text");
    }

    let root = shell_quote(remote_root);
    let command = shell_quote(shell_command);

    Ok(format!(
        "( cd -- {root} && eval {command} ); __uu_status=$?; printf '\\033]7770;UUCLI;{nonce};%d\\007' \"$__uu_status\"\n"
    ))
}

pub fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// Streaming decoder for the private OSC completion marker.
///
/// It holds only a short suffix while a marker may be split across PTY frames.
/// Once completion is observed, bytes after the marker (normally the next shell
/// prompt) are intentionally discarded rather than leaking shell UI into uu's
/// stdout.
#[derive(Debug)]
pub struct CompletionDecoder {
    marker_prefix: Vec<u8>,
    pending: Vec<u8>,
    completed: Option<i32>,
}

impl CompletionDecoder {
    pub fn new(nonce: &str) -> Result<Self> {
        if nonce.is_empty() || !nonce.bytes().all(|b| b.is_ascii_alphanumeric()) {
            bail!("job nonce must be non-empty ASCII alphanumeric text");
        }
        let mut marker_prefix = OSC_PREFIX.to_vec();
        marker_prefix.extend_from_slice(nonce.as_bytes());
        marker_prefix.push(b';');
        Ok(Self {
            marker_prefix,
            pending: Vec::new(),
            completed: None,
        })
    }

    pub fn completed(&self) -> Option<i32> {
        self.completed
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>> {
        if self.completed.is_some() {
            return Ok(Vec::new());
        }

        self.pending.extend_from_slice(chunk);

        if let Some(start) = find_subslice(&self.pending, &self.marker_prefix) {
            let status_start = start + self.marker_prefix.len();
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

        // Keep enough trailing bytes to recognize a marker split across chunks.
        let keep = self.marker_prefix.len().saturating_sub(1);
        if self.pending.len() <= keep {
            return Ok(Vec::new());
        }
        let emit_len = self.pending.len() - keep;
        Ok(self.pending.drain(..emit_len).collect())
    }

    /// Flush buffered non-marker output only when the transport ended before a
    /// marker. Callers should then report the job state as unknown, never replay.
    pub fn finish_without_marker(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
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
    fn wrapper_isolates_state_in_subshell() {
        let wrapped = wrap_job(
            "/home/user/my project",
            "cd src && export X=1 && exit 7",
            "AbC123",
        )
        .unwrap();
        assert!(wrapped.starts_with("( cd -- '/home/user/my project' && eval "));
        assert!(wrapped.contains("'cd src && export X=1 && exit 7'"));
        assert!(wrapped.contains("UUCLI;AbC123;%d"));
    }

    #[test]
    fn decoder_handles_marker_split_across_frames() {
        let mut decoder = CompletionDecoder::new("abc123").unwrap();
        let marker = b"\x1b]7770;UUCLI;abc123;17\x07prompt$ ";
        let mut received = Vec::new();
        received.extend(decoder.push(b"hello ").unwrap());
        received.extend(decoder.push(&marker[..7]).unwrap());
        received.extend(decoder.push(&marker[7..18]).unwrap());
        received.extend(decoder.push(&marker[18..]).unwrap());
        assert_eq!(received, b"hello ");
        assert_eq!(decoder.completed(), Some(17));
    }

    #[test]
    fn decoder_does_not_match_echoed_backslash_escape() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        let text = b"printf '\\033]7770;UUCLI;nonce;%d\\007'\r\nreal output\r\n";
        let mut output = decoder.push(text).unwrap();
        output.extend(decoder.finish_without_marker());
        assert_eq!(output, text);
        assert_eq!(decoder.completed(), None);
    }

    #[test]
    fn invalid_status_is_protocol_error() {
        let mut decoder = CompletionDecoder::new("nonce").unwrap();
        let err = decoder
            .push(b"out\x1b]7770;UUCLI;nonce;nope\x07")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid exit status"));
    }
}
