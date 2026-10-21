//! One hidden, current-user-only local route to one authorized UU attachment.
//! Leases and crash quarantine remain owned by the public caller. This process
//! never reconnects or replays a dispatched job.
#![cfg(windows)]

use crate::config::ConfigStore;
use crate::model::SessionInfo;
use crate::runner::{JobInput, JobResult, JobRunError};
use crate::transport::OfficialAttachPty;
use anyhow::{anyhow, bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROTOCOL: u32 = 1;
const MAX_FRAME: usize = 1024 * 1024;
const IDLE: Duration = Duration::from_secs(300);
type Handle = *mut c_void;

#[derive(Serialize, Deserialize)]
struct HostInfo {
    protocol: u32,
    pid: u32,
    session: SessionInfo,
}

#[derive(Serialize, Deserialize)]
enum Request {
    Hello {
        protocol: u32,
        fingerprint: String,
    },
    Run {
        id: u64,
        root: String,
        command: String,
    },
    Pwd {
        id: u64,
    },
    Input {
        id: u64,
        bytes: Option<Vec<u8>>,
        interrupt: bool,
    },
}

#[derive(Serialize, Deserialize)]
enum Response {
    Ready { protocol: u32, fingerprint: String },
    Output(Vec<u8>),
    Done { exit_code: i32, pwd: Option<String> },
    Failed { kind: String, message: String },
}

fn write_frame(file: &mut File, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker frame too large",
        ));
    }
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&bytes)
}

fn read_frame<T: DeserializeOwned>(file: &mut File) -> io::Result<T> {
    let mut size = [0; 4];
    file.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker frame too large",
        ));
    }
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn helper_exe() -> Result<PathBuf> {
    let path = std::env::current_exe()?.with_file_name("uu-background.exe");
    if !path.is_file() {
        bail!("uu-cli package is incomplete: uu-background.exe is missing");
    }
    Ok(path)
}

