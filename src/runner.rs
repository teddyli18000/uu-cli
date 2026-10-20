use crate::job::{wrap_job, wrap_pwd_probe, CompletionDecoder};
use anyhow::{anyhow, Context};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
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
    /// The remote command finished, but its result could not be used.
    Completed(anyhow::Error),
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
            Self::Completed(error) => write!(f, "remote operation completed with an error: {error:#}"),
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
    run_framed_job_with_nonce(reader, writer, output, remote_root, shell_command, &nonce)
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
    run_prepared_job(reader, writer, output, &wrapped, nonce)
}

/// Run the read-only pwd probe in the current shell directory. The caller must
/// still treat any transport failure after sending the probe as an unknown job.
pub fn run_framed_pwd_probe<R, W, O>(
    reader: &mut R,
    writer: &mut W,
    output: &mut O,
) -> Result<JobResult, JobRunError>
where
    R: Read,
    W: Write,
    O: Write,
{
    let nonce = next_nonce();
    let wrapped = wrap_pwd_probe(&nonce).map_err(JobRunError::Preparation)?;
    run_prepared_job(reader, writer, output, &wrapped, &nonce)
}

fn run_prepared_job<R, W, O>(
    reader: &mut R,
    writer: &mut W,
    output: &mut O,
    wrapped: &str,
    nonce: &str,
) -> Result<JobResult, JobRunError>
where
    R: Read,
    W: Write,
    O: Write,
{
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
            write_visible(output, &trailing, "failed to stream final remote output")?;
            return Err(JobRunError::Unknown(anyhow!(
                "terminal stream ended before the completion marker"
            )));
        }

        let visible = decoder
            .push(&buffer[..read])
            .context("invalid remote job framing")
            .map_err(JobRunError::Unknown)?;
        write_visible(output, &visible, "failed to stream remote output")?;

        if let Some(exit_code) = decoder.completed() {
            return Ok(JobResult { exit_code });
        }
    }
}

const INPUT_POLL: Duration = Duration::from_millis(10);

/// Bytes are forwarded as terminal input; Interrupt sends the terminal's
/// Ctrl+C character; Eof sends Ctrl+D once and closes this job's input stream.
#[derive(Debug)]
pub enum JobInput {
    Bytes(Vec<u8>),
    Interrupt,
    Eof,
}

/// Run a framed command while also forwarding input events. Input stays queued
/// until the remote BEGIN marker proves the command wrapper reached the shell.
pub fn run_framed_job_with_input<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    input: Receiver<JobInput>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write,
{
    let nonce = next_nonce();
    run_framed_job_with_input_and_nonce(
        reader,
        writer,
        output,
        remote_root,
        shell_command,
        &nonce,
        input,
    )
}

pub(crate) fn run_framed_job_with_input_and_nonce<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    nonce: &str,
    input: Receiver<JobInput>,
) -> Result<JobResult, JobRunError>
where
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

    let mut input_closed = false;
    loop {
        if decoder.started() && !input_closed {
            match input.recv_timeout(INPUT_POLL) {
                Ok(event) => {
                    let (bytes, close_input): (&[u8], bool) = match &event {
                        JobInput::Bytes(bytes) => (bytes, false),
                        JobInput::Interrupt => (b"\x03", false),
                        JobInput::Eof => (b"\x04", true),
                    };
                    if !bytes.is_empty() {
                        writer
                            .write_all(bytes)
                            .and_then(|_| writer.flush())
                            .context("failed while forwarding terminal input")
                            .map_err(JobRunError::Unknown)?;
                    }
                    input_closed = close_input;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => input_closed = true,
            }
        }

        match reader
            .read_event_timeout(INPUT_POLL)
            .context("terminal transport failed while waiting for completion")
            .map_err(JobRunError::Unknown)?
        {
            PumpRead::Data(bytes) => {
                let visible = decoder
                    .push(&bytes)
                    .context("invalid remote job framing")
                    .map_err(JobRunError::Unknown)?;
                write_visible(output, &visible, "failed to stream remote output")?;
                if let Some(exit_code) = decoder.completed() {
                    return Ok(JobResult { exit_code });
                }
            }
            PumpRead::Eof => {
                let trailing = decoder.finish_without_marker();
                write_visible(output, &trailing, "failed to stream final remote output")?;
                return Err(JobRunError::Unknown(anyhow!(
                    "terminal stream ended before the completion marker"
                )));
            }
            PumpRead::Timeout => {}
        }
    }
}

