#![cfg(all(windows, feature = "test-fixture"))]

use fs2::FileExt;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEVICE_ID: &str = "device-binding";
const DEVICE_NAME: &str = "测试设备";
const SESSION_NAME: &str = "bash-first";
const CREATED_AT: &str = "2026-10-20T12:00:00Z";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

struct Workspace {
    _temp: TempDir,
    root: PathBuf,
    nested: PathBuf,
    config: PathBuf,
    fixture: PathBuf,
    events: PathBuf,
    transcript: PathBuf,
}
impl Workspace {
    fn new() -> Self {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("workspace");
        let nested = root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create workspace");
        let git = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .output()
            .expect("git init");
        assert!(
            git.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        let config = temp.path().join("config").join("missing.json");
        let fixture = temp.path().join("fixture.json");
        let events = temp.path().join("events.jsonl");
        let transcript = temp.path().join("decision-console.txt");
        atomic_fixture(&fixture, &json!({"devices": []}));
        Self {
            _temp: temp,
            root,
            nested,
            config,
            fixture,
            events,
            transcript,
        }
    }
}

fn atomic_fixture(path: &Path, value: &Value) {
    let temporary = path.with_extension("json.next");
    fs::write(
        &temporary,
        serde_json::to_vec(value).expect("serialize fixture"),
    )
    .expect("write fixture temp");
    let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) };
    assert_ne!(
        replaced,
        0,
        "atomically replace fixture: {}",
        std::io::Error::last_os_error()
    );
}
fn bash_path() -> PathBuf {
    [
        std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .map(|p| p.join("Git")),
        std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .map(|p| p.join("Git")),
        Some(PathBuf::from(r"C:\Program Files\Git")),
        Some(PathBuf::from(r"C:\Program Files (x86)\Git")),
    ]
    .into_iter()
    .flatten()
    .map(|root| root.join("bin").join("bash.exe"))
    .find(|path| path.is_file())
    .expect("hosted Windows Git Bash executable")
}
fn base_command(test: &Workspace, bash: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uu"));
    command
        .current_dir(&test.root)
        .env("UU_CLI_CONFIG", &test.config)
        .env("UUYC_CLI", env!("CARGO_BIN_EXE_fake-uuyc"))
        .env("UU_TEST_FIXTURE_JSON", &test.fixture)
        .env("UU_TEST_FIXTURE_EVENTS", &test.events)
        .env("UU_TEST_MODE", "bash")
        .env("UU_TEST_BASH", bash)
        .env("UU_TEST_BASH_CWD", test._temp.path().join("remote-root"))
        .env("UU_CLI_NO_BACKGROUND", "1");
    command
}
fn read_events(path: &Path) -> Vec<Value> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    FileExt::lock_shared(&file).expect("shared-lock event read");
    let mut contents = String::new();
    let result = (&file).read_to_string(&mut contents);
    FileExt::unlock(&file).expect("unlock event read");
    result.expect("read event JSONL");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse event"))
        .collect()
}
fn wait_for_available_terminal(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let events = read_events(path);
        let available = events.iter().any(|event| {
            event["event"] == "terminal_list"
                && event["sessions"].as_array().is_some_and(|sessions| {
                    sessions.iter().any(|session| {
                        session["deviceId"] == DEVICE_ID
                            && session["sessionName"] == SESSION_NAME
                            && session["createdAt"] == CREATED_AT
                            && session["available"] == true
                    })
                })
        });
        if available {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "terminal list did not observe fixture update:\n{}",
                serde_json::to_string_pretty(&events).unwrap()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}
fn pipe_reader<R: Read + Send + 'static>(mut reader: R) -> Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader.read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    receive
}
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
struct Running {
    child: Option<Child>,
    pid: u32,
    stdout: Receiver<std::io::Result<Vec<u8>>>,
    stderr: Receiver<std::io::Result<Vec<u8>>>,
}
impl Running {
    fn start(mut command: Command, input: &[u8]) -> Self {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn uu public entry");
        child
            .stdin
            .take()
            .expect("uu stdin")
            .write_all(input)
            .expect("write caller stdin sentinel");
        let pid = child.id();
        let stdout = pipe_reader(child.stdout.take().unwrap());
        let stderr = pipe_reader(child.stderr.take().unwrap());
        Self {
            child: Some(child),
            pid,
            stdout,
            stderr,
        }
    }
    fn finish(mut self, timeout: Duration) -> Output {
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        loop {
            match self.child.as_mut().unwrap().try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => {
                    timed_out = true;
                    kill_tree(self.pid);
                    break;
                }
            }
        }
        let mut child = self.child.take().unwrap();
        let status = child.wait().expect("wait uu");
        let stdout = self
            .stdout
            .recv_timeout(Duration::from_secs(10))
            .expect("stdout closed")
            .expect("read stdout");
        let stderr = self
            .stderr
            .recv_timeout(Duration::from_secs(10))
            .expect("stderr closed")
            .expect("read stderr");
        assert!(
            !timed_out,
            "uu timed out; stdout={}\nstderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        Output {
            status,
            stdout,
            stderr,
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_tree(self.pid);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn read_pipe(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes);
    bytes
}
fn drive(test: &Workspace, parent_pid: u32, needle: &str, action: &str) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_decision-driver"));
    command
        .args([
            parent_pid.to_string(),
            "20000".to_owned(),
            test.transcript.to_string_lossy().into_owned(),
            needle.to_owned(),
            action.to_owned(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn independent console driver");
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let stdout = read_pipe(child.stdout.take().unwrap());
                    let stderr = read_pipe(child.stderr.take().unwrap());
                    panic!(
                        "decision driver failed ({status})\n{}\n{}\nconsole:\n{}\nevents:\n{}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr),
                        fs::read_to_string(&test.transcript).unwrap_or_else(|e| format!("<{e}>")),
                        serde_json::to_string_pretty(&read_events(&test.events)).unwrap()
                    );
                }
                return;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "console driver timed out\nconsole:\n{}\nevents:\n{}",
                    fs::read_to_string(&test.transcript).unwrap_or_else(|e| format!("<{e}>")),
                    serde_json::to_string_pretty(&read_events(&test.events)).unwrap()
                );
            }
        }
    }
}
fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn windows_first_binding_uses_isolated_console_and_preserves_job_stdin() {
    let test = Workspace::new();
    fs::create_dir_all(test._temp.path().join("remote-root")).unwrap();
    assert!(
        !test.config.exists(),
        "config must not exist before uu starts"
    );
    let bash = bash_path();
    let mut command = base_command(&test, &bash);
    command.arg("read -r line; printf 'REMOTE_STDIN:<%s>\\n' \"$line\"");
    let running = Running::start(command, b"binding-sentinel\n");

    drive(&test, running.pid, "Ctrl+C", "wait");
    atomic_fixture(
        &test.fixture,
        &json!({"devices": [{
            "deviceId": DEVICE_ID, "deviceName": DEVICE_NAME, "isOnline": true,
            "terminals": [{"name": SESSION_NAME, "shell": "bash", "state": "detached",
                "createdAt": CREATED_AT, "attachable": true}]
        }]}),
    );
    wait_for_available_terminal(&test.events, Duration::from_secs(20));
    drive(&test, running.pid, "Remote root [", "enter");

    let output = running.finish(Duration::from_secs(90));
    assert_success(&output, "first uu command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REMOTE_STDIN:<binding-sentinel>"),
        "job did not receive caller stdin: {stdout:?}"
    );
    assert!(
        !stdout.contains("Remote root") && !stdout.contains("Ctrl+C") && !stdout.contains("N/Esc"),
        "decision console leaked into uu stdout: {stdout:?}"
    );

    let config: Value =
        serde_json::from_slice(&fs::read(&test.config).expect("first binding saved")).unwrap();
    assert_eq!(config["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["authorized_workers"].as_array().unwrap().len(), 1);
    assert_eq!(config["bindings"][0]["device_identity"], DEVICE_ID);
    assert_eq!(
        config["authorized_workers"][0],
        format!("{DEVICE_ID}\u{1f}{SESSION_NAME}\u{1f}{CREATED_AT}")
    );

    let probe = Command::new(&bash)
        .args(["--noprofile", "--norc", "-c", "pwd"])
        .current_dir(test._temp.path().join("remote-root"))
        .output()
        .expect("probe remote cwd");
    assert!(probe.status.success());
    let expected_root = String::from_utf8_lossy(&probe.stdout).trim().to_owned();
    assert_eq!(config["bindings"][0]["remote_root"], expected_root);

    let events = read_events(&test.events);
    let dispatches: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "dispatch")
        .collect();
    assert_eq!(
        dispatches.len(),
        2,
        "first binding needs one pwd capture and one job: {events:#?}"
    );
    assert!(dispatches[0]["wrapper"]
        .as_str()
        .unwrap()
        .starts_with("printf '\\r\\n%s%s%s' 'UUC'"));
    assert!(dispatches[1]["wrapper"]
        .as_str()
        .unwrap()
        .contains("read -r line"));
    for dispatch in &dispatches {
        assert_eq!(dispatch["deviceId"], DEVICE_ID);
        assert_eq!(dispatch["sessionName"], SESSION_NAME);
        assert_eq!(dispatch["createdAt"], CREATED_AT);
    }
    let transcript = fs::read_to_string(&test.transcript).unwrap_or_default();
    assert!(
        !transcript.contains("[N/Esc]"),
        "pending request asked a second authorization: {transcript}"
    );

    let mut nested = base_command(&test, &bash);
    let quoted_root = format!("'{}'", expected_root);
    nested.current_dir(&test.nested).arg(format!(
        "test \"$(pwd)\" = {quoted_root} && printf ROOT-REUSE-OK"
    ));
    let reused = Running::start(nested, b"").finish(Duration::from_secs(60));
    assert_success(&reused, "nested-directory reuse");
    assert!(String::from_utf8_lossy(&reused.stdout).contains("ROOT-REUSE-OK"));
    let all = read_events(&test.events);
    let all_dispatches: Vec<_> = all
        .iter()
        .filter(|event| event["event"] == "dispatch")
        .collect();
    assert_eq!(
        all_dispatches.len(),
        3,
        "nested call should reuse binding without recapturing pwd"
    );
    assert_eq!(all_dispatches[2]["deviceId"], DEVICE_ID);
    assert_eq!(all_dispatches[2]["createdAt"], CREATED_AT);
}
