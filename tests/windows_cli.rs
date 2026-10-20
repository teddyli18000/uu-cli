#![cfg(all(windows, feature = "test-fixture"))]

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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

fn read_dispatches(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("fake official CLI should record a dispatch")
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse dispatch record"))
        .collect()
}

#[test]
fn windows_cli_streams_unicode_output_and_propagates_remote_exit_code() {
    let fake_cli = fake_cli_path();
    let test = TestWorkspace::new();

    let output = uu_command(&test, &fake_cli, "success")
        .arg("printf '你好 世界'")
        .output()
        .expect("run uu through the fake official CLI");

    assert_eq!(
        output.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fake output: 你好 世界"),
        "stdout: {stdout:?}"
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

    let dropped = uu_command(&test, &fake_cli, "drop")
        .arg("printf '断线 世界'")
        .output()
        .expect("run uu with a dropping fake terminal");
    assert_ne!(dropped.status.code(), Some(0));
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

    let list = uu_command(&test, &fake_cli, "drop")
        .arg("list")
        .output()
        .expect("list quarantined worker state");
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
    let output = uu_command(&test, &broken, "success").arg("list").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("incompatible"));
    assert!(!test.log_path.exists(), "diagnostics must not attach or dispatch");
}
