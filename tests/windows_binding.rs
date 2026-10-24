#![cfg(all(windows, feature = "test-fixture"))]

use fs2::FileExt;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
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
}

impl Workspace {
    fn new() -> Self {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("workspace");
        let nested = root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create workspace");
        init_git(&root);
        let config = temp.path().join("config").join("missing.json");
        let fixture = temp.path().join("fixture.json");
        let events = temp.path().join("events.jsonl");
        atomic_fixture(&fixture, &json!({"devices": []}));
        Self {
            _temp: temp,
            root,
            nested,
            config,
            fixture,
            events,
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let Ok(contents) = fs::read_to_string(&self.events) else {
            return;
        };
        for event in contents
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        {
            if event.get("event").and_then(Value::as_str) != Some("worker_host_started") {
                continue;
            }
            let Some(pid) = event
                .get("pid")
                .and_then(Value::as_u64)
                .map(|pid| pid as u32)
            else {
                continue;
            };
            let Some(started_at_ms) = event.get("started_at_unix_ms").and_then(Value::as_u64)
            else {
                continue;
            };
            if process_created_at_unix_ms(pid)
                .is_some_and(|created_at_ms| created_at_ms.abs_diff(started_at_ms) < 5_000)
            {
                kill_tree(pid);
            }
        }
    }
}

fn process_created_at_unix_ms(pid: u32) -> Option<u64> {
    use std::ffi::c_void;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const WINDOWS_TO_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut creation = FileTime { low: 0, high: 0 };
    let mut exit = FileTime { low: 0, high: 0 };
    let mut kernel = FileTime { low: 0, high: 0 };
    let mut user = FileTime { low: 0, high: 0 };
    let success =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    unsafe {
        CloseHandle(process);
    }
    if !success {
        return None;
    }
    let ticks = (u64::from(creation.high) << 32) | u64::from(creation.low);
    ticks
        .checked_sub(WINDOWS_TO_UNIX_EPOCH_TICKS)
        .map(|ticks| ticks / 10_000)
}

fn init_git(root: &Path) {
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .output()
        .expect("git init");
    assert!(
        git.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git.stderr)
    );
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
            .expect("write caller-provided interaction input");
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

    fn finish(mut self, timeout: Duration, diagnostics: &Workspace) -> Output {
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
            "uu timed out; stdout={}\nstderr={}\nfixture events:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
            serde_json::to_string_pretty(&read_events(&diagnostics.events)).unwrap()
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

fn fingerprint(session_name: &str, created_at: &str) -> String {
    format!("{DEVICE_ID}\u{1f}{session_name}\u{1f}{created_at}")
}

fn remote_pwd(bash: &Path, working_directory: &Path) -> String {
    let probe = Command::new(bash)
        .args(["--noprofile", "--norc", "-c", "pwd"])
        .current_dir(working_directory)
        .output()
        .expect("probe remote cwd");
    assert!(probe.status.success());
    String::from_utf8_lossy(&probe.stdout).trim().to_owned()
}

fn fixture_terminal(name: &str, created_at: &str) -> Value {
    json!({
        "name": name,
        "shell": "bash",
        "state": "detached",
        "createdAt": created_at,
        "attachable": true
    })
}

fn seed_existing_binding(test: &Workspace, remote_root: &str) -> String {
    let old_fingerprint = fingerprint("bash-existing", "2026-10-20T12:30:00Z");
    fs::create_dir_all(test.config.parent().unwrap()).unwrap();
    fs::write(
        &test.config,
        serde_json::to_vec(&json!({
            "version": 1,
            "bindings": [{
                "local_root": fs::canonicalize(&test.root).unwrap().to_string_lossy(),
                "device_identity": DEVICE_ID,
                "device_name": DEVICE_NAME,
                "remote_root": remote_root,
            }],
            "authorized_workers": [old_fingerprint],
            "declined_workers": [],
        }))
        .unwrap(),
    )
    .unwrap();
    old_fingerprint
}

fn two_new_terminals_fixture() -> Value {
    json!({"devices": [{
        "deviceId": DEVICE_ID,
        "deviceName": DEVICE_NAME,
        "isOnline": true,
        "terminals": [
            fixture_terminal("bash-new-a", "2026-10-21T12:00:00Z"),
            fixture_terminal("bash-new-b", "2026-10-21T12:01:00Z")
        ]
    }]})
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

fn events_named<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["event"] == name)
        .collect()
}

#[test]
fn windows_unbound_command_requires_explicit_bind_without_attaching() {
    let test = Workspace::new();
    let bash = bash_path();
    let mut command = base_command(&test, &bash);
    command.arg("printf MUST_NOT_RUN");
    let started = Instant::now();
    let output = Running::start(command, b"").finish(Duration::from_secs(10), &test);

    assert!(!output.status.success());
    let diagnostic = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("uu bind"), "{diagnostic:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "did not fail fast"
    );
    if test.config.exists() {
        let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
        assert!(config["bindings"].as_array().unwrap().is_empty());
    }
    let events = read_events(&test.events);
    assert!(
        events_named(&events, "attach_started").is_empty(),
        "{events:#?}"
    );
    assert!(events_named(&events, "dispatch").is_empty(), "{events:#?}");
}

