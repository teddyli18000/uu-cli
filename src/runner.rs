use crate::job::{wrap_job, CompletionDecoder};
use anyhow::{anyhow, Context};
use std::fmt;
use std::io::{Read, Write};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobResult {
    pub exit_code: i32,
}

/// Failure classes deliberately separate preparation from an uncertain remote
/// execution. Once any part of the framed command has been written, transport,
/// protocol, or local-output failures are `Unknown`: the caller must not replay
/// the command automatically and should quarantine that worker.
#[derive(Debug)]
pub enum JobRunError {
    Preparation(anyhow::Error),
    Unknown(anyhow::Error),
}

impl JobRunError {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl fmt::Display for JobRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparation(error) => write!(f, "cannot prepare remote job: {error:#}"),
            Self::Unknown(error) => write!(f, "remote job state is unknown: {error:#}"),
        }
    }
}

impl std::error::Error for JobRunError {}

pub fn run_framed_job<R, W, O>(
    reader: &mut R,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
) -> Result<JobResult, JobRunError>
where
    R: Read,
    W: Write,
    O: Write,
{
    let nonce = next_nonce();
    run_framed_job_with_nonce(
        reader,
        writer,
        output,
        remote_root,
        shell_command,
        &nonce,
    )
}

pub fn run_framed_job_with_nonce<R, W, O>(
    reader: &mut R,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    nonce: &str,
) -> Result<JobResult, JobRunError>
where
    R: Read,
    W: Write,
    O: Write,
{
    let wrapped = wrap_job(remote_root, shell_command, nonce).map_err(JobRunError::Preparation)?;
    let mut decoder = CompletionDecoder::new(nonce).map_err(JobRunError::Preparation)?;

    writer
        .write_all(wrapped.as_bytes())
        .and_then(|_| writer.flush())
        .context("failed while sending the framed command")
        .map_err(JobRunError::Unknown)?;

    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("terminal transport failed while waiting for completion")
            .map_err(JobRunError::Unknown)?;

        if read == 0 {
            let trailing = decoder.finish_without_marker();
            if !trailing.is_empty() {
                output
                    .write_all(&trailing)
                    .context("failed to stream final remote output")
                    .map_err(JobRunError::Unknown)?;
            }
            return Err(JobRunError::Unknown(anyhow!(
                "terminal stream ended before the completion marker"
            )));
        }

        let visible = decoder
            .push(&buffer[..read])
            .context("invalid remote job framing")
            .map_err(JobRunError::Unknown)?;
        if !visible.is_empty() {
            output
                .write_all(&visible)
                .context("failed to stream remote output")
                .map_err(JobRunError::Unknown)?;
            output
                .flush()
                .context("failed to flush remote output")
                .map_err(JobRunError::Unknown)?;
        }

        if let Some(exit_code) = decoder.completed() {
            return Ok(JobResult { exit_code });
        }
    }
}

fn next_nonce() -> String {
    let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{nanos:x}{counter:x}", process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn streams_only_framed_user_output_and_returns_exit_code() {
        let remote = b"old prompt and echoed wrapper\r\n\x1b]7770;UUCLI;abc123;BEGIN\x07hello\r\nworld\r\n\x1b]7770;UUCLI;abc123;END;7\x07next$ ";
        let mut reader = Cursor::new(remote.to_vec());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let result = run_framed_job_with_nonce(
            &mut reader,
            &mut writer,
            &mut output,
            "/home/pi/aic",
            "python3 main.py",
            "abc123",
        )
        .unwrap();

        assert_eq!(result.exit_code, 7);
        assert_eq!(output, b"hello\r\nworld\r\n");
        let sent = String::from_utf8(writer).unwrap();
        assert!(sent.contains("cd -- '/home/pi/aic'"));
        assert!(sent.contains("'python3 main.py'"));
        assert!(sent.contains("UUCLI;abc123;BEGIN"));
        assert!(sent.contains("UUCLI;abc123;END;%d"));
    }

    #[test]
    fn eof_after_start_is_unknown_and_preserves_partial_output() {
        let remote = b"noise\x1b]7770;UUCLI;abc123;BEGIN\x07partial";
        let mut reader = Cursor::new(remote.to_vec());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let error = run_framed_job_with_nonce(
            &mut reader,
            &mut writer,
            &mut output,
            "/root",
            "do-work",
            "abc123",
        )
        .unwrap_err();

        assert!(error.is_unknown());
        assert_eq!(output, b"partial");
    }

    #[test]
    fn eof_before_start_is_unknown_without_leaking_shell_ui() {
        let mut reader = Cursor::new(b"prompt$ echoed command".to_vec());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let error = run_framed_job_with_nonce(
            &mut reader,
            &mut writer,
            &mut output,
            "/root",
            "pwd",
            "abc123",
        )
        .unwrap_err();

        assert!(error.is_unknown());
        assert!(output.is_empty());
    }

    #[test]
    fn invalid_remote_root_fails_before_writing() {
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let error = run_framed_job_with_nonce(
            &mut reader,
            &mut writer,
            &mut output,
            "",
            "pwd",
            "abc123",
        )
        .unwrap_err();

        assert!(!error.is_unknown());
        assert!(writer.is_empty());
    }
}
