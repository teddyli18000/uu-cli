use crate::job::{wrap_job, wrap_pwd_probe, CompletionDecoder};
use anyhow::{anyhow, Context};
use std::fmt;
use std::io::{self, Read, Write};
use std::process;
#[cfg(any(windows, test))]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(windows, test))]
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
#[cfg(any(windows, test))]
use std::sync::Arc;
#[cfg(any(windows, test))]
use std::thread;
#[cfg(any(windows, test))]
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn run_framed_job_with_child<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    child_has_exited: impl FnMut() -> io::Result<bool>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
{
    let nonce = next_nonce();
    let wrapped = wrap_job(remote_root, shell_command, &nonce).map_err(JobRunError::Preparation)?;
    run_prepared_job_with_child(
        reader,
        writer,
        output,
        &wrapped,
        &nonce,
        true,
        None,
        child_has_exited,
    )
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
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn run_framed_pwd_probe_with_child<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    child_has_exited: impl FnMut() -> io::Result<bool>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
{
    let nonce = next_nonce();
    let wrapped = wrap_pwd_probe(&nonce).map_err(JobRunError::Preparation)?;
    run_prepared_job_with_child(
        reader,
        writer,
        output,
        &wrapped,
        &nonce,
        true,
        None,
        child_has_exited,
    )
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

    send_wrapped_command(writer, wrapped)
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
const CHILD_EXIT_DRAIN: Duration = Duration::from_millis(250);

#[cfg(any(windows, test))]
#[derive(Default)]
struct ConPtyInputNormalizer {
    previous_was_cr: bool,
}

#[cfg(any(windows, test))]
impl ConPtyInputNormalizer {
    fn normalize(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut normalized = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match byte {
                b'\n' if self.previous_was_cr => self.previous_was_cr = false,
                b'\n' => {
                    normalized.push(b'\r');
                    self.previous_was_cr = false;
                }
                other => {
                    normalized.push(other);
                    self.previous_was_cr = other == b'\r';
                }
            }
        }
        normalized
    }
}

/// Bytes are forwarded as terminal input; Interrupt sends the terminal's
/// Ctrl+C character; Eof sends Ctrl+D once and closes this job's input stream.
#[derive(Debug)]
pub enum JobInput {
    Bytes(Vec<u8>),
    Interrupt,
    Eof,
}
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn run_framed_job_with_input_and_child<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    remote_root: &str,
    shell_command: &str,
    input: &Receiver<JobInput>,
    child_has_exited: impl FnMut() -> io::Result<bool>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
{
    let nonce = next_nonce();
    let wrapped = wrap_job(remote_root, shell_command, &nonce).map_err(JobRunError::Preparation)?;
    run_prepared_job_with_child(
        reader,
        writer,
        output,
        &wrapped,
        &nonce,
        true,
        Some(input),
        child_has_exited,
    )
}

#[cfg(test)]
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
    run_prepared_job_with_child(
        reader,
        writer,
        output,
        &wrapped,
        nonce,
        false,
        Some(input),
        || Ok(false),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_prepared_job_with_child<W, O>(
    reader: &mut ReadPump,
    writer: &mut W,
    output: &mut O,
    wrapped: &str,
    nonce: &str,
    console_decoder: bool,
    input: Option<&Receiver<JobInput>>,
    mut child_has_exited: impl FnMut() -> io::Result<bool>,
) -> Result<JobResult, JobRunError>
where
    W: Write,
    O: Write + ?Sized,
{
    let mut decoder = if console_decoder {
        CompletionDecoder::new_console(nonce)
    } else {
        CompletionDecoder::new(nonce)
    }
    .map_err(JobRunError::Preparation)?;
    #[cfg(any(windows, test))]
    let mut conpty_dsr = ConPtyDsrHandshake::default();

    send_wrapped_command(writer, wrapped)
        .context("failed while sending the framed command")
        .map_err(JobRunError::Unknown)?;

    let mut stdin_eof = false;
    let mut input_disconnected = false;
    let mut child_exit_deadline = None;
    #[cfg(windows)]
    let mut console_input_newlines = if console_decoder {
        Some(ConPtyInputNormalizer::default())
    } else {
        None
    };

    loop {
        if child_has_exited()
            .context("failed while checking official UU terminal process")
            .map_err(JobRunError::Unknown)?
        {
            child_exit_deadline.get_or_insert_with(|| Instant::now() + CHILD_EXIT_DRAIN);
        }

        if let Some(deadline) = child_exit_deadline {
            if Instant::now() >= deadline {
                flush_incomplete_job_output(
                    &mut decoder,
                    #[cfg(any(windows, test))]
                    &mut conpty_dsr,
                    output,
                )?;
                return Err(JobRunError::Unknown(anyhow!(
                    "official UU terminal process exited before the completion marker after {CHILD_EXIT_DRAIN:?} of PTY draining"
                )));
            }
        }

        if decoder.started() && !input_disconnected && child_exit_deadline.is_none() {
            if let Some(input) = input {
                match input.recv_timeout(INPUT_POLL) {
                    Ok(JobInput::Interrupt) => {
                        writer
                            .write_all(b"\x03")
                            .and_then(|_| writer.flush())
                            .context("failed while forwarding terminal interrupt")
                            .map_err(JobRunError::Unknown)?;
                    }
                    Ok(JobInput::Bytes(bytes)) if !stdin_eof => {
                        #[cfg(windows)]
                        let bytes = match console_input_newlines.as_mut() {
                            Some(normalizer) => normalizer.normalize(&bytes),
                            None => bytes,
                        };
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
        }

        let wait = child_exit_deadline
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(INPUT_POLL)
            })
            .unwrap_or(INPUT_POLL);
        match reader
            .read_event_timeout(wait)
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
                flush_incomplete_job_output(
                    &mut decoder,
                    #[cfg(any(windows, test))]
                    &mut conpty_dsr,
                    output,
                )?;
                return Err(JobRunError::Unknown(anyhow!(
                    "terminal stream ended before the completion marker"
                )));
            }
            PumpRead::Timeout => {}
        }
    }
}

fn flush_incomplete_job_output<O: Write + ?Sized>(
    decoder: &mut CompletionDecoder,
    #[cfg(any(windows, test))] conpty_dsr: &mut ConPtyDsrHandshake,
    output: &mut O,
) -> Result<(), JobRunError> {
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
    write_visible(output, &trailing, "failed to stream final remote output")
}
// ConPTY consumes keyboard input with CR as Enter; convert the wrapper's trailing LF.
fn send_wrapped_command<W: Write>(writer: &mut W, wrapped: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let mut bytes = wrapped.as_bytes().to_vec();
        if bytes.last() == Some(&b'\n') {
            *bytes.last_mut().expect("nonempty command") = b'\r';
        }
        writer.write_all(&bytes)?;
    }
    #[cfg(not(windows))]
    writer.write_all(wrapped.as_bytes())?;
    writer.flush()
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

/// Complete ConPTY's initial cursor query before writing any shell command.
/// Input sent earlier can be consumed by the cursor-position exchange.
#[cfg(any(windows, test))]
pub(crate) fn await_conpty_startup(
    reader: &mut ReadPump,
    writer: &mut (impl Write + ?Sized),
    wait: Duration,
) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + wait;
    let mut handshake = ConPtyDsrHandshake::default();
    let mut output = Vec::new();
    while Instant::now() < deadline {
        let poll = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(25));
        match reader.read_event_timeout(poll)? {
            PumpRead::Data(bytes) => {
                let visible = handshake.push(writer, &bytes)?;
                let remaining = 16_384usize.saturating_sub(output.len());
                output.extend_from_slice(&visible[..visible.len().min(remaining)]);
                if handshake.cursor_queries > 0 {
                    output.extend(handshake.finish());
                    return Ok(output);
                }
            }
            PumpRead::Eof => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "ConPTY closed before its cursor handshake; startup output: {:?}",
                        String::from_utf8_lossy(&output)
                    ),
                ));
            }
            PumpRead::Timeout => {}
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut,
        format!("ConPTY cursor handshake was not received within {wait:?}; this terminal environment is incompatible; startup output: {:?}", String::from_utf8_lossy(&output))))
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
#[cfg(any(windows, test))]
const READ_PUMP_MAX_BACKLOG: usize = 8 * 1024 * 1024;