#[test]
fn windows_bind_is_inline_and_uses_enter_from_caller_stdin() {
    let test = Workspace::new();
    let remote_directory = test._temp.path().join("remote-root");
    fs::create_dir_all(&remote_directory).unwrap();
    let expected_root = remote_pwd(&bash_path(), &remote_directory);
    atomic_fixture(
        &test.fixture,
        &json!({"devices": [{
            "deviceId": DEVICE_ID,
            "deviceName": DEVICE_NAME,
            "isOnline": true,
            "terminals": [fixture_terminal(SESSION_NAME, CREATED_AT)]
        }]}),
    );

    let mut command = base_command(&test, &bash_path());
    command.current_dir(&test.nested).arg("bind");
    let output = Running::start(command, b"\n").finish(Duration::from_secs(30), &test);
    assert_success(&output, "inline first bind");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Remote root ["),
        "root prompt was not inline: {stdout:?}"
    );
    assert!(
        !stdout.contains("Terminal 编号"),
        "unique Terminal should not ask for a choice: {stdout:?}"
    );
    assert!(
        !stdout.contains("[Enter]") && !stdout.contains("N/Esc"),
        "redundant authorization prompt: {stdout:?}"
    );

    let config: Value =
        serde_json::from_slice(&fs::read(&test.config).expect("binding saved")).unwrap();
    assert_eq!(config["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["authorized_workers"].as_array().unwrap().len(), 1);
    assert_eq!(
        config["authorized_workers"][0],
        fingerprint(SESSION_NAME, CREATED_AT)
    );
    assert_eq!(config["bindings"][0]["device_identity"], DEVICE_ID);
    assert_eq!(
        config["bindings"][0]["local_root"],
        fs::canonicalize(&test.nested)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
        "bind must use the exact caller working directory, even inside a Git repository"
    );
    assert_eq!(config["bindings"][0]["remote_root"], expected_root);

    let events = read_events(&test.events);
    assert_eq!(
        events_named(&events, "attach_started").len(),
        1,
        "{events:#?}"
    );
    assert_eq!(
        events_named(&events, "dispatch").len(),
        1,
        "bind should only capture pwd: {events:#?}"
    );
}

#[test]
fn windows_repeated_bind_adds_and_routes_new_terminal() {
    let test = Workspace::new();
    let bash = bash_path();
    let remote_directory = test._temp.path().join("remote-root");
    fs::create_dir_all(&remote_directory).unwrap();
    let kept_root = remote_pwd(&bash, &remote_directory);
    let old_created_at = "2026-10-20T12:30:00Z";
    let old_fingerprint = fingerprint("bash-existing", old_created_at);
    let new_created_at = "2026-10-21T12:01:00Z";
    let mut old_terminal = fixture_terminal("bash-existing", old_created_at);
    old_terminal["state"] = json!("running");
    old_terminal["attachable"] = json!(false);
    fs::create_dir_all(test.config.parent().unwrap()).unwrap();
    fs::write(
        &test.config,
        serde_json::to_vec(&json!({
            "version": 1,
            "bindings": [{
                "local_root": fs::canonicalize(&test.root).unwrap().to_string_lossy(),
                "device_identity": DEVICE_ID,
                "device_name": DEVICE_NAME,
                "remote_root": kept_root,
            }],
            "authorized_workers": [old_fingerprint],
            "declined_workers": [],
        }))
        .unwrap(),
    )
    .unwrap();
    atomic_fixture(
        &test.fixture,
        &json!({"devices": [{
            "deviceId": DEVICE_ID,
            "deviceName": DEVICE_NAME,
            "isOnline": true,
            "terminals": [old_terminal, fixture_terminal("bash-new", new_created_at)]
        }]}),
    );

    let mut command = base_command(&test, &bash);
    command.arg("bind");
    let output = Running::start(command, b"").finish(Duration::from_secs(20), &test);
    assert_success(&output, "repeat bind");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bash-new"),
        "new Terminal should be reported: {stdout:?}"
    );
    assert!(
        !stdout.contains("Terminal 编号"),
        "a single new Terminal should not ask for a choice: {stdout:?}"
    );
    assert!(
        !stdout.contains("Remote root ["),
        "same-device rebind should retain its existing remote root: {stdout:?}"
    );

    let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
    assert_eq!(
        config["bindings"].as_array().unwrap().len(),
        1,
        "{config:#}"
    );
    assert_eq!(config["bindings"][0]["remote_root"], kept_root);
    let authorized = config["authorized_workers"].as_array().unwrap();
    assert_eq!(
        authorized.len(),
        2,
        "bind must add one worker only: {config:#}"
    );
    assert!(authorized.contains(&json!(old_fingerprint)));
    assert!(authorized.contains(&json!(fingerprint("bash-new", new_created_at))));

    let bind_events = read_events(&test.events);
    assert!(
        events_named(&bind_events, "attach_started").is_empty(),
        "same-device rebind should not attach: {bind_events:#?}"
    );
    assert!(
        events_named(&bind_events, "dispatch").is_empty(),
        "same-device rebind should not run a remote command: {bind_events:#?}"
    );

    for (shell_command, expected_output) in [
        ("printf 'new-worker-first\\n'", "new-worker-first"),
        ("printf 'new-worker-second\\n'", "new-worker-second"),
    ] {
        let mut command = base_command(&test, &bash);
        command.arg(shell_command);
        let output = Running::start(command, b"").finish(Duration::from_secs(30), &test);
        assert_success(&output, "command after repeated bind");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected_output),
            "command output did not come back from the bound worker: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    let events = read_events(&test.events);
    let attaches = events_named(&events, "attach_started");
    assert_eq!(
        attaches.len(),
        1,
        "the new worker should attach only once: {events:#?}"
    );
    assert_eq!(
        attaches[0]["sessionName"], "bash-new",
        "must not attach the old busy instance: {events:#?}"
    );
    assert_eq!(attaches[0]["createdAt"], new_created_at);
    assert_eq!(attaches[0]["available"], true);
    let dispatches = events_named(&events, "dispatch");
    assert_eq!(
        dispatches.len(),
        2,
        "both ordinary commands should dispatch: {events:#?}"
    );
    assert!(
        dispatches.iter().all(|event| {
            event["sessionName"] == "bash-new" && event["createdAt"] == new_created_at
        }),
        "commands must use the newly authorized instance, never the old one: {dispatches:#?}"
    );
}

#[test]
fn windows_bind_selects_second_new_terminal_and_retains_existing_root() {
    let test = Workspace::new();
    let kept_root = "/home/kept-root";
    let old_fingerprint = seed_existing_binding(&test, kept_root);
    atomic_fixture(&test.fixture, &two_new_terminals_fixture());

    let mut command = base_command(&test, &bash_path());
    command.arg("bind");
    let output = Running::start(command, b"2\n").finish(Duration::from_secs(20), &test);
    assert_success(&output, "choose second Terminal during repeat bind");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("交给 CLI 的 Terminal 编号（Enter 取消）："),
        "multiple new Terminals should prompt in the caller console: {stdout:?}"
    );
    assert!(
        !stdout.contains("Remote root ["),
        "same-device repeat bind should retain the existing root: {stdout:?}"
    );
    let first = stdout.find("1. 测试设备 · bash-new-a").unwrap();
    let second = stdout.find("2. 测试设备 · bash-new-b").unwrap();
    assert!(first < second, "Terminal list must be sorted: {stdout:?}");

    let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
    assert_eq!(
        config["bindings"].as_array().unwrap().len(),
        1,
        "{config:#}"
    );
    assert_eq!(config["bindings"][0]["remote_root"], kept_root);
    let authorized = config["authorized_workers"].as_array().unwrap();
    assert_eq!(authorized.len(), 2, "{config:#}");
    assert!(authorized.contains(&json!(old_fingerprint)));
    assert!(authorized.contains(&json!(fingerprint("bash-new-b", "2026-10-21T12:01:00Z"))));
    assert!(!authorized.contains(&json!(fingerprint("bash-new-a", "2026-10-21T12:00:00Z"))));
    let events = read_events(&test.events);
    assert!(
        events_named(&events, "attach_started").is_empty(),
        "{events:#?}"
    );
    assert!(events_named(&events, "dispatch").is_empty(), "{events:#?}");
}

