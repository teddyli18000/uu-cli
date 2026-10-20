use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use uu_cli::model::SessionInfo;
use uu_cli::worker_lease::{AcquireResult, WorkerLease};

fn session(instance: &str) -> SessionInfo {
    SessionInfo { device_identity: "device".into(), device_name: "Car".into(),
        session_name: "worker".into(), shell: Some("/bin/bash".into()),
        state: "running".into(), created_at: Some(instance.into()), attachable: Some(true) }
}

#[test]
fn lease_child_fixture() {
    let Some(path) = std::env::var_os("UU_TEST_LEASE_PATH") else { return; };
    let mut lease = match WorkerLease::try_acquire(std::path::Path::new(&path), &session("original")).unwrap() {
        AcquireResult::Acquired(lease) => lease, other => panic!("{other:?}"),
    };
    if std::env::var("UU_TEST_LEASE_MODE").unwrap() == "crash" {
        lease.mark_in_flight().unwrap();
        // Exit without Drop to simulate a killed CLI/broker after dispatch.
        std::process::exit(91);
    }
    println!("LEASE_READY");
    std::io::stdout().flush().unwrap();
    let mut release = [0u8; 1];
    std::io::stdin().read_exact(&mut release).unwrap();
    lease.complete().unwrap();
}

#[test]
fn process_crash_cannot_make_inflight_worker_idle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.json");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lease_child_fixture", "--nocapture"])
        .env("UU_TEST_LEASE_PATH", &path).env("UU_TEST_LEASE_MODE", "crash")
        .status().unwrap();
    assert_eq!(status.code(), Some(91));
    assert!(matches!(WorkerLease::try_acquire(&path, &session("original")).unwrap(), AcquireResult::Quarantined));
    assert!(matches!(WorkerLease::try_acquire(&path, &session("replacement")).unwrap(), AcquireResult::Acquired(_)));
}

#[test]
fn two_processes_cannot_run_on_one_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("config.json");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lease_child_fixture", "--nocapture"])
        .env("UU_TEST_LEASE_PATH", &path).env("UU_TEST_LEASE_MODE", "hold")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.unwrap().contains("LEASE_READY") { let _ = sender.send(()); break; }
        }
    });
    if receiver.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = child.kill(); let _ = child.wait();
        panic!("lease fixture failed to become ready");
    }
    assert!(matches!(WorkerLease::try_acquire(&path, &session("original")).unwrap(), AcquireResult::Busy));
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    assert!(matches!(WorkerLease::try_acquire(&path, &session("original")).unwrap(), AcquireResult::Acquired(_)));
}