#[cfg(any(windows, test))]
fn reserve_read_pump_backlog(queued: &AtomicUsize, incoming: usize) -> bool {
    let mut current = queued.load(Ordering::Relaxed);
    loop {
        let Some(next) = current.checked_add(incoming) else {
            return false;
        };
        if next > READ_PUMP_MAX_BACKLOG {
            return false;
        }
        match queued.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(any(windows, test))]
const PTY_RX_DIAGNOSTIC_MAX_BYTES: usize = 8 * 1024;

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
enum PtyRxPhase {
    Startup,
    Runner,
}

#[cfg(any(windows, test))]
struct PtyRxDiagnostic {
    enabled: bool,
    phase: PtyRxPhase,
    startup: Vec<u8>,
    runner: Vec<u8>,
    captured_bytes: usize,
    truncated: bool,
}

#[cfg(any(windows, test))]
impl PtyRxDiagnostic {
    fn from_env() -> Self {
        let test_mode = std::env::var("UU_TEST_MODE").ok();
        let fixture_events_present = std::env::var_os("UU_TEST_FIXTURE_EVENTS").is_some();
        Self::with_gate(test_mode.as_deref(), fixture_events_present)
    }

    fn with_gate(test_mode: Option<&str>, fixture_events_present: bool) -> Self {
        Self {
            enabled: test_mode == Some("bash") && fixture_events_present,
            phase: PtyRxPhase::Startup,
            startup: Vec::new(),
            runner: Vec::new(),
            captured_bytes: 0,
            truncated: false,
        }
    }

    fn set_runner_phase(&mut self) {
        self.phase = PtyRxPhase::Runner;
    }

    fn record_rx(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        let remaining = PTY_RX_DIAGNOSTIC_MAX_BYTES.saturating_sub(self.captured_bytes);
        let captured = &bytes[..bytes.len().min(remaining)];
        let target = match self.phase {
            PtyRxPhase::Startup => &mut self.startup,
            PtyRxPhase::Runner => &mut self.runner,
        };
        target.extend_from_slice(captured);
        self.captured_bytes += captured.len();
        self.truncated |= captured.len() < bytes.len();
    }

    fn failure_line(&self, child_exit_code: Option<u32>) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let encode_hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        Some(format!(
            "uu-test-pty-rx startup_hex={} runner_hex={} child_exit_code={child_exit_code:?} truncated={}",
            encode_hex(&self.startup),
            encode_hex(&self.runner),
            self.truncated
        ))
    }
}
/// Continuously drains the PTY so ConPTY output cannot block terminal input.
/// Buffered output is bounded; after the limit, output is drained and dropped
/// and the consumer receives one error. Shutdown is bounded in case an
/// underlying reader ignores closure of its PTY handles.
pub(crate) struct ReadPump {
    response_rx: Option<Receiver<io::Result<Option<Vec<u8>>>>>,
    #[cfg(any(windows, test))]
    thread: Option<JoinHandle<()>>,
    #[cfg(any(windows, test))]
    done_rx: Option<Receiver<()>>,
    #[cfg(any(windows, test))]
    stop: Arc<AtomicBool>,
    #[cfg(any(windows, test))]
    queued_bytes: Arc<AtomicUsize>,
    #[cfg(any(windows, test))]
    diagnostic: PtyRxDiagnostic,
    pending: Vec<u8>,
    eof: bool,
}

