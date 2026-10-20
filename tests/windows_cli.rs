#![cfg(all(windows, feature = "test-fixture"))]

use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const DEVICE_ID: &str = "device-1";
const DEVICE_NAME: &str = "测试设备";
const SESSION_NAME: &str = "bash-3";
const CREATED_AT: &str = "2026-10-20T12:00:00Z";

struct TestWorkspace {
    _temp: TempDir,
    config_path: PathBuf,
    workspace: PathBuf,
    log_path: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temporary test directory");
        let workspace = temp.path().join("workspace-项目");
        let config_path = temp.path().join("配置").join("config.json");
        let log_path = temp.path().join("日志").join("dispatch.jsonl");
        fs::create_dir_all(&workspace).expect("create Unicode workspace");
        fs::create_dir_all(config_path.parent().unwrap()).expect("create Unicode config directory");
        fs::create_dir_all(log_path.parent().unwrap()).expect("create Unicode log directory");

        let fingerprint = format!("{DEVICE_ID}\u{1f}{SESSION_NAME}\u{1f}{CREATED_AT}");
        let config = json!({
            "version": 1,
            "bindings": [{
                "local_root": workspace.to_string_lossy(),
                "device_identity": DEVICE_ID,
                "device_name": DEVICE_NAME,
                "remote_root": "/home/test"
            }],
            "authorized_workers": [fingerprint],
            "declined_workers": []
        });
        fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config).expect("serialize test config"),
        )
        .expect("write test config");

        Self {
            _temp: temp,
            config_path,
            workspace,
            log_path,
        }
    }

    fn set_remote_root(&self, remote_root: &str) {
        let mut config: Value =
            serde_json::from_slice(&fs::read(&self.config_path).expect("read test config"))
                .expect("parse test config");
        config["bindings"][0]["remote_root"] = json!(remote_root);
        fs::write(
            &self.config_path,
            serde_json::to_vec_pretty(&config).expect("serialize updated test config"),
        )
        .expect("update test remote root");
    }
}

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-uuyc"))
}

fn uu_command(test: &TestWorkspace, fake_cli: &Path, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uu"));
    command
        .current_dir(&test.workspace)
        .env("UU_CLI_CONFIG", &test.config_path)
        .env("UUYC_CLI", fake_cli)
        .env("UU_TEST_LOG", &test.log_path)
        .env("UU_TEST_MODE", mode)
        .env("UU_CLI_NO_BACKGROUND", "1");
    command
}

fn pipe_reader<R: Read + Send + 'static>(mut reader: R) -> Receiver<Result<Vec<u8>, String>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

fn kill_process_tree(pid: u32) {
    let pid = pid.to_string();
    let mut killer = match Command::new("taskkill")
        .args(["/PID", pid.as_str(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(killer) => killer,
        Err(_) => return,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match killer.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = killer.kill();
                let _ = killer.wait();
                return;
            }
        }
    }
}

fn collect_pipe(receiver: Receiver<Result<Vec<u8>, String>>, pid: u32, stream: &str) -> Vec<u8> {
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => panic!("read child {stream}: {error}"),
        Err(error) => {
            kill_process_tree(pid);
            panic!("child {stream} did not close after process exit: {error}");
        }
    }
}