fn key(store: &ConfigStore, session: &SessionInfo) -> Result<String> {
    let fingerprint = session
        .authorization_fingerprint()
        .context("missing worker instance")?;
    let config = fs::canonicalize(store.path()).unwrap_or_else(|_| store.path().to_path_buf());
    let mut hash = Sha256::new();
    for part in [
        current_sid()?,
        config.to_string_lossy().to_lowercase(),
        helper_exe()?.to_string_lossy().to_lowercase(),
        fingerprint,
        PROTOCOL.to_string(),
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    Ok(hash.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn host_dir(store: &ConfigStore) -> PathBuf {
    crate::watcher::runtime_dir(store.path()).join("attachments")
}

fn metadata_path(store: &ConfigStore, session: &SessionInfo) -> Result<PathBuf> {
    Ok(host_dir(store).join(format!("{}.json", key(store, session)?)))
}

fn pipe_name(store: &ConfigStore, session: &SessionInfo) -> Result<String> {
    Ok(format!(r"\\.\pipe\uu-cli-worker-{}", key(store, session)?))
}

/// Metadata is a candidate index, never authorization or proof of an idle PTY.
pub fn live_sessions(store: &ConfigStore) -> Result<Vec<SessionInfo>> {
    let entries = match fs::read_dir(host_dir(store)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let Ok(info) = serde_json::from_slice::<HostInfo>(&bytes) else {
            continue;
        };
        if info.protocol == PROTOCOL
            && metadata_path(store, &info.session).is_ok_and(|path| path == entry.path())
        {
            sessions.push(info.session);
        }
    }
    Ok(sessions)
}

pub struct WorkerClient {
    pipe: File,
    output: File,
    next_id: u64,
}

impl WorkerClient {
    pub fn try_connect(store: &ConfigStore, session: &SessionInfo) -> Result<Option<Self>> {
        let path = metadata_path(store, session)?;
        let info: HostInfo = match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(info) => info,
            None => return Ok(None),
        };
        if info.protocol != PROTOCOL
            || info.session.authorization_fingerprint() != session.authorization_fingerprint()
        {
            return Ok(None);
        }
        let name = format!("{}-in", pipe_name(store, session)?);
        let deadline = Instant::now() + Duration::from_millis(750);
        let mut pipe = loop {
            match OpenOptions::new().read(true).write(true).open(&name) {
                Ok(pipe) => break pipe,
                Err(error) if error.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) if matches!(error.raw_os_error(), Some(2 | 231 | 233)) => {
                    return Ok(None)
                }
                Err(error) => return Err(error).context("cannot connect to local worker"),
            }
        };
        verify_server(&pipe, info.pid)?;
        // Independent input/output pipe objects keep a blocking read from
        // serializing an unrelated write on the same synchronous file object.
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{}-out", pipe_name(store, session)?))
            .context("cannot open local worker output")?;
        verify_server(&output, info.pid)?;
        let fingerprint = session
            .authorization_fingerprint()
            .context("missing worker instance")?;
        write_frame(
            &mut pipe,
            &Request::Hello {
                protocol: PROTOCOL,
                fingerprint: fingerprint.clone(),
            },
        )?;
        wait_readable(&output, Duration::from_secs(10))?;
        match read_frame(&mut output)? {
            Response::Ready {
                protocol: PROTOCOL,
                fingerprint: actual,
            } if actual == fingerprint => {}
            _ => bail!("local worker protocol/instance mismatch"),
        }
        Ok(Some(Self {
            pipe,
            output,
            next_id: 0,
        }))
    }

    pub fn connect_or_start(
        store: &ConfigStore,
        cli_path: &Path,
        session: &SessionInfo,
    ) -> Result<Self> {
        if let Some(client) = Self::try_connect(store, session)? {
            return Ok(client);
        }
        let child = spawn_helper(session, cli_path)?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(client) = Self::try_connect(store, session)? {
                return Ok(client);
            }
            if let Some(status) = child.exit_code()? {
                bail!("hidden Terminal worker exited before dispatch ({status}); official attachment could not be prepared");
            }
            if Instant::now() >= deadline {
                // No job request has been sent. Only stop this owned helper.
                let _ = child.kill();
                bail!("hidden Terminal worker did not become ready before dispatch");
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn run_job_with_input<O: Write + ?Sized>(
        &mut self,
        output: &mut O,
        root: &str,
        command: &str,
        input: &mpsc::Receiver<JobInput>,
    ) -> Result<JobResult, JobRunError> {
        self.next_id += 1;
        let id = self.next_id;
        let request = Request::Run {
            id,
            root: root.into(),
            command: command.into(),
        };
        self.exchange(&request, output, Some(input))
            .map(|(exit_code, _)| JobResult { exit_code })
    }

    pub fn capture_pwd(&mut self) -> Result<String, JobRunError> {
        self.next_id += 1;
        let request = Request::Pwd { id: self.next_id };
        let (_, pwd) = self.exchange(&request, &mut io::sink(), None)?;
        pwd.ok_or_else(|| JobRunError::Completed(anyhow!("worker returned no remote directory")))
    }

    fn exchange<O: Write + ?Sized>(
        &mut self,
        request: &Request,
        output: &mut O,
        input: Option<&mpsc::Receiver<JobInput>>,
    ) -> Result<(i32, Option<String>), JobRunError> {
        // Validate locally before writing any request bytes.
        let bytes =
            serde_json::to_vec(request).map_err(|error| JobRunError::Preparation(error.into()))?;
        if bytes.len() > MAX_FRAME {
            return Err(JobRunError::Preparation(anyhow!(
                "remote command is too large"
            )));
        }
        // Once request bytes are written, every IPC failure is Unknown. The
        // caller retains its inflight marker; there is no IPC retry.
        (|| -> Result<(i32, Option<String>), JobRunError> {
            write_frame(&mut self.pipe, request).map_err(unknown)?;
            let mut pending = Vec::new();
            // Keep input writes independent of output reads. Otherwise a large
            // duplex job can fill both pipe buffers and deadlock its caller.
            let input_pump = InputPump::new(&self.pipe).map_err(unknown)?;
            let mut pending_input = None;
            loop {
                input_pump.check().map_err(unknown)?;
                // Read available bytes incrementally; never block the input path
                // waiting for the rest of a large response frame.
                let available = available_bytes(&self.output).map_err(unknown)?;
                if available > 0 {
                    let mut bytes = vec![0; available.min(64 * 1024) as usize];
                    let count = self.output.read(&mut bytes).map_err(unknown)?;
                    if count == 0 {
                        return Err(unknown("local worker stream ended"));
                    }
                    pending.extend_from_slice(&bytes[..count]);
                    while let Some(response) = take_response(&mut pending).map_err(unknown)? {
                        match response {
                            Response::Output(bytes) => {
                                output
                                    .write_all(&bytes)
                                    .and_then(|_| output.flush())
                                    .map_err(unknown)?;
                            }
                            Response::Done { exit_code, pwd } => return Ok((exit_code, pwd)),
                            Response::Failed { kind, message } => {
                                return Err(match kind.as_str() {
                                    "preparation" => JobRunError::Preparation(anyhow!(message)),
                                    "completed" => JobRunError::Completed(anyhow!(message)),
                                    _ => unknown(message),
                                })
                            }
                            Response::Ready { .. } => {
                                return Err(unknown("unexpected worker handshake during job"))
                            }
                        }
                    }
                }
                if let Some(input) = input {
                    for _ in 0..8 {
                        let request = match pending_input.take() {
                            Some(request) => request,
                            None => match input.try_recv() {
                                Ok(JobInput::Bytes(bytes)) => Request::Input {
                                    id: self.next_id,
                                    bytes: Some(bytes),
                                    interrupt: false,
                                },
                                Ok(JobInput::Interrupt) => Request::Input {
                                    id: self.next_id,
                                    bytes: None,
                                    interrupt: true,
                                },
                                Ok(JobInput::Eof) => Request::Input {
                                    id: self.next_id,
                                    bytes: None,
                                    interrupt: false,
                                },
                                Err(_) => break,
                            },
                        };
                        match input_pump.sender.as_ref().unwrap().try_send(request) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(request)) => {
                                pending_input = Some(request);
                                break;
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                return Err(unknown("worker input writer stopped"))
                            }
                        }
                    }
                }
                if available == 0 {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        })()
    }
}

struct InputPump {
    sender: Option<mpsc::SyncSender<Request>>,
    done: mpsc::Receiver<io::Result<()>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl InputPump {
    fn new(pipe: &File) -> io::Result<Self> {
        let mut writer = pipe.try_clone()?;
        let (sender, receiver) = mpsc::sync_channel(8);
        let (done_tx, done) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            let result = (|| {
                while !thread_stop.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok(request) => {
                            if thread_stop.load(Ordering::Acquire) {
                                break;
                            }
                            write_frame(&mut writer, &request)?;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                Ok(())
            })();
            let _ = done_tx.send(result);
        });
        Ok(Self {
            sender: Some(sender),
            done,
            stop,
            thread: Some(thread),
        })
    }

    fn check(&self) -> io::Result<()> {
        match self.done.try_recv() {
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) | Err(mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("worker input writer stopped"))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(()),
        }
    }
}

impl Drop for InputPump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            unsafe {
                CancelSynchronousIo(thread.as_raw_handle());
            }
            if thread.is_finished() || self.done.recv_timeout(Duration::from_millis(250)).is_ok() {
                let _ = thread.join();
            }
        }
    }
}