impl ReadPump {
    #[cfg(any(windows, test))]
    pub(crate) fn spawn<R>(mut reader: R) -> Self
    where
        R: Read + Send + 'static,
    {
        let (response_tx, response_rx) = mpsc::channel::<io::Result<Option<Vec<u8>>>>();
        let (done_tx, done_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let reader_stop = Arc::clone(&stop);
        let reader_queued_bytes = Arc::clone(&queued_bytes);
        let thread = thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            let mut dropping_output = false;
            loop {
                if reader_stop.load(Ordering::Acquire) {
                    break;
                }
                let read = reader.read(&mut buffer);
                if reader_stop.load(Ordering::Acquire) {
                    break;
                }
                match read {
                    Ok(0) => {
                        let _ = response_tx.send(Ok(None));
                        break;
                    }
                    Ok(_) if dropping_output => {}
                    Ok(read) => {
                        if !reserve_read_pump_backlog(&reader_queued_bytes, read) {
                            dropping_output = true;
                            let error = io::Error::other(
                                format!(
                                    "terminal output backlog exceeded {READ_PUMP_MAX_BACKLOG} bytes; draining and dropping further output"
                                ),
                            );
                            if response_tx.send(Err(error)).is_err() {
                                break;
                            }
                            continue;
                        }
                        let bytes = buffer[..read].to_vec();
                        if response_tx.send(Ok(Some(bytes))).is_err() {
                            reader_queued_bytes.fetch_sub(read, Ordering::AcqRel);
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
            response_rx: Some(response_rx),
            thread: Some(thread),
            done_rx: Some(done_rx),
            stop,
            queued_bytes,
            diagnostic: PtyRxDiagnostic::from_env(),
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
        let response_rx = self.response_rx.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "terminal reader is stopped")
        })?;
        let received = match timeout {
            Some(timeout) => response_rx.recv_timeout(timeout),
            None => response_rx
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        let received = match received {
            Ok(received) => received,
            Err(RecvTimeoutError::Timeout) => return Ok(PumpRead::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                self.eof = true;
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal reader thread stopped",
                ));
            }
        };

        match received {
            Ok(Some(bytes)) => {
                #[cfg(any(windows, test))]
                self.queued_bytes.fetch_sub(bytes.len(), Ordering::AcqRel);
                #[cfg(any(windows, test))]
                self.diagnostic.record_rx(&bytes);
                Ok(PumpRead::Data(bytes))
            }
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
    pub(crate) fn set_diagnostic_runner_phase(&mut self) {
        self.diagnostic.set_runner_phase();
    }

    #[cfg(any(windows, test))]
    pub(crate) fn record_diagnostic_failure(&mut self, child_exit_code: Option<u32>) {
        if let Some(line) = self.diagnostic.failure_line(child_exit_code) {
            let _ = writeln!(io::stderr().lock(), "{line}");
        }
    }
    #[cfg(any(windows, test))]
    pub(crate) fn shutdown_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
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
    fn conpty_input_normalizes_line_endings_across_chunks_and_preserves_other_bytes() {
        let mut normalizer = ConPtyInputNormalizer::default();

        assert_eq!(
            normalizer.normalize(b"first\nsecond\r\nbinary\0"),
            b"first\rsecond\rbinary\0"
        );
        assert_eq!(normalizer.normalize(b"third\r"), b"third\r");
        assert_eq!(normalizer.normalize(b"\n"), b"");
        // The normalizer buffers no output, so EOF never synthesizes a newline.
        assert_eq!(normalizer.normalize(b""), b"");
    }

    #[test]
    fn streams_only_framed_user_output_and_returns_exit_code() {
        let remote = b"old prompt and echoed wrapper\r\n\r\nUUCabc123Bhello\r\nworld\r\n\r\nUUCabc123E7Znext$ ";
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
        assert!(sent.contains("UUC' 'abc123' 'B"));
        assert!(sent.contains("UUC abc123 E"));
    }

    #[test]
    fn eof_after_start_is_unknown_and_preserves_partial_output() {
        let remote = b"noise\r\nUUCabc123Bpartial";
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
    struct WaitableChannelReader {
        chunks: Receiver<Vec<u8>>,
        current: Cursor<Vec<u8>>,
        stopped: Arc<AtomicBool>,
    }

    impl Read for WaitableChannelReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            loop {
                if (self.current.position() as usize) < self.current.get_ref().len() {
                    return self.current.read(output);
                }
                if self.stopped.load(Ordering::Acquire) {
                    return Ok(0);
                }
                match self.chunks.recv_timeout(Duration::from_millis(10)) {
                    Ok(chunk) => self.current = Cursor::new(chunk),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Ok(0),
                }
            }
        }
    }

    #[test]
    fn pty_rx_diagnostic_is_test_gated_and_bounded() {
        let mut disabled = PtyRxDiagnostic::with_gate(None, true);
        disabled.record_rx(b"not logged");
        assert!(disabled.failure_line(Some(9)).is_none());

        let mut diagnostic = PtyRxDiagnostic::with_gate(Some("bash"), true);
        diagnostic.record_rx(&vec![b'A'; PTY_RX_DIAGNOSTIC_MAX_BYTES / 2]);
        diagnostic.set_runner_phase();
        diagnostic.record_rx(&vec![b'B'; PTY_RX_DIAGNOSTIC_MAX_BYTES]);
        let line = diagnostic.failure_line(Some(7)).unwrap();

        assert!(line.contains("startup_hex=41"));
        assert!(line.contains("runner_hex=42"));
        assert!(line.contains("child_exit_code=Some(7)"));
        assert!(line.contains("truncated=true"));
        assert_eq!(diagnostic.captured_bytes, PTY_RX_DIAGNOSTIC_MAX_BYTES);
    }
    #[test]
    fn read_pump_drains_multiple_chunks_without_event_consumer() {
        let (chunks_tx, chunks_rx) = mpsc::sync_channel(0);
        let (sent_tx, sent_rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            let all_sent = [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
                .into_iter()
                .all(|chunk| chunks_tx.send(chunk).is_ok());
            let _ = sent_tx.send(all_sent);
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let mut reader = ReadPump::spawn(WaitableChannelReader {
            chunks: chunks_rx,
            current: Cursor::new(Vec::new()),
            stopped: Arc::clone(&stopped),
        });

        // A synchronous producer models a writer blocked until the PTY reader consumes output.
        let all_sent = sent_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or(false);
        stopped.store(true, Ordering::Release);
        reader.shutdown_and_join();
        producer.join().unwrap();

        assert!(
            all_sent,
            "PTY output producer blocked while no read_event call was made"
        );
    }

    #[test]
    fn child_exit_is_bounded_for_job_input_and_pwd_probe_paths() {
        for path in ["job", "input", "pwd"] {
            let (_remote_tx, remote_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            let worker = thread::spawn(move || {
                let mut reader = ReadPump::spawn(ChannelReader {
                    chunks: remote_rx,
                    current: Cursor::new(Vec::new()),
                });
                let mut writer = Vec::new();
                let mut output = Vec::new();
                let started = Instant::now();
                let result = match path {
                    "job" => run_framed_job_with_child(
                        &mut reader,
                        &mut writer,
                        &mut output,
                        "/root",
                        "work",
                        || Ok(true),
                    ),
                    "input" => {
                        let (_input_tx, input_rx) = mpsc::channel();
                        run_framed_job_with_input_and_child(
                            &mut reader,
                            &mut writer,
                            &mut output,
                            "/root",
                            "read-input",
                            &input_rx,
                            || Ok(true),
                        )
                    }
                    "pwd" => run_framed_pwd_probe_with_child(
                        &mut reader,
                        &mut writer,
                        &mut output,
                        || Ok(true),
                    ),
                    _ => unreachable!(),
                };
                let unknown = matches!(result, Err(JobRunError::Unknown(_)));
                let _ = result_tx.send((unknown, started.elapsed()));
                reader.shutdown_and_join();
            });

            let outcome = result_rx.recv_timeout(Duration::from_secs(2));
            drop(_remote_tx);
            let (unknown, elapsed) = outcome
                .unwrap_or_else(|error| panic!("{path} path did not notice child exit: {error}"));
            assert!(
                unknown,
                "{path} path should report an unknown result without a completion marker"
            );
            assert!(
                elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(1),
                "{path} path should drain for at most 250 ms after child exit, took {elapsed:?}"
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn child_exit_drain_accepts_late_completion() {
        let (remote_tx, remote_rx) = mpsc::channel();
        remote_tx.send(b"UUCabc123Bpartial".to_vec()).unwrap();
        let delayed_tx = remote_tx.clone();
        let delayed = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            delayed_tx.send(b"UUCabc123E7Z".to_vec()).unwrap();
        });
        let mut reader = ReadPump::spawn(ChannelReader {
            chunks: remote_rx,
            current: Cursor::new(Vec::new()),
        });
        let mut writer = Vec::new();
        let mut output = Vec::new();
        let wrapped = wrap_job("/root", "work", "abc123").unwrap();

        let result = run_prepared_job_with_child(
            &mut reader,
            &mut writer,
            &mut output,
            &wrapped,
            "abc123",
            true,
            None,
            || Ok(true),
        )
        .unwrap();
        delayed.join().unwrap();
        drop(remote_tx);
        reader.shutdown_and_join();

        assert_eq!(result.exit_code, 7);
        assert_eq!(output, b"partial");
    }

    #[test]
    fn child_exit_without_completion_flushes_partial_output_and_returns_unknown() {
        let (remote_tx, remote_rx) = mpsc::channel();
        remote_tx.send(b"UUCabc123Bpartial".to_vec()).unwrap();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut reader = ReadPump::spawn(ChannelReader {
                chunks: remote_rx,
                current: Cursor::new(Vec::new()),
            });
            let mut writer = Vec::new();
            let mut output = Vec::new();
            let wrapped = wrap_job("/root", "work", "abc123").unwrap();
            let started = Instant::now();
            let result = run_prepared_job_with_child(
                &mut reader,
                &mut writer,
                &mut output,
                &wrapped,
                "abc123",
                true,
                None,
                || Ok(true),
            );
            let _ = result_tx.send((
                matches!(result, Err(JobRunError::Unknown(_))),
                output,
                started.elapsed(),
            ));
            reader.shutdown_and_join();
        });

        let outcome = result_rx.recv_timeout(Duration::from_secs(2));
        drop(remote_tx);
        let (unknown, output, elapsed) =
            outcome.unwrap_or_else(|error| panic!("child exit drain did not finish: {error}"));
        assert!(unknown);
        assert_eq!(output, b"partial");
        assert!(elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(1));
        worker.join().unwrap();
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
        assert!(String::from_utf8_lossy(&wrapper).contains("UUC' 'abc123' 'B"));

        input_tx.send(JobInput::Bytes(b"typed".to_vec())).unwrap();
        input_tx.send(JobInput::Eof).unwrap();
        assert!(writes_rx.recv_timeout(Duration::from_millis(30)).is_err());

        remote_tx.send(b"\r\nUUCabc123Bready".to_vec()).unwrap();
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

        remote_tx.send(b"\r\nUUCabc123E0Z".to_vec()).unwrap();
        let (result, output) = job.join().unwrap();
        assert_eq!(result.unwrap().exit_code, 0);
        assert_eq!(output, b"ready");
        assert!(all.lock().unwrap().ends_with(b"typed\x04\x03"));
    }

    #[test]
    fn answers_startup_dsr_before_begin_and_preserves_remote_dsr_after_begin() {
        let remote = b"\x1b[6nnoise\r\nUUCabc123Bpayload\x1b[6n\r\nUUCabc123E0Z";
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
    fn startup_must_receive_a_cursor_query_before_command_dispatch() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"startup\x1b[6n".to_vec()).unwrap();
        let mut reader = ReadPump::spawn(ChannelReader {
            chunks: rx,
            current: Cursor::new(Vec::new()),
        });
        let mut writer = Vec::new();
        assert_eq!(
            await_conpty_startup(&mut reader, &mut writer, Duration::from_millis(100)).unwrap(),
            b"startup"
        );
        assert_eq!(writer, CONPTY_CURSOR_REPLY);
        drop(tx);
        reader.shutdown_and_join();

        let (tx, rx) = mpsc::channel();
        let mut reader = ReadPump::spawn(ChannelReader {
            chunks: rx,
            current: Cursor::new(Vec::new()),
        });
        let mut writer = Vec::new();
        let error =
            await_conpty_startup(&mut reader, &mut writer, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(writer.is_empty());
        drop(tx);
        reader.shutdown_and_join();
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