#[test]
fn windows_bind_enter_escape_and_eof_cancel_without_state_changes() {
    for (label, input, expected_success) in [
        ("Enter", b"\n".as_slice(), true),
        ("Escape", b"\x1b\n".as_slice(), true),
        ("EOF", b"".as_slice(), false),
    ] {
        let test = Workspace::new();
        seed_existing_binding(&test, "/home/kept-root");
        atomic_fixture(&test.fixture, &two_new_terminals_fixture());
        let original_config = fs::read(&test.config).unwrap();
        let mut command = base_command(&test, &bash_path());
        command.arg("bind");
        let output = Running::start(command, input).finish(Duration::from_secs(15), &test);

        assert_eq!(
            output.status.success(),
            expected_success,
            "{label} cancellation status: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if label == "EOF" {
            let diagnostic = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                diagnostic.contains("binding input closed"),
                "{diagnostic:?}"
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("已取消绑定。"),
                "{label} should report cancellation: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        assert_eq!(
            fs::read(&test.config).unwrap(),
            original_config,
            "{label} must not change binding or authorization"
        );
        let events = read_events(&test.events);
        assert!(
            events_named(&events, "attach_started").is_empty(),
            "{label} cancellation must not attach: {events:#?}"
        );
        assert!(
            events_named(&events, "dispatch").is_empty(),
            "{label} cancellation must not dispatch: {events:#?}"
        );
    }
}