fn unknown(error: impl std::fmt::Display) -> JobRunError {
    JobRunError::Unknown(anyhow!("{error}"))
}

fn take_response(bytes: &mut Vec<u8>) -> io::Result<Option<Response>> {
    if bytes.len() < 4 {
        return Ok(None);
    }
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker response too large",
        ));
    }
    if bytes.len() < 4 + length {
        return Ok(None);
    }
    let response = serde_json::from_slice(&bytes[4..4 + length]).map_err(io::Error::other)?;
    bytes.drain(..4 + length);
    Ok(Some(response))
}

struct Output<'a>(&'a mut File);
impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(8192) {
            write_frame(self.0, &Response::Output(chunk.to_vec()))?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type InputRoute = Arc<Mutex<Option<(u64, mpsc::SyncSender<JobInput>)>>>;

fn serve_client(
    pipe: &mut File,
    output: &mut File,
    transport: &mut OfficialAttachPty,
    session: &SessionInfo,
) -> Result<bool> {
    wait_readable(pipe, Duration::from_secs(10))?;
    let expected = session
        .authorization_fingerprint()
        .context("missing worker instance")?;
    match read_frame(pipe)? {
        Request::Hello {
            protocol: PROTOCOL,
            fingerprint,
        } if fingerprint == expected => {}
        _ => bail!("worker client protocol/instance mismatch"),
    }
    write_frame(
        output,
        &Response::Ready {
            protocol: PROTOCOL,
            fingerprint: expected,
        },
    )?;
    let mut reader = pipe.try_clone()?;
    let route: InputRoute = Arc::new(Mutex::new(None));
    let reader_route = route.clone();
    let (jobs_tx, jobs_rx) = mpsc::sync_channel(1);
    // A single reader owns the connection. Job ids stop late input from one
    // completed request becoming stdin for the next request on this pipe.
    thread::spawn(move || {
        while let Ok(request) = read_frame::<Request>(&mut reader) {
            match request {
                Request::Input {
                    id,
                    bytes,
                    interrupt,
                } => {
                    let sender = reader_route.lock().ok().and_then(|route| {
                        route
                            .as_ref()
                            .filter(|(active, _)| *active == id)
                            .map(|(_, sender)| sender.clone())
                    });
                    if let Some(sender) = sender {
                        let input = if interrupt {
                            JobInput::Interrupt
                        } else {
                            bytes.map(JobInput::Bytes).unwrap_or(JobInput::Eof)
                        };
                        let _ = sender.send(input);
                    }
                }
                Request::Run { id, .. } | Request::Pwd { id } => {
                    let (sender, receiver) = mpsc::sync_channel(8);
                    if let Ok(mut route) = reader_route.lock() {
                        *route = Some((id, sender));
                    }
                    if jobs_tx.send((request, receiver)).is_err() {
                        break;
                    }
                }
                Request::Hello { .. } => break,
            }
        }
        if let Ok(mut route) = reader_route.lock() {
            *route = None;
        }
    });
    let mut last_request = Instant::now();
    loop {
        let (request, input) = match jobs_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(true),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !transport.drain_idle()? {
                    return Ok(false);
                }
                if last_request.elapsed() > Duration::from_secs(30) {
                    return Ok(true);
                }
                continue;
            }
        };
        let result = match request {
            Request::Run { root, command, .. } => transport
                .run_job_with_input(&mut Output(output), &root, &command, &input)
                .map(|result| (result.exit_code, None)),
            Request::Pwd { .. } => transport.capture_pwd().map(|pwd| (0, Some(pwd))),
            _ => unreachable!(),
        };
        drop(input);
        if let Ok(mut route) = route.lock() {
            *route = None;
        }
        let keep = !matches!(&result, Err(JobRunError::Unknown(_)));
        let response = match result {
            Ok((exit_code, pwd)) => Response::Done { exit_code, pwd },
            Err(error) => {
                let (kind, message) = match error {
                    JobRunError::Preparation(error) => ("preparation", format!("{error:#}")),
                    JobRunError::Completed(error) => ("completed", format!("{error:#}")),
                    JobRunError::Unknown(error) => ("unknown", format!("{error:#}")),
                };
                Response::Failed {
                    kind: kind.into(),
                    message,
                }
            }
        };
        write_frame(output, &response)?;
        if !keep {
            // DisconnectNamedPipe discards unread output. Let the caller drain
            // the final partial output/error before retiring this attachment.
            unsafe {
                FlushFileBuffers(output.as_raw_handle());
            }
            return Ok(false);
        }
        last_request = Instant::now();
    }
}