fn run_bounded(command: &mut Command, timeout: Duration, description: &str) -> Output {
    let test_log_path = command.get_envs().find_map(|(key, value)| {
        if key == std::ffi::OsStr::new("UU_TEST_LOG") {
            value.map(PathBuf::from)
        } else {
            None
        }
    });
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {description}: {error}"));
    let pid = child.id();
    let stdout = pipe_reader(child.stdout.take().expect("piped stdout"));
    let stderr = pipe_reader(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                timed_out = true;
                kill_process_tree(pid);
                let _ = child.kill();
                break child.wait().expect("wait for timed-out child");
            }
            Err(error) => {
                kill_process_tree(pid);
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait for {description}: {error}");
            }
        }
    };
    let stdout = collect_pipe(stdout, pid, "stdout");
    let stderr = collect_pipe(stderr, pid, "stderr");
    if timed_out {
        let log_contents = match test_log_path.as_deref() {
            Some(path) => match fs::read(path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    "<missing>".to_owned()
                }
                Err(error) => format!("<could not read: {error}>"),
            },
            None => "<UU_TEST_LOG not set on command>".to_owned(),
        };
        let log_path = test_log_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<unset>".to_owned());
        panic!(
            "{description} exceeded {timeout:?}; stdout={}\nstderr={}\nUU_TEST_LOG ({log_path}):\n{log_contents}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }
    Output {
        status,
        stdout,
        stderr,
    }
}

fn find_git_bash() -> (PathBuf, PathBuf) {
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(root).join("Git"));
    }
    if let Some(root) = std::env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(root).join("Git"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\Git"));
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Git"));

    let bash = roots
        .iter()
        .map(|root| root.join("bin").join("bash.exe"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "Git Bash is required for Windows file-transfer coverage; checked Git for Windows under ProgramFiles and ProgramFiles(x86)"
            )
        });
    let git_root = bash
        .parent()
        .and_then(Path::parent)
        .expect("could not infer the Git for Windows root from bash.exe");
    let cygpath = [
        git_root.join("usr").join("bin").join("cygpath.exe"),
        git_root.join("bin").join("cygpath.exe"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("Git for Windows must include cygpath.exe for remote-root conversion");

    let mut probe = Command::new(&bash);
    probe.args([
        "--noprofile",
        "--norc",
        "-c",
        "printf 'UU_TEST_BASH_READY\\n'",
    ]);
    let output = run_bounded(
        &mut probe,
        Duration::from_secs(15),
        "Git Bash capability probe",
    );
    assert!(
        output.status.success(),
        "Git Bash probe failed at {}: {}",
        bash.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "UU_TEST_BASH_READY",
        "unexpected output from Git Bash at {}",
        bash.display()
    );
    (bash, cygpath)
}

fn cygpath_unix(path: &Path, cygpath: &Path) -> String {
    let mut command = Command::new(cygpath);
    command.args(["-u", "--"]).arg(path);
    let output = run_bounded(
        &mut command,
        Duration::from_secs(15),
        "cygpath remote-root conversion",
    );
    assert!(
        output.status.success(),
        "cygpath failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let converted = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        converted.starts_with('/'),
        "cygpath did not return a Git Bash absolute path: {converted:?}"
    );
    converted
}

fn randomized_payload() -> Vec<u8> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ u64::from(std::process::id());
    let mut state = if seed == 0 {
        0x9e37_79b9_7f4a_7c15
    } else {
        seed
    };
    let mut bytes = Vec::with_capacity(1024 * 1024 + 1);
    for _ in 0..(1024 * 1024 + 1) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push((state >> 24) as u8);
    }
    bytes[..8].copy_from_slice(&[0x00, 0xff, 0x0d, 0x0a, 0x1a, 0x80, 0x00, 0xfe]);
    bytes
}

fn assert_transfer_success(output: Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_bytes_equal(expected: &[u8], actual: &[u8], label: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{label} length differs: expected {}, got {}",
        expected.len(),
        actual.len()
    );
    if actual != expected {
        let offset = expected
            .iter()
            .zip(actual)
            .position(|(expected, actual)| expected != actual)
            .expect("different byte vectors have a differing byte");
        panic!(
            "{label} differs at byte {offset}: expected 0x{:02x}, got 0x{:02x}",
            expected[offset], actual[offset]
        );
    }
}

fn is_expected_fake_output(stdout: &str) -> bool {
    if stdout == "fake output: 你好 世界\r\n" {
        return true;
    }
    let Some(cursor) = stdout.strip_prefix("fake output: 你好 世界\x1b[") else {
        return false;
    };
    let Some(cursor) = cursor.strip_suffix('H') else {
        return false;
    };
    let Some((row, column)) = cursor.split_once(';') else {
        return false;
    };
    is_bounded_decimal(row, 24) && is_bounded_decimal(column, 120)
}

