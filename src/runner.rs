use crate::job::{wrap_job, wrap_pwd_probe, CompletionDecoder};
use anyhow::{anyhow, Context};
use std::fmt;
use std::io::{self, Read, Write};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(windows, test))]
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
#[cfg(any(windows, test))]
use std::thread;
#[cfg(any(windows, test))]
use std::thread::JoinHandle;
#[cfg(test)]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
            Self::Completed(error) => {
                write!(f, "remote operation completed with an error: {error:#}")
            }
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
    O: Write + ?Sized,
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
    O: Write + ?Sized,
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
    O: Write + ?Sized,
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
    O: Write + ?Sized,
{
    let mut decoder = CompletionDecoder::new(nonce).map_err(JobRunError::Preparation)?;
    #[cfg(any(windows, test))]
    let mut conpty_dsr = ConPtyDsrHandshake::default();

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
            #[cfg(any(windows, test))]
            let trailing = {
                let pending = decoder
                    .push(&conpty_dsr.finish())
                    .context("invalid remote job framing")
                    .map_err(JobRunError::Unknown)?;
                let mut trailing = pending;
                trailing.extend(decoder.finish_without_marker());
                trailing
            };
            #[cfg(not(any(windows, test)))]
            let trailing = decoder.finish_without_marker();
            write_visible(output, &trailing, "failed to stream final remote output")?;
            return Err(JobRunError::Unknown(anyhow!(
                "terminal stream ended before the completion marker"
            )));
        }

        #[cfg(any(windows, test))]
        let visible = decode_job_bytes(&mut decoder, &mut conpty_dsr, writer, &buffer[..read])
            .context("invalid remote job framing or ConPTY cursor handshake")
            .map_err(JobRunError::Unknown)?;
        #[cfg(not(any(windows, test)))]
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
pub(crate) fn run_framed_job_with_input<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    input: &Receiver<JobInput>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
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
    input: &Receiver<JobInput>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
{
    let wrapped = wrap_job(remote_root, shell_command, nonce).map_err(JobRunError::Preparation)?;
    let mut decoder = CompletionDecoder::new(nonce).map_err(JobRunError::Preparation)?;
    #[cfg(any(windows, test))]
    let mut conpty_dsr = ConPtyDsrHandshake::default();

    writer
        .write_all(wrapped.as_bytes())
        .and_then(|_| writer.flush())
        .context("failed while sending the framed command")
        .map_err(JobRunError::Unknown)?;

    let mut stdin_eof = false;
    let mut input_disconnected = false;
    loop {
        if decoder.started() && !input_disconnected {
            match input.recv_timeout(INPUT_POLL) {
                Ok(JobInput::Interrupt) => {
                    writer
                        .write_all(b"\x03")
                        .and_then(|_| writer.flush())
                        .context("failed while forwarding terminal interrupt")
                        .map_err(JobRunError::Unknown)?;
                }
                Ok(JobInput::Bytes(bytes)) if !stdin_eof => {
                    writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .context("failed while forwarding terminal input")
                        .map_err(JobRunError::Unknown)?;
                }
                Ok(JobInput::Eof) if !stdin_eof => {
                    writer
                        .write_all(b"\x04")
                        .and_then(|_| writer.flush())
                        .context("failed while forwarding terminal EOF")
                        .map_err(JobRunError::Unknown)?;
                    stdin_eof = true;
                }
                Ok(JobInput::Bytes(_) | JobInput::Eof) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => input_disconnected = true,
            }
        }

        match reader
            .read_event_timeout(INPUT_POLL)
            .context("terminal transport failed while waiting for completion")
            .map_err(JobRunError::Unknown)?
        {
            PumpRead::Data(bytes) => {
                #[cfg(any(windows, test))]
                let visible = decode_job_bytes(&mut decoder, &mut conpty_dsr, writer, &bytes)
                    .context("invalid remote job framing or ConPTY cursor handshake")
                    .map_err(JobRunError::Unknown)?;
                #[cfg(not(any(windows, test)))]
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
                #[cfg(any(windows, test))]
                let trailing = {
                    let pending = decoder
                        .push(&conpty_dsr.finish())
                        .context("invalid remote job framing")
                        .map_err(JobRunError::Unknown)?;
                    let mut trailing = pending;
                    trailing.extend(decoder.finish_without_marker());
                    trailing
                };
                #[cfg(not(any(windows, test)))]
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

fn write_visible<O: Write + ?Sized>(
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

#[cfg(any(windows, test))]
const CONPTY_CURSOR_QUERY: &[u8] = b"\x1b[6n";
#[cfg(any(windows, test))]
const CONPTY_CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// Handles the initial cursor-position query emitted by Windows ConPTY before
/// normal terminal traffic. Other startup bytes are preserved for the job
/// decoder. The reply uses the known origin of this newly opened pseudoconsole.
#[cfg(any(windows, test))]
#[derive(Default)]
struct ConPtyDsrHandshake {
    pending: Vec<u8>,
    cursor_queries: usize,
}

#[cfg(any(windows, test))]
impl ConPtyDsrHandshake {
    fn push<W: Write + ?Sized>(&mut self, writer: &mut W, chunk: &[u8]) -> io::Result<Vec<u8>> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::new();

        loop {
            if let Some(start) = self
                .pending
                .windows(CONPTY_CURSOR_QUERY.len())
                .position(|window| window == CONPTY_CURSOR_QUERY)
            {
                output.extend_from_slice(&self.pending[..start]);
                self.pending.drain(..start + CONPTY_CURSOR_QUERY.len());
                writer.write_all(CONPTY_CURSOR_REPLY)?;
                writer.flush()?;
                self.cursor_queries += 1;
                continue;
            }

            let keep = longest_suffix_prefix(&self.pending, CONPTY_CURSOR_QUERY);
            let emit = self.pending.len().saturating_sub(keep);
            output.extend(self.pending.drain(..emit));
            break;
        }

        Ok(output)
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(any(windows, test))]
fn longest_suffix_prefix(bytes: &[u8], prefix: &[u8]) -> usize {
    let max = bytes.len().min(prefix.len().saturating_sub(1));
    for length in (1..=max).rev() {
        if bytes[bytes.len() - length..] == prefix[..length] {
            return length;
        }
    }
    0
}

/// Decode terminal bytes and answer ConPTY startup DSR queries until the
/// remote job BEGIN marker has been recognized.
#[cfg(any(windows, test))]
fn decode_job_bytes<W: Write + ?Sized>(
    decoder: &mut CompletionDecoder,
    handshake: &mut ConPtyDsrHandshake,
    writer: &mut W,
    bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut visible = Vec::new();
    let mut index = 0;

    while index < bytes.len() && !decoder.started() {
        let filtered = handshake.push(writer, &bytes[index..index + 1])?;
        if !filtered.is_empty() {
            visible.extend(decoder.push(&filtered)?);
        }
        index += 1;
    }

    if index < bytes.len() {
        visible.extend(decoder.push(&bytes[index..])?);
    }
    Ok(visible)
}

/// Drain a ConPTY fixture in tests while preserving all bytes other than DSR queries.
#[cfg(test)]
pub(crate) fn initialize_conpty(
    reader: &mut ReadPump,
    writer: &mut (impl Write + ?Sized),
    wait: Duration,
) -> io::Result<(Vec<u8>, usize)> {
    let deadline = Instant::now() + wait;
    let mut handshake = ConPtyDsrHandshake::default();
    let mut output = Vec::new();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let poll = remaining.min(Duration::from_millis(25));
        match reader.read_event_timeout(poll)? {
            PumpRead::Data(bytes) => output.extend(handshake.push(writer, &bytes)?),
            PumpRead::Eof => {
                output.extend(handshake.finish());
                return Ok((output, handshake.cursor_queries));
            }
            PumpRead::Timeout => {}
        }
    }

    output.extend(handshake.finish());
    Ok((output, handshake.cursor_queries))
}
/// A single, demand-driven PTY reader. It reads at most one chunk per request,
/// allowing a completed job to reuse the same terminal without a blocked
/// per-job thread. Shutdown is bounded in case an underlying reader ignores
/// closure of its PTY handles.
pub(crate) struct ReadPump {
    request_tx: Option<SyncSender<()>>,
    response_rx: Option<Receiver<io::Result<Option<Vec<u8>>>>>,
    #[cfg(any(windows, test))]
    thread: Option<JoinHandle<()>>,
    #[cfg(any(windows, test))]
    done_rx: Option<Receiver<()>>,
    request_pending: bool,
    pending: Vec<u8>,
    eof: bool,
}

impl ReadPump {
    #[cfg(any(windows, test))]
    pub(crate) fn spawn<R>(mut reader: R) -> Self
    where
        R: Read + Send + 'static,
    {
        let (request_tx, request_rx) = mpsc::sync_channel::<()>(0);
        let (response_tx, response_rx) = mpsc::sync_channel::<io::Result<Option<Vec<u8>>>>(1);
        let (done_tx, done_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            while request_rx.recv().is_ok() {
                let mut buffer = [0u8; 8192];
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = response_tx.send(Ok(None));
                        break;
                    }
                    Ok(read) => {
                        if response_tx.send(Ok(Some(buffer[..read].to_vec()))).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = response_tx.send(Err(error));
                        break;
                    }
                }
            }
            let _ = done_tx.send(());
        });

        Self {
            request_tx: Some(request_tx),
            response_rx: Some(response_rx),
            #[cfg(any(windows, test))]
            thread: Some(thread),
            #[cfg(any(windows, test))]
            done_rx: Some(done_rx),
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

    #[cfg(any(windows, test))]
    pub(crate) fn shutdown_and_join(&mut self) {
        self.request_tx.take();
        self.response_rx.take();
        if let Some(thread) = self.thread.take() {
            let finished = self
                .done_rx
                .take()
                .map(|done_rx| done_rx.recv_timeout(Duration::from_secs(2)));
            match finished {
                Some(Ok(())) | Some(Err(RecvTimeoutError::Disconnected)) => {
                    let _ = thread.join();
                }
                Some(Err(RecvTimeoutError::Timeout)) | None => drop(thread),
            }
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
                &input_rx,
            );
            reader.shutdown_and_join();
            (result, output)
        });

        let wrapper = writes_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(String::from_utf8_lossy(&wrapper).contains("UUCLI;abc123;BEGIN"));

        input_tx.send(JobInput::Bytes(b"typed".to_vec())).unwrap();
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
            b"\x04"
        );
        input_tx.send(JobInput::Interrupt).unwrap();
        assert_eq!(
            writes_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"\x03"
        );

        remote_tx
            .send(b"\x1b]7770;UUCLI;abc123;END;0\x07".to_vec())
            .unwrap();
        let (result, output) = job.join().unwrap();
        assert_eq!(result.unwrap().exit_code, 0);
        assert_eq!(output, b"ready");
        assert!(all.lock().unwrap().ends_with(b"typed\x04\x03"));
    }

    #[test]
    fn answers_startup_dsr_before_begin_and_preserves_remote_dsr_after_begin() {
        let remote = b"\x1b[6nnoise\x1b]7770;UUCLI;abc123;BEGIN\x07payload\x1b[6n\x1b]7770;UUCLI;abc123;END;0\x07";
        let mut reader = Cursor::new(remote.to_vec());
        let mut writer = Vec::new();
        let mut output = Vec::new();

        let result = run_framed_job_with_nonce(
            &mut reader,
            &mut writer,
            &mut output,
            "/root",
            "printf payload",
            "abc123",
        )
        .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(output, b"payload\x1b[6n");
        assert!(writer.ends_with(CONPTY_CURSOR_REPLY));
        assert_eq!(
            writer
                .windows(CONPTY_CURSOR_REPLY.len())
                .filter(|window| *window == CONPTY_CURSOR_REPLY)
                .count(),
            1
        );
    }

    #[test]
    fn conpty_startup_answers_split_cursor_query_and_preserves_other_bytes() {
        let (remote_tx, remote_rx) = mpsc::channel();
        let mut reader = ReadPump::spawn(ChannelReader {
            chunks: remote_rx,
            current: Cursor::new(Vec::new()),
        });
        remote_tx.send(b"prefix\x1b[6".to_vec()).unwrap();
        remote_tx.send(b"nSuffix".to_vec()).unwrap();
        drop(remote_tx);

        let mut writer = Vec::new();
        let (visible, queries) =
            initialize_conpty(&mut reader, &mut writer, Duration::from_millis(100)).unwrap();
        reader.shutdown_and_join();

        assert_eq!(visible, b"prefixSuffix");
        assert_eq!(queries, 1);
        assert_eq!(writer, CONPTY_CURSOR_REPLY);
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