pub fn run(store: &ConfigStore, session: SessionInfo, cli_path: &Path) -> Result<()> {
    if !store.load()?.is_session_authorized(&session) {
        bail!("Terminal is not authorized");
    }
    fs::create_dir_all(host_dir(store))?;
    let path = metadata_path(store, &session)?;
    let pipe = create_pipe(&format!("{}-in", pipe_name(store, &session)?))?;
    let output = create_pipe(&format!("{}-out", pipe_name(store, &session)?))?;
    let mut transport =
        OfficialAttachPty::attach(cli_path, &session.device_identity, &session.session_name)?;
    let info = HostInfo {
        protocol: PROTOCOL,
        pid: std::process::id(),
        session: session.clone(),
    };
    fs::write(&path, serde_json::to_vec(&info)?)?;
    struct RemoveMetadata(PathBuf);
    impl Drop for RemoveMetadata {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _metadata = RemoveMetadata(path);
    record_start_event();
    let mut pipe = pipe;
    let mut output = output;
    let mut idle_since = Instant::now();
    loop {
        if !transport.drain_idle()? || idle_since.elapsed() >= IDLE {
            return Ok(());
        }
        if !accept(&pipe)? {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let second_deadline = Instant::now() + Duration::from_secs(10);
        while !accept(&output)? {
            if Instant::now() >= second_deadline {
                bail!("worker output connection was not established");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let keep = serve_client(&mut pipe, &mut output, &mut transport, &session);
        unsafe {
            DisconnectNamedPipe(pipe.as_raw_handle());
            DisconnectNamedPipe(output.as_raw_handle());
        }
        if !matches!(keep, Ok(true)) {
            return keep.map(|_| ());
        }
        set_pipe_mode(&pipe, 1)?;
        set_pipe_mode(&output, 1)?;
        idle_since = Instant::now();
    }
}

fn record_start_event() {
    // Explicit synthetic fixture diagnostics only; no default raw/job logging.
    if std::env::var("UU_TEST_MODE").is_err() {
        return;
    }
    let Some(path) = std::env::var_os("UU_TEST_FIXTURE_EVENTS") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        use fs2::FileExt;
        if file.lock_exclusive().is_ok() {
            let started = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let _ = writeln!(
                file,
                "{}",
                serde_json::json!({"event":"worker_host_started","pid":std::process::id(),"started_at_unix_ms":started})
            );
            let _ = FileExt::unlock(&file);
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: Handle,
    inherit: i32,
}

#[repr(C)]
struct StartupInfo {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    columns: u32,
    rows: u32,
    fill: u32,
    flags: u32,
    show: u16,
    reserved_size: u16,
    reserved_bytes: *mut u8,
    stdin: Handle,
    stdout: Handle,
    stderr: Handle,
}

#[repr(C)]
struct ProcessInfo {
    process: Handle,
    thread: Handle,
    pid: u32,
    thread_id: u32,
}

struct HelperProcess(File);
impl HelperProcess {
    fn exit_code(&self) -> io::Result<Option<u32>> {
        if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } == 258 {
            return Ok(None);
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(code))
    }
    fn kill(&self) -> io::Result<()> {
        if unsafe { TerminateProcess(self.0.as_raw_handle(), 2) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn quote_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in argument.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(slashes));
        }
        slashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

fn spawn_helper(session: &SessionInfo, cli: &Path) -> Result<HelperProcess> {
    let exe = helper_exe()?;
    let executable = wide(&exe.to_string_lossy());
    let mut command = wide(
        &[
            exe.to_string_lossy().into_owned(),
            "__worker".into(),
            serde_json::to_string(session)?,
            cli.to_string_lossy().into_owned(),
        ]
        .iter()
        .map(|arg| quote_argument(arg))
        .collect::<Vec<_>>()
        .join(" "),
    );
    let mut startup: StartupInfo = unsafe { std::mem::zeroed() };
    startup.size = std::mem::size_of::<StartupInfo>() as u32;
    let mut process: ProcessInfo = unsafe { std::mem::zeroed() };
    // bInheritHandles=FALSE is essential: a surviving worker must not keep the
    // public caller's stdout/stderr pipes open. Environment/cwd are inherited;
    // the public entry point obtained this same ConfigStore from that environment.
    if unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            0x0800_0000 | 0x0000_0008,
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut startup,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("cannot start hidden Terminal worker");
    }
    unsafe {
        CloseHandle(process.thread);
    }
    Ok(HelperProcess(unsafe {
        File::from_raw_handle(process.process)
    }))
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateProcessW(
        app: *const u16,
        command: *mut u16,
        process_security: *mut SecurityAttributes,
        thread_security: *mut SecurityAttributes,
        inherit: i32,
        flags: u32,
        environment: Handle,
        directory: *const u16,
        startup: *mut StartupInfo,
        process: *mut ProcessInfo,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn TerminateProcess(process: Handle, code: u32) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        access: u32,
        mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *mut SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: Handle) -> i32;
    fn DisconnectNamedPipe(pipe: Handle) -> i32;
    fn FlushFileBuffers(pipe: Handle) -> i32;
    fn CancelSynchronousIo(thread: Handle) -> i32;
    fn SetNamedPipeHandleState(
        pipe: Handle,
        mode: *const u32,
        count: *const u32,
        timeout: *const u32,
    ) -> i32;
    fn PeekNamedPipe(
        pipe: Handle,
        buffer: Handle,
        size: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn GetNamedPipeServerProcessId(pipe: Handle, pid: *mut u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn LocalFree(memory: Handle) -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        data: Handle,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: Handle, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut Handle,
        size: *mut u32,
    ) -> i32;
}

fn sid_for_process(process: Handle) -> Result<String> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, 8, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let result = (|| {
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token, 1, std::ptr::null_mut(), 0, &mut needed);
        }
        let mut storage = vec![0u64; (needed as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(token, 1, storage.as_mut_ptr().cast(), needed, &mut needed)
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let sid = unsafe { *storage.as_ptr().cast::<Handle>() };
        let mut text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut len = 0;
        unsafe {
            while *text.add(len) != 0 {
                len += 1;
            }
        }
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe {
            LocalFree(text.cast());
        }
        Ok(value)
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

fn current_sid() -> Result<String> {
    sid_for_process(-1isize as Handle)
}

fn create_pipe(name: &str) -> Result<File> {
    let sddl = wide(&format!("D:P(A;;GA;;;{})", current_sid()?));
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let mut security = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor,
        inherit: 0,
    };
    let name = wide(name);
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            3 | 0x0008_0000,
            1 | 8,
            1,
            65536,
            65536,
            0,
            &mut security,
        )
    };
    let error = io::Error::last_os_error();
    unsafe {
        LocalFree(descriptor);
    }
    if handle == -1isize as Handle {
        return Err(error).context("cannot create private local worker pipe");
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn set_pipe_mode(pipe: &File, mode: u32) -> io::Result<()> {
    if unsafe {
        SetNamedPipeHandleState(
            pipe.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn accept(pipe: &File) -> io::Result<bool> {
    if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0 {
        // With PIPE_NOWAIT a nonzero return after disconnect only means the
        // instance is listening again. A client exists only at error 535.
        return Ok(false);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(535) => {
            set_pipe_mode(pipe, 0)?;
            Ok(true)
        }
        Some(536) => Ok(false),
        Some(232) => {
            unsafe {
                DisconnectNamedPipe(pipe.as_raw_handle());
            }
            Ok(false)
        }
        _ => Err(io::Error::last_os_error()),
    }
}

fn available_bytes(pipe: &File) -> io::Result<u32> {
    let mut available = 0;
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

fn wait_readable(pipe: &File, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if available_bytes(pipe)? > 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "local worker handshake timed out",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn verify_server(pipe: &File, expected_pid: u32) -> Result<()> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if pid != expected_pid {
        bail!("local worker process identity changed");
    }
    let process = unsafe { OpenProcess(0x1000, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let result = (|| {
        if sid_for_process(process)? != current_sid()? {
            bail!("local worker belongs to a different Windows user");
        }
        let mut name = vec![0u16; 32768];
        let mut size = name.len() as u32;
        if unsafe { QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut size) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if !String::from_utf16_lossy(&name[..size as usize])
            .eq_ignore_ascii_case(&helper_exe()?.to_string_lossy())
        {
            bail!("local worker executable mismatch");
        }
        Ok(())
    })();
    unsafe {
        CloseHandle(process);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_parser_handles_fragmented_frames_and_rejects_oversize() {
        let value = serde_json::to_vec(&Response::Done {
            exit_code: 7,
            pwd: None,
        })
        .unwrap();
        let mut bytes = (value.len() as u32).to_le_bytes().to_vec();
        bytes.extend(&value[..2]);
        assert!(take_response(&mut bytes).unwrap().is_none());
        bytes.extend(&value[2..]);
        assert!(matches!(
            take_response(&mut bytes).unwrap(),
            Some(Response::Done { exit_code: 7, .. })
        ));
        assert!(bytes.is_empty());
        assert!(take_response(&mut ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec()).is_err());
    }
}