#[test]
fn windows_bind_reprompts_after_invalid_terminal_numbers() {
    let test = Workspace::new();
    let old_fingerprint = seed_existing_binding(&test, "/home/kept-root");
    atomic_fixture(&test.fixture, &two_new_terminals_fixture());

    let mut command = base_command(&test, &bash_path());
    command.arg("bind");
    let output =
        Running::start(command, b"0\n99\ntext\n2\n").finish(Duration::from_secs(20), &test);
    assert_success(&output, "reprompt after invalid Terminal choices");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .matches("交给 CLI 的 Terminal 编号（Enter 取消）：")
            .count(),
        4,
        "each invalid answer should be followed by a fresh prompt: {stdout:?}"
    );
    let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
    assert_eq!(config["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["bindings"][0]["remote_root"], "/home/kept-root");
    let authorized = config["authorized_workers"].as_array().unwrap();
    assert_eq!(authorized.len(), 2, "{config:#}");
    assert!(authorized.contains(&json!(old_fingerprint)));
    assert!(authorized.contains(&json!(fingerprint("bash-new-b", "2026-10-21T12:01:00Z"))));
    assert!(!authorized.contains(&json!(fingerprint("bash-new-a", "2026-10-21T12:00:00Z"))));
    let events = read_events(&test.events);
    assert!(
        events_named(&events, "attach_started").is_empty(),
        "{events:#?}"
    );
    assert!(events_named(&events, "dispatch").is_empty(), "{events:#?}");
}

#[test]
fn windows_first_bind_selects_terminal_from_chosen_device() {
    let test = Workspace::new();
    let bash = bash_path();
    let remote_directory = test._temp.path().join("remote-root");
    fs::create_dir_all(&remote_directory).unwrap();
    let expected_root = remote_pwd(&bash, &remote_directory);
    atomic_fixture(
        &test.fixture,
        &json!({"devices": [
            {
                "deviceId": "device-z",
                "deviceName": "Beta device",
                "isOnline": true,
                "terminals": [fixture_terminal("bash-beta", "2026-10-21T12:00:00Z")]
            },
            {
                "deviceId": "device-a",
                "deviceName": "Alpha device",
                "isOnline": true,
                "terminals": [fixture_terminal("bash-alpha", "2026-10-21T12:59:00Z")]
            }
        ]}),
    );

    let mut command = base_command(&test, &bash);
    command.arg("bind");
    let output = Running::start(command, b"2\n\n").finish(Duration::from_secs(30), &test);
    assert_success(&output, "choose device during first bind");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let alpha = stdout.find("1. Alpha device · bash-alpha").unwrap();
    let beta = stdout.find("2. Beta device · bash-beta").unwrap();
    assert!(
        alpha < beta,
        "device candidates should be sorted: {stdout:?}"
    );

    let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
    assert_eq!(config["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["bindings"][0]["device_identity"], "device-z");
    assert_eq!(config["bindings"][0]["device_name"], "Beta device");
    assert_eq!(config["bindings"][0]["remote_root"], expected_root);
    assert_eq!(
        config["authorized_workers"].as_array().unwrap(),
        &[json!("device-z\u{1f}bash-beta\u{1f}2026-10-21T12:00:00Z")]
    );
    let events = read_events(&test.events);
    let attaches = events_named(&events, "attach_started");
    assert_eq!(attaches.len(), 1, "{events:#?}");
    assert_eq!(attaches[0]["deviceId"], "device-z");
    assert_eq!(attaches[0]["sessionName"], "bash-beta");
    assert_eq!(attaches[0]["available"], true);
    let dispatches = events_named(&events, "dispatch");
    assert_eq!(
        dispatches.len(),
        1,
        "first bind should only capture pwd: {events:#?}"
    );
    assert!(dispatches
        .iter()
        .all(|event| { event["deviceId"] == "device-z" && event["sessionName"] == "bash-beta" }));
}

#[test]
fn windows_regular_command_does_not_adopt_new_terminal_and_fails_fast() {
    let test = Workspace::new();
    fs::create_dir_all(test.config.parent().unwrap()).unwrap();
    fs::write(
        &test.config,
        serde_json::to_vec(&json!({
            "version": 1,
            "bindings": [{
                "local_root": fs::canonicalize(&test.root).unwrap().to_string_lossy(),
                "device_identity": DEVICE_ID,
                "device_name": DEVICE_NAME,
                "remote_root": "/home/abot",
            }],
            "authorized_workers": [],
            "declined_workers": [],
        }))
        .unwrap(),
    )
    .unwrap();
    atomic_fixture(
        &test.fixture,
        &json!({"devices": [{
            "deviceId": DEVICE_ID,
            "deviceName": DEVICE_NAME,
            "isOnline": true,
            "terminals": [fixture_terminal(SESSION_NAME, CREATED_AT)]
        }]}),
    );

    let mut command = base_command(&test, &bash_path());
    command.arg("printf MUST_NOT_BE_SENT");
    let started = Instant::now();
    let output = Running::start(command, b"").finish(Duration::from_secs(10), &test);
    assert!(!output.status.success());
    let diagnostic = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("uu bind"), "{diagnostic:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "did not fail fast"
    );

    let config: Value = serde_json::from_slice(&fs::read(&test.config).unwrap()).unwrap();
    assert!(config["authorized_workers"].as_array().unwrap().is_empty());
    let events = read_events(&test.events);
    assert!(
        events_named(&events, "attach_started").is_empty(),
        "{events:#?}"
    );
    assert!(events_named(&events, "dispatch").is_empty(), "{events:#?}");
}