fn write_visible<O: Write>(
    output: &mut O,
    visible: &[u8],
    error_context: &str,
) -> Result<(), JobRunError> {
    if !visible.is_empty() {
        output
            .write_all(visible)
            .context(error_context.to_owned())
            .map_err(JobRunError::Unknown)?;
        output
            .flush()
            .context("failed to flush remote output")
            .map_err(JobRunError::Unknown)?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum PumpRead {
    Data(Vec<u8>),
    Eof,
    Timeout,
}

/// A single, demand-driven PTY reader. It reads at most one chunk per request,
/// allowing a completed job to reuse the same terminal without a blocked
/// per-job thread. Shutdown is performed after the owning local CLI is stopped.
pub(crate) struct ReadPump {
    request_tx: Option<SyncSender<()>>,
    response_rx: Option<Receiver<io::Result<Option<Vec<u8>>>>>,
    thread: Option<JoinHandle<()>>,
    request_pending: bool,
    pending: Vec<u8>,
    eof: bool,
}

impl ReadPump {
    pub(crate) fn spawn<R>(mut reader: R) -> Self
    where
        R: Read + Send + 'static,
    {
        let (request_tx, request_rx) = mpsc::sync_channel::<()>(0);
        let (response_tx, response_rx) = mpsc::sync_channel::<io::Result<Option<Vec<u8>>>>(1);
        let thread = thread::spawn(move || {
            while request_rx.recv().is_ok() {
                let mut buffer = [0u8; 8192];
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = response_tx.send(Ok(None));
                        break;
                    }
                    Ok(read) => {
                        if response_tx
                            .send(Ok(Some(buffer[..read].to_vec())))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = response_tx.send(Err(error));
                        break;
                    }
                }
            }
        });

        Self {
            request_tx: Some(request_tx),
            response_rx: Some(response_rx),
            thread: Some(thread),
            request_pending: false,
            pending: Vec::new(),
            eof: false,
        }
    }

    pub(crate) fn read_event_timeout(&mut self, timeout: Duration) -> io::Result<PumpRead> {
        self.read_event(Some(timeout))
    }

    fn read_event(&mut self, timeout: Option<Duration>) -> io::Result<PumpRead> {
        if !self.pending.is_empty() {
            return Ok(PumpRead::Data(std::mem::take(&mut self.pending)));
        }
        if self.eof {
            return Ok(PumpRead::Eof);
        }
        if !self.request_pending {
            let request_tx = self.request_tx.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "terminal reader is stopped")
            })?;
            request_tx.send(()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal reader thread stopped",
                )
            })?;
            self.request_pending = true;
        }

        let response_rx = self.response_rx.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "terminal reader is stopped")
        })?;
        let received = match timeout {
            Some(timeout) => match response_rx.recv_timeout(timeout) {
                Ok(received) => received,
                Err(RecvTimeoutError::Timeout) => return Ok(PumpRead::Timeout),
                Err(RecvTimeoutError::Disconnected) => {
                    self.request_pending = false;
                    self.eof = true;
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "terminal reader thread stopped",
                    ));
                }
            },
            None => response_rx.recv().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal reader thread stopped",
                )
            })?,
        };

        self.request_pending = false;
        match received {
            Ok(Some(bytes)) => Ok(PumpRead::Data(bytes)),
            Ok(None) => {
                self.eof = true;
                Ok(PumpRead::Eof)
            }
            Err(error) => {
                self.eof = true;
                Err(error)
            }
        }
    }

    pub(crate) fn shutdown_and_join(&mut self) {
        self.request_tx.take();
        self.response_rx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Read for ReadPump {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        match self.read_event(None)? {
            PumpRead::Data(bytes) => {
                let read = output.len().min(bytes.len());
                output[..read].copy_from_slice(&bytes[..read]);
                if read < bytes.len() {
                    self.pending.extend_from_slice(&bytes[read..]);
                }
                Ok(read)
            }
            PumpRead::Eof => Ok(0),
            PumpRead::Timeout => unreachable!("blocking reader cannot time out"),
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

    struct ChannelReader {
        chunks: Receiver<Vec<u8>>,
        current: Cursor<Vec<u8>>,
    }

    impl Read for ChannelReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            loop {
                if (self.current.position() as usize) < self.current.get_ref().len() {
                    return self.current.read(output);
                }
                match self.chunks.recv() {
                    Ok(chunk) => self.current = Cursor::new(chunk),
                    Err(_) => return Ok(0),
                }
            }
        }
    }

    struct ObservedWriter {
        all: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        writes: mpsc::Sender<Vec<u8>>,
    }

    impl Write for ObservedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.all.lock().unwrap().extend_from_slice(bytes);
            self.writes
                .send(bytes.to_vec())
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "test closed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn input_and_interrupt_wait_until_remote_begin_and_then_forward() {
        let (remote_tx, remote_rx) = mpsc::channel();
        let (input_tx, input_rx) = mpsc::channel();
        let (writes_tx, writes_rx) = mpsc::channel();
        let all = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_for_job = all.clone();

        let job = thread::spawn(move || {
            let mut reader = ReadPump::spawn(ChannelReader {
                chunks: remote_rx,
                current: Cursor::new(Vec::new()),
            });
            let mut writer = ObservedWriter {
                all: all_for_job,
                writes: writes_tx,
            };
            let mut output = Vec::new();
            let result = run_framed_job_with_input_and_nonce(
                &mut reader,
                &mut writer,
                &mut output,
                "/root",
                "read-input",
                "abc123",
                input_rx,
            );
            reader.shutdown_and_join();
            (result, output)
        });

        let wrapper = writes_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(String::from_utf8_lossy(&wrapper).contains("UUCLI;abc123;BEGIN"));

        input_tx.send(JobInput::Bytes(b"typed".to_vec())).unwrap();
        input_tx.send(JobInput::Interrupt).unwrap();
        input_tx.send(JobInput::Eof).unwrap();
        assert!(writes_rx.recv_timeout(Duration::from_millis(30)).is_err());

        remote_tx
            .send(b"\x1b]7770;UUCLI;abc123;BEGIN\x07ready".to_vec())
            .unwrap();
        assert_eq!(
            writes_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"typed"
        );
        assert_eq!(
            writes_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"\x03"
        );
        assert_eq!(
            writes_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"\x04"
        );

        remote_tx
            .send(b"\x1b]7770;UUCLI;abc123;END;0\x07".to_vec())
            .unwrap();
        let (result, output) = job.join().unwrap();
        assert_eq!(result.unwrap().exit_code, 0);
        assert_eq!(output, b"ready");
        assert!(all.lock().unwrap().ends_with(b"typed\x03\x04"));
    }


    #[test]
    fn invalid_remote_root_fails_before_writing() {
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let error =
            run_framed_job_with_nonce(&mut reader, &mut writer, &mut output, "", "pwd", "abc123")
                .unwrap_err();

        assert!(!error.is_unknown());
        assert!(writer.is_empty());
    }
}