fn is_bounded_decimal(value: &str, maximum: u16) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u16>()
            .is_ok_and(|coordinate| (1..=maximum).contains(&coordinate))
}
fn read_dispatches(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("fake official CLI should record a dispatch")
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse dispatch record"))
        .filter(|record: &Value| record.get("event").and_then(Value::as_str) == Some("dispatch"))
        .collect()
}

#[test]
fn windows_cli_streams_unicode_output_and_propagates_remote_exit_code() {
    let fake_cli = fake_cli_path();
    let test = TestWorkspace::new();

    let mut command = uu_command(&test, &fake_cli, "success");
    command.arg("printf '你好 世界'");
    let output = run_bounded(
        &mut command,
        Duration::from_secs(60),
        "uu with the fake official CLI",
    );

    assert_eq!(
        output.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        is_expected_fake_output(&stdout),
        "the full payload should be exact, followed by raw CRLF or one bounded ConPTY cursor sequence; got {stdout:?}"
    );
    assert!(
        !stdout.contains("UUC"),
        "completion framing or command echo leaked to stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("Select target"),
        "stdout contains a human menu: {stdout:?}"
    );
    assert!(
        !stdout.contains("Remote root:"),
        "stdout contains setup input: {stdout:?}"
    );

    let dispatches = read_dispatches(&test.log_path);
    assert_eq!(dispatches.len(), 1);
    assert!(
        dispatches[0]["wrapper"]
            .as_str()
            .unwrap()
            .contains("你好 世界"),
        "Unicode command did not reach the fake terminal"
    );
}

#[test]
fn windows_cli_quarantines_dropped_dispatch_without_replay_or_list_attach() {
    let fake_cli = fake_cli_path();
    let test = TestWorkspace::new();

    let mut drop_command = uu_command(&test, &fake_cli, "drop");
    drop_command.arg("printf '断线 世界'");
    let started = Instant::now();
    let dropped = run_bounded(
        &mut drop_command,
        Duration::from_secs(15),
        "uu with a dropping fake terminal",
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "uu should return promptly after the attach child exits, took {elapsed:?}"
    );
    assert_ne!(dropped.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&dropped.stdout).contains("partial output:"),
        "final PTY output should be drained before returning unknown: {}",
        String::from_utf8_lossy(&dropped.stdout)
    );
    assert!(
        String::from_utf8_lossy(&dropped.stderr)
            .to_ascii_lowercase()
            .contains("unknown"),
        "stderr should report an unknown job state: {}",
        String::from_utf8_lossy(&dropped.stderr)
    );

    let after_drop = read_dispatches(&test.log_path);
    assert_eq!(
        after_drop.len(),
        1,
        "the dropped job must be dispatched once"
    );
    assert!(
        after_drop[0]["wrapper"]
            .as_str()
            .unwrap()
            .contains("断线 世界"),
        "Unicode command did not reach the fake terminal"
    );

    let mut list_command = uu_command(&test, &fake_cli, "drop");
    list_command.arg("list");
    let list = run_bounded(
        &mut list_command,
        Duration::from_secs(60),
        "list quarantined worker state",
    );
    assert!(
        list.status.success(),
        "uu list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(
        String::from_utf8_lossy(&list.stdout)
            .to_ascii_lowercase()
            .contains("unknown"),
        "uu list should display the quarantined worker as unknown: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert_eq!(
        read_dispatches(&test.log_path).len(),
        1,
        "uu list must not attach to or redispatch the unknown worker"
    );
}

#[test]
fn windows_cli_reports_an_unusable_component_without_dispatch() {
    let test = TestWorkspace::new();
    let broken = test.workspace.join("incompatible component.exe");
    fs::write(&broken, b"not a Windows executable").unwrap();
    let mut command = uu_command(&test, &broken, "success");
    command.arg("list");
    let output = run_bounded(
        &mut command,
        Duration::from_secs(60),
        "uu list with an unusable component",
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("incompatible"));
    assert!(
        !test.log_path.exists(),
        "diagnostics must not attach or dispatch"
    );
}

#[test]
fn windows_cli_put_get_binary_files_through_git_bash() {
    let (bash, cygpath) = find_git_bash();
    let fake_cli = fake_cli_path();
    let test = TestWorkspace::new();

    let remote_root = test._temp.path().join("远端 root with spaces");
    fs::create_dir_all(&remote_root).expect("create remote root");
    let remote_root_unix = cygpath_unix(&remote_root, &cygpath);
    test.set_remote_root(&remote_root_unix);

    let local_payload = test._temp.path().join("本地 binary 数据.bin");
    let downloaded_payload = test._temp.path().join("取回 binary 文件.bin");
    let remote_payload = "远端 payload 文件.bin";
    let payload = randomized_payload();
    fs::write(&local_payload, &payload).expect("write randomized 1 MiB+1 payload");

    let mut put_payload = uu_command(&test, &fake_cli, "bash");
    put_payload
        .env("UU_TEST_BASH", &bash)
        .arg("put")
        .arg(&local_payload)
        .arg(remote_payload);
    let output = run_bounded(
        &mut put_payload,
        Duration::from_secs(180),
        "uu put 1 MiB+1 binary payload through Git Bash",
    );
    assert_transfer_success(output, "uu put 1 MiB+1 binary payload");
    let put_dispatches = read_dispatches(&test.log_path);
    assert!(
        put_dispatches.iter().any(|dispatch| {
            dispatch.get("transport").and_then(Value::as_str) == Some("git-bash-pipe")
                && dispatch
                    .get("input_bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    > 0
        }),
        "Git Bash should receive a dispatched command through the piped ConPTY input relay"
    );

    let mut get_payload = uu_command(&test, &fake_cli, "bash");
    get_payload
        .env("UU_TEST_BASH", &bash)
        .arg("get")
        .arg(remote_payload)
        .arg(&downloaded_payload);
    let output = run_bounded(
        &mut get_payload,
        Duration::from_secs(180),
        "uu get 1 MiB+1 binary payload through Git Bash",
    );
    assert_transfer_success(output, "uu get 1 MiB+1 binary payload");
    let actual_payload = fs::read(&downloaded_payload).expect("read downloaded payload");
    assert_bytes_equal(&payload, &actual_payload, "1 MiB+1 binary round-trip");

    let local_empty = test._temp.path().join("本地 empty 文件.bin");
    let downloaded_empty = test._temp.path().join("取回 empty 文件.bin");
    let remote_empty = "远端 empty 文件.bin";
    fs::write(&local_empty, b"").expect("create empty source file");

    let mut put_empty = uu_command(&test, &fake_cli, "bash");
    put_empty
        .env("UU_TEST_BASH", &bash)
        .arg("put")
        .arg(&local_empty)
        .arg(remote_empty);
    let output = run_bounded(
        &mut put_empty,
        Duration::from_secs(180),
        "uu put empty file through Git Bash",
    );
    assert_transfer_success(output, "uu put empty file");

    let mut get_empty = uu_command(&test, &fake_cli, "bash");
    get_empty
        .env("UU_TEST_BASH", &bash)
        .arg("get")
        .arg(remote_empty)
        .arg(&downloaded_empty);
    let output = run_bounded(
        &mut get_empty,
        Duration::from_secs(180),
        "uu get empty file through Git Bash",
    );
    assert_transfer_success(output, "uu get empty file");
    assert_bytes_equal(
        b"",
        &fs::read(&downloaded_empty).expect("read downloaded empty file"),
        "empty-file round-trip",
    );
}
